// Browser-only stub of window.__TAURI__ for the dev harness. Never shipped.
// Backed by dev/fixture.json (real Spotify data from dev/capture.py).
// Scenario from ?s= : playing (default), paused, nothing, nodevice, login, reconnect,
// error, library, library-detail, search, search-empty, long-titles, ad,
// devices (3 devices incl. a restricted one, picker open), library-full (all Library groups),
// artist (The xx artist page open), mix-detail (first Spotify mix open),
// no-volume (the active device has no remote volume),
// engine-login (the in-app player needs its login; "The Run" shows up after engine_login, picker open),
// engine-down (the in-app player failed, picker open),
// slow (list commands and plays take 2s more: skeletons, the play spinner, "Starting…"),
// resume (the in-app player is ready, nothing plays, and a last session is stored: the app loads it paused),
// search-all (search "the xx", then the Songs "See all" page), library-all (Library, then the Albums "See all" page).
// search_page pages through a pool built from the fixture (search hits first, then every other known
// track / album): ~10 pages of songs, fewer of albums, so the last page and "no more" show up.
// The in-app player ("The Run") needs its login by default and isn't listed: engine_login lists it.
// The local_* commands model librespot's Spirc: they act at once, but only while The Run is the active
// device (an inactive Spirc ignores them); local_load activates it first. ENGINE_NOT_READY before ready.
// QA hook: window.__mock = { scenario, state, invoke, advance, handlers, media, calls, cache, emit, setEngine }.
//   media: recorded media_update / media_clear calls ({cmd, args, at}).
//   calls: every invoke, oldest first ({cmd, args, at}): local_* vs Web API routing shows here.
//   cache: the in-memory list cache ("<account>/<key>" → value) behind cache_get.
//   emit(event, payload): fires listeners from __TAURI__.event.listen (media-command, engine-status).
//   setEngine(state, reason?): sets the mock engine state and emits engine-status (e.g. "account_mismatch").
(function () {
  "use strict";

  const SCENARIOS = [
    "ended",
    "playing", "paused", "nothing", "nodevice", "login", "reconnect", "error",
    "library", "library-detail", "search", "search-empty", "long-titles", "ad",
    "devices", "library-full", "artist", "mix-detail", "no-volume", "engine-login", "engine-down",
    "slow", "resume", "search-all", "library-all",
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
  const RUN_ID = "dev_the_run"; // the in-app player's stable device id
  const ME = "dev_user"; // the /me id

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
    active: !["nothing", "nodevice", "resume"].includes(scenario),
    devices,
    deviceId: firstActive ? firstActive.id : null,
    shuffle: false,
    repeat: "off",
    contextUri: null,
    userQueued: 0, // user-added tracks at the front of the queue (Spotify plays them first, FIFO)
    saved: new Set(((fx.liked || {}).tracks || []).map((t) => t.id)),
    engine:
      scenario === "engine-login" ? { state: "needs_login", name: "The Run", device_id: null }
      : scenario === "engine-down" ? { state: "failed", name: "The Run", reason: "Spotify changed its protocol (mock)", device_id: null }
      : scenario === "resume" ? { state: "ready", name: "The Run", device_id: RUN_ID }
      : { state: "needs_login", name: "The Run", device_id: null }, // first run: the player isn't logged in yet
  };
  const likedBase = state.saved.size;
  if (state.queue.length && state.now && state.queue[0].uri === state.now.uri) state.queue.shift();
  if (scenario === "long-titles" && state.now) {
    state.now.name = LONG_TITLE.slice(0, 120).padEnd(120, ".");
    state.now.artists = "Someone With A Fairly Long Name, Another Featured Artist, And A Third";
    state.now.album = "A Deluxe Remastered Anniversary Edition With Bonus Tracks And Demos";
  }
  if (["nothing", "nodevice", "resume"].includes(scenario)) state.queue = [];
  if (scenario === "resume") {
    // what the last run saved: the 3rd song of the first captured playlist, 1:01 in, played from that playlist
    const [plId, rows] = Object.entries(fx.playlistTracks || {})[0] || [null, []];
    const uris = rows.map((t) => t.uri).slice(0, 200);
    if (uris.length) {
      localStorage.setItem("therun.lastSession", JSON.stringify({
        accountId: ME, contextUri: null, origin: { kind: "playlist", id: plId }, uris,
        trackUri: uris[Math.min(2, uris.length - 1)], positionMs: 61000, savedAt: Date.now() - 3600e3,
      }));
    }
  }

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
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  // the in-app player
  const media = []; // media_update / media_clear calls, oldest first
  let loginRunning = false;
  const THE_RUN = {
    id: RUN_ID, name: "The Run", type: "Computer",
    is_active: false, is_restricted: false, supports_volume: true, volume_percent: 50,
  };
  function setEngine(st, reason) {
    const device_id = st === "ready" ? RUN_ID : null; // the stable id, once ready
    state.engine = reason ? { state: st, name: "The Run", reason, device_id } : { state: st, name: "The Run", device_id };
    emit("engine-status", state.engine);
  }
  function listTheRun() {
    if (!state.devices.some((d) => d.id === THE_RUN.id)) state.devices.push(clone(THE_RUN));
  }
  if (scenario === "resume") listTheRun(); // ready: Spotify lists it

  // ---- the in-app player's own commands (librespot Spirc) ----
  const engineReady = () => {
    if (state.engine.state !== "ready") throw "ENGINE_NOT_READY: the player isn't ready (mock)";
  };
  // Spirc ignores everything but load while The Run isn't the active device
  const runActive = () => state.active && state.deviceId === RUN_ID;
  const spirc = (fn) => () => {
    engineReady();
    if (runActive()) fn();
    return null;
  };
  /** The tracks a context plays: the captured playlist/album rows, else a stable pick from the pool. */
  function contextTracks(contextUri) {
    const [, kind, id] = String(contextUri).split(":");
    const rows = kind === "playlist" ? (fx.playlistTracks || {})[id] : kind === "album" ? (fx.albumTracks || {})[id] : null;
    if (rows && rows.length) return rows.map(clone);
    const pool = ((fx.liked || {}).tracks || []).length ? fx.liked.tracks : allTracks();
    if (!pool.length) throw "mock: no tracks";
    return rotate(pool, strHash(contextUri) % pool.length).slice(0, 11).map(clone);
  }
  /** Start tracks at trackUri (the top when it isn't there, like Spotify), from positionMs. */
  function loadTracks(tracks, { trackUri, contextUri = null, positionMs = 0, play = true }) {
    const at = Math.max(0, trackUri ? tracks.findIndex((t) => t.uri === trackUri) : 0);
    pushHistory(state.now);
    state.now = tracks[at];
    state.queue = tracks.slice(at + 1, at + 21);
    state.userQueued = 0;
    state.contextUri = contextUri;
    state.active = true;
    state.isPlaying = !!play;
    setProgress(positionMs || 0);
  }
  let ended = 0; // scenario "ended": polls seen
  const setVol = (d, percent) => (d.volume_percent = Math.max(0, Math.min(100, Math.round(Number(percent) || 0))));
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
  // search_page pools: the captured search hits first, then every other known track / album
  const TRACK_POOL_MAX = 97; // not a round number of pages: the last page is short
  const trackPool = () => {
    const first = ((fx.search || {}).tracks || []);
    const seen = new Set(first.map((t) => t.uri));
    return first.concat(allTracks().filter((t) => !seen.has(t.uri))).slice(0, TRACK_POOL_MAX);
  };
  const albumPool = () => {
    const out = [];
    const seen = new Set();
    const add = (a, artists) => {
      if (!a || !a.id || seen.has(a.id)) return;
      seen.add(a.id);
      out.push({ id: a.id, name: a.name, artists: a.artists || artists || "", cover: a.cover || null });
    };
    ((fx.search || {}).albums || []).forEach((a) => add(a));
    (fx.savedAlbums || []).forEach((a) => add(a));
    Object.values(fx.artists || {}).forEach((pg) => (pg.albums || []).forEach((a) => add(a, pg.artist && pg.artist.name)));
    return repeatTo(out, ALBUM_POOL_MAX);
  };
  const ALBUM_POOL_MAX = 46; // the fixture holds ~20 distinct albums: repeated (new ids) to page past 30
  /** list repeated up to n items; a repeat gets a new id (`<id>_r<k>`) so it isn't a duplicate. */
  const repeatTo = (list, n) => {
    const out = [];
    for (let k = 0; list.length && out.length < n; k++) {
      for (const a of list) {
        if (out.length >= n) break;
        out.push(k ? { ...a, id: `${a.id}_r${k}` } : a);
      }
    }
    return out;
  };

  const handlers = {
    auth_status: () => (scenario === "login" ? "login" : scenario === "reconnect" ? "reconnect" : "ok"),
    login: () => null,

    // a snapshot id per playlist, like Spotify's: the playlist cache is keyed by it
    get_playlists: () => clone(fx.playlists || []).map((p) => ({ snapshot_id: "snap_" + p.id, ...p })),
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
    // one page of 10 (Spotify's max on /search); has_more like the backend: a full page under the 1000 cap
    search_page: ({ query, kind, offset }) => {
      void query;
      if (kind !== "track" && kind !== "album") throw "BAD_ARGS: unknown search kind " + kind;
      if (scenario === "search-empty") return { items: [], has_more: false };
      const pool = kind === "track" ? trackPool() : albumPool();
      const items = pool.slice(offset, offset + 10);
      return { items: clone(items), has_more: items.length === 10 && offset + 10 < 1000 && offset + 10 < pool.length };
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

    app_log: () => null,
    play_on_device: ({ deviceId, uris }) => {
      useDevice(deviceId);
      const tracks = (uris || []).map((u) => byUri.get(u)).filter(Boolean).map(clone);
      if (!tracks.length) throw "mock: unknown uris";
      loadTracks(tracks, { trackUri: tracks[0].uri });
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

    // ---- snappy: the in-app player directly, the account, the list cache ----
    local_play: spirc(() => { setProgress(progress()); state.isPlaying = true; }),
    local_pause: spirc(() => { setProgress(progress()); state.isPlaying = false; }),
    local_next: spirc(() => advance()),
    local_prev: spirc(() => back()),
    local_seek: ({ positionMs }) => spirc(() => setProgress(Math.max(0, Number(positionMs) || 0)))(),
    local_volume: ({ percent }) => spirc(() => {
      const d = activeDevice();
      if (d) setVol(d, percent);
    })(),
    // activates The Run, then loads: Ok only means queued (the poll shows the result)
    local_load: ({ contextUri, uris, trackUri, positionMs, play }) => {
      engineReady();
      if (!!contextUri === !!(uris && uris.length)) throw "BAD_ARGS: exactly one of contextUri and uris (mock)";
      if (uris && uris.length > 200) throw "BAD_ARGS: more than 200 uris (mock)";
      const tracks = contextUri ? contextTracks(contextUri) : uris.map((u) => byUri.get(u)).filter(Boolean).map(clone);
      if (!tracks.length) throw "mock: unknown uris";
      listTheRun();
      setProgress(progress());
      state.deviceId = RUN_ID;
      loadTracks(tracks, { trackUri, contextUri: contextUri || null, positionMs, play });
      return null;
    },
    me_id: () => ME,
    cache_get: ({ account, key }) => clone(cache.get(`${account}/${key}`) ?? null),
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
      setVol(d, percent);
      return null;
    },
    set_shuffle: ({ on }) => { needDevice(); state.shuffle = !!on; return null; },
    set_repeat: ({ mode }) => {
      needDevice();
      if (!["off", "context", "track"].includes(mode)) throw "mock: bad repeat mode " + mode;
      state.repeat = mode;
      return null;
    },
    // trackUri: the offset (start there, like {offset: {uri}}); Spotify starts from the top when it isn't in the context
    play_context: ({ deviceId, contextUri, trackUri }) => {
      useDevice(deviceId);
      if (!contextUri) throw "mock: no context uri";
      loadTracks(contextTracks(contextUri), { trackUri, contextUri });
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
    // library-all: enough saved albums for "See all" (the fixture has 3), from the search pool
    get_saved_albums: () =>
      clone(scenario === "library-all" ? albumPool().slice(0, 26) : fx.savedAlbums || []),

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
    // ---- standalone: the in-app player ("The Run") and OS media controls ----
    engine_status: () => clone(state.engine),
    // resolves once logged in and ready; "The Run" registers with Spotify a moment later
    engine_login: async () => {
      if (loginRunning) throw "LOGIN_IN_PROGRESS: a player login is already running (mock)";
      if (state.engine.state === "failed") throw "mock: the player failed: " + state.engine.reason;
      loginRunning = true;
      try {
        setEngine("starting");
        await sleep(1500); // the browser login
        setEngine("ready");
        setTimeout(listTheRun, 1000);
        return null;
      } finally {
        loginRunning = false;
      }
    },
    engine_restart: () => {
      if (state.engine.state === "ready") {
        setEngine("starting");
        setTimeout(() => setEngine("ready"), 800);
      }
      return null;
    },
    media_update: (args) => { media.push({ cmd: "media_update", args: clone(args), at: Date.now() }); return null; },
    media_clear: () => { media.push({ cmd: "media_clear", args: null, at: Date.now() }); return null; },

    mix_info: ({ playlistId }) => {
      const info = (fx.mixInfo || {})[playlistId];
      if (!info) throw "mock: 404 Not Found (playlist " + playlistId + ")";
      return clone(info);
    },
  };

  // local commands (the engine, the in-app player, media controls, the disk cache) don't need the network
  const LOCAL = /^(auth_status|engine_|media_|local_|cache_get$)/;
  // `slow`: lists and plays take 2s more
  const SLOW = /^(get_playlists|get_playlist_tracks|get_album_tracks|get_saved_|get_followed_artists|get_top|get_artist|search|liked_count|play_on_device|play_context|local_load|resume|transfer_playback)/;

  // the backend's list cache: what each list command writes (and, for snapshots and albums, reads)
  const cache = new Map();
  const CACHE_KEYS = {
    get_playlists: () => "playlists",
    get_saved_tracks: () => "liked",
    get_saved_albums: () => "albums",
    get_followed_artists: () => "following",
    get_top: (a) => `top:${a.kind}:${a.range}:${Math.min(50, Math.max(1, a.limit || 20))}`,
    get_album_tracks: (a) => `album:${a.albumId}`,
    get_playlist_tracks: (a) => (a.snapshotId ? `playlist:${a.playlistId}:${a.snapshotId}` : null),
  };
  const READS_CACHE = new Set(["get_album_tracks", "get_playlist_tracks"]); // a hit makes no request

  const calls = []; // every invoke, oldest first

  async function invoke(cmd, args) {
    args = args || {};
    calls.push({ cmd, args: clone(args), at: Date.now() });
    await sleep(40); // feel async, like IPC
    const h = handlers[cmd];
    if (!h) return reject(`mock: unknown command ${cmd}`);
    const key = args.account && CACHE_KEYS[cmd] ? CACHE_KEYS[cmd](args) : null;
    const slot = key && `${args.account}/${key}`;
    if (slot && READS_CACHE.has(cmd) && cache.has(slot)) return clone(cache.get(slot));
    if (scenario === "error" && !LOCAL.test(cmd)) return reject("network down");
    // the login ends after the first poll: the "session ended" screen
    if (scenario === "ended" && cmd === "playback_state" && (ended = ended + 1) > 1) return reject("AUTH_EXPIRED: session ended (mock)");
    if (scenario === "slow" && SLOW.test(cmd)) await sleep(2000);
    try {
      const out = await h(args);
      if (slot) cache.set(slot, clone(out));
      return out;
    } catch (e) {
      return reject(String(e));
    }
  }

  // __TAURI__.event: listen(name, cb) → Promise<unlisten>; cb gets {event, id, payload} like Tauri's Event
  const listeners = new Map(); // name → Set of callbacks
  let eventId = 0;
  function listen(name, cb) {
    if (!listeners.has(name)) listeners.set(name, new Set());
    listeners.get(name).add(cb);
    return Promise.resolve(() => listeners.get(name).delete(cb));
  }
  function emit(name, payload) {
    for (const cb of [...(listeners.get(name) || [])]) cb({ event: name, id: ++eventId, payload: clone(payload) });
  }

  window.__TAURI__ = { core: { invoke }, event: { listen } };
  // handlers: QA swaps one to inject a failure
  window.__mock = { scenario, state, invoke, advance, handlers, media, calls, cache, emit, setEngine };

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
    if (["devices", "engine-login", "engine-down"].includes(scenario)) (await waitFor("#deviceBtn"))?.click();
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
        const row = await waitFor("#libList .row:not(.is-skeleton)");
        row?.click();
      }
    }
    if (scenario === "search" || scenario === "search-empty" || scenario === "search-all") {
      (await waitFor("#searchBtn"))?.click();
      const input = await waitFor("#searchInput");
      if (input) {
        input.value = scenario === "search-empty" ? "zzqx nothing" : (fx.search && fx.search.query) || "the xx";
        input.dispatchEvent(new Event("input", { bubbles: true }));
      }
      if (scenario === "search-all") (await waitFor('#searchResults [data-see="track"]'))?.click();
    }
    if (scenario === "library-all") {
      (await waitFor("#libraryBtn"))?.click();
      (await waitFor('#libAlbums [data-see="albums"]:not([hidden])'))?.click();
    }
  }
  const DRIVEN = ["library", "library-detail", "search", "search-empty", "search-all", "library-all", "devices", "library-full", "artist", "mix-detail", "engine-login", "engine-down"];
  if (DRIVEN.includes(scenario)) {
    window.addEventListener("load", () => setTimeout(drive, 300));
  }
})();
