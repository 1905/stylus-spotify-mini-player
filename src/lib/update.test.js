import { describe, it, expect } from "vitest";
import { updateView } from "./update.js";

const at = (status, more = {}) => updateView({ status, version: null, message: null, current: "0.4.0", auto: true, ...more });

describe("updateView", () => {
  it("says where the update is", () => {
    expect(at("idle").text).toBe("Version 0.4.0");
    expect(at("checking").text).toBe("Checking for updates…");
    expect(at("upToDate").text).toBe("Up to date (0.4.0)");
    expect(at("available", { version: "0.4.1", auto: false }).text).toBe("0.4.1 is available");
    expect(at("downloading", { version: "0.4.1" }).text).toBe("Downloading 0.4.1…");
    expect(at("staged", { version: "0.4.1" }).text).toBe("0.4.1 installs when Stylus quits");
    expect(at("failed", { message: "error sending request" }).text).toBe("Update failed: error sending request");
    expect(at("notInstallable").text).toBe("Updates need Stylus in Applications");
  });

  it("Check now only when nothing is on its way; Restart now only when staged", () => {
    const shown = (s) => {
      const v = at(s, { version: "0.4.1" });
      return [v.check, v.restart];
    };
    expect(shown("idle")).toEqual([true, false]);
    expect(shown("upToDate")).toEqual([true, false]);
    expect(shown("available")).toEqual([true, false]);
    expect(shown("failed")).toEqual([true, false]);
    expect(shown("checking")).toEqual([false, false]);
    expect(shown("downloading")).toEqual([false, false]);
    expect(shown("staged")).toEqual([false, true]);
    expect(shown("notInstallable")).toEqual([false, false]);
  });

  it("missing fields still read as a sentence", () => {
    expect(at("idle", { current: null }).text).toBe("");
    expect(at("upToDate", { current: "" }).text).toBe("Up to date");
    expect(at("failed").text).toBe("Update failed");
    expect(at("staged").text).toBe("The update installs when Stylus quits");
  });

  it("no updater (older backend, dev harness) or an unknown status = an empty row", () => {
    const none = { text: "", check: false, restart: false };
    expect(updateView(null)).toEqual(none);
    expect(updateView(undefined)).toEqual(none);
    expect(updateView({})).toEqual(none);
    expect(updateView({ status: "exploded" })).toEqual(none);
  });
});
