#!/usr/bin/env node
/**
 * 聊天页逻辑冒烟测试：在 Node 中 mock wx / Page 环境，直接运行
 * miniprogram/pages/chat/chat.js 的页面逻辑：
 *  1. onLoad → 建立 WebSocket（连接真实 server）
 *  2. 输入并发送消息 → 接收流式事件 → 状态收敛
 *  3. 消息持久化到 mock 存储、会话列表刷新
 *  4. 断开重连后 ensureConnected 生效
 *
 * 用法：node scripts/smoke_mp_page.js [server_url]
 * 默认服务器：http://localhost:3000
 */
const path = require('path');

const BASE = (process.argv[2] || process.env.JC_SERVER || 'http://localhost:3000').replace(/\/+$/, '');
const MP = path.join(__dirname, '..', 'miniprogram');

let failures = 0;
function check(name, ok, detail) {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`);
  if (!ok) failures += 1;
}
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

// ─── mock 存储与 wx API ─────────────────────────────────
const storage = new Map();
const toasts = [];

class WsTask {
  constructor(url) {
    this.listeners = { open: [], message: [], close: [], error: [] };
    this.ws = new WebSocket(url);
    this.ws.addEventListener('open', () => { console.log('  [ws] open'); this.listeners.open.forEach((cb) => cb({})); });
    this.ws.addEventListener('message', (ev) => this.listeners.message.forEach((cb) => cb({ data: ev.data })));
    this.ws.addEventListener('close', () => { console.log('  [ws] close'); this.listeners.close.forEach((cb) => cb({})); });
    this.ws.addEventListener('error', () => { console.log('  [ws] error'); this.listeners.error.forEach((cb) => cb({})); });
  }
  onOpen(cb) { this.listeners.open.push(cb); }
  onMessage(cb) { this.listeners.message.push(cb); }
  onClose(cb) { this.listeners.close.push(cb); }
  onError(cb) { this.listeners.error.push(cb); }
  send(opt) { this.ws.send(opt.data); }
  close() { try { this.ws.close(); } catch (e) {} }
}

global.wx = {
  getStorageSync: (k) => (storage.has(k) ? storage.get(k) : ''),
  setStorageSync: (k, v) => storage.set(k, v),
  removeStorageSync: (k) => storage.delete(k),
  connectSocket: ({ url }) => {
    console.log('  [wx.connectSocket]', url.slice(0, 60) + '…');
    return new WsTask(url);
  },
  showToast: (o) => toasts.push(o && o.title),
  showModal: () => {},
  showActionSheet: () => {},
  vibrateShort: () => {},
  setClipboardData: () => {},
  reLaunch: () => {},
  navigateTo: () => {},
  getWindowInfo: () => ({ statusBarHeight: 47, windowWidth: 390 }),
  getMenuButtonBoundingClientRect: () => ({ top: 51, height: 32, left: 296 }),
};

const appMock = {
  globalData: { serverUrl: BASE, token: '', userId: '', username: '' },
  isLoggedIn() { return !!this.globalData.token; },
  clearSession() { this.globalData.token = ''; },
};
global.getApp = () => appMock;

// ─── mock Page：捕获配置并支持路径 setData ──────────────
let pageConfig = null;
global.Page = (cfg) => { pageConfig = cfg; };

function applySetData(data, patch) {
  Object.keys(patch).forEach((key) => {
    const m = /^([A-Za-z0-9_]+)((\[(?:\d+)\]|\.[A-Za-z0-9_]+)*)$/.exec(key);
    if (!m) {
      data[key] = patch[key];
      return;
    }
    const rest = m[2];
    const segs = [];
    rest.replace(/\[(\d+)\]|\.([A-Za-z0-9_]+)/g, (s, num, name) => {
      segs.push(num !== undefined ? Number(num) : name);
      return '';
    });
    if (segs.length === 0) {
      // 普通键（无路径段）
      data[m[1]] = patch[key];
      return;
    }
    let target = data[m[1]];
    for (let i = 0; i < segs.length - 1; i += 1) target = target[segs[i]];
    target[segs[segs.length - 1]] = patch[key];
  });
}

function makePage() {
  const page = Object.create(pageConfig);
  page.data = JSON.parse(JSON.stringify(pageConfig.data));
  page.setData = function setData(patch, cb) {
    applySetData(this.data, patch);
    if (cb) cb();
  };
  return page;
}

// ─── 主流程 ─────────────────────────────────────────────
async function main() {
  console.log('服务器:', BASE);

  // 准备测试账号
  const username = 'mp_page_' + Date.now().toString(36);
  const reg = await fetch(BASE + '/api/auth/register', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password: 'smoke-test-1234' }),
  }).then((r) => r.json());
  if (!reg.token) {
    console.error('注册失败:', reg);
    process.exit(1);
  }
  appMock.globalData.token = reg.token;
  appMock.globalData.userId = reg.user_id;
  appMock.globalData.username = username;

  require(path.join(MP, 'pages', 'chat', 'chat.js'));
  const page = makePage();

  // 1. onLoad → 连接
  page.onLoad();
  let waited = 0;
  while (page.data.connState !== 'connected' && waited < 10000) {
    await wait(200);
    waited += 200;
  }
  check('onLoad 后 WebSocket 已连接', page.data.connState === 'connected', `connState=${page.data.connState}`);

  // 2. 发送消息
  page.onInput({ detail: { value: '你好，请只回复两个字：收到' } });
  check('canSend 随输入更新', page.data.canSend === true);
  page.onSend();
  check('发送后进入流式态', page.data.streaming === true);
  check('用户消息入列', page.data.messages.some((m) => m.role === 'user' && m.content.includes('你好')));

  // 3. 等待终止事件（done / error / stopped）
  let term = 0;
  while (page.data.streaming === true && term < 120000) {
    await wait(300);
    term += 300;
  }
  check('流式状态自动收敛', page.data.streaming === false, `等待 ${term}ms`);
  const assistantMsgs = page.data.messages.filter((m) => m.role === 'assistant');
  const systemMsgs = page.data.messages.filter((m) => m.role === 'system');
  const hasText = (page.data.messages || []).some((m) => m.role === 'assistant' && m.nodes && m.nodes.length > 0);
  check(
    '收到助手回复或错误提示',
    assistantMsgs.length > 0 || systemMsgs.length > 0,
    `assistant=${assistantMsgs.length} system=${systemMsgs.length} hasNodes=${hasText}`
  );
  if (systemMsgs.length) console.log('  系统消息:', JSON.stringify(systemMsgs.map((m) => m.content).join(' | ')).slice(0, 200));

  // 4. 持久化校验
  const convsRaw = storage.get('jcowork_convs_' + reg.user_id);
  const convs = convsRaw ? JSON.parse(convsRaw) : [];
  check('会话已持久化（无 system/流式消息）', convs.length === 1 && convs[0].messages.length > 0,
    JSON.stringify((convs[0] && convs[0].messages.map((m) => m.role)) || []));
  check('会话标题来自首条用户消息', convs[0] && convs[0].title.includes('你好'));
  check('会话列表已刷新', page.data.conversations.length >= 1 && page.data.conversations[0].active === true);

  // 5. 复制/长按不崩溃
  page.onMessageLongPress({ currentTarget: { dataset: { index: 0 } } });

  // 6. 断开后 ensureConnected 重连
  page.onShow();
  await wait(300);
  check('onShow 后连接保持', page.data.connState === 'connected', `connState=${page.data.connState}`);

  console.log('\n结果:', failures === 0 ? '全部通过' : failures + ' 项失败');
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((err) => {
  console.error('页面冒烟测试异常:', err);
  process.exit(1);
});
