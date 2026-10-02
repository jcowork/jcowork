/**
 * HTTP API 封装：统一处理服务器地址、鉴权头与错误信息。
 * 接口清单与 server 端 crates/jcowork-gateway/src/router 保持一致。
 */
const { normalizeServerUrl, httpBase, STORAGE_KEYS } = require('../config');

let redirectingToLogin = false;

function getAppSafe() {
  try {
    return getApp();
  } catch (e) {
    return null;
  }
}

/** 当前生效的服务器地址（优先内存态，兜底本地存储） */
function currentServerUrl() {
  const app = getAppSafe();
  if (app && app.globalData.serverUrl) return normalizeServerUrl(app.globalData.serverUrl);
  try {
    return normalizeServerUrl(wx.getStorageSync(STORAGE_KEYS.serverUrl) || '');
  } catch (e) {
    return '';
  }
}

function currentToken() {
  const app = getAppSafe();
  if (app && app.globalData.token) return app.globalData.token;
  try {
    return wx.getStorageSync(STORAGE_KEYS.token) || '';
  } catch (e) {
    return '';
  }
}

/** 令牌失效：清理登录态并回到登录页 */
function handleUnauthorized() {
  if (redirectingToLogin) return;
  redirectingToLogin = true;
  const app = getAppSafe();
  if (app && app.clearSession) app.clearSession();
  wx.showToast({ title: '登录已过期，请重新登录', icon: 'none' });
  setTimeout(() => {
    wx.reLaunch({
      url: '/pages/login/login',
      complete: () => {
        redirectingToLogin = false;
      },
    });
  }, 600);
}

/**
 * 发起请求。resolve 返回响应 JSON；失败时 reject(Error 带 message)。
 * @param {string} path 接口路径，如 /api/auth/login
 * @param {object} [opts] { method, data, auth, baseUrl, timeout }
 */
function request(path, opts = {}) {
  const { method = 'GET', data, auth = true, timeout = 60000 } = opts;
  const base = normalizeServerUrl(opts.baseUrl || currentServerUrl());
  return new Promise((resolve, reject) => {
    if (!base) {
      reject(new Error('请先填写服务器地址'));
      return;
    }
    const header = { 'Content-Type': 'application/json' };
    if (auth) {
      const token = currentToken();
      if (token) header.Authorization = `Bearer ${token}`;
    }
    wx.request({
      url: httpBase(base) + path,
      method,
      data,
      header,
      timeout,
      success: (res) => {
        const status = res.statusCode;
        if (status >= 200 && status < 300) {
          resolve(res.data);
          return;
        }
        if (status === 401 && auth) {
          handleUnauthorized();
        }
        const body = res.data;
        const msg =
          (body && (body.error || body.message)) ||
          (status === 401
            ? '用户名或密码错误'
            : status === 403
              ? '账号不可用（可能已被删除）'
              : status === 409
                ? '用户名已存在'
                : `请求失败（HTTP ${status}）`);
        reject(new Error(msg));
      },
      fail: (err) => {
        const raw = (err && err.errMsg) || '';
        let msg = '网络请求失败，请检查服务器地址与网络';
        if (/timeout/i.test(raw)) msg = '请求超时，请稍后重试';
        else if (/not in domain list|合法域名/i.test(raw)) {
          msg = '域名未加入小程序合法域名：开发时请在开发者工具中勾选「不校验合法域名」';
        }
        reject(new Error(msg));
      },
    });
  });
}

// ─── 认证 ────────────────────────────────────────────────

/** 登录：POST /api/auth/login → { token, user_id, username, is_admin } */
function login(serverUrl, username, password) {
  return request('/api/auth/login', {
    method: 'POST',
    data: { username, password },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

/** 注册：POST /api/auth/register → { token, user_id, username, is_public, is_admin } */
function register(serverUrl, username, password, isPublic) {
  return request('/api/auth/register', {
    method: 'POST',
    data: { username, password, is_public: !!isPublic },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

// ─── 其他 ────────────────────────────────────────────────

/** 可用 LLM provider 列表（仅已注册的活跃 provider）→ { providers:[{id,name,models}], default_model } */
function fetchProviders() {
  return request('/api/providers');
}

module.exports = {
  request,
  login,
  register,
  fetchProviders,
  currentServerUrl,
  currentToken,
  handleUnauthorized,
};
