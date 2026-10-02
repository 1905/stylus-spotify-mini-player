import { describe, it, expect } from "vitest";
import { thisMacRow, isTheRun, THE_RUN } from "./engine.js";

const RUN = { id: "r", name: THE_RUN, type: "Computer" };
const MARANTZ = { id: "m", name: "Marantz", type: "AVR" };
const MACBOOK = { id: "b", name: "MacBook Pro", type: "Computer" };

describe("thisMacRow", () => {
  it("has no row while the device list is unknown", () => {
    expect(thisMacRow({ state: "needs_login" }, null)).toBeNull();
  });

  it("has no row once The Run is listed, whatever the engine says", () => {
    for (const state of ["needs_login", "starting", "failed", "account_mismatch"]) {
      expect(thisMacRow({ state }, [MARANTZ, RUN])).toBeNull();
    }
    expect(thisMacRow({ state: "needs_login" }, [RUN], "login")).toBeNull();
  });

  it("has no row when the engine is ready or unknown", () => {
    expect(thisMacRow({ state: "ready" }, [MARANTZ])).toBeNull();
    expect(thisMacRow(null, [MARANTZ])).toBeNull();
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

  it("account mismatch offers the player login, the title is the reason", () => {
    const row = thisMacRow({ state: "account_mismatch", reason: "player: a, app: b" }, []);
    expect(row).toEqual({ type: "Log in to play here", title: "player: a, app: b" });
  });

  it("a running click outranks the engine state", () => {
    expect(thisMacRow({ state: "needs_login" }, [], "login").type).toBe("Waiting for login…");
    expect(thisMacRow({ state: "ready" }, [], "connecting").type).toBe("Connecting…");
  });
});

describe("isTheRun", () => {
  it("matches by name", () => {
    expect(isTheRun(RUN)).toBe(true);
    expect(isTheRun(MACBOOK)).toBe(false);
    expect(isTheRun(null)).toBe(false);
  });
});
