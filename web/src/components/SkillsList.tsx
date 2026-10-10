import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';

export interface SkillEntry {
  id: string;
  name: string;
  description: string;
  content: string;
  source: 'builtin' | 'user';
  origin: 'builtin' | 'agent' | 'thirdparty';
  version: number;
  enabled: boolean;
  /** Workspace-relative path of the on-demand instruction file (third-party). */
  source_path?: string | null;
}

type FilterTab = 'all' | 'builtin' | 'user' | 'thirdparty';

const IMAGE_TO_HTML_ID = 'builtin:image_to_html';

/** Example third-party skill shown in the import dialog. */
const EXAMPLE_SKILL_URL = 'https://github.com/simonlin1212/a-stock-data';

export default function SkillsList({ token, onCountChange }: {
  token: string;
  onCountChange?: (enabled: number, total: number) => void;
}) {
  const t = useT();
  const [skills, setSkills] = useState<SkillEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [toggling, setToggling] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState<FilterTab>('all');
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<Set<string>>(new Set());
  // window.confirm is unsupported in Tauri's WKWebView, so skill deletion
  // goes through a custom confirmation modal.
  const [confirmDeleteSkill, setConfirmDeleteSkill] = useState<SkillEntry | null>(null);

  // Vision models offered to the image-to-HTML skill, and the user's selection
  const [vlModels, setVlModels] = useState<string[]>([]);
  const [vlModel, setVlModel] = useState('');
  const [vlConfigLoaded, setVlConfigLoaded] = useState(false);

  // ── Third-party skill import dialog ──
  const [importOpen, setImportOpen] = useState(false);
  const [importMode, setImportMode] = useState<'url' | 'paste'>('url');
  const [importUrl, setImportUrl] = useState('');
  const [importContent, setImportContent] = useState('');
  const [importName, setImportName] = useState('');
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState('');
  const [importNote, setImportNote] = useState('');

  const fetchSkills = useCallback(async () => {
    try {
      const res = await fetch('/api/skills/all', {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        const data: SkillEntry[] = await res.json();
        setSkills(data);
      }
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }, [token]);

  useEffect(() => {
    fetchSkills();
  }, [fetchSkills]);

  // Load vision-capable models and the saved VL model config for the skill
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [provRes, cfgRes] = await Promise.all([
          fetch('/api/providers', { headers: { Authorization: `Bearer ${token}` } }),
          fetch(`/api/skills/${encodeURIComponent(IMAGE_TO_HTML_ID)}/config`, {
            headers: { Authorization: `Bearer ${token}` },
          }),
        ]);
        if (cancelled) return;
        if (provRes.ok) {
          // GET /api/providers returns { providers: [...], default_model }
          const data: { providers?: { id: string; models?: { id: string; vision?: boolean }[] }[] } = await provRes.json();
          const visionModels: string[] = [];
          for (const p of data.providers ?? []) {
            for (const m of p.models ?? []) {
              if (m.vision) visionModels.push(`${p.id}:${m.id}`);
            }
          }
          setVlModels(visionModels);
        }
        if (cfgRes.ok) {
          const cfg: { vl_model?: string | null } = await cfgRes.json();
          setVlModel(cfg.vl_model ?? '');
        }
      } catch {
        // ignore
      } finally {
        if (!cancelled) setVlConfigLoaded(true);
      }
    })();
    return () => { cancelled = true; };
  }, [token]);

  const saveVlModel = async (value: string) => {
    setVlModel(value);
    try {
      await fetch(`/api/skills/${encodeURIComponent(IMAGE_TO_HTML_ID)}/config`, {
        method: 'PUT',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ vl_model: value || null }),
      });
    } catch {
      // ignore
    }
  };

  const enabledCount = skills.filter(s => s.enabled).length;

  useEffect(() => {
    onCountChange?.(enabledCount, skills.length);
  }, [enabledCount, skills.length, onCountChange]);

  const toggleSkill = async (id: string, currentEnabled: boolean) => {
    setToggling(prev => new Set(prev).add(id));
    try {
      const res = await fetch(`/api/skills/${encodeURIComponent(id)}/toggle`, {
        method: 'PUT',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({ enabled: !currentEnabled }),
      });
      if (res.ok) {
        setSkills(prev =>
          prev.map(s => (s.id === id ? { ...s, enabled: !currentEnabled } : s))
        );
      }
    } catch {
      // ignore
    } finally {
      setToggling(prev => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
  };

  // Delete a third-party skill (🗑 button → confirmation modal).
  const deleteSkill = async (skill: SkillEntry) => {
    setDeleting(prev => new Set(prev).add(skill.id));
    try {
      const res = await fetch(`/api/skills/${encodeURIComponent(skill.id)}`, {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${token}` },
      });
      if (res.ok) {
        setSkills(prev => prev.filter(s => s.id !== skill.id));
      }
    } catch {
      // ignore
    } finally {
      setDeleting(prev => {
        const next = new Set(prev);
        next.delete(skill.id);
        return next;
      });
    }
  };

  const submitImport = async () => {
    if (importing) return;
    const url = importUrl.trim();
    const content = importContent;
    if (importMode === 'url' && !url) return;
    if (importMode === 'paste' && !content.trim()) return;

    setImporting(true);
    setImportError('');
    try {
      const body = importMode === 'url'
        ? { url }
        : { content, name: importName.trim() || undefined };
      const res = await fetch('/api/skills/import', {
        method: 'POST',
        headers: {
          Authorization: `Bearer ${token}`,
          'Content-Type': 'application/json',
        },
        body: JSON.stringify(body),
      });
      const data: { error?: string } = await res.json().catch(() => ({}));
      if (!res.ok) {
        setImportError(data.error || t('importFailed'));
        return;
      }
      await fetchSkills();
      setImportOpen(false);
      setImportUrl('');
      setImportContent('');
      setImportName('');
      setImportNote(t('importSuccess'));
      window.setTimeout(() => setImportNote(''), 4000);
    } catch {
      setImportError(t('networkError'));
    } finally {
      setImporting(false);
    }
  };

  const isThirdParty = (s: SkillEntry) => s.origin === 'thirdparty';

  const filtered = skills.filter(s => {
    if (filter === 'builtin') return s.source === 'builtin';
    if (filter === 'thirdparty') return isThirdParty(s);
    if (filter === 'user') return s.source === 'user' && !isThirdParty(s);
    return true;
  });

  const tabStyle = (active: boolean): React.CSSProperties => ({
    padding: '6px 16px',
    borderRadius: 6,
    border: 'none',
    background: active ? '#1f6feb' : 'transparent',
    color: active ? '#fff' : '#888',
    cursor: 'pointer',
    fontSize: 13,
    fontWeight: active ? 600 : 400,
    transition: 'all 0.15s',
  });

  // Toggle switch component
  const Toggle = ({ id, enabled }: { id: string; enabled: boolean }) => {
    const busy = toggling.has(id);
    return (
      <button
        onClick={e => { e.stopPropagation(); toggleSkill(id, enabled); }}
        disabled={busy}
        title={enabled ? 'Disable skill' : 'Enable skill'}
        style={{
          position: 'relative',
          width: 40,
          height: 22,
          borderRadius: 11,
          border: 'none',
          background: busy ? '#444' : enabled ? '#1f6feb' : '#444',
          cursor: busy ? 'not-allowed' : 'pointer',
          transition: 'background 0.2s',
          flexShrink: 0,
          padding: 0,
        }}
      >
        <span
          style={{
            position: 'absolute',
            top: 3,
            left: enabled ? 21 : 3,
            width: 16,
            height: 16,
            borderRadius: '50%',
            background: '#fff',
            transition: 'left 0.2s',
          }}
        />
      </button>
    );
  };

  const importInputStyle: React.CSSProperties = {
    width: '100%',
    boxSizing: 'border-box',
    background: '#0d1117',
    color: '#c9d1d9',
    border: '1px solid #30363d',
    borderRadius: 6,
    padding: '8px 10px',
    fontSize: 13,
    outline: 'none',
  };

  const importSegmentStyle = (active: boolean): React.CSSProperties => ({
    flex: 1,
    padding: '7px 0',
    borderRadius: 6,
    border: 'none',
    background: active ? '#1f6feb' : '#21262d',
    color: active ? '#fff' : '#999',
    cursor: 'pointer',
    fontSize: 13,
    fontWeight: active ? 600 : 400,
  });

  return (
    <div style={{ display: 'flex', flexDirection: 'column' }}>
      {/* Filter tabs + import action */}
      <div style={{
        padding: '12px 24px',
        borderBottom: '1px solid #222',
        display: 'flex',
        gap: 4,
        flexShrink: 0,
        alignItems: 'center',
      }}>
        <button style={tabStyle(filter === 'all')} onClick={() => setFilter('all')}>
          {t('all')} ({skills.length})
        </button>
        <button style={tabStyle(filter === 'builtin')} onClick={() => setFilter('builtin')}>
          {t('builtIn')} ({skills.filter(s => s.source === 'builtin').length})
        </button>
        <button style={tabStyle(filter === 'user')} onClick={() => setFilter('user')}>
          {t('mySkills')} ({skills.filter(s => s.source === 'user' && !isThirdParty(s)).length})
        </button>
        <button style={tabStyle(filter === 'thirdparty')} onClick={() => setFilter('thirdparty')}>
          {t('thirdParty')} ({skills.filter(isThirdParty).length})
        </button>

        <div style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 10 }}>
          {importNote && (
            <span style={{ fontSize: 12, color: '#3fb950' }}>✓ {importNote}</span>
          )}
          <button
            onClick={() => { setImportOpen(true); setImportError(''); }}
            style={{
              padding: '6px 14px',
              borderRadius: 6,
              border: '1px solid #1f6feb55',
              background: '#1f6feb22',
              color: '#58a6ff',
              cursor: 'pointer',
              fontSize: 13,
              fontWeight: 500,
              whiteSpace: 'nowrap',
            }}
          >
            + {t('addThirdPartySkill')}
          </button>
        </div>
      </div>

      {/* Content */}
      <div style={{ padding: 24 }}>
        {loading ? (
          <div style={{ color: '#666', textAlign: 'center', paddingTop: 60 }}>{t('loadingSkills')}</div>
        ) : filtered.length === 0 ? (
          <div style={{ color: '#555', textAlign: 'center', paddingTop: 60 }}>
            {filter === 'user' ? t('noCustomSkills')
              : filter === 'thirdparty' ? t('noThirdPartySkills')
              : t('noSkillsFound')}
          </div>
        ) : (
          <div className="skills-grid" style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fill, minmax(280px, 1fr))',
            gap: 16,
          }}>
            {filtered.map(skill => {
              const isExpanded = expandedId === skill.id;
              const thirdParty = isThirdParty(skill);
              return (
                <div
                  key={skill.id}
                  onClick={() => setExpandedId(isExpanded ? null : skill.id)}
                  style={{
                    background: skill.enabled ? '#111d2e' : '#1a1a1a',
                    border: `1px solid ${skill.enabled ? '#1f4070' : '#2a2a2a'}`,
                    borderRadius: 12,
                    padding: 18,
                    cursor: 'pointer',
                    transition: 'border-color 0.2s, background 0.2s',
                    display: 'flex',
                    flexDirection: 'column',
                    gap: 10,
                  }}
                >
                  {/* Card header */}
                  <div style={{ display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between', gap: 10 }}>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                        <span style={{ fontWeight: 600, fontSize: 15 }}>{skill.name}</span>
                        <span style={{
                          fontSize: 11,
                          padding: '2px 7px',
                          borderRadius: 4,
                          background: skill.source === 'builtin' ? '#1a3a1a' : thirdParty ? '#2d1f4d' : '#1a1a3a',
                          color: skill.source === 'builtin' ? '#3fb950' : thirdParty ? '#bc8cff' : '#58a6ff',
                          fontWeight: 500,
                        }}>
                          {skill.source === 'builtin' ? t('builtIn') : thirdParty ? t('thirdParty') : t('mySkills')}
                        </span>
                        {skill.source === 'user' && (
                          <span style={{ fontSize: 11, color: '#555' }}>v{skill.version}</span>
                        )}
                      </div>
                      <div style={{
                        fontSize: 13,
                        color: '#888',
                        marginTop: 6,
                        lineHeight: 1.5,
                      }}>
                        {skill.description || 'No description.'}
                      </div>
                    </div>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0 }}>
                      {thirdParty && (
                        <button
                          onClick={e => { e.stopPropagation(); setConfirmDeleteSkill(skill); }}
                          disabled={deleting.has(skill.id)}
                          title={t('deleteThirdPartySkill')}
                          style={{
                            border: 'none',
                            background: 'transparent',
                            color: deleting.has(skill.id) ? '#555' : '#f85149',
                            cursor: deleting.has(skill.id) ? 'not-allowed' : 'pointer',
                            fontSize: 14,
                            padding: 2,
                            lineHeight: 1,
                          }}
                        >
                          🗑
                        </button>
                      )}
                      <Toggle id={skill.id} enabled={skill.enabled} />
                    </div>
                  </div>

                  {/* VL model selector + reminder for the image-to-HTML skill */}
                  {skill.id === IMAGE_TO_HTML_ID && vlConfigLoaded && (
                    <div
                      onClick={e => e.stopPropagation()}
                      style={{ display: 'flex', flexDirection: 'column', gap: 6 }}
                    >
                      <label style={{ fontSize: 12, color: '#888', display: 'flex', alignItems: 'center', gap: 8 }}>
                        {t('vlModel')}
                        <select
                          value={vlModel}
                          onChange={e => saveVlModel(e.target.value)}
                          style={{
                            background: '#0d1117',
                            color: '#c9d1d9',
                            border: '1px solid #30363d',
                            borderRadius: 6,
                            padding: '4px 8px',
                            fontSize: 12,
                            maxWidth: 220,
                          }}
                        >
                          <option value="">{t('vlModelAuto')}</option>
                          {vlModel && !vlModels.includes(vlModel) && (
                            <option value={vlModel}>{vlModel}</option>
                          )}
                          {vlModels.map(m => (
                            <option key={m} value={m}>{m}</option>
                          ))}
                        </select>
                      </label>
                      {skill.enabled && !vlModel && (
                        <div style={{ fontSize: 12, color: '#d29922', display: 'flex', alignItems: 'flex-start', gap: 4 }}>
                          <span>⚠️</span>
                          <span>{vlModels.length === 0 ? t('vlModelNone') : t('vlModelRequired')}</span>
                        </div>
                      )}
                    </div>
                  )}

                  {/* Expanded content preview */}
                  {isExpanded && (
                    <div
                      onClick={e => e.stopPropagation()}
                      style={{
                        marginTop: 4,
                        padding: '12px 14px',
                        background: '#0d1117',
                        borderRadius: 8,
                        border: '1px solid #30363d',
                        maxHeight: 260,
                        overflowY: 'auto',
                      }}
                    >
                      <div style={{ fontSize: 11, color: '#666', marginBottom: 8, textTransform: 'uppercase', letterSpacing: '0.5px' }}>
                        {t('skillInstructions')}
                      </div>
                      {thirdParty && skill.source_path && (
                        <div style={{ fontSize: 11, color: '#8b949e', marginBottom: 8 }}>
                          {t('skillSourceFile')}: <code style={{ color: '#bc8cff' }}>{skill.source_path}</code>
                        </div>
                      )}
                      <pre style={{
                        fontSize: 12,
                        color: '#c9d1d9',
                        whiteSpace: 'pre-wrap',
                        wordBreak: 'break-word',
                        margin: 0,
                        fontFamily: 'inherit',
                        lineHeight: 1.6,
                      }}>
                        {skill.content.length > 12000
                          ? `${skill.content.slice(0, 12000)}\n\n... (${skill.content.length} chars total)`
                          : skill.content}
                      </pre>
                    </div>
                  )}

                  {/* Footer hint */}
                  <div style={{ fontSize: 11, color: '#555' }}>
                    {isExpanded ? t('clickToCollapse') : t('clickToPreview')}
                    {thirdParty && !isExpanded && (
                      <span style={{ marginLeft: 6, color: '#6e5a9a' }}>· {t('thirdPartyHint')}</span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>

      {/* ── Import third-party skill dialog ── */}
      {importOpen && (
        <div
          onClick={() => { if (!importing) setImportOpen(false); }}
          style={{
            position: 'fixed',
            inset: 0,
            background: 'rgba(0,0,0,0.6)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            zIndex: 1000,
          }}
        >
          <div
            onClick={e => e.stopPropagation()}
            style={{
              width: 560,
              maxWidth: 'calc(100vw - 48px)',
              maxHeight: 'calc(100vh - 80px)',
              overflowY: 'auto',
              background: '#161b22',
              border: '1px solid #30363d',
              borderRadius: 12,
              padding: 24,
              display: 'flex',
              flexDirection: 'column',
              gap: 14,
            }}
          >
            <div style={{ fontSize: 17, fontWeight: 600 }}>{t('importSkillTitle')}</div>

            {/* Mode switch */}
            <div style={{ display: 'flex', gap: 8 }}>
              <button style={importSegmentStyle(importMode === 'url')} onClick={() => setImportMode('url')}>
                {t('importFromUrl')}
              </button>
              <button style={importSegmentStyle(importMode === 'paste')} onClick={() => setImportMode('paste')}>
                {t('importFromPaste')}
              </button>
            </div>

            {importMode === 'url' ? (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                <input
                  type="text"
                  value={importUrl}
                  placeholder={t('importUrlPh')}
                  onChange={e => setImportUrl(e.target.value)}
                  onKeyDown={e => { if (e.key === 'Enter') submitImport(); }}
                  autoFocus
                  style={importInputStyle}
                />
                <div style={{ fontSize: 12, color: '#8b949e', lineHeight: 1.5 }}>{t('importUrlHint')}</div>
                <div style={{ fontSize: 12, color: '#6e7681' }}>
                  {t('importExample')}:{' '}
                  <code
                    onClick={() => setImportUrl(EXAMPLE_SKILL_URL)}
                    style={{ color: '#58a6ff', cursor: 'pointer', textDecoration: 'underline' }}
                  >
                    {EXAMPLE_SKILL_URL}
                  </code>
                </div>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                <input
                  type="text"
                  value={importName}
                  placeholder={t('importNamePh')}
                  onChange={e => setImportName(e.target.value)}
                  style={importInputStyle}
                />
                <textarea
                  value={importContent}
                  placeholder="---&#10;name: my-skill&#10;description: ...&#10;---&#10;&#10;# Skill instructions..."
                  onChange={e => setImportContent(e.target.value)}
                  rows={10}
                  style={{ ...importInputStyle, resize: 'vertical', fontFamily: 'monospace', lineHeight: 1.5 }}
                />
                <div style={{ fontSize: 12, color: '#8b949e', lineHeight: 1.5 }}>{t('importContentHint')}</div>
              </div>
            )}

            {importError && (
              <div style={{ fontSize: 12, color: '#f85149', lineHeight: 1.5, wordBreak: 'break-word' }}>
                {importError}
              </div>
            )}

            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 10 }}>
              <button
                onClick={() => setImportOpen(false)}
                disabled={importing}
                style={{
                  padding: '7px 18px',
                  borderRadius: 6,
                  border: '1px solid #30363d',
                  background: 'transparent',
                  color: '#c9d1d9',
                  cursor: importing ? 'not-allowed' : 'pointer',
                  fontSize: 13,
                }}
              >
                {t('cancel')}
              </button>
              <button
                onClick={submitImport}
                disabled={importing || (importMode === 'url' ? !importUrl.trim() : !importContent.trim())}
                style={{
                  padding: '7px 18px',
                  borderRadius: 6,
                  border: 'none',
                  background: importing ? '#1f6feb88' : '#1f6feb',
                  color: '#fff',
                  cursor: importing ? 'wait' : 'pointer',
                  fontSize: 13,
                  fontWeight: 500,
                }}
              >
                {importing ? t('importing') : t('importBtn')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Delete-skill confirmation modal (window.confirm is unsupported in Tauri's WKWebView) */}
      {confirmDeleteSkill && (
        <div
          onClick={() => setConfirmDeleteSkill(null)}
          style={{
            position: 'fixed',
            inset: 0,
            background: 'rgba(0,0,0,0.6)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            zIndex: 1000,
          }}
        >
          <div
            onClick={e => e.stopPropagation()}
            style={{
              background: '#161b22',
              border: '1px solid #30363d',
              borderRadius: 12,
              padding: 20,
              width: 360,
              maxWidth: 'calc(100vw - 48px)',
              display: 'flex',
              flexDirection: 'column',
              gap: 16,
            }}
          >
            <div style={{ fontSize: 14, color: '#c9d1d9', lineHeight: 1.6 }}>
              {t('confirmDeleteSkill')}
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 10 }}>
              <button
                onClick={() => setConfirmDeleteSkill(null)}
                style={{
                  padding: '7px 18px',
                  borderRadius: 6,
                  border: '1px solid #30363d',
                  background: 'transparent',
                  color: '#c9d1d9',
                  cursor: 'pointer',
                  fontSize: 13,
                }}
              >
                {t('cancel')}
              </button>
              <button
                onClick={() => {
                  deleteSkill(confirmDeleteSkill);
                  setConfirmDeleteSkill(null);
                }}
                style={{
                  padding: '7px 18px',
                  borderRadius: 6,
                  border: 'none',
                  background: '#f85149',
                  color: '#fff',
                  cursor: 'pointer',
                  fontSize: 13,
                  fontWeight: 500,
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
