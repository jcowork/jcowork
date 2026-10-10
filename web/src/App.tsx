import { useState, useEffect, useRef, useCallback } from 'react';
import Chat from './components/Chat';
import Documents from './components/Documents';
import Memory from './components/Memory';
import Schedule from './components/Schedule';
import Sidebar from './components/Sidebar';
import Settings from './components/Settings';
import SkillsSquare from './components/SkillsSquare';
import PublicProfile from './components/PublicProfile';
import AdminUsers from './components/AdminUsers';
import PhoneCodeFields from './components/PhoneCode';
import { I18nProvider, useT } from './i18n';
import { API_BASE } from './config';
import { mapAuthError as translateAuthError, type AuthErrorDetail } from './authErrors';
import {
  type Conversation,
  loadConversations,
  getActiveConvId,
  setActiveConvId,
  createConversation,
  deleteConversation,
} from './chatStore';
import {
  type AuthState,
  loadAccounts,
  saveAccounts,
  getActiveAccount,
  setActiveAccount,
  upsertAccount,
  removeAccount,
} from './accountStore';

// Global fetch wrapper:
// 1. Prepend API_BASE for relative URLs (Tauri custom-protocol → http://localhost:3000)
// 2. 401 interceptor — remove the specific account whose token caused the 401.
// This runs once at module load and affects ALL fetch calls across the app.
const _origFetch = window.fetch;
window.fetch = async function(input: RequestInfo | URL, init?: RequestInit) {
  // Prepend API_BASE for relative URL strings (e.g. "/api/auth/login")
  if (API_BASE && typeof input === 'string' && input.startsWith('/')) {
    input = API_BASE + input;
  }
  // Extract the Authorization header token (if any) before the request
  let reqToken: string | undefined;
  if (init?.headers) {
    const h = init.headers instanceof Headers ? init.headers : new Headers(init.headers as Record<string, string>);
    reqToken = h.get('Authorization')?.replace('Bearer ', '') ?? undefined;
  }
  const res = await _origFetch.call(window, input, init);
  if (res.status === 401 && reqToken) {
    // Decode JWT payload to find the userId for this specific token
    try {
      const payload = JSON.parse(atob(reqToken.split('.')[1]));
      const uid = payload.sub || payload.user_id || payload.userId;
      if (uid) {
        const remaining = removeAccount(uid);
        saveAccounts(remaining);
        // If no accounts left, reload to show login
        if (remaining.length === 0) {
          window.location.reload();
        } else {
          // Switch to the first remaining account
          setActiveAccount(remaining[0].userId);
          window.location.reload();
        }
      }
    } catch {}
  }
  return res;
};

// Auth-screen styling. Module-level because the login card and the forced
// phone-binding card are two separate early returns that must look the same.
const authInputStyle: React.CSSProperties = {
  width: '100%', padding: 10, marginBottom: 12, borderRadius: 8,
  border: '1px solid #555', background: '#1a1a1a', color: '#eee', fontSize: 16,
};

const authBtnStyle: React.CSSProperties = {
  width: '100%', padding: 10, borderRadius: 8, border: 'none',
  background: '#1a73e8', color: '#fff', fontSize: 16, cursor: 'pointer',
};

function AuthBanner({ msg, type }: { msg: string; type: 'error' | 'success' }) {
  if (!msg) return null;
  return (
    <div style={{
      background: type === 'error' ? '#3d1c1c' : '#123322',
      border: `1px solid ${type === 'error' ? '#e74c3c' : '#22aa55'}`,
      borderRadius: 8,
      padding: '8px 12px',
      marginBottom: 12,
      color: type === 'error' ? '#f5a5a5' : '#7bd88f',
      fontSize: 14,
      textAlign: 'center',
      lineHeight: 1.4,
    }}>
      {msg}
    </div>
  );
}

export default function App() {
  return (
    <I18nProvider>
      <AppInner />
    </I18nProvider>
  );
}

function AppInner() {
  const t = useT();
  const [accounts, setAccounts] = useState<AuthState[]>(() => loadAccounts());
  const [activeUserId, setActiveUserId] = useState<string>(() => {
    const saved = getActiveAccount();
    const accts = loadAccounts();
    if (saved && accts.some((a) => a.userId === saved)) return saved;
    return accts[0]?.userId ?? '';
  });
  // When true, show login form to add a new account without logging out current
  const [addingAccount, setAddingAccount] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [showSchedule, setShowSchedule] = useState(false);
  const [showMemory, setShowMemory] = useState(false);
  const [showSkills, setShowSkills] = useState(false);
  const [showDocuments, setShowDocuments] = useState(false);
  // Admin-only user management view (visible only when activeAccount.isAdmin)
  const [showAdminUsers, setShowAdminUsers] = useState(false);
  const [mobileSidebar, setMobileSidebar] = useState(false);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeConvId, setActiveConvIdState] = useState<string>('');
  const [, setTick] = useState(0); // periodic re-render for 1h history threshold
  // SMS code is the only way in; the password form exists solely so a
  // freshly seeded admin can reach the forced phone-binding screen.
  const [authMode, setAuthMode] = useState<'sms' | 'admin'>('sms');
  const [smsForm, setSmsForm] = useState({ phone: '', code: '' });
  const [adminForm, setAdminForm] = useState({ username: '', password: '' });
  const [authBusy, setAuthBusy] = useState(false);
  const [sendingCode, setSendingCode] = useState(false);
  const [codeCountdown, setCodeCountdown] = useState(0);
  const [authError, setAuthError] = useState('');
  // Forced phone binding (admin bootstrap). The two forms are never on screen
  // together, but a cooldown started on one must not carry over to the other.
  const [bindForm, setBindForm] = useState({ phone: '', code: '' });
  const [bindBusy, setBindBusy] = useState(false);
  const [bindSendingCode, setBindSendingCode] = useState(false);
  const [bindCountdown, setBindCountdown] = useState(0);
  const [bindError, setBindError] = useState('');
  const hiddenTimeRef = useRef(0);

  // Per-account conversation storage (background accounts keep their convs here)
  const accountConvsRef = useRef<Record<string, Conversation[]>>({});
  // Per-account active conversation ID
  const accountActiveConvRef = useRef<Record<string, string>>({});
  // Accounts currently streaming a task
  const [streamingAccounts, setStreamingAccounts] = useState<Set<string>>(new Set());
  // Accounts with completed-but-unread tasks (set when background task finishes, cleared on switch)
  const [unreadAccounts, setUnreadAccounts] = useState<Set<string>>(new Set());
  // Public accounts visible to the active account (fetched with its token)
  const [publicUsers, setPublicUsers] = useState<{ userId: string; username: string }[]>([]);
  // Public account currently opened in the read-only profile view (null = normal views)
  const [viewingPublicUser, setViewingPublicUser] = useState<{ userId: string; username: string } | null>(null);

  const activeAccount = accounts.find((a) => a.userId === activeUserId) ?? null;

  // Localized backend error text; see authErrors.ts for the string matching.
  const mapAuthError = (raw: string, extra?: AuthErrorDetail) => translateAuthError(t, raw, extra);

  // Sleep/wake recovery: when the page becomes visible again after being hidden
  // for a while (e.g. laptop lid closed), reload to restore WebView rendering.
  const handleVisibilityChange = useCallback(() => {
    if (document.visibilityState === 'visible') {
      const now = Date.now();
      const elapsed = now - hiddenTimeRef.current;
      // If was hidden for more than 30 seconds, force reload to fix WebKit rendering
      if (hiddenTimeRef.current > 0 && elapsed > 30_000) {
        window.location.reload();
        return;
      }
      // Verify account state hasn't been cleared by the 401 interceptor
      const current = loadAccounts();
      if (current.length === 0 && accounts.length > 0) {
        setAccounts([]);
        setActiveUserId('');
      }
    } else if (document.visibilityState === 'hidden') {
      hiddenTimeRef.current = Date.now();
    }
  }, [accounts]);

  useEffect(() => {
    document.addEventListener('visibilitychange', handleVisibilityChange);
    return () => document.removeEventListener('visibilitychange', handleVisibilityChange);
  }, [handleVisibilityChange]);

  // Load conversations when active account changes; create one if none exists
  useEffect(() => {
    if (!activeAccount) return;
    // Always reload from localStorage (the Chat's persistence useEffect keeps it fresh)
    const convs = loadConversations(activeAccount.userId);
    // Prefer the saved active conv ID from the ref (set during switch), fallback to localStorage
    const refConvId = accountActiveConvRef.current[activeAccount.userId];
    const savedId = refConvId || getActiveConvId(activeAccount.userId);
    if (convs.length > 0 && savedId && convs.some((c) => c.id === savedId)) {
      setConversations(convs);
      setActiveConvIdState(savedId);
    } else if (convs.length > 0) {
      setConversations(convs);
      setActiveConvIdState(convs[0].id);
    } else {
      const res = createConversation(activeAccount.userId);
      setConversations(res.convs);
      setActiveConvIdState(res.id);
    }
  }, [activeAccount?.userId]);

  // Fetch public accounts for the active account's token. They show up in
  // the contacts list and can be @mentioned in chat.
  useEffect(() => {
    if (!activeAccount) {
      setPublicUsers([]);
      return;
    }
    let cancelled = false;
    fetch('/api/public-users', {
      headers: { Authorization: `Bearer ${activeAccount.token}` },
    })
      .then((res) => (res.ok ? res.json() : []))
      .then((data) => {
        if (cancelled) return;
        setPublicUsers(
          (Array.isArray(data) ? data : []).map((u: { user_id: string; username: string }) => ({
            userId: u.user_id,
            username: u.username,
          })),
        );
      })
      .catch(() => { if (!cancelled) setPublicUsers([]); });
    return () => { cancelled = true; };
  }, [activeAccount?.userId, activeAccount?.token]);

  // Re-evaluate the 1h history threshold every minute
  useEffect(() => {
    const iv = window.setInterval(() => setTick((v) => v + 1), 60_000);
    return () => window.clearInterval(iv);
  }, []);

  const switchToChatView = () => {
    setShowSettings(false); setShowSchedule(false); setShowMemory(false);
    setShowSkills(false); setShowDocuments(false);
    setViewingPublicUser(null);
  };

  const handleNewChat = useCallback(() => {
    if (!activeAccount) return;
    // Reuse the active conversation if it's still empty
    const active = conversations.find((c) => c.id === activeConvId);
    if (active && active.messages.length === 0) {
      switchToChatView();
      return;
    }
    const res = createConversation(activeAccount.userId);
    setConversations(res.convs);
    setActiveConvIdState(res.id);
    switchToChatView();
  }, [activeAccount, conversations, activeConvId]);

  const handleSelectConversation = useCallback((id: string) => {
    if (!activeAccount) return;
    setActiveConvId(activeAccount.userId, id);
    setActiveConvIdState(id);
    switchToChatView();
  }, [activeAccount]);

  const handleDeleteConversation = useCallback((id: string) => {
    if (!activeAccount) return;
    const convs = deleteConversation(activeAccount.userId, id);
    setConversations(convs);
    if (activeConvId === id) {
      const res = createConversation(activeAccount.userId);
      setConversations(res.convs);
      setActiveConvIdState(res.id);
    }
  }, [activeAccount, activeConvId]);

  /// POST a JSON body and hand back the parsed response.
  /// A non-JSON body yields `{}` rather than throwing, so callers only ever
  /// have to look at the status code plus whatever fields are present.
  const postJson = async (path: string, body: Record<string, unknown>, token?: string) => {
    const res = await fetch(`${API_BASE}${path}`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      },
      body: JSON.stringify(body),
    });
    const data = await res.json().catch(() => ({}));
    return { ok: res.ok, data };
  };

  /// Ask the server to text a code, then start the countdown it reports.
  /// Returns false after writing the reason into `setError`.
  const requestSmsCode = async (
    phone: string,
    hooks: {
      setError: (msg: string) => void;
      setBusy: (busy: boolean) => void;
      startCountdown: (secs: number) => void;
    },
  ): Promise<boolean> => {
    const { setError, setBusy, startCountdown } = hooks;
    setError('');
    if (!phone.trim()) {
      setError(t('invalidPhone'));
      return false;
    }
    setBusy(true);
    try {
      const { ok, data } = await postJson('/api/auth/sms/send', { phone: phone.trim() });
      if (!ok) {
        setError(mapAuthError(data.error ?? '', data));
        return false;
      }
      // Let the server pick the cooldown so the disabled button stays in step
      // with the throttle that will actually reject the next request.
      startCountdown(Number(data.resend_after) || 60);
      return true;
    } catch {
      setError(t('networkError'));
      return false;
    } finally {
      setBusy(false);
    }
  };

  /// Store a freshly issued session and leave the login screens behind.
  const signIn = (data: {
    token: string;
    user_id: string;
    username: string;
    is_admin?: boolean;
    must_bind_phone?: boolean;
  }) => {
    const authState: AuthState = {
      token: data.token,
      userId: data.user_id,
      username: data.username,
      isAdmin: !!data.is_admin,
      mustBindPhone: !!data.must_bind_phone,
    };
    // Upsert into multi-account store
    const updated = upsertAccount(authState);
    setAccounts(updated);
    setActiveUserId(authState.userId);
    setActiveAccount(authState.userId);
    setAddingAccount(false);
    setSmsForm({ phone: '', code: '' });
    setAdminForm({ username: '', password: '' });
  };

  /// Report a failed auth response, or sign in if it carried a token.
  const finishAuth = (
    ok: boolean,
    data: { token?: string; error?: string } & AuthErrorDetail,
    setError: (msg: string) => void,
  ) => {
    if (ok && data.token) {
      signIn(data as { token: string; user_id: string; username: string });
      return;
    }
    setError(mapAuthError(data.error ?? '', data));
  };

  const handleSendLoginCode = () =>
    requestSmsCode(smsForm.phone, {
      setError: setAuthError,
      setBusy: setSendingCode,
      startCountdown: setCodeCountdown,
    });

  /// An unknown number auto-provisions an account, so this doubles as signup.
  const handleSmsLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setAuthError('');
    setAuthBusy(true);
    try {
      const { ok, data } = await postJson('/api/auth/sms/login', {
        phone: smsForm.phone.trim(),
        code: smsForm.code.trim(),
      });
      finishAuth(ok, data, setAuthError);
    } catch {
      setAuthError(t('networkError'));
    } finally {
      setAuthBusy(false);
    }
  };

  /// Only reaches a session for the seeded admin with no phone bound; the
  /// server refuses everyone else, so a successful call means "go bind".
  const handleAdminLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setAuthError('');
    setAuthBusy(true);
    try {
      const { ok, data } = await postJson('/api/auth/login', {
        username: adminForm.username.trim(),
        password: adminForm.password,
      });
      finishAuth(ok, data, setAuthError);
    } catch {
      setAuthError(t('networkError'));
    } finally {
      setAuthBusy(false);
    }
  };

  const handleSendBindCode = () =>
    requestSmsCode(bindForm.phone, {
      setError: setBindError,
      setBusy: setBindSendingCode,
      startCountdown: setBindCountdown,
    });

  /// Submit the forced binding. The bootstrap token keeps working afterwards —
  /// the server re-reads the account on every request — so there is no need to
  /// sign in again.
  const handleBindPhone = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!activeAccount) return;
    setBindError('');
    setBindBusy(true);
    try {
      const { ok, data } = await postJson(
        '/api/profile/phone',
        { phone: bindForm.phone.trim(), code: bindForm.code.trim() },
        activeAccount.token,
      );
      if (ok && data.phone_bound) {
        const updated = upsertAccount({ ...activeAccount, mustBindPhone: false });
        setAccounts(updated);
        setBindForm({ phone: '', code: '' });
      } else {
        setBindError(mapAuthError(data.error ?? '', data));
      }
    } catch {
      setBindError(t('networkError'));
    } finally {
      setBindBusy(false);
    }
  };

  const switchAuthMode = (mode: 'sms' | 'admin') => {
    setAuthMode(mode);
    setAuthError('');
  };

  const logout = () => {
    if (!activeAccount) return;
    const remaining = removeAccount(activeAccount.userId);
    setAccounts(remaining);
    // Clean up refs for removed account
    delete accountConvsRef.current[activeAccount.userId];
    delete accountActiveConvRef.current[activeAccount.userId];
    if (remaining.length > 0) {
      const nextId = remaining[0].userId;
      setActiveUserId(nextId);
      setActiveAccount(nextId);
    } else {
      setActiveUserId('');
      setActiveAccount('');
    }
  };

  // Switch active account: save current convs to ref, load target's
  const handleSwitchAccount = useCallback((userId: string) => {
    if (userId === activeUserId) return;
    // Save current account's conv state
    if (activeAccount) {
      accountConvsRef.current[activeAccount.userId] = conversations;
      accountActiveConvRef.current[activeAccount.userId] = activeConvId;
    }
    setActiveUserId(userId);
    setActiveAccount(userId);
    // Clear unread marker for the account we just switched to
    setUnreadAccounts((prev) => {
      if (!prev.has(userId)) return prev;
      const next = new Set(prev);
      next.delete(userId);
      return next;
    });
    // Reset tab state on account switch
    setShowSettings(false); setShowSchedule(false); setShowMemory(false);
    setShowSkills(false); setShowDocuments(false);
    setViewingPublicUser(null);
    setShowAdminUsers(false);
  }, [activeUserId, activeAccount, conversations, activeConvId]);

  const handleAddAccount = useCallback(() => {
    setAddingAccount(true);
    setAuthMode('sms');
    setAuthError('');
    setSmsForm({ phone: '', code: '' });
    setAdminForm({ username: '', password: '' });
  }, []);

  // Resend cooldowns. The server enforces them too; ticking here only stops
  // the button from looking broken while it refuses to send.
  useEffect(() => {
    if (codeCountdown <= 0) return;
    const h = window.setTimeout(() => setCodeCountdown((c) => c - 1), 1000);
    return () => window.clearTimeout(h);
  }, [codeCountdown]);

  useEffect(() => {
    if (bindCountdown <= 0) return;
    const h = window.setTimeout(() => setBindCountdown((c) => c - 1), 1000);
    return () => window.clearTimeout(h);
  }, [bindCountdown]);

  // Probe every stored account once on load. `/api/profile` is one of the two
  // endpoints an account with no phone number may still call, so it doubles as
  // the check — and accounts saved before phone binding existed all need it.
  useEffect(() => {
    loadAccounts().forEach((acct) => {
      if (acct.mustBindPhone) return;
      fetch('/api/profile', {
        headers: { Authorization: `Bearer ${acct.token}` },
      })
        .then((res) => (res.ok ? res.json() : null))
        .then((data) => {
          if (!data || data.phone_bound !== false) return;
          setAccounts((prev) => {
            const next = prev.map((a) => (a.userId === acct.userId ? { ...a, mustBindPhone: true } : a));
            saveAccounts(next);
            return next;
          });
        })
        .catch(() => {});
    });
  }, []);

  const handleRemoveAccount = useCallback((userId: string) => {
    const remaining = removeAccount(userId);
    setAccounts(remaining);
    delete accountConvsRef.current[userId];
    delete accountActiveConvRef.current[userId];
    if (userId === activeUserId) {
      // Removed the active account — switch to another or show login
      if (remaining.length > 0) {
        const nextId = remaining[0].userId;
        setActiveUserId(nextId);
        setActiveAccount(nextId);
      } else {
        setActiveUserId('');
        setActiveAccount('');
      }
    }
  }, [activeUserId]);

  // Handle onConversationsSync from any Chat (including background ones).
  // Active account: update state directly. Background accounts: store in ref.
  // Also reload from localStorage on switch-back to capture any
  // persistence-layer updates the Chat wrote while hidden.
  const handleConversationsSync = useCallback((userId: string, convs: Conversation[]) => {
    accountConvsRef.current[userId] = convs;
    if (userId === activeUserId) {
      setConversations(convs);
    }
  }, [activeUserId]);

  // Handle streaming state changes from each Chat component
  const handleStreamingChange = useCallback((userId: string, isStreaming: boolean) => {
    setStreamingAccounts((prev) => {
      const has = prev.has(userId);
      if (isStreaming && !has) {
        const next = new Set(prev);
        next.add(userId);
        return next;
      }
      if (!isStreaming && has) {
        const next = new Set(prev);
        next.delete(userId);
        // Task just finished on a background account → mark unread
        if (userId !== activeUserId) {
          setUnreadAccounts((u) => {
            if (u.has(userId)) return u;
            const nu = new Set(u);
            nu.add(userId);
            return nu;
          });
        }
        return next;
      }
      return prev;
    });
  }, [activeUserId]);

  if (accounts.length === 0 || addingAccount) {
    return (
      <div className="login-container">
        <div className="login-card">
          <img src="/logo.png" className="login-logo" alt="Jcowork" />
          <h1 style={{ fontSize: 28, marginBottom: 24 }}>Jcowork Agent</h1>
          {authMode === 'sms' ? (
            <form onSubmit={handleSmsLogin}>
              <AuthBanner msg={authError} type="error" />
              <PhoneCodeFields
                phone={smsForm.phone}
                code={smsForm.code}
                onPhoneChange={(v) => { setSmsForm({ ...smsForm, phone: v }); setAuthError(''); }}
                onCodeChange={(v) => { setSmsForm({ ...smsForm, code: v }); setAuthError(''); }}
                onSendCode={handleSendLoginCode}
                countdown={codeCountdown}
                sending={sendingCode}
              />
              <p style={{ color: '#888', fontSize: 12, lineHeight: 1.5, margin: '0 0 16px' }}>
                {t('smsLoginHint')}
              </p>
              <button
                type="submit"
                disabled={authBusy}
                style={{ ...authBtnStyle, marginBottom: 0, background: authBusy ? '#333' : '#1a73e8', cursor: authBusy ? 'not-allowed' : 'pointer' }}
              >
                {authBusy ? t('loggingIn') : t('login')}
              </button>
            </form>
          ) : (
            <form onSubmit={handleAdminLogin}>
              <p style={{ color: '#888', marginBottom: 16, fontSize: 13, lineHeight: 1.5 }}>{t('adminLoginHint')}</p>
              <AuthBanner msg={authError} type="error" />
              <input
                type="text"
                placeholder={t('username')}
                value={adminForm.username}
                onChange={(e) => { setAdminForm({ ...adminForm, username: e.target.value }); setAuthError(''); }}
                style={authInputStyle}
                autoFocus
              />
              <input
                type="password"
                placeholder={t('password')}
                value={adminForm.password}
                onChange={(e) => { setAdminForm({ ...adminForm, password: e.target.value }); setAuthError(''); }}
                style={authInputStyle}
              />
              <button
                type="submit"
                disabled={authBusy}
                style={{ ...authBtnStyle, marginBottom: 0, background: authBusy ? '#333' : '#1a73e8', cursor: authBusy ? 'not-allowed' : 'pointer' }}
              >
                {authBusy ? t('loggingIn') : t('login')}
              </button>
            </form>
          )}
          {addingAccount && (
            <p style={{ marginTop: 12, textAlign: 'center' }}>
              <a href="#" onClick={() => { setAddingAccount(false); setAuthError(''); }} style={{ color: '#1a73e8', fontSize: 14 }}>
                {t('cancel')}
              </a>
            </p>
          )}
          <p style={{ marginTop: 16, textAlign: 'center' }}>
            <a href="#" onClick={() => switchAuthMode(authMode === 'sms' ? 'admin' : 'sms')} style={{ color: '#888', fontSize: 13 }}>
              {authMode === 'sms' ? t('adminLoginEntry') : t('smsLoginEntry')}
            </a>
          </p>
        </div>
      </div>
    );
  }

  // Forced binding. This account holds a valid token but no phone number, and
  // the server answers every other endpoint with 403 until one exists — so
  // there is deliberately no way past this screen except binding or signing out.
  if (activeAccount?.mustBindPhone) {
    return (
      <div className="login-container">
        <div className="login-card">
          <img src="/logo.png" className="login-logo" alt="Jcowork" />
          <h1 style={{ fontSize: 24, marginBottom: 8 }}>{t('bindPhoneTitle')}</h1>
          <p style={{ color: '#888', fontSize: 13, lineHeight: 1.6, marginBottom: 20 }}>
            {t('bindPhoneHint')}
          </p>
          <form onSubmit={handleBindPhone}>
            <AuthBanner msg={bindError} type="error" />
            <PhoneCodeFields
              phone={bindForm.phone}
              code={bindForm.code}
              onPhoneChange={(v) => { setBindForm({ ...bindForm, phone: v }); setBindError(''); }}
              onCodeChange={(v) => { setBindForm({ ...bindForm, code: v }); setBindError(''); }}
              onSendCode={handleSendBindCode}
              countdown={bindCountdown}
              sending={bindSendingCode}
            />
            <button
              type="submit"
              disabled={bindBusy}
              style={{ ...authBtnStyle, marginBottom: 0, background: bindBusy ? '#333' : '#1a73e8', cursor: bindBusy ? 'not-allowed' : 'pointer' }}
            >
              {bindBusy ? t('saving') : t('bindPhone')}
            </button>
          </form>
          <p style={{ marginTop: 16, textAlign: 'center' }}>
            <a href="#" onClick={logout} style={{ color: '#888', fontSize: 13 }}>
              {t('logout')}
            </a>
          </p>
        </div>
      </div>
    );
  }

  // --- Authenticated view ---
  const chatVisible = !showSettings && !showDocuments && !showSchedule && !showMemory && !showSkills && !showAdminUsers && !viewingPublicUser;

  return (
    <div style={{ display: 'flex', height: '100vh', background: '#111', color: '#eee' }}>
      <Sidebar
        accounts={accounts.map((a) => ({ userId: a.userId, username: a.username }))}
        activeUserId={activeUserId}
        streamingAccounts={streamingAccounts}
        unreadAccounts={unreadAccounts}
        onSwitchAccount={handleSwitchAccount}
        onAddAccount={handleAddAccount}
        onRemoveAccount={handleRemoveAccount}
        onLogout={logout}
        publicUsers={publicUsers}
        viewingPublicUserId={viewingPublicUser?.userId ?? null}
        onOpenPublicUser={(u) => {
          setViewingPublicUser(u);
          setShowSettings(false); setShowSchedule(false); setShowMemory(false);
          setShowSkills(false); setShowDocuments(false); setShowAdminUsers(false);
        }}
        isAdmin={activeAccount?.isAdmin}
        onChat={() => { setShowSettings(false); setShowSchedule(false); setShowMemory(false); setShowSkills(false); setShowDocuments(false); setShowAdminUsers(false); setViewingPublicUser(null); }}
        onDocuments={() => { setShowDocuments(true); setShowSettings(false); setShowSchedule(false); setShowMemory(false); setShowSkills(false); setShowAdminUsers(false); setViewingPublicUser(null); }}
        onSettings={() => { setShowSettings(true); setShowSchedule(false); setShowMemory(false); setShowSkills(false); setShowDocuments(false); setShowAdminUsers(false); setViewingPublicUser(null); }}
        onSchedule={() => { setShowSchedule(true); setShowSettings(false); setShowMemory(false); setShowSkills(false); setShowDocuments(false); setShowAdminUsers(false); setViewingPublicUser(null); }}
        onMemory={() => { setShowMemory(true); setShowSchedule(false); setShowSettings(false); setShowSkills(false); setShowDocuments(false); setShowAdminUsers(false); setViewingPublicUser(null); }}
        onSkills={() => { setShowSkills(true); setShowMemory(false); setShowSchedule(false); setShowSettings(false); setShowDocuments(false); setShowAdminUsers(false); setViewingPublicUser(null); }}
        onAdminUsers={() => { setShowAdminUsers(true); setShowSettings(false); setShowSchedule(false); setShowMemory(false); setShowSkills(false); setShowDocuments(false); setViewingPublicUser(null); }}
        currentView={showSettings ? 'settings' : showSchedule ? 'schedule' : showMemory ? 'memory' : showSkills ? 'skills' : showDocuments ? 'documents' : showAdminUsers ? 'admin' : 'chat'}
        conversations={conversations}
        activeConvId={activeConvId}
        onNewChat={handleNewChat}
        onSelectConversation={handleSelectConversation}
        onDeleteConversation={handleDeleteConversation}
        mobileOpen={mobileSidebar}
        onClose={() => setMobileSidebar(false)}
      />
      <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0 }}>
        {/* Mobile top bar with hamburger */}
        <div className="mobile-topbar">
          <button
            onClick={() => setMobileSidebar(true)}
            style={{ background: 'none', border: 'none', color: '#eee', fontSize: 22, cursor: 'pointer', padding: '4px 8px' }}
          >
            ☰
          </button>
          <span style={{ display: 'flex', alignItems: 'center', gap: 6, fontWeight: 600, fontSize: 16 }}>
            <img src="/logo.png" alt="Jcowork" style={{ width: 20, height: 20 }} />
            Jcowork
          </span>
          <span style={{ width: 30 }} />
        </div>
        {/* Content area */}
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'auto' }}>
          {/* Render one Chat per account, all mounted. Only active is visible.
              This keeps WS connections alive so background tasks continue streaming. */}
          {accounts.map((acct) => {
            const isActive = acct.userId === activeUserId;
            const acctConvId = isActive ? activeConvId : (accountActiveConvRef.current[acct.userId] ?? '');
            if (!acctConvId) return null;
            return (
              <div key={acct.userId} style={{ display: (isActive && chatVisible) ? 'flex' : 'none', flex: 1, flexDirection: 'column', minHeight: 0 }}>
                <Chat
                  userId={acct.userId}
                  token={acct.token}
                  conversationId={acctConvId}
                  onConversationsSync={handleConversationsSync}
                  onStreamingChange={handleStreamingChange}
                  visible={isActive && chatVisible}
                />
              </div>
            );
          })}
          {viewingPublicUser && activeAccount ? (
            <PublicProfile
              viewerToken={activeAccount.token}
              publicUser={viewingPublicUser}
              onClose={() => setViewingPublicUser(null)}
            />
          ) : showSettings && activeAccount ? (
            <Settings onClose={() => setShowSettings(false)} userId={activeAccount.userId} token={activeAccount.token} />
          ) : showDocuments && activeAccount ? (
            <Documents userId={activeAccount.userId} token={activeAccount.token} />
          ) : showSchedule && activeAccount ? (
            <Schedule userId={activeAccount.userId} token={activeAccount.token} />
          ) : showMemory && activeAccount ? (
            <Memory userId={activeAccount.userId} token={activeAccount.token} />
          ) : showSkills && activeAccount ? (
            <SkillsSquare userId={activeAccount.userId} token={activeAccount.token} />
          ) : showAdminUsers && activeAccount?.isAdmin ? (
            <AdminUsers token={activeAccount.token} />
          ) : null}
        </div>
      </div>
    </div>
  );
}
