// Browser-only mock of window.__TAURI__ so the UI renders without the Tauri
// backend. Loaded ONLY by dev.html, never shipped. Lets us screenshot and
// iterate on the design in a normal browser.
(function () {
  const img = (seed) => `https://picsum.photos/seed/${seed}/300/300`;

  const playlists = [
    { id: "p1", name: "Deep Focus", images: [{ url: img("focus") }], tracks: { total: 42 } },
    { id: "p2", name: "Night Drive", images: [{ url: img("drive") }], tracks: { total: 18 } },
    { id: "p3", name: "Goodbye Blue Monday", images: [{ url: img("blue") }], tracks: { total: 27 } },
    { id: "p4", name: "Sunroom", images: [{ url: img("sun") }], tracks: { total: 9 } },
    { id: "p5", name: "Low End Theory", images: [{ url: img("bass") }], tracks: { total: 64 } },
    { id: "p6", name: "Morning Coffee", images: [{ url: img("coffee") }], tracks: { total: 31 } },
  ];

  const mk = (i, name, artist, seed, dur) => ({
    id: "t" + i, uri: "spotify:track:" + i, name, artists: artist,
    album: name + " (Single)", cover: img(seed), duration_ms: dur,
  });
  const tracks = [
    mk(1, "Shpongleyes - Remastered", "Shpongle", "sh1", 534000),
    mk(2, "Once Upon a Sea of Blissful Awareness", "Shpongle", "sh2", 410000),
    mk(3, "Around the World in a Tea Daze", "Shpongle", "sh3", 489000),
    mk(4, "Flute Fruit - Remastered", "Shpongle", "sh4", 372000),
    mk(5, "Intro", "The xx", "xx1", 128000),
    mk(6, "Playing God", "Polyphia", "pol1", 196000),
    mk(7, "Teardrop", "Massive Attack", "ma1", 331000),
    mk(8, "Midnight City", "M83", "m83", 241000),
    mk(9, "Nightcall", "Kavinsky", "kav", 258000),
    mk(10, "Breathe", "Télépopmusik", "tel", 285000),
  ];

  const devices = [{ id: "d1", name: "MacBook Pro", type: "Computer", is_active: true, volume_percent: 60 }];

  let nowIdx = 0;
  const handlers = {
    is_authenticated: async () => true,
    login: async () => true,
    get_playlists: async () => playlists,
    get_playlist_tracks: async () => tracks,
    get_album_tracks: async () => tracks,
    search: async ({ query }) => ({
      tracks: tracks.slice(0, 6),
      albums: playlists.map((p) => ({
        id: p.id, uri: "spotify:album:" + p.id, name: p.name,
        artists: "Various", cover: p.images[0].url, year: "2024", total_tracks: p.tracks.total,
      })),
    }),
    get_access_token: async () => "mock",
    list_devices: async () => devices,
    playback_state: async () => ({
      active: true, is_playing: true, progress_ms: 44000,
      device_id: "d1", device_name: "MacBook Pro", volume: 60,
      track: tracks[nowIdx],
    }),
    play_on_device: async () => {},
    resume: async () => {}, pause: async () => {},
    next_track: async () => { nowIdx = (nowIdx + 1) % tracks.length; },
    previous_track: async () => {},
    seek: async () => {}, set_volume: async () => {},
  };

  window.__TAURI__ = {
    core: {
      invoke: async (cmd, args) => {
        if (handlers[cmd]) return handlers[cmd](args || {});
        console.warn("mock: unhandled", cmd);
        return null;
      },
    },
  };
})();

// Dev-only: auto-navigate by ?v= for screenshots.
window.addEventListener("load", () => {
  const v = new URLSearchParams(location.search).get("v");
  if (!v) return;
  setTimeout(() => {
    if (v === "library") document.getElementById("navLibrary").click();
    if (v === "detail") {
      document.getElementById("navLibrary").click();
      setTimeout(() => document.querySelector("#playlistGrid .card")?.click(), 150);
    }
    if (v === "search") {
      const inp = document.getElementById("searchInput");
      inp.value = "sh"; inp.dispatchEvent(new Event("input", { bubbles: true }));
    }
  }, 200);
});
