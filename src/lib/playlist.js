// The playlist panel: what played, what plays now, what's next, as rows. Pure.

export const PANEL_HISTORY_MAX = 20;

/**
 * The panel's rows, top to bottom.
 * - list: the playing playlist / album / Liked Songs tracks in order, or null when unknown.
 * - now: the current track; history: recent plays, newest first ([{track, played_at}]);
 *   queue: Spotify's queue (tracks); shuffle: the shuffle state.
 * Shuffle off and the current song in the list: the whole list, played / now / next by position.
 * Otherwise: recent plays (newest just above now), now, then the queue.
 * Returns {mode: "list" | "queue", rows: [{key, role: "played" | "now" | "next", track, i}]};
 * i: the row's index in list (list mode), or in history / queue (queue mode; -1 for now).
 */
export function panelRows({ list = null, now = null, history = [], queue = [], shuffle = false } = {}) {
  const nowUri = now && now.uri;
  const tracks = (list || []).filter((t) => t && t.uri);
  const at = nowUri ? tracks.findIndex((t) => t.uri === nowUri) : -1;
  if (!shuffle && at >= 0) {
    const role = (i) => (i < at ? "played" : i === at ? "now" : "next");
    return { mode: "list", rows: tracks.map((track, i) => ({ key: `l${i}`, role: role(i), track, i })) };
  }
  // like the run: skip plays of the current song, collapse repeats in a row
  const past = [];
  (history || []).forEach((h, i) => {
    const t = h && h.track;
    if (!t || !t.uri || past.length >= PANEL_HISTORY_MAX || t.uri === nowUri) return;
    if (past.length && past[past.length - 1].track.uri === t.uri) return;
    past.push({ key: `h${i}`, role: "played", track: t, i });
  });
  past.reverse();
  const next = (queue || []).filter((t) => t && t.uri).map((track, i) => ({ key: `q${i}`, role: "next", track, i }));
  const cur = now ? [{ key: "now", role: "now", track: now, i: -1 }] : [];
  return { mode: "queue", rows: [...past, ...cur, ...next] };
}
