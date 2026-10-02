// "Your favorites by <artist>": Spotify no longer exposes an artist's top tracks or any play
// counts (Feb 2026), so the artist page ranks from your own listening instead.

/**
 * Tracks by artistId from lists ranked best-first (e.g. top 4 weeks, 6 months, all time, liked).
 * The first occurrence of a uri wins, so earlier lists rank higher.
 */
export function favoritesBy(artistId, lists, max = 10) {
  const seen = new Set();
  const out = [];
  for (const list of lists) {
    for (const t of list || []) {
      if (out.length >= max) return out;
      if (!t || !t.uri || seen.has(t.uri)) continue;
      if (!(t.artist_list || []).some((a) => a && a.id === artistId)) continue;
      seen.add(t.uri);
      out.push(t);
    }
  }
  return out;
}
