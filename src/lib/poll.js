// How the UI learns what plays, and how often it may ask the Spotify Web API.
//
// Spotify blocked the app's Web API quota once (429, ~15 h) after a 1 s poll. Now:
// - "events": this Mac (the in-app player) is the active device. Rust sends `player-state` on every
//   change; the loop only re-renders from it (no request), plus one Web API sanity check a minute.
// - "poll": another device plays (or nothing): playback_state every 5 s visible, 30 s hidden.
// - "blocked": Spotify rate-limited the app: no Web API request at all until the block ends.

export const LOCAL_TICK_MS = 1000; // events mode: a local re-render, no request
export const HIDDEN_LOCAL_TICK_MS = 3000;
export const POLL_MS = 5000; // another device, window visible
export const HIDDEN_POLL_MS = 30000; // another device or nothing, window hidden
export const BLOCKED_TICK_MS = 5000; // blocked: a local tick, no request
export const SANITY_MS = 60000; // events mode: one playback_state a minute, to catch a missed event
export const LIST_MIN_MS = 30000; // queue / recently played / devices: at most this often each
export const ERROR_POLL_MS = POLL_MS;
// failed polls retry from 1s up to the normal poll period: ~15 s of quiet "Connecting…" before an error shows
export const RETRY_FIRST_MS = 1000;
export const GIVE_UP_FAILURES = 6;

/** True once enough polls in a row failed that the error is shown instead of a loader. */
export function gaveUp(failures) {
  return failures >= GIVE_UP_FAILURES;
}

/**
 * Which source the loop reads.
 * local: the last `player-state` payload (null = none yet). distrust: a Web API check showed
 * another device active since that payload. blockedMs: rate-limit time left (0 = open).
 * The in-app player's state needs no Web API, so a block doesn't stop "events".
 */
export function pollMode({ local, distrust = false, blockedMs = 0 }) {
  if (local && local.engine_active && !distrust) return "events";
  if (blockedMs > 0) return "blocked";
  return "poll";
}

/** Why that mode, for the log line written when the mode changes. */
export function modeReason({ mode, blockedMs = 0, distrust = false }) {
  if (mode === "events") return blockedMs > 0 ? "this Mac plays (player-state events); Web API rate-limited" : "this Mac plays (player-state events)";
  if (mode === "blocked") return `Spotify rate limit, ${Math.ceil(blockedMs / 1000)} s left`;
  return distrust ? "the Web API shows another device active" : "another device or nothing plays";
}

/** The delay before the next loop tick. */
export function pollDelay({ hidden, mode, failures = 0 }) {
  if (mode === "events") return hidden ? HIDDEN_LOCAL_TICK_MS : LOCAL_TICK_MS;
  if (mode === "blocked") return BLOCKED_TICK_MS;
  const base = hidden ? HIDDEN_POLL_MS : POLL_MS;
  if (failures > 0) return Math.min(RETRY_FIRST_MS * 2 ** (failures - 1), base);
  return base;
}

/** Events mode: time for the Web API sanity check? Never while blocked. */
export const sanityDue = ({ lastAt, now, blockedMs = 0 }) => blockedMs <= 0 && now - lastAt >= SANITY_MS;

/**
 * May a list (queue, recently played, devices) be fetched now? Only when something needs it
 * (force: the user just changed it), never while blocked, and at most every LIST_MIN_MS.
 */
export function listDue({ lastAt, now, need, force = false, blockedMs = 0 }) {
  if (blockedMs > 0 || !(need || force)) return false;
  return force || now - lastAt >= LIST_MIN_MS;
}
