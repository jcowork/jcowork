/** Shared standalone-document URL helpers.
 *
 * The mobile feed's document details link out to a bare `#/doc?...` viewer
 * (DocViewer.tsx) that renders without the app shell, so a URL can be opened
 * or shared independently of any login state.
 */

export interface DocViewerParams {
  userId: string;
  username: string;
  path: string;
  /** True when the document is public — the content endpoint needs no login. */
  pub: boolean;
}

/**
 * Parse the standalone-document hash route: `#/doc?u=..&n=..&p=..&pub=1`.
 * Returns null for any other URL, meaning the main app should render.
 */
export function parseDocHash(): DocViewerParams | null {
  const hash = window.location.hash;
  if (!hash.startsWith('#/doc')) return null;
  const qIndex = hash.indexOf('?');
  if (qIndex === -1) return null;
  const q = new URLSearchParams(hash.slice(qIndex + 1));
  const userId = q.get('u') || '';
  const path = q.get('p') || '';
  if (!userId || !path) return null;
  return {
    userId,
    username: q.get('n') || userId,
    path,
    pub: q.get('pub') === '1',
  };
}

/** Build the shareable standalone-doc URL for a feed document. */
export function buildDocUrl(userId: string, username: string, path: string, pub: boolean): string {
  const q = new URLSearchParams({ u: userId, n: username, p: path, pub: pub ? '1' : '0' });
  return `${window.location.origin}${window.location.pathname}#/doc?${q.toString()}`;
}
