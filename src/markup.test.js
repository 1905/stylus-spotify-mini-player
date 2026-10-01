import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";

// dev/index.html is the QA harness copy of the app markup: a drift means QA tests stale DOM.
const body = (path) => readFileSync(new URL(path, import.meta.url), "utf8").split("<body")[1];

describe("dev harness markup", () => {
  it("has the same <body> as src/index.html", () => {
    expect(body("../dev/index.html")).toBe(body("./index.html"));
  });
});
