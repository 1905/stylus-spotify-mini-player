import { describe, it, expect } from "vitest";
import { fold, matches, matchIndexes, isNowRow } from "./libsearch.js";

const T = (n, name, artists = "A", album = "") => ({ uri: `spotify:track:${n}`, name, artists, album });

describe("matches", () => {
  it("ignores case and accents", () => {
    expect(fold("Beyoncé ÅNGEL")).toBe("beyonce angel");
    expect(matches("BEYONCE", ["Beyoncé"])).toBe(true);
    expect(matches("sigur rós", ["Sigur Ros"])).toBe(true);
  });

  it("needs every word, in any field", () => {
    expect(matches("xx intro", ["Intro", "The xx", "xx"])).toBe(true);
    expect(matches("xx outro", ["Intro", "The xx"])).toBe(false);
  });

  it("an empty query matches everything", () => {
    expect(matches("", ["x"])).toBe(true);
    expect(matches("   ", [])).toBe(true);
  });

  it("tolerates missing fields", () => {
    expect(matches("a", [null, undefined, "A"])).toBe(true);
    expect(matches("a", [null])).toBe(false);
  });
});

describe("matchIndexes", () => {
  const tracks = [T(1, "Golden", "Harry Styles", "Fine Line"), T(2, "Toxic", "Britney"), null, T(3, "Stitches", "Shawn")];
  it("title, artists and album; skips holes", () => {
    expect([...matchIndexes("fine", tracks)]).toEqual([0]);
    expect([...matchIndexes("britney tox", tracks)]).toEqual([1]);
    expect([...matchIndexes("", tracks)]).toEqual([0, 1, 3]);
  });
});

describe("isNowRow", () => {
  it("compares the uri with the playing track", () => {
    expect(isNowRow("spotify:track:1", { uri: "spotify:track:1" })).toBe(true);
    expect(isNowRow("spotify:track:1", { uri: "spotify:track:2" })).toBe(false);
    expect(isNowRow("spotify:track:1", null)).toBe(false);
    expect(isNowRow("", { uri: "" })).toBe(false);
  });
});
