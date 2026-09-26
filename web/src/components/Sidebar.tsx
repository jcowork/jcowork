import { useState } from 'react';
import { useT } from '../i18n';
import { API_BASE } from '../config';
import { type Conversation } from '../chatStore';

interface SidebarProps {
  accounts: { userId: string; username: string }[];
  activeUserId: string;
  streamingAccounts?: Set<string>;
  unreadAccounts?: Set<string>;
  onSwitchAccount: (userId: string) => void;
  onAddAccount: () => void;
  onRemoveAccount: (userId: string) => void;
  onLogout: () => void;
  onChat: () => void;
  onSettings: () => void;
  onSchedule: () => void;
  onMemory: () => void;
  onSkills: () => void;
  onDocuments: () => void;
  currentView: string;
  conversations: Conversation[];
  activeConvId: string;
  onNewChat: () => void;
  onSelectConversation: (id: string) => void;
  onDeleteConversation: (id: string) => void;
  mobileOpen?: boolean;
  onClose?: () => void;
  /** Public accounts visible from the active account (read-only). */
  publicUsers?: { userId: string; username: string }[];
  /** userId of the public account currently opened in the read-only profile view. */
  viewingPublicUserId?: string | null;
  onOpenPublicUser?: (user: { userId: string; username: string }) => void;
  /** True when the active account is a super user — shows the admin nav entry. */
  isAdmin?: boolean;
  onAdminUsers?: () => void;
}

export default function Sidebar({ accounts, activeUserId, streamingAccounts, unreadAccounts, onSwitchAccount, onAddAccount, onRemoveAccount, onLogout, onChat, onSettings, onSchedule, onMemory, onSkills, onDocuments, currentView, conversations, activeConvId, onNewChat, onSelectConversation, onDeleteConversation, mobileOpen, onClose, publicUsers, viewingPublicUserId, onOpenPublicUser, isAdmin, onAdminUsers }: SidebarProps) {
  const t = useT();
  const [historyOpen, setHistoryOpen] = useState(true);
  // History list shows only the first few chats by default; the rest sit
  // behind an expand toggle below the visible ones.
  const [showAllHistory, setShowAllHistory] = useState(false);
  // window.confirm is unsupported in Tauri's WKWebView, use a custom modal
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  // Contacts section state
  const [contactsOpen, setContactsOpen] = useState(true);
  const [contactsSearch, setContactsSearch] = useState('');
  // Whether the web access URL was just copied (shows "copied" feedback)
  const [webCopied, setWebCopied] = useState(false);

  // Show all conversations with messages (except the currently active one)
  const historyConvs = conversations
    .filter((c) => c.messages.length > 0 && c.id !== activeConvId)
    .sort((a, b) => b.lastInputAt - a.lastInputAt);

  // By default only the most recent HISTORY_CHAT_VISIBLE history chats
  // render; an expand toggle below them reveals the rest.
  const HISTORY_CHAT_VISIBLE = 3;
  const hiddenHistoryCount = Math.max(0, historyConvs.length - HISTORY_CHAT_VISIBLE);
  const visibleHistoryConvs = showAllHistory ? historyConvs : historyConvs.slice(0, HISTORY_CHAT_VISIBLE);

  const NAV_ITEMS = [
    { key: 'chat', label: t('chat') },
    { key: 'documents', label: t('documents') },
    { key: 'schedule', label: t('schedule') },
    { key: 'memory', label: t('navMemo') },
    { key: 'skills', label: t('navSkillsConnectors') },
    // Super users get an extra entry for user management
    ...(isAdmin ? [{ key: 'admin', label: t('userManagement') }] : []),
    { key: 'settings', label: t('settings') },
  ];

  const navHandlers: Record<string, () => void> = { chat: onChat, documents: onDocuments, schedule: onSchedule, memory: onMemory, skills: onSkills, settings: onSettings, admin: onAdminUsers ?? (() => {}) };

  const handleNav = (key: string) => {
    navHandlers[key]?.();
    onClose?.();
  };

  // Copy the browser access URL to the clipboard so it can be opened locally
  // or shared with other devices on the LAN.
  const handleWebAccess = async () => {
    let url = API_BASE || window.location.origin;
    const tauri = (window as any).__TAURI__;
    if (tauri) {
      try {
        // Backend resolves the LAN IP so the link also works from other computers.
        url = await tauri.core.invoke('get_web_access_url');
      } catch (err) {
        console.error('get_web_access_url failed:', err);
      }
    }
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(url);
      } else {
        // Fallback for environments without the async clipboard API
        const ta = document.createElement('textarea');
        ta.value = url;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
      }
      setWebCopied(true);
      window.setTimeout(() => setWebCopied(false), 2000);
    } catch (err) {
      console.error('copy to clipboard failed:', err);
    }
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

        {/* Contacts section */}
        <div style={{ marginBottom: 16 }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: contactsOpen ? 8 : 0 }}>
            <div
              onClick={() => setContactsOpen((o) => !o)}
              style={{ color: '#888', fontSize: 12, textTransform: 'uppercase', cursor: 'pointer', display: 'flex', alignItems: 'center', gap: 4, flex: 1 }}
            >
              <span style={{ fontSize: 10 }}>{contactsOpen ? '▼' : '▶'}</span>
              {t('contacts')}
              {unreadAccounts && unreadAccounts.size > 0 && (
                <span style={{
                  background: '#e53935', color: '#fff', borderRadius: '50%',
                  width: 16, height: 16, fontSize: 10, display: 'inline-flex',
                  alignItems: 'center', justifyContent: 'center', fontWeight: 700,
                }}>{unreadAccounts.size}</span>
              )}
            </div>
            {contactsOpen && (
              <button
                onClick={onAddAccount}
                title={t('addAccount')}
                style={{
                  background: 'none', border: 'none', color: '#888', cursor: 'pointer',
                  fontSize: 16, lineHeight: 1, padding: '0 2px',
                }}
              >+</button>
            )}
          </div>
          {contactsOpen && (
            <>
              {accounts.length > 3 && (
                <input
                  type="text"
                  placeholder={t('searchContacts')}
                  value={contactsSearch}
                  onChange={(e) => setContactsSearch(e.target.value)}
                  style={{
                    width: '100%', padding: '5px 8px', borderRadius: 5,
                    border: '1px solid #333', background: '#222', color: '#ccc',
                    fontSize: 12, marginBottom: 6, outline: 'none',
                    boxSizing: 'border-box',
                  }}
                />
              )}
              {accounts
                .filter((a) => !contactsSearch || a.username.toLowerCase().includes(contactsSearch.toLowerCase()))
                .map((a) => {
                const isStreaming = streamingAccounts?.has(a.userId) ?? false;
                const hasUnread = unreadAccounts?.has(a.userId) ?? false;
                return (
                  <div
                    key={a.userId}
                    onClick={() => onSwitchAccount(a.userId)}
                    style={{
                      display: 'flex', alignItems: 'center', padding: '6px 8px',
                      borderRadius: 6, cursor: 'pointer', marginBottom: 2,
                      background: a.userId === activeUserId ? '#2a2a2a' : 'transparent',
                    }}
                  >
                    <span style={{
                      flex: 1, fontSize: 13, overflow: 'hidden', textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                      color: a.userId === activeUserId ? '#eee' : '#999',
                      fontWeight: a.userId === activeUserId ? 600 : 400,
                    }}>
                      {a.username}
                    </span>
                    {isStreaming && (
                      <span style={{
                        display: 'inline-block', width: 8, height: 8,
                        borderRadius: '50%', background: '#4caf50',
                        flexShrink: 0, marginLeft: 4,
                        animation: 'pulse 1.5s infinite',
                      }} title="Task running" />
                    )}
                    {!isStreaming && hasUnread && (
                      <span style={{
                        display: 'inline-block', width: 8, height: 8,
                        borderRadius: '50%', background: '#ff9800',
                        flexShrink: 0, marginLeft: 4,
                      }} title="Unread results" />
                    )}
                    {a.userId !== activeUserId && (
                      <button
                        onClick={(e) => { e.stopPropagation(); onRemoveAccount(a.userId); }}
                        title={t('removeAccount')}
                        style={{
                          background: 'none', border: 'none', color: '#666', cursor: 'pointer',
                          fontSize: 11, padding: '0 2px', flexShrink: 0,
                        }}
                      >✕</button>
                    )}
                  </div>
                );
              })}
              {/* Public accounts (read-only) — merged into the contacts list */}
              {(() => {
                const visiblePublicUsers = (publicUsers ?? [])
                  .filter((u) => !accounts.some((a) => a.userId === u.userId))
                  .filter((u) => !contactsSearch || u.username.toLowerCase().includes(contactsSearch.toLowerCase()));
                if (visiblePublicUsers.length === 0) return null;
                return (
                  <>
                    <div style={{ color: '#666', fontSize: 10, textTransform: 'uppercase', letterSpacing: '0.5px', margin: '8px 4px 4px' }}>
                      {t('publicUsers')}
                    </div>
                    {visiblePublicUsers.map((u) => (
                      <div
                        key={u.userId}
                        onClick={() => onOpenPublicUser?.(u)}
                        style={{
                          display: 'flex', alignItems: 'center', padding: '6px 8px',
                          borderRadius: 6, cursor: 'pointer', marginBottom: 2,
                          background: u.userId === viewingPublicUserId ? '#1a3a5a' : 'transparent',
                        }}
                      >
                        <span style={{ fontSize: 12, marginRight: 6, flexShrink: 0 }}>🌐</span>
                        <span style={{
                          flex: 1, fontSize: 13, overflow: 'hidden', textOverflow: 'ellipsis',
                          whiteSpace: 'nowrap',
                          color: u.userId === viewingPublicUserId ? '#eee' : '#999',
                          fontWeight: u.userId === viewingPublicUserId ? 600 : 400,
                        }}>
                          {u.username}
                        </span>
                        <span style={{
                          fontSize: 9, color: '#58a6ff', background: '#1f6feb22',
                          border: '1px solid #1f6feb55', borderRadius: 8, padding: '0 6px',
                          flexShrink: 0,
                        }}>
                          {t('publicBadge')}
                        </span>
                      </div>
                    ))}
                  </>
                );
              })()}
            </>
          )}
        </div>

        <div style={{ flex: 1 }}>
          <div style={{ color: '#888', fontSize: 12, marginBottom: 8, textTransform: 'uppercase' }}>
            {t('navigation')}
          </div>
          {NAV_ITEMS.map(item => {
            const isChat = item.key === 'chat';
            return (
              <div key={item.key}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                  <button
                    onClick={() => handleNav(item.key)}
                    style={{
                      flex: 1,
                      display: 'flex',
                      alignItems: 'center',
                      justifyContent: 'space-between',
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
                    <span>{item.label}</span>
                    {isChat && (
                      <span
                        onClick={(e) => { e.stopPropagation(); setHistoryOpen((o) => !o); }}
                        style={{ color: '#888', fontSize: 10, cursor: 'pointer' }}
                        title={t('historyChat')}
                      >
                        {historyOpen ? '▲' : '▼'}
                      </span>
                    )}
                  </button>
                  {isChat && (
                    <button
                      onClick={(e) => { e.stopPropagation(); onNewChat(); }}
                      title={t('newChat')}
                      style={{
                        width: 30,
                        height: 30,
                        flexShrink: 0,
                        display: 'flex',
                        alignItems: 'center',
                        justifyContent: 'center',
                        borderRadius: 6,
                        border: '1px solid #444',
                        background: 'transparent',
                        color: '#aaa',
                        cursor: 'pointer',
                        fontSize: 16,
                        marginBottom: 4,
                      }}
                    >
                      +
                    </button>
                  )}
                </div>
                {/* History task chats dropdown */}
                {isChat && historyOpen && (
                  <div style={{ marginBottom: 6 }}>
                    {historyConvs.length === 0 ? (
                      <div style={{ padding: '4px 12px 4px 24px', fontSize: 12, color: '#666' }}>
                        {t('noHistoryChat')}
                      </div>
                    ) : (
                      visibleHistoryConvs.map((c) => (
                        <div
                          key={c.id}
                          className={`hist-item${c.id === activeConvId ? ' hist-active' : ''}`}
                          onClick={() => { onSelectConversation(c.id); onClose?.(); }}
                          style={{
                            display: 'flex',
                            alignItems: 'center',
                            padding: '6px 8px 6px 20px',
                            borderRadius: 6,
                            cursor: 'pointer',
                            background: c.id === activeConvId ? '#2a2a2a' : 'transparent',
                            marginBottom: 2,
                          }}
                        >
                          <span
                            style={{
                              flex: 1,
                              fontSize: 13,
                              color: '#ccc',
                              overflow: 'hidden',
                              textOverflow: 'ellipsis',
                              whiteSpace: 'nowrap',
                            }}
                            title={c.title}
                          >
                            {c.title || t('newChat')}
                          </span>
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              setConfirmDeleteId(c.id);
                            }}
                            title={t('delete')}
                            style={{
                              background: 'none',
                              border: 'none',
                              color: '#777',
                              cursor: 'pointer',
                              fontSize: 12,
                              padding: '0 2px',
                              flexShrink: 0,
                            }}
                          >
                            ✕
                          </button>
                        </div>
                      ))
                    )}
                    {hiddenHistoryCount > 0 && (
                      <div style={{ padding: '2px 12px 4px 20px' }}>
                        <button
                          onClick={() => setShowAllHistory(!showAllHistory)}
                          style={{
                            padding: '2px 10px',
                            borderRadius: 10,
                            border: '1px solid #3a3a3a',
                            background: 'transparent',
                            color: '#8ab4f8',
                            cursor: 'pointer',
                            fontSize: 12,
                          }}
                        >
                          {showAllHistory ? `▾ ${t('collapseList')}` : `▸ ${t('expandMore')}`}
                        </button>
                      </div>
                    )}
                    <div style={{ padding: '4px 12px 0 24px', fontSize: 11, color: '#555' }}>
                      {t('historyHint')}
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>

        <div style={{ display: 'flex', gap: 8 }}>
          <button
            onClick={handleWebAccess}
            title={t('webAccessHint')}
            style={{
              flex: 1,
              padding: '8px 12px',
              borderRadius: 6,
              border: '1px solid #555',
              background: 'transparent',
              color: '#eee',
              cursor: 'pointer',
              fontSize: 14,
            }}
          >
            {webCopied ? t('webAccessCopied') : t('webAccess')}
          </button>
          <button
            onClick={onLogout}
            style={{
              flex: 1,
              padding: '8px 12px',
              borderRadius: 6,
              border: '1px solid #555',
              background: 'transparent',
              color: '#eee',
              cursor: 'pointer',
              fontSize: 14,
            }}
          >
            {t('removeAccount')}
          </button>
        </div>
      </div>

      {/* Delete confirmation modal */}
      {confirmDeleteId && (
        <div
          onClick={() => setConfirmDeleteId(null)}
          style={{
            position: 'fixed',
            inset: 0,
            background: 'rgba(0,0,0,0.6)',
            zIndex: 999,
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
          }}
        >
          <div
            onClick={(e) => e.stopPropagation()}
            style={{
              background: '#1f1f1f',
              border: '1px solid #333',
              borderRadius: 10,
              padding: 20,
              width: 320,
              boxShadow: '0 8px 32px rgba(0,0,0,0.6)',
            }}
          >
            <div style={{ color: '#eee', fontSize: 14, lineHeight: 1.6, marginBottom: 16 }}>
              {t('confirmDeleteChat')}
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
              <button
                onClick={() => setConfirmDeleteId(null)}
                style={{
                  padding: '6px 16px',
                  borderRadius: 6,
                  border: '1px solid #555',
                  background: 'transparent',
                  color: '#ccc',
                  cursor: 'pointer',
                  fontSize: 13,
                }}
              >
                {t('cancel')}
              </button>
              <button
                onClick={() => {
                  onDeleteConversation(confirmDeleteId);
                  setConfirmDeleteId(null);
                }}
                style={{
                  padding: '6px 16px',
                  borderRadius: 6,
                  border: 'none',
                  background: '#e53935',
                  color: '#fff',
                  cursor: 'pointer',
                  fontSize: 13,
                }}
              >
                {t('delete')}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
