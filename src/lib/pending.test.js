import { describe, it, expect } from "vitest";
import { createPending } from "./pending.js";

const T = "spotify:track:1";

describe("createPending", () => {
  it("resolves on a poll after landing that plays the expected track", () => {
    const p = createPending();
    const tok = p.start("list", { trackUri: T });
    p.landed(tok, 100);
    expect(p.onPoll({ isPlaying: true, trackUri: T, at: 150 })).toMatchObject({ token: tok, kind: "list" });
    expect(p.current()).toBeNull();
  });

  it("not before the command landed, nor from a poll that started earlier", () => {
    const p = createPending();
    const tok = p.start("list", { trackUri: T });
    expect(p.onPoll({ isPlaying: true, trackUri: T, at: 50 })).toBeNull();
    p.landed(tok, 100);
    expect(p.onPoll({ isPlaying: true, trackUri: T, at: 99 })).toBeNull();
    expect(p.current()).not.toBeNull();
  });

  it("waits while paused or on another track", () => {
    const p = createPending();
    p.landed(p.start("cover", { trackUri: T }), 0);
    expect(p.onPoll({ isPlaying: false, trackUri: T, at: 1 })).toBeNull();
    expect(p.onPoll({ isPlaying: true, trackUri: "spotify:track:2", at: 1 })).toBeNull();
    expect(p.onPoll({ isPlaying: true, trackUri: T, at: 2 })).not.toBeNull();
  });

  it("no expected track: any playing poll resolves it", () => {
    const p = createPending();
    p.landed(p.start("mix"), 0);
    expect(p.onPoll({ isPlaying: true, trackUri: "spotify:track:9", at: 1 })).toMatchObject({ kind: "mix" });
  });

  it("a resume loads paused: the track alone resolves it", () => {
    const p = createPending();
    p.landed(p.start("resume", { trackUri: T, needPlaying: false }), 0);
    expect(p.onPoll({ isPlaying: false, trackUri: T, at: 1 })).toMatchObject({ kind: "resume" });
  });

  it("timeout and cancel clear only their own token", () => {
    const p = createPending();
    const a = p.start("play");
    const b = p.start("list"); // replaces a
    expect(p.timeout(a)).toBe(false);
    expect(p.current().token).toBe(b);
    expect(p.cancel(b)).toBe(true);
    expect(p.current()).toBeNull();
    expect(p.timeout(b)).toBe(false);
  });

  it("landed for a replaced token does nothing", () => {
    const p = createPending();
    const a = p.start("play");
    p.start("list");
    p.landed(a, 0);
    expect(p.onPoll({ isPlaying: true, trackUri: T, at: 5 })).toBeNull();
  });

  it("reset drops everything", () => {
    const p = createPending();
    p.start("play");
    p.reset();
    expect(p.current()).toBeNull();
  });
});
