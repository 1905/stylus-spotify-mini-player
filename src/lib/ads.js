// A Free-mode ad break: Spotify names only the first ad (its uri is spotify:ad:*); a following ad
// reports the uri of the track that waits after the break. While the break goes on, a poll whose
// uri is another track (not the break's end) is another ad: skip it, and that uri becomes the
// break's end. A poll that names nothing (the track left: the break ended into a pause) or a song
// that is the break's end (or the break went idle) ends the break.
//
// The break outlives the polls that see it: a poll is every POLL_MS (visible) or HIDDEN_POLL_MS
// (hidden), and an ad can outlive a single poll. So this runs on every poll, not only when the
// playing item changes — inside the break the mode stays "other" and the uri stays the same (Spotify
// names only the first ad), so a change-gated skip would see the second ad never.

export const AD_SETTLE_MS = 35000; // a gap this long with no ad reported: the break is over

/**
 * The ad-break state machine, one call per poll. Returns what to do and the state to keep.
 *
 * poll: { mode, uri } — the poll's front-end mode ("track" | "other" | "idle") and its track uri
 * (null when the cluster names none). at: performance.now() ms. free: engine.mode is "free".
 * state: { uri, until } | null — the break so far (uri = the break's end, until = when a gap ends it).
 *
 * → { state, skip } — skip: true when a "next" must go (the break began, or this poll is another ad);
 * toast: true when the break began (the user should be told); log: an applog line for the break's
 * start or a re-skip, else null.
 */
export function adSkipOnPoll({ mode, uri = null, at }, { free = false, state = null }) {
  if (!free) return { state: null, skip: false }; // the in-app player has no ads
  if (state && (mode === "idle" || mode === "track" && uri === state.uri))
    return { state: null, skip: false }; // the break ended: idle, or the song that was its end
  if (!state) {
    if (mode !== "other") return { state: null, skip: false };
    return { state: { uri, until: at + AD_SETTLE_MS }, skip: true, toast: true, log: `ad break started (uri ${uri || "none"})` };
  }
  if (mode === "other" && uri === null) return { state: null, skip: false }; // nothing ad-like named: the break ended into a pause
  if (mode === "track" && uri && uri !== state.uri) return { state: null, skip: false }; // a song that isn't the break's end
  if (at > state.until) return { state: null, skip: false }; // no ad reported since the last skip: the break is over
  if (mode !== "other") return { state, skip: false }; // a named song inside the break: its end, keep watching
  state = { uri, until: at + AD_SETTLE_MS }; // another ad (it names its own end): skip it
  return { state, skip: true, log: `ad break: skipping again (uri ${uri})` };
}
