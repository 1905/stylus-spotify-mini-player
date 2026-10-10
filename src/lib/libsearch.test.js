import { describe, it, expect } from "vitest";
import { isNowRow } from "./libsearch.js";

describe("isNowRow", () => {
  it("compares the uri with the playing track", () => {
    expect(isNowRow("spotify:track:1", { uri: "spotify:track:1" })).toBe(true);
    expect(isNowRow("spotify:track:1", { uri: "spotify:track:2" })).toBe(false);
    expect(isNowRow("spotify:track:1", null)).toBe(false);
    expect(isNowRow("", { uri: "" })).toBe(false);
  });
});
