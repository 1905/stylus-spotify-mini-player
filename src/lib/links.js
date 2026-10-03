// Spotify share links and URIs → {kind, id, uri}: the same rules as Rust's links.rs.
// https://open.spotify.com/playlist/<id>?si=…, …/intl-de/album/<id>, …/user/<name>/playlist/<id>,
// spotify:track:<id>, spotify:user:<name>:playlist:<id>.

const KINDS = new Set(["playlist", "album", "artist", "track"]);
const isId = (s) => /^[A-Za-z0-9]{22}$/.test(s);

/** The playlist, album, artist or track a link or URI names, or null. */
export function parseLink(text) {
  const t = String(text || "").trim();
  let parts;
  if (t.startsWith("spotify:")) {
    parts = t.slice(8).split(":");
  } else {
    const rest = t.replace(/^https?:\/\//i, "");
    const slash = rest.indexOf("/");
    if (slash < 0) return null;
    const host = rest.slice(0, slash).toLowerCase();
    if (host !== "open.spotify.com" && host !== "play.spotify.com") return null;
    parts = rest.slice(slash + 1).split(/[?#]/)[0].split("/").filter(Boolean);
  }
  // the last kind word followed by an id: skips intl-xx/ and user/<name>/
  for (let i = parts.length - 2; i >= 0; i--) {
    if (KINDS.has(parts[i]) && isId(parts[i + 1])) return { kind: parts[i], id: parts[i + 1], uri: `spotify:${parts[i]}:${parts[i + 1]}` };
  }
  return null;
}

/** Text that looks like an attempt at a Spotify link (so Search says it can't open it, not "no results"). */
export const looksLikeLink = (text) => /^(https?:\/\/)?(open|play)\.spotify\.com\/|^spotify:|^https?:\/\/spotify\.link\//i.test(String(text || "").trim());
