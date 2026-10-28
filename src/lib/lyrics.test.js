import { describe, it, expect } from "vitest";
import { lyricsActive, lineAt, preview } from "./lyrics.js";

const L = [
  { ms: 1000, text: "a" },
  { ms: 2500, text: "" },
  { ms: 4000, text: "c" },
];

describe("lyricsActive", () => {
  it("needs a placement and a song, in either mode", () => {
    expect(lyricsActive({ lyrics: "title" }, "track")).toBe(true);
    expect(lyricsActive({ lyrics: "cover" }, "track")).toBe(true);
    expect(lyricsActive({ lyrics: "off" }, "track")).toBe(false);
    for (const mode of ["other", "idle"]) expect(lyricsActive({ lyrics: "title" }, mode)).toBe(false);
  });
});

describe("lineAt", () => {
  it("is the last line that has started, -1 before the first", () => {
    expect(lineAt(L, 0)).toBe(-1);
    expect(lineAt(L, 999)).toBe(-1);
    expect(lineAt(L, 1000)).toBe(0);
    expect(lineAt(L, 2499)).toBe(0);
    expect(lineAt(L, 2500)).toBe(1);
    expect(lineAt(L, 99999)).toBe(2);
    expect(lineAt([], 5)).toBe(-1);
  });

  it("takes the previous answer as a hint and still answers right", () => {
    expect(lineAt(L, 2600, 1)).toBe(1); // the hint holds
    expect(lineAt(L, 4000, 1)).toBe(2); // the song moved on
    expect(lineAt(L, 500, 2)).toBe(-1); // a seek back
  });
});

describe("preview", () => {
  it("shows the line just sung, the current one and the next two, ♪ for the intro and the gaps", () => {
    expect(preview(L, -1)).toEqual(["", "♪", "a", "♪"]);
    expect(preview(L, 0)).toEqual(["", "a", "♪", "c"]);
    expect(preview(L, 1)).toEqual(["a", "♪", "c", ""]);
    expect(preview(L, 2)).toEqual(["♪", "c", "", ""]);
    expect(preview([], -1)).toEqual(["", "", "", ""]);
  });
});
