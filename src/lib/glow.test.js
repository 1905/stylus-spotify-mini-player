import { describe, it, expect } from "vitest";
import { srgbToOklab, toLch } from "./color.js";
import {
  samplePixels, kmeans, palette, composeGlow, pickAccents, toneBlob, toneInk, cellOf,
  brightest, textSpot, glowVars, fallbackVars, falloff, BLOB_L, INK_L, MIN_CONTRAST, TEXT_CONTRAST,
} from "./glow.js";

const W = 32;
/** a W×W rgba image from fn(x, y) → [r, g, b] with x, y in 0..1 */
const image = (fn) => {
  const out = new Uint8ClampedArray(W * W * 4);
  for (let y = 0; y < W; y++) {
    for (let x = 0; x < W; x++) {
      const [r, g, b] = fn((x + 0.5) / W, (y + 0.5) / W);
      out.set([r, g, b, 255], (y * W + x) * 4);
    }
  }
  return out;
};
const WHITE = [245, 246, 248], BLUE = [40, 120, 230], BRICK = [170, 70, 50], BLACK = [8, 8, 10];
/** Younger Brother-like: white wall, a blue strip on the right, brick along the bottom */
const wallSkyBrick = image((x, y) => (y > 0.88 ? BRICK : x > 0.82 ? BLUE : WHITE));
const lab = (rgb) => toLch(srgbToOklab(rgb));
const isBlue = ([r, g, b]) => b > r + 40 && b > g;

describe("samplePixels / cellOf", () => {
  it("skips transparent pixels and keeps positions in 0..1", () => {
    const img = image(() => WHITE);
    img[3] = 0; // first pixel transparent
    const pts = samplePixels(img, W, W);
    expect(pts).toHaveLength(W * W - 1);
    expect(pts[0].x).toBeCloseTo(1.5 / W);
    expect(pts.every((p) => p.x > 0 && p.x < 1 && p.y > 0 && p.y < 1)).toBe(true);
  });

  it("maps positions to a 3×3 grid, row by row", () => {
    expect(cellOf(0.1, 0.1)).toBe(0);
    expect(cellOf(0.9, 0.1)).toBe(2);
    expect(cellOf(0.5, 0.5)).toBe(4);
    expect(cellOf(0.99, 0.99)).toBe(8);
  });
});

describe("kmeans", () => {
  it("is deterministic on a fixed pixel array", () => {
    const img = image((x, y) => [Math.round(x * 255), Math.round(y * 255), 128]);
    const a = kmeans(samplePixels(img, W, W));
    const b = kmeans(samplePixels(img, W, W));
    expect(a).toEqual(b);
    expect(a.length).toBeGreaterThan(3);
  });

  it("finds exactly the distinct colours of a flat 3-colour image, with populations", () => {
    const cl = kmeans(samplePixels(wallSkyBrick, W, W));
    expect(cl).toHaveLength(3);
    expect(cl.reduce((s, c) => s + c.n, 0)).toBe(W * W);
    expect(cl[0].n).toBeGreaterThan(cl[1].n); // largest first: the wall
  });
});

describe("palette", () => {
  it("accent weighting: a blue strip beats a white wall 4× its size", () => {
    const pal = palette(wallSkyBrick, W, W);
    expect(isBlue(pal.colors[0].rgb)).toBe(true);
    expect(pal.colors[0].share).toBeLessThan(0.2);
    const white = pal.colors.find((c) => c.L > 0.95);
    expect(white.share).toBeGreaterThan(0.6);
    expect(white.score).toBeLessThan(pal.colors[0].score / 5);
    expect(pal.mono).toBe(false);
  });

  it("regions: the right column belongs to the blue, the bottom row to the brick", () => {
    const pal = palette(wallSkyBrick, W, W);
    const blue = pal.colors.findIndex((c) => isBlue(c.rgb));
    const brick = pal.colors.findIndex((c) => c.rgb[0] > 140 && c.rgb[2] < 90);
    expect(pal.regions[2]).toBe(blue);
    expect(pal.regions[5]).toBe(blue);
    expect([6, 7].map((i) => pal.regions[i])).toEqual([brick, brick]);
    expect(pal.colors[blue].x).toBeGreaterThan(0.8);
  });

  it("a greys-only cover is monochrome", () => {
    const pal = palette(image((x) => (x < 0.5 ? BLACK : [200, 200, 200])), W, W);
    expect(pal.mono).toBe(true);
  });
});

describe("composeGlow", () => {
  it("puts the blue on the right and the brick below", () => {
    const g = composeGlow(palette(wallSkyBrick, W, W));
    const blue = g.blobs.find((b) => isBlue(b.rgb));
    expect(blue).toBeTruthy();
    expect(blue.x).toBeGreaterThan(0.5);
    const brick = g.blobs.find((b) => b.rgb[0] > b.rgb[2] + 40);
    expect(brick.y).toBeGreaterThan(0.5);
  });

  it("monochrome cover → a neutral glow", () => {
    const g = composeGlow(palette(image((x) => (x < 0.5 ? BLACK : WHITE)), W, W));
    expect(g.mono).toBe(true);
    for (const b of g.blobs) expect(lab(b.rgb)[1]).toBeLessThan(0.03);
    expect(lab(g.ink)[1]).toBeLessThan(0.015);
  });

  it("keeps --fg readable: ≥ 4.5:1 at the brightest point, ≥ 7:1 over the text band", () => {
    const covers = [
      wallSkyBrick,
      image(() => [255, 230, 0]), // all bright yellow
      image(() => [255, 255, 255]),
      image((x) => (x < 0.5 ? [255, 0, 200] : [0, 255, 230])), // neon halves
      image((x, y) => [Math.round(x * 255), 255, Math.round(y * 255)]),
    ];
    for (const img of covers) {
      const g = composeGlow(palette(img, W, W));
      expect(brightest(g).contrast).toBeGreaterThanOrEqual(MIN_CONTRAST);
      expect(textSpot(g).contrast).toBeGreaterThanOrEqual(TEXT_CONTRAST);
    }
  });

  it("picks at most 3 blobs and every alpha stays in 0..1", () => {
    const g = composeGlow(palette(image((x, y) => [Math.round(x * 255), Math.round(y * 255), 140]), W, W));
    expect(g.blobs.length).toBeGreaterThan(0);
    expect(g.blobs.length).toBeLessThanOrEqual(3);
    for (const b of [...g.blobs, g.wash]) {
      expect(b.a).toBeGreaterThan(0);
      expect(b.a).toBeLessThanOrEqual(1);
    }
  });
});

describe("tone-mapping", () => {
  it("blob lightness lands in BLOB_L and the hue survives, for dark, mid and pastel inputs", () => {
    for (const rgb of [[40, 10, 60], [200, 40, 40], [230, 200, 250], [20, 90, 30]]) {
      const [L, C, h] = lab(rgb);
      const [L2, , h2] = lab(toneBlob({ L, C, h }));
      expect(L2).toBeGreaterThanOrEqual(BLOB_L[0] - 0.01);
      expect(L2).toBeLessThanOrEqual(BLOB_L[1] + 0.01);
      expect(Math.abs(Math.atan2(Math.sin(h2 - h), Math.cos(h2 - h)))).toBeLessThan(0.08);
    }
  });

  it("ink is dark and low-chroma whatever the accent", () => {
    for (const rgb of [[255, 0, 0], [0, 255, 0], [30, 60, 255]]) {
      const [L, C, h] = lab(rgb);
      const [Li, Ci] = lab(toneInk({ L, C, h }));
      expect(Li).toBeCloseTo(INK_L, 1);
      expect(Ci).toBeLessThanOrEqual(0.046);
    }
  });

  it("falloff runs 1 → 0 and is smooth at both ends", () => {
    expect(falloff(0)).toBe(1);
    expect(falloff(0.5)).toBeCloseTo(0.5);
    expect(falloff(1)).toBe(0);
    expect(falloff(0.02)).toBeGreaterThan(0.99);
    expect(falloff(0.98)).toBeLessThan(0.01);
  });
});

describe("pickAccents", () => {
  const c = (h, score) => ({ h, score, L: 0.6, C: 0.15 });
  it("prefers a new hue over a near twin of a taken one", () => {
    const deg = Math.PI / 180;
    const picks = pickAccents([c(40 * deg, 1), c(70 * deg, 0.8), c(200 * deg, 0.5)]);
    expect(picks.map((p) => Math.round(p.h / deg))).toEqual([40, 200, 70]);
  });

  it("drops colours under 28 % of the best score", () => {
    expect(pickAccents([c(0, 1), c(2, 0.2)])).toHaveLength(1);
  });
});

describe("glowVars", () => {
  it("emits every property .bg reads, unused blobs at alpha 0", () => {
    const v = fallbackVars({ vivid: [139, 124, 240], ink: [21, 19, 27] });
    expect(v["--i"]).toBe("21 19 27");
    expect(v["--v"]).toBe("139 124 240");
    expect(v["--wa"]).toBe("0.220");
    for (const n of [1, 2, 3]) {
      expect(v[`--g${n}a`]).toBe("0");
      for (const k of ["", "x", "y", "r"]) expect(v[`--g${n}${k}`]).toBeDefined();
    }
    const g = glowVars(composeGlow(palette(wallSkyBrick, W, W)));
    expect(Number(g["--g1a"])).toBeGreaterThan(0);
  });
});

describe("pickAccents twins", () => {
  it("skips a shade that tone-maps onto a taken colour", () => {
    const deg = Math.PI / 180;
    const c = (h, score) => ({ h, score, L: 0.6, C: 0.15 });
    expect(pickAccents([c(40 * deg, 1), c(48 * deg, 0.9)])).toHaveLength(1);
  });
});
