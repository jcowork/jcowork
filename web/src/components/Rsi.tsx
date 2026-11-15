import { useCallback, useEffect, useRef, useState } from 'react';
import { useT } from '../i18n';

interface RsiStatusDto {
  enabled: boolean;
  role: 'admin' | 'user';
  running: boolean;
  last_report_at: string | null;
  schedule_hour: number;
  /** Admin only: the checkout a run reads as `source/` (null when missing). */
  source_root: string | null;
  /** Admin only: the stored override; null means auto-detection. */
  source_configured: string | null;
}

interface RsiReportDto {
  id: string;
  role: string;
  period_start: string;
  period_end: string;
  summary: string;
  status: string;
  created_at: string;
}

interface RsiReportDetailDto extends RsiReportDto {
  output: string;
}

interface RsiProps {
  token: string;
  /** The account's flag, shared with the nav switch (single source of truth). */
  enabled: boolean;
  /** Mirror the flag into the nav switch when this page changes it. */
  onEnabledChange?: (enabled: boolean) => void;
}

/** Format an RFC 3339 / SQLite UTC timestamp for display. */
function formatTs(raw: string | null): string {
  if (!raw) return '';
  const iso = raw.includes('T') ? raw : `${raw.replace(' ', 'T')}Z`;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return raw;
  return date.toLocaleString();
}

/**
 * RSI (recursive self-improvement) page.
 *
 * The nav switch and this page share the same per-account flag. The server
 * decides the scope from the account flag: the admin reviews deployment-wide
 * usage and proposes project-code changes; everyone else improves their own
 * workspace workflows and periodic plans.
 */
export default function Rsi({ token, enabled, onEnabledChange }: RsiProps) {
  const t = useT();
  // Kept in a ref so the load callback does not change identity every render.
  const onEnabledChangeRef = useRef(onEnabledChange);
  useEffect(() => { onEnabledChangeRef.current = onEnabledChange; }, [onEnabledChange]);
  const [status, setStatus] = useState<RsiStatusDto | null>(null);
  const [reports, setReports] = useState<RsiReportDto[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadFailed, setLoadFailed] = useState(false);
  const [saving, setSaving] = useState(false);
  const [actionError, setActionError] = useState('');
  const [notice, setNotice] = useState('');
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [details, setDetails] = useState<Record<string, RsiReportDetailDto>>({});
  // Bumped after each poll so the poll effect re-arms while a run is live.
  const [pollTick, setPollTick] = useState(0);
  // Inline source-directory editor (admin only).
  const [sourceEditing, setSourceEditing] = useState(false);
  const [sourceInput, setSourceInput] = useState('');
  const [sourceSaving, setSourceSaving] = useState(false);
  const [sourceError, setSourceError] = useState('');

  const load = useCallback(async () => {
    const headers = { 'Authorization': `Bearer ${token}` };
    try {
      const [statusRes, reportsRes] = await Promise.all([
        fetch('/api/rsi', { headers }),
        fetch('/api/rsi/reports?limit=20', { headers }),
      ]);
      if (!statusRes.ok || !reportsRes.ok) throw new Error('request failed');
      const data: RsiStatusDto = await statusRes.json();
      setStatus(data);
      onEnabledChangeRef.current?.(data.enabled);
      setReports(await reportsRes.json());
      setLoadFailed(false);
    } catch (err) {
      console.error('Failed to load RSI status:', err);
      setLoadFailed(true);
    } finally {
      setLoading(false);
    }
  }, [token]);

  useEffect(() => {
    // Kick the first fetch off the effect path so its state updates land in a
    // microtask, the same way the polling refresh below does.
    void Promise.resolve().then(load);
  }, [load]);

  // While a run is in flight, poll so the report appears on its own.
  useEffect(() => {
    if (!status?.running) return;
    const timer = window.setTimeout(async () => {
      await load();
      setPollTick((n) => n + 1);
    }, 6000);
    return () => window.clearTimeout(timer);
  }, [status?.running, pollTick, load]);

  const toggle = async () => {
    if (!status || saving) return;
    const next = !enabled;
    setSaving(true);
    setActionError('');
    setNotice('');
    try {
      const res = await fetch('/api/rsi', {
        method: 'PUT',
        headers: { 'Authorization': `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ enabled: next }),
      });
      const data = await res.json().catch(() => null);
      if (res.ok && data) {
        setStatus(data);
        onEnabledChangeRef.current?.(!!data.enabled);
      } else {
        setActionError(t('rsiSaveFailed'));
      }
    } catch (err) {
      console.error('Failed to toggle RSI:', err);
      setActionError(t('rsiSaveFailed'));
    } finally {
      setSaving(false);
    }
  };

  const runNow = async () => {
    if (!status || status.running) return;
    setActionError('');
    setNotice('');
    try {
      const res = await fetch('/api/rsi/run', {
        method: 'POST',
        headers: { 'Authorization': `Bearer ${token}` },
      });
      const data = await res.json().catch(() => null);
      if (res.status === 409) {
        setActionError(t('rsiRunConflict'));
      } else if (res.ok) {
        setNotice(t('rsiRunStarted'));
        setStatus((prev) => (prev ? { ...prev, running: true } : prev));
      } else {
        setActionError(data?.error || t('rsiLoadFailed'));
      }
    } catch (err) {
      console.error('Failed to start RSI run:', err);
      setActionError(t('rsiLoadFailed'));
    }
  };

  const openSourceEditor = () => {
    setSourceInput(status?.source_configured ?? status?.source_root ?? '');
    setSourceError('');
    setSourceEditing(true);
  };

  const cancelSourceEditor = () => {
    setSourceEditing(false);
    setSourceError('');
  };

  // An empty path clears the override and restores auto-detection.
  const saveSource = async (path: string) => {
    if (sourceSaving) return;
    setSourceSaving(true);
    setSourceError('');
    try {
      const res = await fetch('/api/rsi/source', {
        method: 'PUT',
        headers: { 'Authorization': `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ path: path.trim() }),
      });
      const data = await res.json().catch(() => null);
      if (res.ok && data) {
        setStatus((prev) => prev ? {
          ...prev,
          source_root: data.source_root ?? null,
          source_configured: data.source_configured ?? null,
        } : prev);
        setSourceEditing(false);
      } else {
        setSourceError(data?.error || t('rsiSourceInvalid'));
      }
    } catch (err) {
      console.error('Failed to save RSI source directory:', err);
      setSourceError(t('networkError'));
    } finally {
      setSourceSaving(false);
    }
  };

  const toggleReport = async (id: string) => {
    if (expandedId === id) {
      setExpandedId(null);
      return;
    }
    setExpandedId(id);
    if (details[id]) return;
    try {
      const res = await fetch(`/api/rsi/reports/${id}`, {
        headers: { 'Authorization': `Bearer ${token}` },
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data: RsiReportDetailDto = await res.json();
      setDetails((prev) => ({ ...prev, [id]: data }));
    } catch (err) {
      console.error('Failed to load RSI report:', err);
    }
  };

  const isAdminView = status?.role === 'admin';
  const running = status?.running ?? false;

  const cardStyle = {
    background: '#1f1f1f',
    border: '1px solid #333',
    borderRadius: 10,
    padding: 16,
  } as const;

  const smallButtonStyle = {
    padding: '4px 10px',
    borderRadius: 6,
    border: '1px solid #3d7eff',
    background: 'transparent',
    color: '#8ab4f8',
    cursor: 'pointer',
    fontSize: 12,
  } as const;

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0, overflow: 'auto', padding: 20, gap: 16 }}>
      {/* Header + toggle */}
      <div style={cardStyle}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
          <div style={{ flex: 1, minWidth: 200 }}>
            <div style={{ fontSize: 18, fontWeight: 700, color: '#eee' }}>🧬 {t('rsi')}</div>
            <div style={{ fontSize: 12, color: '#8ab4f8', marginTop: 4 }}>
              {isAdminView ? t('rsiAdminScope') : t('rsiUserScope')}
            </div>
          </div>
          <button
            role="switch"
            aria-checked={enabled}
            aria-label={t('rsiToggleHint')}
            title={t('rsiToggleHint')}
            onClick={toggle}
            disabled={saving || !status}
            style={{
              width: 46,
              height: 26,
              borderRadius: 13,
              border: 'none',
              padding: 3,
              background: enabled ? '#2ea043' : '#444',
              cursor: saving || !status ? 'default' : 'pointer',
              display: 'flex',
              alignItems: 'center',
              justifyContent: enabled ? 'flex-end' : 'flex-start',
              transition: 'background 0.15s',
              flexShrink: 0,
            }}
          >
            <span style={{ width: 20, height: 20, borderRadius: '50%', background: '#fff', display: 'block' }} />
          </button>
          <span style={{ fontSize: 13, color: enabled ? '#3fb950' : '#888', minWidth: 52 }}>
            {enabled ? t('rsiOn') : t('rsiOff')}
          </span>
        </div>

        <p style={{ fontSize: 13, color: '#aaa', lineHeight: 1.7, margin: '12px 0 0' }}>
          {isAdminView ? t('rsiIntroAdmin') : t('rsiIntroUser')}
        </p>

        {isAdminView && (
          <div style={{ marginTop: 10, fontSize: 12, lineHeight: 1.7 }}>
            {!sourceEditing ? (
              <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
                <span style={{ color: status?.source_root ? '#7ee787' : '#e57373', wordBreak: 'break-all' }}>
                  {status?.source_root
                    ? `${t('rsiSourceRoot')}: ${status.source_root}`
                    : t('rsiSourceRootMissing')}
                </span>
                <button onClick={openSourceEditor} style={smallButtonStyle}>
                  {t('rsiSourceConfigure')}
                </button>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                <input
                  value={sourceInput}
                  onChange={(e) => { setSourceInput(e.target.value); setSourceError(''); }}
                  onKeyDown={(e) => { if (e.key === 'Enter') void saveSource(sourceInput); }}
                  placeholder={t('rsiSourcePathPlaceholder')}
                  spellCheck={false}
                  autoComplete="off"
                  style={{
                    width: '100%',
                    maxWidth: 560,
                    padding: '8px 10px',
                    borderRadius: 6,
                    border: '1px solid #444',
                    background: '#141414',
                    color: '#eee',
                    fontSize: 12,
                    boxSizing: 'border-box',
                  }}
                />
                <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
                  <button
                    onClick={() => void saveSource(sourceInput)}
                    disabled={sourceSaving}
                    style={{
                      ...smallButtonStyle,
                      border: '1px solid #3d7eff',
                      background: sourceSaving ? '#26344f' : '#1f6feb',
                      color: '#fff',
                      cursor: sourceSaving ? 'default' : 'pointer',
                    }}
                  >
                    {sourceSaving ? t('saving') : t('save')}
                  </button>
                  <button onClick={() => void saveSource('')} disabled={sourceSaving} style={smallButtonStyle}>
                    {t('rsiSourceAutoDetect')}
                  </button>
                  <button
                    onClick={cancelSourceEditor}
                    style={{ ...smallButtonStyle, border: '1px solid #444', color: '#888' }}
                  >
                    ✕
                  </button>
                  {sourceError && <span style={{ color: '#e57373' }}>{sourceError}</span>}
                </div>
                <span style={{ color: '#777', fontSize: 11 }}>{t('rsiSourceHint')}</span>
              </div>
            )}
          </div>
        )}

        <div style={{ display: 'flex', alignItems: 'center', gap: 12, marginTop: 14, flexWrap: 'wrap' }}>
          <button
            onClick={runNow}
            disabled={running}
            style={{
              padding: '8px 16px',
              borderRadius: 6,
              border: '1px solid #3d7eff',
              background: running ? '#26344f' : '#1f6feb',
              color: running ? '#9db7e8' : '#fff',
              cursor: running ? 'default' : 'pointer',
              fontSize: 13,
              fontWeight: 600,
            }}
          >
            {running ? t('rsiRunning') : t('rsiRunNow')}
          </button>
          <span style={{ fontSize: 12, color: '#777' }}>
            {t('rsiScheduleHint').replace('{hour}', String(status?.schedule_hour ?? 3))}
          </span>
          <span style={{ fontSize: 12, color: '#777' }}>
            {t('rsiLastRun')}: {status?.last_report_at ? formatTs(status.last_report_at) : t('rsiNeverRun')}
          </span>
        </div>

        {actionError && <div style={{ marginTop: 10, fontSize: 12, color: '#e57373' }}>{actionError}</div>}
        {notice && <div style={{ marginTop: 10, fontSize: 12, color: '#3fb950' }}>{notice}</div>}
        {loadFailed && <div style={{ marginTop: 10, fontSize: 12, color: '#e57373' }}>{t('rsiLoadFailed')}</div>}
      </div>

      {/* History */}
      <div>
        <div style={{ fontSize: 14, fontWeight: 600, color: '#ddd', margin: '0 2px 10px' }}>{t('rsiReports')}</div>
        {loading ? (
          <div style={{ fontSize: 13, color: '#777', padding: 8 }}>…</div>
        ) : reports.length === 0 ? (
          <div style={{ ...cardStyle, fontSize: 13, color: '#888', lineHeight: 1.7 }}>{t('rsiNoReports')}</div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
            {reports.map((report) => {
              const expanded = expandedId === report.id;
              const detail = details[report.id];
              const failed = report.status !== 'success';
              return (
                <div key={report.id} style={cardStyle}>
                  <div
                    onClick={() => toggleReport(report.id)}
                    style={{ display: 'flex', alignItems: 'flex-start', gap: 10, cursor: 'pointer' }}
                  >
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                        <span style={{ fontSize: 13, color: '#eee', fontWeight: 600 }}>
                          {formatTs(report.created_at)}
                        </span>
                        <span
                          style={{
                            fontSize: 10,
                            padding: '1px 8px',
                            borderRadius: 8,
                            fontWeight: 600,
                            background: report.role === 'admin' ? '#8957e522' : '#1f6feb22',
                            color: report.role === 'admin' ? '#b083f0' : '#58a6ff',
                            border: `1px solid ${report.role === 'admin' ? '#8957e555' : '#1f6feb55'}`,
                          }}
                        >
                          {report.role === 'admin' ? 'Admin' : 'User'}
                        </span>
                        {failed && (
                          <span
                            style={{
                              fontSize: 10,
                              padding: '1px 8px',
                              borderRadius: 8,
                              fontWeight: 600,
                              background: '#f8514922',
                              color: '#f85149',
                              border: '1px solid #f8514955',
                            }}
                          >
                            {t('rsiReportError')}
                          </span>
                        )}
                      </div>
                      <div style={{ fontSize: 13, color: '#bbb', marginTop: 6, lineHeight: 1.6 }}>
                        {report.summary}
                      </div>
                      <div style={{ fontSize: 11, color: '#666', marginTop: 6 }}>
                        {formatTs(report.period_start)} → {formatTs(report.period_end)}
                      </div>
                    </div>
                    <span style={{ fontSize: 12, color: '#8ab4f8', flexShrink: 0 }}>
                      {expanded ? t('rsiCollapseReport') : t('rsiExpandReport')}
                    </span>
                  </div>
                  {expanded && (
                    <pre
                      style={{
                        margin: '12px 0 0',
                        padding: 12,
                        background: '#161616',
                        border: '1px solid #2c2c2c',
                        borderRadius: 8,
                        color: '#cfd8dc',
                        fontSize: 12.5,
                        lineHeight: 1.7,
                        whiteSpace: 'pre-wrap',
                        wordBreak: 'break-word',
                        fontFamily: 'inherit',
                        maxHeight: 520,
                        overflow: 'auto',
                      }}
                    >
                      {detail ? detail.output : '…'}
                    </pre>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
