// 设置页：账号信息、模型选择、本机数据管理、退出登录。
const api = require('../../utils/api');
const store = require('../../utils/conversationStore');
const socket = require('../../utils/socket');

Page({
  data: {
    username: '',
    userId: '',
    serverUrl: '',
    modelLabels: ['默认模型（跟随服务器）'],
    modelValues: [''],
    modelIndex: 0,
    loadingModels: false,
    error: '',
  },

  onLoad() {
    const app = getApp();
    if (!app || !app.isLoggedIn()) {
      wx.reLaunch({ url: '/pages/login/login' });
      return;
    }
    this.userId = app.globalData.userId;
    this.setData({
      username: app.globalData.username || '',
      userId: app.globalData.userId || '',
      serverUrl: app.globalData.serverUrl || '',
    });
    this.loadModels();
  },

  /** 从服务器拉取可用 provider / 模型，构建选择器数据 */
  async loadModels() {
    this.setData({ loadingModels: true, error: '' });
    try {
      const data = await api.fetchProviders();
      const providers = (data && data.providers) || [];
      const labels = ['默认模型（跟随服务器）'];
      const values = [''];
      providers.forEach((p) => {
        const models = p.models && p.models.length ? p.models : [];
        models.forEach((m) => {
          labels.push((p.name || p.id) + ' · ' + m);
          values.push(p.id + ':' + m);
        });
      });
      const current = store.getModel(this.userId);
      let idx = values.indexOf(current);
      if (idx < 0) idx = 0;
      this.setData({
        modelLabels: labels,
        modelValues: values,
        modelIndex: idx,
        loadingModels: false,
      });
    } catch (err) {
      this.setData({
        loadingModels: false,
        error: '模型列表加载失败：' + ((err && err.message) || '未知错误'),
      });
    }
  },

  onModelChange(e) {
    const idx = Number(e.detail.value);
    const value = this.data.modelValues[idx] || '';
    store.setModel(this.userId, value);
    this.setData({ modelIndex: idx });
    wx.showToast({ title: '已切换模型', icon: 'none' });
  },

  /** 切换服务器或账号：清登录态回登录页（本机聊天记录保留） */
  goRelogin() {
    wx.showModal({
      title: '重新登录',
      content: '切换服务器或账号需要重新登录。本机聊天记录会保留，可在登录页修改服务器地址。',
      success: (res) => {
        if (!res.confirm) return;
        socket.close();
        getApp().clearSession();
        wx.reLaunch({ url: '/pages/login/login' });
      },
    });
  },

  clearChats() {
    wx.showModal({
      title: '清空本机聊天记录',
      content: '将删除本机保存的全部会话记录（不影响服务端数据）。确认清空？',
      confirmText: '清空',
      confirmColor: '#f85149',
      success: (res) => {
        if (!res.confirm) return;
        store.clearAll(this.userId);
        wx.showToast({ title: '已清空', icon: 'success' });
        setTimeout(() => {
          wx.reLaunch({ url: '/pages/chat/chat' });
        }, 400);
      },
    });
  },

  logout() {
    wx.showModal({
      title: '退出登录',
      content: '确认退出当前账号？',
      success: (res) => {
        if (!res.confirm) return;
        socket.close();
        getApp().clearSession();
        wx.reLaunch({ url: '/pages/login/login' });
      },
    });
  },
});
