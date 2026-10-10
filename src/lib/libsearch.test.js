import { describe, it, expect } from "vitest";
import { fold, matches, matchIndexes, searchLibrary, isNowRow } from "./libsearch.js";

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

describe("searchLibrary", () => {
  const mia = { source: { kind: "playlist", id: "p1", name: "Mia" }, tracks: [T(1, "Toxic", "Britney"), T(2, "Golden"), T(1, "Toxic", "Britney")] };
  const liked = { source: { kind: "liked", id: null, name: "Liked Songs" }, tracks: [T(1, "Toxic", "Britney")] };
  const xx = { source: { kind: "album", id: "a1", name: "xx" }, tracks: [T(9, "Intro", "The xx")] };
  const index = {
    playlists: [{ id: "p1", name: "Mia", owner: { display_name: "Kass" } }, { id: "p2", name: "Toxic Vibes" }],
    albums: [{ id: "a1", name: "xx", artists: "The xx" }],
    artists: [{ id: "r1", name: "Britney Spears" }],
    lists: [liked, mia, xx],
  };

  it("groups by kind, a song once per list, with its list and index", () => {
    const r = searchLibrary("toxic", index);
    expect(r.playlists.map((p) => p.id)).toEqual(["p2"]);
    expect(r.albums).toEqual([]);
    expect(r.songs.map((s) => [s.list.source.name, s.i])).toEqual([
      ["Liked Songs", 0],
      ["Mia", 0],
    ]);
  });

  it("lists match name and owner; artists by name", () => {
    expect(searchLibrary("kass", index).playlists.map((p) => p.id)).toEqual(["p1"]);
    expect(searchLibrary("britney", index).artists.map((a) => a.id)).toEqual(["r1"]);
  });

  it("an album's songs match the album name", () => {
    const r = searchLibrary("xx intro", index);
    expect(r.songs.map((s) => s.track.uri)).toEqual(["spotify:track:9"]);
    expect(r.albums).toEqual([]); // "intro" isn't in the album's name or artists
    expect(searchLibrary("the xx", index).albums.map((a) => a.id)).toEqual(["a1"]);
  });

  it("caps songs and the other kinds", () => {
    const many = { source: { kind: "playlist", id: "big", name: "Big" }, tracks: Array.from({ length: 80 }, (_, i) => T(i, `Song ${i}`)) };
    const pls = Array.from({ length: 30 }, (_, i) => ({ id: `p${i}`, name: `Song list ${i}` }));
    const r = searchLibrary("song", { playlists: pls, lists: [many, mia] });
    expect(r.songs).toHaveLength(50);
    expect(r.playlists).toHaveLength(20);
    expect(searchLibrary("song", { lists: [many] }, { songs: 3, other: 1 }).songs).toHaveLength(3);
  });

  it("an empty query or no index finds nothing", () => {
    expect(searchLibrary(" ", index)).toEqual({ playlists: [], albums: [], artists: [], songs: [] });
    expect(searchLibrary("x", null).songs).toEqual([]);
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
