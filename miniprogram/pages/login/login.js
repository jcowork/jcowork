// 登录页：短信验证码登录（默认）/ 邮箱密码登录（登录·注册·找回密码）/ 管理员密码引导 / 强制绑定手机号。
const { DEFAULT_SERVER_URL, normalizeServerUrl, STORAGE_KEYS } = require('../../config');
const api = require('../../utils/api');
const { getNavMetrics } = require('../../utils/ui');

Page({
  data: {
    mode: 'sms', // sms | email | admin | bind
    emailMode: 'login', // 邮箱模式子页：login | register | forgot
    serverUrl: '',
    defaultServer: DEFAULT_SERVER_URL,
    phone: '',
    code: '',
    email: '',
    confirmPassword: '',
    username: '',
    password: '',
    loading: false,
    sendingCode: false,
    countdown: 0,
    error: '',
    statusBarHeight: 20,
    navTotalHeight: 64,
  },

  onLoad() {
    const metrics = getNavMetrics();
    let savedServer = '';
    let savedUser = '';
    let mustBind = false;
    try {
      savedServer = wx.getStorageSync(STORAGE_KEYS.serverUrl) || '';
      savedUser = wx.getStorageSync(STORAGE_KEYS.username) || '';
      mustBind = !!wx.getStorageSync(STORAGE_KEYS.mustBindPhone);
    } catch (e) {}
    this.setData({
      ...metrics,
      serverUrl: savedServer || DEFAULT_SERVER_URL,
      username: savedUser,
      // 上次退出时绑定尚未完成：令牌还在本地，直接回到绑定界面
      mode: mustBind ? 'bind' : 'sms',
    });
  },

  onShow() {
    const app = getApp();
    if (app && app.isLoggedIn()) {
      wx.reLaunch({ url: '/pages/chat/chat' });
    }
  },

  onUnload() {
    this.stopCountdown();
  },

  switchMode(e) {
    if (this.data.loading) return;
    this.stopCountdown();
    this.setData({ mode: e.currentTarget.dataset.mode, emailMode: 'login', error: '', code: '', countdown: 0 });
  },

  /** 邮箱模式内部切换：登录 / 注册账号 / 找回密码（共用同一发送接口，倒计时保留） */
  switchEmailMode(e) {
    if (this.data.loading) return;
    this.setData({ emailMode: e.currentTarget.dataset.emode, error: '' });
  },

  onInput(e) {
    this.setData({ [e.currentTarget.dataset.field]: e.detail.value });
  },

  useDefaultServer() {
    this.setData({ serverUrl: DEFAULT_SERVER_URL, error: '' });
  },

  setError(msg) {
    this.setData({ error: msg, loading: false, sendingCode: false });
  },

  stopCountdown() {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  },

  startCountdown(secs) {
    this.stopCountdown();
    this.setData({ countdown: secs });
    this.timer = setInterval(() => {
      const next = this.data.countdown - 1;
      if (next <= 0) {
        this.stopCountdown();
        this.setData({ countdown: 0 });
      } else {
        this.setData({ countdown: next });
      }
    }, 1000);
  },

  /** 获取验证码：短信（登录与绑定）与邮件（注册与找回密码）共用入口 */
  async sendCode() {
    if (this.data.sendingCode || this.data.countdown > 0) return;
    if (this.data.mode === 'email') return this.sendEmailCode();
    const serverUrl = normalizeServerUrl(this.data.serverUrl);
    const phone = String(this.data.phone || '').trim();
    if (this.data.mode !== 'bind' && !serverUrl) {
      this.setError('请输入有效的服务器地址，如 https://your-server.com');
      return;
    }
    if (!phone) {
      this.setError('请输入手机号');
      return;
    }
    this.setData({ sendingCode: true, error: '' });
    try {
      const data = await api.sendSmsCode(serverUrl, phone);
      this.startCountdown(Number(data && data.resend_after) || 60);
      wx.showToast({ title: '验证码已发送', icon: 'none' });
    } catch (err) {
      this.setError((err && err.message) || '验证码发送失败，请稍后重试');
    }
    this.setData({ sendingCode: false });
  },

  async sendEmailCode() {
    const serverUrl = normalizeServerUrl(this.data.serverUrl);
    const email = String(this.data.email || '').trim();
    if (!serverUrl) {
      this.setError('请输入有效的服务器地址，如 https://your-server.com');
      return;
    }
    if (!email) {
      this.setError('请输入邮箱地址');
      return;
    }
    this.setData({ sendingCode: true, error: '' });
    try {
      const data = await api.sendEmailCode(serverUrl, email);
      this.startCountdown(Number(data && data.resend_after) || 60);
      wx.showToast({ title: '验证码已发送', icon: 'none' });
    } catch (err) {
      this.setError((err && err.message) || '验证码发送失败，请稍后重试');
    }
    this.setData({ sendingCode: false });
  },

  enterApp() {
    wx.showToast({ title: '登录成功', icon: 'success' });
    setTimeout(() => {
      wx.reLaunch({ url: '/pages/chat/chat' });
    }, 350);
  },

  async submit() {
    if (this.data.loading) return;
    const mode = this.data.mode;
    const serverUrl = normalizeServerUrl(this.data.serverUrl);
    const phone = String(this.data.phone || '').trim();
    const code = String(this.data.code || '').trim();

    if (mode === 'bind') return this.submitBind(phone, code);

    if (!serverUrl) {
      this.setError('请输入有效的服务器地址，如 https://your-server.com');
      return;
    }
    if (mode === 'email') return this.submitEmail(serverUrl);
    if (mode === 'sms') {
      if (!phone) return this.setError('请输入手机号');
      if (!code) return this.setError('请输入验证码');
    } else {
      if (!String(this.data.username || '').trim()) return this.setError('请输入用户名');
      if (!this.data.password) return this.setError('请输入密码');
    }

    this.setData({ loading: true, error: '' });
    try {
      const data =
        mode === 'sms'
          ? await api.smsLogin(serverUrl, phone, code)
          : await api.adminLogin(serverUrl, String(this.data.username).trim(), this.data.password);
      if (!data || !data.token) {
        throw new Error('服务器响应异常（缺少令牌）');
      }
      getApp().saveSession({
        serverUrl,
        token: data.token,
        userId: data.user_id,
        username: data.username || (mode === 'sms' ? phone : String(this.data.username).trim()),
        mustBindPhone: !!data.must_bind_phone,
      });
      this.setData({ password: '' });
      // 引导登录后服务端只放行资料接口，必须先绑定手机号
      if (data.must_bind_phone) {
        this.stopCountdown();
        this.setData({ loading: false, mode: 'bind', code: '', countdown: 0, error: '' });
        return;
      }
      this.enterApp();
    } catch (err) {
      this.setError((err && err.message) || '请求失败，请稍后重试');
    }
  },

  /** 邮箱模式提交：登录 / 注册 / 找回密码 */
  async submitEmail(serverUrl) {
    const emailMode = this.data.emailMode;
    const email = String(this.data.email || '').trim();
    const code = String(this.data.code || '').trim();
    const password = String(this.data.password || '');
    const confirmPassword = String(this.data.confirmPassword || '');

    if (!email) return this.setError('请输入邮箱地址');
    if (emailMode === 'login') {
      if (!password) return this.setError('请输入密码');
    } else {
      if (!code) return this.setError('请输入验证码');
      if (!password) return this.setError(emailMode === 'register' ? '请设置密码' : '请输入新密码');
      if (password.length < 8) return this.setError('密码至少 8 位');
      if (password !== confirmPassword) return this.setError('两次输入的密码不一致');
    }

    this.setData({ loading: true, error: '' });
    try {
      if (emailMode === 'forgot') {
        await api.resetPassword(serverUrl, email, code, password);
        this.stopCountdown();
        this.setData({
          loading: false, emailMode: 'login', code: '', password: '', confirmPassword: '', countdown: 0,
        });
        wx.showToast({ title: '密码已重置，请使用新密码登录', icon: 'none' });
        return;
      }
      const data =
        emailMode === 'login'
          ? await api.emailLogin(serverUrl, email, password)
          : await api.emailRegister(serverUrl, email, password, code);
      if (!data || !data.token) {
        throw new Error('服务器响应异常（缺少令牌）');
      }
      getApp().saveSession({
        serverUrl,
        token: data.token,
        userId: data.user_id,
        username: data.username || email,
        mustBindPhone: !!data.must_bind_phone,
      });
      this.setData({ password: '', confirmPassword: '', code: '' });
      this.enterApp();
    } catch (err) {
      this.setError((err && err.message) || '请求失败，请稍后重试');
    }
  },

  async submitBind(phone, code) {
    if (!phone) return this.setError('请输入手机号');
    if (!code) return this.setError('请输入验证码');
    this.setData({ loading: true, error: '' });
    try {
      const data = await api.bindPhone(phone, code);
      if (!data || !data.phone_bound) {
        throw new Error('绑定失败，请重试');
      }
      const app = getApp();
      app.markPhoneBound();
      if (data.username) {
        app.globalData.username = data.username;
        try {
          wx.setStorageSync(STORAGE_KEYS.username, data.username);
        } catch (e) {}
      }
      this.setData({ loading: false, phone: '', code: '' });
      this.stopCountdown();
      this.enterApp();
    } catch (err) {
      this.setError((err && err.message) || '绑定失败，请重试');
    }
  },

  /** 绑定界面上唯一的出口：放弃这个令牌 */
  logout() {
    this.stopCountdown();
    getApp().clearSession();
    this.setData({
      mode: 'sms', emailMode: 'login', phone: '', code: '', email: '',
      password: '', confirmPassword: '', countdown: 0, error: '',
    });
  },
});
