// How long until the next poll.

export const POLL_MS = 1000;
export const HIDDEN_POLL_MS = 3000;
export const ERROR_POLL_MS = 4000;
export const HIDDEN_IDLE_POLL_MS = 10000;
// failed polls retry fast first (0.5, 1, 2, 4, 4, 4 s): ~15 s of quiet "Connecting…" before an error shows
export const RETRY_FIRST_MS = 500;
export const GIVE_UP_FAILURES = 6;

/** True once enough polls in a row failed that the error is shown instead of a loader. */
export function gaveUp(failures) {
  return failures >= GIVE_UP_FAILURES;
}

/**
 * The delay before the next poll. A hidden window polls slower: 3s while something plays
 * (Now Playing and the media keys stay current), 10s when idle (so playback started from a
 * phone on this Mac still wakes it). After a failed poll: back off from 0.5s up to 4s.
 */
export function pollDelay({ hidden, mode, failures = 0 }) {
  // hidden and idle: still look now and then — a phone can start playback on this Mac
  if (hidden && mode === "idle") return HIDDEN_IDLE_POLL_MS;
  if (failures > 0) return Math.min(RETRY_FIRST_MS * 2 ** (failures - 1), ERROR_POLL_MS);
  return hidden ? HIDDEN_POLL_MS : POLL_MS;
}
