// 登录页：邮箱密码登录（默认）/ 管理员密码登录 / 强制修改管理员密码。
// 短信注册暂时从前端隐藏（2026-10-11）：邮箱注册是唯一公开入口，后端接口未动，
// 改回 true 即可恢复短信登录页签。
const SMS_LOGIN_VISIBLE = false;
const { DEFAULT_SERVER_URL, normalizeServerUrl, STORAGE_KEYS } = require('../../config');
const api = require('../../utils/api');
const { getNavMetrics } = require('../../utils/ui');

Page({
  data: {
    mode: SMS_LOGIN_VISIBLE ? 'sms' : 'email', // sms | email | admin | changePwd
    smsEnabled: SMS_LOGIN_VISIBLE,
    emailMode: 'login', // 邮箱模式子页：login | register | forgot
    serverUrl: '',
    defaultServer: DEFAULT_SERVER_URL,
    phone: '',
    code: '',
    email: '',
    confirmPassword: '',
    username: '',
    password: '',
    // 图形验证码（仅注册子页）：captchaId 与服务端存储配对，图片为 data URL
    captchaId: '',
    captchaImage: '',
    captchaCode: '',
    captchaBusy: false,
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
    let mustChange = false;
    try {
      savedServer = wx.getStorageSync(STORAGE_KEYS.serverUrl) || '';
      savedUser = wx.getStorageSync(STORAGE_KEYS.username) || '';
      mustChange = !!wx.getStorageSync(STORAGE_KEYS.mustChangePassword);
    } catch (e) {}
    this.setData({
      ...metrics,
      serverUrl: savedServer || DEFAULT_SERVER_URL,
      username: savedUser,
      // 上次退出时改密尚未完成：令牌还在本地，直接回到改密界面
      mode: mustChange ? 'changePwd' : SMS_LOGIN_VISIBLE ? 'sms' : 'email',
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

  /** 邮箱模式内部切换：登录 / 注册账号 / 找回密码。进入注册页时换一张新图形验证码 */
  switchEmailMode(e) {
    if (this.data.loading) return;
    const emailMode = e.currentTarget.dataset.emode;
    this.setData({ emailMode, error: '' });
    if (emailMode === 'register') this.refreshCaptcha();
  },

  /** 拉取图形验证码：注册门槛，一次性使用，注册失败后必须换新的 */
  async refreshCaptcha() {
    if (this.data.captchaBusy) return;
    const serverUrl = normalizeServerUrl(this.data.serverUrl);
    if (!serverUrl) return;
    this.setData({ captchaBusy: true });
    try {
      const data = await api.getCaptcha(serverUrl);
      if (data && data.captcha_id && data.image) {
        this.setData({ captchaId: data.captcha_id, captchaImage: data.image, captchaCode: '' });
      }
    } catch (err) {
      // 图片拉取失败：保留旧图（若有），用户点图即可重试
    }
    this.setData({ captchaBusy: false });
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

  /** 获取验证码：短信（登录）与邮件（找回密码）共用入口；注册改用图形验证码 */
  async sendCode() {
    if (this.data.sendingCode || this.data.countdown > 0) return;
    if (this.data.mode === 'email') return this.sendEmailCode();
    const serverUrl = normalizeServerUrl(this.data.serverUrl);
    const phone = String(this.data.phone || '').trim();
    if (!serverUrl) {
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

    if (mode === 'changePwd') return this.submitChangePassword();

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
        mustChangePassword: !!data.must_change_password,
      });
      this.setData({ password: '' });
      // 管理员首次登录：服务端只放行资料接口，必须先改掉初始密码
      if (data.must_change_password) {
        this.stopCountdown();
        this.setData({ loading: false, mode: 'changePwd', code: '', countdown: 0, error: '' });
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
    } else if (emailMode === 'forgot') {
      if (!code) return this.setError('请输入验证码');
      if (!password) return this.setError('请输入新密码');
      if (password.length < 8) return this.setError('密码至少 8 位');
      if (password !== confirmPassword) return this.setError('两次输入的密码不一致');
    } else {
      // 注册：图形验证码（不消耗邮件额度），不要求邮件验证码
      if (!this.data.captchaId) {
        this.refreshCaptcha();
        return this.setError('图形验证码未就绪，请稍候或点击图片刷新');
      }
      if (!String(this.data.captchaCode || '').trim()) return this.setError('请输入图形验证码');
      if (!password) return this.setError('请设置密码');
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
          : await api.emailRegister(
              serverUrl,
              email,
              password,
              this.data.captchaId,
              String(this.data.captchaCode || '').trim()
            );
      if (!data || !data.token) {
        throw new Error('服务器响应异常（缺少令牌）');
      }
      getApp().saveSession({
        serverUrl,
        token: data.token,
        userId: data.user_id,
        username: data.username || email,
        mustChangePassword: !!data.must_change_password,
      });
      this.setData({ password: '', confirmPassword: '', code: '', captchaCode: '' });
      if (emailMode === 'register' && data.email_sent === false) {
        // 账号已建好可直接登录，只是账号密码邮件没发出去
        wx.showModal({
          title: '注册成功',
          content: '账号与密码的邮件发送失败，账号现在就可以使用；稍后可通过「找回密码」重置。',
          showCancel: false,
          success: () => this.enterApp(),
        });
        return;
      }
      this.enterApp();
    } catch (err) {
      // 注册的验证码无论对错都被服务端销毁，必须换一张再试
      if (emailMode === 'register') this.refreshCaptcha();
      this.setError((err && err.message) || '请求失败，请稍后重试');
    }
  },

  /** 管理员强制改密：令牌已通过密码或短信验证，只填新密码 */
  async submitChangePassword() {
    const password = String(this.data.password || '');
    const confirmPassword = String(this.data.confirmPassword || '');
    if (!password) return this.setError('请输入新密码');
    if (password.length < 8) return this.setError('密码至少 8 位');
    if (password !== confirmPassword) return this.setError('两次输入的密码不一致');
    this.setData({ loading: true, error: '' });
    try {
      await api.changePassword(password);
      getApp().markPasswordChanged();
      this.setData({ loading: false, password: '', confirmPassword: '' });
      this.enterApp();
    } catch (err) {
      this.setError((err && err.message) || '修改失败，请重试');
    }
  },

  /** 改密界面上唯一的出口：放弃这个令牌 */
  logout() {
    this.stopCountdown();
    getApp().clearSession();
    this.setData({
      mode: SMS_LOGIN_VISIBLE ? 'sms' : 'email',
      emailMode: 'login', phone: '', code: '', email: '',
      password: '', confirmPassword: '', countdown: 0, error: '',
      captchaId: '', captchaImage: '', captchaCode: '',
    });
  },
});
