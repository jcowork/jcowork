import { useState, useEffect, useCallback, useRef } from 'react';
import { useT } from '../i18n';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

/** Image component that loads images via fetch with Bearer token auth.
 *  Browser <img> tags don't send Authorization headers, so we fetch
 *  the image manually and convert it to a blob URL. */
function AuthImage({ src, token, alt, ...props }: { src: string; token: string; alt?: string } & React.ImgHTMLAttributes<HTMLImageElement>) {
  const [blobUrl, setBlobUrl] = useState<string>('');
  const [error, setError] = useState(false);

  useEffect(() => {
    if (!src) return;
    let cancelled = false;
    fetch(src, { headers: { Authorization: `Bearer ${token}` } })
      .then(res => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        return res.blob();
      })
      .then(blob => {
        if (cancelled) return;
        const url = URL.createObjectURL(blob);
        setBlobUrl(url);
        setError(false);
      })
      .catch(() => {
        if (!cancelled) setError(true);
      });
    return () => { cancelled = true; };
  }, [src, token]);

  if (error) {
    return <span style={{ color: '#666', fontSize: 12 }}>[Image load failed]</span>;
  }

  return (
    <img
      src={blobUrl || undefined}
      alt={alt || 'Document image'}
      style={{
        maxWidth: '100%',
        height: 'auto',
        borderRadius: 6,
        border: '1px solid #333',
        margin: '12px 0',
      }}
      {...props}
    />
  );
}

interface FileEntry {
  name: string;
  type: 'file' | 'dir';
}

interface TreeNode {
  name: string;
  type: 'file' | 'dir';
  path: string;
  children?: TreeNode[];
  expanded?: boolean;
  loading?: boolean;
}

interface DocumentsProps {
  userId: string;
  token: string;
}

interface ExcelTablePreview {
  name: string;
  columns: [string, string][];
  row_count: number;
  rows: (string | number | null)[][];
}

interface ExcelPreview {
  path: string;
  db_name: string;
  preview_rows: number;
  tables: ExcelTablePreview[];
}

interface DocChunk {
  file_path: string;
  chunk_type: string;
  content: string;
  heading: string;
  chunk_index: number;
  image_path?: string;
}

const ALLOWED_EXTENSIONS = '.pdf,.md,.html,.htm,.xlsx,.xls,.docx,.doc';

// Characters fetched per page when previewing long documents (PDFs)
const PREVIEW_PAGE_SIZE = 30000;

/**
 * Inject a script into HTML content that overrides `fetch()` to resolve
 * relative URLs against the workspace download API. Used only for iframe
 * srcDoc rendering — the stored content stays clean.
 */
function injectWorkspaceFetch(html: string, filePath: string, token: string): string {
  const dir = filePath.includes('/') ? filePath.substring(0, filePath.lastIndexOf('/')) : '';
  const script = `<script>
(function() {
  var __dir = ${JSON.stringify(dir)};
  var __token = ${JSON.stringify(token)};
  var __orig = window.fetch;
  function __resolve(u) { var c = u.split('?')[0].split('#')[0]; return __dir ? __dir + '/' + c : c; }
  window.fetch = function(u, o) {
    if (typeof u === 'string' && !u.startsWith('/') && !u.startsWith('http') && !u.startsWith('blob:') && !u.startsWith('data:')) {
      u = '/api/workspace/download?path=' + encodeURIComponent(__resolve(u)) + '&token=' + encodeURIComponent(__token);
    }
    return __orig.call(this, u, o);
  };
  var __xo = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function(m, u) {
    if (typeof u === 'string' && !u.startsWith('/') && !u.startsWith('http') && !u.startsWith('blob:') && !u.startsWith('data:')) {
      u = '/api/workspace/download?path=' + encodeURIComponent(__resolve(u)) + '&token=' + encodeURIComponent(__token);
    }
    return __xo.apply(this, arguments);
  };
})();
</script>`;
  if (/<head[^>]*>/i.test(html)) {
    return html.replace(/(<head[^>]*>)/i, `$1${script}`);
  }
  return script + html;
}

export default function Documents({ token }: DocumentsProps) {
  const t = useT();
  const [tree, setTree] = useState<TreeNode[]>([]);
  const [loading, setLoading] = useState(true);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [previewContent, setPreviewContent] = useState<string>('');
  const [previewLoading, setPreviewLoading] = useState(false);
  const [excelData, setExcelData] = useState<ExcelPreview | null>(null);
  const [activeTable, setActiveTable] = useState(0);
  const [docChunks, setDocChunks] = useState<DocChunk[] | null>(null);
  const [previewMode, setPreviewMode] = useState<'content' | 'chunks'>('content');
  const [htmlViewMode, setHtmlViewMode] = useState<'preview' | 'source'>('preview');
  // Pagination state for long documents (PDF preview "load more on scroll")
  const [previewPaging, setPreviewPaging] = useState<{ nextOffset: number } | null>(null);
  const [previewLoadingMore, setPreviewLoadingMore] = useState(false);
  const [showNewFolder, setShowNewFolder] = useState(false);
  const [newFolderName, setNewFolderName] = useState('');
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [statusMessage, setStatusMessage] = useState<string>('');
  const [currentDir, setCurrentDir] = useState<string>('');
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<{ path: string; isDir: boolean; name: string } | null>(null);
  const [draggedPath, setDraggedPath] = useState<string | null>(null);
  const [dragOverPath, setDragOverPath] = useState<string | null>(null);
  const draggedPathRef = useRef<string | null>(null);
  const [dragDebug, setDragDebug] = useState<string>('');
  // Editor state for text files
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState('');
  const [editDirty, setEditDirty] = useState(false);
  const [saving, setSaving] = useState(false);

  const isEditableFile = (path: string) => {
    const ext = path.split('.').pop()?.toLowerCase();
    return ext === 'html' || ext === 'htm' || ext === 'md' || ext === 'txt' || ext === 'csv';
  };

  const startEditing = () => {
    setEditContent(previewContent);
    setEditDirty(false);
    setEditing(true);
  };

  const cancelEditing = () => {
    if (editDirty && !confirm('有未保存的更改，确定放弃吗？')) return;
    setEditing(false);
    setEditDirty(false);
  };

  const saveFile = async () => {
    if (!previewPath || saving) return;
    setSaving(true);
    try {
      const res = await fetch('/api/workspace/save', {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ path: previewPath, content: editContent }),
      });
      if (res.ok) {
        setPreviewContent(editContent);
        setEditing(false);
        setEditDirty(false);
        setStatusMessage('✓ 已保存');
        setTimeout(() => setStatusMessage(''), 2000);
      } else {
        const err = await res.json();
        alert(err.error || '保存失败');
      }
    } catch {
      alert('网络错误，保存失败');
    }
    setSaving(false);
  };

  const handleMove = async (from: string, to: string) => {
    // Prevent moving a folder into itself or its own descendant
    const name = from.split('/').pop() || from;
    if (to === from || to.startsWith(from + '/')) {
      alert(`Cannot move "${name}" into itself or its subfolder.`);
      return;
    }
    // Prevent no-op move
    const fromParent = from.includes('/') ? from.substring(0, from.lastIndexOf('/')) : '';
    if (fromParent === to) return;
    const toPath = to ? `${to}/${name}` : name;
    if (toPath === from) return;
    try {
      const res = await fetch('/api/workspace/move', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ from, to: toPath }),
      });
      if (res.ok) {
        // If the moved file/folder was being previewed, update preview path
        if (previewPath && (previewPath === from || previewPath.startsWith(from + '/'))) {
          setPreviewPath(toPath + previewPath.slice(from.length));
        }
        await reloadTree();
      } else {
        const err = await res.json();
        alert(err.error || 'Move failed');
      }
    } catch {
      alert('Network error');
    }
  };

  const handleCreateFolder = async () => {
    if (!newFolderName.trim()) return;
    setCreatingFolder(true);
    const folderPath = currentDir ? `${currentDir}/${newFolderName.trim()}` : newFolderName.trim();
    try {
      const res = await fetch('/api/workspace/mkdir', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ path: folderPath }),
      });
      if (res.ok) {
        setShowNewFolder(false);
        setNewFolderName('');
        await loadRoot();
      } else {
        const err = await res.json();
        alert(err.error || 'Failed to create folder');
      }
    } catch {
      alert('Network error');
    }
    setCreatingFolder(false);
  };

  const handleUploadFiles = async (files: FileList | null) => {
    if (!files || files.length === 0) return;
    setUploading(true);
    setStatusMessage('');
    try {
      // Check if any PDF files are being uploaded
      const hasPdf = Array.from(files).some(f =>
        f.name.toLowerCase().endsWith('.pdf')
      );

      if (hasPdf) {
        // Check Docling service status
        setStatusMessage(t('doclingChecking'));
        try {
          const statusRes = await fetch('/api/docling/status', {
            headers: { Authorization: `Bearer ${token}` },
          });
          if (statusRes.ok) {
            const status = await statusRes.json();
            if (!status.running) {
              // Start Docling service
              setStatusMessage(t('doclingStarting'));
              const startRes = await fetch('/api/docling/start', {
                method: 'POST',
                headers: { Authorization: `Bearer ${token}` },
              });
              if (startRes.ok) {
                // Poll until service is ready
                const maxWait = 180000; // 3 minutes
                const pollInterval = 3000;
                const start = Date.now();
                let ready = false;
                while (Date.now() - start < maxWait) {
                  await new Promise(r => setTimeout(r, pollInterval));
                  try {
                    const pollRes = await fetch('/api/docling/status', {
                      headers: { Authorization: `Bearer ${token}` },
                    });
                    if (pollRes.ok) {
                      const pollStatus = await pollRes.json();
                      if (pollStatus.running) {
                        ready = true;
                        break;
                      }
                    }
                  } catch { /* keep polling */ }
                }
                if (!ready) {
                  setStatusMessage(t('doclingStartFailed'));
                  setUploading(false);
                  return;
                }
                setStatusMessage(t('doclingReady'));
              } else {
                const errData = await startRes.json().catch(() => null);
                setStatusMessage(`${t('doclingStartFailed')}: ${errData?.message || 'unknown error'}`);
                setUploading(false);
                return;
              }
            }
          }
        } catch {
          // Status check failed — proceed with upload anyway, backend will auto-start
        }
      }

      // Upload files
      setStatusMessage(t('pdfParsing'));
      const formData = new FormData();
      // Send target directory
      formData.append('path', currentDir || '.');
      for (let i = 0; i < files.length; i++) {
        formData.append('files', files[i]);
      }
      const res = await fetch('/api/workspace/upload', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}` },
        body: formData,
      });
      if (res.ok) {
        const data = await res.json();
        await loadRoot();
        // Show indexing status
        if (data.files) {
          const failed = data.files.filter((f: any) => !f.indexed);
          if (failed.length > 0) {
            const msgs = failed.map((f: any) => `${f.filename}: ${f.index_error || 'indexing failed'}`).join('\n');
            alert(`Upload succeeded but indexing failed for:\n${msgs}\n\nYou can try re-indexing from the directory menu.`);
            setStatusMessage('');
          } else {
            setStatusMessage(t('doclingReady'));
            // Clear status after 3 seconds
            setTimeout(() => setStatusMessage(''), 3000);
          }
        } else {
          setStatusMessage('');
        }
      } else {
        const err = await res.json();
        alert(err.error || 'Upload failed');
        setStatusMessage('');
      }
    } catch {
      alert('Network error');
      setStatusMessage('');
    }
    setUploading(false);
    if (fileInputRef.current) fileInputRef.current.value = '';
  };

  const fetchDir = useCallback(async (path: string): Promise<FileEntry[]> => {
    const res = await fetch(`/api/workspace/files?path=${encodeURIComponent(path)}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (res.ok) {
      return await res.json();
    }
    return [];
  }, [token]);

  const loadRoot = useCallback(async () => {
    setLoading(true);
    const entries = await fetchDir('.');
    // Filter out hidden files/dirs (starting with ".")
    const visible = entries.filter(e => !e.name.startsWith('.'));
    setTree(visible.map(e => ({
      name: e.name,
      type: e.type,
      path: e.name,
      expanded: false,
      children: e.type === 'dir' ? undefined : undefined,
    })));
    setLoading(false);
  }, [fetchDir]);

  // Collect all expanded directory paths from the tree
  const getExpandedPaths = (nodes: TreeNode[]): string[] => {
    const paths: string[] = [];
    for (const node of nodes) {
      if (node.type === 'dir' && node.expanded) {
        paths.push(node.path);
        if (node.children) paths.push(...getExpandedPaths(node.children));
      }
    }
    return paths;
  };

  // Reload root and re-expand previously expanded directories
  const reloadTree = useCallback(async () => {
    const expandedPaths = getExpandedPaths(tree);
    setLoading(true);
    const entries = await fetchDir('.');
    const visible = entries.filter(e => !e.name.startsWith('.'));
    let newTree: TreeNode[] = visible.map(e => ({
      name: e.name,
      type: e.type as 'file' | 'dir',
      path: e.name,
      expanded: false,
      children: undefined as TreeNode[] | undefined,
    }));
    // Re-expand directories that were previously expanded and still exist
    for (const dirPath of expandedPaths) {
      const node = findNode(newTree, dirPath);
      if (node && node.type === 'dir') {
        const dirEntries = await fetchDir(dirPath);
        const dirVisible = dirEntries.filter(e => !e.name.startsWith('.'));
        newTree = updateNode(newTree, dirPath, (n) => ({
          ...n,
          expanded: true,
          children: dirVisible.map(e => ({
            name: e.name,
            type: e.type,
            path: `${dirPath}/${e.name}`,
            expanded: false,
          })),
        }));
      }
    }
    setTree(newTree);
    setLoading(false);
  }, [tree, fetchDir]);

  useEffect(() => {
    loadRoot();
  }, [loadRoot]);

  const toggleDir = async (nodePath: string) => {
    setCurrentDir(nodePath);
    setTree(prev => updateNode(prev, nodePath, (node) => {
      if (node.expanded) {
        return { ...node, expanded: false };
      }
      return { ...node, expanded: true, loading: true };
    }));

    // If expanding and children not loaded, fetch them
    const node = findNode(tree, nodePath);
    if (node && !node.expanded && (!node.children || node.children.length === 0)) {
      const entries = await fetchDir(nodePath);
      // Filter out hidden files/dirs (starting with ".")
      const visible = entries.filter(e => !e.name.startsWith('.'));
      setTree(prev => updateNode(prev, nodePath, (n) => ({
        ...n,
        loading: false,
        children: visible.map(e => ({
          name: e.name,
          type: e.type,
          path: `${nodePath}/${e.name}`,
          expanded: false,
        })),
      })));
    } else {
      setTree(prev => updateNode(prev, nodePath, (n) => ({ ...n, loading: false })));
    }
  };

  const openFile = async (filePath: string) => {
    setPreviewPath(filePath);
    setPreviewLoading(true);
    setPreviewContent('');
    setExcelData(null);
    setDocChunks(null);
    setPreviewMode('content');
    setHtmlViewMode('preview');
    setPreviewPaging(null);
    setPreviewLoadingMore(false);
    setEditing(false);
    setEditDirty(false);

    const ext = filePath.split('.').pop()?.toLowerCase();

    // Excel files: show the parsed SQLite database tables
    if (ext === 'xlsx' || ext === 'xls') {
      setActiveTable(0);
      const res = await fetch(`/api/workspace/excel-db?path=${encodeURIComponent(filePath)}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setExcelData(data);
      } else {
        const err = await res.json().catch(() => null);
        setPreviewContent(err?.error || '[Excel not parsed yet. Try re-uploading the file.]');
      }
      setPreviewLoading(false);
      return;
    }

    // PDF files: get extracted text from workspace index (parsed during upload),
    // first page only — more pages load automatically when scrolling down
    if (ext === 'pdf') {
      const res = await fetch(`/api/workspace/index/content?path=${encodeURIComponent(filePath)}&offset=0&limit=${PREVIEW_PAGE_SIZE}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const data = await res.json();
        setPreviewContent(data.content || '[No indexed content found]');
        if (data.has_more) {
          setPreviewPaging({ nextOffset: data.next_offset });
        }
      } else {
        setPreviewContent('[PDF not indexed. Please re-upload or re-index the directory.]');
      }
      
      // Also load chunks for vector search preview
      const chunksRes = await fetch(`/api/workspace/doc/chunks?file_path=${encodeURIComponent(filePath)}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (chunksRes.ok) {
        const chunksData = await chunksRes.json();
        if (chunksData.chunks && chunksData.chunks.length > 0) {
          setDocChunks(chunksData.chunks);
        }
      }
    } else {
      // Text files: fetch directly (use raw=true for HTML to get clean content)
      const isHtml = ext === 'html' || ext === 'htm';
      const rawParam = isHtml ? '&raw=true' : '';
      const res = await fetch(`/api/workspace/download?path=${encodeURIComponent(filePath)}${rawParam}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const text = await res.text();
        setPreviewContent(text);
      } else {
        setPreviewContent('Failed to load file.');
      }
    }
    setPreviewLoading(false);
  };

  // Fetch the next page of a long document and append it to the preview
  const loadMorePreview = async () => {
    if (!previewPath || !previewPaging || previewLoadingMore) return;
    setPreviewLoadingMore(true);
    try {
      const res = await fetch(
        `/api/workspace/index/content?path=${encodeURIComponent(previewPath)}&offset=${previewPaging.nextOffset}&limit=${PREVIEW_PAGE_SIZE}`,
        { headers: { Authorization: `Bearer ${token}` } },
      );
      if (res.ok) {
        const data = await res.json();
        setPreviewContent((prev) => prev + (data.content || ''));
        setPreviewPaging(data.has_more ? { nextOffset: data.next_offset } : null);
      }
    } finally {
      setPreviewLoadingMore(false);
    }
  };

  const handleDelete = (filePath: string, isDir: boolean) => {
    const name = filePath.split('/').pop() || filePath;
    setDeleteConfirm({ path: filePath, isDir, name });
  };

  const executeDelete = async () => {
    if (!deleteConfirm) return;
    const { path: filePath } = deleteConfirm;
    setDeleteConfirm(null);
    try {
      const res = await fetch('/api/workspace/delete', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ path: filePath }),
      });
      if (res.ok) {
        // If the deleted file/folder was being previewed, close preview
        if (previewPath && (previewPath === filePath || previewPath.startsWith(filePath + '/'))) {
          setPreviewPath(null);
          setPreviewContent('');
          setExcelData(null);
          setDocChunks(null);
        }
        await reloadTree();
      } else {
        const err = await res.json();
        alert(err.error || 'Delete failed');
      }
    } catch {
      alert('Network error');
    }
  };

  const downloadFile = (filePath: string) => {
    const a = document.createElement('a');
    a.href = `/api/workspace/download?path=${encodeURIComponent(filePath)}&token=${encodeURIComponent(token)}`;
    a.download = filePath.split('/').pop() || 'file';
    a.click();
  };

  const openInNewTab = (filePath: string) => {
    const url = `/api/workspace/download?path=${encodeURIComponent(filePath)}&token=${encodeURIComponent(token)}`;
    // In Tauri WebView, use native command to open in system browser
    if ((window as any).__TAURI__) {
      (window as any).__TAURI__.core.invoke('open_in_browser', { url: `http://localhost:3000${url}` });
    } else {
      window.open(url, '_blank');
    }
  };

  const getFileIcon = (name: string) => {
    const ext = name.split('.').pop()?.toLowerCase();
    switch (ext) {
      case 'html': case 'htm': return '🌐';
      case 'css': return '🎨';
      case 'js': case 'ts': case 'tsx': case 'jsx': return '⚡';
      case 'py': return '🐍';
      case 'rs': return '🦀';
      case 'json': return '📋';
      case 'md': return '📝';
      case 'svg': return '🖼️';
      case 'txt': return '📄';
      case 'yml': case 'yaml': return '⚙️';
      case 'sh': case 'bash': return '💻';
      case 'png': case 'jpg': case 'jpeg': case 'gif': case 'webp': return '🖼️';
      case 'pdf': return '📕';
      case 'xlsx': case 'xls': return '📊';
      case 'docx': case 'doc': return '📃';
      default: return '📄';
    }
  };

  const renderTree = (nodes: TreeNode[], depth: number = 0): React.ReactNode => {
    return nodes.map(node => {
      const isDragTarget = node.type === 'dir';
      const isDragOver = dragOverPath === node.path;
      const isBeingDragged = draggedPath === node.path;
      return (
      <div key={node.path}>
        <div
          className="tree-row"
          draggable
          onClick={() => node.type === 'dir' ? toggleDir(node.path) : openFile(node.path)}
          onDragStart={(e) => {
            e.dataTransfer.effectAllowed = 'move';
            e.dataTransfer.setData('text/plain', node.path);
            draggedPathRef.current = node.path;
            setDraggedPath(node.path);
            setDragDebug(`dragStart: ${node.path}`);
          }}
          onDragEnd={() => {
            draggedPathRef.current = null;
            setDraggedPath(null);
            setDragOverPath(null);
          }}
          onDragOver={(e) => {
            if (isDragTarget && draggedPathRef.current && draggedPathRef.current !== node.path) {
              if (draggedPathRef.current.startsWith(node.path + '/')) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = 'move';
              setDragOverPath(node.path);
              setDragDebug(`dragOver: ${node.path} (from: ${draggedPathRef.current})`);
            }
          }}
          onDragLeave={() => {
            if (dragOverPath === node.path) setDragOverPath(null);
          }}
          onDrop={(e) => {
            e.preventDefault();
            e.stopPropagation();
            const fromPath = draggedPathRef.current || e.dataTransfer.getData('text/plain');
            setDragOverPath(null);
            setDraggedPath(null);
            draggedPathRef.current = null;
            setDragDebug(`drop: from=${fromPath} to=${node.path} isDragTarget=${isDragTarget}`);
            if (fromPath && isDragTarget) {
              handleMove(fromPath, node.path);
            }
          }}
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 6,
            padding: '5px 8px',
            paddingLeft: 8 + depth * 16,
            borderRadius: 4,
            cursor: 'pointer',
            fontSize: 13,
            color: '#ddd',
            border: isDragOver ? '1px dashed #3fb950' : '1px solid transparent',
            opacity: isBeingDragged ? 0.4 : 1,
            transition: 'background 0.1s, border 0.1s, opacity 0.1s',
          }}
          data-selected={previewPath === node.path || undefined}
          data-dragover={isDragOver || undefined}
        >
          {node.type === 'dir' ? (
            <>
              <span style={{ fontSize: 10, color: '#888', width: 12, textAlign: 'center' }}>
                {node.loading ? '⏳' : node.expanded ? '▼' : '▶'}
              </span>
              <span style={{ fontSize: 14 }}>{node.expanded ? '📂' : '📁'}</span>
            </>
          ) : (
            <>
              <span style={{ width: 12 }} />
              <span style={{ fontSize: 14 }}>{getFileIcon(node.name)}</span>
            </>
          )}
          <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {node.name}
          </span>
          {/* Folder action buttons: new folder, upload, delete */}
          {node.type === 'dir' && (
            <>
              <span
                className="tree-icon-btn"
                style={{
                  opacity: 0, fontSize: 12, flexShrink: 0, cursor: 'pointer',
                  transition: 'opacity 0.15s', color: '#58a6ff',
                }}
                onClick={(e) => { e.stopPropagation(); setCurrentDir(node.path); setShowNewFolder(true); setNewFolderName(''); }}
                title={t('newFolder')}
              >
                📁+
              </span>
              <span
                className="tree-icon-btn"
                style={{
                  opacity: 0, fontSize: 12, flexShrink: 0, cursor: 'pointer',
                  transition: 'opacity 0.15s', color: '#58a6ff',
                }}
                onClick={(e) => { e.stopPropagation(); setCurrentDir(node.path); fileInputRef.current?.click(); }}
                title={t('upload')}
              >
                ⬆
              </span>
            </>
          )}
          {/* Delete button */}
          <span
            style={{
              opacity: 0, fontSize: 11, flexShrink: 0, cursor: 'pointer',
              transition: 'opacity 0.15s', color: '#f85149',
            }}
            className="delete-btn"
            onClick={(e) => { e.stopPropagation(); handleDelete(node.path, node.type === 'dir'); }}
            title={t('delete')}
          >
            ✕
          </span>
          {node.type === 'file' && (
            <span
              style={{ opacity: 0.4, fontSize: 11, flexShrink: 0 }}
              onClick={(e) => { e.stopPropagation(); downloadFile(node.path); }}
              title="Download"
            >
              ⬇
            </span>
          )}
          {node.type === 'file' && node.name.endsWith('.html') && (
            <span
              style={{ opacity: 0.4, fontSize: 11, flexShrink: 0, marginLeft: 4 }}
              onClick={(e) => { e.stopPropagation(); openInNewTab(node.path); }}
              title="Open in new tab"
            >
              ↗
            </span>
          )}
        </div>
        {node.type === 'dir' && node.expanded && node.children && (
          <div
            onDragOver={(e) => {
              const src = draggedPathRef.current;
              if (src && !src.startsWith(node.path + '/') && src !== node.path) {
                e.preventDefault();
                e.dataTransfer.dropEffect = 'move';
              }
            }}
            onDrop={(e) => {
              e.preventDefault();
              const fromPath = draggedPathRef.current || e.dataTransfer.getData('text/plain');
              if (fromPath && !fromPath.startsWith(node.path + '/') && fromPath !== node.path) {
                handleMove(fromPath, node.path);
              }
            }}
            style={{ minHeight: 4 }}
          >
            {node.children.length === 0 && !node.loading ? (
              <div style={{ paddingLeft: 24 + depth * 16, fontSize: 12, color: '#555', padding: '4px 8px' }}>
                Empty
              </div>
            ) : (
              renderTree(node.children, depth + 1)
            )}
          </div>
        )}
      </div>
      );
    });
  };

  return (
    <div style={{ display: 'flex', height: '100%' }}>
      <style>{`
        .tree-row { background: transparent; }
        .tree-row:hover { background: #1e1e1e; }
        .tree-row[data-selected] { background: #1a3a5a; }
        .tree-row[data-selected]:hover { background: #1a3a5a; }
        .tree-row[data-dragover] { background: #1a4a2a; }
        .tree-row[data-dragover]:hover { background: #1a4a2a; }
        .tree-row:hover .delete-btn { opacity: 0.6 !important; }
        .tree-row:hover .delete-btn:hover { opacity: 1 !important; }
        .tree-row:hover .tree-icon-btn { opacity: 0.6 !important; }
        .tree-row:hover .tree-icon-btn:hover { opacity: 1 !important; }
      `}</style>
      {/* File tree sidebar */}
      <div style={{
        width: 280,
        borderRight: '1px solid #333',
        display: 'flex',
        flexDirection: 'column',
        overflow: 'hidden',
        flexShrink: 0,
      }}>
        <div style={{
          padding: '12px 16px',
          borderBottom: '1px solid #333',
          display: 'flex',
          alignItems: 'center',
          justifyContent: 'space-between',
        }}>
          <span style={{ fontWeight: 600, fontSize: 15 }}>{t('myDocuments')}</span>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <button
              onClick={() => { setCurrentDir(''); setShowNewFolder(true); setNewFolderName(''); }}
              title="New folder in root"
              style={{
                padding: '3px 8px',
                borderRadius: 4,
                border: '1px solid #555',
                background: 'transparent',
                color: '#aaa',
                cursor: 'pointer',
                fontSize: 11,
              }}
            >
              + Folder
            </button>
            <button
              onClick={() => { setCurrentDir(''); fileInputRef.current?.click(); }}
              title={t('uploadHere')}
              disabled={uploading}
              style={{
                padding: '3px 8px',
                borderRadius: 4,
                border: '1px solid #555',
                background: 'transparent',
                color: uploading ? '#555' : '#aaa',
                cursor: uploading ? 'not-allowed' : 'pointer',
                fontSize: 11,
              }}
            >
              {uploading ? '⏳' : `⬆ ${t('upload')}`}
            </button>
            <button
              onClick={loadRoot}
              style={{
                padding: '3px 8px',
                borderRadius: 4,
                border: '1px solid #555',
                background: 'transparent',
                color: '#aaa',
                cursor: 'pointer',
                fontSize: 11,
              }}
            >
              {t('refresh')}
            </button>
          </div>
          <input
            ref={fileInputRef}
            type="file"
            accept={ALLOWED_EXTENSIONS}
            multiple
            style={{ display: 'none' }}
            onChange={(e) => { handleUploadFiles(e.target.files); }}
          />
        </div>
        {/* Status message bar */}
        {statusMessage && (
          <div style={{
            padding: '8px 16px',
            borderBottom: '1px solid #333',
            background: statusMessage.includes(t('doclingStartFailed')) ? '#3a1a1a' : '#1a2a3a',
            color: statusMessage.includes(t('doclingStartFailed')) ? '#ff8888' : '#8ab4f8',
            fontSize: 12,
            display: 'flex',
            alignItems: 'center',
            gap: 8,
          }}>
            <span>{statusMessage.includes(t('doclingStartFailed')) ? '❌' : '⏳'}</span>
            <span>{statusMessage}</span>
          </div>
        )}
        {/* New folder inline input */}
        {showNewFolder && (
          <div style={{
            padding: '8px 12px',
            borderBottom: '1px solid #333',
            display: 'flex',
            alignItems: 'center',
            gap: 6,
          }}>
            <span style={{ fontSize: 13 }}>📁</span>
            <input
              autoFocus
              value={newFolderName}
              onChange={(e) => setNewFolderName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleCreateFolder();
                if (e.key === 'Escape') { setShowNewFolder(false); setNewFolderName(''); }
              }}
              placeholder={t('folderName')}
              style={{
                flex: 1,
                padding: '3px 8px',
                borderRadius: 4,
                border: '1px solid #555',
                background: '#1a1a1a',
                color: '#eee',
                fontSize: 12,
                outline: 'none',
              }}
            />
            <button
              onClick={handleCreateFolder}
              disabled={creatingFolder || !newFolderName.trim()}
              style={{
                padding: '3px 8px',
                borderRadius: 4,
                border: '1px solid #1f6feb',
                background: '#1f6feb',
                color: '#fff',
                cursor: 'pointer',
                fontSize: 11,
                opacity: creatingFolder || !newFolderName.trim() ? 0.5 : 1,
              }}
            >
              {t('create')}
            </button>
            <button
              onClick={() => { setShowNewFolder(false); setNewFolderName(''); }}
              style={{
                padding: '3px 8px',
                borderRadius: 4,
                border: '1px solid #555',
                background: 'transparent',
                color: '#888',
                cursor: 'pointer',
                fontSize: 11,
              }}
            >
              {t('cancel')}
            </button>
          </div>
        )}
        {/* Current directory indicator */}
        {currentDir && (
          <div style={{
            padding: '4px 12px',
            borderBottom: '1px solid #333',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'space-between',
            fontSize: 11,
            color: '#666',
          }}>
            <span style={{ fontFamily: 'monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              📂 {currentDir}
            </span>
            <div style={{ display: 'flex', gap: 4 }}>
              <button
                onClick={() => { setShowNewFolder(true); setNewFolderName(''); }}
                title={t('newSubfolder')}
                style={{ padding: '2px 6px', borderRadius: 3, border: '1px solid #444', background: 'transparent', color: '#888', cursor: 'pointer', fontSize: 10 }}
              >
                + {t('newFolder')}
              </button>
              <button
                onClick={() => fileInputRef.current?.click()}
                disabled={uploading}
                title={t('uploadHere')}
                style={{ padding: '2px 6px', borderRadius: 3, border: '1px solid #444', background: 'transparent', color: uploading ? '#555' : '#888', cursor: uploading ? 'not-allowed' : 'pointer', fontSize: 10 }}
              >
                ⬆ {t('upload')}
              </button>
            </div>
          </div>
        )}
        <div style={{
          flex: 1, overflowY: 'auto', padding: '8px 4px',
        }}
          onDragOver={(e) => {
            // Allow drop on root area only if dragging a non-root item
            const src = draggedPathRef.current;
            if (src && src.includes('/')) {
              e.preventDefault();
              e.dataTransfer.dropEffect = 'move';
            }
          }}
          onDrop={(e) => {
            e.preventDefault();
            const fromPath = draggedPathRef.current || e.dataTransfer.getData('text/plain');
            // Only allow moving to root if item is currently in a subfolder
            if (fromPath && fromPath.includes('/')) {
              handleMove(fromPath, '');
            }
          }}
        >
          {loading ? (
            <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
          ) : tree.length === 0 ? (
            <div style={{ color: '#555', padding: 16, textAlign: 'center', fontSize: 13 }}>
              {t('workspaceEmpty')}<br />
              <span style={{ fontSize: 12, color: '#444' }}>
                {t('useChatToCreate')}
              </span>
            </div>
          ) : (
            renderTree(tree)
          )}
        </div>
        {/* Drag debug panel - remove after verification */}
        {dragDebug && (
          <div style={{
            padding: '4px 8px', fontSize: 10, color: '#0f0', background: '#000',
            borderTop: '1px solid #333', fontFamily: 'monospace',
            maxHeight: 40, overflow: 'auto',
          }}>
            {dragDebug}
          </div>
        )}
      </div>
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0 }}>
        {previewPath ? (
          <>
            <div style={{
              padding: '8px 16px',
              borderBottom: '1px solid #333',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              gap: 8,
            }}>
              <span style={{ fontSize: 13, color: '#aaa', fontFamily: 'monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', display: 'flex', alignItems: 'center', gap: 6 }}>
                {previewPath.endsWith('.pdf') && (
                  <span style={{
                    fontSize: 10, padding: '1px 6px', borderRadius: 3,
                    background: '#da3633', color: '#fff', fontWeight: 600, flexShrink: 0,
                  }}>PDF</span>
                )}
                {(previewPath.endsWith('.xlsx') || previewPath.endsWith('.xls')) && (
                  <span style={{
                    fontSize: 10, padding: '1px 6px', borderRadius: 3,
                    background: '#1f6feb', color: '#fff', fontWeight: 600, flexShrink: 0,
                  }}>EXCEL → SQLITE</span>
                )}
                {previewPath}
              </span>
              <div style={{ display: 'flex', gap: 6, flexShrink: 0 }}>
                {editing ? (
                  <>
                    <button
                      onClick={saveFile}
                      disabled={saving}
                      style={{
                        padding: '3px 10px',
                        borderRadius: 4,
                        border: '1px solid #3fb950',
                        background: saving ? '#1a3a1a' : '#238636',
                        color: saving ? '#666' : '#fff',
                        cursor: saving ? 'not-allowed' : 'pointer',
                        fontSize: 12,
                        fontWeight: 600,
                      }}
                    >
                      {saving ? '⏳' : '✓'} {t('save')}
                    </button>
                    <button
                      onClick={cancelEditing}
                      style={{
                        padding: '3px 10px',
                        borderRadius: 4,
                        border: '1px solid #555',
                        background: 'transparent',
                        color: '#aaa',
                        cursor: 'pointer',
                        fontSize: 12,
                      }}
                    >
                      {t('cancel')}
                    </button>
                    {editDirty && (
                      <span style={{ fontSize: 11, color: '#f0883e', alignSelf: 'center' }}>● 未保存</span>
                    )}
                  </>
                ) : (
                  <>
                    {previewPath.endsWith('.html') && (
                      <>
                        <div style={{ display: 'flex', borderRadius: 4, border: '1px solid #555', overflow: 'hidden' }}>
                          <button
                            onClick={() => setHtmlViewMode('preview')}
                            style={{
                              padding: '3px 10px',
                              border: 'none',
                              borderRight: '1px solid #555',
                              background: htmlViewMode === 'preview' ? '#1f6feb' : 'transparent',
                              color: htmlViewMode === 'preview' ? '#fff' : '#aaa',
                              cursor: 'pointer',
                              fontSize: 12,
                              fontWeight: htmlViewMode === 'preview' ? 600 : 400,
                            }}
                          >
                            {t('preview')}
                          </button>
                          <button
                            onClick={() => setHtmlViewMode('source')}
                            style={{
                              padding: '3px 10px',
                              border: 'none',
                              background: htmlViewMode === 'source' ? '#1f6feb' : 'transparent',
                              color: htmlViewMode === 'source' ? '#fff' : '#aaa',
                              cursor: 'pointer',
                              fontSize: 12,
                              fontWeight: htmlViewMode === 'source' ? 600 : 400,
                            }}
                          >
                            {t('source')}
                          </button>
                        </div>
                        <button
                          onClick={() => openInNewTab(previewPath)}
                          style={{
                            padding: '3px 10px',
                            borderRadius: 4,
                            border: '1px solid #1f6feb',
                            background: 'transparent',
                            color: '#58a6ff',
                            cursor: 'pointer',
                            fontSize: 12,
                          }}
                        >
                          {t('openInBrowser')}
                        </button>
                      </>
                    )}
                    {isEditableFile(previewPath) && (
                      <button
                        onClick={startEditing}
                        style={{
                          padding: '3px 10px',
                          borderRadius: 4,
                          border: '1px solid #f0883e',
                          background: 'transparent',
                          color: '#f0883e',
                          cursor: 'pointer',
                          fontSize: 12,
                        }}
                      >
                        ✎ {t('edit')}
                      </button>
                    )}
                  </>
                )}
                <button
                  onClick={() => downloadFile(previewPath)}
                  style={{
                    padding: '3px 10px',
                    borderRadius: 4,
                    border: '1px solid #555',
                    background: 'transparent',
                    color: '#aaa',
                    cursor: 'pointer',
                    fontSize: 12,
                  }}
                >
                  {t('download')}
                </button>
                <button
                  onClick={() => {
                    if (editing && editDirty && !confirm('有未保存的更改，确定关闭吗？')) return;
                    setPreviewPath(null); setPreviewContent(''); setExcelData(null); setDocChunks(null); setPreviewPaging(null); setPreviewLoadingMore(false);
                    setEditing(false); setEditDirty(false);
                  }}
                  style={{
                    padding: '3px 10px',
                    borderRadius: 4,
                    border: '1px solid #555',
                    background: 'transparent',
                    color: '#888',
                    cursor: 'pointer',
                    fontSize: 12,
                  }}
                >
                  {t('close')}
                </button>
              </div>
            </div>
            {/* Tab switcher for PDF files with chunks */}
            {previewPath.endsWith('.pdf') && docChunks && docChunks.length > 0 && (
              <div style={{ display: 'flex', gap: 4, padding: '8px 16px 0', borderBottom: '1px solid #333' }}>
                <button
                  onClick={() => setPreviewMode('content')}
                  style={{
                    padding: '6px 16px',
                    borderRadius: '4px 4px 0 0',
                    border: 'none',
                    borderBottom: previewMode === 'content' ? '2px solid #1f6feb' : '2px solid transparent',
                    background: 'transparent',
                    color: previewMode === 'content' ? '#fff' : '#888',
                    cursor: 'pointer',
                    fontSize: 12,
                    fontWeight: previewMode === 'content' ? 600 : 400,
                  }}
                >
                  📄 Content
                </button>
                <button
                  onClick={() => setPreviewMode('chunks')}
                  style={{
                    padding: '6px 16px',
                    borderRadius: '4px 4px 0 0',
                    border: 'none',
                    borderBottom: previewMode === 'chunks' ? '2px solid #1f6feb' : '2px solid transparent',
                    background: 'transparent',
                    color: previewMode === 'chunks' ? '#fff' : '#888',
                    cursor: 'pointer',
                    fontSize: 12,
                    fontWeight: previewMode === 'chunks' ? 600 : 400,
                  }}
                >
                  🧩 Chunks ({docChunks.length})
                </button>
              </div>
            )}
            <div
              style={{ flex: 1, overflow: 'hidden', padding: editing ? 0 : 16 }}
              onScroll={(e) => {
                if (editing) return;
                const el = e.currentTarget;
                if (previewPaging && el.scrollTop + el.clientHeight >= el.scrollHeight - 300) {
                  loadMorePreview();
                }
              }}
            >
              {previewLoading ? (
                <div style={{ color: '#666', textAlign: 'center', paddingTop: 40 }}>{t('loading')}</div>
              ) : editing ? (
                <textarea
                  value={editContent}
                  onChange={(e) => { setEditContent(e.target.value); setEditDirty(true); }}
                  onKeyDown={(e) => {
                    if ((e.metaKey || e.ctrlKey) && e.key === 's') {
                      e.preventDefault();
                      saveFile();
                    }
                    if (e.key === 'Tab') {
                      e.preventDefault();
                      const start = e.currentTarget.selectionStart;
                      const end = e.currentTarget.selectionEnd;
                      const val = editContent;
                      setEditContent(val.substring(0, start) + '  ' + val.substring(end));
                      requestAnimationFrame(() => {
                        e.currentTarget.selectionStart = e.currentTarget.selectionEnd = start + 2;
                      });
                    }
                  }}
                  spellCheck={false}
                  style={{
                    width: '100%',
                    height: '100%',
                    margin: 0,
                    padding: 16,
                    border: 'none',
                    outline: 'none',
                    resize: 'none',
                    fontSize: 13,
                    lineHeight: 1.6,
                    color: '#c9d1d9',
                    background: '#0d1117',
                    fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace",
                    tabSize: 2,
                  }}
                />
              ) : excelData ? (
                renderExcelPreview(excelData, activeTable, setActiveTable, t)
              ) : previewMode === 'chunks' && docChunks ? (
                renderChunksPreview(docChunks)
              ) : previewPath.endsWith('.pdf') ? (
                // PDF content: render as Markdown with image support
                <div className="pdf-markdown-preview" style={{
                  fontSize: 14,
                  lineHeight: 1.7,
                  color: '#c9d1d9',
                }}>
                  <ReactMarkdown
                    remarkPlugins={[remarkGfm]}
                    components={{
                      img: ({ node, src, alt, ...props }) => {
                        // Use AuthImage component to load images with Bearer token
                        const filename = src ? src.split('/').pop() : '';
                        const proxyUrl = `/api/workspace/doc/image?file_path=${encodeURIComponent(previewPath || '')}&filename=${encodeURIComponent(filename || '')}`;
                        return <AuthImage src={proxyUrl} token={token} alt={alt || 'Document image'} {...props} />;
                      },
                      table: ({ children, ...props }) => (
                        <div style={{ overflow: 'auto', border: '1px solid #333', borderRadius: 6, margin: '12px 0' }}>
                          <table style={{ borderCollapse: 'collapse', width: '100%', fontSize: 13 }} {...props}>
                            {children}
                          </table>
                        </div>
                      ),
                      th: ({ children, ...props }) => (
                        <th style={{
                          padding: '6px 12px', borderBottom: '1px solid #333',
                          background: '#161b22', textAlign: 'left', color: '#c9d1d9', fontWeight: 600,
                        }} {...props}>
                          {children}
                        </th>
                      ),
                      td: ({ children, ...props }) => (
                        <td style={{ padding: '4px 12px', borderTop: '1px solid #21262d', color: '#c9d1d9' }} {...props}>
                          {children}
                        </td>
                      ),
                      h1: ({ children, ...props }) => (
                        <h1 style={{ fontSize: 24, fontWeight: 700, color: '#e6edf3', borderBottom: '1px solid #333', paddingBottom: 8, marginTop: 24 }} {...props}>
                          {children}
                        </h1>
                      ),
                      h2: ({ children, ...props }) => (
                        <h2 style={{ fontSize: 20, fontWeight: 600, color: '#e6edf3', borderBottom: '1px solid #333', paddingBottom: 6, marginTop: 20 }} {...props}>
                          {children}
                        </h2>
                      ),
                      h3: ({ children, ...props }) => (
                        <h3 style={{ fontSize: 16, fontWeight: 600, color: '#e6edf3', marginTop: 16 }} {...props}>
                          {children}
                        </h3>
                      ),
                      code: ({ children, ...props }) => (
                        <code style={{ background: '#161b22', padding: '2px 6px', borderRadius: 4, fontSize: '0.9em', color: '#f0883e' }} {...props}>
                          {children}
                        </code>
                      ),
                      pre: ({ children, ...props }) => (
                        <pre style={{ background: '#161b22', padding: 12, borderRadius: 6, overflow: 'auto', fontSize: 13 }} {...props}>
                          {children}
                        </pre>
                      ),
                      blockquote: ({ children, ...props }) => (
                        <blockquote style={{ borderLeft: '3px solid #1f6feb', paddingLeft: 16, margin: '12px 0', color: '#8b949e' }} {...props}>
                          {children}
                        </blockquote>
                      ),
                      a: ({ children, ...props }) => (
                        <a style={{ color: '#58a6ff', textDecoration: 'none' }} {...props}>
                          {children}
                        </a>
                      ),
                    }}
                  >
                    {previewContent}
                  </ReactMarkdown>
                  {previewLoadingMore && (
                    <div style={{ color: '#666', textAlign: 'center', padding: 12 }}>{t('loading')}</div>
                  )}
                </div>
              ) : previewPath.endsWith('.html') && htmlViewMode === 'preview' ? (
                <iframe
                  srcDoc={injectWorkspaceFetch(previewContent, previewPath, token)}
                  style={{
                    width: '100%',
                    height: '100%',
                    border: 'none',
                    borderRadius: 8,
                    background: '#fff',
                  }}
                  title="HTML Preview"
                  sandbox="allow-scripts allow-same-origin"
                />
              ) : (
                <pre style={{
                  margin: 0,
                  fontSize: 13,
                  lineHeight: 1.6,
                  color: '#c9d1d9',
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-word',
                  fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace",
                }}>
                  {previewContent}
                </pre>
              )}
            </div>
          </>
        ) : (
          <div style={{
            flex: 1,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            color: '#444',
            fontSize: 14,
          }}>
            <div style={{ textAlign: 'center' }}>
              <div style={{ fontSize: 48, marginBottom: 16 }}>📁</div>
              <div>{t('selectFilePreview')}</div>
              <div style={{ fontSize: 12, color: '#333', marginTop: 8 }}>
                {t('clickFolderToExpand')}
              </div>
            </div>
          </div>
        )}
      </div>
      {/* Delete confirmation dialog */}
      {deleteConfirm && (
        <div style={{
          position: 'fixed', top: 0, left: 0, right: 0, bottom: 0,
          background: 'rgba(0,0,0,0.5)', display: 'flex',
          alignItems: 'center', justifyContent: 'center', zIndex: 1000,
        }} onClick={() => setDeleteConfirm(null)}>
          <div style={{
            background: '#1e1e1e', borderRadius: 12, padding: '24px 28px',
            border: '1px solid #444', boxShadow: '0 8px 32px rgba(0,0,0,0.6)',
            maxWidth: 400, minWidth: 300,
          }} onClick={e => e.stopPropagation()}>
            <div style={{ fontSize: 15, fontWeight: 600, color: '#eee', marginBottom: 12 }}>
              {t('confirmDelete')}
            </div>
            <div style={{ fontSize: 13, color: '#aaa', marginBottom: 20, lineHeight: 1.5 }}>
              {deleteConfirm.isDir
                ? `确定要删除文件夹 "${deleteConfirm.name}" 及其所有内容吗？`
                : `确定要删除 "${deleteConfirm.name}" 吗？`}
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
              <button
                onClick={() => setDeleteConfirm(null)}
                style={{
                  padding: '6px 16px', borderRadius: 6, border: '1px solid #555',
                  background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 13,
                }}
              >
                {t('cancel')}
              </button>
              <button
                onClick={executeDelete}
                style={{
                  padding: '6px 16px', borderRadius: 6, border: '1px solid #f85149',
                  background: '#f8514920', color: '#f85149', cursor: 'pointer',
                  fontSize: 13, fontWeight: 600,
                }}
              >
                {t('delete')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// Tree helper functions
function findNode(nodes: TreeNode[], path: string): TreeNode | null {
  for (const node of nodes) {
    if (node.path === path) return node;
    if (node.children) {
      const found = findNode(node.children, path);
      if (found) return found;
    }
  }
  return null;
}

// Render the parsed SQLite database of an Excel file as tabbed tables.
function renderExcelPreview(
  data: ExcelPreview,
  activeTable: number,
  setActiveTable: (i: number) => void,
  t: ReturnType<typeof useT>,
): React.ReactNode {
  if (data.tables.length === 0) {
    return <div style={{ color: '#888', textAlign: 'center', paddingTop: 40 }}>{t('excelNoTables')}</div>;
  }
  const active = Math.min(activeTable, data.tables.length - 1);
  const table = data.tables[active];
  return (
    <div>
      {data.tables.length > 1 && (
        <div style={{ display: 'flex', gap: 6, marginBottom: 12, flexWrap: 'wrap' }}>
          {data.tables.map((tb, i) => (
            <button
              key={tb.name}
              onClick={() => setActiveTable(i)}
              style={{
                padding: '4px 12px',
                borderRadius: 4,
                border: i === active ? '1px solid #1f6feb' : '1px solid #444',
                background: i === active ? '#1f6feb' : 'transparent',
                color: i === active ? '#fff' : '#aaa',
                cursor: 'pointer',
                fontSize: 12,
              }}
            >
              {tb.name} ({tb.row_count})
            </button>
          ))}
        </div>
      )}
      <div style={{ fontSize: 12, color: '#777', marginBottom: 8, fontFamily: 'monospace' }}>
        SQLite: {data.db_name}.db · "{table.name}" — {table.row_count} {t('rows')}
        {table.row_count > table.rows.length && ` · ${t('excelPreviewFirst')} ${table.rows.length} ${t('rows')}`}
      </div>
      <div style={{ overflow: 'auto', border: '1px solid #333', borderRadius: 6 }}>
        <table style={{ borderCollapse: 'collapse', width: '100%', fontSize: 12 }}>
          <thead>
            <tr>
              {table.columns.map(([name, ty]) => (
                <th key={name} style={{
                  position: 'sticky', top: 0, background: '#161b22', textAlign: 'left',
                  padding: '6px 12px', borderBottom: '1px solid #333', whiteSpace: 'nowrap',
                  color: '#c9d1d9', fontWeight: 600,
                }}>
                  {name} <span style={{ color: '#555', fontWeight: 400, fontSize: 10 }}>{ty}</span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {table.rows.map((row, ri) => (
              <tr key={ri} style={{ background: ri % 2 ? '#11151c' : 'transparent' }}>
                {row.map((cell, ci) => (
                  <td key={ci} style={{
                    padding: '4px 12px', borderTop: '1px solid #21262d', color: '#c9d1d9',
                    maxWidth: 320, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                  }} title={cell === null ? 'NULL' : String(cell)}>
                    {cell === null
                      ? <span style={{ color: '#555', fontStyle: 'italic' }}>NULL</span>
                      : String(cell)}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// Render document chunks for vector search preview.
function renderChunksPreview(chunks: DocChunk[]): React.ReactNode {
  if (chunks.length === 0) {
    return <div style={{ color: '#888', textAlign: 'center', paddingTop: 40 }}>No indexed chunks found.</div>;
  }
  
  return (
    <div>
      <div style={{ fontSize: 12, color: '#777', marginBottom: 12, fontFamily: 'monospace' }}>
        Document indexed as {chunks.length} chunk(s) for semantic search
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
        {chunks.map((chunk, i) => {
          const typeIcon = chunk.chunk_type === 'table' ? '📊' : chunk.chunk_type === 'image' ? '🖼️' : '📄';
          const typeColor = chunk.chunk_type === 'table' ? '#1f6feb' : chunk.chunk_type === 'image' ? '#da3633' : '#3fb950';
          return (
            <div
              key={i}
              style={{
                border: '1px solid #333',
                borderRadius: 6,
                padding: 12,
                background: '#0d1117',
              }}
            >
              <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8 }}>
                <span style={{ fontSize: 16 }}>{typeIcon}</span>
                <span style={{
                  fontSize: 10, padding: '1px 6px', borderRadius: 3,
                  background: typeColor, color: '#fff', fontWeight: 600,
                }}>
                  {chunk.chunk_type.toUpperCase()}
                </span>
                <span style={{ fontSize: 11, color: '#888' }}>#{chunk.chunk_index}</span>
                {chunk.heading && (
                  <span style={{ fontSize: 11, color: '#58a6ff', fontStyle: 'italic' }}>
                    {chunk.heading}
                  </span>
                )}
              </div>
              <pre style={{
                margin: 0,
                fontSize: 12,
                lineHeight: 1.5,
                color: '#c9d1d9',
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-word',
                fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace",
                maxHeight: 200,
                overflow: 'auto',
              }}>
                {chunk.content.length > 500 ? chunk.content.slice(0, 500) + '...' : chunk.content}
              </pre>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function updateNode(nodes: TreeNode[], path: string, updater: (node: TreeNode) => TreeNode): TreeNode[] {
  return nodes.map(node => {
    if (node.path === path) {
      return updater(node);
    }
    if (node.children) {
      return { ...node, children: updateNode(node.children, path, updater) };
    }
    return node;
  });
}
