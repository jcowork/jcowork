import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';

interface SkillEntry {
  id: string;
  name: string;
  description: string;
  content: string;
  source: 'builtin' | 'user';
  version: number;
  enabled: boolean;
}

interface SkillsSquareProps {
  userId: string;
  token: string;
}

type FilterTab = 'all' | 'builtin' | 'user';

export default function SkillsSquare({ token }: SkillsSquareProps) {
  const t = useT();
  const [skills, setSkills] = useState<SkillEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [toggling, setToggling] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState<FilterTab>('all');
  const [expandedId, setExpandedId] = useState<string | null>(null);

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

  const filtered = skills.filter(s => {
    if (filter === 'builtin') return s.source === 'builtin';
    if (filter === 'user') return s.source === 'user';
    return true;
  });

  const enabledCount = skills.filter(s => s.enabled).length;

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

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Header */}
      <div style={{
        padding: '16px 24px',
        borderBottom: '1px solid #333',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'space-between',
        flexShrink: 0,
      }}>
        <div>
          <h2 style={{ fontSize: 20, fontWeight: 600 }}>{t('skillsSquare')}</h2>
          <div style={{ fontSize: 12, color: '#666', marginTop: 2 }}>
            {enabledCount} {t('skillsEnabled')}
          </div>
        </div>
      </div>

      {/* Filter tabs */}
      <div style={{
        padding: '12px 24px',
        borderBottom: '1px solid #222',
        display: 'flex',
        gap: 4,
        flexShrink: 0,
      }}>
        <button style={tabStyle(filter === 'all')} onClick={() => setFilter('all')}>
          {t('all')} ({skills.length})
        </button>
        <button style={tabStyle(filter === 'builtin')} onClick={() => setFilter('builtin')}>
          {t('builtIn')} ({skills.filter(s => s.source === 'builtin').length})
        </button>
        <button style={tabStyle(filter === 'user')} onClick={() => setFilter('user')}>
          {t('mySkills')} ({skills.filter(s => s.source === 'user').length})
        </button>
      </div>

      {/* Content */}
      <div style={{ flex: 1, overflowY: 'auto', padding: 24 }}>
        {loading ? (
          <div style={{ color: '#666', textAlign: 'center', paddingTop: 60 }}>{t('loadingSkills')}</div>
        ) : filtered.length === 0 ? (
          <div style={{ color: '#555', textAlign: 'center', paddingTop: 60 }}>
            {filter === 'user' ? t('noCustomSkills') : t('noSkillsFound')}
          </div>
        ) : (
          <div className="skills-grid" style={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fill, minmax(280px, 1fr))',
            gap: 16,
          }}>
            {filtered.map(skill => {
              const isExpanded = expandedId === skill.id;
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
                          background: skill.source === 'builtin' ? '#1a3a1a' : '#1a1a3a',
                          color: skill.source === 'builtin' ? '#3fb950' : '#58a6ff',
                          fontWeight: 500,
                        }}>
                          {skill.source === 'builtin' ? t('builtIn') : t('mySkills')}
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
                    <Toggle id={skill.id} enabled={skill.enabled} />
                  </div>

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
                      <pre style={{
                        fontSize: 12,
                        color: '#c9d1d9',
                        whiteSpace: 'pre-wrap',
                        wordBreak: 'break-word',
                        margin: 0,
                        fontFamily: 'inherit',
                        lineHeight: 1.6,
                      }}>
                        {skill.content}
                      </pre>
                    </div>
                  )}

                  {/* Footer hint */}
                  <div style={{ fontSize: 11, color: '#555' }}>
                    {isExpanded ? t('clickToCollapse') : t('clickToPreview')}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
