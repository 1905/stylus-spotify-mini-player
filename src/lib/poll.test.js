import { describe, it, expect } from "vitest";
import { pollDelay, gaveUp, POLL_MS, HIDDEN_POLL_MS, ERROR_POLL_MS, HIDDEN_IDLE_POLL_MS, GIVE_UP_FAILURES } from "./poll.js";

describe("pollDelay", () => {
  it("visible: 1s, 4s after many errors", () => {
    expect(pollDelay({ hidden: false, mode: "track", failures: 0 })).toBe(POLL_MS);
    expect(pollDelay({ hidden: false, mode: "idle", failures: 0 })).toBe(POLL_MS);
    expect(pollDelay({ hidden: false, mode: "idle", failures: 9 })).toBe(ERROR_POLL_MS);
  });

  it("hidden and idle: slow poll, so playback started from a phone still wakes it", () => {
    expect(pollDelay({ hidden: true, mode: "idle", failures: 0 })).toBe(HIDDEN_IDLE_POLL_MS);
    expect(pollDelay({ hidden: true, mode: "idle", failures: 9 })).toBe(HIDDEN_IDLE_POLL_MS);
  });

  it("hidden while something plays: keep polling, slower", () => {
    expect(pollDelay({ hidden: true, mode: "track", failures: 0 })).toBe(HIDDEN_POLL_MS);
    expect(pollDelay({ hidden: true, mode: "other", failures: 0 })).toBe(HIDDEN_POLL_MS);
    expect(pollDelay({ hidden: true, mode: "track", failures: 9 })).toBe(ERROR_POLL_MS);
  });
});

describe("retries", () => {
  it("back off 0.5, 1, 2, 4 s, capped at 4 s", () => {
    const d = [1, 2, 3, 4, 5].map((failures) => pollDelay({ hidden: false, mode: "track", failures }));
    expect(d).toEqual([500, 1000, 2000, 4000, 4000]);
  });

  it("the error shows only after the retries run out", () => {
    expect(gaveUp(0)).toBe(false);
    expect(gaveUp(GIVE_UP_FAILURES - 1)).toBe(false);
    expect(gaveUp(GIVE_UP_FAILURES)).toBe(true);
  });
});
