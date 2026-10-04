// Browser-only stub of window.__TAURI__ for the dev harness. Never shipped.
// Backed by dev/fixture.json (real Spotify data from dev/capture.py).
// Scenario from ?s= : playing (default), paused, nothing, nodevice, login, reconnect,
// error, library, library-detail, search, search-empty, long-titles, ad,
// devices (3 devices incl. a restricted one, picker open), library-full (all Library groups),
// artist (The xx artist page open, with its Popular tracks), mix-detail (first Spotify mix open),
// no-volume (the active device has no remote volume),
// engine-login (the in-app player needs its login; "This Mac" (shown as "Here") shows up after engine_login, picker open),
// engine-down (the in-app player failed, picker open),
// slow (list commands and plays take 2s more: skeletons, the play spinner, "Starting…"),
// resume (the in-app player is ready, nothing plays, and a saved session exists: session_get returns it and
//   "Rust" loads it back paused 2.5s after launch, then emits session-restored),
// search-all (search "the xx", then the Songs "See all" page), library-all (Library, then its Albums tab, 26 albums),
// playlist (playing the 5th song of the first captured playlist, as its context: the playlist panel lists it),
// here (the in-app player is ready and plays the playlist: device "Here", quality changes restart it;
//   session_get returns that play; Rust's player-state events drive the UI, so no playback_state poll),
// ratelimited (like here, but Spotify rate-limited the app's Web API: commands with no other source (remote
//   playback, playback_state) reject with RATE_LIMITED:50000:…, api_status says so; the rest are served as
//   by Spotify's internal API; the disk cache and the store's account are from the last run),
// ratelimited-down (ratelimited, and the internal API fails too: every network command rejects with
//   RATE_LIMITED, as Rust reports when its Web API fallback is blocked).
// player-state: emitted after every command that changes what the in-app player plays, and when its
//   track ends; local_state returns the same payload (playback_state's shape + engine_active + queue).
// The store (store_all / store_set, Rust's state.json) is in memory: ?store=solo seeds settings with the cover row off.
// Mixes and added links (Rust library.rs): mixes_list = added (store savedLinks) + 5 Made For You + seen playing (knownMixes);
//   link_resolve / link_save know the two share-link test playlists (Discover Weekly, Moderat Radio), one other user's
//   playlist (1A2b3C4d5E6f7G8h9I0jKl), the fixture's albums, followed artists and search songs; saves emit library-changed.
// MCP (Rust mcp.rs): mcp_* commands keep an in-memory state; ?mcp=busy makes the server fail with "port 5590 is in use".
// search_page pages through a pool built from the fixture (search hits first, then every other known
// track / album): ~10 pages of songs, fewer of albums, so the last page and "no more" show up.
// The in-app player (Spotify Connect name "This Mac", shown as "Here") needs its login by default and
// isn't listed: engine_login lists it. The local_* commands model librespot's Spirc: they act at once, but
// only while the player is the active device (an inactive Spirc ignores them); local_load activates it
// first. ENGINE_NOT_READY before ready. engine_set_quality restarts it (starting → ready, playback dropped).
// QA hook: window.__mock = { scenario, state, invoke, advance, handlers, media, dockArt, mini, calls, cache, store, emit, setEngine }.
//   store: the in-memory key-value store behind store_all / store_set; logs: app_log lines ("level msg").
//   media: recorded media_update / media_clear calls ({cmd, args, at}).
//   dockArt: recorded set_dock_art urls (null = the app's own icon), oldest first.
//   mini: recorded mini_push payloads and tray_config calls ({cmd, args}), oldest first; emit("mini-command", {action})
//     plays a menu-bar button.
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
    "slow", "resume", "search-all", "library-all", "playlist", "here", "ratelimited", "ratelimited-down",
  ];
  const requested = new URLSearchParams(location.search).get("s") || "playing";
  const scenario = SCENARIOS.includes(requested) ? requested : "playing";
  const limited = scenario === "ratelimited" || scenario === "ratelimited-down";
  const hereLike = scenario === "here" || limited; // the in-app player plays
  const RATE_LIMIT = "RATE_LIMITED:50000: Spotify paused this app's library access";
  if (scenario !== requested) console.warn(`mock: unknown scenario "${requested}", using "playing"`);

  // Synchronous load so invoke is ready before app.js runs.
  const xhr = new XMLHttpRequest();
  xhr.open("GET", "fixture.json", false);
  xhr.send();
  if (xhr.status !== 200) throw new Error("mock: cannot load dev/fixture.json (" + xhr.status + ")");
  const fx = JSON.parse(xhr.responseText);

  const clone = (v) => (v == null ? v : JSON.parse(JSON.stringify(v)));
  const RUN_ID = "dev_the_run"; // the in-app player's stable device id
  const RUN_NAME = "This Mac"; // its Spotify Connect name (the app shows it as "Here")
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
      scenario === "engine-login" ? { state: "needs_login", name: RUN_NAME, device_id: null }
      : scenario === "engine-down" ? { state: "failed", name: RUN_NAME, reason: "Spotify changed its protocol (mock)", device_id: null }
      : scenario === "resume" || hereLike ? { state: "ready", name: RUN_NAME, device_id: RUN_ID }
      : { state: "needs_login", name: RUN_NAME, device_id: null }, // first run: the player isn't logged in yet
    quality: 160, // the in-app player's bitrate, kbps
  };
  const likedBase = state.saved.size;
  if (state.queue.length && state.now && state.queue[0].uri === state.now.uri) state.queue.shift();
  if (scenario === "long-titles" && state.now) {
    state.now.name = LONG_TITLE.slice(0, 120).padEnd(120, ".");
    state.now.artists = "Someone With A Fairly Long Name, Another Featured Artist, And A Third";
    state.now.album = "A Deluxe Remastered Anniversary Edition With Bonus Tracks And Demos";
  }
  if (["nothing", "nodevice", "resume"].includes(scenario)) state.queue = [];
  if (scenario === "playlist" || hereLike) {
    // the 5th song of the first captured playlist, played as that playlist (context)
    const [plId, rows] = Object.entries(fx.playlistTracks || {})[0] || [null, []];
    if (rows.length) {
      const at = Math.min(4, rows.length - 1);
      state.now = clone(rows[at]);
      state.queue = clone(rows.slice(at + 1));
      state.contextUri = "spotify:playlist:" + plId;
      state.history = rows.slice(0, at).reverse().map((t, i) => ({ track: clone(t), played_at: new Date(Date.now() - (i + 1) * 200e3).toISOString(), context_uri: state.contextUri })).concat(state.history);
    }
  }
  // Rust's saved session (session.json): resume = the 3rd song of the first captured playlist, 1:01 in,
  // played from that playlist; here = what plays now
  let savedSession = null;
  if (scenario === "resume") {
    const [plId, rows] = Object.entries(fx.playlistTracks || {})[0] || [null, []];
    if (rows.length) {
      savedSession = {
        contextUri: "spotify:playlist:" + plId, uris: null, trackUri: rows[Math.min(2, rows.length - 1)].uri,
        positionMs: 61000, shuffle: false, repeat: "off", volume: 32768,
      };
    }
  }
  if (hereLike && state.now) {
    savedSession = { contextUri: state.contextUri, uris: null, trackUri: state.now.uri, positionMs: 44000, shuffle: false, repeat: "off", volume: 32768 };
  }
  // the store (state.json), in memory
  const store = {};
  if (new URLSearchParams(location.search).get("store") === "solo") store.settings = { dockArt: true, coverRow: false };
  if (limited) store.account = ME; // the last run saw the account

  const iso = () => new Date().toISOString();

  // the MCP server's state (Rust mcp.rs)
  const mcp = { enabled: false, key: null, calls: 3, busy: new URLSearchParams(location.search).get("mcp") === "busy" };
  const mcpStatus = () => ({
    enabled: mcp.enabled, running: mcp.enabled && !mcp.busy, port: 5590,
    error: mcp.enabled && mcp.busy ? "port 5590 is in use" : null, callsToday: mcp.enabled && !mcp.busy ? mcp.calls : 0,
  });

  // Made For You on the home feed (Rust: pathfinder home), and what pasted links resolve to (library.rs)
  const albumCover = ((fx.savedAlbums || [])[0] || {}).cover || null;
  const HOME_MIXES = [
    { id: "37i9dQZF1E4yLltmVk3nyb", name: "Bonobo Radio", cover: ((fx.mixInfo || {})["37i9dQZF1E4yLltmVk3nyb"] || {}).cover || null },
    { id: "37i9dQZEVXcVV9hd3iqSgp", name: "Discover Weekly", cover: albumCover },
    { id: "37i9dQZF1E383PNAIaEkqr", name: "Daily Mix 1", cover: null },
    { id: "37i9dQZF1E38wXcuDypD19", name: "Daily Mix 2", cover: null },
    { id: "37i9dQZEVXbqEuNs4QsXYB", name: "Release Radar", cover: null },
  ];
  const LINKED = {
    "playlist:37i9dQZEVXcVV9hd3iqSgp": { name: "Discover Weekly", cover: albumCover, owner: "Spotify", owner_id: "spotify", total: 30 },
    "playlist:37i9dQZF1E4qxgJU46pFLr": { name: "Moderat Radio", cover: null, owner: "Spotify", owner_id: "spotify", total: 50 },
    "playlist:1A2b3C4d5E6f7G8h9I0jKl": { name: "Alex's road trip", cover: null, owner: "Alex", owner_id: "alex", total: 12 },
  };
  /** A pasted link → link_resolve's answer, like Rust's (errors are the same sentences). */
  function resolveLink(text) {
    const m = /(playlist|album|artist|track)[/:]([A-Za-z0-9]{22})/.exec(String(text || ""));
    if (!m) throw "That link isn't a Spotify playlist, album, artist or song";
    const [, kind, id] = m;
    const uri = `spotify:${kind}:${id}`;
    const saved = (store.savedLinks || []).some((l) => l.uri === uri);
    if (kind === "playlist") {
      const own = (fx.playlists || []).find((p) => p.id === id);
      const info = own ? { name: own.name, cover: ((own.images || [])[0] || {}).url || null, owner: "You", owner_id: ME, total: (own.tracks || {}).total || 0 } : LINKED[`playlist:${id}`];
      if (!info) throw "Spotify didn't return this playlist";
      const tab = /^37i9/.test(id) || info.owner_id === "spotify" ? "mixes" : "playlists";
      return { kind, id, uri, ...clone(info), tab, saved };
    }
    if (kind === "album") {
      const a = (fx.savedAlbums || []).find((x) => x.id === id) || (((fx.search || {}).albums) || []).find((x) => x.id === id);
      if (!a) throw "Spotify didn't return this album";
      return { kind, id, uri, name: a.name, cover: a.cover, artists: a.artists, total: a.total_tracks || null, tab: "albums", saved };
    }
    if (kind === "artist") {
      const a = (fx.followed || []).find((x) => x.id === id);
      if (!a) throw "Spotify didn't return this artist";
      return { kind, id, uri, name: a.name, cover: a.image, tab: "artists", saved };
    }
    const t = [...(((fx.search || {}).tracks) || []), ...(fx.queue || [])].find((x) => x.id === id);
    if (!t) throw "Spotify didn't return this song";
    return { kind, id, uri, name: t.name, track: clone(t) };
  }
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
    id: RUN_ID, name: RUN_NAME, type: "Computer",
    is_active: false, is_restricted: false, supports_volume: true, volume_percent: 50,
  };
  const dockArt = []; // set_dock_art urls, oldest first
  const mini = []; // mini_push / tray_config calls, oldest first
  function setEngine(st, reason) {
    const device_id = st === "ready" ? RUN_ID : null; // the stable id, once ready
    state.engine = reason ? { state: st, name: RUN_NAME, reason, device_id } : { state: st, name: RUN_NAME, device_id };
    emit("engine-status", state.engine);
    emitLocal();
  }
  function listTheRun() {
    if (!state.devices.some((d) => d.id === THE_RUN.id)) state.devices.push(clone(THE_RUN));
  }
  if (scenario === "resume" || hereLike) listTheRun(); // ready: Spotify lists it
  if (hereLike) state.deviceId = RUN_ID; // and plays on it

  // ---- the in-app player's own commands (librespot Spirc) ----
  const engineReady = () => {
    if (state.engine.state !== "ready") throw "ENGINE_NOT_READY: the player isn't ready (mock)";
  };
  // Spirc ignores everything but load while the player isn't the active device
  const runActive = () => state.active && state.deviceId === RUN_ID;
  // Rust's player-state payload (nowplaying.rs): playback_state's shape, plus engine_active and queue
  const localPayload = () => {
    if (state.engine.state !== "ready" || !runActive()) return { active: false, engine_active: false };
    if (!state.now) return { active: false, engine_active: true };
    return {
      active: true, engine_active: true, is_playing: state.isPlaying, progress_ms: progress(),
      ...playerExtras(), track: clone(state.now), queue: clone(state.queue.slice(0, 20)),
    };
  };
  let localSeen = false; // local_state is null before the player's first event
  let wasRun = false; // the in-app player was active at the last emit
  function emitLocal() {
    const run = state.engine.state === "ready" && runActive();
    if (!run && !wasRun) return; // another device: the in-app player has nothing to say
    wasRun = run;
    localSeen = true;
    emit("player-state", localPayload());
  }
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
    // the back of the current cover's sleeve: OK Computer's facts under the song's own album name
    get_album_info: ({ trackId }) => {
      if (scenario === "error") throw "network down";
      const all = [state.now, ...state.queue, ...state.history.map((h) => h.track)];
      const t = all.find((x) => x && String(x.uri).endsWith(`:${trackId}`)) || {};
      return {
        id: "6dVIqQ8qmQ5GBnJ9shOYGE",
        name: t.album || "OK Computer",
        artists: t.artists || "Radiohead",
        type: "album",
        release_date: "1997-05-21",
        release_precision: "day",
        total_tracks: 12,
        duration_ms: 3221223,
        label: "XL Recordings",
        copyrights: [
          { text: "1997 Radiohead under exclusive licence to XL Recordings Ltd", type: "C" },
          { text: "1997 Radiohead under exclusive licence to XL Recordings Ltd", type: "P" },
        ],
      };
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

    app_log: ({ level, msg }) => { logs.push(`${level} ${msg}`); return null; },
    copy_text: ({ text }) => { copied.push(text); return null; },
    store_all: () => clone(store),
    store_set: ({ key, value }) => {
      if (typeof key !== "string" || !key) throw "BAD_ARGS: key (mock)";
      if (value == null) delete store[key];
      else store[key] = clone(value);
      return null;
    },
    session_get: () => clone(savedSession),
    local_state: () => (localSeen || runActive() ? localPayload() : null),
    api_status: () => ({
      blockedForSecs: limited ? 50000 : 0,
    }),
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
    local_shuffle: ({ on }) => spirc(() => (state.shuffle = !!on))(),
    local_repeat: ({ mode }) => {
      if (!["off", "context", "track"].includes(mode)) throw "BAD_ARGS: bad repeat mode " + mode;
      return spirc(() => (state.repeat = mode))();
    },
    // activates the player, then loads: Ok only means queued (the poll shows the result)
    local_load: ({ spec: { contextUri, uris, trackUri, positionMs, play } }) => {
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
      // the captured artist page has Spotify's popular tracks (internal endpoints); the rest have
      // none, as from the Web API, so the page falls back to "Your favorites"
      if (page) {
        const popular = allTracks().filter((t) => (t.artist_list || []).some((x) => x.id === artistId));
        const seen = new Set();
        return { ...clone(page.artist), top_tracks: clone(popular.filter((t) => !seen.has(t.uri) && seen.add(t.uri)).slice(0, 10)) };
      }
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
    // ---- standalone: the in-app player ("This Mac", shown as "Here") and OS media controls ----
    engine_status: () => clone(state.engine),
    // resolves once logged in and ready; it registers with Spotify a moment later
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
    // the bitrate restarts the player: starting → ready a moment later; whatever it played stops
    engine_get_quality: () => state.quality,
    engine_set_quality: ({ kbps }) => {
      if (![96, 160, 320].includes(kbps)) throw "BAD_ARGS: kbps must be 96, 160 or 320 (mock)";
      state.quality = kbps;
      if (state.engine.state === "ready") {
        setTimeout(() => {
          setEngine("starting");
          if (state.deviceId === RUN_ID) { setProgress(progress()); state.active = false; state.isPlaying = false; }
          setTimeout(() => setEngine("ready"), 1200);
        }, 30);
      }
      return null;
    },
    set_dock_art: ({ url }) => { dockArt.push(url == null ? null : String(url)); return null; },
    mini_push: (args) => { mini.push({ cmd: "mini_push", args: clone(args) }); return null; },
    tray_config: (args) => { mini.push({ cmd: "tray_config", args: clone(args) }); return null; },
    media_update: (args) => { media.push({ cmd: "media_update", args: clone(args), at: Date.now() }); return null; },
    media_clear: () => { media.push({ cmd: "media_clear", args: null, at: Date.now() }); return null; },

    mix_info: ({ playlistId }) => {
      const info = (fx.mixInfo || {})[playlistId];
      if (!info) throw "mock: 404 Not Found (playlist " + playlistId + ")";
      return clone(info);
    },

    // the Mixes tab (Rust library.rs): added links (tab mixes), the home feed's Made For You, then seen playing
    mixes_list: () => {
      const out = [];
      const push = (id, name, cover, source) => {
        if (id && !out.some((m) => m.id === id)) out.push({ id, uri: "spotify:playlist:" + id, name, cover, source });
      };
      for (const l of store.savedLinks || []) if (l.tab === "mixes") push(l.id, l.name, l.cover, "added");
      for (const m of HOME_MIXES) push(m.id, m.name, m.cover, "made_for_you");
      for (const m of store.knownMixes || []) {
        const info = (fx.mixInfo || {})[m.id] || {};
        push(m.id, info.name || "Spotify mix", info.cover || null, "played");
      }
      return out;
    },
    links_list: () => clone(store.savedLinks || []),
    link_resolve: ({ link }) => resolveLink(link),
    link_save: ({ link }) => {
      const info = resolveLink(link);
      if (info.kind === "track") throw "A song can't be added to the library here: open it or play it instead";
      const list = store.savedLinks || [];
      const old = list.find((l) => l.uri === info.uri);
      if (old) return { item: clone(old), already: true };
      const item = { kind: info.kind, id: info.id, uri: info.uri, name: info.name, cover: info.cover, owner: info.owner || null,
        owner_id: info.owner_id || null, artists: info.artists || null, total: info.total || null, tab: info.tab, added: Math.floor(Date.now() / 1000) };
      store.savedLinks = [item, ...list];
      setTimeout(() => emit("library-changed", null), 0);
      return { item: clone(item), already: false };
    },
    link_remove: ({ uri }) => {
      const list = store.savedLinks || [];
      const next = list.filter((l) => l.uri !== uri);
      store.savedLinks = next;
      if (next.length !== list.length) setTimeout(() => emit("library-changed", null), 0);
      return next.length !== list.length;
    },

    // the local MCP server (Rust mcp.rs): ?mcp=busy says port 5590 is taken
    mcp_status: () => mcpStatus(),
    mcp_set_enabled: ({ on }) => {
      mcp.enabled = !!on;
      if (on && !mcp.key) mcp.key = "mock-key-0123456789abcdefghijklmnopqrstuvwxyzAB";
      return mcpStatus();
    },
    mcp_reset_key: () => {
      mcp.key = "mock-key-" + Math.random().toString(36).slice(2).padEnd(34, "x");
      return mcpStatus();
    },
    mcp_connect_text: ({ format }) => {
      if (!mcp.key) mcp.key = "mock-key-0123456789abcdefghijklmnopqrstuvwxyzAB";
      const url = "http://127.0.0.1:5590/mcp";
      if (format === "json") return JSON.stringify({ mcpServers: { stylus: { type: "http", url, headers: { Authorization: "Bearer " + mcp.key } } } }, null, 2);
      if (format === "claude") return `claude mcp add --scope user --transport http stylus ${url} --header "Authorization: Bearer ${mcp.key}"`;
      throw "BAD_ARGS: unknown format " + format;
    },
    mcp_skill_text: () => "---\nname: stylus\ndescription: Control the Stylus Spotify player (mock)\n---\n",
  };

  // local commands (the engine, the in-app player, media controls, the disk cache) don't need the network
  const LOCAL = /^(auth_status|login$|engine_|media_|local_|cache_get$|set_dock_art$|mini_|tray_|store_|session_get$|app_log$|copy_text$|api_status$|mcp_|links_list$|link_remove$)/;
  // commands with no source but the Web API (Rust's spotify.rs): Rust refuses these while rate-limited
  const WEB_ONLY = /^(playback_state|transfer_playback|set_volume|set_shuffle|set_repeat|play_context|play_on_device|resume|resume_at|pause|next_track|previous_track|seek)$/;
  // commands that can change what the in-app player plays: a player-state follows them
  const CHANGES_PLAYER = /^(local_|play_|resume|pause$|next_track$|previous_track$|seek$|transfer_playback$|set_(volume|shuffle|repeat)$|add_to_queue$|engine_)/;
  // `slow`: lists and plays take 2s more
  const SLOW = /^(get_playlists|get_playlist_tracks|get_album_tracks|get_album_info|get_saved_|get_followed_artists|get_top|get_artist|search|liked_count|play_on_device|play_context|local_load|resume|transfer_playback)/;

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
  const logs = []; // app_log lines, oldest first
  const copied = []; // copy_text texts, oldest first

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
    if (limited && (scenario === "ratelimited-down" ? !LOCAL.test(cmd) : WEB_ONLY.test(cmd))) return reject(RATE_LIMIT);
    // the login ends after the first poll: the "session ended" screen
    if (scenario === "ended" && cmd === "playback_state" && (ended = ended + 1) > 1) return reject("AUTH_EXPIRED: session ended (mock)");
    if (scenario === "slow" && SLOW.test(cmd)) await sleep(2000);
    try {
      const out = await h(args);
      if (slot) cache.set(slot, clone(out));
      return out;
    } catch (e) {
      return reject(String(e));
    } finally {
      if (CHANGES_PLAYER.test(cmd)) emitLocal();
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
  // resume: the disk cache from the last run holds the playlists and that playlist: the restored song shows by name
  if (scenario === "resume" && savedSession) {
    const plId = savedSession.contextUri.split(":")[2];
    cache.set(`${ME}/playlists`, handlers.get_playlists());
    cache.set(`${ME}/playlist:${plId}:snap_${plId}`, clone((fx.playlistTracks || {})[plId] || []));
  }
  // ratelimited: the disk cache from the last run (playlists, the first playlist, liked songs)
  if (limited) {
    cache.set(`${ME}/playlists`, handlers.get_playlists());
    for (const [plId, rows] of Object.entries(fx.playlistTracks || {})) cache.set(`${ME}/playlist:${plId}:snap_${plId}`, clone(rows));
    cache.set(`${ME}/liked`, handlers.get_saved_tracks());
  }
  // the in-app player's track ends by itself: librespot plays the next one and says so (no poll drives it)
  setInterval(() => {
    if (state.engine.state !== "ready" || !runActive() || !state.now || !state.now.duration_ms) return;
    if (state.isPlaying && progress() >= state.now.duration_ms) {
      advance();
      emitLocal();
    }
  }, 500);
  if (hereLike) localSeen = true; // the player is up and playing: it has spoken
  window.__mock = { scenario, state, invoke, advance, handlers, media, dockArt, mini, calls, cache, store, logs, emit, setEngine, emitLocal };

  // resume: Rust loads the saved session back (paused) once the player is up, then tells the UI
  if (scenario === "resume" && savedSession) {
    setTimeout(() => {
      handlers.local_load({ spec: { ...savedSession, uris: undefined, play: false } });
      emit("session-restored", savedSession);
    }, 2500);
  }

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
      (await waitFor("#libTab-albums:not([hidden])"))?.click();
    }
  }
  const DRIVEN = ["library", "library-detail", "search", "search-empty", "search-all", "library-all", "devices", "library-full", "artist", "mix-detail", "engine-login", "engine-down"];
  if (DRIVEN.includes(scenario)) {
    window.addEventListener("load", () => setTimeout(drive, 300));
  }
})();
