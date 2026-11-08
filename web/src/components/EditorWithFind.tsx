import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent, ReactNode } from 'react';
import { useT } from '../i18n';

/** Cap on reported matches so a short query can't stall on huge documents. */
const MAX_MATCHES = 1000;

const MONO_FONT = "'SF Mono', 'Fira Code', 'Consolas', monospace";
const PAD = 16;
const FONT_SIZE = 13;
const LINE_HEIGHT = 1.6;

/** Case-insensitive indexOf scan returning the offset of every match. */
function findMatches(query: string, text: string): number[] {
  const out: number[] = [];
  if (!query) return out;
  const hay = text.toLowerCase();
  const needle = query.toLowerCase();
  let i = hay.indexOf(needle);
  while (i !== -1 && out.length < MAX_MATCHES) {
    out.push(i);
    i = hay.indexOf(needle, i + needle.length);
  }
  return out;
}

/**
 * Pixel offset of the end of `prefix` inside a textarea, measured with an
 * offscreen mirror div styled identically. Soft-wrapped text cannot be
 * located from a character count alone, so the mirror re-flows the prefix.
 */
function measureTextTop(ta: HTMLTextAreaElement, prefix: string): number {
  const mirror = document.createElement('div');
  const s = mirror.style;
  s.position = 'absolute';
  s.left = '-9999px';
  s.top = '0';
  s.visibility = 'hidden';
  s.pointerEvents = 'none';
  s.boxSizing = 'border-box';
  s.width = `${ta.clientWidth}px`;
  s.border = 'none';
  s.padding = `${PAD}px`;
  s.fontFamily = MONO_FONT;
  s.fontSize = `${FONT_SIZE}px`;
  s.lineHeight = String(LINE_HEIGHT);
  s.whiteSpace = 'pre-wrap';
  s.overflowWrap = 'break-word';
  s.tabSize = '2';
  mirror.textContent = prefix;
  document.body.appendChild(mirror);
  const y = mirror.getBoundingClientRect().height;
  document.body.removeChild(mirror);
  return y;
}

interface EditorWithFindProps {
  value: string;
  onChange: (value: string) => void;
  textareaRef: { current: HTMLTextAreaElement | null };
  /** Extra key handling (e.g. Cmd-S), run before the find bar's shortcuts. */
  onKeyDown?: (e: ReactKeyboardEvent<HTMLTextAreaElement>) => void;
  /** Sizing for the outer flex column (height: 100% in a block parent). */
  containerStyle?: CSSProperties;
}

/**
 * Textarea document editor with a keyword find bar and match highlighting.
 *
 * The textarea itself cannot paint coloured match backgrounds, so a backdrop
 * layer rendered with identical typography sits behind it: the editor text is
 * transparent there and every match is wrapped in a <mark>. The layer mirrors
 * the textarea's scroll offset and client box so wrapped lines stay aligned.
 * Keyboard focus always stays in the find input while searching — focusing
 * the textarea would send the next typed characters into the document.
 */
export function EditorWithFind({
  value,
  onChange,
  textareaRef,
  onKeyDown,
  containerStyle,
}: EditorWithFindProps) {
  const t = useT();
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const [box, setBox] = useState<{ w: number; h: number }>({ w: 0, h: 0 });
  const inputRef = useRef<HTMLInputElement>(null);
  const backdropRef = useRef<HTMLDivElement>(null);

  // Derived, so highlight offsets stay correct while the text is edited.
  const matches = useMemo(() => (query ? findMatches(query, value) : []), [query, value]);
  // Clamp after edits remove matches above the active one.
  const activeIdx = matches.length ? Math.min(active, matches.length - 1) : 0;

  const syncScroll = useCallback(() => {
    const ta = textareaRef.current;
    const bd = backdropRef.current;
    if (!ta || !bd) return;
    bd.scrollTop = ta.scrollTop;
    bd.scrollLeft = ta.scrollLeft;
  }, [textareaRef]);

  const jumpTo = useCallback((index: number, length: number) => {
    const ta = textareaRef.current;
    if (!ta) return;
    // Select the match (helps a follow-up click land the caret), then scroll
    // it into view.
    ta.setSelectionRange(index, index + length);
    const y = measureTextTop(ta, ta.value.slice(0, index));
    // Land the match in the upper third of the viewport, not flush at the edge.
    ta.scrollTop = Math.max(0, y - ta.clientHeight / 3);
    syncScroll();
  }, [textareaRef, syncScroll]);

  const runSearch = useCallback((q: string) => {
    setQuery(q);
    setActive(0);
    if (!q) return;
    const ta = textareaRef.current;
    const m = findMatches(q, ta ? ta.value : '');
    if (m.length) jumpTo(m[0], q.length);
  }, [textareaRef, jumpTo]);

  const step = useCallback((dir: 1 | -1) => {
    if (!matches.length) return;
    const next = (activeIdx + dir + matches.length) % matches.length;
    setActive(next);
    jumpTo(matches[next], query.length);
  }, [matches, activeIdx, query, jumpTo]);

  // Route the platform Find shortcut to the in-editor bar.
  useEffect(() => {
    const onDocKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'f') {
        e.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
    };
    document.addEventListener('keydown', onDocKeyDown);
    return () => document.removeEventListener('keydown', onDocKeyDown);
  }, []);

  // Keep the backdrop exactly as large as the textarea's client box; the
  // scrollbar appearing/disappearing changes the usable width.
  useLayoutEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    const update = () => setBox({ w: ta.clientWidth, h: ta.clientHeight });
    update();
    const ro = new ResizeObserver(update);
    ro.observe(ta);
    return () => ro.disconnect();
  }, [textareaRef]);

  // Re-sync after content/box changes (line wraps shift the text).
  useLayoutEffect(() => {
    syncScroll();
  }, [box, value, matches, syncScroll]);

  const segments = useMemo(() => {
    if (!query || !matches.length) return null;
    const nodes: ReactNode[] = [];
    let last = 0;
    matches.forEach((idx, i) => {
      if (idx > last) nodes.push(value.slice(last, idx));
      nodes.push(
        <mark
          key={`${idx}-${i}`}
          style={{
            background: i === activeIdx ? 'rgba(240,136,62,0.85)' : 'rgba(210,153,34,0.40)',
            color: 'transparent',
            borderRadius: 2,
          }}
        >
          {value.slice(idx, idx + query.length)}
        </mark>,
      );
      last = idx + query.length;
    });
    nodes.push(value.slice(last));
    return nodes;
  }, [value, matches, activeIdx, query]);

  const navStyle = (disabled: boolean) => ({
    padding: '2px 8px', borderRadius: 4, border: '1px solid #444',
    background: 'transparent', color: disabled ? '#555' : '#aaa',
    cursor: disabled ? ('not-allowed' as const) : ('pointer' as const),
    fontSize: 11, lineHeight: '16px',
  });

  return (
    <div style={{ display: 'flex', flexDirection: 'column', minHeight: 0, ...containerStyle }}>
      <div style={{
        display: 'flex', alignItems: 'center', gap: 6, flexShrink: 0,
        padding: '6px 12px', borderBottom: '1px solid #333', background: '#161b22',
      }}>
        <span style={{ fontSize: 12, color: '#8b949e', flexShrink: 0 }}>🔍</span>
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => runSearch(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              step(e.shiftKey ? -1 : 1);
            } else if (e.key === 'Escape') {
              inputRef.current?.blur();
            }
          }}
          placeholder={t('findInDoc')}
          style={{
            flex: 1, maxWidth: 260, padding: '3px 8px', borderRadius: 4,
            border: '1px solid #444', background: '#0d1117', color: '#c9d1d9',
            fontSize: 12, outline: 'none',
          }}
        />
        <span style={{
          fontSize: 11, minWidth: 52, textAlign: 'center', flexShrink: 0,
          color: !query ? '#666' : matches.length ? '#8b949e' : '#e57373',
        }}>
          {query ? (matches.length ? `${activeIdx + 1}/${matches.length}` : t('findNoResult')) : ''}
        </span>
        <button onClick={() => step(-1)} disabled={!matches.length} title={t('prevMatch')} style={navStyle(!matches.length)}>
          ▲
        </button>
        <button onClick={() => step(1)} disabled={!matches.length} title={t('nextMatch')} style={navStyle(!matches.length)}>
          ▼
        </button>
        <button onClick={() => runSearch('')} disabled={!query} title={t('close')} style={navStyle(!query)}>
          ✕
        </button>
      </div>

      <div style={{ position: 'relative', flex: 1, minHeight: 0, background: '#0d1117' }}>
        {/* Highlight layer: same typography as the textarea, text invisible. */}
        <div
          ref={backdropRef}
          aria-hidden
          style={{
            position: 'absolute', top: 0, left: 0,
            width: box.w || '100%', height: box.h || '100%',
            overflow: 'hidden', pointerEvents: 'none', boxSizing: 'border-box',
            padding: PAD,
            fontFamily: MONO_FONT, fontSize: FONT_SIZE, lineHeight: LINE_HEIGHT,
            whiteSpace: 'pre-wrap', overflowWrap: 'break-word', wordBreak: 'normal',
            tabSize: 2, color: 'transparent',
          }}
        >
          {segments ?? value}
        </div>
        <textarea
          ref={textareaRef}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => onKeyDown?.(e)}
          onScroll={syncScroll}
          spellCheck={false}
          style={{
            position: 'absolute', inset: 0, width: '100%', height: '100%',
            margin: 0, padding: PAD, border: 'none', outline: 'none', resize: 'none',
            fontSize: FONT_SIZE, lineHeight: LINE_HEIGHT, color: '#c9d1d9',
            background: 'transparent',
            fontFamily: MONO_FONT, tabSize: 2,
          }}
        />
      </div>
    </div>
  );
}
