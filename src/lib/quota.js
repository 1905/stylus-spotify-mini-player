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

/** Why an action failed while blocked: "remote" = controlling another device, "library" = a library read or write. */
export const quotaNotice = (kind, secs) => `Spotify limits this for ${waitText(secs)}${kind === "remote" ? " — works on This Mac" : ""}`;

/** The quiet line in Settings while blocked. */
export const quotaStatus = (secs) => `Web API paused for ${waitText(secs)}`;

// Commands with no source but the Web API (Rust's spotify.rs): playback of a remote device and its
// state. Everything else is local, or Rust tries Spotify's internal API first and the Web API last,
// so it may still work while blocked: Rust answers RATE_LIMITED only when every source failed.
const WEB_ONLY = new Set([
  "playback_state",
  "transfer_playback",
  "set_volume",
  "set_shuffle",
  "set_repeat",
  "play_context",
  "play_on_device",
  "resume",
  "resume_at",
  "pause",
  "next_track",
  "previous_track",
  "seek",
]);

/** True for a command that can only run on the Web API (fails at once while rate-limited). */
export const isWebOnly = (cmd) => WEB_ONLY.has(cmd);
