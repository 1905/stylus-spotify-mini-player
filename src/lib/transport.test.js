import { describe, it, expect } from "vitest";
import { createIntents, nextRepeat, stepVolume } from "./transport.js";

describe("createIntents", () => {
  it("a key can have its own settle lag", () => {
    const i = createIntents(500, { volume: 2500 });
    i.start("volume");
    i.finish("volume", 1000);
    i.start("play");
    i.finish("play", 1000);
    expect(i.settled("play", 1500)).toBe(true);
    expect(i.settled("volume", 1500)).toBe(false);
    expect(i.settled("volume", 3500)).toBe(true);
  });

  it("is settled before anything was asked", () => {
    const i = createIntents(500);
    expect(i.settled("play", 0)).toBe(true);
  });

  it("is not settled while a command is pending", () => {
    const i = createIntents(500);
    i.start("play");
    expect(i.settled("play", 10_000)).toBe(false);
  });

  it("settles only for polls started lagMs after the last command landed", () => {
    const i = createIntents(500);
    i.start("play");
    i.start("play");
    i.finish("play", 100);
    expect(i.settled("play", 10_000)).toBe(false); // one still pending
    i.finish("play", 200);
    expect(i.settled("play", 699)).toBe(false);
    expect(i.settled("play", 700)).toBe(true);
  });

  it("keeps keys apart", () => {
    const i = createIntents(500);
    i.start("shuffle");
    expect(i.settled("shuffle", 0)).toBe(false);
    expect(i.settled("repeat", 0)).toBe(true);
  });

  it("knows the latest click", () => {
    const i = createIntents(500);
    const a = i.start("volume");
    const b = i.start("volume");
    expect(i.latest("volume", a)).toBe(false);
    expect(i.latest("volume", b)).toBe(true);
  });

  it("drop lets the next poll apply at once", () => {
    const i = createIntents(500);
    i.start("repeat");
    i.finish("repeat", 1000);
    i.drop("repeat");
    expect(i.settled("repeat", 0)).toBe(true);
  });

  it("reset forgets every key", () => {
    const i = createIntents(500);
    i.start("play");
    i.reset();
    expect(i.settled("play", 0)).toBe(true);
  });
});

describe("nextRepeat", () => {
  it.each([
    ["off", "context"],
    ["context", "track"],
    ["track", "off"],
    [undefined, "context"],
    ["weird", "context"],
  ])("%s → %s", (from, want) => {
    expect(nextRepeat(from)).toBe(want);
  });
});

describe("stepVolume", () => {
  it.each([
    [50, "ArrowRight", 55],
    [50, "ArrowUp", 55],
    [50, "ArrowLeft", 45],
    [50, "ArrowDown", 45],
    [98, "ArrowRight", 100],
    [3, "ArrowLeft", 0],
    [40, "Home", 0],
    [40, "End", 100],
    [40, "a", null],
    [null, "ArrowRight", 5],
  ])("%s + %s = %s", (v, key, want) => {
    expect(stepVolume(v, key)).toBe(want);
  });
});
