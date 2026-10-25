// The playing song's lyrics: when they can show, which line is current, what the two-line preview
// says. Spotify's lyrics endpoint answers to the web player's own token (Rust lyrics.rs): the Free
// page's, or in Premium mode the token page's.

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

/** The two lines under the title: the current one (♪ before the first line and in the gaps) and the next. */
export function preview(lines, i) {
  if (!lines.length) return { cur: "", next: "" };
  return { cur: i < 0 ? "♪" : sung(lines[i]), next: sung(lines[i + 1]) };
}
