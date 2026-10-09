import { describe, it, expect } from "vitest";
import { originUri, offsettable, restoredSource } from "./source.js";

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

describe("restoredSource", () => {
  it("keeps a saved track, else the first uri of a list", () => {
    expect(restoredSource({ contextUri: "spotify:playlist:p", uris: null, trackUri: "spotify:track:b" })).toEqual({ contextUri: "spotify:playlist:p", uris: null, trackUri: "spotify:track:b" });
    expect(restoredSource({ contextUri: null, uris: ["spotify:track:a", "spotify:track:b"], trackUri: null })).toEqual({ contextUri: null, uris: ["spotify:track:a", "spotify:track:b"], trackUri: "spotify:track:a" });
  });
  it("a finished context has no track yet: still a source", () => {
    expect(restoredSource({ contextUri: "spotify:user:u:collection", uris: null, trackUri: null, finished: true })).toEqual({ contextUri: "spotify:user:u:collection", uris: null, trackUri: null });
  });
  it("nothing usable: null", () => {
    expect(restoredSource(null)).toBeNull();
    expect(restoredSource({})).toBeNull();
    expect(restoredSource({ contextUri: "", uris: [], trackUri: "" })).toBeNull();
  });
});
