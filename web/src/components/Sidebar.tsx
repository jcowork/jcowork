interface SidebarProps {
  username: string;
  onLogout: () => void;
  onChat: () => void;
  onSettings: () => void;
  onSchedule: () => void;
  onMemory: () => void;
  onSkills: () => void;
  currentView: string;
  mobileOpen?: boolean;
  onClose?: () => void;
}

const NAV_ITEMS = [
  { key: 'chat', label: 'Chat' },
  { key: 'schedule', label: 'Schedule' },
  { key: 'memory', label: 'Memory' },
  { key: 'skills', label: 'Skills' },
  { key: 'settings', label: 'Settings' },
];

export default function Sidebar({ username, onLogout, onChat, onSettings, onSchedule, onMemory, onSkills, currentView, mobileOpen, onClose }: SidebarProps) {
  const navHandlers: Record<string, () => void> = { chat: onChat, schedule: onSchedule, memory: onMemory, skills: onSkills, settings: onSettings };

  const handleNav = (key: string) => {
    navHandlers[key]?.();
    onClose?.();
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

        <div style={{ color: '#888', fontSize: 12, marginBottom: 8 }}>Signed in as</div>
        <div style={{ marginBottom: 24, fontWeight: 500 }}>{username}</div>

        <div style={{ flex: 1 }}>
          <div style={{ color: '#888', fontSize: 12, marginBottom: 8, textTransform: 'uppercase' }}>
            Navigation
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
          Logout
        </button>
      </div>
    </>
  );
}
