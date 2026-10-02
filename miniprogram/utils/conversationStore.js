/**
 * 多会话本地存储（对应 Web 端 web/src/chatStore.ts 的小程序版本）。
 *
 * 每个会话保存消息列表与 lastInputAt（最后一次用户输入时间）。
 * 数据按用户隔离存储：jcowork_convs_<userId>。
 */
const { STORAGE_KEYS } = require('../config');

/** 单个会话最多持久化的消息条数（微信本地存储单 key 上限 1MB） */
const MAX_PERSISTED_MESSAGES = 500;

const CONVS_KEY = (userId) => `jcowork_convs_${userId}`;
const ACTIVE_KEY = (userId) => `jcowork_active_conv_${userId}`;

/** 会话 ID：与 Web 端同款生成规则 */
function newConversationId() {
  return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
}

/** 标题 = 第一条用户消息（截断 40 字） */
function titleFromMessages(messages) {
  const first = (messages || []).find((m) => m.role === 'user');
  if (!first) return '';
  const t = String(first.content || '').trim().replace(/\s+/g, ' ');
  return t.length > 40 ? t.slice(0, 40) + '…' : t;
}

/**
 * 持久化前的清理：与 Web 端一致，不保存 system 消息与仍在流式输出的
 * 消息（后者在重连时由服务端 task_resume 重放，避免半截回答被当作
 * 最终回复而错过补发）。
 */
function cleanMessages(messages) {
  const cleaned = (messages || [])
    .filter((m) => !m.streaming && m.role !== 'system')
    .map((m) => ({ role: m.role, content: m.content, timestamp: m.timestamp, kind: m.kind }));
  return cleaned.length > MAX_PERSISTED_MESSAGES
    ? cleaned.slice(cleaned.length - MAX_PERSISTED_MESSAGES)
    : cleaned;
}

function loadConversations(userId) {
  try {
    const saved = wx.getStorageSync(CONVS_KEY(userId));
    if (saved) {
      const parsed = typeof saved === 'string' ? JSON.parse(saved) : saved;
      if (Array.isArray(parsed)) return parsed;
    }
  } catch (e) {}
  return [];
}

function saveConversations(userId, convs) {
  try {
    // 不持久化空会话
    const toSave = (convs || [])
      .filter((c) => c.messages && c.messages.length > 0)
      .map((c) => ({ ...c, messages: cleanMessages(c.messages) }));
    wx.setStorageSync(CONVS_KEY(userId), JSON.stringify(toSave));
  } catch (e) {}
}

function getActiveConvId(userId) {
  try {
    return wx.getStorageSync(ACTIVE_KEY(userId)) || null;
  } catch (e) {
    return null;
  }
}

function setActiveConvId(userId, id) {
  try {
    wx.setStorageSync(ACTIVE_KEY(userId), id);
  } catch (e) {}
}

/** 新建会话并设为当前会话 */
function createConversation(userId) {
  const convs = loadConversations(userId);
  const conv = {
    id: newConversationId(),
    title: '',
    messages: [],
    createdAt: Date.now(),
    lastInputAt: Date.now(),
  };
  saveConversations(userId, [...convs, conv]);
  setActiveConvId(userId, conv.id);
  return { convs: [...convs, conv], id: conv.id };
}

/** 覆盖某个会话的消息；根据首条用户消息重算标题（不存在时自动补插） */
function updateConvMessages(userId, convId, messages) {
  const loaded = loadConversations(userId);
  const exists = loaded.some((c) => c.id === convId);
  const base = exists
    ? loaded
    : [
        ...loaded,
        { id: convId, title: '', messages: [], createdAt: Date.now(), lastInputAt: Date.now() },
      ];
  const convs = base.map((c) =>
    c.id === convId
      ? {
          ...c,
          messages,
          title: messages.some((m) => m.role === 'user') ? titleFromMessages(messages) : '',
        }
      : c
  );
  saveConversations(userId, convs);
  return convs;
}

/** 记录一次用户输入：更新 lastInputAt */
function touchConversation(userId, convId) {
  const convs = loadConversations(userId).map((c) =>
    c.id === convId ? { ...c, lastInputAt: Date.now() } : c
  );
  saveConversations(userId, convs);
  return convs;
}

function deleteConversation(userId, convId) {
  const convs = loadConversations(userId).filter((c) => c.id !== convId);
  saveConversations(userId, convs);
  return convs;
}

/** 会话列表：按最后输入时间倒序 */
function listConversations(userId) {
  return loadConversations(userId).sort((a, b) => (b.lastInputAt || 0) - (a.lastInputAt || 0));
}

/** 清空该账号在本机的全部聊天记录 */
function clearAll(userId) {
  try {
    wx.removeStorageSync(CONVS_KEY(userId));
    wx.removeStorageSync(ACTIVE_KEY(userId));
  } catch (e) {}
}

// ─── 模型选择 ────────────────────────────────────────────

/** 读取当前账号选择的模型（"provider:model" 或空串=跟随服务器默认） */
function getModel(userId) {
  try {
    return wx.getStorageSync(STORAGE_KEYS.model(userId)) || '';
  } catch (e) {
    return '';
  }
}

function setModel(userId, value) {
  try {
    wx.setStorageSync(STORAGE_KEYS.model(userId), value || '');
  } catch (e) {}
}

module.exports = {
  newConversationId,
  titleFromMessages,
  loadConversations,
  listConversations,
  saveConversations,
  getActiveConvId,
  setActiveConvId,
  createConversation,
  updateConvMessages,
  touchConversation,
  deleteConversation,
  clearAll,
  getModel,
  setModel,
};
