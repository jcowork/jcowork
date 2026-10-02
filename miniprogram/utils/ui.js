/** 布局工具：自定义导航栏尺寸（适配刘海屏 / 胶囊按钮） */

function getWindowInfoSafe() {
  try {
    if (typeof wx.getWindowInfo === 'function') return wx.getWindowInfo();
  } catch (e) {}
  try {
    return wx.getSystemInfoSync();
  } catch (e) {}
  return { statusBarHeight: 20, windowWidth: 375 };
}

/**
 * 返回 { statusBarHeight, navBarHeight, navTotalHeight, menuRightGap }
 * navBarHeight 依据胶囊按钮高度计算，保证标题栏与胶囊垂直居中对齐；
 * menuRightGap 是导航栏右侧需要预留的避让宽度（胶囊按钮占位）。
 */
function getNavMetrics() {
  const win = getWindowInfoSafe();
  const statusBarHeight = win.statusBarHeight || 20;
  const windowWidth = win.windowWidth || 375;
  let navBarHeight = 44;
  let menuRightGap = 96;
  try {
    const rect = wx.getMenuButtonBoundingClientRect();
    if (rect && rect.height > 0) {
      navBarHeight = (rect.top - statusBarHeight) * 2 + rect.height;
      menuRightGap = Math.max(8, windowWidth - rect.left + 8);
    }
  } catch (e) {}
  return {
    statusBarHeight,
    navBarHeight,
    navTotalHeight: statusBarHeight + navBarHeight,
    menuRightGap,
  };
}

module.exports = { getNavMetrics };
