// 全局应用状态：会话令牌、服务器地址等。启动时从本地存储恢复。
const { STORAGE_KEYS } = require('./config');

App({
  globalData: {
    serverUrl: '',
    token: '',
    userId: '',
    username: '',
    // 服务端在未绑定手机号前只放行 /api/profile*，因此这里也必须拦下所有页面
    mustBindPhone: false,
  },

  onLaunch() {
    this.restoreSession();
  },

  /** 从本地存储恢复登录态 */
  restoreSession() {
    const g = this.globalData;
    try {
      g.serverUrl = wx.getStorageSync(STORAGE_KEYS.serverUrl) || '';
      g.token = wx.getStorageSync(STORAGE_KEYS.token) || '';
      g.userId = wx.getStorageSync(STORAGE_KEYS.userId) || '';
      g.username = wx.getStorageSync(STORAGE_KEYS.username) || '';
      g.mustBindPhone = !!wx.getStorageSync(STORAGE_KEYS.mustBindPhone);
    } catch (e) {
      // 存储读取失败时按未登录处理
      g.token = '';
      g.userId = '';
      g.username = '';
      g.mustBindPhone = false;
    }
  },

  /** 登录成功后保存会话 */
  saveSession({ serverUrl, token, userId, username, mustBindPhone }) {
    const g = this.globalData;
    g.serverUrl = serverUrl;
    g.token = token;
    g.userId = userId;
    g.username = username || '';
    g.mustBindPhone = !!mustBindPhone;
    try {
      wx.setStorageSync(STORAGE_KEYS.serverUrl, serverUrl);
      wx.setStorageSync(STORAGE_KEYS.token, token);
      wx.setStorageSync(STORAGE_KEYS.userId, userId);
      wx.setStorageSync(STORAGE_KEYS.username, username || '');
      wx.setStorageSync(STORAGE_KEYS.mustBindPhone, g.mustBindPhone);
    } catch (e) {}
  },

  /** 手机号绑定完成：解除页面级拦截 */
  markPhoneBound() {
    this.globalData.mustBindPhone = false;
    try {
      wx.setStorageSync(STORAGE_KEYS.mustBindPhone, false);
    } catch (e) {}
  },

  /** 退出登录：仅清令牌，保留服务器地址与用户名便于下次预填 */
  clearSession() {
    const g = this.globalData;
    g.token = '';
    g.userId = '';
    g.mustBindPhone = false;
    try {
      wx.removeStorageSync(STORAGE_KEYS.token);
      wx.removeStorageSync(STORAGE_KEYS.userId);
      wx.removeStorageSync(STORAGE_KEYS.mustBindPhone);
    } catch (e) {}
  },

  isLoggedIn() {
    return !!(this.globalData.token && this.globalData.serverUrl && !this.globalData.mustBindPhone);
  },
});
