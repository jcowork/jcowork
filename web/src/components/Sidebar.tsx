import { useLang, useT } from '../i18n';

interface SidebarProps {
  username: string;
  onLogout: () => void;
  onChat: () => void;
  onSettings: () => void;
  onSchedule: () => void;
  onMemory: () => void;
  onSkills: () => void;
  onDocuments: () => void;
  onConnectors: () => void;
  currentView: string;
  mobileOpen?: boolean;
  onClose?: () => void;
}

export default function Sidebar({ username, onLogout, onChat, onSettings, onSchedule, onMemory, onSkills, onDocuments, onConnectors, currentView, mobileOpen, onClose }: SidebarProps) {
  const t = useT();
  const { lang, setLang } = useLang();

  const NAV_ITEMS = [
    { key: 'chat', label: t('chat') },
    { key: 'documents', label: t('documents') },
    { key: 'schedule', label: t('schedule') },
    { key: 'memory', label: t('memory') },
    { key: 'skills', label: t('skills') },
    { key: 'connectors', label: t('connectors') },
    { key: 'settings', label: t('settings') },
  ];

  const navHandlers: Record<string, () => void> = { chat: onChat, documents: onDocuments, schedule: onSchedule, memory: onMemory, skills: onSkills, connectors: onConnectors, settings: onSettings };

  const handleNav = (key: string) => {
    navHandlers[key]?.();
    onClose?.();
  };

  const toggleLang = () => {
    setLang(lang === 'zh' ? 'en' : 'zh');
  };

  return (
    <>
      {/* Mobile overlay */}
      {mobileOpen && (
        <div
          onClick={onClose}
          style={{
            position: 'fixed',
            inset: 0,
            background: 'rgba(0,0,0,0.5)',
            zIndex: 998,
          }}
        />
      )}
      <div
        className={`sidebar${mobileOpen ? ' sidebar-open' : ''}`}
        style={{
          width: 240,
          background: '#1a1a1a',
          borderRight: '1px solid #333',
          display: 'flex',
          flexDirection: 'column',
          padding: 16,
          flexShrink: 0,
          transition: 'transform 0.25s',
        }}
      >
        <div style={{ fontWeight: 700, fontSize: 18, marginBottom: 24 }}>
          Jcowork
        </div>

        <div style={{ color: '#888', fontSize: 12, marginBottom: 8 }}>{t('signedInAs')}</div>
        <div style={{ marginBottom: 24, fontWeight: 500 }}>{username}</div>

        <div style={{ flex: 1 }}>
          <div style={{ color: '#888', fontSize: 12, marginBottom: 8, textTransform: 'uppercase' }}>
            {t('navigation')}
          </div>
          {NAV_ITEMS.map(item => (
            <button
              key={item.key}
              onClick={() => handleNav(item.key)}
              style={{
                display: 'block',
                width: '100%',
                padding: '8px 12px',
                borderRadius: 6,
                border: 'none',
                background: currentView === item.key ? '#2a2a2a' : 'transparent',
                color: '#eee',
                textAlign: 'left' as const,
                cursor: 'pointer',
                fontSize: 14,
                marginBottom: 4,
              }}
            >
              {item.label}
            </button>
          ))}
        </div>

        {/* Language toggle */}
        <button
          onClick={toggleLang}
          style={{
            padding: '8px 12px',
            borderRadius: 6,
            border: '1px solid #444',
            background: '#222',
            color: '#aaa',
            cursor: 'pointer',
            fontSize: 13,
            marginBottom: 8,
            display: 'flex',
            alignItems: 'center',
            gap: 6,
          }}
        >
          🌐 {lang === 'zh' ? 'English' : '中文'}
        </button>

        <button
          onClick={onLogout}
          style={{
            padding: '8px 12px',
            borderRadius: 6,
            border: '1px solid #555',
            background: 'transparent',
            color: '#eee',
            cursor: 'pointer',
            fontSize: 14,
          }}
        >
          {t('logout')}
        </button>
      </div>
    </>
  );
}
