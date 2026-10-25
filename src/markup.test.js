import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { LYRICS_PLACEMENTS } from "./lib/settings.js";

// dev/*.html are the QA harness copies of the app markup: a drift means QA tests stale DOM.
const body = (path) => readFileSync(new URL(path, import.meta.url), "utf8").split("<body")[1];

describe("dev harness markup", () => {
  it("has the same <body> as src/index.html", () => {
    expect(body("../dev/index.html")).toBe(body("./index.html"));
  });

  it("has the same <body> as src/mini.html", () => {
    expect(body("../dev/mini.html")).toBe(body("./mini.html"));
  });
});

describe("the Lyrics setting", () => {
  it("offers exactly the placements parseSettings accepts, in order", () => {
    const html = readFileSync(new URL("./index.html", import.meta.url), "utf8");
    const group = html.split('id="lyricsOpts"')[1].split("</div>")[0];
    expect([...group.matchAll(/data-lyrics="([^"]+)"/g)].map((m) => m[1])).toEqual(LYRICS_PLACEMENTS);
  });
});
