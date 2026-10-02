// 登录 / 注册页：填写服务器地址 + 用户名密码。
const { DEFAULT_SERVER_URL, normalizeServerUrl, STORAGE_KEYS } = require('../../config');
const api = require('../../utils/api');
const { getNavMetrics } = require('../../utils/ui');

Page({
  data: {
    mode: 'login', // login | register
    serverUrl: '',
    defaultServer: DEFAULT_SERVER_URL,
    username: '',
    password: '',
    isPublic: false,
    loading: false,
    error: '',
    statusBarHeight: 20,
    navTotalHeight: 64,
  },

  onLoad() {
    const metrics = getNavMetrics();
    let savedServer = '';
    let savedUser = '';
    try {
      savedServer = wx.getStorageSync(STORAGE_KEYS.serverUrl) || '';
      savedUser = wx.getStorageSync(STORAGE_KEYS.username) || '';
    } catch (e) {}
    this.setData({
      ...metrics,
      serverUrl: savedServer || DEFAULT_SERVER_URL,
      username: savedUser,
    });
  },

  onShow() {
    const app = getApp();
    if (app && app.isLoggedIn()) {
      wx.reLaunch({ url: '/pages/chat/chat' });
    }
  },

  switchMode(e) {
    this.setData({ mode: e.currentTarget.dataset.mode, error: '' });
  },

  onInput(e) {
    this.setData({ [e.currentTarget.dataset.field]: e.detail.value });
  },

  onTogglePublic(e) {
    this.setData({ isPublic: e.detail.value });
  },

  useDefaultServer() {
    this.setData({ serverUrl: DEFAULT_SERVER_URL, error: '' });
  },

  setError(msg) {
    this.setData({ error: msg, loading: false });
  },

  async submit() {
    if (this.data.loading) return;
    const serverUrl = normalizeServerUrl(this.data.serverUrl);
    const username = String(this.data.username || '').trim();
    const password = String(this.data.password || '');
    const isRegister = this.data.mode === 'register';

    if (!serverUrl) {
      this.setError('请输入有效的服务器地址，如 https://your-server.com');
      return;
    }
    if (!username) {
      this.setError('请输入用户名');
      return;
    }
    if (!password) {
      this.setError('请输入密码');
      return;
    }

    this.setData({ loading: true, error: '' });
    try {
      const data = isRegister
        ? await api.register(serverUrl, username, password, this.data.isPublic)
        : await api.login(serverUrl, username, password);
      if (!data || !data.token) {
        throw new Error('服务器响应异常（缺少令牌）');
      }
      const app = getApp();
      app.saveSession({
        serverUrl,
        token: data.token,
        userId: data.user_id,
        username: data.username || username,
      });
      wx.showToast({ title: isRegister ? '注册成功' : '登录成功', icon: 'success' });
      setTimeout(() => {
        wx.reLaunch({ url: '/pages/chat/chat' });
      }, 350);
    } catch (err) {
      this.setError((err && err.message) || '请求失败，请稍后重试');
    }
  },
});
