// App settings (localStorage "therun.settings"): parsed with defaults, so a broken value can't break the app.

export const SETTINGS_KEY = "therun.settings";

/** The player's bitrates, kbps, with their labels: Spotify's Normal / High / Very high. */
export const QUALITIES = [
  { kbps: 96, label: "Normal" },
  { kbps: 160, label: "High" },
  { kbps: 320, label: "Very high" },
];

export const isQuality = (v) => QUALITIES.some((q) => q.kbps === v);

/** The stored settings with defaults; raw is the stored string (or null). */
export function parseSettings(raw) {
  let s = null;
  try {
    s = typeof raw === "string" ? JSON.parse(raw) : raw;
  } catch {
    s = null;
  }
  const o = s && typeof s === "object" ? s : {};
  return { dockArt: o.dockArt !== false };
}
