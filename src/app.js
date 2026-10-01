// The Run — stage UI: boot, sequential poll loop, the run of covers, transport.
import { fmtTime, esc } from "./lib/format.js";
import { FALLBACK, extractColors } from "./lib/color.js";
import { buildRun, mergeHistory, measure, flip } from "./lib/timeline.js";

// Every call belongs to a login session. A result or error from an older session
// (still in flight across a logout) never settles, so it can't touch the new one.
let authSession = 0;
const STALE = new Promise(() => {});
function invoke(cmd, args) {
  const sess = authSession;
  return window.__TAURI__.core.invoke(cmd, args).then(
    (v) => (sess === authSession ? v : STALE),
    (e) => (sess === authSession ? Promise.reject(e) : STALE),
  );
}
const $ = (id) => document.getElementById(id);

const POLL_MS = 1000;
const ERROR_POLL_MS = 4000;
const QUEUE_EVERY = 10; // ticks
const HOLD_MS = 1500; // ignore polled is_playing right after a play/pause click
// played_at is when a play ended, the same moment we see a track leave: closer than this = same play
const SESSION_MATCH_MS = 2 * 60 * 1000;
const PLAYED_MS = 30 * 1000; // Spotify counts a play after 30s; a track skipped sooner isn't history
const small = matchMedia("(max-width: 899px)");

const state = {
  loginKind: "login",
  device: null, // {id, name}
  devices: null, // last list_devices result, null = unknown
  mode: "idle", // "track": a song plays, "other": an ad or podcast plays, "idle": nothing
  now: null,
  isPlaying: false,
  progressMs: 0,
  progressAt: 0,
  holdUntil: 0,
  seekHoldUntil: 0,
  listenedMs: 0, // real playing time of "now" (seeks and pauses don't count)
  listenAt: 0,
  queue: [],
  recent: [], // get_recently_played, newest first
  historyOk: false, // false after a failed history fetch: periodic ticks retry it
  session: [], // tracks observed leaving "now" while the app is open
  loaded: false,
  error: null,
  overlay: null,
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
  authSession++;
  playlistsLoading = false; // a load from the old session will never finish
  playerChain = Promise.resolve(); // a player command from the old session will never finish either
  clearTimeout(searchTimer); // a debounced search must not start in the next session
  clearTimeout(seekTimer); // nor a debounced seek
  state.gen.search++;
  state.gen.detail++;
  // the next login may be another account: drop everything that belonged to this one
  playlists = null;
  listScroll = 0;
  $("libList").innerHTML = "";
  $("run").replaceChildren();
  Object.assign(state, { mode: "idle", now: null, device: null, devices: null, queue: [], recent: [], session: [], historyOk: false, error: null });
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
  if ($("stage").hidden) return; // already on the login screen: a straggler, nothing to end
  showLogin("ended");
  $("loginBtn").focus(); // the stage the user was in is gone: land on the way back
}

// ---------- poll loop (sequential: next tick starts after this one ends) ----------

let polling = false;
let pollTimer = null;
let inFlight = false;
let pollAgain = false;
let tick = 0;
let pollEpoch = 0; // bumped on every start/stop: a poll from an older epoch must not touch state

function startPolling() {
  pollEpoch++;
  polling = true;
  inFlight = false;
  pollAgain = false;
  tick = 0;
  state.loaded = false; // full fetch and render: a stopped poll may have skipped one
  state.now = null; // unknown what played during the gap: don't record the old track as played
  state.listenedMs = 0;
  schedule(0);
}

function stopPolling() {
  pollEpoch++;
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
  const epoch = pollEpoch;
  inFlight = true;
  let delay = POLL_MS;
  try {
    await refresh(epoch);
    if (epoch !== pollEpoch) return;
    state.error = null;
  } catch (e) {
    if (epoch !== pollEpoch) return; // stale: the session it belonged to is gone
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

async function refresh(epoch) {
  const s = await invoke("playback_state");
  if (epoch !== pollEpoch) return; // a newer session took over while this one waited
  tick++;
  listen(); // close the old "now"'s listening time before this poll overwrites it
  const active = Boolean(s && s.active);
  const track = active && s.track && s.track.uri ? s.track : null;
  const mode = track ? "track" : active ? "other" : "idle";
  const modeChanged = mode !== state.mode;
  state.mode = mode;

  if (active) {
    state.device = { id: s.device_id, name: s.device_name };
    if (performance.now() > state.holdUntil) state.isPlaying = Boolean(s.is_playing);
  } else {
    state.isPlaying = false;
  }
  // right after a local seek, Spotify may still report the old position: keep ours
  const seeking = track && state.now && track.uri === state.now.uri && performance.now() < state.seekHoldUntil;
  if (!seeking) {
    state.progressMs = track ? s.progress_ms || 0 : 0;
    state.progressAt = performance.now();
  }

  const changed = (state.now && state.now.uri) !== (track && track.uri);
  if (changed) trackGen++;
  if (changed && state.now) observe(state.now, state.listenedMs);
  if (changed) state.listenedMs = 0;
  state.now = track;

  if (changed || modeChanged || !state.loaded) {
    // a song needs its queue; with no song, the device list says who could play
    const [queue, recent, devices] = await Promise.all([
      track ? fetchOr("get_queue") : [],
      fetchOr("get_recently_played"),
      track ? null : fetchOr("list_devices"),
    ]);
    if (epoch !== pollEpoch) return;
    if (queue) state.queue = queue;
    if (recent) state.recent = recent;
    state.historyOk = Boolean(recent);
    if (devices) setDevices(devices);
    state.loaded = true;
    renderNow();
    renderRun();
    if (track) paint(track.cover);
  } else if (tick % QUEUE_EVERY === 0 && mode !== "other") {
    // between changes only the queue (song) or the device list (idle) can move
    const [fresh, recent] = await Promise.all([
      fetchOr(track ? "get_queue" : "list_devices"),
      state.historyOk ? null : fetchOr("get_recently_played"),
    ]);
    if (epoch !== pollEpoch) return;
    if (recent) {
      state.recent = recent;
      state.historyOk = true;
      renderRun();
    }
    if (!fresh) return renderChrome();
    if (track && !sameUris(fresh, state.queue)) {
      state.queue = fresh;
      renderRun();
    } else if (!track && setDevices(fresh)) {
      renderNow();
    }
  }
  renderChrome();
}

const LISTEN_STEP_CAP_MS = ERROR_POLL_MS + 1000; // a longer step is a stall, not listening

/** Add the time since the last call to listenedMs, if a song was playing. */
function listen() {
  const t = performance.now();
  if (state.now && state.isPlaying) state.listenedMs += Math.min(t - state.listenAt, LISTEN_STEP_CAP_MS);
  state.listenAt = t;
}

/** Remember a track that just left "now", in case recently-played lags behind. */
function observe(track, playedMs) {
  if (playedMs < Math.min(PLAYED_MS, (track.duration_ms || 0) / 2)) return; // skipped, not played
  state.session.unshift({ track, played_at: new Date().toISOString() });
  state.session.length = Math.min(state.session.length, 50);
}

/** A list from Spotify, or null on failure (keep what we had). AUTH_EXPIRED always propagates. */
async function fetchOr(cmd) {
  try {
    return (await invoke(cmd)) || [];
  } catch (e) {
    if (isCode(e, "AUTH_EXPIRED")) throw e;
    return null;
  }
}

const sameUris = (a, b) => a.length === b.length && a.every((t, i) => t.uri === b[i].uri);

const history = () => mergeHistory(state.recent, state.session, SESSION_MATCH_MS);

/** Store a device list; with nothing playing, show its active (or first) device. Returns true if it changed. */
function setDevices(list) {
  const changed = JSON.stringify(list) !== JSON.stringify(state.devices);
  state.devices = list;
  if (state.mode === "idle") {
    const d = list.find((x) => x.is_active) || list[0];
    state.device = d ? { id: d.id, name: d.name } : null;
  }
  return changed;
}

/** Fetch devices for a transport command: the active (or first) one, or null. */
async function discover() {
  const list = await fetchOr("list_devices");
  if (!list) return null;
  setDevices(list);
  const d = list.find((x) => x.is_active) || list[0];
  return d ? { id: d.id, name: d.name } : null;
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
  // crossorigin: the colour picker loads the same URL with CORS, so both share one cached copy
  el.innerHTML =
    `<div class="art">${artHtml(t.cover, t.name, 'crossorigin="anonymous"')}</div>` +
    `<figcaption><span class="ct">${esc(t.name)}</span><span class="ca">${esc(t.artists)}</span></figcaption>`;
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
  if (state.mode === "other" && state.device) {
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
  const mode = state.mode;
  stage.dataset.mode = mode;
  stage.classList.toggle("is-playing", state.isPlaying && mode !== "idle");
  $("playBtn").setAttribute("aria-label", state.isPlaying ? "Pause" : "Play");
  $("playBtn").disabled = mode === "idle"; // an ad or a podcast can still be paused
  for (const id of ["prevBtn", "nextBtn"]) $(id).disabled = mode !== "track";
  $("scrub").tabIndex = mode === "track" ? 0 : -1;

  const noDevice = state.devices && state.devices.length === 0 && mode === "idle";
  $("libraryBtn").classList.toggle("is-primary", mode === "idle" && !noDevice && state.loaded);

  const dev = $("device");
  dev.hidden = !state.device;
  if (state.device) dev.querySelector(".device-name").textContent = state.device.name;
  renderProgress();
  startFrames();
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

let framing = false;

/** Animate the scrub bar while a song plays; the loop stops itself otherwise. */
function frame() {
  if ($("stage").hidden || !state.now || !state.isPlaying) {
    framing = false;
    return;
  }
  renderProgress();
  requestAnimationFrame(frame);
}

function startFrames() {
  if (framing) return;
  framing = true;
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

// Spotify doesn't promise order across player endpoints: every player command waits
// for the one before it, so a slow pause can't land after a later play.
let playerChain = Promise.resolve();

/** Run a player command after the ones before it; on NO_ACTIVE_DEVICE rediscover the device once and retry once. */
function withDevice(fn) {
  const sess = authSession; // a command queued before a logout must not run after it
  const run = playerChain.then(() => (sess === authSession ? withDeviceNow(fn) : false));
  playerChain = run.catch(() => {});
  return run;
}

async function withDeviceNow(fn) {
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
// Bumped when a track change is asked for, when it lands, and when a poll sees one.
// A seek runs only if it hasn't moved since the seek was made: its position is for that track.
let trackGen = 0;

/** A track-changing player command: invalidates seeks made before it, and again once it lands. */
function changeTrack(fn) {
  clearTimeout(seekTimer);
  trackGen++;
  return withDevice(async (id) => {
    await fn(id);
    trackGen++;
  });
}

async function playUris(uris) {
  if (!uris.length) return false;
  const ok = await changeTrack(async (id) => invoke("play_on_device", { deviceId: await needDevice(id), uris }));
  kick();
  return ok;
}

let playSeq = 0; // counts play/pause clicks: only the latest one may undo the UI

/** Flip play/pause at once in the UI; the command joins the player chain in click order. */
async function togglePlay() {
  if (state.mode === "idle") return;
  listen();
  state.progressMs = progress();
  state.progressAt = performance.now();
  const want = (state.isPlaying = !state.isPlaying);
  state.holdUntil = performance.now() + HOLD_MS;
  renderChrome();
  const seq = ++playSeq;
  const ok = await withDevice(async (id) => (want ? invoke("resume", { deviceId: await needDevice(id) }) : invoke("pause")));
  if (!ok && seq === playSeq) {
    state.isPlaying = !want; // the device still has the state before this click
    state.holdUntil = 0;
    renderChrome();
  }
  kick();
}

async function skip(cmd) {
  if (!state.now) return;
  await changeTrack(() => invoke(cmd));
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
  // a held key repeats ~30 times a second: move the bar now, send one seek when the keys go quiet
  showSeek(Math.min(state.now.duration_ms, Math.max(0, to)));
  clearTimeout(seekTimer);
  const gen = trackGen; // the position is for the track on screen now, not whatever plays in 250ms
  seekTimer = setTimeout(() => seekTo(progress(), gen), SEEK_QUIET_MS);
}

const SEEK_QUIET_MS = 250;
let seekTimer = null;

function showSeek(ms) {
  state.progressMs = Math.round(ms);
  state.progressAt = performance.now();
  state.seekHoldUntil = performance.now() + SEEK_QUIET_MS + HOLD_MS;
  renderProgress();
}

async function seekTo(ms, gen = trackGen) {
  if (gen !== trackGen) return; // the track changed since this seek was made
  showSeek(ms);
  const positionMs = state.progressMs;
  await withDevice(() => (gen === trackGen ? invoke("seek", { positionMs }) : null));
  kick();
}

// ---------- overlays: one open at a time ----------

let returnFocus = null;

let overlayRev = 0; // bumped on every overlay view change: a slow Play closes only the view it came from

function openOverlay(name) {
  if (state.overlay === name) return;
  overlayRev++;
  if (state.overlay) $(state.overlay).hidden = true;
  else returnFocus = document.activeElement;
  state.overlay = name;
  $(name).hidden = false;
  $("stage").inert = true;
}

function closeOverlay() {
  if (!state.overlay) return;
  overlayRev++;
  $(state.overlay).hidden = true;
  state.overlay = null;
  $("stage").inert = false;
  returnFocus?.focus?.();
  returnFocus = null;
}

/** Overlay commands that hit a dead session send the user to login. */
function overlayFailed(e) {
  if (!isCode(e, "AUTH_EXPIRED")) return false;
  expire(); // showLogin closes the overlay
  return true;
}

const artHtml = (url, name, attrs = 'loading="lazy"') =>
  url
    ? `<img src="${esc(url)}" alt="" draggable="false" decoding="async" ${attrs} data-letter="${esc(letterOf(name))}" />`
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
  overlayRev++;
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
  overlayRev++;
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
  const rev = overlayRev;
  if ((await playUris(uris)) && rev === overlayRev) closeOverlay();
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
  overlayRev++; // new results are a new view: a slow Play from the old ones must not close it
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
  overlayRev++;
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
  overlayRev++; // replaced results are a new view
  box.innerHTML = html;
  box.hidden = false;
  box.scrollTop = 0;
}

async function onSearchClick(e) {
  const song = e.target.closest(".row-track");
  if (song) {
    const t = searchHits.tracks[Number(song.dataset.i)];
    const rev = overlayRev;
    if (t && (await playUris([t.uri])) && rev === overlayRev) closeOverlay();
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
  $("run").addEventListener("error", onImgError, true);
  // a hidden window needs no 1s polling; come back with a fresh poll
  document.addEventListener("visibilitychange", () => {
    if ($("stage").hidden) return;
    if (document.hidden) stopPolling();
    else startPolling();
  });

  let status = "login";
  try {
    status = await invoke("auth_status");
  } catch {
    /* treat as logged out */
  }
  if (status === "ok") startStage();
  else showLogin(status);
}

boot();
