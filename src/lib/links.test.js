import { describe, it, expect } from "vitest";
import { parseLink, looksLikeLink, trackLink } from "./links.js";

describe("parseLink", () => {
  it("reads share links", () => {
    expect(parseLink("https://open.spotify.com/playlist/37i9dQZEVXcVV9hd3iqSgp?si=384f379cb4d54126")).toEqual({
      kind: "playlist",
      id: "37i9dQZEVXcVV9hd3iqSgp",
      uri: "spotify:playlist:37i9dQZEVXcVV9hd3iqSgp",
    });
    expect(parseLink(" https://open.spotify.com/playlist/37i9dQZF1E4qxgJU46pFLr?si=83348eaf08b543d1\n").id).toBe("37i9dQZF1E4qxgJU46pFLr");
    expect(parseLink("https://open.spotify.com/intl-de/album/6dVIqQ8qmQ5GBnJ9shOYGE").kind).toBe("album");
    expect(parseLink("open.spotify.com/artist/4Z8W4fKeB5YxbusRsdQVPb#x").kind).toBe("artist");
    expect(parseLink("https://open.spotify.com/track/7c378mlmubSu7NGkLFa4sN?si=a&context=b").uri).toBe("spotify:track:7c378mlmubSu7NGkLFa4sN");
    expect(parseLink("https://open.spotify.com/user/spotify/playlist/37i9dQZF1DXcBWIGoYBM5M").id).toBe("37i9dQZF1DXcBWIGoYBM5M");
  });

  it("reads URIs", () => {
    expect(parseLink("spotify:playlist:37i9dQZEVXcVV9hd3iqSgp").kind).toBe("playlist");
    expect(parseLink("spotify:user:someone:playlist:37i9dQZEVXcVV9hd3iqSgp").uri).toBe("spotify:playlist:37i9dQZEVXcVV9hd3iqSgp");
  });

  it("refuses anything else", () => {
    for (const bad of ["", null, "bonobo", "https://example.com/playlist/37i9dQZEVXcVV9hd3iqSgp", "https://open.spotify.com/show/37i9dQZEVXcVV9hd3iqSgp", "https://open.spotify.com/playlist/short", "spotify:episode:7c378mlmubSu7NGkLFa4sN", "https://spotify.link/abc"]) {
      expect(parseLink(bad)).toBe(null);
    }
  });

  it("knows an attempt at a link", () => {
    expect(looksLikeLink("https://spotify.link/abc")).toBe(true);
    expect(looksLikeLink("https://open.spotify.com/show/x")).toBe(true);
    expect(looksLikeLink("bonobo radio")).toBe(false);
  });
});

describe("trackLink", () => {
  it("gives the open.spotify.com link of a track", () => {
    expect(trackLink("spotify:track:7c378mlmubSu7NGkLFa4sN")).toBe("https://open.spotify.com/track/7c378mlmubSu7NGkLFa4sN");
    expect(trackLink("https://open.spotify.com/track/7c378mlmubSu7NGkLFa4sN?si=a")).toBe("https://open.spotify.com/track/7c378mlmubSu7NGkLFa4sN");
  });

  it("gives null for anything else", () => {
    for (const bad of ["spotify:local:Artist:Album:Song:180", "spotify:local:Artist:track:ABCDEFGHIJKLMNOPQRSTUV:180", "spotify:episode:7c378mlmubSu7NGkLFa4sN", "spotify:album:6dVIqQ8qmQ5GBnJ9shOYGE", "spotify:track:short", "", null, undefined]) {
      expect(trackLink(bad), String(bad)).toBe(null);
    }
  });
});
