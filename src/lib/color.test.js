import { describe, it, expect } from "vitest";
import { pickColors, FALLBACK } from "./color.js";

const pixels = (list) => Uint8ClampedArray.from(list.flatMap(([r, g, b]) => [r, g, b, 255]));
const greys = (n) => Array.from({ length: n }, (_, i) => [40 + i * 10, 40 + i * 10, 40 + i * 10]);

// HSL lightness in 0..1
const lightness = ([r, g, b]) => (Math.max(r, g, b) + Math.min(r, g, b)) / 2 / 255;

describe("pickColors", () => {
  it("all-grey image falls back", () => {
    const c = pickColors(pixels(greys(16)));
    expect(c.vivid).toEqual(FALLBACK.vivid);
    expect(lightness(c.ink)).toBeLessThanOrEqual(0.1);
  });

  it("one saturated red pixel among greys wins vivid", () => {
    const c = pickColors(pixels([...greys(15), [220, 30, 30]]));
    const [r, g, b] = c.vivid;
    expect(r).toBeGreaterThan(180);
    expect(g).toBeLessThan(60);
    expect(b).toBeLessThan(60);
  });

  it("ink is dark (lightness <= 10%) and keeps the red hue", () => {
    const c = pickColors(pixels([...greys(15), [220, 30, 30]]));
    expect(lightness(c.ink)).toBeLessThanOrEqual(0.1);
    const [r, g, b] = c.ink;
    expect(r).toBeGreaterThan(g);
    expect(r).toBeGreaterThan(b);
  });

  it("ignores transparent pixels", () => {
    const data = pixels(greys(4));
    const red = Uint8ClampedArray.from([...data, 255, 0, 0, 0]);
    expect(pickColors(red).vivid).toEqual(FALLBACK.vivid);
  });
});
