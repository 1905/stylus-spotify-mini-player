import { describe, it, expect } from "vitest";
import { srgbToOklab, oklchToSrgb, toLch, contrast } from "./color.js";

describe("OKLab", () => {
  it("white is L 1, black is L 0, both neutral", () => {
    const w = srgbToOklab([255, 255, 255]);
    const k = srgbToOklab([0, 0, 0]);
    expect(w[0]).toBeCloseTo(1, 3);
    expect(k[0]).toBeCloseTo(0, 6);
    for (const v of [w[1], w[2], k[1], k[2]]) expect(Math.abs(v)).toBeLessThan(1e-3);
  });

  it("matches reference values (sRGB red, Ottosson)", () => {
    const [L, a, b] = srgbToOklab([255, 0, 0]);
    expect(L).toBeCloseTo(0.628, 3);
    expect(a).toBeCloseTo(0.2249, 3);
    expect(b).toBeCloseTo(0.1258, 3);
  });

  it("round-trips through OKLCH within 1/255", () => {
    for (const rgb of [[12, 200, 90], [250, 240, 10], [70, 60, 200], [128, 128, 128]]) {
      const back = oklchToSrgb(toLch(srgbToOklab(rgb)));
      back.forEach((v, i) => expect(Math.abs(v - rgb[i])).toBeLessThanOrEqual(1));
    }
  });

  it("cuts chroma, not lightness or hue, to fit sRGB", () => {
    const h = Math.atan2(-0.3, -0.05);
    const rgb = oklchToSrgb([0.6, 0.4, h]); // far out of gamut
    const [L, C, hh] = toLch(srgbToOklab(rgb));
    expect(L).toBeCloseTo(0.6, 2);
    expect(C).toBeLessThan(0.4);
    expect(Math.abs(hh - h)).toBeLessThan(0.05);
  });

  it("WCAG contrast: black on white is 21, same colour is 1", () => {
    expect(contrast([0, 0, 0], [255, 255, 255])).toBeCloseTo(21, 5);
    expect(contrast([90, 30, 10], [90, 30, 10])).toBe(1);
  });
});
