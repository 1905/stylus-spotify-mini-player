// Browser-only stub of window.__TAURI__ for the dev harness. Never shipped.
// Backed by dev/fixture.json (real Spotify data from dev/capture.py).
// Scenario from ?s= : playing (default), paused, nothing, nodevice, login, reconnect,
// error, library, library-detail, search, search-empty, long-titles, ad,
// devices (3 devices incl. a restricted one, picker open), library-full (all Library groups),
// artist (The xx artist page open), mix-detail (first Spotify mix open),
// no-volume (the active device has no remote volume).
// QA hook: window.__mock = { scenario, state, invoke, advance, handlers }.
(function () {
  "use strict";

  const SCENARIOS = [
    "playing", "paused", "nothing", "nodevice", "login", "reconnect", "error",
    "library", "library-detail", "search", "search-empty", "long-titles", "ad",
    "devices", "library-full", "artist", "mix-detail", "no-volume",
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
  ((fx.liked || {}).tracks || []).forEach(remember);
  ((fx.top || {}).tracks || []).forEach(remember);

  // Devices: fixture list (Marantz active). `devices` adds a restricted TV; `no-volume`
  // strips remote volume from the active device. is_active is derived from state.
  const devices = scenario === "nodevice" ? [] : clone(fx.devices || []);
  if (scenario === "devices" && fx.restrictedDevice) devices.push(clone(fx.restrictedDevice));
  const firstActive = devices.find((d) => d.is_active) || devices[0] || null;
  if (scenario === "no-volume" && firstActive) {
    firstActive.supports_volume = false;
    firstActive.volume_percent = null;
  }
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
    devices,
    deviceId: firstActive ? firstActive.id : null,
    shuffle: false,
    repeat: "off",
    contextUri: null,
    userQueued: 0, // user-added tracks at the front of the queue (Spotify plays them first, FIFO)
    saved: new Set(((fx.liked || {}).tracks || []).map((t) => t.id)),
  };
  const likedBase = state.saved.size;
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

  const pushHistory = (t) => {
    if (t) state.history.unshift({ track: clone(t), played_at: iso(), context_uri: state.contextUri });
  };

  function advance() {
    if (!state.now && !state.queue.length) return;
    pushHistory(state.now);
    state.now = state.queue.shift() || null;
    if (state.userQueued > 0) state.userQueued--;
    setProgress(0);
  }

  function back() {
    if (!state.history.length) return setProgress(0);
    if (state.now) { state.queue.unshift(state.now); state.userQueued = 0; }
    state.now = state.history.shift().track;
    setProgress(0);
  }

  const reject = (msg) => Promise.reject(msg);
  const needDevice = () => {
    if (!state.devices.length || !state.active) throw "NO_ACTIVE_DEVICE: no active device (mock)";
  };
  const activeDevice = () => state.devices.find((d) => d.id === state.deviceId) || null;
  const findDevice = (id) => {
    const d = state.devices.find((x) => x.id === id);
    if (!d) throw "NO_ACTIVE_DEVICE: unknown device (mock)";
    if (d.is_restricted) throw "Device is restricted (mock)";
    return d;
  };
  // Optional deviceId on a play command: switch to it when given.
  const useDevice = (deviceId) => {
    if (!state.devices.length) throw "NO_ACTIVE_DEVICE: no device (mock)";
    if (deviceId) state.deviceId = findDevice(deviceId).id;
  };
  const playerExtras = () => {
    const d = activeDevice();
    const vol = !!(d && d.supports_volume);
    return {
      shuffle: state.shuffle,
      repeat: state.repeat,
      volume_percent: vol ? d.volume_percent : null,
      supports_volume: vol,
      context_uri: state.contextUri,
      device_id: d ? d.id : null,
      device_name: d ? d.name : null,
    };
  };
  const strHash = (s) => [...String(s)].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);
  const allTracks = () => [...byUri.values()];
  // Rotate a list so the 3 time ranges show different orders.
  const RANGES = { short_term: 0, medium_term: 3, long_term: 6 };
  const rotate = (list, n) => list.slice(n % (list.length || 1)).concat(list.slice(0, n % (list.length || 1)));
  const artistTiles = () => [...((fx.top || {}).artists || []), ...(fx.followed || [])];

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
    get_recently_played: () =>
      clone(state.history.slice(0, 30)).map((r) => ({ context_uri: null, ...r })),

    playback_state: () => {
      if (scenario === "ad") return { active: true, is_playing: state.isPlaying, progress_ms: 0, ...playerExtras(), track: null };
      if (!state.active || !state.now) return { active: false };
      if (state.now.duration_ms && progress() >= state.now.duration_ms) advance();
      if (!state.now) return { active: false };
      return {
        active: true,
        is_playing: state.isPlaying,
        progress_ms: progress(),
        ...playerExtras(),
        track: clone(state.now),
      };
    },
    list_devices: () =>
      state.devices.map((d) => ({ ...clone(d), is_active: state.active && d.id === state.deviceId })),

    play_on_device: ({ deviceId, uris }) => {
      useDevice(deviceId);
      const tracks = (uris || []).map((u) => byUri.get(u)).filter(Boolean).map(clone);
      if (!tracks.length) throw "mock: unknown uris";
      pushHistory(state.now);
      state.now = tracks[0];
      state.queue = tracks.slice(1);
      state.userQueued = 0;
      state.contextUri = null;
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
    resume_at: ({ deviceId, uri, positionMs }) => {
      useDevice(deviceId);
      const t = allTracks().find((x) => x.uri === uri) || state.now;
      if (!t) throw "mock: unknown track";
      state.active = true;
      state.now = clone(t);
      setProgress(positionMs || 0);
      state.isPlaying = true;
      return null;
    },
    pause: () => { needDevice(); setProgress(progress()); state.isPlaying = false; return null; },
    next_track: () => { needDevice(); advance(); return null; },
    previous_track: () => { needDevice(); back(); return null; },
    seek: ({ positionMs }) => { needDevice(); setProgress(Math.max(0, Number(positionMs) || 0)); return null; },

    // ---- v2: devices and player modes ----
    transfer_playback: ({ deviceId, play }) => {
      const d = findDevice(deviceId);
      setProgress(progress());
      state.deviceId = d.id;
      state.active = true;
      if (play) state.isPlaying = true;
      return null;
    },
    set_volume: ({ percent }) => {
      needDevice();
      const d = activeDevice();
      if (!d || !d.supports_volume) throw "Cannot control device volume (mock)";
      d.volume_percent = Math.max(0, Math.min(100, Math.round(Number(percent) || 0)));
      return null;
    },
    set_shuffle: ({ on }) => { needDevice(); state.shuffle = !!on; return null; },
    set_repeat: ({ mode }) => {
      needDevice();
      if (!["off", "context", "track"].includes(mode)) throw "mock: bad repeat mode " + mode;
      state.repeat = mode;
      return null;
    },
    play_context: ({ deviceId, contextUri }) => {
      useDevice(deviceId);
      if (!contextUri) throw "mock: no context uri";
      const pool = ((fx.liked || {}).tracks || []).length ? fx.liked.tracks : allTracks();
      if (!pool.length) throw "mock: no tracks";
      const start = strHash(contextUri) % pool.length;
      const run = rotate(pool, start).slice(0, 11).map(clone);
      pushHistory(state.now);
      state.now = run[0];
      state.queue = run.slice(1);
      state.userQueued = 0;
      state.contextUri = contextUri;
      state.active = true;
      state.isPlaying = true;
      setProgress(0);
      return null;
    },
    add_to_queue: ({ deviceId, uri }) => {
      needDevice();
      void deviceId;
      const t = byUri.get(uri);
      if (!t) throw "mock: unknown uri " + uri;
      state.queue.splice(state.userQueued, 0, clone(t));
      state.userQueued++;
      return null;
    },

    // ---- v2: Liked songs ----
    is_saved: ({ trackId }) => state.saved.has(trackId),
    save_track: ({ trackId }) => { state.saved.add(trackId); return null; },
    unsave_track: ({ trackId }) => { state.saved.delete(trackId); return null; },
    liked_count: () => handlers.get_saved_tracks().total,
    get_saved_tracks: () => {
      const base = (fx.liked || {}).tracks || [];
      const baseIds = new Set(base.map((t) => t.id));
      const added = allTracks().filter((t) => state.saved.has(t.id) && !baseIds.has(t.id));
      const tracks = [...added, ...base.filter((t) => state.saved.has(t.id))];
      return { tracks: clone(tracks), total: ((fx.liked || {}).total || base.length) + state.saved.size - likedBase };
    },
    get_saved_albums: () => clone(fx.savedAlbums || []),

    // ---- v2: taste and artists ----
    get_top: ({ kind, range }) => {
      if (!(range in RANGES)) throw "mock: bad range " + range;
      const top = fx.top || {};
      if (kind === "tracks") return clone(rotate(top.tracks || [], RANGES[range]));
      if (kind === "artists") return clone(rotate(top.artists || [], RANGES[range]));
      throw "mock: bad kind " + kind;
    },
    get_artist: ({ artistId }) => {
      const page = (fx.artists || {})[artistId];
      if (page) return clone(page.artist);
      const tile = artistTiles().find((a) => a.id === artistId);
      if (tile) return clone(tile);
      for (const t of allTracks()) {
        const a = (t.artist_list || []).find((x) => x.id === artistId);
        if (a) return { id: a.id, name: a.name, image: t.cover || null };
      }
      throw "mock: 404 unknown artist " + artistId;
    },
    get_artist_albums: ({ artistId }) => {
      const page = (fx.artists || {})[artistId];
      if (page) return clone(page.albums);
      // Fallback: one album per distinct album name among this artist's tracks.
      const seen = new Map();
      for (const t of allTracks()) {
        if (!(t.artist_list || []).some((x) => x.id === artistId) || seen.has(t.album)) continue;
        seen.set(t.album, { id: "al_" + strHash(t.album).toString(36), name: t.album, cover: t.cover, year: "", kind: "album" });
      }
      return [...seen.values()];
    },
    get_followed_artists: () => clone(fx.followed || []),
    mix_info: ({ playlistId }) => {
      const info = (fx.mixInfo || {})[playlistId];
      if (!info) throw "mock: 404 Not Found (playlist " + playlistId + ")";
      return clone(info);
    },
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
  window.__mock = { scenario, state, invoke, advance, handlers }; // handlers: QA swaps one to inject a failure

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

  const XX = "3iOvXCl6edW5Um0fXEBRXy";
  async function drive() {
    if (scenario === "devices") (await waitFor("#deviceBtn"))?.click();
    if (["library-full", "artist", "mix-detail"].includes(scenario)) {
      (await waitFor("#libraryBtn"))?.click();
      if (scenario === "artist") {
        if (typeof window.__openArtist === "function") window.__openArtist(XX);
        else (await waitFor("#nowArtist .artist-link"))?.click();
      }
      if (scenario === "mix-detail") (await waitFor("#libMixes [data-mix]"))?.click();
    }
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
  if (["library", "library-detail", "search", "search-empty", "devices", "library-full", "artist", "mix-detail"].includes(scenario)) {
    window.addEventListener("load", () => setTimeout(drive, 300));
  }
})();
