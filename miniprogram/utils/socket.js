/**
 * WebSocket 管理器（单连接）。
 *
 * 连接地址与 Web 端一致：<ws|wss>://host/api/ws?token=<jwt>&conv=<会话ID>
 *
 * 消息协议（与 server 端 crates/jcowork-gateway/src/ws.rs 对齐）：
 *   发送：{ content, model, context_documents, images }
 *        { type: "load_history", history: [{role, content, kind}] }
 *        { type: "stop" }
 *   接收：text_delta / image_html / tool_call_start / tool_call_end /
 *        done / error / status / reminder / task_resume / stopped
 *
 * 断线自动重连：后台任务在服务端持续执行，重连后服务端会通过
 * task_resume 事件补发错过的输出。小程序切后台超过 5 秒系统会关闭
 * socket，回到前台由页面调用 ensureConnected() 恢复。
 */
const { wsBase } = require('../config');

const MAX_OUTBOX = 50;
const RECONNECT_BASE_DELAY = 3000;
const RECONNECT_MAX_DELAY = 30000;

const state = {
  task: null,
  url: '',
  params: null, // { serverUrl, token, convId }
  handlers: {},
  open: false,
  connecting: false,
  manualClosed: true,
  reconnectTimer: null,
  reconnectAttempts: 0,
  outbox: [],
};

function safeCall(name, ...args) {
  const fn = state.handlers && state.handlers[name];
  if (typeof fn === 'function') {
    try {
      fn(...args);
    } catch (e) {}
  }
}

function buildUrl({ serverUrl, token, convId }) {
  return (
    wsBase(serverUrl) +
    '/api/ws?token=' +
    encodeURIComponent(token) +
    '&conv=' +
    encodeURIComponent(convId || '')
  );
}

function clearReconnectTimer() {
  if (state.reconnectTimer) {
    clearTimeout(state.reconnectTimer);
    state.reconnectTimer = null;
  }
}

function scheduleReconnect() {
  if (state.manualClosed || !state.params || state.reconnectTimer) return;
  const delay = Math.min(
    RECONNECT_BASE_DELAY * Math.pow(1.5, state.reconnectAttempts),
    RECONNECT_MAX_DELAY
  );
  state.reconnectTimer = setTimeout(() => {
    state.reconnectTimer = null;
    if (state.manualClosed || !state.params) return;
    state.reconnectAttempts += 1;
    safeCall('onReconnecting', state.reconnectAttempts);
    open(state.params);
  }, delay);
}

function flushOutbox() {
  if (!state.open || !state.task) return;
  const pending = state.outbox;
  state.outbox = [];
  pending.forEach((payload) => {
    try {
      state.task.send({ data: payload });
    } catch (e) {}
  });
}

/** 建立连接（same url 且已连接/连接中时幂等返回） */
function open(params) {
  const url = buildUrl(params);
  if (state.task && state.url === url && (state.open || state.connecting)) return;
  // 备注：conv 改变会生成新 url，旧连接先关闭
  closeTaskSilently();

  state.params = params;
  state.url = url;
  state.manualClosed = false;
  state.open = false;
  state.connecting = true;

  const task = wx.connectSocket({ url });
  state.task = task;

  task.onOpen(() => {
    if (state.task !== task) return;
    state.open = true;
    state.connecting = false;
    state.reconnectAttempts = 0;
    flushOutbox();
    safeCall('onOpen');
  });

  task.onMessage((res) => {
    if (state.task !== task) return;
    let data = res && res.data;
    if (typeof data === 'string') {
      try {
        data = JSON.parse(data);
      } catch (e) {
        return;
      }
    }
    safeCall('onMessage', data);
  });

  task.onError((err) => {
    if (state.task !== task) return;
    state.connecting = false;
    safeCall('onSocketError', err);
  });

  task.onClose(() => {
    if (state.task !== task) return;
    state.open = false;
    state.connecting = false;
    safeCall('onClose');
    scheduleReconnect();
  });
}

/** 关闭当前连接（不触发自动重连） */
function closeTaskSilently() {
  if (state.task) {
    try {
      state.task.close({ code: 1000 });
    } catch (e) {}
  }
  state.task = null;
  state.open = false;
  state.connecting = false;
}

/**
 * 连接/切换到指定会话。
 * @param {object} params { serverUrl, token, convId }
 * @param {object} handlers { onOpen, onMessage, onClose, onReconnecting, onSocketError }
 */
function connect(params, handlers) {
  state.handlers = handlers || {};
  clearReconnectTimer();
  state.reconnectAttempts = 0;
  open(params);
}

/** 主动断开并停止重连（退出登录/页面销毁时调用） */
function close() {
  state.manualClosed = true;
  state.params = null;
  state.outbox = [];
  state.reconnectAttempts = 0;
  clearReconnectTimer();
  closeTaskSilently();
}

/** 页面回到前台时调用：未连接则立即恢复 */
function ensureConnected() {
  if (state.manualClosed || !state.params) return;
  if (state.open || state.connecting) return;
  clearReconnectTimer();
  open(state.params);
}

function isOpen() {
  return state.open;
}

/** 发送原始 JSON 对象；未连接时进入待发队列 */
function send(payload) {
  const json = JSON.stringify(payload);
  if (state.open && state.task) {
    try {
      state.task.send({ data: json });
      return true;
    } catch (e) {}
  }
  if (state.outbox.length < MAX_OUTBOX) state.outbox.push(json);
  return false;
}

/** 发送一条聊天消息 */
function sendChat({ content, model }) {
  const payload = { content };
  if (model) payload.model = model;
  return send(payload);
}

/** 恢复历史上下文：服务端据此继承话题（存最近消息即可） */
function loadHistory(history) {
  if (!history || !history.length) return;
  return send({ type: 'load_history', history });
}

/** 停止当前生成 */
function stop() {
  return send({ type: 'stop' });
}

module.exports = { connect, close, ensureConnected, isOpen, send, sendChat, loadHistory, stop };
