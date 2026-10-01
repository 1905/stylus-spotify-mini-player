import { describe, it, expect } from "vitest";
import { buildRun, mergeHistory } from "./timeline.js";

const T = (n) => ({ id: "t" + n, uri: "spotify:track:" + n, name: "Song " + n, artists: "A", album: "B", cover: null, duration_ms: 1000 });
const H = (t, i = 0) => ({ track: t, played_at: `2026-10-01T10:${String(59 - i).padStart(2, "0")}:00Z` });
// history newest-first
const hist = (...ns) => ns.map((n, i) => H(T(n), i));

describe("buildRun", () => {
  it("empty input → []", () => {
    expect(buildRun({ history: [], now: null, queue: [] })).toEqual([]);
  });

  it("only now → 1 item at offset 0", () => {
    const run = buildRun({ history: [], now: T(1), queue: [] });
    expect(run).toHaveLength(1);
    expect(run[0]).toMatchObject({ role: "now", offset: 0, track: T(1) });
  });

  it("history of 6 → 4 past, offsets -4..-1, oldest leftmost", () => {
    const run = buildRun({ history: hist(6, 5, 4, 3, 2, 1), now: T(10), queue: [] });
    const past = run.filter((x) => x.role === "past");
    expect(past.map((x) => x.offset)).toEqual([-4, -3, -2, -1]);
    // newest-first 6,5,4,3 taken → reversed: 3,4,5,6
    expect(past.map((x) => x.track.uri)).toEqual([3, 4, 5, 6].map((n) => T(n).uri));
    expect(run.map((x) => x.role)).toEqual(["past", "past", "past", "past", "now"]);
  });

  it("history head equal to now is dropped", () => {
    const run = buildRun({ history: hist(1, 1, 2), now: T(1), queue: [] });
    const past = run.filter((x) => x.role === "past");
    expect(past.map((x) => x.track.uri)).toEqual([T(2).uri]);
  });

  it("past never repeats now, at any depth", () => {
    const run = buildRun({ history: hist(2, 1, 3), now: T(1), queue: [T(4)] });
    expect(run.map((x) => `${x.role}:${x.track.id}`)).toEqual(["past:t3", "past:t2", "now:t1", "next:t4"]);
  });

  it("queued repeats keep their past plays (a short playlist on repeat)", () => {
    const run = buildRun({ history: hist(3, 2, 1), now: T(1), queue: [T(2), T(3), T(1)] });
    expect(run.filter((x) => x.role === "past").map((x) => x.track.id)).toEqual(["t2", "t3"]);
  });

  it("consecutive duplicate uris collapse", () => {
    const run = buildRun({ history: hist(3, 3, 2, 2, 2, 1), now: T(9), queue: [] });
    const past = run.filter((x) => x.role === "past");
    expect(past.map((x) => x.track.uri)).toEqual([1, 2, 3].map((n) => T(n).uri));
  });

  it("queue of 12 → 8 next, offsets 1..8", () => {
    const queue = Array.from({ length: 12 }, (_, i) => T(100 + i));
    const run = buildRun({ history: [], now: T(1), queue });
    const next = run.filter((x) => x.role === "next");
    expect(next).toHaveLength(8);
    expect(next.map((x) => x.offset)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
    expect(next[0].track.uri).toBe(T(100).uri);
  });

  it("respects custom maxPast/maxNext", () => {
    const queue = Array.from({ length: 12 }, (_, i) => T(100 + i));
    const run = buildRun({ history: hist(6, 5, 4), now: T(1), queue }, { maxPast: 2, maxNext: 4 });
    expect(run.filter((x) => x.role === "past")).toHaveLength(2);
    expect(run.filter((x) => x.role === "next")).toHaveLength(4);
  });

  it("now null → past and next still returned, no now item", () => {
    const run = buildRun({ history: hist(2, 1), now: null, queue: [T(5)] });
    expect(run.map((x) => x.role)).toEqual(["past", "past", "next"]);
  });

  it("keys stay stable across a track change", () => {
    const before = { history: hist(3, 2, 1), now: T(4), queue: [T(5), T(6), T(7)] };
    const a = buildRun(before);
    const after = {
      history: [H(T(4), -1), ...before.history],
      now: before.queue[0],
      queue: before.queue.slice(1),
    };
    const b = buildRun(after);
    const keyOf = (run) => new Map(run.map((x) => [x.track.uri, x.key]));
    const ka = keyOf(a);
    const kb = keyOf(b);
    const shared = [...ka.keys()].filter((u) => kb.has(u));
    expect(shared.length).toBeGreaterThanOrEqual(5);
    for (const u of shared) expect(kb.get(u)).toBe(ka.get(u));
    // roles moved as expected
    expect(b.find((x) => x.track.uri === T(4).uri).role).toBe("past");
    expect(b.find((x) => x.track.uri === T(5).uri).role).toBe("now");
  });

  it("repeated uri in the display list gets ~k suffix", () => {
    const run = buildRun({ history: [], now: T(2), queue: [T(1), T(3), T(1)] });
    expect(run.map((x) => x.key)).toEqual([`${T(2).uri}~0`, `${T(1).uri}~0`, `${T(3).uri}~0`, `${T(1).uri}~1`]);
  });
});

describe("mergeHistory", () => {
  const at = (min) => `2026-10-01T10:${String(min).padStart(2, "0")}:00.000Z`;
  const row = (n, min) => ({ track: T(n), played_at: at(min) });
  const W = 2 * 60 * 1000; // app.js SESSION_MATCH_MS

  it("a session play the API already has is dropped; the rest is newest first", () => {
    const out = mergeHistory([row(1, 5)], [row(2, 8), row(1, 5)], W);
    expect(out.map((r) => r.track.id + "@" + r.played_at.slice(14, 16))).toEqual(["t2@08", "t1@05"]);
  });

  it("matching is one-to-one by closest time: a replay is kept until the API has it", () => {
    // B played at 10:03 (API and session), B again at 10:09 (session only, API lagging)
    const out = mergeHistory([row(2, 3), row(3, 6)], [row(2, 9), row(2, 3)], W);
    expect(out.map((r) => r.track.id + "@" + r.played_at.slice(14, 16))).toEqual(["t2@09", "t3@06", "t2@03"]);
  });

  it("an earlier API play doesn't eat a new replay (played_at is the end of a play)", () => {
    // API: A ended 10:00, B 10:04. The app saw A end again at 10:08; the API lags.
    const out = mergeHistory([row(2, 4), row(1, 0)], [row(1, 8)], 2 * 60 * 1000);
    expect(out.map((r) => r.track.id + "@" + r.played_at.slice(14, 16))).toEqual(["t1@08", "t2@04", "t1@00"]);
  });

  it("a session play far from any API play is kept", () => {
    const out = mergeHistory([row(1, 0)], [row(1, 40)], W);
    expect(out).toHaveLength(2);
  });
});
