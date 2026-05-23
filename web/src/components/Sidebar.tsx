interface SidebarProps {
  username: string;
  onLogout: () => void;
  onChat: () => void;
  onSettings: () => void;
  onSchedule: () => void;
  onMemory: () => void;
  currentView: string;
}

export default function Sidebar({ username, onLogout, onChat, onSettings, onSchedule, onMemory, currentView }: SidebarProps) {
  return (
    <div style={{
      width: 240,
      background: '#1a1a1a',
      borderRight: '1px solid #333',
      display: 'flex',
      flexDirection: 'column',
      padding: 16,
    }}>
      <div style={{ fontWeight: 700, fontSize: 18, marginBottom: 24 }}>
        Jcowork
      </div>

      <div style={{ color: '#888', fontSize: 12, marginBottom: 8 }}>Signed in as</div>
      <div style={{ marginBottom: 24, fontWeight: 500 }}>{username}</div>

      <div style={{ flex: 1 }}>
        <div style={{ color: '#888', fontSize: 12, marginBottom: 8, textTransform: 'uppercase' }}>
          Navigation
        </div>
        <button
          onClick={onChat}
          style={{
            display: 'block',
            width: '100%',
            padding: '8px 12px',
            borderRadius: 6,
            border: 'none',
            background: currentView === 'chat' ? '#2a2a2a' : 'transparent',
            color: '#eee',
            textAlign: 'left' as const,
            cursor: 'pointer',
            fontSize: 14,
            marginBottom: 4,
          }}
        >
          Chat
        </button>
        <button
          onClick={onSchedule}
          style={{
            display: 'block',
            width: '100%',
            padding: '8px 12px',
            borderRadius: 6,
            border: 'none',
            background: currentView === 'schedule' ? '#2a2a2a' : 'transparent',
            color: '#eee',
            textAlign: 'left' as const,
            cursor: 'pointer',
            fontSize: 14,
            marginBottom: 4,
          }}
        >
          Schedule
        </button>
        <button
          onClick={onMemory}
          style={{
            display: 'block',
            width: '100%',
            padding: '8px 12px',
            borderRadius: 6,
            border: 'none',
            background: currentView === 'memory' ? '#2a2a2a' : 'transparent',
            color: '#eee',
            textAlign: 'left' as const,
            cursor: 'pointer',
            fontSize: 14,
            marginBottom: 4,
          }}
        >
          Memory
        </button>
        <button
          onClick={onSettings}
          style={{
            display: 'block',
            width: '100%',
            padding: '8px 12px',
            borderRadius: 6,
            border: 'none',
            background: currentView === 'settings' ? '#2a2a2a' : 'transparent',
            color: '#eee',
            textAlign: 'left' as const,
            cursor: 'pointer',
            fontSize: 14,
          }}
        >
          Settings
        </button>
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
  );
}
