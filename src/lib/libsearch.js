// Library find: the list filter and the Library search, over lists already on the screen or in the disk cache.

/** Lower case, accents off: "Beyoncé" and "beyonce" are the same. */
export const fold = (s) =>
  String(s ?? "")
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLowerCase();

/** The typed words, folded. */
export const queryWords = (q) => fold(q).split(/\s+/).filter(Boolean);

/** True when every word is in one of the fields. No words matches everything. */
export function matchesWords(words, fields) {
  if (!words.length) return true;
  const hay = fields.map(fold).join("\n");
  return words.every((w) => hay.includes(w));
}

export const matches = (q, fields) => matchesWords(queryWords(q), fields);

/** A song's searchable fields; fallbackAlbum names the album when the track itself doesn't (album lists). */
export const trackFields = (t, fallbackAlbum = "") => [t && t.name, t && t.artists, (t && t.album) || fallbackAlbum];

/** The indexes of the tracks that match q (all of them for an empty q). */
export function matchIndexes(q, tracks) {
  const words = queryWords(q);
  const out = new Set();
  (tracks || []).forEach((t, i) => {
    if (t && matchesWords(words, trackFields(t))) out.add(i);
  });
  return out;
}

const ownerName = (o) => (typeof o === "string" ? o : (o && (o.display_name || o.name)) || "");

export const SEARCH_LIMITS = { songs: 50, other: 20 };

/**
 * Search the Library for q. index: {playlists, albums, artists, lists}; lists = [{source: {kind, id, name}, tracks}],
 * songs from the disk cache. Returns {playlists, albums, artists, songs}; a song is {track, list, i} (i: its index
 * in list.tracks), once per list. An empty q finds nothing.
 */
export function searchLibrary(q, index, limits = SEARCH_LIMITS) {
  const words = queryWords(q);
  const none = { playlists: [], albums: [], artists: [], songs: [] };
  if (!words.length || !index) return none;
  const pick = (items, fields) => {
    const out = [];
    for (const it of items || []) {
      if (out.length >= limits.other) break;
      if (it && matchesWords(words, fields(it))) out.push(it);
    }
    return out;
  };
  const songs = [];
  for (const list of index.lists || []) {
    if (songs.length >= limits.songs) break;
    const seen = new Set();
    const album = list.source && list.source.kind === "album" ? list.source.name : "";
    (list.tracks || []).forEach((t, i) => {
      if (songs.length >= limits.songs || !t || !t.uri || seen.has(t.uri)) return;
      if (!matchesWords(words, trackFields(t, album))) return;
      seen.add(t.uri);
      songs.push({ track: t, list, i });
    });
  }
  return {
    playlists: pick(index.playlists, (p) => [p.name, ownerName(p.owner)]),
    albums: pick(index.albums, (a) => [a.name, a.artists]),
    artists: pick(index.artists, (a) => [a.name]),
    songs,
  };
}

/** The row of uri is the song that plays now. */
export const isNowRow = (uri, now) => Boolean(uri && now && now.uri === uri);
