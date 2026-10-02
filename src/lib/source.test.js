import { describe, it, expect } from "vitest";
import { originUri, offsettable } from "./source.js";

describe("originUri / offsettable", () => {
  it("builds playlist and album uris only", () => {
    expect(originUri({ kind: "playlist", id: "p1" })).toBe("spotify:playlist:p1");
    expect(originUri({ kind: "album", id: "a" })).toBe("spotify:album:a");
    expect(originUri({ kind: "liked", id: "liked" })).toBeNull();
    expect(originUri(null)).toBeNull();
  });
  it("only playlists and albums take an offset", () => {
    expect(offsettable("spotify:playlist:x")).toBe(true);
    expect(offsettable("spotify:album:x")).toBe(true);
    expect(offsettable("spotify:artist:x")).toBe(false);
    expect(offsettable(null)).toBe(false);
  });
});
