import { useState, useEffect, useCallback } from 'react';
import { useT } from '../i18n';

interface AdminUsersProps {
  /** Auth token of the signed-in super user. */
  token: string;
}

interface AdminUserRow {
  user_id: string;
  username: string;
  is_public: boolean;
  is_admin: boolean;
  created_at: string;
  deleted_at: string | null;
  days_left: number | null;
}

/** Pending destructive action awaiting confirmation in the custom modal. */
type ConfirmAction =
  | { type: 'trash'; user: AdminUserRow }
  | { type: 'purge'; user: AdminUserRow };

/**
 * Super-user user management view.
 *
 * Two tabs: active users and the recycle bin. Users can be searched by
 * username, moved to the trash, restored, or permanently deleted.
 * Trashed accounts are purged automatically 7 days after deletion.
 *
 * Note: window.confirm is unsupported in Tauri's WKWebView, so destructive
 * actions go through a custom confirmation modal.
 */
export default function AdminUsers({ token }: AdminUsersProps) {
  const t = useT();
  const [tab, setTab] = useState<'active' | 'trash'>('active');
  const [query, setQuery] = useState('');
  const [debouncedQuery, setDebouncedQuery] = useState('');
  const [users, setUsers] = useState<AdminUserRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [confirmAction, setConfirmAction] = useState<ConfirmAction | null>(null);
  const [busyUserId, setBusyUserId] = useState<string | null>(null);

  // Debounce the search box so typing does not hammer the API
  useEffect(() => {
    const handle = window.setTimeout(() => setDebouncedQuery(query), 300);
    return () => window.clearTimeout(handle);
  }, [query]);

  // Auto-dismiss the success banner
  useEffect(() => {
    if (!notice) return;
    const handle = window.setTimeout(() => setNotice(''), 2500);
    return () => window.clearTimeout(handle);
  }, [notice]);

  const loadUsers = useCallback(async () => {
    setLoading(true);
    setError('');
    try {
      const params = new URLSearchParams();
      if (debouncedQuery.trim()) params.set('query', debouncedQuery.trim());
      params.set('status', tab === 'trash' ? 'trash' : 'active');
      const res = await fetch(`/api/admin/users?${params.toString()}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      const data = await res.json().catch(() => null);
      if (res.ok && Array.isArray(data)) {
        setUsers(data);
      } else {
        setUsers([]);
        setError(data?.error || t('operationFailed'));
      }
    } catch {
      setUsers([]);
      setError(t('networkError'));
    }
    setLoading(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [debouncedQuery, tab, token]);

  useEffect(() => {
    loadUsers();
  }, [loadUsers]);

  // Run a destructive admin action against the given URL and refresh on success
  const performAction = async (url: string, method: 'POST' | 'DELETE', successMessage: string, userId: string) => {
    setError('');
    setNotice('');
    setBusyUserId(userId);
    try {
      const res = await fetch(url, {
        method,
        headers: { Authorization: `Bearer ${token}` },
      });
      const data = await res.json().catch(() => null);
      if (res.ok) {
        setNotice(successMessage);
        await loadUsers();
      } else {
        setError(data?.error || t('operationFailed'));
      }
    } catch {
      setError(t('networkError'));
    }
    setBusyUserId(null);
  };

  const handleConfirm = async () => {
    if (!confirmAction) return;
    const { type, user } = confirmAction;
    setConfirmAction(null);
    if (type === 'trash') {
      await performAction(`/api/admin/users/${encodeURIComponent(user.user_id)}/trash`, 'POST', t('userMovedToTrash'), user.user_id);
    } else {
      await performAction(`/api/admin/users/${encodeURIComponent(user.user_id)}`, 'DELETE', t('userPermanentlyDeleted'), user.user_id);
    }
  };

  const confirmMessage = confirmAction?.type === 'trash' ? t('confirmDeleteUser') : t('confirmPermanentDelete');

  const actionButtonStyle: React.CSSProperties = {
    padding: '4px 12px',
    borderRadius: 6,
    border: '1px solid #555',
    background: 'transparent',
    color: '#ccc',
    cursor: 'pointer',
    fontSize: 12,
    flexShrink: 0,
  };

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minWidth: 0 }}>
      {/* Header */}
      <div style={{
        padding: '12px 16px', borderBottom: '1px solid #333', flexShrink: 0,
        display: 'flex', alignItems: 'center', gap: 10,
      }}>
        <span style={{ fontSize: 18 }}>👥</span>
        <span style={{ fontWeight: 600, fontSize: 16, color: '#e6edf3' }}>{t('userManagement')}</span>
        <div style={{ flex: 1 }} />
        <button onClick={() => loadUsers()} disabled={loading} style={actionButtonStyle}>
          {t('refresh')}
        </button>
      </div>

      {/* Tabs */}
      <div style={{ display: 'flex', gap: 4, padding: '8px 16px 0', borderBottom: '1px solid #333', flexShrink: 0 }}>
        {(['active', 'trash'] as const).map((key) => (
          <button
            key={key}
            onClick={() => setTab(key)}
            style={{
              padding: '6px 16px', borderRadius: '4px 4px 0 0', border: 'none',
              borderBottom: tab === key ? '2px solid #1f6feb' : '2px solid transparent',
              background: 'transparent', color: tab === key ? '#fff' : '#888',
              cursor: 'pointer', fontSize: 12, fontWeight: tab === key ? 600 : 400,
            }}
          >
            {key === 'active' ? `👤 ${t('userListTab')}` : `🗑️ ${t('trashTab')}`}
          </button>
        ))}
      </div>

      {/* Search */}
      <div style={{ padding: '10px 16px', flexShrink: 0 }}>
        <input
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t('searchUsers')}
          style={{
            width: '100%', maxWidth: 360, padding: '7px 12px', borderRadius: 6,
            border: '1px solid #444', background: '#1e1e1e', color: '#eee',
            fontSize: 13, outline: 'none', boxSizing: 'border-box',
          }}
        />
      </div>

      {/* Banners */}
      {error && (
        <div style={{
          margin: '0 16px 8px', padding: '8px 12px', borderRadius: 6,
          background: '#3a1f1f', border: '1px solid #e5393555', color: '#e57373', fontSize: 12,
          display: 'flex', alignItems: 'center', gap: 8, flexShrink: 0,
        }}>
          <span style={{ flex: 1 }}>{error}</span>
          <button onClick={() => setError('')} style={{ background: 'none', border: 'none', color: '#e57373', cursor: 'pointer', fontSize: 13 }}>✕</button>
        </div>
      )}
      {notice && (
        <div style={{
          margin: '0 16px 8px', padding: '8px 12px', borderRadius: 6,
          background: '#1e3a24', border: '1px solid #81c78455', color: '#81c784', fontSize: 12, flexShrink: 0,
        }}>
          {notice}
        </div>
      )}

      {/* User list */}
      <div style={{ flex: 1, overflowY: 'auto', padding: '4px 16px 16px' }}>
        {loading ? (
          <div style={{ color: '#666', padding: 24, textAlign: 'center' }}>{t('loading')}</div>
        ) : users.length === 0 ? (
          <div style={{
            color: '#666', padding: 24, textAlign: 'center',
            border: '1px dashed #444', borderRadius: 8,
          }}>
            {tab === 'trash' ? t('noTrashedUsers') : t('noUsersFound')}
          </div>
        ) : (
          <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
            {users.map((u) => (
              <div key={u.user_id} style={{
                borderRadius: 8, background: '#2a2a2a', border: '1px solid #444',
                padding: '10px 14px', display: 'flex', alignItems: 'center', gap: 10,
              }}>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
                    <span style={{ fontWeight: 600, fontSize: 14, color: '#e6edf3' }}>{u.username}</span>
                    {u.is_admin && (
                      <span
                        title={t('cannotDeleteAdminAccount')}
                        style={{
                          fontSize: 10, padding: '1px 8px', borderRadius: 8, fontWeight: 600, flexShrink: 0,
                          background: '#8957e522', color: '#b083f0', border: '1px solid #8957e555',
                        }}
                      >
                        {t('adminBadge')}
                      </span>
                    )}
                    {u.is_public && (
                      <span style={{
                        fontSize: 10, padding: '1px 8px', borderRadius: 8, fontWeight: 600, flexShrink: 0,
                        background: '#1f6feb22', color: '#58a6ff', border: '1px solid #1f6feb55',
                      }}>
                        {t('publicBadge')}
                      </span>
                    )}
                  </div>
                  <div style={{ fontSize: 11, color: '#888', marginTop: 3, display: 'flex', gap: 12, flexWrap: 'wrap' }}>
                    <span>{t('createdAt')}: {u.created_at}</span>
                    {tab === 'trash' && u.deleted_at && (
                      <span>{t('deletedAt')}: {u.deleted_at}</span>
                    )}
                    {tab === 'trash' && u.days_left !== null && (
                      <span style={{ color: u.days_left <= 2 ? '#e57373' : '#d29922' }}>
                        {t('remainingDays')}: {u.days_left}
                      </span>
                    )}
                  </div>
                </div>

                {/* Actions */}
                {!u.is_admin && (
                  <div style={{ display: 'flex', gap: 6, flexShrink: 0 }}>
                    {tab === 'active' ? (
                      <button
                        onClick={() => setConfirmAction({ type: 'trash', user: u })}
                        disabled={busyUserId === u.user_id}
                        style={{
                          ...actionButtonStyle,
                          borderColor: '#e5393588',
                          color: busyUserId === u.user_id ? '#666' : '#e57373',
                        }}
                      >
                        {t('deleteUser')}
                      </button>
                    ) : (
                      <>
                        <button
                          onClick={() => performAction(`/api/admin/users/${encodeURIComponent(u.user_id)}/restore`, 'POST', t('userRestored'), u.user_id)}
                          disabled={busyUserId === u.user_id}
                          style={{ ...actionButtonStyle, color: busyUserId === u.user_id ? '#666' : '#81c784', borderColor: '#81c78488' }}
                        >
                          {t('restoreUser')}
                        </button>
                        <button
                          onClick={() => setConfirmAction({ type: 'purge', user: u })}
                          disabled={busyUserId === u.user_id}
                          style={{
                            ...actionButtonStyle,
                            borderColor: '#e5393588',
                            color: busyUserId === u.user_id ? '#666' : '#e57373',
                          }}
                        >
                          {t('permanentlyDelete')}
                        </button>
                      </>
                    )}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Confirmation modal (window.confirm is unsupported in Tauri WKWebView) */}
      {confirmAction && (
        <div
          onClick={() => setConfirmAction(null)}
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
              width: 340,
              boxShadow: '0 8px 32px rgba(0,0,0,0.6)',
            }}
          >
            <div style={{ color: '#eee', fontSize: 14, lineHeight: 1.6, marginBottom: 8 }}>
              {confirmMessage}
            </div>
            <div style={{ color: '#888', fontSize: 12, marginBottom: 16 }}>
              {confirmAction.user.username}
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
              <button
                onClick={() => setConfirmAction(null)}
                style={{
                  padding: '6px 16px', borderRadius: 6, border: '1px solid #555',
                  background: 'transparent', color: '#ccc', cursor: 'pointer', fontSize: 13,
                }}
              >
                {t('cancel')}
              </button>
              <button
                onClick={handleConfirm}
                style={{
                  padding: '6px 16px', borderRadius: 6, border: 'none',
                  background: '#e53935', color: '#fff', cursor: 'pointer', fontSize: 13,
                }}
              >
                {t('confirm')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
