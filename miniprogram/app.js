// 全局应用状态：会话令牌、服务器地址等。启动时从本地存储恢复。
const { STORAGE_KEYS } = require('./config');

App({
  globalData: {
    serverUrl: '',
    token: '',
    userId: '',
    username: '',
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
    } catch (e) {
      // 存储读取失败时按未登录处理
      g.token = '';
      g.userId = '';
      g.username = '';
    }
  },

  /** 登录成功后保存会话 */
  saveSession({ serverUrl, token, userId, username }) {
    const g = this.globalData;
    g.serverUrl = serverUrl;
    g.token = token;
    g.userId = userId;
    g.username = username || '';
    try {
      wx.setStorageSync(STORAGE_KEYS.serverUrl, serverUrl);
      wx.setStorageSync(STORAGE_KEYS.token, token);
      wx.setStorageSync(STORAGE_KEYS.userId, userId);
      wx.setStorageSync(STORAGE_KEYS.username, username || '');
    } catch (e) {}
  },

  /** 退出登录：仅清令牌，保留服务器地址与用户名便于下次预填 */
  clearSession() {
    const g = this.globalData;
    g.token = '';
    g.userId = '';
    try {
      wx.removeStorageSync(STORAGE_KEYS.token);
      wx.removeStorageSync(STORAGE_KEYS.userId);
    } catch (e) {}
  },

  isLoggedIn() {
    return !!(this.globalData.token && this.globalData.serverUrl);
  },
});
