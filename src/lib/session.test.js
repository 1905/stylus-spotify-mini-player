import { describe, it, expect } from "vitest";
import { parseSession, playSession, sessionToSave, resumeSource, originUri, offsettable, SESSION_THROTTLE_MS } from "./session.js";

const U = (n) => `spotify:track:${n}`;
const PL = { kind: "playlist", id: "p1" };
const ME = "me";

const saved = (over = {}) => ({
  accountId: ME, contextUri: null, origin: null, uris: null, trackUri: U(1), positionMs: 0, savedAt: 0, ...over,
});

describe("originUri / offsettable", () => {
  it("builds playlist and album uris only", () => {
    expect(originUri(PL)).toBe("spotify:playlist:p1");
    expect(originUri({ kind: "album", id: "a" })).toBe("spotify:album:a");
    expect(originUri({ kind: "liked", id: "liked" })).toBeNull();
    expect(originUri(null)).toBeNull();
  });
  it("only playlists and albums take an offset", () => {
    expect(offsettable("spotify:playlist:x")).toBe(true);
    expect(offsettable("spotify:album:x")).toBe(true);
    expect(offsettable("spotify:artist:x")).toBe(false);
    expect(offsettable(null)).toBe(false);
  });
});

describe("parseSession", () => {
  it("round-trips a stored session", () => {
    const s = saved({ origin: PL, uris: [U(1), U(2)], positionMs: 1234.4, savedAt: 9 });
    expect(parseSession(JSON.stringify(s))).toEqual({ ...s, positionMs: 1234 });
  });
  it("broken or empty → null", () => {
    expect(parseSession("{nope")).toBeNull();
    expect(parseSession(null)).toBeNull();
    expect(parseSession(JSON.stringify({ accountId: ME }))).toBeNull();
  });
  it("drops a bad origin and caps uris at 200", () => {
    const uris = Array.from({ length: 250 }, (_, i) => U(i));
    const s = parseSession(saved({ origin: { kind: "artist", id: "x" }, uris }));
    expect(s.origin).toBeNull();
    expect(s.uris).toHaveLength(200);
  });
});

describe("playSession", () => {
  it("a uris play keeps the list (capped) and its origin", () => {
    const uris = Array.from({ length: 250 }, (_, i) => U(i));
    const s = playSession(ME, { uris }, PL, 100);
    expect(s).toMatchObject({ accountId: ME, contextUri: null, origin: PL, trackUri: U(0), positionMs: 0, savedAt: 100 });
    expect(s.uris).toHaveLength(200);
  });
  it("a context play has no uris", () => {
    expect(playSession(ME, { contextUri: "spotify:playlist:m", trackUri: U(3) }, null, 1)).toMatchObject({
      contextUri: "spotify:playlist:m", uris: null, trackUri: U(3), origin: null,
    });
  });
  it("an origin of another kind is dropped", () => {
    expect(playSession(ME, { uris: [U(1)] }, { kind: "liked", id: "liked" }, 1).origin).toBeNull();
  });
});

describe("sessionToSave", () => {
  const poll = (over = {}) => ({ accountId: ME, trackUri: U(2), contextUri: null, positionMs: 5000, ...over });

  it("no song or no account: no write", () => {
    expect(sessionToSave(null, poll({ trackUri: null }), 1)).toBeNull();
    expect(sessionToSave(null, poll({ accountId: null }), 1)).toBeNull();
  });

  it("throttles to one write per 10s, unless forced", () => {
    const prev = saved({ uris: [U(1), U(2)], savedAt: 1000 });
    expect(sessionToSave(prev, poll(), 1000 + SESSION_THROTTLE_MS - 1)).toBeNull();
    expect(sessionToSave(prev, poll(), 1000 + SESSION_THROTTLE_MS)).not.toBeNull();
    expect(sessionToSave(prev, poll(), 1001, true)).not.toBeNull();
  });

  it("a track in the saved list keeps the source and moves the position", () => {
    const prev = saved({ origin: PL, uris: [U(1), U(2)] });
    expect(sessionToSave(prev, poll(), 20000)).toEqual({
      accountId: ME, contextUri: null, origin: PL, uris: [U(1), U(2)], trackUri: U(2), positionMs: 5000, savedAt: 20000,
    });
  });

  it("takes the poll's context when it has one, keeping the origin when it's the same source", () => {
    const prev = saved({ origin: PL, uris: [U(1), U(2)] });
    const s = sessionToSave(prev, poll({ contextUri: "spotify:playlist:p1" }), 20000);
    expect(s).toMatchObject({ contextUri: "spotify:playlist:p1", origin: PL, uris: [U(1), U(2)] });
  });

  it("provenance: a track outside the source takes the poll's context", () => {
    const prev = saved({ origin: PL, uris: [U(1)] });
    expect(sessionToSave(prev, poll({ trackUri: U(9), contextUri: "spotify:album:z" }), 20000)).toMatchObject({
      contextUri: "spotify:album:z", origin: null, uris: null, trackUri: U(9),
    });
  });

  it("provenance: no context → that one track", () => {
    const prev = saved({ uris: [U(1)] });
    expect(sessionToSave(prev, poll({ trackUri: U(9) }), 20000)).toMatchObject({ contextUri: null, origin: null, uris: [U(9)] });
  });

  it("another account's session is replaced, not throttled", () => {
    const prev = saved({ accountId: "other", uris: [U(2)], savedAt: 19999 });
    expect(sessionToSave(prev, poll(), 20000)).toMatchObject({ accountId: ME, uris: [U(2)], origin: null });
  });
});

describe("resumeSource", () => {
  const paused = (over = {}) => ({ active: true, is_playing: false, progress_ms: 7000, context_uri: null, track: { uri: U(2) }, ...over });

  it("something plays anywhere: nothing to do", () => {
    expect(resumeSource(paused({ is_playing: true }), saved(), ME)).toBeNull();
  });

  it("paused somewhere with a context: that context at that track and second", () => {
    expect(resumeSource(paused({ context_uri: "spotify:playlist:x" }), saved(), ME)).toEqual({
      contextUri: "spotify:playlist:x", trackUri: U(2), positionMs: 7000,
    });
  });

  it("paused without a context, the saved list has the track: its origin", () => {
    const last = saved({ origin: PL, uris: [U(1), U(2)] });
    expect(resumeSource(paused(), last, ME)).toEqual({ contextUri: "spotify:playlist:p1", trackUri: U(2), positionMs: 7000 });
  });

  it("paused without a context, the saved list has the track, no origin: that list", () => {
    const last = saved({ uris: [U(1), U(2)] });
    expect(resumeSource(paused(), last, ME)).toEqual({ uris: [U(1), U(2)], trackUri: U(2), positionMs: 7000 });
  });

  it("paused without a context, no matching saved list: the one track", () => {
    expect(resumeSource(paused(), saved({ uris: [U(5)] }), ME)).toEqual({ uris: [U(2)], trackUri: U(2), positionMs: 7000 });
    expect(resumeSource(paused(), null, ME)).toEqual({ uris: [U(2)], trackUri: U(2), positionMs: 7000 });
  });

  it("nothing on Spotify: the saved session, origin first", () => {
    const last = saved({ origin: PL, uris: [U(1), U(2)], trackUri: U(2), positionMs: 61000 });
    expect(resumeSource({ active: false }, last, ME)).toEqual({ contextUri: "spotify:playlist:p1", trackUri: U(2), positionMs: 61000 });
    expect(resumeSource(null, saved({ contextUri: "spotify:album:a", trackUri: U(3) }), ME)).toEqual({
      contextUri: "spotify:album:a", trackUri: U(3), positionMs: 0,
    });
    expect(resumeSource(null, saved({ uris: [U(1), U(3)], trackUri: U(3) }), ME)).toEqual({ uris: [U(1), U(3)], trackUri: U(3), positionMs: 0 });
  });

  it("nothing at all, or another account's session: null", () => {
    expect(resumeSource({ active: false }, null, ME)).toBeNull();
    expect(resumeSource(null, saved({ accountId: "other" }), ME)).toBeNull();
    expect(resumeSource(null, saved(), null)).toBeNull();
  });

  it("an ad or podcast (active, no track) counts as nothing on Spotify", () => {
    expect(resumeSource({ active: true, is_playing: false, track: null }, saved({ uris: [U(1)] }), ME)).toEqual({
      uris: [U(1)], trackUri: U(1), positionMs: 0,
    });
  });
});
