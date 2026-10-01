import { describe, it, expect } from "vitest";
import { fmtTime, esc } from "./format.js";

describe("fmtTime", () => {
  it.each([
    [0, "0:00"],
    [59000, "0:59"],
    [61000, "1:01"],
    [3600000, "1:00:00"],
    [null, "0:00"],
  ])("fmtTime(%s) = %s", (ms, want) => {
    expect(fmtTime(ms)).toBe(want);
  });
});

describe("esc", () => {
  it("escapes <>&\"'", () => {
    expect(esc(`<a href="x">Tom & Jerry's</a>`)).toBe(
      "&lt;a href=&quot;x&quot;&gt;Tom &amp; Jerry&#39;s&lt;/a&gt;",
    );
  });
  it("stringifies null/undefined to empty", () => {
    expect(esc(null)).toBe("");
    expect(esc(undefined)).toBe("");
  });
});
