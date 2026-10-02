// App settings (store key "settings" in Rust's state.json): parsed with defaults, so a broken value can't break the app.

export const SETTINGS_KEY = "settings";

/** The player's bitrates, kbps, with their labels: Spotify's Normal / High / Very high. */
export const QUALITIES = [
  { kbps: 96, label: "Normal" },
  { kbps: 160, label: "High" },
  { kbps: 320, label: "Very high" },
];

export const isQuality = (v) => QUALITIES.some((q) => q.kbps === v);

/**
 * The stored settings with defaults; raw is the stored object (or a JSON string, or null).
 * - dockArt: the current cover as the app icon (default on).
 * - coverRow: played and next covers around the current one (default on); off = the current cover alone, centred.
 */
export function parseSettings(raw) {
  let s = null;
  try {
    s = typeof raw === "string" ? JSON.parse(raw) : raw;
  } catch {
    s = null;
  }
  const o = s && typeof s === "object" && !Array.isArray(s) ? s : {};
  return { dockArt: o.dockArt !== false, coverRow: o.coverRow !== false };
}
