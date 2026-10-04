import { describe, it, expect } from "vitest";
import { panelRows, PANEL_HISTORY_MAX } from "./playlist.js";

const t = (id) => ({ uri: `spotify:track:${id}`, name: id });
const played = (id, at = "2026-10-02T10:00:00Z") => ({ track: t(id), played_at: at });
const shape = (r) => r.rows.map((x) => `${x.role}:${x.track.name}:${x.i}`);

describe("panelRows", () => {
  it("shuffle off, the song in the list: the whole list, split at the song", () => {
    const r = panelRows({ list: [t("a"), t("b"), t("c"), t("d")], now: t("b"), history: [played("x")], queue: [t("q")] });
    expect(r.mode).toBe("list");
    expect(shape(r)).toEqual(["played:a:0", "now:b:1", "next:c:2", "next:d:3"]);
  });

  it("a song twice in the list: the first one is now", () => {
    const r = panelRows({ list: [t("a"), t("b"), t("a")], now: t("a") });
    expect(shape(r)).toEqual(["now:a:0", "next:b:1", "next:a:2"]);
  });

  it("keys are unique and stable per position", () => {
    const r = panelRows({ list: [t("a"), t("a")], now: t("a") });
    expect(r.rows.map((x) => x.key)).toEqual(["l0", "l1"]);
  });

  it("shuffle on: history, now, queue even with a list", () => {
    const r = panelRows({ list: [t("a"), t("b")], now: t("b"), history: [played("y"), played("x")], queue: [t("q1"), t("q2")], shuffle: true });
    expect(r.mode).toBe("queue");
    expect(shape(r)).toEqual(["played:x:1", "played:y:0", "now:b:-1", "next:q1:0", "next:q2:1"]);
  });

  it("the song isn't in the list (stale or another source): the queue view", () => {
    const r = panelRows({ list: [t("a")], now: t("z"), queue: [t("q")] });
    expect(r.mode).toBe("queue");
    expect(shape(r)).toEqual(["now:z:-1", "next:q:0"]);
  });

  it("no list: the queue view", () => {
    expect(panelRows({ list: null, now: t("a") }).mode).toBe("queue");
  });

  it("history skips the current song and repeats in a row, caps its length", () => {
    const history = [played("a"), played("now"), played("b"), played("b"), played("c")];
    const r = panelRows({ now: t("now"), history });
    expect(shape(r)).toEqual(["played:c:4", "played:b:2", "played:a:0", "now:now:-1"]);
    const long = Array.from({ length: 40 }, (_, i) => played(`h${i}`));
    const rows = panelRows({ now: t("now"), history: long }).rows;
    expect(rows.filter((x) => x.role === "played")).toHaveLength(PANEL_HISTORY_MAX);
    expect(rows[PANEL_HISTORY_MAX - 1].track.name).toBe("h0"); // the newest sits just above now
  });

  it("ignores broken rows", () => {
    const r = panelRows({ list: [null, t("a")], now: t("a"), history: [null, { track: null }], queue: [null] });
    expect(shape(r)).toEqual(["now:a:0"]);
  });

  it("nothing playing: history and queue only", () => {
    expect(shape(panelRows({ history: [played("a")], queue: [t("q")] }))).toEqual(["played:a:0", "next:q:0"]);
  });
});
