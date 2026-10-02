// A play the user asked for, from the click until a poll shows it really started (or it times out).

export const PENDING_MS = 8000;

/**
 * One pending play at a time; a newer start replaces the older one.
 * - start(kind, {trackUri?, needPlaying = true}) → token. kind "resume" loads paused: needPlaying false.
 * - landed(token, at): the command returned at time at. Only polls that started after that count:
 *   one that started earlier may still show the old state.
 * - onPoll({isPlaying, trackUri, at}) → the resolved pending play, or null. It resolves when the
 *   play landed, the poll started after that, the track matches (when one was expected) and it
 *   plays (when needPlaying).
 * - timeout(token) / cancel(token) → true if that token was still pending (and clear it).
 */
export function createPending() {
  let cur = null;
  let seq = 0;
  const clear = (token) => {
    if (!cur || (token !== undefined && cur.token !== token)) return false;
    cur = null;
    return true;
  };
  return {
    start(kind, { trackUri = null, needPlaying = true } = {}) {
      cur = { token: ++seq, kind, trackUri, needPlaying, landedAt: null };
      return cur.token;
    },
    landed(token, at) {
      if (cur && cur.token === token) cur.landedAt = at;
    },
    onPoll({ isPlaying, trackUri, at }) {
      if (!cur || cur.landedAt === null || at < cur.landedAt) return null;
      if (cur.needPlaying && !isPlaying) return null;
      if (cur.trackUri && cur.trackUri !== trackUri) return null;
      const done = cur;
      cur = null;
      return done;
    },
    timeout: (token) => clear(token),
    cancel: (token) => clear(token),
    current: () => cur,
    reset() {
      cur = null;
    },
  };
}
