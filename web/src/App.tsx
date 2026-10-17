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
import EmailCodeFields from './components/EmailCode';
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

// SMS signup is temporarily hidden from the UI (decided 2026-10-11): the
// email register form is the only public entry for now, and the backend
// endpoints stay untouched — flip this to true to bring the SMS card back.
const SMS_LOGIN_VISIBLE = false;

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
  // Email is the default way in while SMS signup is hidden; email covers
  // register / login / forgot-password with a password, and the admin form
  // exists solely so a freshly seeded admin can reach the forced
  // password-change screen.
  const [authMode, setAuthMode] = useState<'sms' | 'email' | 'admin'>(SMS_LOGIN_VISIBLE ? 'sms' : 'email');
  // Which of the three email screens is showing.
  const [emailMode, setEmailMode] = useState<'login' | 'register' | 'forgot'>('login');
  const [smsForm, setSmsForm] = useState({ phone: '', code: '' });
  const [emailForm, setEmailForm] = useState({ username: '', email: '', code: '', password: '', confirmPassword: '' });
  const [adminForm, setAdminForm] = useState({ username: '', password: '' });
  const [authBusy, setAuthBusy] = useState(false);
  const [sendingCode, setSendingCode] = useState(false);
  const [codeCountdown, setCodeCountdown] = useState(0);
  // Separate cooldown/busy from the SMS form so a countdown started on one
  // does not carry over to the other.
  const [sendingEmailCode, setSendingEmailCode] = useState(false);
  const [emailCountdown, setEmailCountdown] = useState(0);
  const [authError, setAuthError] = useState('');
  // Success notice for the email card (password reset redirects to login).
  const [authSuccess, setAuthSuccess] = useState('');
  // Forced password change (admin bootstrap).
  const [changePwdForm, setChangePwdForm] = useState({ password: '', confirmPassword: '' });
  const [changePwdBusy, setChangePwdBusy] = useState(false);
  const [changePwdError, setChangePwdError] = useState('');
  // Forced mail-channel setup (admin, unconfigured auto→dev fallback).
  const [mailForm, setMailForm] = useState({ host: '', port: '465', username: '', password: '', from: '' });
  const [mailBusy, setMailBusy] = useState(false);
  const [mailError, setMailError] = useState('');
  // Image captcha for the register screen (id + data-URL image).
  const [captcha, setCaptcha] = useState<{ id: string; image: string }>({ id: '', image: '' });
  const [captchaBusy, setCaptchaBusy] = useState(false);
  // One-shot notice shown after signing in (e.g. the credentials email failed).
  const [postLoginNotice, setPostLoginNotice] = useState('');
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
    must_change_password?: boolean;
    must_configure_mail?: boolean;
  }) => {
    const authState: AuthState = {
      token: data.token,
      userId: data.user_id,
      username: data.username,
      isAdmin: !!data.is_admin,
      mustChangePassword: !!data.must_change_password,
      mustConfigureMail: !!data.must_configure_mail,
    };
    // Upsert into multi-account store
    const updated = upsertAccount(authState);
    setAccounts(updated);
    setActiveUserId(authState.userId);
    setActiveAccount(authState.userId);
    setAddingAccount(false);
    setSmsForm({ phone: '', code: '' });
    setEmailForm({ username: '', email: '', code: '', password: '', confirmPassword: '' });
    setEmailMode('login');
    setAdminForm({ username: '', password: '' });
    setAuthSuccess('');
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

  /// Only reaches a session for the seeded admin; the server refuses everyone
  /// else. A successful call with the change flag set means "change the
  /// bootstrap password before anything else".
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

  /// Ask the server to mail a code to the address in the form. Shared by the
  /// register and forgot-password screens, which hit the same endpoint.
  const handleSendEmailCode = async () => {
    setAuthError('');
    setAuthSuccess('');
    const email = emailForm.email.trim();
    if (!email) {
      setAuthError(t('invalidEmail'));
      return;
    }
    setSendingEmailCode(true);
    try {
      const { ok, data } = await postJson('/api/auth/email/send', { email });
      if (!ok) {
        setAuthError(mapAuthError(data.error ?? '', data));
        return;
      }
      // The server picks the cooldown so the button stays in step with the
      // throttle that will actually reject the next request.
      setEmailCountdown(Number(data.resend_after) || 60);
    } catch {
      setAuthError(t('networkError'));
    } finally {
      setSendingEmailCode(false);
    }
  };

  const handleEmailLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setAuthError('');
    setAuthBusy(true);
    try {
      const { ok, data } = await postJson('/api/auth/email/login', {
        username: emailForm.username.trim(),
        password: emailForm.password,
      });
      finishAuth(ok, data, setAuthError);
    } catch {
      setAuthError(t('networkError'));
    } finally {
      setAuthBusy(false);
    }
  };

  /// Fetch a fresh image captcha for the register screen. Called on entering
  /// the mode, on image click, and after every failed attempt — the server
  /// burns the challenge either way.
  const refreshCaptcha = async () => {
    if (captchaBusy) return;
    setCaptchaBusy(true);
    try {
      const res = await fetch(`${API_BASE}/api/auth/captcha`);
      const data = await res.json().catch(() => ({}));
      if (res.ok && data.captcha_id) {
        setCaptcha({ id: data.captcha_id, image: data.image });
        setEmailForm((f) => ({ ...f, code: '' }));
      }
    } catch {
      // Network hiccup: keep the old image; a retry click will refetch.
    } finally {
      setCaptchaBusy(false);
    }
  };

  // A captcha is only valid for the register screen, so mint one whenever the
  // user lands there.
  useEffect(() => {
    if (authMode === 'email' && emailMode === 'register') {
      // Minting on entry is exactly the external-system sync effects are for;
      // the transient busy-state flicker is accepted.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      refreshCaptcha();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [authMode, emailMode]);

  /// New account: email + password + a solved image captcha (no emailed code
  /// anymore). A successful response carries a session, like login, plus
  /// `email_sent` saying whether the account/password record reached the
  /// mailbox.
  const handleEmailRegister = async (e: React.FormEvent) => {
    e.preventDefault();
    setAuthError('');
    if (emailForm.password !== emailForm.confirmPassword) {
      setAuthError(t('passwordMismatch'));
      return;
    }
    if (!captcha.id || !emailForm.code.trim()) {
      setAuthError(t('captchaRequired'));
      return;
    }
    setAuthBusy(true);
    try {
      const { ok, data } = await postJson('/api/auth/email/register', {
        username: emailForm.username.trim(),
        email: emailForm.email.trim(),
        password: emailForm.password,
        captcha_id: captcha.id,
        captcha_code: emailForm.code.trim(),
      });
      if (ok && data.token) {
        // The credentials email failed: the account works, but the user
        // asked for that record, so surface it instead of staying silent.
        if (data.email_sent === false) {
          setPostLoginNotice(t('registeredMailFailed'));
        }
        finishAuth(ok, data, setAuthError);
      } else {
        setAuthError(mapAuthError(data.error ?? '', data));
        refreshCaptcha();
      }
    } catch {
      setAuthError(t('networkError'));
    } finally {
      setAuthBusy(false);
    }
  };

  /// Forgot password: the mailed code unlocks setting a new one. No session is
  /// returned — the user signs in with the new password to see it work.
  const handlePasswordReset = async (e: React.FormEvent) => {
    e.preventDefault();
    setAuthError('');
    if (emailForm.password !== emailForm.confirmPassword) {
      setAuthError(t('passwordMismatch'));
      return;
    }
    setAuthBusy(true);
    try {
      const { ok, data } = await postJson('/api/auth/password/reset', {
        email: emailForm.email.trim(),
        code: emailForm.code.trim(),
        new_password: emailForm.password,
      });
      if (ok) {
        setEmailMode('login');
        setEmailForm({ ...emailForm, code: '', password: '', confirmPassword: '' });
        setAuthSuccess(t('passwordResetDone'));
      } else {
        setAuthError(mapAuthError(data.error ?? '', data));
      }
    } catch {
      setAuthError(t('networkError'));
    } finally {
      setAuthBusy(false);
    }
  };

  /// Submit the forced password change. The bootstrap token keeps working
  /// afterwards — the server re-reads the account on every request — so there
  /// is no need to sign in again.
  const handleChangePassword = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!activeAccount) return;
    setChangePwdError('');
    if (changePwdForm.password !== changePwdForm.confirmPassword) {
      setChangePwdError(t('passwordMismatch'));
      return;
    }
    setChangePwdBusy(true);
    try {
      const { ok, data } = await postJson(
        '/api/profile/password',
        { new_password: changePwdForm.password },
        activeAccount.token,
      );
      if (ok) {
        // The response is the updated profile; its mail flag decides whether
        // the setup screen below comes up next.
        const mustMail = data.must_configure_mail === true;
        const updated = upsertAccount({ ...activeAccount, mustChangePassword: false, mustConfigureMail: mustMail });
        setAccounts(updated);
        setChangePwdForm({ password: '', confirmPassword: '' });
      } else {
        setChangePwdError(mapAuthError(data.error ?? '', data));
      }
    } catch {
      setChangePwdError(t('networkError'));
    } finally {
      setChangePwdBusy(false);
    }
  };

  /// PUT a JSON body with the admin token (mail config endpoints).
  const putJson = async (path: string, body: Record<string, unknown>, token: string) => {
    const res = await fetch(`${API_BASE}${path}`, {
      method: 'PUT',
      headers: {
        'Content-Type': 'application/json',
        Authorization: `Bearer ${token}`,
      },
      body: JSON.stringify(body),
    });
    const data = await res.json().catch(() => ({}));
    return { ok: res.ok, data };
  };

  /// Submit the SMTP settings from the forced-setup screen. The server
  /// rebuilds the channel in place, so success means email login works right
  /// away — no restart, no re-login.
  const handleSaveMailConfig = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!activeAccount) return;
    if (!mailForm.host.trim() || !mailForm.username.trim() || !mailForm.password || !mailForm.from.trim()) {
      setMailError(t('smtpFieldsRequired'));
      return;
    }
    setMailError('');
    setMailBusy(true);
    try {
      const { ok, data } = await putJson(
        '/api/admin/mail',
        {
          provider: 'smtp',
          smtp_host: mailForm.host.trim(),
          smtp_port: Number(mailForm.port) || 465,
          smtp_username: mailForm.username.trim(),
          smtp_password: mailForm.password,
          smtp_from: mailForm.from.trim(),
        },
        activeAccount.token,
      );
      if (ok) {
        const updated = upsertAccount({ ...activeAccount, mustConfigureMail: false });
        setAccounts(updated);
        setMailForm({ host: '', port: '465', username: '', password: '', from: '' });
      } else {
        setMailError(mapAuthError(data.error ?? '', data));
      }
    } catch {
      setMailError(t('networkError'));
    } finally {
      setMailBusy(false);
    }
  };

  /// Explicitly stay on the dev channel. This is the admin's declared choice
  /// — email login stays disabled, codes go to the server log — and lifts the
  /// gate the same as a full SMTP setup would.
  const handleDisableMail = async () => {
    if (!activeAccount) return;
    setMailError('');
    setMailBusy(true);
    try {
      const { ok, data } = await putJson('/api/admin/mail', { provider: 'dev' }, activeAccount.token);
      if (ok) {
        const updated = upsertAccount({ ...activeAccount, mustConfigureMail: false });
        setAccounts(updated);
      } else {
        setMailError(mapAuthError(data.error ?? '', data));
      }
    } catch {
      setMailError(t('networkError'));
    } finally {
      setMailBusy(false);
    }
  };

  const switchAuthMode = (mode: 'sms' | 'email' | 'admin') => {
    setAuthMode(mode);
    setAuthError('');
    setAuthSuccess('');
  };

  const switchEmailMode = (mode: 'login' | 'register' | 'forgot') => {
    setEmailMode(mode);
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
    setViewingPublicUser(null);
    setShowAdminUsers(false);
  }, [activeUserId, activeAccount, conversations, activeConvId]);

  const handleAddAccount = useCallback(() => {
    setAddingAccount(true);
    setAuthMode(SMS_LOGIN_VISIBLE ? 'sms' : 'email');
    setEmailMode('login');
    setAuthError('');
    setAuthSuccess('');
    setSmsForm({ phone: '', code: '' });
    setEmailForm({ username: '', email: '', code: '', password: '', confirmPassword: '' });
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
    if (emailCountdown <= 0) return;
    const h = window.setTimeout(() => setEmailCountdown((c) => c - 1), 1000);
    return () => window.clearTimeout(h);
  }, [emailCountdown]);

  // Probe every stored account once on load. `/api/profile` stays reachable
  // through both admin gates, so it doubles as the check for both — the lock
  // flags must be re-derived after a reload. `must_change_password` /
  // `must_configure_mail` are undefined on servers predating those flows; the
  // strict checks below then skip them.
  useEffect(() => {
    loadAccounts().forEach((acct) => {
      if (acct.mustChangePassword) return;
      fetch('/api/profile', {
        headers: { Authorization: `Bearer ${acct.token}` },
      })
        .then((res) => (res.ok ? res.json() : null))
        .then((data) => {
          if (!data) return;
          setAccounts((prev) => {
            let changed = false;
            const next = prev.map((a) => {
              if (a.userId !== acct.userId) return a;
              const mustChange = data.must_change_password === true || a.mustChangePassword;
              const mustMail = data.must_configure_mail === true || a.mustConfigureMail;
              if (a.mustChangePassword === mustChange && a.mustConfigureMail === mustMail) return a;
              changed = true;
              return { ...a, mustChangePassword: mustChange, mustConfigureMail: mustMail };
            });
            if (changed) saveAccounts(next);
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
          ) : authMode === 'email' ? (
            <div>
              <div style={{ display: 'flex', gap: 4, marginBottom: 16 }}>
                {([
                  ['login', t('emailLoginTab')],
                  ['register', t('emailRegisterTab')],
                  ['forgot', t('emailForgotTab')],
                ] as const).map(([mode, label]) => (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => switchEmailMode(mode)}
                    style={{
                      flex: 1, padding: '6px 0', borderRadius: 8, fontSize: 13,
                      border: `1px solid ${emailMode === mode ? '#1a73e8' : '#444'}`,
                      background: emailMode === mode ? '#1a73e822' : 'transparent',
                      color: emailMode === mode ? '#58a6ff' : '#888',
                      cursor: 'pointer',
                    }}
                  >
                    {label}
                  </button>
                ))}
              </div>
              {emailMode === 'login' ? (
                <form onSubmit={handleEmailLogin}>
                  <AuthBanner msg={authError} type="error" />
                  <AuthBanner msg={authSuccess} type="success" />
                  <input
                    type="text"
                    autoComplete="username"
                    placeholder={t('username')}
                    value={emailForm.username}
                    onChange={(e) => { setEmailForm({ ...emailForm, username: e.target.value }); setAuthError(''); setAuthSuccess(''); }}
                    style={authInputStyle}
                    autoFocus
                  />
                  <input
                    type="password"
                    autoComplete="current-password"
                    placeholder={t('password')}
                    value={emailForm.password}
                    onChange={(e) => { setEmailForm({ ...emailForm, password: e.target.value }); setAuthError(''); }}
                    style={authInputStyle}
                  />
                  <button
                    type="submit"
                    disabled={authBusy}
                    style={{ ...authBtnStyle, marginBottom: 0, background: authBusy ? '#333' : '#1a73e8', cursor: authBusy ? 'not-allowed' : 'pointer' }}
                  >
                    {authBusy ? t('loggingIn') : t('login')}
                  </button>
                  <p style={{ color: '#888', fontSize: 12, lineHeight: 1.5, margin: '12px 0 0', textAlign: 'center' }}>
                    {t('emailLoginHint')}
                  </p>
                </form>
              ) : emailMode === 'register' ? (
                <form onSubmit={handleEmailRegister}>
                  <AuthBanner msg={authError} type="error" />
                  <input
                    type="text"
                    autoComplete="username"
                    placeholder={t('username')}
                    value={emailForm.username}
                    onChange={(e) => { setEmailForm({ ...emailForm, username: e.target.value }); setAuthError(''); }}
                    style={authInputStyle}
                  />
                  <input
                    type="email"
                    autoComplete="email"
                    placeholder={t('emailAddress')}
                    value={emailForm.email}
                    onChange={(e) => { setEmailForm({ ...emailForm, email: e.target.value }); setAuthError(''); }}
                    style={authInputStyle}
                  />
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                    <input
                      placeholder={t('captchaPlaceholder')}
                      value={emailForm.code}
                      onChange={(e) => { setEmailForm({ ...emailForm, code: e.target.value }); setAuthError(''); }}
                      style={{ ...authInputStyle, flex: 1, marginBottom: 0 }}
                      autoComplete="off"
                    />
                    {/* Click to refresh: the server burns a challenge on every
                        attempt, so a new image must come with each retry. */}
                    {captcha.image ? (
                      <img
                        src={captcha.image}
                        alt="captcha"
                        onClick={refreshCaptcha}
                        title={t('captchaClickToRefresh')}
                        style={{ height: 42, borderRadius: 6, cursor: captchaBusy ? 'wait' : 'pointer', opacity: captchaBusy ? 0.5 : 1 }}
                      />
                    ) : (
                      <button
                        type="button"
                        onClick={refreshCaptcha}
                        disabled={captchaBusy}
                        style={{ height: 42, padding: '0 10px', borderRadius: 6, border: '1px solid #444', background: 'transparent', color: '#888', cursor: captchaBusy ? 'not-allowed' : 'pointer', whiteSpace: 'nowrap' }}
                      >
                        {t('captchaClickToRefresh')}
                      </button>
                    )}
                  </div>
                  <input
                    type="password"
                    autoComplete="new-password"
                    placeholder={t('newPassword')}
                    value={emailForm.password}
                    onChange={(e) => { setEmailForm({ ...emailForm, password: e.target.value }); setAuthError(''); }}
                    style={authInputStyle}
                  />
                  <input
                    type="password"
                    autoComplete="new-password"
                    placeholder={t('confirmPassword')}
                    value={emailForm.confirmPassword}
                    onChange={(e) => { setEmailForm({ ...emailForm, confirmPassword: e.target.value }); setAuthError(''); }}
                    style={authInputStyle}
                  />
                  <button
                    type="submit"
                    disabled={authBusy}
                    style={{ ...authBtnStyle, marginBottom: 0, background: authBusy ? '#333' : '#1a73e8', cursor: authBusy ? 'not-allowed' : 'pointer' }}
                  >
                    {authBusy ? t('registering') : t('register')}
                  </button>
                  <p style={{ color: '#888', fontSize: 12, lineHeight: 1.5, margin: '12px 0 0', textAlign: 'center' }}>
                    {t('emailRegisterHint')}
                  </p>
                </form>
              ) : (
                <form onSubmit={handlePasswordReset}>
                  <AuthBanner msg={authError} type="error" />
                  <EmailCodeFields
                    email={emailForm.email}
                    code={emailForm.code}
                    onEmailChange={(v) => { setEmailForm({ ...emailForm, email: v }); setAuthError(''); }}
                    onCodeChange={(v) => { setEmailForm({ ...emailForm, code: v }); setAuthError(''); }}
                    onSendCode={handleSendEmailCode}
                    countdown={emailCountdown}
                    sending={sendingEmailCode}
                  />
                  <input
                    type="password"
                    autoComplete="new-password"
                    placeholder={t('newPassword')}
                    value={emailForm.password}
                    onChange={(e) => { setEmailForm({ ...emailForm, password: e.target.value }); setAuthError(''); }}
                    style={authInputStyle}
                  />
                  <input
                    type="password"
                    autoComplete="new-password"
                    placeholder={t('confirmPassword')}
                    value={emailForm.confirmPassword}
                    onChange={(e) => { setEmailForm({ ...emailForm, confirmPassword: e.target.value }); setAuthError(''); }}
                    style={authInputStyle}
                  />
                  <button
                    type="submit"
                    disabled={authBusy}
                    style={{ ...authBtnStyle, marginBottom: 0, background: authBusy ? '#333' : '#1a73e8', cursor: authBusy ? 'not-allowed' : 'pointer' }}
                  >
                    {authBusy ? t('resetting') : t('resetPassword')}
                  </button>
                </form>
              )}
            </div>
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
          <p style={{ marginTop: 16, textAlign: 'center', fontSize: 13 }}>
            {authMode === 'sms' ? (
              <>
                <a href="#" onClick={() => switchAuthMode('email')} style={{ color: '#888' }}>
                  {t('emailLoginEntry')}
                </a>
                <span style={{ color: '#555', margin: '0 10px' }}>·</span>
                <a href="#" onClick={() => switchAuthMode('admin')} style={{ color: '#888' }}>
                  {t('adminLoginEntry')}
                </a>
              </>
            ) : authMode === 'email' ? (
              // With SMS hidden the email card is the hub: it carries the
              // admin entry; otherwise it only links back to the SMS card.
              SMS_LOGIN_VISIBLE ? (
                <a href="#" onClick={() => switchAuthMode('sms')} style={{ color: '#888', fontSize: 13 }}>
                  {t('smsLoginEntry')}
                </a>
              ) : (
                <a href="#" onClick={() => switchAuthMode('admin')} style={{ color: '#888', fontSize: 13 }}>
                  {t('adminLoginEntry')}
                </a>
              )
            ) : (
              // The admin card's exit goes back to whatever the hub is.
              <a
                href="#"
                onClick={() => switchAuthMode(SMS_LOGIN_VISIBLE ? 'sms' : 'email')}
                style={{ color: '#888', fontSize: 13 }}
              >
                {SMS_LOGIN_VISIBLE ? t('smsLoginEntry') : t('emailLoginEntry')}
              </a>
            )}
          </p>
        </div>
      </div>
    );
  }

  // Forced password change. This admin account holds a valid token but still
  // runs on the bootstrap password, and the server answers every other
  // endpoint with 403 until it is replaced — so there is deliberately no way
  // past this screen except changing the password or signing out.
  if (activeAccount?.mustChangePassword) {
    return (
      <div className="login-container">
        <div className="login-card">
          <img src="/logo.png" className="login-logo" alt="Jcowork" />
          <h1 style={{ fontSize: 24, marginBottom: 8 }}>{t('changePasswordTitle')}</h1>
          <p style={{ color: '#888', fontSize: 13, lineHeight: 1.6, marginBottom: 20 }}>
            {t('changePasswordHint')}
          </p>
          <form onSubmit={handleChangePassword}>
            <AuthBanner msg={changePwdError} type="error" />
            <input
              type="password"
              autoComplete="new-password"
              autoFocus
              placeholder={t('newPassword')}
              value={changePwdForm.password}
              onChange={(e) => { setChangePwdForm({ ...changePwdForm, password: e.target.value }); setChangePwdError(''); }}
              style={authInputStyle}
            />
            <input
              type="password"
              autoComplete="new-password"
              placeholder={t('confirmPassword')}
              value={changePwdForm.confirmPassword}
              onChange={(e) => { setChangePwdForm({ ...changePwdForm, confirmPassword: e.target.value }); setChangePwdError(''); }}
              style={authInputStyle}
            />
            <button
              type="submit"
              disabled={changePwdBusy}
              style={{ ...authBtnStyle, marginBottom: 0, background: changePwdBusy ? '#333' : '#1a73e8', cursor: changePwdBusy ? 'not-allowed' : 'pointer' }}
            >
              {changePwdBusy ? t('saving') : t('saveAndContinue')}
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

  // Forced mail-channel setup, second act of the admin bootstrap. While the
  // channel resolves `auto` to the unconfigured dev fallback the server
  // answers everything but the mail config endpoints with 403, so the only
  // ways past this screen are: working SMTP settings, an explicit "stay on
  // dev" choice, or signing out.
  if (activeAccount?.mustConfigureMail) {
    return (
      <div className="login-container">
        <div className="login-card">
          <img src="/logo.png" className="login-logo" alt="Jcowork" />
          <h1 style={{ fontSize: 24, marginBottom: 8 }}>{t('mailSetupTitle')}</h1>
          <p style={{ color: '#888', fontSize: 13, lineHeight: 1.6, marginBottom: 20 }}>
            {t('mailSetupHint')}
          </p>
          <form onSubmit={handleSaveMailConfig}>
            <AuthBanner msg={mailError} type="error" />
            <input
              autoFocus
              placeholder={t('smtpHost')}
              value={mailForm.host}
              onChange={(e) => { setMailForm({ ...mailForm, host: e.target.value }); setMailError(''); }}
              style={authInputStyle}
            />
            <input
              type="number"
              placeholder={t('smtpPort')}
              value={mailForm.port}
              onChange={(e) => { setMailForm({ ...mailForm, port: e.target.value }); setMailError(''); }}
              style={authInputStyle}
            />
            <input
              placeholder={t('smtpUsername')}
              value={mailForm.username}
              onChange={(e) => { setMailForm({ ...mailForm, username: e.target.value }); setMailError(''); }}
              style={authInputStyle}
            />
            <input
              type="password"
              autoComplete="new-password"
              placeholder={t('smtpPassword')}
              value={mailForm.password}
              onChange={(e) => { setMailForm({ ...mailForm, password: e.target.value }); setMailError(''); }}
              style={authInputStyle}
            />
            <input
              placeholder={t('smtpFrom')}
              value={mailForm.from}
              onChange={(e) => { setMailForm({ ...mailForm, from: e.target.value }); setMailError(''); }}
              style={authInputStyle}
            />
            <button
              type="submit"
              disabled={mailBusy}
              style={{ ...authBtnStyle, marginBottom: 0, background: mailBusy ? '#333' : '#1a73e8', cursor: mailBusy ? 'not-allowed' : 'pointer' }}
            >
              {mailBusy ? t('saving') : t('saveAndEnable')}
            </button>
          </form>
          <button
            type="button"
            onClick={handleDisableMail}
            disabled={mailBusy}
            style={{
              width: '100%', padding: 10, marginTop: 10, borderRadius: 8,
              border: '1px solid #555', background: 'transparent', color: '#aaa',
              fontSize: 14, cursor: mailBusy ? 'not-allowed' : 'pointer',
            }}
          >
            {t('disableMailLogin')}
          </button>
          <p style={{ color: '#666', fontSize: 12, lineHeight: 1.6, marginTop: 10, marginBottom: 0 }}>
            {t('disableMailLoginHint')}
          </p>
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
        {/* One-shot notice (e.g. the registration-credentials email failed) */}
        {postLoginNotice && (
          <div
            style={{
              display: 'flex', alignItems: 'center', gap: 10,
              padding: '8px 14px', background: '#4a3b1a', borderBottom: '1px solid #6b5620',
              fontSize: 13, color: '#e8d9b0',
            }}
          >
            <span style={{ flex: 1 }}>{postLoginNotice}</span>
            <button
              onClick={() => setPostLoginNotice('')}
              style={{ background: 'none', border: 'none', color: '#e8d9b0', fontSize: 15, cursor: 'pointer', padding: '0 4px' }}
            >
              ✕
            </button>
          </div>
        )}
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
