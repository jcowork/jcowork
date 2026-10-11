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
 * 管理员仍在使用初始密码：令牌仍然有效，但服务端只放行 /api/profile*。
 * 保留会话（改密接口需要它），只打上标记并把用户送回登录页。
 */
function handlePasswordChangeRequired() {
  if (redirectingToLogin) return;
  redirectingToLogin = true;
  const app = getAppSafe();
  if (app) {
    app.globalData.mustChangePassword = true;
    try {
      wx.setStorageSync(STORAGE_KEYS.mustChangePassword, true);
    } catch (e) {}
  }
  wx.showToast({ title: '请先修改管理员密码', icon: 'none' });
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
 * 服务端以英文描述（而非错误码）返回鉴权失败原因，这里统一转成中文提示。
 * 匹配顺序有意为之：更具体的串必须先判断，否则会命中包含它的短串。
 */
function localizeAuthError(body) {
  const raw = String((body && (body.error || body.message)) || '');
  if (!raw) return '';
  const retryAfter = body && body.retry_after;
  const attemptsLeft = body && body.attempts_left;
  if (raw.includes('Invalid phone number')) return '手机号格式不正确';
  if (raw.includes('Please wait before requesting another code')) {
    return retryAfter ? `请 ${retryAfter} 秒后再获取验证码` : '请稍后再获取验证码';
  }
  if (raw.includes('Daily limit reached for this phone number')) return '该手机号今日获取次数已达上限';
  if (raw.includes('SMS quota exhausted')) return '短信额度已用尽，请明天再试';
  if (raw.includes('Failed to send verification code')) return '验证码发送失败，请稍后重试';
  if (raw.includes('Invalid email address')) return '邮箱格式不正确';
  if (raw.includes('Invalid email or password')) return '邮箱或密码错误';
  if (raw.includes('Email address is already registered')) return '该邮箱已注册，请直接登录';
  if (raw.includes('Daily limit reached for this email address')) return '该邮箱今日获取验证码次数已达上限';
  if (raw.includes('Email quota exhausted')) return '邮件额度已用尽，请明天再试';
  if (raw.includes('Email registration is disabled')) return '邮箱注册已关闭';
  if (raw.includes('Password must be at least')) return '密码至少 8 位';
  if (raw.includes('Password must be at most')) return '密码不能超过 128 位';
  if (raw.includes('Password cannot be only whitespace')) return '密码不能为纯空白字符';
  if (raw.includes('Verification code is required')) return '请输入验证码';
  if (raw.includes('Verification code has expired')) return '验证码已过期，请重新获取';
  if (raw.includes('Invalid verification code')) {
    return attemptsLeft > 0 ? `验证码不正确，还可尝试 ${attemptsLeft} 次` : '验证码不正确';
  }
  if (raw.includes('Too many failed attempts')) {
    return retryAfter ? `尝试次数过多，请 ${retryAfter} 秒后再试` : '尝试次数过多，请稍后再试';
  }
  if (raw.includes('Phone number is not registered')) return '该手机号未注册';
  if (raw.includes('Account has been deleted')) return '该账号已被删除，无法登录';
  if (raw.includes('Invalid username or password')) return '用户名或密码错误';
  if (raw.includes('Change the admin password to continue')) return '请先修改管理员密码';
  if (raw.includes('already bound')) return '该手机号已被其他账号绑定';
  if (raw.includes('Username must be between') || raw.includes('Username cannot contain')) {
    return '显示名称需为 2-24 个字符，且不能包含空格与 @ / \\ 字符';
  }
  if (raw.includes('already exists')) return '该名称已被占用';
  if (raw.includes('Account not found')) return '账号不存在';
  return raw;
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
        if (status === 403 && body && body.code === 'password_change_required') {
          handlePasswordChangeRequired();
          reject(new Error(localizeAuthError(body)));
          return;
        }
        const msg =
          (body && (body.error || body.message) && localizeAuthError(body)) ||
          `请求失败（HTTP ${status}）`;
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

/** 发送短信验证码：POST /api/auth/sms/send → { expires_in, resend_after } */
function sendSmsCode(serverUrl, phone) {
  return request('/api/auth/sms/send', {
    method: 'POST',
    data: { phone },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

/**
 * 短信验证码登录：POST /api/auth/sms/login
 * → { token, user_id, username, is_admin, must_change_password }
 * 未注册的手机号验证通过后由服务端自动建号。
 */
function smsLogin(serverUrl, phone, code) {
  return request('/api/auth/sms/login', {
    method: 'POST',
    data: { phone, code },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

/**
 * 管理员密码登录：POST /api/auth/login → 同 smsLogin 的响应结构。
 * 管理员长期使用密码登录；首次登录会带上 must_change_password=true，
 * 要求先设置新密码再进入应用。
 */
function adminLogin(serverUrl, username, password) {
  return request('/api/auth/login', {
    method: 'POST',
    data: { username, password },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

/** 发送邮件验证码：POST /api/auth/email/send → { expires_in, resend_after }（注册与找回密码共用） */
function sendEmailCode(serverUrl, email) {
  return request('/api/auth/email/send', {
    method: 'POST',
    data: { email },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

/**
 * 邮箱注册：POST /api/auth/email/register → 同 smsLogin 的响应结构。
 * 邮件验证码确权 + 自选密码，成功即登录。
 */
function emailRegister(serverUrl, email, password, code) {
  return request('/api/auth/email/register', {
    method: 'POST',
    data: { email, password, code },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

/** 邮箱密码登录：POST /api/auth/email/login → 同 smsLogin 的响应结构 */
function emailLogin(serverUrl, email, password) {
  return request('/api/auth/email/login', {
    method: 'POST',
    data: { email, password },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

/** 找回密码：POST /api/auth/password/reset（邮件验证码 + 新密码，不返回令牌） */
function resetPassword(serverUrl, email, code, newPassword) {
  return request('/api/auth/password/reset', {
    method: 'POST',
    data: { email, code, new_password: newPassword },
    auth: false,
    baseUrl: serverUrl,
    timeout: 20000,
  });
}

// ─── 账号资料 ────────────────────────────────────────────

/** 当前账号资料：GET /api/profile → { username, phone_masked, phone_bound, email_masked, email_bound, is_admin, must_change_password } */
function fetchProfile() {
  return request('/api/profile');
}

/** 管理员修改密码（解除初始密码门禁）：POST /api/profile/password → 同 fetchProfile */
function changePassword(newPassword) {
  return request('/api/profile/password', { method: 'POST', data: { new_password: newPassword } });
}

// ─── 其他 ────────────────────────────────────────────────

/** 可用 LLM provider 列表（仅已注册的活跃 provider）→ { providers:[{id,name,models}], default_model } */
function fetchProviders() {
  return request('/api/providers');
}

module.exports = {
  request,
  sendSmsCode,
  smsLogin,
  adminLogin,
  sendEmailCode,
  emailRegister,
  emailLogin,
  resetPassword,
  fetchProfile,
  changePassword,
  fetchProviders,
  currentServerUrl,
  currentToken,
  handleUnauthorized,
};
