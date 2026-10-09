// Play sources: which contexts a detail view names, and which take a start track.

const ORIGIN_KINDS = new Set(["playlist", "album"]);

/** The context uri of a detail view ({kind, id}), or null: playlists and albums only. */
export const originUri = (o) => (o && ORIGIN_KINDS.has(o.kind) && o.id ? `spotify:${o.kind}:${o.id}` : null);

/** Only playlists and albums take a start track (offset); any other context would start from the top. */
export const offsettable = (uri) => /^spotify:(playlist|album):/.test(String(uri || ""));

/**
 * Rust's saved session ({contextUri, uris, trackUri, …}) as {contextUri, uris, trackUri}, or null when it names
 * nothing. trackUri: the saved one, else a list's first; null for a context that played to its end (it reloads
 * on a first track that Spotify picks).
 */
export function restoredSource(s) {
  if (!s || typeof s !== "object") return null;
  const contextUri = typeof s.contextUri === "string" && s.contextUri ? s.contextUri : null;
  const uris = Array.isArray(s.uris) && s.uris.length ? s.uris : null;
  const trackUri = (typeof s.trackUri === "string" && s.trackUri) || (uris && uris[0]) || null;
  if (!trackUri && !contextUri) return null;
  return { contextUri, uris, trackUri };
}
