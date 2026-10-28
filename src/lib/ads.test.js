import { describe, it, expect } from "vitest";
import { AD_SETTLE_MS, AD_SKIP_GAP_MS, adSkipOnPoll } from "./ads.js";
import { HIDDEN_POLL_MS, POLL_MS } from "./poll.js";

const POLL = POLL_MS; // the visible poll period: the gap between two polls
const GAP = AD_SKIP_GAP_MS; // the minimum gap between two "next" commands
const AD = "spotify:ad:abc"; // the first ad names itself
const NEXT = "spotify:track:next"; // the track the break leads to (its end)
const OTHER = "spotify:track:other"; // another track: another ad
const free = { free: true };

const start = () => adSkipOnPoll({ mode: "other", uri: null, at: 0 }, free);

describe("adSkipOnPoll", () => {
  it("not free: no break", () => {
    expect(adSkipOnPoll({ mode: "other", uri: null, at: 0 }, { free: false })).toMatchObject({ state: null, skip: false });
  });

  it("a break begins: skip and log it", () => {
    expect(start()).toMatchObject({ skip: true, log: "ad break started (uri none)" });
    expect(adSkipOnPoll({ mode: "other", uri: AD, at: 0 }, free).log).toBe("ad break started (uri spotify:ad:abc)");
  });

  it("the second ad of the same break: a change-less poll still skips it (the cadence gap has cleared)", () => {
    let { state } = start();
    // the second ad names a track that isn't the break's end: the mode and a uri are present, the uri changed
    expect(adSkipOnPoll({ mode: "other", uri: OTHER, at: GAP + 1 }, { free, state })).toMatchObject({ skip: true, log: `ad break: skipping again (uri ${OTHER})` });
  });

  it("a re-skip no sooner than the gap after the last one: the break's own polls don't storm the command", () => {
    let { state } = start();
    // a poll 1 ms after the break began is the break's own poll (skipAd → changeTrack → kick): no re-skip yet,
    // and the break still holds (the state is preserved, not ended)
    const within = adSkipOnPoll({ mode: "other", uri: OTHER, at: 1 }, { free, state });
    expect(within).toMatchObject({ skip: false });
    expect(within.state).not.toBeNull();
    expect(within.state.skipAt).toBe(GAP);
    // and the gap has cleared: the same poll now skips
    expect(adSkipOnPoll({ mode: "other", uri: OTHER, at: GAP + 1 }, { free, state: within.state })).toMatchObject({ skip: true });
  });

  it("an ad that names its own track: the end moves to it, then the next ad-less poll ends the break", () => {
    let { state } = start();
    const second = adSkipOnPoll({ mode: "other", uri: NEXT, at: GAP + 1 }, { free, state });
    expect(second).toMatchObject({ skip: true });
    expect(second.state.uri).toBe(NEXT); // the end is now NEXT
    // the break's end is NEXT: the next poll that names nothing (the track left) ends the break
    expect(adSkipOnPoll({ mode: "other", uri: null, at: GAP + POLL + 1 }, { free, state: second.state })).toMatchObject({ state: null, skip: false });
  });

  it("the break ends into a pause: a poll that names nothing", () => {
    const { state } = start();
    expect(adSkipOnPoll({ mode: "other", uri: null, at: POLL }, { free, state })).toMatchObject({ state: null, skip: false });
  });

  it("a named song that is the break's end: the break is over", () => {
    let { state } = start();
    state = adSkipOnPoll({ mode: "other", uri: NEXT, at: GAP + 1 }, { free, state }).state; // an ad named the track it precedes
    expect(state.uri).toBe(NEXT);
    expect(adSkipOnPoll({ mode: "track", uri: NEXT, at: GAP + POLL + 1 }, { free, state })).toMatchObject({ state: null, skip: false });
  });

  it("a named song that isn't the break's end: the break is over", () => {
    const { state } = start();
    expect(adSkipOnPoll({ mode: "track", uri: OTHER, at: POLL }, { free, state })).toMatchObject({ state: null, skip: false });
  });

  it("idle ends the break", () => {
    const { state } = start();
    expect(adSkipOnPoll({ mode: "idle", uri: null, at: POLL }, { free, state })).toMatchObject({ state: null, skip: false });
  });

  it("AD_SETTLE_MS outlives the longest poll gap (a hidden window across a whole poll)", () => {
    expect(AD_SETTLE_MS).toBeGreaterThan(HIDDEN_POLL_MS);
  });
});
