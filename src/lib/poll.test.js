import { describe, it, expect } from "vitest";
import {
  pollDelay, pollMode, modeReason, sanityDue, listDue, gaveUp,
  LOCAL_TICK_MS, HIDDEN_LOCAL_TICK_MS, POLL_MS, HIDDEN_POLL_MS, SANITY_MS, LIST_MIN_MS, GIVE_UP_FAILURES,
} from "./poll.js";

const here = { active: true, engine_active: true };
const away = { active: false, engine_active: false };

describe("pollMode", () => {
  it("this Mac active: events", () => {
    expect(pollMode({ local: here })).toBe("events");
  });

  it("another device, nothing known, or a sanity check that disagrees: poll", () => {
    expect(pollMode({ local: null })).toBe("poll");
    expect(pollMode({ local: away })).toBe("poll");
    expect(pollMode({ local: here, distrust: true })).toBe("poll");
  });

  it("names the reason", () => {
    expect(modeReason({ mode: "events" })).toMatch(/player-state/);
    expect(modeReason({ mode: "poll" })).toMatch(/another device or nothing/);
    expect(modeReason({ mode: "poll", distrust: true })).toMatch(/another device active/);
  });
});

describe("pollDelay", () => {
  it("events: a local tick, no request", () => {
    expect(pollDelay({ hidden: false, mode: "events" })).toBe(LOCAL_TICK_MS);
    expect(pollDelay({ hidden: true, mode: "events" })).toBe(HIDDEN_LOCAL_TICK_MS);
  });

  it("poll: 5 s visible, 30 s hidden", () => {
    expect(pollDelay({ hidden: false, mode: "poll" })).toBe(POLL_MS);
    expect(POLL_MS).toBe(5000);
    expect(pollDelay({ hidden: true, mode: "poll" })).toBe(HIDDEN_POLL_MS);
    expect(HIDDEN_POLL_MS).toBe(30000);
  });

  it("retries back off 1, 2, 4 s, capped at the normal period", () => {
    const d = [1, 2, 3, 4, 5].map((failures) => pollDelay({ hidden: false, mode: "poll", failures }));
    expect(d).toEqual([1000, 2000, 4000, 5000, 5000]);
    expect(pollDelay({ hidden: true, mode: "poll", failures: 9 })).toBe(HIDDEN_POLL_MS);
  });

  it("the error shows only after the retries run out", () => {
    expect(gaveUp(GIVE_UP_FAILURES - 1)).toBe(false);
    expect(gaveUp(GIVE_UP_FAILURES)).toBe(true);
  });
});

describe("request budget", () => {
  it("sanity check once a minute", () => {
    expect(sanityDue({ lastAt: 0, now: SANITY_MS - 1 })).toBe(false);
    expect(sanityDue({ lastAt: 0, now: SANITY_MS })).toBe(true);
  });

  it("lists: only when needed, at most every 30 s, a user change goes at once", () => {
    expect(listDue({ lastAt: 0, now: LIST_MIN_MS, need: false })).toBe(false);
    expect(listDue({ lastAt: 0, now: LIST_MIN_MS, need: true })).toBe(true);
    expect(listDue({ lastAt: 1000, now: LIST_MIN_MS, need: true })).toBe(false);
    expect(listDue({ lastAt: 1000, now: 2000, need: false, force: true })).toBe(true);
  });
});
