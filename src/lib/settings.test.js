import { describe, it, expect } from "vitest";
import { parseSettings, isQuality } from "./settings.js";

describe("parseSettings", () => {
  it("album art as app icon is on by default", () => {
    expect(parseSettings(null)).toEqual({ dockArt: true });
    expect(parseSettings("{}")).toEqual({ dockArt: true });
  });

  it("keeps a stored off", () => {
    expect(parseSettings('{"dockArt":false}')).toEqual({ dockArt: false });
  });

  it("broken values fall back to the defaults", () => {
    for (const raw of ["not json", "[1]", "null", "42", '{"dockArt":"no"}']) expect(parseSettings(raw)).toEqual({ dockArt: true });
  });
});

describe("isQuality", () => {
  it("only the three bitrates", () => {
    expect([96, 160, 320].every(isQuality)).toBe(true);
    expect(isQuality(128)).toBe(false);
    expect(isQuality("160")).toBe(false);
  });
});
