import { describe, it, expect } from "vitest";
import { rateLimitedSecs, rateLimitedError, quotaNotice, waitText, isWebApi } from "./quota.js";

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
    expect(quotaNotice(50000)).toBe("Spotify paused library access for 14 h — playback here still works");
  });

  it("knows which commands use the Web API", () => {
    for (const c of ["playback_state", "get_queue", "list_devices", "me_id", "get_playlists", "set_shuffle", "is_saved"]) expect(isWebApi(c)).toBe(true);
    for (const c of ["local_play", "local_state", "local_shuffle", "engine_status", "cache_get", "store_set", "app_log", "api_status", "session_get", "media_update", "set_dock_art", "auth_status", "login"])
      expect(isWebApi(c)).toBe(false);
  });
});
