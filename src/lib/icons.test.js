import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { ICONS } from "./icons.js";

describe("icons", () => {
  const all = Object.entries(ICONS);

  it("are 24×24 svgs, hidden from screen readers, named by class", () => {
    expect(all.length).toBeGreaterThan(15);
    for (const [name, svg] of all) {
      expect(svg, name).toMatch(/^<svg class="ic ic-[a-z-]+" viewBox="0 0 24 24" aria-hidden="true" [^>]*>.+<\/svg>$/);
      expect(svg.match(/<svg/g), name).toHaveLength(1);
      expect(svg, name).toMatch(/^<svg [^>]*fill="(none|currentColor)"/); // the global css sets no fill
    }
    const classes = all.map(([, s]) => /class="ic (ic-[a-z-]+)"/.exec(s)[1]);
    expect(new Set(classes).size).toBe(classes.length);
  });

  it("have the cover's info and close glyphs", () => {
    expect(ICONS.info).toContain('class="ic ic-info"');
    expect(ICONS.close).toContain('class="ic ic-close"');
  });

  it("are the only svgs in index.html (one source of truth)", () => {
    const html = readFileSync(new URL("../index.html", import.meta.url), "utf8");
    const used = html.match(/<svg\b.*?<\/svg>/gs) || [];
    expect(used.length).toBeGreaterThan(10);
    const known = new Set(Object.values(ICONS));
    for (const s of used) expect(known.has(s), s.slice(0, 60)).toBe(true);
  });
});
