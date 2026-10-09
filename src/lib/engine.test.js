import { describe, it, expect } from "vitest";
import { thisMacRow, isHere, isPremiumRequired, deviceLabel, HERE, THE_RUN_MISSING_MS, preferredDevice, notSavedReason } from "./engine.js";

// the in-app player: Spotify lists it as "This Mac"; the engine's device id says it's ours
const RUN = { id: "r", name: "This Mac", type: "Computer" };
const READY = { state: "ready", device_id: "r" };
const MARANTZ = { id: "m", name: "Marantz", type: "AVR" };
const MACBOOK = { id: "b", name: "MacBook Pro", type: "Computer" };

describe("isPremiumRequired", () => {
  it("only the Premium failure", () => {
    expect(isPremiumRequired({ state: "failed", reason: "Spotify Premium is required to play on this Mac" })).toBe(true);
    expect(isPremiumRequired({ state: "failed", reason: "no audio" })).toBe(false);
    expect(isPremiumRequired({ state: "ready" })).toBe(false);
    expect(isPremiumRequired(null)).toBe(false);
  });
});

describe("thisMacRow", () => {
  it("has no row while the device list is unknown", () => {
    expect(thisMacRow({ state: "needs_login" }, null)).toBeNull();
  });

  it("has no row once the player is listed, whatever the engine says", () => {
    for (const state of ["needs_login", "starting", "failed"]) {
      expect(thisMacRow({ state, device_id: "r" }, [MARANTZ, RUN])).toBeNull();
    }
    expect(thisMacRow({ state: "needs_login", device_id: "r" }, [RUN], "login")).toBeNull();
  });

  it("a device named like the player isn't it: only the engine's device id counts", () => {
    expect(thisMacRow({ state: "needs_login", device_id: null }, [RUN]).type).toBe("Log in to play here");
    expect(thisMacRow({ ...READY, device_id: "other" }, [RUN]).type).toBe("Connecting…");
  });

  it("ready but not listed yet: connecting, then a retry after 20s", () => {
    expect(thisMacRow({ state: "ready" }, [MARANTZ]).type).toBe("Connecting…");
    expect(thisMacRow({ state: "ready" }, [MARANTZ], "", THE_RUN_MISSING_MS - 1).type).toBe("Connecting…");
    expect(thisMacRow({ state: "ready" }, [MARANTZ], "", THE_RUN_MISSING_MS).type).toBe("Not showing up — retry");
  });

  it("the retry label is only for a ready engine", () => {
    expect(thisMacRow({ state: "needs_login" }, [], "", THE_RUN_MISSING_MS).type).toBe("Log in to play here");
    expect(thisMacRow({ state: "ready" }, [], "connecting", THE_RUN_MISSING_MS).type).toBe("Connecting…");
  });

  it("unknown engine state: connecting", () => {
    expect(thisMacRow(null, [MARANTZ]).type).toBe("Connecting…");
  });

  it("another computer (the Spotify app) doesn't hide the row", () => {
    expect(thisMacRow({ state: "needs_login" }, [MACBOOK]).type).toBe("Log in to play here");
  });

  it("shows each engine state", () => {
    expect(thisMacRow({ state: "starting" }, []).type).toBe("Connecting…");
    expect(thisMacRow({ state: "reconnecting" }, []).type).toBe("Connecting…");
    expect(thisMacRow({ state: "needs_login" }, []).type).toBe("Log in to play here");
  });

  it("failed: the title is the reason", () => {
    expect(thisMacRow({ state: "failed", reason: "Premium required" }, [])).toEqual({
      type: "Not available right now",
      title: "Premium required",
    });
    expect(thisMacRow({ state: "failed" }, []).title).toMatch(/isn't available/);
  });

  it("a running click outranks the engine state", () => {
    expect(thisMacRow({ state: "needs_login" }, [], "login").type).toBe("Waiting for login…");
    expect(thisMacRow({ state: "ready" }, [], "connecting").type).toBe("Connecting…");
  });
});

describe("isHere", () => {
  it("matches by the engine's device id, never by name", () => {
    expect(isHere(RUN, READY)).toBe(true);
    expect(isHere(MACBOOK, READY)).toBe(false);
    expect(isHere({ id: "x", name: "Stylus" }, READY)).toBe(false);
    expect(isHere({ id: "x", name: "This Mac" }, READY)).toBe(false);
    expect(isHere(RUN, { state: "starting", device_id: null })).toBe(false);
    expect(isHere(RUN, null)).toBe(false);
    expect(isHere(null, READY)).toBe(false);
  });
});

describe("deviceLabel", () => {
  it("the in-app player is Here, others keep their name", () => {
    expect(deviceLabel(RUN, READY)).toBe(HERE);
    expect(HERE).toBe("Here");
    expect(deviceLabel(MARANTZ, READY)).toBe("Marantz");
    expect(deviceLabel(RUN, null)).toBe("This Mac"); // engine unknown: Spotify's name
    expect(deviceLabel(null, READY)).toBe("");
  });
});

describe("preferredDevice", () => {
  const marantz = { id: "m", name: "Marantz STEREO 70s", type: "AVR", is_active: true };
  const run = { id: "r", name: "This Mac", type: "Computer", is_active: false };
  const mac = { id: "c", name: "MacBook Pro", type: "Computer", is_active: false };
  it("prefers the in-app player over an active network player", () => {
    expect(preferredDevice([marantz, mac, run], READY).id).toBe("r");
  });
  it("engine unknown: the player is just a computer", () => {
    expect(preferredDevice([marantz, mac, run]).id).toBe("c");
  });
  it("then any computer, then the active one, then the first", () => {
    expect(preferredDevice([marantz, mac]).id).toBe("c");
    expect(preferredDevice([{ ...marantz, is_active: false }, { id: "t", type: "TV", is_active: true }]).id).toBe("t");
    expect(preferredDevice([{ ...marantz, is_active: false }]).id).toBe("m");
    expect(preferredDevice([])).toBe(null);
  });
  it("never picks a restricted device", () => {
    expect(preferredDevice([{ ...run, is_restricted: true }, marantz], READY).id).toBe("m");
  });
});

describe("notSavedReason", () => {
  it("gives the save error of Rust's LOGIN_NOT_SAVED only", () => {
    expect(notSavedReason("logged in, but the login could not be saved: disk full")).toBe("disk full");
    expect(notSavedReason("logged in, but the login could not be saved")).toBe("unknown error");
    expect(notSavedReason("Spotify refused the player login")).toBe(null);
    expect(notSavedReason(null)).toBe(null);
  });
});
