import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';
import { formatFrequency, type TranslationFn } from '../utils/cron';

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
      <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
        {tab === 'docs' ? (
          docsLoading ? (
            <div style={{ color: '#666', padding: 16, textAlign: 'center' }}>{t('loading')}</div>
          ) : docs.length === 0 ? (
            <div style={{
              color: '#666', padding: 24, textAlign: 'center',
              border: '1px dashed #444', borderRadius: 8,
            }}>
              {t('noPublicDocs')}
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {docs.map((doc) => (
                <div
                  key={doc.file_path}
                  onClick={() => openDocPreview(doc)}
                  className="public-doc-row"
                  style={{
                    padding: '10px 14px', borderRadius: 8, background: '#2a2a2a',
                    border: '1px solid #444', cursor: 'pointer',
                    display: 'flex', alignItems: 'center', gap: 10,
                  }}
                >
                  <span style={{ fontSize: 16, flexShrink: 0 }}>📄</span>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{
                      fontSize: 13, fontWeight: 500, color: '#e6edf3',
                      overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                    }} title={doc.file_path}>
                      {doc.filename}
                    </div>
                    <div style={{ fontSize: 11, color: '#888', display: 'flex', gap: 8, marginTop: 2, flexWrap: 'wrap' }}>
                      <span>{formatSize(doc.size)}</span>
                      <span>{formatTime(doc.indexed_at)}</span>
                      {doc.dir_path && doc.dir_path !== '.' && (
                        <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                          {doc.dir_path}
                        </span>
                      )}
                    </div>
                  </div>
                  <span style={{ fontSize: 11, color: '#58a6ff', flexShrink: 0 }}>{t('preview')}</span>
                </div>
              ))}
            </div>
          )
        ) : (
          jobsLoading ? (
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
          )
        )}
      </div>

      {/* Document preview modal (read-only) */}
      {previewDoc && (
        <div
          onClick={() => setPreviewDoc(null)}
          style={{
            position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.6)', zIndex: 1000,
            display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 24,
          }}
        >
          <div
            onClick={(e) => e.stopPropagation()}
            style={{
              background: '#1e1e1e', borderRadius: 12, border: '1px solid #444',
              boxShadow: '0 8px 32px rgba(0,0,0,0.6)', width: '100%', maxWidth: 900,
              maxHeight: '85vh', display: 'flex', flexDirection: 'column',
            }}
          >
            <div style={{
              padding: '12px 16px', borderBottom: '1px solid #333', flexShrink: 0,
              display: 'flex', alignItems: 'center', gap: 10,
            }}>
              <span style={{ fontSize: 14 }}>📄</span>
              <span style={{
                flex: 1, minWidth: 0, fontSize: 13, fontWeight: 600, color: '#e6edf3',
                fontFamily: 'monospace', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
              }}>
                {previewDoc.file_path}
              </span>
              <span style={{ fontSize: 11, color: '#888', flexShrink: 0 }}>{formatSize(previewDoc.size)}</span>
              <button
                onClick={() => setPreviewDoc(null)}
                style={{
                  padding: '3px 10px', borderRadius: 4, border: '1px solid #555',
                  background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12, flexShrink: 0,
                }}
              >
                {t('close')}
              </button>
            </div>
            <div
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
              ) : (
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
          </div>
        </div>
      )}
    </div>
  );
}
