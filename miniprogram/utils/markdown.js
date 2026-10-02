/**
 * 轻量 Markdown → rich-text 节点解析器。
 *
 * 支持：围栏代码块、标题、粗体 / 斜体 / 删除线、行内代码、链接、
 * 有序 / 无序列表、引用、分割线、表格（真实 table 节点）。
 * 输出 WeChat rich-text 可接受的 nodes 数组，纯结构生成无需转义。
 *
 * 说明：rich-text 内联样式不接受 rpx，统一使用 px。
 */

const MAX_RENDER_CHARS = 40000;

const S_BASE = 'font-size:15px;line-height:1.65;color:#e6edf3;word-break:break-word;overflow-wrap:break-word;';
const S_PARA = 'margin:0 0 6px;white-space:pre-wrap;' + S_BASE;
const S_H = (size) => `margin:12px 0 6px;font-weight:700;font-size:${size}px;color:#e6edf3;`;
const S_CODE_INLINE =
  'font-family:Menlo,Consolas,monospace;background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:1px 4px;font-size:13px;color:#e6edf3;';
const S_PRE =
  'font-family:Menlo,Consolas,monospace;background:#0d1117;border:1px solid #30363d;border-radius:8px;padding:10px;margin:6px 0;font-size:13px;line-height:1.55;white-space:pre-wrap;word-break:break-all;color:#c9d1d9;';
const S_QUOTE = 'border-left:3px solid #30363d;padding-left:10px;margin:6px 0;color:#9da7b3;';
const S_HR = 'border-top:1px solid #30363d;margin:10px 0;height:0;font-size:0;';
const S_LI = 'margin:0 0 4px;padding-left:16px;text-indent:-16px;white-space:pre-wrap;' + S_BASE;
const S_TABLE = 'border-collapse:collapse;margin:6px 0;font-size:13px;width:100%;table-layout:fixed;';
const S_TD = 'border:1px solid #30363d;padding:4px 6px;text-align:left;word-break:break-word;vertical-align:top;';
const S_LINK = 'color:#58a6ff;text-decoration:underline;word-break:break-all;';

function el(name, style, children) {
  const attrs = style ? { style } : {};
  return { name, attrs, children: children && children.length ? children : [{ type: 'text', text: '' }] };
}

function text(t) {
  return { type: 'text', text: t };
}

/**
 * 行内解析：`code`、**bold**、~~del~~、[text](url)、*italic*
 */
function parseInline(line) {
  const nodes = [];
  if (line === undefined || line === null || line === '') return nodes;
  const re = /(`[^`]+`|\*\*[^*]+\*\*|~~[^~]+~~|\[[^\]]+\]\([^)\s]+\)|\*[^*\n]+\*)/g;
  let last = 0;
  let m;
  while ((m = re.exec(line)) !== null) {
    if (m.index > last) nodes.push(text(line.slice(last, m.index)));
    const token = m[0];
    if (token[0] === '`') {
      nodes.push(el('span', S_CODE_INLINE, [text(token.slice(1, -1))]));
    } else if (token.startsWith('**')) {
      nodes.push(el('strong', 'font-weight:700;', parseInline(token.slice(2, -2))));
    } else if (token.startsWith('~~')) {
      nodes.push(el('del', 'text-decoration:line-through;', [text(token.slice(2, -2))]));
    } else if (token.startsWith('[')) {
      const mm = /^\[([^\]]+)\]\(([^)\s]+)\)$/.exec(token);
      if (mm) {
        nodes.push(el('span', S_LINK, [text(mm[1])]));
      } else {
        nodes.push(text(token));
      }
    } else if (token.startsWith('*')) {
      nodes.push(el('em', 'font-style:italic;', parseInline(token.slice(1, -1))));
    } else {
      nodes.push(text(token));
    }
    last = m.index + token.length;
  }
  if (last < line.length) nodes.push(text(line.slice(last)));
  return nodes;
}

/** 图片语法降级为文本标记 */
function demoteImages(line) {
  return line.replace(/!\[([^\]]*)\]\([^)]*\)/g, (s, alt) => '[图片' + (alt ? ': ' + alt : '') + ']');
}

function isHr(line) {
  return /^\s*(-{3,}|\*{3,}|_{3,})\s*$/.test(line);
}

function fenceMatch(line) {
  return /^\s*(```+|~~~+)/.exec(line);
}

/** 按未转义的分隔符切分（避免使用后行断言，兼容旧版 iOS JavaScriptCore） */
function splitUnescaped(s, delim) {
  const out = [];
  let cur = '';
  for (let k = 0; k < s.length; k += 1) {
    const ch = s[k];
    if (ch === '\\' && s[k + 1] === delim) {
      cur += delim;
      k += 1;
    } else if (ch === delim) {
      out.push(cur);
      cur = '';
    } else {
      cur += ch;
    }
  }
  out.push(cur);
  return out;
}

function tableRowCells(line) {
  const t = line.trim();
  if (!t.startsWith('|') && !t.endsWith('|')) return null;
  // 去掉首尾竖线后按未转义竖线切分
  const inner = t.replace(/^\|/, '').replace(/\|$/, '');
  if (!inner.includes('|')) return null;
  return splitUnescaped(inner, '|').map((c) => c.trim());
}

function isTableSeparator(cells) {
  return cells.every((c) => /^:?-{2,}:?$/.test(c));
}

/** 解析表格块：首行为表头（无分隔行时也按表头渲染） */
function parseTable(rows) {
  let header = null;
  let body = rows;
  if (rows.length >= 2 && isTableSeparator(rows[1])) {
    header = rows[0];
    body = rows.slice(2).filter((r) => !isTableSeparator(r));
  }
  const trs = [];
  const cellNode = (name, content) => el(name, S_TD, parseInline(content));
  if (header) {
    trs.push(el('tr', '', header.map((c) => cellNode('th', c))));
  }
  body.forEach((r) => trs.push(el('tr', '', r.map((c) => cellNode('td', c)))));
  if (!trs.length) return null;
  return el('table', S_TABLE, trs);
}

/**
 * 主入口：Markdown 文本 → rich-text nodes
 */
function parseMarkdown(md) {
  let src = String(md || '');
  if (src.length > MAX_RENDER_CHARS) {
    src = src.slice(0, MAX_RENDER_CHARS) + '\n\n…（内容过长，已截断显示）';
  }
  const lines = src.split('\n');
  const nodes = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    // 空行：跳过
    if (/^\s*$/.test(line)) {
      i += 1;
      continue;
    }

    // 围栏代码块（未闭合时视为延续到结尾 —— 流式输出过程中很常见）
    const fence = fenceMatch(line);
    if (fence) {
      const marker = fence[1][0];
      const codeLines = [];
      i += 1;
      while (i < lines.length) {
        const f2 = fenceMatch(lines[i]);
        if (f2 && f2[1][0] === marker) {
          i += 1;
          break;
        }
        codeLines.push(lines[i]);
        i += 1;
      }
      nodes.push(el('div', S_PRE, [text(codeLines.join('\n'))]));
      continue;
    }

    // 分割线
    if (isHr(line)) {
      nodes.push(el('div', S_HR, []));
      i += 1;
      continue;
    }

    // 标题
    const h = /^\s*(#{1,6})\s+(.*)$/.exec(line);
    if (h) {
      const level = h[1].length;
      const size = level === 1 ? 20 : level === 2 ? 18 : level === 3 ? 16.5 : 15.5;
      nodes.push(el('div', S_H(size), parseInline(demoteImages(h[2]))));
      i += 1;
      continue;
    }

    // 引用（连续行合并为一个块）
    if (/^\s*>\s?/.test(line)) {
      const quoteLines = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i])) {
        quoteLines.push(lines[i].replace(/^\s*>\s?/, ''));
        i += 1;
      }
      nodes.push(el('div', S_QUOTE, [el('div', 'white-space:pre-wrap;' + S_BASE, parseInline(demoteImages(quoteLines.join('\n'))))]));
      continue;
    }

    // 表格
    const cells = tableRowCells(line);
    if (cells) {
      const rows = [];
      while (i < lines.length) {
        const c = tableRowCells(lines[i]);
        if (!c) break;
        rows.push(c);
        i += 1;
      }
      const table = parseTable(rows);
      if (table) {
        nodes.push(table);
      } else {
        rows.forEach((r) => nodes.push(el('div', S_PARA, [text(r.join(' | '))])));
      }
      continue;
    }

    // 列表项
    const li = /^(\s*)([-*+]|\d{1,3}\.)\s+(.*)$/.exec(line);
    if (li) {
      const indent = Math.min(Math.floor(li[1].length / 2), 4) * 14;
      const marker = /^\d/.test(li[2]) ? li[2] + ' ' : '• ';
      nodes.push(
        el('div', `margin-left:${indent}px;` + S_LI, [text(marker)].concat(parseInline(demoteImages(li[3]))))
      );
      i += 1;
      continue;
    }

    // 普通段落：合并连续普通行，保留换行
    const paraLines = [];
    while (i < lines.length) {
      const l = lines[i];
      if (
        /^\s*$/.test(l) ||
        fenceMatch(l) ||
        isHr(l) ||
        /^\s*(#{1,6})\s+/.test(l) ||
        /^\s*>\s?/.test(l) ||
        /^(\s*)([-*+]|\d{1,3}\.)\s+/.test(l) ||
        tableRowCells(l)
      ) {
        break;
      }
      paraLines.push(l);
      i += 1;
    }
    if (paraLines.length) {
      nodes.push(el('div', S_PARA, parseInline(demoteImages(paraLines.join('\n')))));
    }
  }

  return nodes;
}

module.exports = { parseMarkdown };
