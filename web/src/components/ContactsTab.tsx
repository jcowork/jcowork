import { useT } from '../i18n';

interface ContactsTabProps {
  accounts: { userId: string; username: string }[];
  activeUserId: string;
  streamingAccounts?: Set<string>;
  unreadAccounts?: Set<string>;
  onSwitchAccount: (userId: string) => void;
  onAddAccount: () => void;
  onRemoveAccount: (userId: string) => void;
  publicUsers: { userId: string; username: string }[];
  onOpenPublicUser: (u: { userId: string; username: string }) => void;
}

const avatarHue = (name: string): number => {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) % 360;
  return h;
};

/** Mobile "联系人" tab: locally signed-in accounts + public accounts. */
export default function ContactsTab({
  accounts,
  activeUserId,
  streamingAccounts,
  unreadAccounts,
  onSwitchAccount,
  onAddAccount,
  onRemoveAccount,
  publicUsers,
  onOpenPublicUser,
}: ContactsTabProps) {
  const t = useT();

  const visiblePublicUsers = publicUsers.filter((u) => !accounts.some((a) => a.userId === u.userId));

  const rowStyle: React.CSSProperties = {
    display: 'flex', alignItems: 'center', gap: 10,
    padding: '12px 14px', borderBottom: '1px solid #222',
    background: '#161616', cursor: 'pointer', minHeight: 52,
  };

  const sectionTitle = (label: string) => (
    <div style={{ padding: '10px 14px 4px', fontSize: 11, color: '#666', textTransform: 'uppercase', letterSpacing: '0.5px' }}>
      {label}
    </div>
  );

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', background: '#111', minWidth: 0 }}>
      {/* Header */}
      <div style={{
        padding: '10px 14px', borderBottom: '1px solid #222', flexShrink: 0,
        display: 'flex', alignItems: 'center', gap: 8,
      }}>
        <span style={{ fontSize: 17, fontWeight: 700, color: '#eee' }}>👥 {t('tabContacts')}</span>
        <div style={{ flex: 1 }} />
        <button
          onClick={onAddAccount}
          style={{
            padding: '4px 14px', borderRadius: 6, border: '1px solid #555',
            background: 'transparent', color: '#aaa', cursor: 'pointer', fontSize: 12,
          }}
        >
          + {t('addAccount')}
        </button>
      </div>

      <div style={{ flex: 1, overflowY: 'auto', WebkitOverflowScrolling: 'touch' }}>
        {/* Locally signed-in accounts */}
        {sectionTitle(t('myAccounts'))}
        {accounts.map((a) => {
          const isActive = a.userId === activeUserId;
          const isStreaming = streamingAccounts?.has(a.userId) ?? false;
          const hasUnread = unreadAccounts?.has(a.userId) ?? false;
          return (
            <div
              key={a.userId}
              onClick={() => onSwitchAccount(a.userId)}
              style={{ ...rowStyle, background: isActive ? '#1c2a3a' : '#161616' }}
            >
              <span
                style={{
                  width: 32, height: 32, borderRadius: '50%', flexShrink: 0,
                  display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
                  background: `hsl(${avatarHue(a.username)}, 45%, 32%)`,
                  color: '#eee', fontSize: 14, fontWeight: 700,
                }}
              >
                {a.username.charAt(0).toUpperCase()}
              </span>
              <span style={{
                flex: 1, minWidth: 0, fontSize: 15,
                color: isActive ? '#e6edf3' : '#bbb',
                fontWeight: isActive ? 600 : 400,
                overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
              }}>
                {a.username}
              </span>
              {isStreaming && (
                <span style={{
                  width: 9, height: 9, borderRadius: '50%', background: '#4caf50', flexShrink: 0,
                  animation: 'pulse 1.5s infinite',
                }} title="Task running" />
              )}
              {!isStreaming && hasUnread && (
                <span style={{ width: 9, height: 9, borderRadius: '50%', background: '#ff9800', flexShrink: 0 }} title="Unread results" />
              )}
              {isActive && <span style={{ fontSize: 12, color: '#58a6ff', flexShrink: 0 }}>✓</span>}
              {!isActive && (
                <button
                  onClick={(e) => { e.stopPropagation(); onRemoveAccount(a.userId); }}
                  title={t('removeAccount')}
                  style={{ background: 'none', border: 'none', color: '#666', cursor: 'pointer', fontSize: 13, padding: '2px 4px', flexShrink: 0 }}
                >
                  ✕
                </button>
              )}
            </div>
          );
        })}

        {/* Public accounts (read-only) */}
        {visiblePublicUsers.length > 0 && sectionTitle(t('publicUsers'))}
        {visiblePublicUsers.map((u) => (
          <div
            key={u.userId}
            onClick={() => onOpenPublicUser(u)}
            style={rowStyle}
          >
            <span
              style={{
                width: 32, height: 32, borderRadius: '50%', flexShrink: 0,
                display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
                background: `hsl(${avatarHue(u.username)}, 45%, 26%)`,
                color: '#eee', fontSize: 14, fontWeight: 700,
              }}
            >
              {u.username.charAt(0).toUpperCase()}
            </span>
            <span style={{
              flex: 1, minWidth: 0, fontSize: 15, color: '#bbb',
              overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
            }}>
              {u.username}
            </span>
            <span style={{
              fontSize: 10, color: '#58a6ff', background: '#1f6feb22',
              border: '1px solid #1f6feb55', borderRadius: 8, padding: '1px 8px', flexShrink: 0,
            }}>
              {t('publicBadge')}
            </span>
          </div>
        ))}

        {accounts.length === 0 && visiblePublicUsers.length === 0 && (
          <div style={{ padding: '60px 24px', textAlign: 'center', color: '#666', fontSize: 13 }}>
            {t('searchContacts')}
          </div>
        )}
      </div>
    </div>
  );
}
