/**
 * 聊天主页面。
 *
 * 与 Web 端 (web/src/components/Chat.tsx) 行为对齐：
 * - WebSocket 流式对话（text_delta / done / stopped / error ...）
 * - 多会话（drawer 抽屉切换、新建、长按删除）
 * - 断线自动重连；服务端后台任务通过 task_resume 重放补发
 * - 会话消息本地持久化（按用户隔离）
 */
const store = require('../../utils/conversationStore');
const socket = require('../../utils/socket');
const { parseMarkdown } = require('../../utils/markdown');
const { formatClock, formatRelativeTime } = require('../../utils/format');
const { getNavMetrics } = require('../../utils/ui');

const STATUS_CLEAR_MS = 2000;
const FLUSH_INTERVAL_MS = 100;
/** load_history 最多带回的历史消息条数 */
const MAX_HISTORY_MESSAGES = 100;

/** 原始消息 → 渲染项 */
function toRender(m) {
  const item = {
    role: m.role,
    content: m.content,
    time: formatClock(m.timestamp || Date.now()),
    kind: m.kind || '',
  };
  if (m.role === 'assistant' && m.kind !== 'image_html') {
    const suffix = m.streaming ? ' ▍' : '';
    item.nodes = parseMarkdown((m.content || '') + suffix);
  }
  return item;
}

function truncate(s, n) {
  const t = String(s || '');
  return t.length > n ? t.slice(0, n) + '…' : t;
}

Page({
  data: {
    statusBarHeight: 20,
    navBarHeight: 44,
    navTotalHeight: 64,
    menuRightGap: 96,
    messages: [],
    input: '',
    canSend: false,
    streaming: false,
    connState: 'connecting', // connected | connecting | offline
    statusLine: '',
    keyboardHeight: 0,
    scrollAnchor: 'anchor-a',
    drawerOpen: false,
    conversations: [],
    title: 'Jcowork',
    username: '',
  },

  onLoad() {
    const app = getApp();
    if (!app || !app.isLoggedIn()) {
      socket.close();
      wx.reLaunch({ url: '/pages/login/login' });
      return;
    }
    const metrics = getNavMetrics();
    this.userId = app.globalData.userId;
    this.statusMsg = '';
    this.messages = [];
    this._anchorB = false;
    this._flushTimer = null;
    this.setData({
      ...metrics,
      username: app.globalData.username || '',
    });
    this.initConversation();
  },

  onShow() {
    const app = getApp();
    if (!app || !app.isLoggedIn()) {
      socket.close();
      wx.reLaunch({ url: '/pages/login/login' });
      return;
    }
    if (app.globalData.userId !== this.userId) {
      // 账号已切换：重置本地状态
      this.userId = app.globalData.userId;
      this.setData({ username: app.globalData.username || '' });
      this.initConversation();
      return;
    }
    socket.ensureConnected();
  },

  onHide() {
    this.flushStreaming();
    this.persist(false);
  },

  onUnload() {
    if (this._flushTimer) {
      clearTimeout(this._flushTimer);
      this._flushTimer = null;
    }
  },

  // ─── 初始化与渲染 ──────────────────────────────────────

  initConversation() {
    let convId = store.getActiveConvId(this.userId);
    if (!convId) convId = store.createConversation(this.userId).id;
    this.convId = convId;
    this.loadConvMessages(convId);
    this.syncMessages();
    this.connectSocket();
    this.refreshConvList();
    this.refreshStatusLine();
    this.scrollToBottom();
  },

  loadConvMessages(convId) {
    const conv = store.loadConversations(this.userId).find((c) => c.id === convId);
    this.messages = conv ? conv.messages.map((m) => ({ ...m })) : [];
  },

  /** 全量同步渲染列表（结构变化时调用） */
  syncMessages() {
    const title = store.titleFromMessages(this.messages) || '';
    this.setData({
      messages: this.messages.map(toRender),
      title: title ? truncate(title, 14) : 'Jcowork',
    });
  },

  /** 追加一条消息（1:1 追加渲染项） */
  appendMessage(msg) {
    this.messages.push(msg);
    const idx = this.messages.length - 1;
    this.setData({ [`messages[${idx}]`]: toRender(msg) });
    if (msg.role === 'user') {
      const title = store.titleFromMessages(this.messages) || '';
      this.setData({ title: title ? truncate(title, 14) : 'Jcowork' });
    }
    this.scrollToBottom();
  },

  scrollToBottom() {
    this._anchorB = !this._anchorB;
    this.setData({ scrollAnchor: this._anchorB ? 'anchor-b' : 'anchor-a' });
  },

  refreshStatusLine() {
    let line = this.statusMsg || '';
    if (!line && this.data.streaming) line = '正在生成…';
    if (this.data.connState !== 'connected') {
      if (this.data.streaming) {
        line = '连接中断，服务端任务仍在运行，正在重连…';
      } else if (!line) {
        line = this.data.connState === 'connecting' ? '正在连接服务器…' : '连接已断开，正在重连…';
      }
    }
    this.setData({ statusLine: line });
  },

  // ─── WebSocket ─────────────────────────────────────────

  connectSocket() {
    const app = getApp();
    socket.connect(
      {
        serverUrl: app.globalData.serverUrl,
        token: app.globalData.token,
        convId: this.convId,
      },
      {
        onOpen: () => this.handleSocketOpen(),
        onMessage: (data) => this.handleSocketMessage(data),
        onReconnecting: () => {
          this.setData({ connState: 'connecting' });
          this.refreshStatusLine();
        },
        onClose: () => {
          this.setData({ connState: 'offline' });
          this.refreshStatusLine();
        },
        onSocketError: () => {},
      }
    );
  },

  handleSocketOpen() {
    this.setData({ connState: 'connected' });
    // 恢复上下文：把本地持久化的历史交给服务端，模型得以继承话题
    const hist = this.messages
      .filter((m) => (m.role === 'user' || m.role === 'assistant') && String(m.content || '').trim())
      .slice(-MAX_HISTORY_MESSAGES)
      .map((m) => ({ role: m.role, content: m.content, kind: m.kind }));
    if (hist.length > 0) socket.loadHistory(hist);
    this.refreshStatusLine();
  },

  handleSocketMessage(data) {
    if (!data || !data.type) return;
    switch (data.type) {
      case 'text_delta':
        this.onTextDelta(data.content || '');
        break;
      case 'image_html': {
        // 转换结果同时入上下文（与 Web 端一致，load_history 会带回）
        const content = '[图片] `' + (data.name || 'image') + '` 已转换为 HTML 内容\n\n```html\n' + data.html + '\n```';
        this.appendMessage({
          role: 'assistant',
          content,
          timestamp: Date.now(),
          kind: 'image_html',
        });
        this.persist(true);
        break;
      }
      case 'done':
        this.finalizeStreaming();
        this.setStatusMsg('');
        this.persist(true);
        break;
      case 'tool_call_start': {
        const preview = truncate(this.formatArgs(data.arguments), 200);
        this.pushSystem('▸ 调用工具：' + data.name + (preview ? '\n' + preview : ''));
        break;
      }
      case 'tool_call_end': {
        const result = typeof data.result === 'string' ? data.result : JSON.stringify(data.result || '');
        this.pushSystem('✓ ' + data.name + ' 完成' + (result ? '\n' + truncate(result, 300) : ''));
        break;
      }
      case 'task_resume': {
        // 服务端重新接上了断线前的后台任务：丢弃本地半截回答，等待重放
        this.setData({ streaming: true });
        const lastUser = this.lastUserIndex();
        if (lastUser >= 0) {
          const kept = this.messages.slice(lastUser + 1).filter((m) => m.kind === 'image_html');
          this.messages = this.messages.slice(0, lastUser + 1).concat(kept);
          this.syncMessages();
        }
        this.refreshStatusLine();
        break;
      }
      case 'reminder':
        this.pushSystem('提醒：' + data.message);
        try {
          wx.vibrateShort({ type: 'heavy' });
        } catch (e) {}
        break;
      case 'stopped':
        this.finalizeStreaming();
        this.setStatusMsg('已停止生成');
        setTimeout(() => {
          if (this.statusMsg === '已停止生成') this.setStatusMsg('');
        }, STATUS_CLEAR_MS);
        this.persist(true);
        break;
      case 'error':
        this.finalizeStreaming();
        this.setStatusMsg('');
        this.pushSystem('✕ ' + (data.message || '发生错误'));
        this.persist(true);
        break;
      case 'status':
        this.setStatusMsg(data.message || '');
        break;
      default:
        break;
    }
  },

  formatArgs(rawArguments) {
    if (!rawArguments) return '';
    try {
      const parsed = typeof rawArguments === 'string' ? JSON.parse(rawArguments) : rawArguments;
      return JSON.stringify(parsed, null, 2);
    } catch (e) {
      return String(rawArguments);
    }
  },

  lastUserIndex() {
    let idx = -1;
    this.messages.forEach((m, i) => {
      if (m.role === 'user') idx = i;
    });
    return idx;
  },

  // ─── 流式文本 ──────────────────────────────────────────

  onTextDelta(delta) {
    const last = this.messages[this.messages.length - 1];
    let target;
    if (last && last.role === 'assistant' && last.streaming) {
      last.content = (last.content || '') + delta;
      target = this.messages.length - 1;
    } else {
      this.messages.push({ role: 'assistant', content: delta, timestamp: Date.now(), streaming: true });
      target = this.messages.length - 1;
      this.setData({ [`messages[${target}]`]: toRender(this.messages[target]) });
    }
    if (!this.data.streaming) {
      this.setData({ streaming: true });
      this.refreshStatusLine();
    }
    this._streamIdx = target;
    this.scheduleFlush();
  },

  scheduleFlush() {
    if (this._flushTimer) return;
    this._flushTimer = setTimeout(() => {
      this._flushTimer = null;
      this.flushStreaming();
    }, FLUSH_INTERVAL_MS);
  },

  /** 把积累的流式文本刷进渲染层（节流，避免每个 delta 都 setData） */
  flushStreaming() {
    const idx = this._streamIdx;
    if (idx === undefined || idx < 0 || idx >= this.messages.length) return;
    const m = this.messages[idx];
    if (!m || m.role !== 'assistant') return;
    this.setData({
      [`messages[${idx}].content`]: m.content,
      [`messages[${idx}].nodes`]: parseMarkdown(m.content + (m.streaming ? ' ▍' : '')),
    });
    this.scrollToBottom();
  },

  /** 结束流式态（done / stopped / error） */
  finalizeStreaming() {
    if (this._flushTimer) {
      clearTimeout(this._flushTimer);
      this._flushTimer = null;
    }
    // 工具调用会插入 system 消息，导致“流式回答”不一定在数组末尾：
    // 清除所有 assistant 消息的流式标记，避免光标残留并保证可持久化。
    this.messages.forEach((m, idx) => {
      if (m.role === 'assistant' && m.streaming) {
        m.streaming = false;
        this.setData({ [`messages[${idx}]`]: toRender(m) });
      }
    });
    this._streamIdx = -1;
    this.setData({ streaming: false, canSend: !!(this.data.input && this.data.input.trim()) });
    this.refreshStatusLine();
  },

  pushSystem(content) {
    this.appendMessage({ role: 'system', content, timestamp: Date.now() });
  },

  setStatusMsg(msg) {
    this.statusMsg = msg;
    this.refreshStatusLine();
  },

  // ─── 持久化 ────────────────────────────────────────────

  /** 将当前会话消息写入本地存储（system / 流式中消息不保存） */
  persist(refreshList) {
    const clean = this.messages.filter((m) => !m.streaming && m.role !== 'system');
    store.updateConvMessages(this.userId, this.convId, clean);
    if (refreshList) this.refreshConvList();
  },

  refreshConvList() {
    const items = store.listConversations(this.userId).map((c) => ({
      id: c.id,
      title: c.title || '新对话',
      timeText: formatRelativeTime(c.lastInputAt || c.createdAt || Date.now()),
      active: c.id === this.convId,
    }));
    // 当前会话可能尚未持久化（空会话），仍需出现在列表里
    if (!items.some((it) => it.active)) {
      items.unshift({ id: this.convId, title: '新对话', timeText: '刚刚', active: true });
    }
    this.setData({ conversations: items });
  },

  // ─── 会话管理 ──────────────────────────────────────────

  openDrawer() {
    this.refreshConvList();
    this.setData({ drawerOpen: true });
  },

  closeDrawer() {
    this.setData({ drawerOpen: false });
  },

  onNewChat() {
    this.persist(true);
    if (this.messages.length > 0) {
      const { id } = store.createConversation(this.userId);
      this.switchToConv(id);
    }
    this.closeDrawer();
  },

  onSelectConv(e) {
    this.switchToConv(e.currentTarget.dataset.id);
    this.closeDrawer();
  },

  onConvLongPress(e) {
    const convId = e.currentTarget.dataset.id;
    wx.showActionSheet({
      itemList: ['删除该会话'],
      itemColor: '#f85149',
      success: (res) => {
        if (res.tapIndex === 0) this.deleteConv(convId);
      },
      fail: () => {},
    });
  },

  deleteConv(convId) {
    wx.showModal({
      title: '删除会话',
      content: '删除后本机记录不可恢复，服务端后台任务不受影响。确认删除？',
      confirmText: '删除',
      confirmColor: '#f85149',
      success: (res) => {
        if (!res.confirm) return;
        store.deleteConversation(this.userId, convId);
        if (convId === this.convId) {
          const { id } = store.createConversation(this.userId);
          this.switchToConv(id);
        }
        this.refreshConvList();
      },
    });
  },

  switchToConv(convId) {
    if (convId === this.convId) return;
    this.persist(false);
    this.flushStreaming();
    this.convId = convId;
    store.setActiveConvId(this.userId, convId);
    this.loadConvMessages(convId);
    this.statusMsg = '';
    this._streamIdx = -1;
    this.setData({ streaming: false, input: '', canSend: false, statusLine: '' });
    this.syncMessages();
    this.connectSocket();
    this.refreshConvList();
    this.scrollToBottom();
  },

  // ─── 输入与发送 ────────────────────────────────────────

  onInput(e) {
    const value = e.detail.value;
    this.setData({ input: value, canSend: !!value.trim() });
  },

  onKeyboardHeightChange(e) {
    const height = (e.detail && e.detail.height) || 0;
    this.setData({ keyboardHeight: height });
    if (height > 0) {
      setTimeout(() => this.scrollToBottom(), 150);
    }
  },

  onSendOrStop() {
    if (this.data.streaming) {
      socket.stop();
      return;
    }
    this.onSend();
  },

  onSend() {
    const text = String(this.data.input || '').trim();
    if (!text || this.data.streaming) return;
    if (!socket.isOpen()) {
      wx.showToast({ title: '连接中，请稍候再试', icon: 'none' });
      socket.ensureConnected();
      return;
    }

    const userMsg = { role: 'user', content: text, timestamp: Date.now() };
    this.appendMessage(userMsg);
    this.setData({ input: '', canSend: false });
    this.setData({ streaming: true });
    this.refreshStatusLine();
    this.persist(true);
    store.touchConversation(this.userId, this.convId);

    const model = store.getModel(this.userId);
    socket.sendChat({ content: text, model: model || undefined });
    this.scrollToBottom();
  },

  onMessageLongPress(e) {
    const index = e.currentTarget.dataset.index;
    const m = this.messages[index];
    if (!m || !m.content) return;
    if (m.kind === 'image_html') {
      wx.showToast({ title: '该消息为图片 HTML 内容', icon: 'none' });
      return;
    }
    wx.setClipboardData({ data: m.content });
  },

  // ─── 设置入口 ──────────────────────────────────────────

  goSettings() {
    this.closeDrawer();
    wx.navigateTo({ url: '/pages/settings/settings' });
  },
});
