// The Run — stage UI: boot, sequential poll loop, the run of covers, transport.
import { fmtTime, esc } from "./lib/format.js";
import { FALLBACK, extractColors } from "./lib/color.js";
import { buildRun, measure, flip } from "./lib/timeline.js";

const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

const POLL_MS = 1000;
const ERROR_POLL_MS = 4000;
const QUEUE_EVERY = 10; // ticks
const HOLD_MS = 1500; // ignore polled is_playing right after a play/pause click
const SESSION_MATCH_MS = 15 * 60 * 1000; // a session row this close to an API row is the same play
const small = matchMedia("(max-width: 899px)");

const state = {
  auth: null,
  loginKind: "login",
  device: null, // {id, name}
  devices: null, // last list_devices result, null = unknown
  now: null,
  isPlaying: false,
  progressMs: 0,
  progressAt: 0,
  holdUntil: 0,
  queue: [],
  recent: [], // get_recently_played, newest first
  session: [], // tracks observed leaving "now" while the app is open
  loaded: false,
  error: null,
  overlay: null,
  other: false, // a device is active but plays something that isn't a song
  gen: { detail: 0, search: 0 },
};

// ---------- errors ----------

const isCode = (e, code) => String(e).startsWith(code);
const reason = (e) => String(e).replace(/^[A-Z_]+:\s*/, "").slice(0, 80) || "unknown error";

// ---------- toast ----------

let toastTimer = null;
function toast(msg) {
  const el = $("toast");
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 3200);
}

// ---------- login ----------

const LOGIN_COPY = {
  login: {
    title: "Your music, in order",
    sub: "Connect Spotify to see what played, what's playing, and what's next.",
    btn: "Connect Spotify",
  },
  reconnect: {
    title: "Reconnect Spotify to load your listening history",
    sub: "Spotify needs one more permission. It takes a few seconds.",
    btn: "Reconnect Spotify",
  },
  ended: {
    title: "Your Spotify session ended",
    sub: "Connect again to pick up where you left off.",
    btn: "Connect Spotify",
  },
};

function showLogin(kind) {
  stopPolling();
  returnFocus = null; // the stage is about to hide: nothing to give focus back to
  closeOverlay();
  state.loginKind = LOGIN_COPY[kind] ? kind : "login";
  const copy = LOGIN_COPY[state.loginKind];
  $("loginTitle").textContent = copy.title;
  $("loginSub").textContent = copy.sub;
  $("loginBtn").textContent = copy.btn;
  $("loginBtn").disabled = false;
  $("loginError").hidden = true;
  $("stage").hidden = true;
  $("login").hidden = false;
}

async function onLogin() {
  const btn = $("loginBtn");
  btn.disabled = true;
  btn.textContent = "Waiting for Spotify…";
  $("loginError").hidden = true;
  try {
    await invoke("login");
    const status = await invoke("auth_status");
    if (status === "ok") return startStage();
    showLogin(status);
  } catch (e) {
    $("loginError").textContent = `Couldn't connect: ${reason(e)}`;
    $("loginError").hidden = false;
    btn.disabled = false;
    btn.textContent = LOGIN_COPY[state.loginKind].btn;
  }
}

function expire() {
  showLogin("ended");
}

// ---------- poll loop (sequential: next tick starts after this one ends) ----------

let polling = false;
let pollTimer = null;
let inFlight = false;
let pollAgain = false;
let tick = 0;

function startPolling() {
  polling = true;
  tick = 0;
  schedule(0);
}

function stopPolling() {
  polling = false;
  clearTimeout(pollTimer);
  pollTimer = null;
}

function schedule(ms) {
  clearTimeout(pollTimer);
  pollTimer = setTimeout(poll, ms);
}

/** Poll as soon as possible, without overlapping a running poll. */
function kick() {
  if (!polling) return;
  if (inFlight) pollAgain = true;
  else schedule(0);
}

async function poll() {
  pollTimer = null;
  if (!polling) return;
  inFlight = true;
  let delay = POLL_MS;
  try {
    await refresh();
    state.error = null;
  } catch (e) {
    inFlight = false;
    if (isCode(e, "AUTH_EXPIRED")) return expire();
    if (!state.error && state.loaded) toast("Can't reach Spotify. Retrying.");
    state.error = reason(e);
    delay = ERROR_POLL_MS;
    renderNow();
  }
  inFlight = false;
  if (!polling) return;
  if (pollAgain) {
    pollAgain = false;
    delay = 0;
  }
  schedule(delay);
}

async function refresh() {
  const s = await invoke("playback_state");
  tick++;

  // active but no track = an ad or a podcast episode: keep the device, show no cover
  state.other = Boolean(s && s.active && !(s.track && s.track.uri));
  if (state.other) {
    state.device = { id: s.device_id, name: s.device_name };
    if (performance.now() > state.holdUntil) state.isPlaying = Boolean(s.is_playing);
  }

  if (!s || !s.active || state.other) {
    const hadNow = Boolean(state.now);
    if (hadNow) observe(state.now);
    state.now = null;
    if (!state.other) state.isPlaying = false;
    state.progressMs = 0;
    if (hadNow || !state.loaded || tick % QUEUE_EVERY === 0) {
      await Promise.all([discover(), loadHistory()]);
      state.queue = [];
      state.loaded = true;
      renderNow();
      renderRun();
    }
    renderChrome();
    return;
  }

  state.device = { id: s.device_id, name: s.device_name };
  if (performance.now() > state.holdUntil) state.isPlaying = Boolean(s.is_playing);
  state.progressMs = s.progress_ms || 0;
  state.progressAt = performance.now();

  const t = s.track;
  const changed = !state.now || state.now.uri !== t.uri;
  if (changed || !state.loaded) {
    if (state.now && changed) observe(state.now);
    state.now = t;
    await Promise.all([loadQueue(), loadHistory()]);
    state.loaded = true;
    renderNow();
    renderRun();
    paint(t.cover);
  } else if (tick % QUEUE_EVERY === 0) {
    if (await loadQueue()) renderRun();
  }
  renderChrome();
}

/** Remember a track that just left "now", in case recently-played lags behind. */
function observe(track) {
  state.session.unshift({ track, played_at: new Date().toISOString() });
  state.session.length = Math.min(state.session.length, 50);
}

async function loadQueue() {
  try {
    const q = (await invoke("get_queue")) || [];
    const same = q.length === state.queue.length && q.every((t, i) => t.uri === state.queue[i].uri);
    state.queue = q;
    return !same;
  } catch (e) {
    if (isCode(e, "AUTH_EXPIRED")) throw e;
    return false; // keep the old queue
  }
}

async function loadHistory() {
  try {
    state.recent = (await invoke("get_recently_played")) || [];
  } catch (e) {
    if (isCode(e, "AUTH_EXPIRED")) throw e;
    // keep what we had; session history still fills the run
  }
}

/** recently-played + session-observed, deduped by uri+played_at, newest first. */
function history() {
  const api = state.recent.filter((r) => r && r.track);
  const near = (a, b) => Math.abs(Date.parse(a) - Date.parse(b)) < SESSION_MATCH_MS;
  const extra = state.session.filter(
    (s) => !api.some((r) => r.track.uri === s.track.uri && near(r.played_at, s.played_at)),
  );
  const seen = new Set();
  return [...api, ...extra]
    .filter((r) => {
      const k = `${r.track.uri}|${r.played_at}`;
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    })
    .sort((a, b) => Date.parse(b.played_at) - Date.parse(a.played_at));
}

async function discover() {
  try {
    const list = (await invoke("list_devices")) || [];
    state.devices = list;
    const d = list.find((x) => x.is_active) || list[0];
    if (!state.now) state.device = d ? { id: d.id, name: d.name } : null;
    return d ? { id: d.id, name: d.name } : null;
  } catch (e) {
    if (isCode(e, "AUTH_EXPIRED")) throw e;
    return null;
  }
}

// ---------- the run ----------

const slot = Object.assign(document.createElement("div"), { className: "slot" });

const letterOf = (name) => ([...String(name || "").trim()][0] || "?").toUpperCase();
const letterTile = (t) => `<span class="letter">${esc(letterOf(t.name))}</span>`;

function makeCover(item) {
  const t = item.track;
  const el = document.createElement("figure");
  el.className = "cover";
  el.dataset.key = item.key;
  const art = t.cover ? `<img src="${esc(t.cover)}" alt="" draggable="false" />` : letterTile(t);
  el.innerHTML =
    `<div class="art">${art}</div>` +
    `<figcaption><span class="ct">${esc(t.name)}</span><span class="ca">${esc(t.artists)}</span></figcaption>`;
  const img = el.querySelector("img");
  if (img) img.addEventListener("error", () => img.replaceWith(Object.assign(document.createElement("span"), {
    className: "letter",
    textContent: letterOf(t.name),
  })), { once: true });
  return el;
}

function renderRun() {
  const run = $("run");
  const prev = measure(run);
  const limits = small.matches ? { maxPast: 2, maxNext: 4 } : { maxPast: 4, maxNext: 8 };
  const items = buildRun({ history: history(), now: state.now, queue: state.queue }, limits);

  const old = new Map([...run.querySelectorAll(".cover")].map((el) => [el.dataset.key, el]));
  const els = [];
  for (const it of items) {
    const el = old.get(it.key) || makeCover(it);
    el.dataset.role = it.role;
    el.dataset.offset = String(it.offset);
    els.push(el);
  }
  if (!state.now) els.splice(items.filter((i) => i.role === "past").length, 0, slot);

  run.replaceChildren(...els);
  center();
  flip(run, prev);
}

/** Shift the run so the current cover (or the empty slot) sits in the horizontal centre. */
function center() {
  const run = $("run");
  const anchor = run.querySelector('[data-role="now"], .slot');
  if (!anchor) return;
  const cur = parseFloat(run.style.getPropertyValue("--shift")) || 0;
  const delta = run.clientWidth / 2 - (anchor.offsetLeft + anchor.offsetWidth / 2);
  run.style.setProperty("--shift", `${cur + delta}px`);
  // a past cover cut by the window edge reads as a sliver: hide it instead
  // every cover: a reused element keeps the class when its role changes
  for (const el of run.querySelectorAll(".cover")) el.classList.toggle("is-off", el.dataset.role === "past" && el.offsetLeft < 0);
}

// ---------- now block, chrome, progress ----------

function setText(id, text) {
  const el = $(id);
  el.textContent = text || "";
  el.hidden = !text;
}

function renderNow() {
  const t = state.now;
  const title = $("nowTitle");
  if (t) {
    title.textContent = t.name;
    title.title = t.name;
    setText("nowArtist", t.artists);
    setText("nowAlbum", t.album);
    setText("emptyState", "");
    return;
  }
  let head = "Nothing playing";
  let line = "Pick a playlist from your library to start.";
  if (state.other && state.device) {
    head = `Playing on ${state.device.name}`;
    line = "An ad or a podcast is on. Songs show up here.";
  } else if (state.error && !state.loaded) {
    head = "Can't reach Spotify";
    line = "Check your connection. Retrying every few seconds.";
  } else if (state.devices && state.devices.length === 0) {
    head = "Open Spotify on a device";
    line = "Your Mac, phone, or speaker — then press play.";
  }
  title.textContent = head;
  title.title = "";
  setText("nowArtist", "");
  setText("nowAlbum", "");
  setText("emptyState", line);
}

function renderChrome() {
  const stage = $("stage");
  const idle = !state.now;
  const other = idle && state.other; // an ad or a podcast: only play/pause makes sense
  stage.classList.toggle("is-playing", state.isPlaying && (!idle || other));
  stage.classList.toggle("is-idle", idle && !other);
  stage.classList.toggle("is-other", other);
  $("playBtn").setAttribute("aria-label", state.isPlaying ? "Pause" : "Play");
  for (const id of ["prevBtn", "nextBtn"]) $(id).disabled = idle;
  $("playBtn").disabled = idle && !other;
  $("scrub").tabIndex = idle ? -1 : 0;

  const noDevice = state.devices && state.devices.length === 0 && idle;
  $("libraryBtn").classList.toggle("is-primary", idle && !other && !noDevice && state.loaded);

  const dev = $("device");
  dev.hidden = !state.device;
  if (state.device) dev.querySelector(".device-name").textContent = state.device.name;
  renderProgress();
}

function progress() {
  if (!state.now) return 0;
  const p = state.progressMs + (state.isPlaying ? performance.now() - state.progressAt : 0);
  return Math.min(p, state.now.duration_ms || p);
}

let lastShown = "";
function renderProgress() {
  const dur = state.now ? state.now.duration_ms || 0 : 0;
  const p = progress();
  const pct = dur ? Math.min(100, (p / dur) * 100) : 0;
  $("scrubFill").style.width = `${pct}%`;
  const shown = `${fmtTime(p)}|${fmtTime(dur)}`;
  if (shown !== lastShown) {
    lastShown = shown;
    $("curTime").textContent = fmtTime(p);
    $("durTime").textContent = fmtTime(dur);
    $("scrub").setAttribute("aria-valuenow", String(Math.round(pct)));
    $("scrub").setAttribute("aria-valuetext", `${fmtTime(p)} of ${fmtTime(dur)}`);
  }
}

function frame() {
  if (!$("stage").hidden && state.now) renderProgress();
  requestAnimationFrame(frame);
}

// ---------- background colour ----------

let activeBg = null;
let colorToken = 0;

function setLayer(el, { vivid, ink }) {
  el.style.setProperty("--v", vivid.join(" "));
  el.style.setProperty("--i", ink.join(" "));
}

async function paint(url) {
  const token = ++colorToken;
  const c = await extractColors(url);
  if (token !== colorToken || !c) return; // keep previous colours on failure
  const key = `${c.vivid}|${c.ink}`;
  if (activeBg.dataset.colors === key) return;
  const next = activeBg === $("bgA") ? $("bgB") : $("bgA");
  const old = activeBg;
  setLayer(next, c);
  next.dataset.colors = key;
  next.style.zIndex = "1";
  old.style.zIndex = "0";
  next.classList.add("is-on");
  activeBg = next;
  document.documentElement.style.setProperty("--i", c.ink.join(" "));
  document.documentElement.style.setProperty("--v", c.vivid.join(" "));
  setTimeout(() => {
    if (activeBg !== old) old.classList.remove("is-on");
  }, 950);
}

// ---------- transport ----------

/** Run a player command; on NO_ACTIVE_DEVICE rediscover the device once and retry once. */
async function withDevice(fn) {
  try {
    await fn(state.device && state.device.id);
    return true;
  } catch (e) {
    if (isCode(e, "AUTH_EXPIRED")) return expire(), false;
    if (!isCode(e, "NO_ACTIVE_DEVICE")) {
      toast(`Spotify didn't respond: ${reason(e)}`);
      return false;
    }
  }
  const lost = state.device && state.device.name;
  state.device = null;
  try {
    const d = await discover();
    if (d) {
      state.device = d;
      await fn(d.id);
      return true;
    }
  } catch (e) {
    if (isCode(e, "AUTH_EXPIRED")) return expire(), false;
  }
  toast(lost ? `Couldn't reach ${lost}` : "Open Spotify on a device first");
  return false;
}

/** The known device id, or a freshly discovered one; throws NO_ACTIVE_DEVICE when there is none. */
async function needDevice(id) {
  const deviceId = id || (await discover())?.id;
  if (!deviceId) throw "NO_ACTIVE_DEVICE: no device";
  return deviceId;
}

/** Start playback of these uris; returns true on success. */
async function playUris(uris) {
  if (!uris.length) return false;
  const ok = await withDevice(async (id) => invoke("play_on_device", { deviceId: await needDevice(id), uris }));
  kick();
  return ok;
}

async function togglePlay() {
  if (!state.now && !state.other) return;
  const was = state.isPlaying;
  state.progressMs = progress();
  state.progressAt = performance.now();
  state.isPlaying = !was;
  state.holdUntil = performance.now() + HOLD_MS;
  renderChrome();
  const ok = await withDevice(async (id) =>
    was ? invoke("pause") : invoke("resume", { deviceId: await needDevice(id) }),
  );
  if (!ok) {
    state.isPlaying = was;
    state.holdUntil = 0;
    renderChrome();
  }
  kick();
}

async function skip(cmd) {
  if (!state.now) return;
  await withDevice(() => invoke(cmd));
  kick();
}

function seekClick(ev) {
  if (!state.now || !state.now.duration_ms) return;
  const r = $("scrub").getBoundingClientRect();
  seekTo(Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width)) * state.now.duration_ms);
}

const SEEK_STEP_MS = 5000;

/** Arrow keys on the focused scrub bar: ±5s, Home/End jump to the ends. */
function seekKey(ev) {
  if (!state.now || !state.now.duration_ms) return;
  const p = progress();
  const to = { ArrowLeft: p - SEEK_STEP_MS, ArrowRight: p + SEEK_STEP_MS, Home: 0, End: state.now.duration_ms - 1000 }[ev.key];
  if (to === undefined) return;
  ev.preventDefault();
  seekTo(Math.min(state.now.duration_ms, Math.max(0, to)));
}

async function seekTo(ms) {
  const positionMs = Math.round(ms);
  state.progressMs = positionMs;
  state.progressAt = performance.now();
  renderProgress();
  await withDevice(() => invoke("seek", { positionMs }));
  kick();
}

// ---------- overlays: one open at a time ----------

let returnFocus = null;

function openOverlay(name) {
  if (state.overlay === name) return;
  if (state.overlay) $(state.overlay).hidden = true;
  else returnFocus = document.activeElement;
  state.overlay = name;
  $(name).hidden = false;
  $("stage").inert = true;
}

function closeOverlay() {
  if (!state.overlay) return;
  $(state.overlay).hidden = true;
  state.overlay = null;
  $("stage").inert = false;
  returnFocus?.focus?.();
  returnFocus = null;
}

/** Overlay commands that hit a dead session send the user to login. */
function overlayFailed(e) {
  if (!isCode(e, "AUTH_EXPIRED")) return false;
  closeOverlay();
  expire();
  return true;
}

const artHtml = (url, name) =>
  url
    ? `<img src="${esc(url)}" alt="" draggable="false" data-letter="${esc(letterOf(name))}" />`
    : letterTile({ name });

/** Overlay covers that fail to load become the letter tile. */
function onImgError(e) {
  const img = e.target;
  if (img.tagName !== "IMG" || !img.dataset.letter) return;
  img.replaceWith(Object.assign(document.createElement("span"), { className: "letter", textContent: img.dataset.letter }));
}

/** A playable track row. num: show the position; art: show the cover (album rows skip it, it's the same every row). */
function trackRow(t, i, { num, art }) {
  const kind = `${num ? " has-num" : ""}${art ? " has-art" : ""}`;
  return (
    `<button class="row row-track${kind}" type="button" data-i="${i}" title="${esc(t.name)}">` +
    (num ? `<span class="row-num">${i + 1}</span>` : "") +
    (art ? `<span class="art row-art">${artHtml(t.cover, t.name)}</span>` : "") +
    `<span class="row-text"><span class="row-title">${esc(t.name)}</span><span class="row-sub">${esc(t.artists)}</span></span>` +
    `<span class="row-time">${fmtTime(t.duration_ms)}</span></button>`
  );
}

// ---------- library: level 1 playlists, level 2 tracks ----------

let playlists = null; // loaded once, then cached
let playlistsLoading = false;
let detailTracks = [];
let listScroll = 0;

const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

/** Smallest image that still looks sharp at 56px on a 2x screen. */
function pickImage(images) {
  const list = images || [];
  return (list.filter((im) => !im.width || im.width >= 112).pop() || list[0] || {}).url || null;
}

function openLibrary() {
  openOverlay("library");
  showList();
  $("sheet").focus();
  if (!playlists) loadPlaylists();
}

function showList() {
  state.gen.detail++; // drop any detail response still in flight
  detailTracks = [];
  $("libDetail").hidden = true;
  $("libBack").hidden = true;
  $("libLevel1").hidden = false;
  $("libTitle").hidden = false;
  $("sheetBody").scrollTop = listScroll;
}

async function loadPlaylists() {
  if (playlistsLoading) return;
  playlistsLoading = true;
  setText("listStatus", "Loading your playlists…");
  try {
    playlists = (await invoke("get_playlists")) || [];
    renderPlaylists();
  } catch (e) {
    if (!overlayFailed(e)) setText("listStatus", `Couldn't load your playlists — ${reason(e)}`);
  } finally {
    playlistsLoading = false;
  }
}

function renderPlaylists() {
  setText("listStatus", playlists.length ? "" : "No playlists yet.");
  $("libList").innerHTML = playlists
    .map(
      (p, i) =>
        `<button class="row row-playlist" type="button" data-id="${esc(p.id)}" data-i="${i}" title="${esc(p.name)}">` +
        `<span class="art row-art">${artHtml(pickImage(p.images), p.name)}</span>` +
        `<span class="row-text"><span class="row-title">${esc(p.name)}</span>` +
        `<span class="row-sub">${plural((p.tracks && p.tracks.total) || 0, "track", "tracks")}</span></span></button>`,
    )
    .join("");
}

/** Level 2 for a playlist or an album: {kind, id, name, cover, sub}. */
async function openDetail(src) {
  if (state.overlay !== "library") {
    openOverlay("library");
    $("sheet").focus();
  }
  if (!$("libLevel1").hidden) listScroll = $("sheetBody").scrollTop;
  const gen = ++state.gen.detail;
  detailTracks = [];
  $("libLevel1").hidden = true;
  $("libTitle").hidden = true;
  $("libBack").hidden = false;
  $("libDetail").hidden = false;
  $("detailCover").innerHTML = artHtml(src.cover, src.name);
  $("detailName").textContent = src.name;
  $("detailName").title = src.name;
  setText("detailSub", src.sub);
  $("detailPlay").disabled = true;
  $("detailRows").innerHTML = "";
  setText("detailStatus", "Loading tracks…");
  $("sheetBody").scrollTop = 0;

  const album = src.kind === "album";
  let tracks;
  try {
    tracks = album
      ? await invoke("get_album_tracks", { albumId: src.id })
      : await invoke("get_playlist_tracks", { playlistId: src.id });
  } catch (e) {
    if (gen !== state.gen.detail) return;
    if (!overlayFailed(e)) setText("detailStatus", `Couldn't load tracks — ${reason(e)}`);
    return;
  }
  if (gen !== state.gen.detail) return; // a newer detail (or the list) took over

  detailTracks = (tracks || []).filter((t) => t && t.uri);
  if (!album) setText("detailSub", plural(detailTracks.length, "track", "tracks"));
  setText("detailStatus", detailTracks.length ? "" : album ? "This album is empty." : "This playlist is empty.");
  $("detailRows").innerHTML = detailTracks.map((t, i) => trackRow(t, i, { num: true, art: !album })).join("");
  $("detailPlay").disabled = !detailTracks.length;
}

async function playDetailFrom(i) {
  const uris = detailTracks.slice(i).map((t) => t.uri);
  if (await playUris(uris)) closeOverlay();
}

// ---------- search: songs + albums, debounced, last request wins ----------

const SEARCH_DEBOUNCE_MS = 250;
let searchTimer = null;
let searchHits = { tracks: [], albums: [] };

function openSearch() {
  openOverlay("search");
  const input = $("searchInput");
  input.focus();
  input.select();
}

function onSearchInput() {
  clearTimeout(searchTimer);
  const gen = ++state.gen.search;
  const q = $("searchInput").value.trim();
  if (!q) {
    searchHits = { tracks: [], albums: [] };
    $("searchResults").innerHTML = "";
    $("searchResults").hidden = true;
    return;
  }
  searchTimer = setTimeout(() => runSearch(q, gen), SEARCH_DEBOUNCE_MS);
}

function searchMessage(text) {
  searchHits = { tracks: [], albums: [] };
  const box = $("searchResults");
  box.innerHTML = `<p class="status">${esc(text)}</p>`;
  box.hidden = false;
}

async function runSearch(q, gen) {
  let res;
  try {
    res = await invoke("search", { query: q });
  } catch (e) {
    if (gen !== state.gen.search) return;
    if (!overlayFailed(e)) searchMessage(`Search failed — ${reason(e)}`);
    return;
  }
  if (gen !== state.gen.search) return;
  const tracks = ((res && res.tracks) || []).filter((t) => t && t.uri).slice(0, 10);
  const albums = ((res && res.albums) || []).filter((a) => a && a.id).slice(0, 10);
  if (!tracks.length && !albums.length) return searchMessage(`No songs or albums for "${q}".`);
  searchHits = { tracks, albums };

  let html = "";
  if (tracks.length) {
    html += `<section class="group"><h3 class="group-title">Songs</h3><div class="rows">`;
    html += tracks.map((t, i) => trackRow(t, i, { num: false, art: true })).join("");
    html += `</div></section>`;
  }
  if (albums.length) {
    html += `<section class="group"><h3 class="group-title">Albums</h3><div class="albums">`;
    html += albums
      .map(
        (a, i) =>
          `<button class="album" type="button" data-album="${i}" title="${esc(a.name)}">` +
          `<span class="art">${artHtml(a.cover, a.name)}</span>` +
          `<span class="album-name">${esc(a.name)}</span><span class="album-sub">${esc(a.artists)}</span></button>`,
      )
      .join("");
    html += `</div></section>`;
  }
  const box = $("searchResults");
  box.innerHTML = html;
  box.hidden = false;
  box.scrollTop = 0;
}

async function onSearchClick(e) {
  const song = e.target.closest(".row-track");
  if (song) {
    const t = searchHits.tracks[Number(song.dataset.i)];
    if (t && (await playUris([t.uri]))) closeOverlay();
    return;
  }
  const al = e.target.closest("[data-album]");
  if (al) {
    const a = searchHits.albums[Number(al.dataset.album)];
    if (!a) return;
    if (!playlists) loadPlaylists(); // so Back has something to show
    openDetail({ kind: "album", id: a.id, name: a.name, cover: a.cover, sub: a.artists });
  }
}

// ---------- keyboard: Space = play/pause, Esc = close the overlay ----------

const typing = (t) => t && t.closest && t.closest("input, textarea, select, [contenteditable]");

function onKey(e) {
  if (e.key === "Escape" && e.type === "keydown" && state.overlay) {
    e.preventDefault();
    closeOverlay();
    return;
  }
  if (e.code !== "Space" || $("stage").hidden || typing(e.target)) return;
  if (e.target.closest && e.target.closest("button, a, [role='slider']")) return; // Space presses what has focus
  e.preventDefault(); // stops the page from scrolling
  if (e.type === "keydown" && !e.repeat) togglePlay();
}

// ---------- boot ----------

function startStage() {
  $("login").hidden = true;
  $("stage").hidden = false;
  renderNow();
  renderChrome();
  startPolling();
}

async function boot() {
  activeBg = $("bgA");
  setLayer($("bgA"), FALLBACK);
  setLayer($("bgB"), FALLBACK);

  $("loginBtn").addEventListener("click", onLogin);
  $("playBtn").addEventListener("click", togglePlay);
  $("prevBtn").addEventListener("click", () => skip("previous_track"));
  $("nextBtn").addEventListener("click", () => skip("next_track"));
  $("scrub").addEventListener("click", seekClick);
  $("scrub").addEventListener("keydown", seekKey);

  $("libraryBtn").addEventListener("click", openLibrary);
  $("searchBtn").addEventListener("click", openSearch);
  for (const id of ["library", "search"]) {
    $(id).addEventListener("click", (e) => e.target.closest("[data-close]") && closeOverlay());
    $(id).addEventListener("error", onImgError, true);
  }
  $("libBack").addEventListener("click", showList);
  $("libList").addEventListener("click", (e) => {
    const row = e.target.closest("[data-id]");
    const p = row && playlists && playlists[Number(row.dataset.i)];
    if (p) openDetail({ kind: "playlist", id: p.id, name: p.name, cover: pickImage(p.images), sub: plural((p.tracks && p.tracks.total) || 0, "track", "tracks") });
  });
  $("detailRows").addEventListener("click", (e) => {
    const row = e.target.closest("[data-i]");
    if (row) playDetailFrom(Number(row.dataset.i));
  });
  $("detailPlay").addEventListener("click", () => playDetailFrom(0));
  $("searchInput").addEventListener("input", onSearchInput);
  $("searchResults").addEventListener("click", onSearchClick);
  document.addEventListener("keydown", onKey);
  document.addEventListener("keyup", onKey);
  addEventListener("resize", center);
  small.addEventListener("change", () => state.loaded && renderRun());
  requestAnimationFrame(frame);

  let status = "login";
  try {
    status = await invoke("auth_status");
  } catch {
    /* treat as logged out */
  }
  state.auth = status;
  if (status === "ok") startStage();
  else showLogin(status);
}

boot();
