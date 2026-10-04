import { describe, it, expect, vi, afterEach } from "vitest";
import { MINI_DRIFT_MS, miniPayload, miniChanged, miniProgress, volumeIcon } from "./mini.js";

const song = { name: "Intro", artists: "The xx", cover: "https://i.scdn.co/image/a", duration_ms: 128000, uri: "spotify:track:1" };
const base = { mode: "track", now: song, status: "", isPlaying: true, pending: false, skipping: null, loading: false, positionMs: 1000, volume: 60, heart: false, device: "Here" };

afterEach(() => vi.useRealTimers());

describe("miniPayload", () => {
  it("summarises a song", () => {
    vi.useFakeTimers({ now: 5000 });
    expect(miniPayload(base)).toEqual({
      mode: "track",
      title: "Intro",
      artist: "The xx",
      cover: "https://i.scdn.co/image/a",
      status: null,
      playing: true,
      pending: false,
      skipping: null,
      loading: false,
      positionMs: 1000,
      durationMs: 128000,
      sentAt: 5000,
      volume: 60,
      heart: false,
      device: "Here",
    });
  });

  it("shows the main window's headline when no song shows", () => {
    const p = miniPayload({ ...base, mode: "idle", now: null, status: "Nothing playing", isPlaying: true, volume: null, heart: null });
    expect(p).toMatchObject({ title: null, status: "Nothing playing", playing: false, durationMs: 0, volume: null, heart: null });
    // an ad: pausable, no song data
    expect(miniPayload({ ...base, mode: "other", status: "Playing on Kitchen" })).toMatchObject({ title: null, status: "Playing on Kitchen", playing: true });
  });

  it("a starting play shows its preview with the bar at rest", () => {
    const p = miniPayload({ ...base, mode: "idle", loading: true, pending: true, positionMs: 50000 });
    expect(p).toMatchObject({ title: "Intro", loading: true, pending: true, positionMs: 0 });
    expect(miniPayload({ ...base, skipping: "next", positionMs: 9000 }).positionMs).toBe(0);
  });
});

describe("miniChanged", () => {
  it("on any change but the position", () => {
    vi.useFakeTimers({ now: 0 });
    const a = miniPayload(base);
    expect(miniChanged(null, a)).toBe(true);
    expect(miniChanged(a, null)).toBe(true);
    expect(miniChanged(null, null)).toBe(false);
    expect(miniChanged(a, { ...a })).toBe(false);
    for (const [k, v] of [["playing", false], ["pending", true], ["skipping", "next"], ["volume", 61], ["heart", true], ["title", "B"], ["status", "x"]]) {
      expect(miniChanged(a, { ...a, [k]: v }), k).toBe(true);
    }
  });

  it("on a position jump only", () => {
    const a = { ...miniPayload(base), sentAt: 0 };
    // playing: 10 s later at +10 s is on track
    expect(miniChanged(a, { ...a, sentAt: 10000, positionMs: 11000 })).toBe(false);
    expect(miniChanged(a, { ...a, sentAt: 10000, positionMs: 11000 + MINI_DRIFT_MS + 1 })).toBe(true);
    // paused: the position holds
    const p = { ...a, playing: false };
    expect(miniChanged(p, { ...p, sentAt: 10000, positionMs: 1000 })).toBe(false);
    expect(miniChanged(p, { ...p, sentAt: 10000, positionMs: 60000 })).toBe(true);
  });
});

describe("miniProgress", () => {
  const s = { durationMs: 100000, positionMs: 10000, playing: true, sentAt: 0, loading: false, skipping: null };
  it("runs on while playing, holds while paused", () => {
    expect(miniProgress(s, 0)).toBe(0.1);
    expect(miniProgress(s, 40000)).toBe(0.5);
    expect(miniProgress({ ...s, playing: false }, 40000)).toBe(0.1);
    expect(miniProgress(s, 10 ** 9)).toBe(1);
    expect(miniProgress(s, -5000)).toBe(0.1); // a clock step back doesn't run it backwards
  });
  it("is empty with no song, while loading or skipping", () => {
    expect(miniProgress(null, 0)).toBe(0);
    expect(miniProgress({ ...s, durationMs: 0 }, 0)).toBe(0);
    expect(miniProgress({ ...s, loading: true }, 0)).toBe(0);
    expect(miniProgress({ ...s, skipping: "previous" }, 0)).toBe(0);
  });
});

describe("helpers", () => {
  it("volumeIcon", () => {
    expect([0, 1, 39, 40, 100].map(volumeIcon)).toEqual(["mute", "volumeLow", "volumeLow", "volumeHigh", "volumeHigh"]);
  });
});
