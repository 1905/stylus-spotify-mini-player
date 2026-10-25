// App settings (store key "settings" in Rust's state.json): parsed with defaults, so a broken value can't break the app.

export const SETTINGS_KEY = "settings";

/** The player's bitrates, kbps, with their labels: Spotify's Normal / High / Very high. */
export const QUALITIES = [
  { kbps: 96, label: "Normal" },
  { kbps: 160, label: "High" },
  { kbps: 320, label: "Very high" },
];

export const isQuality = (v) => QUALITIES.some((q) => q.kbps === v);

/** Where the playing song's lyrics show (Free mode only): under the title, instead of the cover, or off. */
export const LYRICS_PLACEMENTS = ["title", "cover", "off"];

/**
 * The stored settings with defaults; raw is the stored object (or a JSON string, or null).
 * - dockArt: the current cover as the app icon (default on).
 * - coverRow: played and next covers around the current one (default on); off = the current cover alone, centred.
 * - menuBar: the icon and mini player in the menu bar (default on); Rust tray.rs reads it at launch too.
 * - menuBarTitle: the current song next to that icon (default off).
 * - autoUpdate: updates download and install at the quit (default on); Rust updater.rs reads it too.
 * - lyrics: where the song's words show, one of LYRICS_PLACEMENTS (default "title"); Free mode only, the web player has the token.
 * - lyricsShown: the Lyrics button's state (default on); toggles lyrics visibility in Free mode.
 */
export function parseSettings(raw) {
  let s = null;
  try {
    s = typeof raw === "string" ? JSON.parse(raw) : raw;
  } catch {
    s = null;
  }
  const o = s && typeof s === "object" && !Array.isArray(s) ? s : {};
  const lyricsValue = o.lyrics;
  let lyrics = "title";
  if (LYRICS_PLACEMENTS.includes(lyricsValue)) {
    lyrics = lyricsValue;
  } else if (lyricsValue === "panel") {
    lyrics = "title";
  }
  return { dockArt: o.dockArt !== false, coverRow: o.coverRow !== false, menuBar: o.menuBar !== false, menuBarTitle: o.menuBarTitle === true, autoUpdate: o.autoUpdate !== false, lyrics, lyricsShown: o.lyricsShown !== false };
}
