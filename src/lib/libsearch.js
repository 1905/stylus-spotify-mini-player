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

/** The row of uri is the song that plays now. */
export const isNowRow = (uri, now) => Boolean(uri && now && now.uri === uri);
