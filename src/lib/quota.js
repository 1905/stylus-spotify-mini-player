// Spotify's rate limit on this app's Web API (Rust's quota.rs answers RATE_LIMITED:<secs>: …).

export const RATE_LIMITED = "RATE_LIMITED";

/** Seconds left from a RATE_LIMITED error, 0 for any other error. */
export function rateLimitedSecs(e) {
  const m = /^RATE_LIMITED:(\d+)/.exec(String(e));
  return m ? Number(m[1]) : 0;
}

/** Rust's RATE_LIMITED error for `secs` left (the UI builds one from api_status at launch). */
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
