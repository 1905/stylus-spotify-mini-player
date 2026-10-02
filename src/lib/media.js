// OS media controls (Now Playing + media keys): what to send, when, and what a key press means.

/** A position this far off the expected one is a seek: Now Playing needs it. */
export const MEDIA_DRIFT_MS = 2000;

/**
 * The media_update payload, or null when nothing plays (idle: media_clear).
 * mode "other" (an ad or a podcast) has no song data, but its play state still matters for the keys.
 */
export function mediaPayload({ mode, now, isPlaying, positionMs }) {
  if (mode === "idle") return null;
  const t = mode === "track" ? now : null;
  return {
    title: (t && t.name) || null,
    artist: (t && t.artists) || null,
    album: (t && t.album) || null,
    cover: (t && t.cover) || null,
    durationMs: (t && t.duration_ms) || null,
    positionMs: t ? Math.max(0, Math.round(positionMs || 0)) : null,
    playing: Boolean(isPlaying),
  };
}

/**
 * Should next replace prev (both payloads or null) in Now Playing? On a track or play-state change,
 * and when the position jumped (a seek) against where prev, sent elapsedMs ago, would be by now.
 */
export function mediaChanged(prev, next, elapsedMs) {
  if (!prev || !next) return prev !== next;
  if (prev.title !== next.title || prev.artist !== next.artist || prev.album !== next.album) return true;
  if (prev.cover !== next.cover || prev.durationMs !== next.durationMs || prev.playing !== next.playing) return true;
  if (prev.positionMs == null || next.positionMs == null) return false;
  const expected = prev.positionMs + (prev.playing ? elapsedMs : 0);
  return Math.abs(next.positionMs - expected) > MEDIA_DRIFT_MS;
}

/**
 * A media-command payload as an app action: "toggle", "next_track", "previous_track",
 * {seek: ms}, or null (nothing to do). play/pause only act when they change something.
 */
export function mediaAction(cmd, isPlaying) {
  switch (cmd && cmd.action) {
    case "toggle":
      return "toggle";
    case "play":
      return isPlaying ? null : "toggle";
    case "pause":
      return isPlaying ? "toggle" : null;
    case "next":
      return "next_track";
    case "previous":
      return "previous_track";
    case "seek": {
      const ms = Number(cmd.positionMs);
      return Number.isFinite(ms) && ms >= 0 ? { seek: ms } : null;
    }
    default:
      return null;
  }
}
