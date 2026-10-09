import { describe, it, expect } from "vitest";
import { coverSrc } from "./cover.js";

describe("coverSrc", () => {
  it.each([
    "https://i.scdn.co/image/ab67616d0000b273abc",
    "https://mosaic.scdn.co/640/ab67616d0000b273abc",
    "https://image-cdn-ak.spotifycdn.com/image/ab67706c0000da84abc",
  ])("CDN URL %s → cover://", (url) => {
    expect(coverSrc(url)).toBe(`cover://localhost/${encodeURIComponent(url)}`);
  });

  it("encodes the whole URL into one path segment", () => {
    expect(coverSrc("https://i.scdn.co/image/x?y=1")).toBe("cover://localhost/https%3A%2F%2Fi.scdn.co%2Fimage%2Fx%3Fy%3D1");
  });

  it.each([
    ["data:image/png;base64,AAAA"],
    [""],
    [null],
    [undefined],
    ["http://i.scdn.co/image/abc"],
    ["https://example.com/image.jpg"],
    ["https://scdn.co.evil.com/image.jpg"],
    ["https://notscdn.co/image.jpg"],
    ["https://i.scdn.co:8443/image/abc"],
    ["cover://localhost/https%3A%2F%2Fi.scdn.co%2Fimage%2Fabc"],
  ])("%s → unchanged", (url) => {
    expect(coverSrc(url)).toBe(url);
  });
});
