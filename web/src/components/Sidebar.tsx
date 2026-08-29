import { useState } from 'react';
import { useLang, useT } from '../i18n';
import { type Conversation } from '../chatStore';

interface SidebarProps {
  username: string;
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
}

export default function Sidebar({ username, onLogout, onChat, onSettings, onSchedule, onMemory, onSkills, onDocuments, currentView, conversations, activeConvId, onNewChat, onSelectConversation, onDeleteConversation, mobileOpen, onClose }: SidebarProps) {
  const t = useT();
  const { lang, setLang } = useLang();
  const [historyOpen, setHistoryOpen] = useState(true);
  // window.confirm is unsupported in Tauri's WKWebView, use a custom modal
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  // Show all conversations with messages (except the currently active one)
  const historyConvs = conversations
    .filter((c) => c.messages.length > 0 && c.id !== activeConvId)
    .sort((a, b) => b.lastInputAt - a.lastInputAt);

  const NAV_ITEMS = [
    { key: 'chat', label: t('chat') },
    { key: 'documents', label: t('documents') },
    { key: 'schedule', label: t('schedule') },
    { key: 'memory', label: t('navMemo') },
    { key: 'skills', label: t('navSkillsConnectors') },
    { key: 'settings', label: t('settings') },
  ];

  const navHandlers: Record<string, () => void> = { chat: onChat, documents: onDocuments, schedule: onSchedule, memory: onMemory, skills: onSkills, settings: onSettings };

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
                      historyConvs.map((c) => (
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
                    <div style={{ padding: '4px 12px 0 24px', fontSize: 11, color: '#555' }}>
                      {t('historyHint')}
                    </div>
                  </div>
                )}
              </div>
            );
          })}
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
