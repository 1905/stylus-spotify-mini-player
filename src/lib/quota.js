// Spotify's rate limit on this app's Web API (Rust's quota.rs answers RATE_LIMITED:<secs>: …).

export const RATE_LIMITED = "RATE_LIMITED";

/** Seconds left from a RATE_LIMITED error, 0 for any other error. */
export function rateLimitedSecs(e) {
  const m = /^RATE_LIMITED:(\d+)/.exec(String(e));
  return m ? Number(m[1]) : 0;
}

/** The error the UI raises itself while blocked (same shape as Rust's). */
export const rateLimitedError = (secs) => `RATE_LIMITED:${Math.max(0, Math.ceil(secs))}: Spotify paused this app's library access`;

/** "14 h", "25 min", "1 min". */
export function waitText(secs) {
  if (secs >= 3600) return `${Math.round(secs / 3600)} h`;
  return `${Math.max(1, Math.round(secs / 60))} min`;
}

/** The calm notice shown while blocked. */
export const quotaNotice = (secs) => `Spotify paused library access for ${waitText(secs)} — playback here still works`;

// Commands that never reach the Web API: the in-app player, the store, the disk cache, the log,
// the OS media controls, the login flow and the quota status itself.
const LOCAL_CMD = /^(auth_status$|login$|engine_|media_|local_|cache_get$|set_dock_art$|store_|session_get$|app_log$|api_status$)/;

/** True for a command that calls the Spotify Web API (blocked while rate-limited). */
export const isWebApi = (cmd) => !LOCAL_CMD.test(cmd);
