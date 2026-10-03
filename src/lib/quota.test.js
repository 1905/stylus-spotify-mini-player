import { describe, it, expect } from "vitest";
import { rateLimitedSecs, rateLimitedError, quotaNotice, quotaStatus, waitText, isWebOnly } from "./quota.js";

describe("rate limit", () => {
  it("reads the seconds from Rust's error", () => {
    expect(rateLimitedSecs("RATE_LIMITED:53393: Spotify paused this app's library access")).toBe(53393);
    expect(rateLimitedSecs("Spotify API 500: oops")).toBe(0);
    expect(rateLimitedSecs(null)).toBe(0);
    expect(rateLimitedSecs(rateLimitedError(12.2))).toBe(13);
  });

  it("says how long in plain words", () => {
    expect(waitText(53393)).toBe("15 h");
    expect(waitText(50000)).toBe("14 h");
    expect(waitText(1500)).toBe("25 min");
    expect(waitText(5)).toBe("1 min");
    expect(quotaNotice("remote", 50000)).toBe("Spotify limits this for 14 h — works on This Mac");
    expect(quotaNotice("library", 50000)).toBe("Spotify limits this for 14 h");
    expect(quotaStatus(46800)).toBe("Web API paused for 13 h");
  });

  it("knows which commands have no source but the Web API", () => {
    for (const c of ["playback_state", "transfer_playback", "set_shuffle", "set_repeat", "pause", "seek", "play_context"]) expect(isWebOnly(c)).toBe(true);
    // the internal API serves these first (or they are local): Rust decides
    for (const c of ["get_playlists", "search", "get_queue", "list_devices", "me_id", "is_saved", "save_track", "add_to_queue", "local_play", "store_set", "api_status"])
      expect(isWebOnly(c)).toBe(false);
  });
});
