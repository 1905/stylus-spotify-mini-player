import { describe, it, expect } from "vitest";
import { pageOffsets, foldPages } from "./paging.js";

const ok = (items, has_more = true) => ({ status: "fulfilled", value: { items, has_more } });
const fail = (reason) => ({ status: "rejected", reason });
const key = (t) => t.uri;
const tr = (n) => ({ uri: `u${n}` });
const page = (from) => Array.from({ length: 10 }, (_, i) => tr(from + i));

describe("pageOffsets", () => {
  it("n pages of 10 from an offset", () => {
    expect(pageOffsets(0, 3)).toEqual([0, 10, 20]);
    expect(pageOffsets(10, 2)).toEqual([10, 20]);
  });
  it("stops at the 1000 cap", () => {
    expect(pageOffsets(980, 3)).toEqual([980, 990]);
    expect(pageOffsets(1000, 3)).toEqual([]);
  });
});

describe("foldPages", () => {
  it("adds every page in order and points at the next one", () => {
    const r = foldPages([], [0, 10, 20], [ok(page(0)), ok(page(10)), ok(page(20))], key);
    expect(r.added.map(key)).toEqual(page(0).concat(page(10), page(20)).map(key));
    expect(r).toMatchObject({ next: 30, hasMore: true, error: null });
  });
  it("stops at the last page and ignores the ones after it", () => {
    const r = foldPages([], [0, 10, 20], [ok(page(0)), ok([tr(10)], false), ok(page(20))], key);
    expect(r.added.length).toBe(11);
    expect(r).toMatchObject({ next: 20, hasMore: false });
  });
  it("a failed page keeps the earlier ones and retries from itself", () => {
    const r = foldPages([], [0, 10, 20], [ok(page(0)), fail("boom"), ok(page(20))], key);
    expect(r.added.length).toBe(10);
    expect(r).toMatchObject({ next: 10, hasMore: true, error: "boom" });
  });
  it("a failed first page adds nothing", () => {
    const r = foldPages([], [30], [fail("x")], key);
    expect(r).toMatchObject({ added: [], next: 30, hasMore: true, error: "x" });
  });
  it("drops repeats, nulls and items already listed", () => {
    const r = foldPages([tr(1)], [10], [ok([tr(1), null, tr(2), tr(2), {}])], key);
    expect(r.added).toEqual([tr(2)]);
  });
  it("no more pages past the cap", () => {
    expect(foldPages([], [990], [ok(page(990))], key).hasMore).toBe(false);
  });
});
