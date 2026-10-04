// The menu-bar mini player (Rust tray.rs, src/mini.js): what the main window sends it, when, and
// what the popover derives from it. The main window stays the only brain; this is its summary.
import { drifted } from "./media.js";

/** A position this far off the expected one is a jump (a seek, a restart): the popover needs it. */
export const MINI_DRIFT_MS = 3000;

/**
 * The mini_push payload. now: the track on screen (a pending play's preview counts), status: the
 * main window's headline when no song shows ("Nothing playing", "Loading…"). sentAt: Date.now(),
 * so the popover runs the bar on from there. volume null = no volume control; heart null = none.
 */
export function miniPayload({ mode, now, status, isPlaying, pending, skipping, loading, positionMs, volume, heart, device }) {
  const t = mode === "track" || loading ? now : null;
  return {
    mode,
    title: (t && t.name) || null,
    artist: (t && t.artists) || null,
    cover: (t && t.cover) || null,
    status: t ? null : status || null,
    playing: Boolean(isPlaying) && mode !== "idle",
    pending: Boolean(pending),
    skipping: skipping || null,
    loading: Boolean(loading),
    positionMs: t && !loading && !skipping ? Math.max(0, Math.round(positionMs || 0)) : 0,
    durationMs: (t && t.duration_ms) || 0,
    sentAt: Date.now(),
    volume: volume == null ? null : Math.round(volume),
    heart: heart == null ? null : heart,
    device: device || null,
  };
}

const KEYS = ["mode", "title", "artist", "cover", "status", "playing", "pending", "skipping", "loading", "durationMs", "volume", "heart", "device"];

/** Should next replace prev? On any change but the position, and on a position jump. */
export function miniChanged(prev, next) {
  if (!prev || !next) return prev !== next;
  if (KEYS.some((k) => prev[k] !== next[k])) return true;
  return drifted(prev, next, next.sentAt - prev.sentAt, MINI_DRIFT_MS);
}

/** The bar's fill, 0–1, at wall-clock time nowMs. */
export function miniProgress(s, nowMs) {
  if (!s || !s.durationMs || s.loading || s.skipping) return 0;
  const p = s.positionMs + (s.playing ? Math.max(0, nowMs - s.sentAt) : 0);
  return Math.min(1, Math.max(0, p / s.durationMs));
}

/** The speaker icon for a level: "mute" | "volumeLow" | "volumeHigh" (icons.js names). */
export const volumeIcon = (v) => (v === 0 ? "mute" : v < 40 ? "volumeLow" : "volumeHigh");
