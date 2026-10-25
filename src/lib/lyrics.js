// The playing song's lyrics: when they can show, which line is current, what the two-line preview
// says. Spotify's lyrics endpoint answers to the web player's own token (Rust lyrics.rs), fetched
// by a hidden token page in both modes.

/** A placement is picked and a song (not an ad) plays. */
export const lyricsActive = (settings, mode) => settings.lyrics && settings.lyrics !== "off" && mode === "track";

/**
 * The index of the last line that has started by `ms`, -1 before the first. `lines` are sorted by
 * `ms`; `hint` is the previous answer, kept without a scan while it still holds (every frame).
 */
export function lineAt(lines, ms, hint = -1) {
  if (hint >= 0 && hint < lines.length && lines[hint].ms <= ms && (hint + 1 === lines.length || ms < lines[hint + 1].ms)) return hint;
  return lines.findLastIndex((l) => l.ms <= ms);
}

/** A line as sung: its words, or ♪ for an instrumental gap. */
export const sung = (line) => (line ? line.text || "♪" : "");

/** The four lines by the title: the one just sung, the current one (♪ before the first line and in the gaps), the next two. */
export function preview(lines, i) {
  if (!lines.length) return ["", "", "", ""];
  return [sung(lines[i - 1]), i < 0 ? "♪" : sung(lines[i]), sung(lines[i + 1]), sung(lines[i + 2])];
}
