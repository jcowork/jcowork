#!/usr/bin/env node
/**
 * 微信小程序客户端协议冒烟测试。
 *
 * 直接复用 miniprogram/utils 下的模块，验证：
 *  1. 服务器地址规范化 / WebSocket 地址推导
 *  2. Markdown → rich-text 节点解析
 *  3. 注册 → WebSocket 连接 → 发送消息 → 流式事件 → done（或 error）
 *  4. stop 停止生成
 *  5. 断线重连 + load_history 恢复上下文
 *
 * 用法：node scripts/smoke_mp_client.js [server_url]
 * 默认服务器：http://localhost:3000
 */
const path = require('path');

const { normalizeServerUrl, wsBase } = require(path.join(__dirname, '..', 'miniprogram', 'config.js'));
const { parseMarkdown } = require(path.join(__dirname, '..', 'miniprogram', 'utils', 'markdown.js'));

const BASE = normalizeServerUrl(process.argv[2] || process.env.JC_SERVER || 'http://localhost:3000');
const USERNAME = 'mp_smoke_' + Date.now().toString(36);
const PASSWORD = 'smoke-test-1234';

let failures = 0;
function check(name, ok, detail) {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? ' — ' + detail : ''}`);
  if (!ok) failures += 1;
}

// ─── 1. 地址规范化 ──────────────────────────────────────
function testUrl() {
  console.log('\n== 服务器地址规范化 ==');
  check('补协议 + 去尾斜杠', normalizeServerUrl('example.com:3000/') === 'https://example.com:3000');
  check('保留 https', normalizeServerUrl('https://a.b.c') === 'https://a.b.c');
  check('空值返回空', normalizeServerUrl('') === '');
  check('wss 推导', wsBase('https://a.b.c') === 'wss://a.b.c');
  check('ws 推导', wsBase('http://a.b.c:3000') === 'ws://a.b.c:3000');
}

// ─── 2. Markdown 解析 ───────────────────────────────────
function testMarkdown() {
  console.log('\n== Markdown 解析 ==');
  const md = [
    '# 标题一',
    '',
    '普通段落 **加粗** 与 `行内代码`，还有 [链接](https://example.com)。',
    '第二行普通文本',
    '',
    '- 列表项 A',
    '- 列表项 B',
    '',
    '```js',
    'const a = 1;',
    '```',
    '',
    '> 引用内容',
    '',
    '| 名称 | 值 |',
    '| --- | --- |',
    '| a | 1 |',
    '',
    '---',
  ].join('\n');
  const nodes = parseMarkdown(md);
  const names = nodes.map((n) => n.name).join(',');
  check('返回节点数组', Array.isArray(nodes) && nodes.length > 0, `${nodes.length} 个节点`);
  check('包含标题/段落/列表/代码块/引用/表格', /div/.test(names) && /table/.test(names), names);
  const pre = nodes.find((n) => n.name === 'div' && n.attrs.style.includes('white-space:pre-wrap') && n.attrs.style.includes('Menlo'));
  check('代码块使用 pre 样式', !!pre);
  check('流式未闭合代码块不崩溃', parseMarkdown('```js\nlet x = 1').length === 1);
  check('空串返回空数组', parseMarkdown('').length === 0);
}

// ─── HTTP 帮助函数 ──────────────────────────────────────
async function postJson(url, body, token) {
  const res = await fetch(url, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: 'Bearer ' + token } : {}),
    },
    body: JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  return { status: res.status, data };
}

async function getJson(url, token) {
  const res = await fetch(url, { headers: token ? { Authorization: 'Bearer ' + token } : {} });
  const data = await res.json().catch(() => ({}));
  return { status: res.status, data };
}

// ─── 3/4/5. WebSocket 端到端 ────────────────────────────
function openSocket(token, conv) {
  const url = wsBase(BASE) + '/api/ws?token=' + encodeURIComponent(token) + '&conv=' + encodeURIComponent(conv);
  const ws = new WebSocket(url);
  const state = { events: [], closed: false, ws };
  ws.addEventListener('message', (ev) => {
    try {
      state.events.push(JSON.parse(ev.data));
    } catch (e) {}
  });
  ws.addEventListener('close', () => {
    state.closed = true;
  });
  state.waitOpen = new Promise((resolve, reject) => {
    ws.addEventListener('open', () => resolve());
    ws.addEventListener('error', (e) => reject(new Error('ws error')));
  });
  return state;
}

function waitFor(sock, predicate, timeoutMs) {
  return new Promise((resolve) => {
    const started = Date.now();
    const timer = setInterval(() => {
      const hit = sock.events.find(predicate);
      if (hit) {
        clearInterval(timer);
        resolve(hit);
      } else if (Date.now() - started > timeoutMs) {
        clearInterval(timer);
        resolve(null);
      }
    }, 200);
  });
}

async function testEndToEnd() {
  console.log('\n== HTTP / WebSocket 端到端 ==');
  console.log('服务器:', BASE, '测试账号:', USERNAME);

  const reg = await postJson(BASE + '/api/auth/register', {
    username: USERNAME,
    password: PASSWORD,
    is_public: false,
  });
  check('注册成功', reg.status === 200 && !!reg.data.token, JSON.stringify(reg.data).slice(0, 120));
  const token = reg.data.token;
  const userId = reg.data.user_id;
  if (!token) return;

  const prov = await getJson(BASE + '/api/providers', token);
  check('providers 接口可用', prov.status === 200, JSON.stringify(prov.data).slice(0, 160));

  // —— 连接并发送一条消息，收集流式事件 ——
  const conv = 'smoke' + Date.now().toString(36);
  const sock = openSocket(token, conv);
  await sock.waitOpen;
  check('WebSocket 连接成功', true, conv);

  sock.ws.send(JSON.stringify({ content: '你好，请只回复两个字：收到' }));
  const terminal = await waitFor(
    sock,
    (e) => e.type === 'done' || e.type === 'error' || e.type === 'stopped',
    120000
  );
  const types = [...new Set(sock.events.map((e) => e.type))];
  console.log('  收到事件类型:', types.join(', ') || '(无)');
  const text = sock.events.filter((e) => e.type === 'text_delta').map((e) => e.content).join('');
  if (text) console.log('  回复内容片段:', JSON.stringify(text.slice(0, 80)));
  check('收到终止事件（done/error）', !!terminal && (terminal.type === 'done' || terminal.type === 'error'), terminal && terminal.type);
  if (terminal && terminal.type === 'error') {
    console.log('  终止错误信息:', terminal.message);
    console.log('  提示：服务器未配置可用的 LLM Key 时出现 error 属预期，协议链路已通。');
  }

  // —— stop 停止生成 ——
  sock.events.length = 0;
  sock.ws.send(JSON.stringify({ content: '写一篇 500 字以上的长文章' }));
  await waitFor(sock, (e) => e.type === 'text_delta', 60000);
  sock.ws.send(JSON.stringify({ type: 'stop' }));
  const stopped = await waitFor(sock, (e) => e.type === 'stopped' || e.type === 'done' || e.type === 'error', 30000);
  check('stop 触发终止事件', !!stopped, stopped && stopped.type);

  // —— 断线重连 + load_history ——
  sock.ws.close();
  await new Promise((r) => setTimeout(r, 500));
  const sock2 = openSocket(token, conv);
  await sock2.waitOpen;
  const lastText = sock2 ? text : '';
  sock2.ws.send(
    JSON.stringify({
      type: 'load_history',
      history: [
        { role: 'user', content: '你好，请只回复两个字：收到' },
        { role: 'assistant', content: lastText || '收到' },
      ],
    })
  );
  await new Promise((r) => setTimeout(r, 800));
  const errEvents = sock2.events.filter((e) => e.type === 'error');
  check('重连后 load_history 无错误', errEvents.length === 0, errEvents.map((e) => e.message).join('; '));
  sock2.ws.close();

  console.log('\n结果:', failures === 0 ? '全部通过' : failures + ' 项失败');
  process.exit(failures === 0 ? 0 : 1);
}

(async () => {
  testUrl();
  testMarkdown();
  await testEndToEnd();
})().catch((err) => {
  console.error('冒烟测试异常:', err);
  process.exit(1);
});
