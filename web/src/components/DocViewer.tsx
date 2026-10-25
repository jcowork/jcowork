import { useState, useEffect, useRef, useCallback } from 'react';
import { useT } from '../i18n';
import { ResultMarkdown } from './Schedule';
import { loadAccounts } from '../accountStore';
import { parseDocHash } from '../utils/docUrl';
import type { DocViewerParams } from '../utils/docUrl';

/** Characters fetched per page when loading document content. */
const DOC_PAGE = 30000;

/**
 * Full-screen document viewer, reachable through a bare `#/doc` URL so a
 * link can be opened (and shared) independently of the main app. Public
 * documents render without any account; private ones use the first locally
 * signed-in account.
 */
export function DocViewer() {
  const t = useT();
  const [params] = useState<DocViewerParams | null>(() => parseDocHash());
  // Private documents need a locally signed-in account; resolve that up front
  // so the effect below never has to set state synchronously.
  const [needsLogin] = useState(() => {
    const p = parseDocHash();
    return p !== null && !p.pub && loadAccounts().length === 0;
  });
  const [content, setContent] = useState('');
  const [nextOffset, setNextOffset] = useState<number | null>(null);
  const [loading, setLoading] = useState(!needsLogin);
  const [error, setError] = useState('');
  const [copied, setCopied] = useState(false);
  // PDFs offer two views: the extracted (parsed) content and the original
  // file rendered by the platform PDF viewer. `pdfToken` mirrors tokenRef so
  // the original-file iframe URL can carry auth once the account resolves.
  const [viewMode, setViewMode] = useState<'parsed' | 'pdf'>('parsed');
  const [pdfToken, setPdfToken] = useState('');
  // Text documents (.md/.txt/.csv/.html) are editable right in the viewer,
  // matching the old Documents-pane editor (commit 29e5e2f).
  const [editing, setEditing] = useState(false);
  const [editContent, setEditContent] = useState('');
  const [editDirty, setEditDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [savedFlash, setSavedFlash] = useState(false);
  const tokenRef = useRef<string>('');
  // Only shown when the viewer replaced the app in the same tab (e.g. a
  // blocked popup) — then history holds the app to go back to.
  const [canGoBack] = useState(() => window.history.length > 1);

  const load = useCallback(
    async (offset: number) => {
      if (!params) return;
      if (!params.pub && !tokenRef.current) {
        const accounts = loadAccounts();
        const account =
          accounts.find((a) => a.userId === params.userId) || accounts[0];
        if (!account) return; // the needsLogin gate handles this
        tokenRef.current = account.token;
        setPdfToken(account.token);
      }
      setLoading(true);
      setError('');
      const ext = params.path.split('.').pop()?.toLowerCase() || '';
      const isTextLike = ['md', 'markdown', 'txt', 'csv'].includes(ext);
      const isHtmlFile = ext === 'html' || ext === 'htm';
      try {
        if (isTextLike || isHtmlFile) {
          // Text files load the raw file (like the old Documents pane did),
          // so generated documents preview even when they are not indexed.
          const base = params.pub
            ? `/api/public-users/${encodeURIComponent(params.userId)}/documents/download`
            : '/api/workspace/download';
          const q = new URLSearchParams({ path: params.path });
          if (isHtmlFile) q.set('raw', 'true');
          const res = await fetch(`${base}?${q.toString()}`, {
            headers: tokenRef.current ? { Authorization: `Bearer ${tokenRef.current}` } : undefined,
          });
          if (!res.ok) {
            let msg = `HTTP ${res.status}`;
            const data = await res.json().catch(() => ({}));
            if (data.error) msg = data.error;
            throw new Error(msg);
          }
          setContent(await res.text());
          setNextOffset(null);
          return;
        }
        const base = params.pub
          ? `/api/public-users/${encodeURIComponent(params.userId)}/documents/content`
          : '/api/workspace/index/content';
        const url = `${base}?path=${encodeURIComponent(params.path)}&offset=${offset}&limit=${DOC_PAGE}`;
        const res = await fetch(url, {
          headers: tokenRef.current ? { Authorization: `Bearer ${tokenRef.current}` } : undefined,
        });
        const data = await res.json().catch(() => ({}));
        if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
        setContent((prev) => (offset === 0 ? data.content || '' : prev + (data.content || '')));
        setNextOffset(data.has_more ? data.next_offset : null);
      } catch (e) {
        setError(String((e as Error).message || e));
      } finally {
        setLoading(false);
      }
    },
    [params],
  );

  useEffect(() => {
    if (!params || needsLogin) return;
    // Deferred one tick so the fetch's synchronous setState calls don't run
    // inside the effect body (react-hooks/set-state-in-effect).
    const id = window.setTimeout(() => void load(0), 0);
    return () => window.clearTimeout(id);
  }, [params, needsLogin, load]);

  const copyLink = async () => {
    if (!params) return;
    const q = new URLSearchParams({
      u: params.userId,
      n: params.username,
      p: params.path,
      pub: params.pub ? '1' : '0',
    });
    const url = `${window.location.origin}${window.location.pathname}#/doc?${q.toString()}`;
    try {
      await navigator.clipboard.writeText(url);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard API unavailable (non-secure context) — nothing to do.
    }
  };

  const goBack = () => {
    if (window.history.length > 1) {
      window.history.back();
    } else {
      window.location.hash = '';
    }
  };
  
  const startEditing = async () => {
    if (isPdf) {
      // The parsed view is paginated; fetch the full indexed content so the
      // editor always starts from the complete document.
      setLoading(true);
      setError('');
      try {
        const url = `/api/workspace/index/content?path=${encodeURIComponent(params?.path || '')}`;
        const res = await fetch(url, {
          headers: { Authorization: `Bearer ${tokenRef.current}` },
        });
        const data = await res.json().catch(() => ({}));
        if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
        setEditContent(data.content || '');
        setEditDirty(false);
        setEditing(true);
      } catch (e) {
        setError(String((e as Error).message || e));
      } finally {
        setLoading(false);
      }
      return;
    }
    setEditContent(content);
    setEditDirty(false);
    setEditing(true);
  };
  
  const cancelEditing = () => {
    if (editDirty && !window.confirm('有未保存的更改，确定放弃吗？')) return;
    setEditing(false);
    setEditDirty(false);
  };
  
  const saveEdit = async () => {
    if (!params || saving) return;
    setSaving(true);
    try {
      // PDFs: persist the edited parsed text in the workspace index (the raw
      // PDF stays untouched). Text files: overwrite the file itself.
      const res = isPdf
        ? await fetch('/api/workspace/index/content', {
            method: 'PUT',
            headers: {
              Authorization: `Bearer ${tokenRef.current}`,
              'Content-Type': 'application/json',
            },
            body: JSON.stringify({ path: params.path, content: editContent }),
          })
        : await fetch('/api/workspace/save', {
            method: 'POST',
            headers: {
              Authorization: `Bearer ${tokenRef.current}`,
              'Content-Type': 'application/json',
            },
            body: JSON.stringify({ path: params.path, content: editContent }),
          });
      if (res.ok) {
        setEditing(false);
        setEditDirty(false);
        setSavedFlash(true);
        window.setTimeout(() => setSavedFlash(false), 2000);
        void load(0);
      } else {
        const err = await res.json().catch(() => ({}));
        window.alert(err.error || '保存失败');
      }
    } catch {
      window.alert('网络错误，保存失败');
    } finally {
      setSaving(false);
    }
  };
  
  if (!params) return null;
  
  const filename = params.path.split('/').pop() || params.path;
  const isHtml = /\.(html?|htm)$/i.test(params.path);
  const isMd = /\.(md|markdown)$/i.test(params.path);
  const isPdf = /\.pdf$/i.test(params.path);
  const isEditableFile = /\.(md|markdown|txt|csv|html?|htm)$/i.test(params.path);
  // Only the document owner can edit; public share links stay read-only.
  const isOwner = loadAccounts().some((a) => a.userId === params.userId);
  const pdfView = isPdf && viewMode === 'pdf';
  const canEdit =
    !params.pub && isOwner && !error && content !== '' &&
    (isEditableFile || (isPdf && !pdfView));
  // Original-file URL: public docs go through the anonymous public download
  // endpoint; private ones through the workspace download API (token in the
  // query so the iframe request authenticates without headers).
  const pdfUrl = params.pub
    ? `/api/public-users/${encodeURIComponent(params.userId)}/documents/download?path=${encodeURIComponent(params.path)}`
    : `/api/workspace/download?path=${encodeURIComponent(params.path)}&token=${encodeURIComponent(pdfToken)}`;

  return (
    <div style={{ position: 'fixed', inset: 0, background: '#0d1117', display: 'flex', flexDirection: 'column' }}>
      <div style={{
        padding: '10px 14px', borderBottom: '1px solid #333', flexShrink: 0,
        display: 'flex', alignItems: 'center', gap: 10, background: '#1a1a1a',
        paddingTop: 'calc(10px + env(safe-area-inset-top))',
      }}>
        <span style={{ fontSize: 18, flexShrink: 0 }}>📄</span>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="clamp-1" style={{ fontSize: 14, fontWeight: 600, color: '#e6edf3' }}>{filename}</div>
          <div className="clamp-1" style={{ fontSize: 11, color: '#777' }}>{params.username}</div>
        </div>
        {isPdf && !editing && (
          <div style={{ display: 'flex', borderRadius: 4, border: '1px solid #555', overflow: 'hidden', flexShrink: 0 }}>
            <button
              onClick={() => setViewMode('parsed')}
              style={{
                padding: '3px 10px', border: 'none', cursor: 'pointer', fontSize: 12,
                background: viewMode === 'parsed' ? '#2d6cdf' : 'transparent',
                color: viewMode === 'parsed' ? '#fff' : '#aaa',
              }}
            >
              {t('parsedContent')}
            </button>
            <button
              onClick={() => setViewMode('pdf')}
              style={{
                padding: '3px 10px', border: 'none', cursor: 'pointer', fontSize: 12,
                background: viewMode === 'pdf' ? '#2d6cdf' : 'transparent',
                color: viewMode === 'pdf' ? '#fff' : '#aaa',
              }}
            >
              {t('originalPdf')}
            </button>
          </div>
        )}
        {editing ? (
          <>
            {savedFlash && <span style={{ fontSize: 12, color: '#7bd88f', flexShrink: 0 }}>{t('saved')}</span>}
            <button
              onClick={() => void saveEdit()}
              disabled={saving}
              style={{
                padding: '4px 14px', borderRadius: 6, border: 'none', cursor: saving ? 'not-allowed' : 'pointer',
                background: '#2d6cdf', color: '#fff', fontSize: 12, flexShrink: 0,
              }}
            >
              {saving ? t('loading') : t('save')}
            </button>
            <button
              onClick={cancelEditing}
              style={{
                padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
                background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12, flexShrink: 0,
              }}
            >
              {t('cancel')}
            </button>
          </>
        ) : (
          <>
            {savedFlash && <span style={{ fontSize: 12, color: '#7bd88f', flexShrink: 0 }}>{t('saved')}</span>}
            {canEdit && (
              <button
                onClick={() => void startEditing()}
                style={{
                  padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
                  background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12, flexShrink: 0,
                }}
              >
                ✎ {t('edit')}
              </button>
            )}
            {canGoBack && (
              <button
                onClick={goBack}
                title={t('back')}
                style={{
                  padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
                  background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12, flexShrink: 0,
                }}
              >
                ← {t('back')}
              </button>
            )}
            <button
              onClick={() => void copyLink()}
              style={{
                padding: '4px 12px', borderRadius: 6, border: '1px solid #555',
                background: 'transparent', color: copied ? '#7bd88f' : '#aaa',
                cursor: 'pointer', fontSize: 12, flexShrink: 0,
              }}
            >
              {copied ? t('linkCopied') : t('copyLink')}
            </button>
          </>
        )}
      </div>

      <div
        style={{ flex: 1, minHeight: 0, overflowY: isHtml || pdfView ? 'hidden' : 'auto', WebkitOverflowScrolling: 'touch', display: 'flex', flexDirection: 'column' }}
        onScroll={(e) => {
          if (isHtml || pdfView || editing || nextOffset === null) return;
          const el = e.currentTarget;
          if (el.scrollTop + el.clientHeight >= el.scrollHeight - 200) {
            void load(nextOffset);
          }
        }}
      >
        {needsLogin ? (
          <div style={{ color: '#e57373', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('docNeedsLogin')}</div>
        ) : editing ? (
          <textarea
            value={editContent}
            onChange={(e) => { setEditContent(e.target.value); setEditDirty(true); }}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === 's') {
                e.preventDefault();
                void saveEdit();
              }
            }}
            spellCheck={false}
            style={{
              flex: 1, minHeight: 0, width: '100%', padding: 16,
              background: '#0d1117', color: '#c9d1d9', border: 'none', outline: 'none',
              resize: 'none', fontSize: 13, lineHeight: 1.6,
              fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace",
            }}
          />
        ) : error ? (
          <div style={{ color: '#e57373', padding: 40, textAlign: 'center', fontSize: 13 }}>{error}</div>
        ) : loading && content === '' ? (
          <div style={{ color: '#666', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('loading')}</div>
        ) : pdfView ? (
          !params.pub && !pdfToken ? (
            <div style={{ color: '#666', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('loading')}</div>
          ) : (
            <iframe
              src={pdfUrl}
              title={filename}
              style={{ flex: 1, width: '100%', height: '100%', minHeight: 0, border: 'none', background: '#fff' }}
            />
          )
        ) : isHtml ? (
          <iframe
            srcDoc={content}
            title={filename}
            sandbox="allow-scripts allow-same-origin"
            style={{ flex: 1, width: '100%', height: '100%', minHeight: 0, border: 'none', background: '#fff' }}
          />
        ) : (
          <div style={{ padding: '16px 16px 32px', flex: 1, minWidth: 0 }}>
            {isMd ? <ResultMarkdown content={content} /> : (
              <pre style={{ margin: 0, fontSize: 13, lineHeight: 1.6, color: '#c9d1d9', whiteSpace: 'pre-wrap', wordBreak: 'break-word', fontFamily: "'SF Mono', 'Fira Code', 'Consolas', monospace" }}>
                {content}
              </pre>
            )}
            {nextOffset !== null && (
              <div style={{ textAlign: 'center', padding: '12px 0' }}>
                <button
                  onClick={() => void load(nextOffset)}
                  disabled={loading}
                  style={{
                    padding: '6px 16px', borderRadius: 6, border: '1px solid #555',
                    background: 'transparent', color: loading ? '#555' : '#aaa',
                    cursor: loading ? 'not-allowed' : 'pointer', fontSize: 12,
                  }}
                >
                  {loading ? t('loading') : t('loadMore')}
                </button>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
