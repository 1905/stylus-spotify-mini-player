// Pure transport helpers: pending user intents, the repeat cycle, volume keys.

/**
 * Pending user intents, keyed by setting ("play", "shuffle", "repeat", "volume", "device").
 * A poll may overwrite a setting only when no command for it is queued or running, and the
 * poll started at least lagMs after the last one landed: Spotify can report the old value that long.
 */
export function createIntents(lagMs, lagFor = {}) {
  const all = new Map();
  const get = (key) => {
    if (!all.has(key)) all.set(key, { pending: 0, settleAfter: 0, seq: 0 });
    return all.get(key);
  };
  return {
    /** A command for key was asked for; returns its sequence number. */
    start(key) {
      const it = get(key);
      it.pending++;
      return ++it.seq;
    },
    /** A command for key landed (or failed) at time now. */
    finish(key, now) {
      const it = get(key);
      if (--it.pending <= 0) {
        it.pending = 0;
        // a lag set for "volume" also covers "volume:<device>"
        it.settleAfter = now + (lagFor[key] ?? lagFor[key.split(":")[0]] ?? lagMs);
      }
    },
    /** True if seq is the newest command for key: only it may undo the UI. */
    latest: (key, seq) => get(key).seq === seq,
    /** The UI went back to the device's value: the next poll may apply at once. */
    drop(key) {
      get(key).settleAfter = 0;
    },
    /** May a poll that started at pollStartedAt overwrite key? */
    settled(key, pollStartedAt) {
      const it = get(key);
      return it.pending === 0 && pollStartedAt >= it.settleAfter;
    },
    reset: () => all.clear(),
  };
}

const REPEAT = ["off", "context", "track"];

/** off → context → track → off. Anything unknown counts as off. */
export function nextRepeat(mode) {
  return REPEAT[(Math.max(0, REPEAT.indexOf(mode)) + 1) % REPEAT.length];
}

export const VOLUME_STEP = 5;

/** The volume after a key on the slider, clamped to 0–100; null for a key the slider ignores. */
export function stepVolume(v, key) {
  const cur = Number(v) || 0;
  const to = {
    ArrowRight: cur + VOLUME_STEP,
    ArrowUp: cur + VOLUME_STEP,
    ArrowLeft: cur - VOLUME_STEP,
    ArrowDown: cur - VOLUME_STEP,
    Home: 0,
    End: 100,
  }[key];
  return to === undefined ? null : Math.min(100, Math.max(0, to));
}
