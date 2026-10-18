import { useT } from '../i18n';

interface MineTabProps {
  username: string;
  onMemory: () => void;
  onSkills: () => void;
  onSettings: () => void;
}

/** Mobile "我的" tab: memory memo, skills & connectors, settings. */
export default function MineTab({ username, onMemory, onSkills, onSettings }: MineTabProps) {
  const t = useT();

  const items = [
    { icon: '🧠', label: t('navMemo'), onClick: onMemory },
    { icon: '🔌', label: t('navSkillsConnectors'), onClick: onSkills },
    { icon: '⚙️', label: t('navSettings'), onClick: onSettings },
  ];

  let hue = 0;
  for (let i = 0; i < username.length; i++) hue = (hue * 31 + username.charCodeAt(i)) % 360;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', background: '#111', minWidth: 0 }}>
      {/* Header */}
      <div style={{
        padding: '10px 14px', borderBottom: '1px solid #222', flexShrink: 0,
        display: 'flex', alignItems: 'center',
      }}>
        <span style={{ fontSize: 17, fontWeight: 700, color: '#eee' }}>👤 {t('tabMine')}</span>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', WebkitOverflowScrolling: 'touch' }}>
        {/* Current account card */}
        <div style={{ display: 'flex', alignItems: 'center', gap: 12, padding: '18px 16px', margin: 16, borderRadius: 10, background: '#1a1a1a', border: '1px solid #2a2a2a' }}>
          <span
            style={{
              width: 46, height: 46, borderRadius: '50%', flexShrink: 0,
              display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
              background: `hsl(${hue}, 45%, 32%)`, color: '#fff', fontSize: 20, fontWeight: 700,
            }}
          >
            {username.charAt(0).toUpperCase()}
          </span>
          <div style={{ minWidth: 0 }}>
            <div style={{ fontSize: 17, fontWeight: 600, color: '#e6edf3', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {username}
            </div>
            <div style={{ fontSize: 12, color: '#777', marginTop: 2 }}>{t('signedInAs')}</div>
          </div>
        </div>

        {/* Menu */}
        <div style={{ borderRadius: 10, overflow: 'hidden', border: '1px solid #2a2a2a', margin: '0 16px' }}>
          {items.map((item, idx) => (
            <button
              key={item.label}
              onClick={item.onClick}
              style={{
                width: '100%', display: 'flex', alignItems: 'center', gap: 12,
                padding: '14px 16px', minHeight: 52, cursor: 'pointer', textAlign: 'left',
                background: '#161616',
                borderTop: idx > 0 ? '1px solid #2a2a2a' : 'none',
                borderLeft: 'none', borderRight: 'none', borderBottom: 'none',
                color: '#ddd', fontSize: 15,
              }}
            >
              <span style={{ fontSize: 18 }}>{item.icon}</span>
              <span style={{ flex: 1 }}>{item.label}</span>
              <span style={{ color: '#555', fontSize: 14 }}>›</span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
