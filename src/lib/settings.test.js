import { describe, it, expect } from "vitest";
import { parseSettings, isQuality, LYRICS_PLACEMENTS } from "./settings.js";

const DEFAULTS = { dockArt: true, coverRow: true, menuBar: true, menuBarTitle: false, autoUpdate: true, lyrics: "title", lyricsShown: true };

describe("parseSettings", () => {
  it("everything but the song in the menu bar is on by default (automatic updates too)", () => {
    expect(parseSettings(null)).toEqual(DEFAULTS);
    expect(parseSettings(undefined)).toEqual(DEFAULTS);
    expect(parseSettings("{}")).toEqual(DEFAULTS);
    expect(parseSettings({})).toEqual(DEFAULTS);
  });

  it("keeps a stored off", () => {
    expect(parseSettings('{"dockArt":false}')).toEqual({ ...DEFAULTS, dockArt: false });
    expect(parseSettings({ coverRow: false })).toEqual({ ...DEFAULTS, coverRow: false });
    expect(parseSettings({ dockArt: false, coverRow: false })).toEqual({ ...DEFAULTS, dockArt: false, coverRow: false });
    expect(parseSettings({ menuBar: false, menuBarTitle: true })).toEqual({ ...DEFAULTS, menuBar: false, menuBarTitle: true });
    expect(parseSettings({ autoUpdate: false })).toEqual({ ...DEFAULTS, autoUpdate: false });
    expect(parseSettings({ lyrics: "panel", lyricsShown: false })).toEqual({ ...DEFAULTS, lyrics: "panel", lyricsShown: false });
  });

  it("broken values fall back to the defaults", () => {
    for (const raw of ["not json", "[1]", [1], "null", "42", 42, '{"dockArt":"no"}', { coverRow: 0 }, { menuBarTitle: "yes" }, { autoUpdate: 0 }, { autoUpdate: "off" }, { lyrics: "big" }, { lyrics: 1 }, { lyricsShown: "no" }]) expect(parseSettings(raw)).toEqual(DEFAULTS);
  });
});

describe("LYRICS_PLACEMENTS", () => {
  it("lists the four placements", () => {
    expect(LYRICS_PLACEMENTS).toEqual(["off", "title", "cover", "panel"]);
  });
});

describe("isQuality", () => {
  it("only the three bitrates", () => {
    expect([96, 160, 320].every(isQuality)).toBe(true);
    expect(isQuality(128)).toBe(false);
    expect(isQuality("160")).toBe(false);
  });
});
