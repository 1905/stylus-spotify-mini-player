import { describe, it, expect } from "vitest";
import { isEngineDevice, isLocal, refusedText, volumeTiming } from "./route.js";

const READY = { state: "ready", device_id: "run" };

describe("isEngineDevice", () => {
  it("is the ready engine's own device", () => {
    expect(isEngineDevice(READY, "run")).toBe(true);
  });
  it("not another device, not before ready, not without an id", () => {
    expect(isEngineDevice(READY, "marantz")).toBe(false);
    expect(isEngineDevice({ state: "starting", device_id: "run" }, "run")).toBe(false);
    expect(isEngineDevice({ state: "ready", device_id: null }, null)).toBe(false);
    expect(isEngineDevice(null, "run")).toBe(false);
    expect(isEngineDevice(READY, null)).toBe(false);
  });
});

describe("isLocal", () => {
  it("ready, our device, and the last poll's active device", () => {
    expect(isLocal(READY, "run", "run")).toBe(true);
  });
  it("inactive (another device active, or none): remote", () => {
    expect(isLocal(READY, "run", "marantz")).toBe(false);
    expect(isLocal(READY, "run", null)).toBe(false);
  });
  it("a command captured for another device stays remote", () => {
    expect(isLocal(READY, "marantz", "marantz")).toBe(false);
  });
  it("engine not ready: remote", () => {
    expect(isLocal({ state: "reconnecting", device_id: "run" }, "run", "run")).toBe(false);
    expect(isLocal(null, "run", "run")).toBe(false);
  });
});

describe("volumeTiming", () => {
  it("local: 30ms quiet, 1s lag; remote: 200ms, 2.5s", () => {
    expect(volumeTiming(true)).toEqual({ quietMs: 30, lagMs: 1000 });
    expect(volumeTiming(false)).toEqual({ quietMs: 200, lagMs: 2500 });
  });
});

describe("refusedText", () => {
  it("names the action and the device", () => {
    expect(refusedText("NOT_AVAILABLE_REMOTE: pause on other devices", "Kitchen")).toBe("Pause isn't available on Kitchen");
    expect(refusedText("NOT_AVAILABLE_REMOTE: volume on other devices", "TV")).toBe("Volume isn't available on TV");
    expect(refusedText("NOT_AVAILABLE_REMOTE: transfer on other devices", "TV")).toBe("Moving playback isn't available on TV");
    expect(refusedText("NOT_AVAILABLE_REMOTE: queue on other devices", "TV")).toBe("Add to queue isn't available on TV");
  });
  it("an unknown action as is; no device name", () => {
    expect(refusedText("NOT_AVAILABLE_REMOTE: dance on other devices", "TV")).toBe("dance isn't available on TV");
    expect(refusedText("NOT_AVAILABLE_REMOTE: seek on other devices")).toBe("Seek isn't available on that device");
  });
  it("other errors: null", () => {
    expect(refusedText("HTTP 500: boom", "TV")).toBeNull();
    expect(refusedText("NO_ACTIVE_DEVICE: x", "TV")).toBeNull();
    expect(refusedText(undefined, "TV")).toBeNull();
  });
});
