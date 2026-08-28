/**
 * Runtime environment detection for API and WebSocket endpoints.
 *
 * When the frontend is served by Tauri's custom-protocol (tauri://localhost),
 * API calls must go to the embedded Axum server at http://localhost:3000.
 * When served directly by Axum (browser mode), relative URLs work fine.
 */

const isTauri =
  typeof window !== 'undefined' &&
  (window.location.protocol === 'tauri:' ||
   window.location.protocol === 'tauri-port:' ||
   // @ts-expect-error – injected by Tauri when withGlobalTauri is true
   typeof window.__TAURI__ !== 'undefined');

/** Base URL for HTTP API calls. Empty string = relative to current origin. */
export const API_BASE: string = isTauri ? 'http://localhost:3000' : '';

/** Base URL for WebSocket connections. */
export const WS_BASE: string = isTauri ? 'ws://localhost:3000' : '';
