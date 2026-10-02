import { describe, it, expect } from "vitest";
import { parseSettings, isQuality } from "./settings.js";

const DEFAULTS = { dockArt: true, coverRow: true };

describe("parseSettings", () => {
  it("everything is on by default", () => {
    expect(parseSettings(null)).toEqual(DEFAULTS);
    expect(parseSettings(undefined)).toEqual(DEFAULTS);
    expect(parseSettings("{}")).toEqual(DEFAULTS);
    expect(parseSettings({})).toEqual(DEFAULTS);
  });

  it("keeps a stored off", () => {
    expect(parseSettings('{"dockArt":false}')).toEqual({ dockArt: false, coverRow: true });
    expect(parseSettings({ coverRow: false })).toEqual({ dockArt: true, coverRow: false });
    expect(parseSettings({ dockArt: false, coverRow: false })).toEqual({ dockArt: false, coverRow: false });
  });

  it("broken values fall back to the defaults", () => {
    for (const raw of ["not json", "[1]", [1], "null", "42", 42, '{"dockArt":"no"}', { coverRow: 0 }]) expect(parseSettings(raw)).toEqual(DEFAULTS);
  });
});

describe("isQuality", () => {
  it("only the three bitrates", () => {
    expect([96, 160, 320].every(isQuality)).toBe(true);
    expect(isQuality(128)).toBe(false);
    expect(isQuality("160")).toBe(false);
  });
});
