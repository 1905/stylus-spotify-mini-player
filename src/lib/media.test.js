import { describe, it, expect } from "vitest";
import { mediaPayload, mediaChanged, mediaAction, MEDIA_DRIFT_MS } from "./media.js";

const T = { uri: "spotify:track:1", name: "Intro", artists: "The xx", album: "xx", cover: "c.jpg", duration_ms: 128000 };

describe("mediaPayload", () => {
  it("is null when idle", () => {
    expect(mediaPayload({ mode: "idle", now: null, isPlaying: false, positionMs: 0 })).toBeNull();
  });

  it("carries the song", () => {
    expect(mediaPayload({ mode: "track", now: T, isPlaying: true, positionMs: 1234.6 })).toEqual({
      title: "Intro", artist: "The xx", album: "xx", cover: "c.jpg", durationMs: 128000, positionMs: 1235, playing: true,
    });
  });

  it("an ad or podcast: only the play state", () => {
    expect(mediaPayload({ mode: "other", now: null, isPlaying: true, positionMs: 0 })).toEqual({
      title: null, artist: null, album: null, cover: null, durationMs: null, positionMs: null, playing: true,
    });
  });
});

describe("mediaChanged", () => {
  const p = (over = {}) => ({ ...mediaPayload({ mode: "track", now: T, isPlaying: true, positionMs: 10000 }), ...over });

  it("null ↔ payload", () => {
    expect(mediaChanged(null, null, 0)).toBe(false);
    expect(mediaChanged(null, p(), 0)).toBe(true);
    expect(mediaChanged(p(), null, 0)).toBe(true);
  });

  it("not on a normal tick", () => {
    expect(mediaChanged(p(), p({ positionMs: 11000 }), 1000)).toBe(false);
    expect(mediaChanged(p({ playing: false }), p({ playing: false, positionMs: 10000 }), 5000)).toBe(false);
  });

  it("on a track or play-state change", () => {
    expect(mediaChanged(p(), p({ title: "VCR" }), 1000)).toBe(true);
    expect(mediaChanged(p(), p({ playing: false }), 1000)).toBe(true);
  });

  it("on a seek (position jump)", () => {
    expect(mediaChanged(p(), p({ positionMs: 11000 + MEDIA_DRIFT_MS + 1 }), 1000)).toBe(true);
    expect(mediaChanged(p(), p({ positionMs: 0 }), 1000)).toBe(true);
  });
});

describe("mediaAction", () => {
  it("toggle always toggles", () => {
    expect(mediaAction({ action: "toggle" }, true)).toBe("toggle");
    expect(mediaAction({ action: "toggle" }, false)).toBe("toggle");
  });

  it("play/pause act only when they change something", () => {
    expect(mediaAction({ action: "play" }, false)).toBe("toggle");
    expect(mediaAction({ action: "play" }, true)).toBeNull();
    expect(mediaAction({ action: "pause" }, true)).toBe("toggle");
    expect(mediaAction({ action: "pause" }, false)).toBeNull();
  });

  it("next / previous", () => {
    expect(mediaAction({ action: "next" }, true)).toBe("next_track");
    expect(mediaAction({ action: "previous" }, true)).toBe("previous_track");
  });

  it("seek needs a valid position", () => {
    expect(mediaAction({ action: "seek", positionMs: 5000 }, true)).toEqual({ seek: 5000 });
    expect(mediaAction({ action: "seek" }, true)).toBeNull();
    expect(mediaAction({ action: "seek", positionMs: -1 }, true)).toBeNull();
  });

  it("unknown or missing", () => {
    expect(mediaAction({ action: "stop" }, true)).toBeNull();
    expect(mediaAction(null, true)).toBeNull();
  });
});
