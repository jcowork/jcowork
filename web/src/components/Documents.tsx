import { useState, useEffect, useCallback, useRef } from 'react';

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

const ALLOWED_EXTENSIONS = '.pdf,.md,.html,.htm,.xlsx,.xls,.docx,.doc';

export default function Documents({ token }: DocumentsProps) {
  const [tree, setTree] = useState<TreeNode[]>([]);
  const [loading, setLoading] = useState(true);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [previewContent, setPreviewContent] = useState<string>('');
  const [previewLoading, setPreviewLoading] = useState(false);
  const [showNewFolder, setShowNewFolder] = useState(false);
  const [newFolderName, setNewFolderName] = useState('');
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [currentDir, setCurrentDir] = useState<string>('');
  const fileInputRef = useRef<HTMLInputElement>(null);

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
    try {
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
        await loadRoot();
      } else {
        const err = await res.json();
        alert(err.error || 'Upload failed');
      }
    } catch {
      alert('Network error');
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
    setTree(entries.map(e => ({
      name: e.name,
      type: e.type,
      path: e.name,
      expanded: false,
      children: e.type === 'dir' ? undefined : undefined,
    })));
    setLoading(false);
  }, [fetchDir]);

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
      setTree(prev => updateNode(prev, nodePath, (n) => ({
        ...n,
        loading: false,
        children: entries.map(e => ({
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

    const res = await fetch(`/api/workspace/download?path=${encodeURIComponent(filePath)}`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    if (res.ok) {
      const text = await res.text();
      setPreviewContent(text);
    } else {
      setPreviewContent('Failed to load file.');
    }
    setPreviewLoading(false);
  };

  const downloadFile = (filePath: string) => {
    const a = document.createElement('a');
    a.href = `/api/workspace/download?path=${encodeURIComponent(filePath)}&token=${encodeURIComponent(token)}`;
    a.download = filePath.split('/').pop() || 'file';
    a.click();
  };

  const openInNewTab = (filePath: string) => {
    window.open(`/api/workspace/download?path=${encodeURIComponent(filePath)}&token=${encodeURIComponent(token)}`, '_blank');
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
      default: return '📄';
    }
  };

  const renderTree = (nodes: TreeNode[], depth: number = 0): React.ReactNode => {
    return nodes.map(node => (
      <div key={node.path}>
        <div
          onClick={() => node.type === 'dir' ? toggleDir(node.path) : openFile(node.path)}
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
            background: previewPath === node.path ? '#1a3a5a' : 'transparent',
            transition: 'background 0.1s',
          }}
          onMouseEnter={e => {
            if (previewPath !== node.path) {
              e.currentTarget.style.background = '#1e1e1e';
            }
          }}
          onMouseLeave={e => {
            if (previewPath !== node.path) {
              e.currentTarget.style.background = 'transparent';
            }
          }}
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
          <div>
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
    ));
  };

  return (
    <div style={{ display: 'flex', height: '100%' }}>
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
          <span style={{ fontWeight: 600, fontSize: 15 }}>My Documents</span>
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
              title="Upload files to root"
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
              {uploading ? '⏳' : '⬆ Upload'}
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
              Refresh
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
              placeholder="Folder name..."
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
              Create
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
              Cancel
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
                title="New subfolder here"
                style={{ padding: '2px 6px', borderRadius: 3, border: '1px solid #444', background: 'transparent', color: '#888', cursor: 'pointer', fontSize: 10 }}
              >
                + Folder
              </button>
              <button
                onClick={() => fileInputRef.current?.click()}
                disabled={uploading}
                title="Upload here"
                style={{ padding: '2px 6px', borderRadius: 3, border: '1px solid #444', background: 'transparent', color: uploading ? '#555' : '#888', cursor: uploading ? 'not-allowed' : 'pointer', fontSize: 10 }}
              >
                ⬆ Upload
              </button>
            </div>
          </div>
        )}
        <div style={{ flex: 1, overflowY: 'auto', padding: '8px 4px' }}>
          {loading ? (
            <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>Loading...</div>
          ) : tree.length === 0 ? (
            <div style={{ color: '#555', padding: 16, textAlign: 'center', fontSize: 13 }}>
              Your workspace is empty.<br />
              <span style={{ fontSize: 12, color: '#444' }}>
                Use chat to create files and projects!
              </span>
            </div>
          ) : (
            renderTree(tree)
          )}
        </div>
      </div>

      {/* File preview area */}
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
              <span style={{ fontSize: 13, color: '#aaa', fontFamily: 'monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {previewPath}
              </span>
              <div style={{ display: 'flex', gap: 8, flexShrink: 0 }}>
                {previewPath.endsWith('.html') && (
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
                    Open in Browser
                  </button>
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
                  Download
                </button>
                <button
                  onClick={() => { setPreviewPath(null); setPreviewContent(''); }}
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
                  Close
                </button>
              </div>
            </div>
            <div style={{ flex: 1, overflow: 'auto', padding: 16 }}>
              {previewLoading ? (
                <div style={{ color: '#666', textAlign: 'center', paddingTop: 40 }}>Loading...</div>
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
              <div>Select a file to preview</div>
              <div style={{ fontSize: 12, color: '#333', marginTop: 8 }}>
                Click a folder to expand · Click a file to view
              </div>
            </div>
          </div>
        )}
      </div>
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
