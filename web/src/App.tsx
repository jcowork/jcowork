import { useState, useEffect, useRef, useCallback } from 'react';
import Chat from './components/Chat';
import Documents from './components/Documents';
import Memory from './components/Memory';
import Schedule from './components/Schedule';
import Sidebar from './components/Sidebar';
import Settings from './components/Settings';
import SkillsSquare from './components/SkillsSquare';
import { I18nProvider, useT } from './i18n';
import { API_BASE } from './config';
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
  const [mobileSidebar, setMobileSidebar] = useState(false);
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeConvId, setActiveConvIdState] = useState<string>('');
  const [, setTick] = useState(0); // periodic re-render for 1h history threshold
  const [loginForm, setLoginForm] = useState({ username: '', password: '' });
  const [authView, setAuthView] = useState<'login' | 'register' | 'forgot'>('login');
  const [forgotStep, setForgotStep] = useState<'request' | 'reset'>('request');
  const [forgotUsername, setForgotUsername] = useState('');
  const [generatedCode, setGeneratedCode] = useState<string | null>(null);
  const [resetForm, setResetForm] = useState({ code: '', password: '', confirmPassword: '' });
  const [authError, setAuthError] = useState('');
  const [authSuccess, setAuthSuccess] = useState('');
  const [forgotError, setForgotError] = useState('');
  const [resetError, setResetError] = useState('');
  const hiddenTimeRef = useRef(0);

  // Per-account conversation storage (background accounts keep their convs here)
  const accountConvsRef = useRef<Record<string, Conversation[]>>({});
  // Per-account active conversation ID
  const accountActiveConvRef = useRef<Record<string, string>>({});
  // Accounts currently streaming a task
  const [streamingAccounts, setStreamingAccounts] = useState<Set<string>>(new Set());
  // Accounts with completed-but-unread tasks (set when background task finishes, cleared on switch)
  const [unreadAccounts, setUnreadAccounts] = useState<Set<string>>(new Set());

  const activeAccount = accounts.find((a) => a.userId === activeUserId) ?? null;

  // Map backend error strings to localized, user-friendly messages.
  const mapAuthError = (raw: string): string => {
    if (raw.includes('User not found')) return t('userNotFound');
    if (raw.includes('Invalid username or password')) return t('authFailed');
    if (raw.includes('already exists')) return t('usernameExists');
    if (raw.includes('Password must be at least')) return t('passwordTooShort');
    if (raw.includes('Invalid or expired')) return t('invalidOrExpiredCode');
    if (raw.includes('Reset code has expired')) return t('resetCodeExpired');
    if (raw.includes('Invalid reset code')) return t('invalidResetCode');
    return raw;
  };

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

  // Re-evaluate the 1h history threshold every minute
  useEffect(() => {
    const iv = window.setInterval(() => setTick((v) => v + 1), 60_000);
    return () => window.clearInterval(iv);
  }, []);

  const switchToChatView = () => {
    setShowSettings(false); setShowSchedule(false); setShowMemory(false);
    setShowSkills(false); setShowDocuments(false);
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

  const handleAuth = async (e: React.FormEvent) => {
    e.preventDefault();
    setAuthError('');
    setAuthSuccess('');
    const endpoint = authView === 'register' ? '/api/auth/register' : '/api/auth/login';
    try {
      const res = await fetch(`${API_BASE}${endpoint}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(loginForm),
      });
      const data = await res.json();
      if (data.token) {
        const authState: AuthState = {
          token: data.token,
          userId: data.user_id,
          username: data.username,
        };
        // Upsert into multi-account store
        const updated = upsertAccount(authState);
        setAccounts(updated);
        setActiveUserId(authState.userId);
        setActiveAccount(authState.userId);
        setAddingAccount(false);
      } else if (data.error) {
        setAuthError(mapAuthError(data.error));
      } else {
        setAuthError(t('authFailed'));
      }
    } catch {
      console.error('Auth failed');
      setAuthError(t('networkError'));
    }
  };

  const handleForgotPassword = async (e: React.FormEvent) => {
    e.preventDefault();
    setForgotError('');
    try {
      const res = await fetch(`${API_BASE}/api/auth/forgot-password`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: forgotUsername }),
      });
      const data = await res.json();
      if (data.code) {
        setGeneratedCode(data.code);
        setForgotStep('reset');
      } else if (data.error) {
        setForgotError(mapAuthError(data.error));
      } else {
        setForgotError(t('networkError'));
      }
    } catch {
      setForgotError(t('networkError'));
    }
  };

  const handleResetPassword = async (e: React.FormEvent) => {
    e.preventDefault();
    setResetError('');
    if (resetForm.password !== resetForm.confirmPassword) {
      setResetError(t('passwordMismatch'));
      return;
    }
    if (resetForm.password.length < 6) {
      setResetError(t('passwordTooShort'));
      return;
    }
    try {
      const res = await fetch(`${API_BASE}/api/auth/reset-password`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: forgotUsername, code: resetForm.code, password: resetForm.password }),
      });
      const data = await res.json();
      if (data.message) {
        setAuthView('login');
        setForgotStep('request');
        setGeneratedCode(null);
        setResetForm({ code: '', password: '', confirmPassword: '' });
        setForgotUsername('');
        setAuthError('');
        setAuthSuccess(t('resetSuccess'));
      } else if (data.error) {
        setResetError(mapAuthError(data.error));
      } else {
        setResetError(t('networkError'));
      }
    } catch {
      setResetError(t('networkError'));
    }
  };

  const goBackToLogin = () => {
    setAuthView('login');
    setForgotStep('request');
    setGeneratedCode(null);
    setResetForm({ code: '', password: '', confirmPassword: '' });
    setForgotUsername('');
    setForgotError('');
    setResetError('');
    setAuthError('');
    setAuthSuccess('');
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
  }, [activeUserId, activeAccount, conversations, activeConvId]);

  const handleAddAccount = useCallback(() => {
    setAddingAccount(true);
    setAuthView('login');
    setAuthError(''); setAuthSuccess('');
    setLoginForm({ username: '', password: '' });
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
    const inputStyle = { width: '100%', padding: 10, marginBottom: 12, borderRadius: 8, border: '1px solid #555', background: '#1a1a1a', color: '#eee', fontSize: 16 };
    const btnStyle = { width: '100%', padding: 10, borderRadius: 8, border: 'none', background: '#1a73e8', color: '#fff', fontSize: 16, cursor: 'pointer' };
    const banner = (msg: string, type: 'error' | 'success') => msg ? (
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
    ) : null;

    if (authView === 'forgot') {
      return (
        <div className="login-container">
          <div className="login-card">
            <h1 style={{ fontSize: 28, marginBottom: 24 }}>Jcowork Agent</h1>
            {forgotStep === 'request' ? (
              <form onSubmit={handleForgotPassword}>
                <p style={{ color: '#888', marginBottom: 16, fontSize: 14 }}>{t('forgotPassword')}</p>
                {banner(forgotError, 'error')}
                <input
                  type="text"
                  placeholder={t('username')}
                  value={forgotUsername}
                  onChange={(e) => { setForgotUsername(e.target.value); setForgotError(''); }}
                  required
                  autoFocus
                  style={forgotError ? { ...inputStyle, border: '1px solid #e74c3c' } : inputStyle}
                />
                <button type="submit" style={btnStyle}>
                  {t('getResetCode')}
                </button>
              </form>
            ) : (
              <form onSubmit={handleResetPassword}>
                <div style={{ background: '#1a2744', border: '1px solid #1a73e8', borderRadius: 8, padding: 12, marginBottom: 16, textAlign: 'center' }}>
                  <p style={{ color: '#1a73e8', fontSize: 13, marginBottom: 4 }}>{t('resetCodeGenerated')}</p>
                  <p style={{ color: '#888', fontSize: 12, marginBottom: 8 }}>{t('resetCodeExpiresIn')}</p>
                  <p style={{ fontSize: 32, fontFamily: 'monospace', letterSpacing: 6, margin: 0, color: '#fff' }}>
                    {generatedCode}
                  </p>
                </div>
                {banner(resetError, 'error')}
                <input
                  type="text"
                  placeholder={t('resetCode')}
                  value={resetForm.code}
                  onChange={(e) => { setResetForm({ ...resetForm, code: e.target.value }); setResetError(''); }}
                  required
                  maxLength={6}
                  style={{ ...inputStyle, letterSpacing: 4, textAlign: 'center', fontSize: 20 }}
                />
                <input
                  type="password"
                  placeholder={t('newPassword')}
                  value={resetForm.password}
                  onChange={(e) => { setResetForm({ ...resetForm, password: e.target.value }); setResetError(''); }}
                  required
                  style={inputStyle}
                />
                <input
                  type="password"
                  placeholder={t('confirmPassword')}
                  value={resetForm.confirmPassword}
                  onChange={(e) => { setResetForm({ ...resetForm, confirmPassword: e.target.value }); setResetError(''); }}
                  required
                  style={{ ...inputStyle, marginBottom: 16 }}
                />
                <button type="submit" style={btnStyle}>
                  {t('resetPassword')}
                </button>
              </form>
            )}
            <p style={{ marginTop: 16, textAlign: 'center' }}>
              <a href="#" onClick={goBackToLogin} style={{ color: '#1a73e8' }}>
                {t('backToLogin')}
              </a>
            </p>
          </div>
        </div>
      );
    }

    return (
      <div className="login-container">
        <div className="login-card">
          <h1 style={{ fontSize: 28, marginBottom: 24 }}>Jcowork Agent</h1>
          <form onSubmit={handleAuth}>
            {banner(authError, 'error')}
            {banner(authSuccess, 'success')}
            <input
              type="text"
              placeholder={t('username')}
              value={loginForm.username}
              onChange={(e) => { setLoginForm({ ...loginForm, username: e.target.value }); setAuthError(''); setAuthSuccess(''); }}
              style={inputStyle}
              autoFocus
            />
            <input
              type="password"
              placeholder={t('password')}
              value={loginForm.password}
              onChange={(e) => { setLoginForm({ ...loginForm, password: e.target.value }); setAuthError(''); }}
              style={inputStyle}
            />
            <button type="submit" style={{ ...btnStyle, marginBottom: 0 }}>
              {authView === 'register' ? t('register') : t('login')}
            </button>
          </form>
          {addingAccount && (
            <p style={{ marginTop: 12, textAlign: 'center' }}>
              <a href="#" onClick={() => { setAddingAccount(false); setAuthError(''); setAuthSuccess(''); }} style={{ color: '#1a73e8', fontSize: 14 }}>
                {t('cancel')}
              </a>
            </p>
          )}
          {!addingAccount && authView === 'login' && (
            <p style={{ marginTop: 12, textAlign: 'center' }}>
              <a href="#" onClick={() => { setAuthView('forgot'); setAuthError(''); setAuthSuccess(''); }} style={{ color: '#1a73e8', fontSize: 14 }}>
                {t('forgotPassword')}
              </a>
            </p>
          )}
          <p style={{ marginTop: 16, textAlign: 'center' }}>
            <span style={{ color: '#888' }}>{authView === 'register' ? t('alreadyHaveAccount') : t('dontHaveAccount')}</span>{' '}
            <a href="#" onClick={() => { setAuthView(authView === 'register' ? 'login' : 'register'); setAuthError(''); setAuthSuccess(''); }} style={{ color: '#1a73e8' }}>
              {authView === 'register' ? t('login') : t('register')}
            </a>
          </p>
        </div>
      </div>
    );
  }

  // --- Authenticated view ---
  const chatVisible = !showSettings && !showDocuments && !showSchedule && !showMemory && !showSkills;

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
        onChat={() => { setShowSettings(false); setShowSchedule(false); setShowMemory(false); setShowSkills(false); setShowDocuments(false); }}
        onDocuments={() => { setShowDocuments(true); setShowSettings(false); setShowSchedule(false); setShowMemory(false); setShowSkills(false); }}
        onSettings={() => { setShowSettings(true); setShowSchedule(false); setShowMemory(false); setShowSkills(false); setShowDocuments(false); }}
        onSchedule={() => { setShowSchedule(true); setShowSettings(false); setShowMemory(false); setShowSkills(false); setShowDocuments(false); }}
        onMemory={() => { setShowMemory(true); setShowSchedule(false); setShowSettings(false); setShowSkills(false); setShowDocuments(false); }}
        onSkills={() => { setShowSkills(true); setShowMemory(false); setShowSchedule(false); setShowSettings(false); setShowDocuments(false); }}
        currentView={showSettings ? 'settings' : showSchedule ? 'schedule' : showMemory ? 'memory' : showSkills ? 'skills' : showDocuments ? 'documents' : 'chat'}
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
          <span style={{ fontWeight: 600, fontSize: 16 }}>Jcowork</span>
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
          {showSettings && activeAccount ? (
            <Settings onClose={() => setShowSettings(false)} userId={activeAccount.userId} token={activeAccount.token} />
          ) : showDocuments && activeAccount ? (
            <Documents userId={activeAccount.userId} token={activeAccount.token} />
          ) : showSchedule && activeAccount ? (
            <Schedule userId={activeAccount.userId} token={activeAccount.token} />
          ) : showMemory && activeAccount ? (
            <Memory userId={activeAccount.userId} token={activeAccount.token} />
          ) : showSkills && activeAccount ? (
            <SkillsSquare userId={activeAccount.userId} token={activeAccount.token} />
          ) : null}
        </div>
      </div>
    </div>
  );
}
