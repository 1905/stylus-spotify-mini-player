// The last session: what played, from where, and how far. Written while the app runs, read once at launch.
//
// Shape (localStorage "therun.lastSession"):
//   {accountId, contextUri, origin, uris, trackUri, positionMs, savedAt}
// - origin: {kind: "playlist"|"album", id} when the play started from that detail view; a resume
//   plays it as the context, so the whole playlist continues, not a fragment.
// - uris: the list Spotify was given (≤ 200), when the play wasn't by context. It also tells which
//   tracks belong to the source (an origin play is by uris too).

export const SESSION_KEY = "therun.lastSession";
export const SESSION_THROTTLE_MS = 10000;
export const SESSION_URIS_MAX = 200;

const ORIGIN_KINDS = new Set(["playlist", "album"]);

/** The context uri of an origin, or null. */
export const originUri = (o) => (o && ORIGIN_KINDS.has(o.kind) && o.id ? `spotify:${o.kind}:${o.id}` : null);

/** Only playlists and albums take a start track (offset); any other context would start from the top. */
export const offsettable = (uri) => /^spotify:(playlist|album):/.test(String(uri || ""));

const cleanUris = (uris) => (Array.isArray(uris) ? uris.filter((u) => typeof u === "string" && u).slice(0, SESSION_URIS_MAX) : null);

/** A stored session, validated; null when missing or broken. */
export function parseSession(raw) {
  let s;
  try {
    s = typeof raw === "string" ? JSON.parse(raw) : raw;
  } catch {
    return null;
  }
  if (!s || typeof s !== "object" || typeof s.trackUri !== "string" || !s.trackUri) return null;
  const uris = cleanUris(s.uris);
  return {
    accountId: typeof s.accountId === "string" ? s.accountId : null,
    contextUri: typeof s.contextUri === "string" && s.contextUri ? s.contextUri : null,
    origin: originUri(s.origin) ? { kind: s.origin.kind, id: s.origin.id } : null,
    uris: uris && uris.length ? uris : null,
    trackUri: s.trackUri,
    positionMs: Math.max(0, Math.round(Number(s.positionMs) || 0)),
    savedAt: Number(s.savedAt) || 0,
  };
}

/**
 * The session for a play the app just started. src: {contextUri?} or {uris}; origin: the detail
 * view it was started from ({kind, id}) or null.
 */
export function playSession(accountId, src, origin, nowMs, members = null) {
  // with a context, `members` (the context's known tracks) is kept only as a membership list
  const uris = cleanUris(src.contextUri ? members : src.uris);
  const trackUri = src.trackUri || (uris && uris[0]) || null;
  if (!trackUri && !src.contextUri) return null;
  return {
    accountId: accountId || null,
    contextUri: src.contextUri || null,
    origin: originUri(origin) ? { kind: origin.kind, id: origin.id } : null,
    uris: uris && uris.length ? uris : null,
    trackUri,
    positionMs: 0,
    savedAt: nowMs,
  };
}

/** Does the session's source hold this track (or play this context)? */
function holds(prev, trackUri, contextUri) {
  if (prev.uris && prev.uris.includes(trackUri)) return true;
  if (!contextUri) return false;
  return prev.contextUri === contextUri || originUri(prev.origin) === contextUri;
}

/**
 * The session to write after a poll, or null for no write.
 * poll: {accountId, trackUri, contextUri, positionMs}. force: a pause or a quit (no throttle).
 * - At most every SESSION_THROTTLE_MS, and only while a song is current and the account is known.
 * - Another account's session is replaced, never merged.
 * - Provenance: a track outside the saved source replaces the source with the poll's context,
 *   or with that one track when there is none.
 */
export function sessionToSave(prev, poll, nowMs, force = false) {
  if (!poll || !poll.trackUri || !poll.accountId) return null;
  const mine = prev && prev.accountId === poll.accountId ? prev : null;
  if (mine && !force && nowMs - (mine.savedAt || 0) < SESSION_THROTTLE_MS) return null;
  const positionMs = Math.max(0, Math.round(Number(poll.positionMs) || 0));
  const base = { accountId: poll.accountId, trackUri: poll.trackUri, positionMs, savedAt: nowMs };
  if (mine && holds(mine, poll.trackUri, poll.contextUri)) {
    return { ...base, contextUri: poll.contextUri || mine.contextUri, origin: mine.origin, uris: mine.uris };
  }
  if (poll.contextUri) return { ...base, contextUri: poll.contextUri, origin: null, uris: null };
  return { ...base, contextUri: null, origin: null, uris: [poll.trackUri] };
}

/**
 * What to load (paused) at launch, normalized to exactly one source, or null for nothing.
 * playback: the first playback_state; last: the stored session; accountId: the signed-in account.
 * Returns {contextUri, trackUri, positionMs} or {uris, trackUri, positionMs}.
 */
export function resumeSource(playback, last, accountId) {
  if (playback && playback.is_playing) return null; // something plays: leave it alone
  const saved = last && accountId && last.accountId === accountId ? last : null;
  const track = playback && playback.active && playback.track && playback.track.uri;
  if (track) {
    const positionMs = Math.max(0, Math.round(Number(playback.progress_ms) || 0));
    if (playback.context_uri) return { contextUri: playback.context_uri, trackUri: track, positionMs };
    if (saved && saved.uris && saved.uris.includes(track)) return fromSaved({ ...saved, trackUri: track, positionMs });
    return { uris: [track], trackUri: track, positionMs };
  }
  return saved ? fromSaved(saved) : null;
}

/** A saved session as one source: its origin first, then its context, then its uris. */
function fromSaved(s) {
  const { trackUri, positionMs } = s;
  if (!trackUri) return null;
  const ctx = originUri(s.origin) || s.contextUri;
  if (ctx && offsettable(ctx)) return { contextUri: ctx, trackUri, positionMs };
  if (s.uris && s.uris.includes(trackUri)) return { uris: s.uris, trackUri, positionMs };
  if (ctx) return { contextUri: ctx, trackUri, positionMs };
  return { uris: [trackUri], trackUri, positionMs };
}
