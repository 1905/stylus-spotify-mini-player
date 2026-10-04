import { describe, it, expect } from "vitest";
import { rubberBand, rubberRaw, RUBBER_K, RUBBER_CAP } from "./pan.js";

describe("rubberBand", () => {
  it("is 1:1 inside the limits", () => {
    for (const x of [-300, -1, 0, 50, 200]) expect(rubberBand(x, -300, 200)).toBe(x);
  });

  it("resists past either end, about RUBBER_K at first", () => {
    expect(rubberBand(210, -300, 200) - 200).toBeCloseTo(10 * RUBBER_K, 0);
    expect(-300 - rubberBand(-310, -300, 200)).toBeCloseTo(10 * RUBBER_K, 0);
    expect(rubberBand(260, -300, 200) - 200).toBeLessThan(60 * RUBBER_K);
  });

  it("never goes past the cap, and keeps growing toward it", () => {
    const a = rubberBand(1000, 0, 0);
    const b = rubberBand(100000, 0, 0);
    expect(a).toBeLessThan(RUBBER_CAP);
    expect(b).toBeLessThan(RUBBER_CAP);
    expect(b).toBeGreaterThan(a);
    expect(rubberBand(-100000, 0, 0)).toBeGreaterThan(-RUBBER_CAP);
  });

  it("rubberRaw undoes it", () => {
    for (const raw of [-900, -320, -300, 0, 200, 230, 700]) expect(rubberRaw(rubberBand(raw, -300, 200), -300, 200)).toBeCloseTo(raw, 6);
  });

  it("rubberRaw of a shown offset past the cap stays finite", () => {
    expect(Number.isFinite(rubberRaw(RUBBER_CAP + 50, 0, 0))).toBe(true);
  });
});
