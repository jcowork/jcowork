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

interface AuthState {
  token: string;
  userId: string;
  username: string;
}

// Global fetch wrapper:
// 1. Prepend API_BASE for relative URLs (Tauri custom-protocol → http://localhost:3000)
// 2. 401 interceptor — auto-logout on expired token.
// This runs once at module load and affects ALL fetch calls across the app.
const _origFetch = window.fetch;
window.fetch = async function(input: RequestInfo | URL, init?: RequestInit) {
  // Prepend API_BASE for relative URL strings (e.g. "/api/auth/login")
  if (API_BASE && typeof input === 'string' && input.startsWith('/')) {
    input = API_BASE + input;
  }
  const res = await _origFetch.call(window, input, init);
  if (res.status === 401) {
    localStorage.removeItem('jcowork_auth');
    // Only reload if not already on the login screen
    if (document.querySelector('#root')?.childElementCount) {
      window.location.reload();
    }
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
  const [auth, setAuth] = useState<AuthState | null>(() => {
    const saved = localStorage.getItem('jcowork_auth');
    return saved ? JSON.parse(saved) : null;
  });
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
      // Even for short hides, verify auth state hasn't been cleared by the 401 interceptor
      const current = localStorage.getItem('jcowork_auth');
      if (!current && auth) {
        setAuth(null);
      }
    } else if (document.visibilityState === 'hidden') {
      hiddenTimeRef.current = Date.now();
    }
  }, [auth]);

  useEffect(() => {
    document.addEventListener('visibilitychange', handleVisibilityChange);
    return () => document.removeEventListener('visibilitychange', handleVisibilityChange);
  }, [handleVisibilityChange]);

  // Load conversations once authenticated; create one if none exists
  useEffect(() => {
    if (!auth) return;
    const convs = loadConversations(auth.userId);
    const savedId = getActiveConvId(auth.userId);
    if (savedId && convs.some((c) => c.id === savedId)) {
      setConversations(convs);
      setActiveConvIdState(savedId);
    } else {
      const res = createConversation(auth.userId);
      setConversations(res.convs);
      setActiveConvIdState(res.id);
    }
  }, [auth]);

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
    if (!auth) return;
    // Reuse the active conversation if it's still empty
    const active = conversations.find((c) => c.id === activeConvId);
    if (active && active.messages.length === 0) {
      switchToChatView();
      return;
    }
    const res = createConversation(auth.userId);
    setConversations(res.convs);
    setActiveConvIdState(res.id);
    switchToChatView();
  }, [auth, conversations, activeConvId]);

  const handleSelectConversation = useCallback((id: string) => {
    if (!auth) return;
    setActiveConvId(auth.userId, id);
    setActiveConvIdState(id);
    switchToChatView();
  }, [auth]);

  const handleDeleteConversation = useCallback((id: string) => {
    if (!auth) return;
    const convs = deleteConversation(auth.userId, id);
    setConversations(convs);
    if (activeConvId === id) {
      const res = createConversation(auth.userId);
      setConversations(res.convs);
      setActiveConvIdState(res.id);
    }
  }, [auth, activeConvId]);

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
        setAuth(authState);
        localStorage.setItem('jcowork_auth', JSON.stringify(authState));
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
    setAuth(null);
    localStorage.removeItem('jcowork_auth');
  };

  if (!auth) {
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
          {authView === 'login' && (
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

  return (
    <div style={{ display: 'flex', height: '100vh', background: '#111', color: '#eee' }}>
      <Sidebar username={auth.username} onLogout={logout}
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
        {/* Content area with max-width for readability */}
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'auto' }}>
          {showSettings ? (
            <Settings onClose={() => setShowSettings(false)} userId={auth.userId} token={auth.token} />
          ) : showDocuments ? (
            <Documents userId={auth.userId} token={auth.token} />
          ) : showSchedule ? (
            <Schedule userId={auth.userId} token={auth.token} />
          ) : showMemory ? (
            <Memory userId={auth.userId} token={auth.token} />
          ) : showSkills ? (
            <SkillsSquare userId={auth.userId} token={auth.token} />
          ) : activeConvId ? (
            <Chat key={activeConvId} userId={auth.userId} token={auth.token} conversationId={activeConvId} onConversationsSync={setConversations} />
          ) : null}
        </div>
      </div>
    </div>
  );
}
