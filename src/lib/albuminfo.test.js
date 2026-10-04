import { describe, it, expect } from "vitest";
import { releaseText, lengthText, songsText, typeLabel, copyrightLines, catalogueNo, sleeveBackHtml, SLEEVE_ERROR } from "./albuminfo.js";

describe("releaseText", () => {
  it.each([
    ["2021-03-12", "day", "12 March 2021"],
    ["1997-05-28", "day", "28 May 1997"],
    ["2021-03", "month", "March 2021"],
    ["2021-03-01", "month", "March 2021"],
    ["1971", "year", "1971"],
    ["1971-01-01", "year", "1971"],
    ["2021-12-01", null, "1 December 2021"], // no precision: as precise as the date
    ["2021-12", null, "December 2021"],
    ["", "day", ""],
    [null, null, ""],
  ])("%s (%s) = %s", (date, precision, want) => {
    expect(releaseText(date, precision)).toBe(want);
  });
});

describe("lengthText", () => {
  it.each([
    [3221223, "54 min"], // OK Computer: 53:41
    [942786, "16 min"],
    [3600000, "1 hr 0 min"],
    [4350000, "1 hr 13 min"],
    [45000, "45 sec"],
    [0, ""],
    [null, ""],
    [undefined, ""],
  ])("%s ms = %s", (ms, want) => {
    expect(lengthText(ms)).toBe(want);
  });
});

describe("songsText and typeLabel", () => {
  it("counts songs", () => {
    expect(songsText(12)).toBe("12 songs");
    expect(songsText(1)).toBe("1 song");
    expect(songsText(0)).toBe("");
    expect(songsText(null)).toBe("");
  });

  it("names the release type", () => {
    expect(typeLabel("album")).toBe("Album");
    expect(typeLabel("EP")).toBe("EP");
    expect(typeLabel("single")).toBe("Single");
    expect(typeLabel("compilation")).toBe("Compilation");
    expect(typeLabel("")).toBe("Album");
  });
});

describe("copyrightLines", () => {
  it("puts © on C and ℗ on P", () => {
    expect(copyrightLines([{ text: "1997 XL Recordings Ltd", type: "C" }, { text: "1997 XL Recordings Ltd", type: "P" }])).toEqual([
      "© 1997 XL Recordings Ltd",
      "℗ 1997 XL Recordings Ltd",
    ]);
  });

  it("keeps one symbol when the text has its own", () => {
    expect(copyrightLines([{ text: "(P) 2020 Label", type: "P" }])).toEqual(["℗ 2020 Label"]);
    expect(copyrightLines([{ text: "(c) 2020 Label", type: "C" }])).toEqual(["© 2020 Label"]);
    expect(copyrightLines([{ text: "© 2020 Label", type: "C" }])).toEqual(["© 2020 Label"]);
    expect(copyrightLines([{ text: "℗2020 Label", type: "P" }])).toEqual(["℗ 2020 Label"]);
  });

  it("drops empty and repeated lines", () => {
    expect(copyrightLines([{ text: " ", type: "C" }, { text: "A", type: "C" }, { text: "(C) A", type: "C" }])).toEqual(["© A"]);
    expect(copyrightLines(null)).toEqual([]);
  });
});

describe("sleeveBackHtml", () => {
  const info = {
    id: "6dVIqQ8qmQ5GBnJ9shOYGE",
    name: "OK <Computer>",
    artists: "Radiohead",
    type: "album",
    release_date: "1997-05-28",
    release_precision: "day",
    total_tracks: 12,
    duration_ms: 3221223,
    label: "XL Recordings",
    copyrights: [{ text: "1997 XL Recordings Ltd", type: "C" }],
  };

  it("shows the details, escaped, and no track list", () => {
    const html = sleeveBackHtml(info);
    expect(html).toContain("OK &lt;Computer&gt;");
    expect(html).toContain("28 May 1997");
    expect(html).toContain("12 songs · 54 min");
    expect(html).toContain("XL Recordings");
    expect(html).toContain("© 1997 XL Recordings Ltd");
    expect(html).toContain(catalogueNo(info.id));
    expect(html).not.toContain("<ol");
  });

  it("leaves out what's unknown", () => {
    const html = sleeveBackHtml({ name: "X", artists: "Y" });
    expect(html).not.toContain("sb-facts");
    expect(html).not.toContain("sb-legal");
    expect(html).toContain(">Album<");
  });

  it("has a plain error line", () => {
    expect(SLEEVE_ERROR).toContain("Album details aren't available");
  });
});
