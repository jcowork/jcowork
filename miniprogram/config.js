/**
 * 小程序运行时配置。
 *
 * DEFAULT_SERVER_URL 是登录页预填的默认服务器地址（官方 server）。
 * 如果你部署了自己的 Jcowork server，可在登录页修改地址，修改后
 * 会持久化到本地存储，下次启动自动使用。
 */

// 官方服务器地址（部署到正式服务器后请替换为实际域名）
const DEFAULT_SERVER_URL = 'https://api.jcowork.com';

// 本地存储键
const STORAGE_KEYS = {
  serverUrl: 'jcowork_server_url',
  token: 'jcowork_token',
  userId: 'jcowork_user_id',
  username: 'jcowork_username',
  model: (userId) => `jcowork_model_${userId}`,
};

/**
 * 规范化服务器地址：缺省协议时补 https://，去掉末尾斜杠。
 * 返回空字符串表示无法解析。
 */
function normalizeServerUrl(raw) {
  let url = String(raw || '').trim();
  if (!url) return '';
  if (!/^https?:\/\//i.test(url)) url = 'https://' + url;
  url = url.replace(/\/+$/, '');
  // 必须是合法的 http(s) 地址
  if (!/^https?:\/\/[^\s/]+/i.test(url)) return '';
  return url;
}

/** HTTP(S) 基地址，如 https://host:3000 */
function httpBase(raw) {
  return normalizeServerUrl(raw);
}

/** WebSocket 基地址：http→ws，https→wss */
function wsBase(raw) {
  return normalizeServerUrl(raw).replace(/^http/i, 'ws');
}

module.exports = {
  DEFAULT_SERVER_URL,
  STORAGE_KEYS,
  normalizeServerUrl,
  httpBase,
  wsBase,
};
