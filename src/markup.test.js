import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";

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
