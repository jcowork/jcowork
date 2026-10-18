import { useState, useEffect, useRef, useCallback } from 'react';
import { useT } from '../i18n';
import { ResultMarkdown } from './Schedule';
import { buildDocUrl } from '../utils/docUrl';

/** Feed items per source page request. */
const PAGE = 20;
/** Items merged before the first paint (≈2+ phone screens). */
const INITIAL_TARGET = 30;
/** Characters fetched per page when previewing a document. */
const DOC_PAGE = 30000;

interface FeedTaskItem {
  type: 'task';
  id: string;
  job_id: string;
  job_name: string | null;
  prompt: string | null;
  excerpt: string;
  /** AI teaser summary; empty until the backend generates it. */
  summary?: string;
  status: string;
  executed_at: string;
}

interface FeedDocItem {
  type: 'document';
  file_path: string;
  dir_path: string;
  filename: string;
  content_type: string;
  size: number;
  snippet: string;
  /** AI teaser summary; empty until the backend generates it. */
  summary?: string;
  /** Owner-marked public flag (share eligibility). */
  is_public?: boolean;
  indexed_at: string;
}

type FeedItem = FeedTaskItem | FeedDocItem;

interface FeedSource {
  key: string;
  userId: string;
  username: string;
  isLocal: boolean;
  endpoint: string;
  token: string;
  items: FeedItem[];
  ptr: number;
  nextOffset: number;
  exhausted: boolean;
  error: boolean;
}

interface AiFeedProps {
  /** All locally signed-in accounts (each contributes its own /api/feed). */
  accounts: { userId: string; username: string; token: string }[];
  /** Token used for public-user feed requests. */
  activeToken: string;
  /** Public accounts visible to the active account. */
  publicUsers: { userId: string; username: string }[];
}

interface FeedEntry {
  item: FeedItem;
  source: FeedSource;
}

const itemTime = (item: FeedItem): number => {
  const raw = item.type === 'task' ? item.executed_at : item.indexed_at;
  const t = Date.parse(raw);
  return Number.isNaN(t) ? 0 : t;
};

/** Collapse markdown noise into a one-paragraph plain-text summary. */
const plainSummary = (s: string): string =>
  s
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/[#*_`>|]+/g, '')
    .replace(/\s+/g, ' ')
    .trim();

const avatarHue = (name: string): number => {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) % 360;
  return h;
};

const formatFeedTime = (iso: string): string => {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return iso;
  const d = new Date(t);
  const now = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  const hm = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (d.toDateString() === now.toDateString()) return hm;
  const sameYear = d.getFullYear() === now.getFullYear();
  return `${sameYear ? '' : d.getFullYear() + '/'}${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${hm}`;
};

const formatSize = (bytes: number): string => {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
};

interface TaskDetailData {
  output: string;
  status: string;
  executed_at: string;
  job_name: string | null;
  prompt: string | null;
}

/**
 * The mobile "AI头条" tab: a merged, time-sorted stream of periodic-task
 * results and documents from every locally signed-in account (their own
 * data) and every public user (public data), loaded incrementally.
 *
 * Merging is an exact k-way merge: items are taken one at a time from the
 * source with the newest buffered head; when a source's buffer runs dry but
 * has more pages, the next page is fetched before the merge continues.
 */
export default function AiFeed({ accounts, activeToken, publicUsers }: AiFeedProps) {
  const t = useT();
  const sourcesRef = useRef<FeedSource[]>([]);
  const entriesRef = useRef<FeedEntry[]>([]);
  const targetRef = useRef(0);
  const ensuringRef = useRef(false);
  const [entries, setEntries] = useState<FeedEntry[]>([]);
  const [busy, setBusy] = useState(false);
  const [sourceStats, setSourceStats] = useState<{ failed: FeedSource[]; allDone: boolean }>({
    failed: [],
    allDone: false,
  });
  const [detail, setDetail] = useState<FeedEntry | null>(null);
  const [linkCopied, setLinkCopied] = useState(false);

  // ── Task detail state ──
  const [taskDetail, setTaskDetail] = useState<TaskDetailData | null>(null);
  const [taskDetailLoading, setTaskDetailLoading] = useState(false);
  const [taskDetailError, setTaskDetailError] = useState('');

  // ── Document detail state ──
  const [docContent, setDocContent] = useState('');
  const [docNextOffset, setDocNextOffset] = useState<number | null>(null);
  const [docLoading, setDocLoading] = useState(false);
  const [docError, setDocError] = useState('');

  const fetchPage = useCallback(async (source: FeedSource): Promise<void> => {
    try {
      const res = await fetch(`${source.endpoint}?offset=${source.nextOffset}&limit=${PAGE}`, {
        headers: { Authorization: `Bearer ${source.token}` },
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      const items: FeedItem[] = Array.isArray(data.items) ? data.items : [];
      source.items = source.items.concat(items);
      source.nextOffset = Number(data.next_offset) || source.nextOffset + items.length;
      source.exhausted = !data.has_more;
      source.error = false;
    } catch {
      source.error = true;
      source.exhausted = true; // stop auto-retrying a failing source
    }
  }, []);

  /** Grow the merged stream until at least `n` items exist (or all sources
   *  are exhausted). Concurrent calls coalesce through `targetRef`. */
  const ensureCount = useCallback(
    async (n: number): Promise<void> => {
      if (targetRef.current >= n) return;
      targetRef.current = Math.max(targetRef.current, n);
      if (ensuringRef.current) return;
      ensuringRef.current = true;
      setBusy(true);
      try {
        while (true) {
          if (entriesRef.current.length >= targetRef.current) break;
          const needFetch = sourcesRef.current.filter(
            (s) => s.ptr >= s.items.length && !s.exhausted && !s.error,
          );
          if (needFetch.length > 0) {
            await Promise.all(needFetch.map((s) => fetchPage(s)));
            continue;
          }
          const avail = sourcesRef.current.filter((s) => s.ptr < s.items.length);
          if (avail.length === 0) break; // every source is done or failed
          let best = avail[0];
          let bestT = itemTime(best.items[best.ptr]);
          for (const s of avail) {
            const t2 = itemTime(s.items[s.ptr]);
            if (t2 > bestT) {
              best = s;
              bestT = t2;
            }
          }
          entriesRef.current = [...entriesRef.current, { item: best.items[best.ptr], source: best }];
          best.ptr++;
        }
      } finally {
        ensuringRef.current = false;
        setBusy(false);
        setEntries(entriesRef.current);
        setSourceStats({
          failed: sourcesRef.current.filter((s) => s.error),
          allDone:
            sourcesRef.current.length > 0 &&
            sourcesRef.current.every((s) => s.exhausted || s.error),
        });
        if (entriesRef.current.length < targetRef.current) {
          void ensureCount(targetRef.current);
        }
      }
    },
    [fetchPage],
  );

  /** Rebuild the source set (accounts / public users changed) and reload. */
  const reload = useCallback(() => {
    const localIds = new Set(accounts.map((a) => a.userId));
    const sources: FeedSource[] = accounts.map((a) => ({
      key: `acct:${a.userId}`,
      userId: a.userId,
      username: a.username,
      isLocal: true,
      endpoint: '/api/feed',
      token: a.token,
      items: [],
      ptr: 0,
      nextOffset: 0,
      exhausted: false,
      error: false,
    }));
    for (const u of publicUsers) {
      // A user that is also signed in locally is already covered by their
      // own /api/feed (which includes private documents) — skip the public copy.
      if (localIds.has(u.userId)) continue;
      sources.push({
        key: `pub:${u.userId}`,
        userId: u.userId,
        username: u.username,
        isLocal: false,
        endpoint: `/api/public-users/${u.userId}/feed`,
        token: activeToken,
        items: [],
        ptr: 0,
        nextOffset: 0,
        exhausted: false,
        error: false,
      });
    }
    sourcesRef.current = sources;
    entriesRef.current = [];
    targetRef.current = 0;
    setEntries([]);
    setSourceStats({ failed: [], allDone: false });
    void ensureCount(INITIAL_TARGET);
  }, [accounts, publicUsers, activeToken, ensureCount]);

  useEffect(() => {
    const id = window.setTimeout(reload, 0);
    return () => window.clearTimeout(id);
  }, [reload]);

  const failedSources = sourceStats.failed;
  const allDone = sourceStats.allDone;
  const initialLoading = busy && entries.length === 0;

  // ── Detail: task result (full output) ──
  const openDetail = (entry: FeedEntry) => {
    setDetail(entry);
    setTaskDetail(null);
    setTaskDetailError('');
    setDocContent('');
    setDocNextOffset(null);
    setDocError('');
    if (entry.item.type === 'task') {
      setTaskDetailLoading(true);
      fetch(`/api/feed/results/${entry.item.id}`, {
        headers: { Authorization: `Bearer ${entry.source.token}` },
      })
        .then(async (res) => {
          const data = await res.json().catch(() => ({}));
          if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
          setTaskDetail({
            output: data.output || '',
            status: data.status || '',
            executed_at: data.executed_at || '',
            job_name: data.job_name ?? null,
            prompt: data.prompt ?? null,
          });
        })
        .catch((e) => setTaskDetailError(String(e.message || e)))
        .finally(() => setTaskDetailLoading(false));
    } else {
      void loadDocPage(entry, 0);
    }
  };

  // ── Detail: document content (paged) ──
  const loadDocPage = async (entry: FeedEntry, offset: number) => {
    if (entry.item.type !== 'document') return;
    setDocLoading(true);
    setDocError('');
    const url = entry.source.isLocal
      ? `/api/workspace/index/content?path=${encodeURIComponent(entry.item.file_path)}&offset=${offset}&limit=${DOC_PAGE}`
      : `/api/public-users/${entry.source.userId}/documents/content?path=${encodeURIComponent(entry.item.file_path)}&offset=${offset}&limit=${DOC_PAGE}`;
    try {
      const res = await fetch(url, { headers: { Authorization: `Bearer ${entry.source.token}` } });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      setDocContent((prev) => (offset === 0 ? data.content || '' : prev + (data.content || '')));
      setDocNextOffset(data.has_more ? data.next_offset : null);
    } catch (e) {
      setDocError(String((e as Error).message || e));
    } finally {
      setDocLoading(false);
    }
  };

  const detailIsMd = (): boolean => {
    if (!detail || detail.item.type !== 'document') return false;
    return /\.(md|markdown)$/i.test(detail.item.filename);
  };
  const detailIsHtml = (): boolean => {
    if (!detail || detail.item.type !== 'document') return false;
    return /\.(html?|htm)$/i.test(detail.item.filename);
  };

  // ── Standalone / share link for a document detail ──
  const docStandaloneUrl = (): string => {
    if (!detail || detail.item.type !== 'document') return '';
    const item = detail.item;
    // Feed entries from public accounts are always public documents; local
    // documents only when their owner flagged them so. Private ones still
    // open standalone, but the viewer needs a signed-in account.
    const pub = detail.source.isLocal ? item.is_public === true : true;
    return buildDocUrl(detail.source.userId, detail.source.username, item.file_path, pub);
  };

  const openStandalone = () => {
    const url = docStandaloneUrl();
    if (url) window.open(url, '_blank');
  };

  const shareDoc = async () => {
    const url = docStandaloneUrl();
    if (!url) return;
    if (navigator.share) {
      try {
        await navigator.share({ title: detail?.item.type === 'document' ? detail.item.filename : '', url });
        return;
      } catch (e) {
        if ((e as Error).name === 'AbortError') return;
        // Other failures fall through to the clipboard copy.
      }
    }
    try {
      await navigator.clipboard.writeText(url);
      setLinkCopied(true);
      window.setTimeout(() => setLinkCopied(false), 1500);
    } catch {
      // Clipboard unavailable — the standalone URL is still copyable by hand.
    }
  };

  const renderCard = (entry: FeedEntry, idx: number) => {
    const { item, source } = entry;
    const isTask = item.type === 'task';
    const time = isTask ? item.executed_at : item.indexed_at;
    const title = isTask ? item.job_name || item.prompt || t('feedTask') : item.filename;
    const summary = plainSummary(item.summary || (isTask ? item.excerpt : item.snippet));
    return (
      <div
        key={`${source.key}:${isTask ? item.id : item.file_path}:${idx}`}
        className="feed-card"
        onClick={() => openDetail(entry)}
        style={{
          padding: '10px 14px',
          borderBottom: '1px solid #222',
          background: '#161616',
          cursor: 'pointer',
        }}
      >
        <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 6 }}>
          <span
            style={{
              width: 20, height: 20, borderRadius: '50%', flexShrink: 0,
              display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
              background: `hsl(${avatarHue(source.username)}, 45%, 32%)`,
              color: '#eee', fontSize: 11, fontWeight: 700,
            }}
          >
            {source.username.charAt(0).toUpperCase()}
          </span>
          <span
            style={{ flex: 1, minWidth: 0, fontSize: 12, color: '#888', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
          >
            {source.username}
          </span>
          {!source.isLocal && (
            <span style={{ fontSize: 9, color: '#58a6ff', background: '#1f6feb22', border: '1px solid #1f6feb55', borderRadius: 8, padding: '0 6px', flexShrink: 0 }}>
              {t('publicBadge')}
            </span>
          )}
          <span style={{ fontSize: 11, color: '#666', flexShrink: 0 }}>{formatFeedTime(time)}</span>
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <span style={{ fontSize: 16, flexShrink: 0, lineHeight: '20px' }}>{isTask ? '📋' : '📄'}</span>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="clamp-1" style={{ fontSize: 14, fontWeight: 600, color: '#e6edf3', lineHeight: 1.4 }}>{title}</div>
            {summary && (
              <div className="clamp-2" style={{ fontSize: 13, color: '#8b949e', lineHeight: 1.5, marginTop: 2 }}>{summary}</div>
            )}
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 4, fontSize: 11, color: '#666' }}>
              <span style={{ padding: '0 6px', borderRadius: 4, background: '#262626', color: '#999', fontWeight: 600 }}>
                {isTask ? t('feedTask') : t('feedDocument')}
              </span>
              {isTask ? (
                item.status !== 'success' && <span style={{ color: '#e57373' }}>{t('failed')}</span>
              ) : (
                <span>{formatSize(item.size)}</span>
              )}
            </div>
          </div>
        </div>
      </div>
    );
  };

  const detailHeader = detail && (
    <div style={{
      padding: '10px 14px', borderBottom: '1px solid #333', flexShrink: 0,
      display: 'flex', alignItems: 'center', gap: 10, background: '#1a1a1a',
    }}>
      <button
        onClick={() => setDetail(null)}
        aria-label={t('close')}
        style={{ background: 'none', border: 'none', color: '#ccc', fontSize: 20, cursor: 'pointer', padding: '0 4px', flexShrink: 0 }}
      >
        ✕
      </button>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div className="clamp-1" style={{ fontSize: 14, fontWeight: 600, color: '#e6edf3' }}>
          {detail.item.type === 'task'
            ? detail.item.job_name || detail.item.prompt || t('feedTask')
            : detail.item.filename}
        </div>
        <div style={{ fontSize: 11, color: '#777', display: 'flex', gap: 6, alignItems: 'center' }}>
          <span>{detail.source.username}</span>
          <span>·</span>
          <span>{formatFeedTime(detail.item.type === 'task' ? detail.item.executed_at : detail.item.indexed_at)}</span>
          {detail.item.type === 'task' && detail.item.status !== 'success' && (
            <span style={{ color: '#e57373' }}>{t('failed')}</span>
          )}
        </div>
      </div>
      {detail.item.type === 'document' && (
        <div style={{ display: 'flex', gap: 6, flexShrink: 0 }}>
          <button
            onClick={openStandalone}
            style={{
              padding: '4px 10px', borderRadius: 6, border: '1px solid #555',
              background: 'transparent', color: '#aaa', cursor: 'pointer',
              fontSize: 12, whiteSpace: 'nowrap',
            }}
          >
            {t('openStandalone')}
          </button>
          <button
            onClick={() => void shareDoc()}
            style={{
              padding: '4px 10px', borderRadius: 6, border: '1px solid #555',
              background: 'transparent', color: linkCopied ? '#7bd88f' : '#aaa',
              cursor: 'pointer', fontSize: 12, whiteSpace: 'nowrap',
            }}
          >
            {linkCopied ? t('linkCopied') : t('shareDoc')}
          </button>
        </div>
      )}
    </div>
  );

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', background: '#111', minWidth: 0 }}>
      {/* Header */}
      <div style={{
        padding: '10px 14px', borderBottom: '1px solid #222', flexShrink: 0,
        display: 'flex', alignItems: 'center', gap: 8,
      }}>
        <span style={{ fontSize: 17, fontWeight: 700, color: '#eee' }}>📰 {t('tabFeed')}</span>
        <div style={{ flex: 1 }} />
        <button
          onClick={reload}
          style={{
            padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
            background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12,
          }}
        >
          {busy ? t('feedLoadingMore') : t('refresh')}
        </button>
      </div>

      {/* Source failures */}
      {failedSources.length > 0 && (
        <div style={{ padding: '6px 14px', background: '#3d1c1c', borderBottom: '1px solid #5a2a2a', fontSize: 12, color: '#f5a5a5' }}>
          {t('feedSourceError')} ({failedSources.map((s) => s.username).join(', ')})
        </div>
      )}

      {/* Card list — scroll container drives incremental loading */}
      <div
        style={{ flex: 1, overflowY: 'auto', WebkitOverflowScrolling: 'touch' }}
        onScroll={(e) => {
          const el = e.currentTarget;
          if (el.scrollTop + el.clientHeight >= el.scrollHeight - 300) {
            void ensureCount(entriesRef.current.length + PAGE);
          }
        }}
      >
        {initialLoading ? (
          <div style={{ color: '#666', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('loading')}</div>
        ) : entries.length === 0 ? (
          <div style={{ padding: '60px 24px', textAlign: 'center' }}>
            <div style={{ fontSize: 44, marginBottom: 12 }}>📰</div>
            <div style={{ color: '#777', fontSize: 14 }}>{t('feedEmpty')}</div>
            <div style={{ color: '#555', fontSize: 12, marginTop: 8, lineHeight: 1.6 }}>{t('feedEmptyHint')}</div>
          </div>
        ) : (
          <>
            {entries.map(renderCard)}
            {busy && (
              <div style={{ color: '#666', padding: 14, textAlign: 'center', fontSize: 12 }}>{t('feedLoadingMore')}</div>
            )}
            {allDone && !busy && (
              <div style={{ color: '#555', padding: 16, textAlign: 'center', fontSize: 12 }}>{t('feedNoMore')}</div>
            )}
          </>
        )}
      </div>

      {/* Detail overlay */}
      {detail && (
        <div style={{ position: 'fixed', inset: 0, zIndex: 1000, background: '#0d1117', display: 'flex', flexDirection: 'column' }}>
          {detailHeader}
          <div
            style={{
              flex: 1, minHeight: 0,
              overflowY: detailIsHtml() ? 'hidden' : 'auto',
              WebkitOverflowScrolling: 'touch',
              display: 'flex', flexDirection: 'column',
            }}
            onScroll={(e) => {
              if (detail.item.type !== 'document' || detailIsHtml()) return;
              const el = e.currentTarget;
              if (docNextOffset !== null && el.scrollTop + el.clientHeight >= el.scrollHeight - 200) {
                loadDocPage(detail, docNextOffset);
              }
            }}
          >
            {detail.item.type === 'task' ? (
              taskDetailLoading ? (
                <div style={{ color: '#666', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('loading')}</div>
              ) : taskDetailError ? (
                <div style={{ color: '#e57373', padding: 40, textAlign: 'center', fontSize: 13 }}>{taskDetailError}</div>
              ) : (
                <div style={{ padding: '16px 16px 32px' }}>
                  {taskDetail?.prompt && (
                    <div className="clamp-3" style={{ fontSize: 12, color: '#777', lineHeight: 1.6, marginBottom: 14, borderLeft: '3px solid #1f6feb', paddingLeft: 10 }}>
                      {taskDetail.prompt}
                    </div>
                  )}
                  <ResultMarkdown content={taskDetail?.output || ''} />
                </div>
              )
            ) : (
              <div style={detailIsHtml()
                ? { flex: 1, minHeight: 0, display: 'flex', flexDirection: 'column', padding: '8px 8px 0' }
                : { padding: '16px 16px 32px' }}>
                {docLoading && docContent === '' ? (
                  <div style={{ color: '#666', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('loading')}</div>
                ) : docError ? (
                  <div style={{ color: '#e57373', padding: 40, textAlign: 'center', fontSize: 13 }}>{docError}</div>
                ) : detailIsHtml() ? (
                  <iframe
                    srcDoc={docContent}
                    title="HTML Preview"
                    sandbox="allow-scripts allow-same-origin"
                    style={{ flex: 1, width: '100%', height: '100%', minHeight: 0, border: 'none', borderRadius: 8, background: '#fff' }}
                  />
                ) : detailIsMd() ? (
                  <ResultMarkdown content={docContent} />
                ) : (
                  <pre style={{ margin: 0, fontSize: 13, lineHeight: 1.6, color: '#c9d1d9', whiteSpace: 'pre-wrap', wordBreak: 'break-word', fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace" }}>
                    {docContent}
                  </pre>
                )}
                {docNextOffset !== null && (
                  <div style={{ textAlign: 'center', padding: '12px 0', flexShrink: 0 }}>
                    <button
                      onClick={() => detail && loadDocPage(detail, docNextOffset)}
                      disabled={docLoading}
                      style={{
                        padding: '6px 16px', borderRadius: 6, border: '1px solid #555',
                        background: 'transparent', color: docLoading ? '#555' : '#aaa',
                        cursor: docLoading ? 'not-allowed' : 'pointer', fontSize: 12,
                      }}
                    >
                      {docLoading ? t('loading') : t('loadMore')}
                    </button>
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
