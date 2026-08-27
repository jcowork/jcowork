import { useState, useEffect, useRef, useCallback } from 'react';
import Chat from './components/Chat';
import Documents from './components/Documents';
import Memory from './components/Memory';
import Schedule from './components/Schedule';
import Sidebar from './components/Sidebar';
import Settings from './components/Settings';
import SkillsSquare from './components/SkillsSquare';
import { I18nProvider, useT } from './i18n';

const API_BASE = '';

interface AuthState {
  token: string;
  userId: string;
  username: string;
}

// Global 401 interceptor — wraps window.fetch to auto-logout on expired token.
// This runs once at module load and affects ALL fetch calls across the app.
const _origFetch = window.fetch;
window.fetch = async function(input: RequestInfo | URL, init?: RequestInit) {
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
  const [loginForm, setLoginForm] = useState({ username: '', password: '' });
  const [isRegister, setIsRegister] = useState(false);
  const hiddenTimeRef = useRef(0);

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

  const handleAuth = async (e: React.FormEvent) => {
    e.preventDefault();
    const endpoint = isRegister ? '/api/auth/register' : '/api/auth/login';
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
        alert(data.error);
      }
    } catch (err) {
      console.error('Auth failed:', err);
      alert('Authentication failed. Please try again.');
    }
  };

  const logout = () => {
    setAuth(null);
    localStorage.removeItem('jcowork_auth');
  };

  if (!auth) {
    return (
      <div className="login-container">
        <div className="login-card">
          <h1 style={{ fontSize: 28, marginBottom: 24 }}>Jcowork Agent</h1>
          <form onSubmit={handleAuth}>
            <input
              type="text"
              placeholder={t('username')}
              value={loginForm.username}
              onChange={(e) => setLoginForm({ ...loginForm, username: e.target.value })}
              style={{ width: '100%', padding: 10, marginBottom: 12, borderRadius: 8, border: '1px solid #555', background: '#1a1a1a', color: '#eee', fontSize: 16 }}
            />
            <input
              type="password"
              placeholder={t('password')}
              value={loginForm.password}
              onChange={(e) => setLoginForm({ ...loginForm, password: e.target.value })}
              style={{ width: '100%', padding: 10, marginBottom: 16, borderRadius: 8, border: '1px solid #555', background: '#1a1a1a', color: '#eee', fontSize: 16 }}
            />
            <button
              type="submit"
              style={{ width: '100%', padding: 10, borderRadius: 8, border: 'none', background: '#1a73e8', color: '#fff', fontSize: 16, cursor: 'pointer' }}
            >
              {isRegister ? t('register') : t('login')}
            </button>
          </form>
          <p style={{ marginTop: 16, textAlign: 'center' }}>
            <span style={{ color: '#888' }}>{isRegister ? t('alreadyHaveAccount') : t('dontHaveAccount')}</span>{' '}
            <a href="#" onClick={() => setIsRegister(!isRegister)} style={{ color: '#1a73e8' }}>
              {isRegister ? t('login') : t('register')}
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
          ) : (
            <Chat userId={auth.userId} token={auth.token} />
          )}
        </div>
      </div>
    </div>
  );
}
