import { describe, it, expect } from "vitest";
import { favoritesBy } from "./favorites.js";

const T = (n, ...artists) => ({ uri: `spotify:track:${n}`, name: `S${n}`, artist_list: artists.map((id) => ({ id, name: id })) });

describe("favoritesBy", () => {
  it("keeps only tracks with the artist, in list order", () => {
    const out = favoritesBy("A", [[T(1, "A"), T(2, "B")], [T(3, "B", "A")]]);
    expect(out.map((t) => t.uri)).toEqual(["spotify:track:1", "spotify:track:3"]);
  });

  it("dedupes by uri, first list wins", () => {
    const out = favoritesBy("A", [[T(1, "A")], [T(1, "A"), T(2, "A")]]);
    expect(out.map((t) => t.uri)).toEqual(["spotify:track:1", "spotify:track:2"]);
  });

  it("caps at max and tolerates missing lists and fields", () => {
    const many = Array.from({ length: 15 }, (_, i) => T(i, "A"));
    expect(favoritesBy("A", [null, many, undefined], 10)).toHaveLength(10);
    expect(favoritesBy("A", [[{ uri: "x" }, null]])).toEqual([]);
  });
});
