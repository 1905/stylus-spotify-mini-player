// How long until the next poll.

export const POLL_MS = 1000;
export const HIDDEN_POLL_MS = 3000;
export const ERROR_POLL_MS = 4000;

/**
 * The delay before the next poll, or null to stop polling.
 * A hidden window stops only when nothing plays (idle); while something plays it keeps
 * polling, slower, so Now Playing and the media keys stay current.
 */
export function pollDelay({ hidden, mode, error }) {
  if (hidden && mode === "idle") return null;
  if (error) return ERROR_POLL_MS;
  return hidden ? HIDDEN_POLL_MS : POLL_MS;
}
