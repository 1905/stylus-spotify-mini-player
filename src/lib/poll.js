// How long until the next poll.

export const POLL_MS = 1000;
export const HIDDEN_POLL_MS = 3000;
export const ERROR_POLL_MS = 4000;
export const HIDDEN_IDLE_POLL_MS = 10000;

/**
 * The delay before the next poll. A hidden window polls slower: 3s while something plays
 * (Now Playing and the media keys stay current), 10s when idle (so playback started from a
 * phone on this Mac still wakes it).
 */
export function pollDelay({ hidden, mode, error }) {
  // hidden and idle: still look now and then — a phone can start playback on this Mac
  if (hidden && mode === "idle") return HIDDEN_IDLE_POLL_MS;
  if (error) return ERROR_POLL_MS;
  return hidden ? HIDDEN_POLL_MS : POLL_MS;
}
