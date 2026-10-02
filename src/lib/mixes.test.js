import { describe, it, expect } from "vitest";
import { noteMixes, MAX_MIXES } from "./mixes.js";

const T0 = "2026-10-01T10:00:00.000Z";
const T1 = "2026-10-02T10:00:00.000Z";
const pl = (id) => `spotify:playlist:${id}`;

describe("noteMixes", () => {
  it("adds new playlist contexts, newest first", () => {
    expect(noteMixes([], [pl("a"), pl("b")], [], T1)).toEqual([
      { id: "a", seen: T1 },
      { id: "b", seen: T1 },
    ]);
  });

  it("ignores albums, artists, collections and empty values", () => {
    const uris = ["spotify:album:x", "spotify:artist:y", "spotify:user:me:collection", null, "", undefined];
    expect(noteMixes([], uris, [], T1)).toEqual([]);
  });

  it("leaves out the user's own playlists, also ones already known", () => {
    const known = [{ id: "own", seen: T0 }, { id: "m", seen: T0 }];
    expect(noteMixes(known, [pl("own"), pl("x")], ["own"], T1)).toEqual([
      { id: "x", seen: T1 },
      { id: "m", seen: T0 },
    ]);
  });

  it("moves a known mix to the front with a new seen time, without a duplicate", () => {
    const known = [{ id: "a", seen: T0 }, { id: "b", seen: T0 }];
    expect(noteMixes(known, [pl("b")], [], T1)).toEqual([
      { id: "b", seen: T1 },
      { id: "a", seen: T0 },
    ]);
  });

  it("dedupes repeats within one call", () => {
    expect(noteMixes([], [pl("a"), pl("a"), pl("b"), pl("a")], [], T1)).toEqual([
      { id: "a", seen: T1 },
      { id: "b", seen: T1 },
    ]);
  });

  it("caps the list at 30, dropping the oldest", () => {
    const known = Array.from({ length: MAX_MIXES }, (_, i) => ({ id: `k${i}`, seen: T0 }));
    const out = noteMixes(known, [pl("new")], [], T1);
    expect(MAX_MIXES).toBe(30);
    expect(out).toHaveLength(30);
    expect(out[0]).toEqual({ id: "new", seen: T1 });
    expect(out.at(-1).id).toBe("k28");
  });

  it("returns the same items when nothing new was seen", () => {
    const known = [{ id: "a", seen: T0 }];
    expect(noteMixes(known, [null, "spotify:album:x"], [], T1)).toEqual(known);
  });

  it("survives broken stored data", () => {
    expect(noteMixes(null, [pl("a")], null, T1)).toEqual([{ id: "a", seen: T1 }]);
    expect(noteMixes([null, { seen: T0 }, { id: "b", seen: T0 }], [], new Set(), T1)).toEqual([{ id: "b", seen: T0 }]);
  });

  it("accepts own ids as a Set", () => {
    expect(noteMixes([], [pl("own")], new Set(["own"]), T1)).toEqual([]);
  });
});
