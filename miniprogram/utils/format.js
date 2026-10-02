/** 时间显示格式化工具 */

function pad2(n) {
  return n < 10 ? '0' + n : '' + n;
}

/** HH:mm */
function formatClock(ts) {
  const d = new Date(ts);
  return pad2(d.getHours()) + ':' + pad2(d.getMinutes());
}

/** 会话列表用的相对时间：刚刚 / n 分钟前 / 今天 HH:mm / 昨天 HH:mm / MM-DD / YYYY-MM-DD */
function formatRelativeTime(ts, now) {
  const t = typeof now === 'number' ? now : Date.now();
  const diff = t - ts;
  if (diff < 60 * 1000) return '刚刚';
  if (diff < 60 * 60 * 1000) return Math.floor(diff / 60000) + ' 分钟前';

  const d = new Date(ts);
  const today = new Date(t);
  const startOfToday = new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime();
  const startOfYesterday = startOfToday - 24 * 60 * 60 * 1000;
  const hm = formatClock(ts);
  if (ts >= startOfToday) return '今天 ' + hm;
  if (ts >= startOfYesterday) return '昨天 ' + hm;
  const md = pad2(d.getMonth() + 1) + '-' + pad2(d.getDate());
  if (d.getFullYear() === today.getFullYear()) return md;
  return d.getFullYear() + '-' + md;
}

module.exports = { formatClock, formatRelativeTime };
