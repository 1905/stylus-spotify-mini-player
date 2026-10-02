import { describe, it, expect } from "vitest";
import { pollDelay, POLL_MS, HIDDEN_POLL_MS, ERROR_POLL_MS, HIDDEN_IDLE_POLL_MS } from "./poll.js";

describe("pollDelay", () => {
  it("visible: 1s, 4s after an error", () => {
    expect(pollDelay({ hidden: false, mode: "track", error: false })).toBe(POLL_MS);
    expect(pollDelay({ hidden: false, mode: "idle", error: false })).toBe(POLL_MS);
    expect(pollDelay({ hidden: false, mode: "idle", error: true })).toBe(ERROR_POLL_MS);
  });

  it("hidden and idle: slow poll, so playback started from a phone still wakes it", () => {
    expect(pollDelay({ hidden: true, mode: "idle", error: false })).toBe(HIDDEN_IDLE_POLL_MS);
    expect(pollDelay({ hidden: true, mode: "idle", error: true })).toBe(HIDDEN_IDLE_POLL_MS);
  });

  it("hidden while something plays: keep polling, slower", () => {
    expect(pollDelay({ hidden: true, mode: "track", error: false })).toBe(HIDDEN_POLL_MS);
    expect(pollDelay({ hidden: true, mode: "other", error: false })).toBe(HIDDEN_POLL_MS);
    expect(pollDelay({ hidden: true, mode: "track", error: true })).toBe(ERROR_POLL_MS);
  });
});
