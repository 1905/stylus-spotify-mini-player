// Browser-only stub of window.__TAURI__ for the dev harness. Never shipped.
// Backed by dev/fixture.json (real Spotify data from dev/capture.py).
// Scenario from ?s= : playing (default), paused, nothing, nodevice, login, reconnect,
// error, library, library-detail, search, search-empty, long-titles.
// QA hook: window.__mock = { scenario, state, invoke }.
(function () {
  "use strict";

  const SCENARIOS = [
    "playing", "paused", "nothing", "nodevice", "login", "reconnect", "error",
    "library", "library-detail", "search", "search-empty", "long-titles",
  ];
  const requested = new URLSearchParams(location.search).get("s") || "playing";
  const scenario = SCENARIOS.includes(requested) ? requested : "playing";
  if (scenario !== requested) console.warn(`mock: unknown scenario "${requested}", using "playing"`);

  // Synchronous load so invoke is ready before app.js runs.
  const xhr = new XMLHttpRequest();
  xhr.open("GET", "fixture.json", false);
  xhr.send();
  if (xhr.status !== 200) throw new Error("mock: cannot load dev/fixture.json (" + xhr.status + ")");
  const fx = JSON.parse(xhr.responseText);

  const clone = (v) => (v == null ? v : JSON.parse(JSON.stringify(v)));

  // Every known track by uri, so play_on_device can resolve uris.
  const byUri = new Map();
  const remember = (t) => { if (t && t.uri && !byUri.has(t.uri)) byUri.set(t.uri, t); };
  remember(fx.now);
  (fx.queue || []).forEach(remember);
  (fx.recent || []).forEach((r) => remember(r.track));
  Object.values(fx.playlistTracks || {}).forEach((rows) => rows.forEach(remember));
  Object.values(fx.albumTracks || {}).forEach((rows) => rows.forEach(remember));
  ((fx.search || {}).tracks || []).forEach(remember);

  const DEVICE = { id: "mock-device-1", name: "MacBook Pro", type: "Computer", is_active: true };
  const LONG_TITLE =
    "An Extraordinarily Long Song Title That Keeps Going Well Past Any Reasonable Length " +
    "To Test Two-Line Clamping And Ellipsis";

  const state = {
    now: clone(fx.now) || clone((fx.queue || [])[0]) || null,
    queue: clone(fx.queue || []),
    history: clone(fx.recent || []), // newest first: [{track, played_at}]
    isPlaying: scenario !== "paused",
    progressBase: 44000,
    progressAt: Date.now(),
    active: !["nothing", "nodevice"].includes(scenario),
    devices: scenario === "nodevice" ? [] : [DEVICE],
  };
  if (state.queue.length && state.now && state.queue[0].uri === state.now.uri) state.queue.shift();
  if (scenario === "long-titles" && state.now) {
    state.now.name = LONG_TITLE.slice(0, 120).padEnd(120, ".");
    state.now.artists = "Someone With A Fairly Long Name, Another Featured Artist, And A Third";
    state.now.album = "A Deluxe Remastered Anniversary Edition With Bonus Tracks And Demos";
  }
  if (scenario === "nothing" || scenario === "nodevice") state.queue = [];

  const iso = () => new Date().toISOString();
  const progress = () => {
    if (!state.now) return 0;
    const p = state.progressBase + (state.isPlaying ? Date.now() - state.progressAt : 0);
    return Math.min(p, state.now.duration_ms || p);
  };
  const setProgress = (ms) => { state.progressBase = ms; state.progressAt = Date.now(); };

  const pushHistory = (t) => { if (t) state.history.unshift({ track: clone(t), played_at: iso() }); };

  function advance() {
    if (!state.now && !state.queue.length) return;
    pushHistory(state.now);
    state.now = state.queue.shift() || null;
    setProgress(0);
  }

  function back() {
    if (!state.history.length) return setProgress(0);
    if (state.now) state.queue.unshift(state.now);
    state.now = state.history.shift().track;
    setProgress(0);
  }

  const reject = (msg) => Promise.reject(msg);
  const needDevice = () => {
    if (!state.devices.length || !state.active) throw "NO_ACTIVE_DEVICE: no active device (mock)";
  };

  const handlers = {
    auth_status: () => (scenario === "login" ? "login" : scenario === "reconnect" ? "reconnect" : "ok"),
    login: () => null,

    get_playlists: () => clone(fx.playlists || []),
    get_playlist_tracks: ({ playlistId }) => {
      const pt = fx.playlistTracks || {};
      return clone(pt[playlistId] || Object.values(pt)[0] || []);
    },
    get_album_tracks: ({ albumId }) => {
      const at = fx.albumTracks || {};
      return clone(at[albumId] || Object.values(at)[0] || []);
    },
    search: ({ query }) => {
      if (scenario === "search-empty") return { tracks: [], albums: [] };
      const s = fx.search || {};
      void query; // fixture holds one captured query; any query returns it
      return { tracks: clone((s.tracks || []).slice(0, 10)), albums: clone((s.albums || []).slice(0, 10)) };
    },
    get_queue: () => clone(state.queue),
    get_recently_played: () => clone(state.history.slice(0, 30)),

    playback_state: () => {
      if (!state.active || !state.now) return { active: false };
      if (state.now.duration_ms && progress() >= state.now.duration_ms) advance();
      if (!state.now) return { active: false };
      return {
        active: true,
        is_playing: state.isPlaying,
        progress_ms: progress(),
        device_id: DEVICE.id,
        device_name: DEVICE.name,
        track: clone(state.now),
      };
    },
    list_devices: () => clone(state.devices),

    play_on_device: ({ deviceId, uris }) => {
      if (!state.devices.length) throw "NO_ACTIVE_DEVICE: no device (mock)";
      void deviceId;
      const tracks = (uris || []).map((u) => byUri.get(u)).filter(Boolean).map(clone);
      if (!tracks.length) throw "mock: unknown uris";
      pushHistory(state.now);
      state.now = tracks[0];
      state.queue = tracks.slice(1);
      state.active = true;
      state.isPlaying = true;
      setProgress(0);
      return null;
    },
    resume: () => {
      if (!state.devices.length) throw "NO_ACTIVE_DEVICE: no device (mock)";
      if (!state.now) throw "NO_ACTIVE_DEVICE: nothing to resume (mock)";
      state.active = true;
      setProgress(progress());
      state.isPlaying = true;
      return null;
    },
    pause: () => { needDevice(); setProgress(progress()); state.isPlaying = false; return null; },
    next_track: () => { needDevice(); advance(); return null; },
    previous_track: () => { needDevice(); back(); return null; },
    seek: ({ positionMs }) => { needDevice(); setProgress(Math.max(0, Number(positionMs) || 0)); return null; },
  };

  async function invoke(cmd, args) {
    await new Promise((r) => setTimeout(r, 40)); // feel async, like IPC
    const h = handlers[cmd];
    if (!h) return reject(`mock: unknown command ${cmd}`);
    if (scenario === "error" && cmd !== "auth_status") return reject("network down");
    try {
      return h(args || {});
    } catch (e) {
      return reject(String(e));
    }
  }

  window.__TAURI__ = { core: { invoke } };
  window.__mock = { scenario, state, invoke, advance };

  // Overlay scenarios: drive the real UI once it exists (T5/T6 markup).
  const waitFor = (sel, ms = 5000) =>
    new Promise((resolve) => {
      const t0 = Date.now();
      (function poll() {
        const el = document.querySelector(sel);
        if (el) return resolve(el);
        if (Date.now() - t0 > ms) {
          console.warn("mock: timed out waiting for " + sel);
          return resolve(null);
        }
        setTimeout(poll, 50);
      })();
    });

  async function drive() {
    if (scenario === "library" || scenario === "library-detail") {
      (await waitFor("#libraryBtn"))?.click();
      if (scenario === "library-detail") {
        const row = await waitFor("#libList [data-id], #libList button, #libList > *");
        row?.click();
      }
    }
    if (scenario === "search" || scenario === "search-empty") {
      (await waitFor("#searchBtn"))?.click();
      const input = await waitFor("#searchInput");
      if (input) {
        input.value = scenario === "search" ? (fx.search && fx.search.query) || "the xx" : "zzqx nothing";
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }
    }
  }
  if (["library", "library-detail", "search", "search-empty"].includes(scenario)) {
    window.addEventListener("load", () => setTimeout(drive, 300));
  }
})();
