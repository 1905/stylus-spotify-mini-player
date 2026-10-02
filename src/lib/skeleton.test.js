import { describe, it, expect } from "vitest";
import { skeletonRows, skeletonTiles } from "./skeleton.js";

const count = (html, re) => (html.match(re) || []).length;

describe("skeletonRows", () => {
  it("makes n hidden placeholder rows with art, title and sub", () => {
    const html = skeletonRows(8);
    expect(count(html, /class="row is-skeleton" aria-hidden="true"/g)).toBe(8);
    expect(count(html, /sk-art/g)).toBe(8);
    expect(count(html, /class="sk sk-line"/g)).toBe(8);
    expect(count(html, /sk-sub/g)).toBe(8);
  });
  it("0 or less → nothing", () => {
    expect(skeletonRows(0)).toBe("");
    expect(skeletonRows(-2)).toBe("");
  });
  it("titles vary in width", () => {
    const ws = [...skeletonRows(3).matchAll(/class="sk sk-line" style="width: (\d+)%"/g)].map((m) => m[1]);
    expect(new Set(ws).size).toBe(3);
  });
});

describe("skeletonTiles", () => {
  it("makes n hidden placeholder tiles", () => {
    const html = skeletonTiles(4);
    expect(count(html, /class="album is-skeleton" aria-hidden="true"/g)).toBe(4);
    expect(count(html, /class="art sk"/g)).toBe(4);
    expect(skeletonTiles(0)).toBe("");
  });
});
