// Pure helper: the Spotify mixes the app has seen (Spotify doesn't list its own playlists).

export const MAX_MIXES = 30;
const PREFIX = "spotify:playlist:";

/**
 * Note the playlist contexts in contextUris (newest first) in the known list [{id, seen}].
 * Only playlists the user doesn't own count. A mix seen again moves to the front with seen = nowIso.
 * Returns a new list, newest first, at most MAX_MIXES long.
 */
export function noteMixes(known, contextUris, ownPlaylistIds, nowIso) {
  const own = new Set(ownPlaylistIds || []);
  const fresh = [];
  for (const uri of contextUris || []) {
    if (typeof uri !== "string" || !uri.startsWith(PREFIX)) continue;
    const id = uri.slice(PREFIX.length);
    if (id && !own.has(id) && !fresh.some((m) => m.id === id)) fresh.push({ id, seen: nowIso });
  }
  const old = (Array.isArray(known) ? known : []).filter(
    (m) => m && typeof m.id === "string" && m.id && !own.has(m.id) && !fresh.some((f) => f.id === m.id),
  );
  return [...fresh, ...old].slice(0, MAX_MIXES);
}
