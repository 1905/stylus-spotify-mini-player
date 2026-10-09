// How the UI learns what plays, and how often it asks Spotify.
//
// - "events": this Mac (the in-app player) is the active device. Rust sends `player-state` on every
//   change; the loop only re-renders from it (no request), plus one playback_state sanity check a minute.
// - "poll": another device plays (or nothing): playback_state every 5 s visible, 30 s hidden.

export const LOCAL_TICK_MS = 1000; // events mode: a local re-render, no request
export const HIDDEN_LOCAL_TICK_MS = 3000;
export const POLL_MS = 5000; // another device, window visible
export const HIDDEN_POLL_MS = 30000; // another device or nothing, window hidden
export const SANITY_MS = 60000; // events mode: one playback_state a minute, to catch a missed event
export const LIST_MIN_MS = 30000; // queue / recently played / devices: at most this often each
// failed polls retry from 1s up to the normal poll period: ~15 s of quiet "Connecting…" before an error shows
export const RETRY_FIRST_MS = 1000;
export const GIVE_UP_FAILURES = 6;

/** True once enough polls in a row failed that the error is shown instead of a loader. */
export function gaveUp(failures) {
  return failures >= GIVE_UP_FAILURES;
}

/**
 * Which source the loop reads.
 * local: the last `player-state` payload (null = none yet). distrust: a sanity check showed
 * another device active since that payload.
 */
export function pollMode({ local, distrust = false }) {
  if (local && local.engine_active && !distrust) return "events";
  return "poll";
}

/** Why that mode, for the log line written when the mode changes. */
export function modeReason({ mode, distrust = false }) {
  if (mode === "events") return "this Mac plays (player-state events)";
  return distrust ? "Spotify shows another device active" : "another device or nothing plays";
}

/** The delay before the next loop tick. */
export function pollDelay({ hidden, mode, failures = 0 }) {
  if (mode === "events") return hidden ? HIDDEN_LOCAL_TICK_MS : LOCAL_TICK_MS;
  const base = hidden ? HIDDEN_POLL_MS : POLL_MS;
  if (failures > 0) return Math.min(RETRY_FIRST_MS * 2 ** (failures - 1), base);
  return base;
}

/** Events mode: time for the sanity check? */
export const sanityDue = ({ lastAt, now }) => now - lastAt >= SANITY_MS;

/**
 * May a list (queue, recently played, devices) be fetched now? Only when something needs it
 * (force: the user just changed it), and at most every LIST_MIN_MS.
 */
export function listDue({ lastAt, now, need, force = false }) {
  if (!(need || force)) return false;
  return force || now - lastAt >= LIST_MIN_MS;
}
