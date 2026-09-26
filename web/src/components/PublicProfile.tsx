import { useState, useEffect, useCallback, useRef } from 'react';
import { useT } from '../i18n';
import { formatFrequency, type TranslationFn } from '../utils/cron';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface PublicProfileProps {
  viewerToken: string;
  publicUser: { userId: string; username: string };
  onClose: () => void;
}

interface PublicDoc {
  id: number;
  file_path: string;
  dir_path: string;
  filename: string;
  content_type: string;
  size: number;
  indexed_at: string;
  snippet: string;
}

interface CronJob {
  id: string;
  user_id: string;
  schedule: string;
  prompt: string;
  enabled: boolean;
  last_run: string | null;
  created_at: string;
  name?: string;
  model?: string;
}

interface TaskResult {
  id: string;
  cron_job_id: string;
  user_id: string;
  output: string;
  status: string;
  executed_at: string;
}

// Characters fetched per page when previewing a public document
const CONTENT_PAGE_SIZE = 30000;

/**
 * Read-only viewer for a public account's shared content.
 *
 * Shows the account's public documents (double-gated: document AND account
 * must be public) and its periodic-task execution results. There are no
 * add/edit/delete/download actions anywhere — viewers can only read.
 *
 * Layout mirrors the local Documents view: left list + right preview panel.
 * HTML and Markdown files get a "Preview / Source" toggle (default: Preview).
 */
export default function PublicProfile({ viewerToken, publicUser, onClose }: PublicProfileProps) {
  const t = useT();
  const [tab, setTab] = useState<'docs' | 'tasks'>('docs');

  // ── Public documents state ──
  const [docs, setDocs] = useState<PublicDoc[]>([]);
  const [docsLoading, setDocsLoading] = useState(true);
  const [previewDoc, setPreviewDoc] = useState<PublicDoc | null>(null);
  const [previewContent, setPreviewContent] = useState('');
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState('');
  const [previewNextOffset, setPreviewNextOffset] = useState<number | null>(null);
  const [previewLoadingMore, setPreviewLoadingMore] = useState(false);
  const [htmlViewMode, setHtmlViewMode] = useState<'preview' | 'source'>('preview');
  const previewScrollRef = useRef<HTMLDivElement>(null);

  // ── Periodic tasks state ──
  const [jobs, setJobs] = useState<CronJob[]>([]);
  const [jobsLoading, setJobsLoading] = useState(true);
  const [expandedJob, setExpandedJob] = useState<string | null>(null);
  const [jobResults, setJobResults] = useState<Record<string, TaskResult[]>>({});
  const [resultsLoadingJob, setResultsLoadingJob] = useState<string | null>(null);

  const authHeaders = { Authorization: `Bearer ${viewerToken}` };

  const fetchDocs = useCallback(async () => {
    setDocsLoading(true);
    try {
      const res = await fetch(`/api/public-users/${publicUser.userId}/documents`, {
        headers: { Authorization: `Bearer ${viewerToken}` },
      });
      if (res.ok) {
        const data = await res.json();
        setDocs(data.documents || []);
      } else {
        setDocs([]);
      }
    } catch {
      setDocs([]);
    }
    setDocsLoading(false);
  }, [publicUser.userId, viewerToken]);

  const fetchJobs = useCallback(async () => {
    setJobsLoading(true);
    try {
      const res = await fetch(`/api/public-users/${publicUser.userId}/cron-jobs`, {
        headers: { Authorization: `Bearer ${viewerToken}` },
      });
      if (res.ok) {
        setJobs(await res.json());
      } else {
        setJobs([]);
      }
    } catch {
      setJobs([]);
    }
    setJobsLoading(false);
  }, [publicUser.userId, viewerToken]);

  useEffect(() => {
    setPreviewDoc(null);
    setPreviewContent('');
    setExpandedJob(null);
    setJobResults({});
    fetchDocs();
    fetchJobs();
  }, [fetchDocs, fetchJobs]);

  // ── Document preview (paged, read-only) ──
  const openDocPreview = async (doc: PublicDoc) => {
    setPreviewDoc(doc);
    setPreviewContent('');
    setPreviewError('');
    setPreviewNextOffset(null);
    setPreviewLoading(true);
    setHtmlViewMode('preview');
    try {
      const res = await fetch(
        `/api/public-users/${publicUser.userId}/documents/content?path=${encodeURIComponent(doc.file_path)}&offset=0&limit=${CONTENT_PAGE_SIZE}`,
        { headers: authHeaders },
      );
      if (res.ok) {
        const data = await res.json();
        setPreviewContent(data.content || '');
        setPreviewNextOffset(data.has_more ? data.next_offset : null);
      } else {
        setPreviewError(t('networkError'));
      }
    } catch {
      setPreviewError(t('networkError'));
    }
    setPreviewLoading(false);
  };

  const loadMorePreview = async () => {
    if (!previewDoc || previewNextOffset === null || previewLoadingMore) return;
    setPreviewLoadingMore(true);
    try {
      const res = await fetch(
        `/api/public-users/${publicUser.userId}/documents/content?path=${encodeURIComponent(previewDoc.file_path)}&offset=${previewNextOffset}&limit=${CONTENT_PAGE_SIZE}`,
        { headers: authHeaders },
      );
      if (res.ok) {
        const data = await res.json();
        setPreviewContent((prev) => prev + (data.content || ''));
        setPreviewNextOffset(data.has_more ? data.next_offset : null);
      }
    } finally {
      setPreviewLoadingMore(false);
    }
  };

  // ── Task results (loaded lazily on expand) ──
  const toggleJobResults = async (jobId: string) => {
    if (expandedJob === jobId) {
      setExpandedJob(null);
      return;
    }
    setExpandedJob(jobId);
    if (jobResults[jobId]) return;
    setResultsLoadingJob(jobId);
    try {
      const res = await fetch(`/api/public-users/${publicUser.userId}/cron-jobs/${jobId}/results`, {
        headers: authHeaders,
      });
      if (res.ok) {
        const results = await res.json();
        setJobResults((prev) => ({ ...prev, [jobId]: results }));
      } else {
        setJobResults((prev) => ({ ...prev, [jobId]: [] }));
      }
    } catch {
      setJobResults((prev) => ({ ...prev, [jobId]: [] }));
    }
    setResultsLoadingJob(null);
  };

  // ── Formatting helpers ──
  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const formatTime = (isoStr: string) => {
    try {
      return new Date(isoStr).toLocaleString('zh-CN', {
        month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false,
      });
    } catch { return isoStr; }
  };

  const formatFullTime = (isoStr: string) => {
    try {
      const d = new Date(isoStr);
      const pad = (n: number) => String(n).padStart(2, '0');
      return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
    } catch { return isoStr; }
  };

  const formatModelName = (modelStr?: string) => {
    if (!modelStr) return '';
    const parts = modelStr.split(':');
    return parts.length > 1 ? parts[1] : modelStr;
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

  // Determine if a file is HTML or Markdown (for preview/source toggle)
  const isHtmlOrMd = (filePath: string): boolean => {
    const ext = filePath.split('.').pop()?.toLowerCase();
    return ext === 'html' || ext === 'htm' || ext === 'md';
  };

  // Render Markdown content with styled components (mirrors Documents.tsx)
  const renderMarkdownPreview = (content: string): React.ReactNode => {
    return (
      <div className="pdf-markdown-preview" style={{
        fontSize: 14,
        lineHeight: 1.7,
        color: '#c9d1d9',
      }}>
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          components={{
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
          {content}
        </ReactMarkdown>
      </div>
    );
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minWidth: 0 }}>
      {/* Header */}
      <div style={{
        padding: '12px 16px', borderBottom: '1px solid #333', flexShrink: 0,
        display: 'flex', alignItems: 'center', gap: 10,
      }}>
        <span style={{ fontSize: 18 }}>🌐</span>
        <span style={{ fontWeight: 600, fontSize: 16, color: '#e6edf3' }}>{publicUser.username}</span>
        <span style={{
          fontSize: 10, padding: '1px 8px', borderRadius: 8, fontWeight: 600,
          background: '#1f6feb22', color: '#58a6ff', border: '1px solid #1f6feb55', flexShrink: 0,
        }}>
          {t('readOnly')}
        </span>
        <div style={{ flex: 1 }} />
        <button
          onClick={onClose}
          style={{
            padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
            background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12, flexShrink: 0,
          }}
        >
          {t('close')}
        </button>
      </div>

      {/* Tabs */}
      <div style={{ display: 'flex', gap: 4, padding: '8px 16px 0', borderBottom: '1px solid #333', flexShrink: 0 }}>
        {(['docs', 'tasks'] as const).map((key) => (
          <button
            key={key}
            onClick={() => setTab(key)}
            style={{
              padding: '6px 16px', borderRadius: '4px 4px 0 0', border: 'none',
              borderBottom: tab === key ? '2px solid #1f6feb' : '2px solid transparent',
              background: 'transparent', color: tab === key ? '#fff' : '#888',
              cursor: 'pointer', fontSize: 12, fontWeight: tab === key ? 600 : 400,
            }}
          >
            {key === 'docs' ? `📄 ${t('publicDocs')}` : `📋 ${t('publicTasks')}`}
          </button>
        ))}
      </div>

      {/* Content */}
      <div style={{ flex: 1, overflow: 'hidden', display: 'flex' }}>
        {tab === 'docs' ? (
          // ── Public Documents: dual-pane layout (list + preview) ──
          <div style={{ display: 'flex', width: '100%', height: '100%', overflow: 'hidden' }}>
            {/* Left: document list */}
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
                <span style={{ fontWeight: 600, fontSize: 15 }}>{t('publicDocs')}</span>
                <span style={{ fontSize: 11, color: '#888' }}>
                  {docs.length} {docs.length === 1 ? 'file' : 'files'}
                </span>
              </div>
              <div style={{
                flex: 1, overflowY: 'auto', padding: '8px 4px',
              }}>
                {docsLoading ? (
                  <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
                ) : docs.length === 0 ? (
                  <div style={{
                    color: '#666', padding: 24, textAlign: 'center',
                    border: '1px dashed #444', borderRadius: 8, margin: 8,
                  }}>
                    {t('noPublicDocs')}
                  </div>
                ) : (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 2 }}>
                    {docs.map((doc) => (
                      <div
                        key={doc.file_path}
                        className="public-doc-row"
                        onClick={() => openDocPreview(doc)}
                        data-selected={previewDoc?.file_path === doc.file_path || undefined}
                        style={{
                          padding: '6px 10px', borderRadius: 4, background: 'transparent',
                          border: '1px solid transparent', cursor: 'pointer',
                          display: 'flex', alignItems: 'center', gap: 8,
                          transition: 'background 0.1s, border 0.1s',
                        }}
                      >
                        <span style={{ fontSize: 14, flexShrink: 0 }}>{getFileIcon(doc.filename)}</span>
                        <div style={{ flex: 1, minWidth: 0 }}>
                          <div style={{
                            fontSize: 13, fontWeight: 500, color: '#e6edf3',
                            overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                          }} title={doc.file_path}>
                            {doc.filename}
                          </div>
                          <div style={{ fontSize: 11, color: '#888', display: 'flex', gap: 6, marginTop: 1, flexWrap: 'wrap' }}>
                            <span>{formatSize(doc.size)}</span>
                            <span>{formatTime(doc.indexed_at)}</span>
                            {doc.dir_path && doc.dir_path !== '.' && (
                              <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                                {doc.dir_path}
                              </span>
                            )}
                          </div>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>

            {/* Right: preview panel */}
            <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0 }}>
              {previewDoc ? (
                <>
                  {/* Preview header */}
                  <div style={{
                    padding: '8px 16px',
                    borderBottom: '1px solid #333',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'space-between',
                    gap: 8,
                  }}>
                    <span style={{ fontSize: 13, color: '#aaa', fontFamily: 'monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', display: 'flex', alignItems: 'center', gap: 6 }}>
                      {previewDoc.filename.toLowerCase().endsWith('.md') && (
                        <span style={{
                          fontSize: 10, padding: '1px 6px', borderRadius: 3,
                          background: '#8957e5', color: '#fff', fontWeight: 600, flexShrink: 0,
                        }}>MARKDOWN</span>
                      )}
                      {(previewDoc.filename.toLowerCase().endsWith('.html') || previewDoc.filename.toLowerCase().endsWith('.htm')) && (
                        <span style={{
                          fontSize: 10, padding: '1px 6px', borderRadius: 3,
                          background: '#1f6feb', color: '#fff', fontWeight: 600, flexShrink: 0,
                        }}>HTML</span>
                      )}
                      {previewDoc.file_path}
                    </span>
                    <div style={{ display: 'flex', gap: 6, flexShrink: 0 }}>
                      {/* Preview / Source toggle for HTML and Markdown */}
                      {isHtmlOrMd(previewDoc.filename) && (
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
                      )}
                      <span style={{ fontSize: 11, color: '#888', alignSelf: 'center', flexShrink: 0 }}>
                        {formatSize(previewDoc.size)}
                      </span>
                      <button
                        onClick={() => {
                          setPreviewDoc(null);
                          setPreviewContent('');
                        }}
                        style={{
                          padding: '3px 10px', borderRadius: 4, border: '1px solid #555',
                          background: 'transparent', color: '#888', cursor: 'pointer', fontSize: 12,
                        }}
                      >
                        {t('close')}
                      </button>
                    </div>
                  </div>

                  {/* Preview content */}
                  <div
                    ref={previewScrollRef}
                    style={{ flex: 1, overflow: 'auto', padding: 16 }}
                    onScroll={(e) => {
                      const el = e.currentTarget;
                      if (previewNextOffset !== null && el.scrollTop + el.clientHeight >= el.scrollHeight - 200) {
                        loadMorePreview();
                      }
                    }}
                  >
                    {previewLoading ? (
                      <div style={{ color: '#666', textAlign: 'center', paddingTop: 40 }}>{t('loading')}</div>
                    ) : previewError ? (
                      <div style={{ color: '#e57373', textAlign: 'center', paddingTop: 40 }}>{previewError}</div>
                    ) : previewDoc.filename.toLowerCase().endsWith('.md') && htmlViewMode === 'preview' ? (
                      // Markdown rendered preview
                      <>
                        {renderMarkdownPreview(previewContent)}
                        {previewNextOffset !== null && (
                          <div style={{ textAlign: 'center', padding: '12px 0' }}>
                            <button
                              onClick={loadMorePreview}
                              disabled={previewLoadingMore}
                              style={{
                                padding: '6px 16px', borderRadius: 6, border: '1px solid #555',
                                background: 'transparent', color: previewLoadingMore ? '#555' : '#aaa',
                                cursor: previewLoadingMore ? 'not-allowed' : 'pointer', fontSize: 12,
                              }}
                            >
                              {previewLoadingMore ? t('loading') : t('loadMore')}
                            </button>
                          </div>
                        )}
                      </>
                    ) : (previewDoc.filename.toLowerCase().endsWith('.html') || previewDoc.filename.toLowerCase().endsWith('.htm')) && htmlViewMode === 'preview' ? (
                      // HTML iframe preview
                      <iframe
                        srcDoc={previewContent}
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
                      // Source code / plain text
                      <>
                        <pre style={{
                          margin: 0, fontSize: 13, lineHeight: 1.6, color: '#c9d1d9',
                          whiteSpace: 'pre-wrap', wordBreak: 'break-word',
                          fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace",
                        }}>
                          {previewContent}
                        </pre>
                        {previewNextOffset !== null && (
                          <div style={{ textAlign: 'center', padding: '12px 0' }}>
                            <button
                              onClick={loadMorePreview}
                              disabled={previewLoadingMore}
                              style={{
                                padding: '6px 16px', borderRadius: 6, border: '1px solid #555',
                                background: 'transparent', color: previewLoadingMore ? '#555' : '#aaa',
                                cursor: previewLoadingMore ? 'not-allowed' : 'pointer', fontSize: 12,
                              }}
                            >
                              {previewLoadingMore ? t('loading') : t('loadMore')}
                            </button>
                          </div>
                        )}
                      </>
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
                    <div style={{ fontSize: 48, marginBottom: 16 }}>📄</div>
                    <div>{t('selectFilePreview')}</div>
                    <div style={{ fontSize: 12, color: '#333', marginTop: 8 }}>
                      {t('clickFolderToExpand')}
                    </div>
                  </div>
                </div>
              )}
            </div>
          </div>
        ) : (
          // ── Periodic Tasks tab ──
          <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
            {jobsLoading ? (
              <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
            ) : jobs.length === 0 ? (
              <div style={{
                color: '#666', padding: 24, textAlign: 'center',
                border: '1px dashed #444', borderRadius: 8,
              }}>
                {t('noPublicTasks')}
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
                {jobs.map((job) => {
                  const isExpanded = expandedJob === job.id;
                  const results = jobResults[job.id] || [];
                  const isLoading = resultsLoadingJob === job.id;
                  return (
                    <div key={job.id} style={{
                      borderRadius: 8, background: '#2a2a2a', border: '1px solid #444',
                      overflow: 'hidden',
                    }}>
                      <div
                        onClick={() => toggleJobResults(job.id)}
                        style={{ padding: '12px 16px', cursor: 'pointer' }}
                      >
                        <div style={{ fontWeight: 500, marginBottom: 4, fontSize: 14, display: 'flex', alignItems: 'center', gap: 6 }}>
                          <span style={{ fontSize: 10, color: '#888', flexShrink: 0 }}>{isExpanded ? '▼' : '▶'}</span>
                          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                            {job.name || job.prompt}
                          </span>
                        </div>
                        <div style={{ fontSize: 12, color: '#888', display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
                          <span style={{
                            display: 'inline-block', padding: '1px 6px', borderRadius: 4,
                            fontSize: 11, fontWeight: 600, color: '#81c784', background: '#81c78420',
                          }}>
                            {formatFrequency(job.schedule, t as TranslationFn)}
                          </span>
                          {job.model && (
                            <span style={{
                              display: 'inline-block', padding: '1px 6px', borderRadius: 4,
                              fontSize: 11, fontWeight: 600, color: '#6a9fd8', background: '#6a9fd820',
                            }}>
                              {formatModelName(job.model)}
                            </span>
                          )}
                          {job.last_run && (
                            <span>{t('lastRunLabel')}: {formatTime(job.last_run)}</span>
                          )}
                        </div>
                        {job.name && job.prompt && (
                          <div style={{ fontSize: 12, color: '#666', marginTop: 4, lineHeight: 1.4 }}>
                            {job.prompt.length > 100 ? job.prompt.slice(0, 100) + '...' : job.prompt}
                          </div>
                        )}
                      </div>

                      {isExpanded && (
                        <div style={{
                          borderTop: '1px solid #333', background: '#1e1e1e',
                          padding: '8px 12px', display: 'flex', flexDirection: 'column', gap: 10,
                          maxHeight: 420, overflowY: 'auto',
                        }}>
                          {isLoading ? (
                            <div style={{ color: '#555', fontSize: 12, textAlign: 'center', padding: 10 }}>
                              {t('loading')}
                            </div>
                          ) : results.length === 0 ? (
                            <div style={{ color: '#555', fontSize: 12, textAlign: 'center', padding: 10 }}>
                              {t('noResults')}
                            </div>
                          ) : (
                            results.map((result) => (
                              <div key={result.id} style={{
                                borderRadius: 6, border: '1px solid #333', background: '#161b22',
                                overflow: 'hidden',
                              }}>
                                <div style={{
                                  display: 'flex', alignItems: 'center', gap: 8,
                                  padding: '6px 10px', borderBottom: '1px solid #333',
                                }}>
                                  <span style={{
                                    width: 8, height: 8, borderRadius: '50%', flexShrink: 0,
                                    background: result.status === 'success' ? '#81c784' : '#e57373',
                                  }} />
                                  <span style={{ flex: 1, fontSize: 12, color: '#ccc' }}>
                                    {formatFullTime(result.executed_at)}
                                  </span>
                                  <span style={{
                                    fontSize: 10, fontWeight: 600, padding: '1px 6px', borderRadius: 4, flexShrink: 0,
                                    color: result.status === 'success' ? '#81c784' : '#e57373',
                                    background: result.status === 'success' ? '#81c78420' : '#e5737320',
                                  }}>
                                    {result.status === 'success' ? t('success') : t('failed')}
                                  </span>
                                </div>
                                <pre style={{
                                  margin: 0, padding: '10px 12px', fontSize: 12, lineHeight: 1.6,
                                  color: '#c9d1d9', whiteSpace: 'pre-wrap', wordBreak: 'break-word',
                                  fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace",
                                  maxHeight: 260, overflow: 'auto',
                                }}>
                                  {result.output}
                                </pre>
                              </div>
                            ))
                          )}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        )}
      </div>

      {/* Inline styles for doc list hover/selection */}
      <style>{`
        .public-doc-row:hover { background: #1e1e1e !important; }
        .public-doc-row[data-selected] { background: #1a3a5a !important; }
        .public-doc-row[data-selected]:hover { background: #1a3a5a !important; }
      `}</style>
    </div>
  );
}
