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
  const tokenRef = useRef<string>('');

  const load = useCallback(
    async (offset: number) => {
      if (!params) return;
      if (!params.pub && !tokenRef.current) {
        const accounts = loadAccounts();
        const account =
          accounts.find((a) => a.userId === params.userId) || accounts[0];
        if (!account) return; // the needsLogin gate handles this
        tokenRef.current = account.token;
      }
      setLoading(true);
      setError('');
      const base = params.pub
        ? `/api/public-users/${encodeURIComponent(params.userId)}/documents/content`
        : '/api/workspace/index/content';
      const url = `${base}?path=${encodeURIComponent(params.path)}&offset=${offset}&limit=${DOC_PAGE}`;
      try {
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
    void load(0);
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

  if (!params) return null;

  const filename = params.path.split('/').pop() || params.path;
  const isHtml = /\.(html?|htm)$/i.test(params.path);
  const isMd = /\.(md|markdown)$/i.test(params.path);

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
      </div>

      <div
        style={{ flex: 1, minHeight: 0, overflowY: isHtml ? 'hidden' : 'auto', WebkitOverflowScrolling: 'touch', display: 'flex', flexDirection: 'column' }}
        onScroll={(e) => {
          if (isHtml || nextOffset === null) return;
          const el = e.currentTarget;
          if (el.scrollTop + el.clientHeight >= el.scrollHeight - 200) {
            void load(nextOffset);
          }
        }}
      >
        {needsLogin ? (
          <div style={{ color: '#e57373', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('docNeedsLogin')}</div>
        ) : error ? (
          <div style={{ color: '#e57373', padding: 40, textAlign: 'center', fontSize: 13 }}>{error}</div>
        ) : loading && content === '' ? (
          <div style={{ color: '#666', padding: 40, textAlign: 'center', fontSize: 13 }}>{t('loading')}</div>
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
