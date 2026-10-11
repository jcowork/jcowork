// 全局应用状态：会话令牌、服务器地址等。启动时从本地存储恢复。
const { STORAGE_KEYS } = require('./config');

App({
  globalData: {
    serverUrl: '',
    token: '',
    userId: '',
    username: '',
    // 服务端在管理员换掉初始密码前只放行 /api/profile*，因此这里也必须
    // 拦下所有页面
    mustChangePassword: false,
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
      g.mustChangePassword = !!wx.getStorageSync(STORAGE_KEYS.mustChangePassword);
      // 旧版本遗留的绑定标记，读到就清掉
      wx.removeStorageSync('jcowork_must_bind_phone');
    } catch (e) {
      // 存储读取失败时按未登录处理
      g.token = '';
      g.userId = '';
      g.username = '';
      g.mustChangePassword = false;
    }
  },

  /** 登录成功后保存会话 */
  saveSession({ serverUrl, token, userId, username, mustChangePassword }) {
    const g = this.globalData;
    g.serverUrl = serverUrl;
    g.token = token;
    g.userId = userId;
    g.username = username || '';
    g.mustChangePassword = !!mustChangePassword;
    try {
      wx.setStorageSync(STORAGE_KEYS.serverUrl, serverUrl);
      wx.setStorageSync(STORAGE_KEYS.token, token);
      wx.setStorageSync(STORAGE_KEYS.userId, userId);
      wx.setStorageSync(STORAGE_KEYS.username, username || '');
      wx.setStorageSync(STORAGE_KEYS.mustChangePassword, g.mustChangePassword);
    } catch (e) {}
  },

  /** 管理员密码修改完成：解除页面级拦截 */
  markPasswordChanged() {
    this.globalData.mustChangePassword = false;
    try {
      wx.setStorageSync(STORAGE_KEYS.mustChangePassword, false);
    } catch (e) {}
  },

  /** 退出登录：仅清令牌，保留服务器地址与用户名便于下次预填 */
  clearSession() {
    const g = this.globalData;
    g.token = '';
    g.userId = '';
    g.mustChangePassword = false;
    try {
      wx.removeStorageSync(STORAGE_KEYS.token);
      wx.removeStorageSync(STORAGE_KEYS.userId);
      wx.removeStorageSync(STORAGE_KEYS.mustChangePassword);
    } catch (e) {}
  },

  isLoggedIn() {
    return !!(this.globalData.token && this.globalData.serverUrl && !this.globalData.mustChangePassword);
  },
});
