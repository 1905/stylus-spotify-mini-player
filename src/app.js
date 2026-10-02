// The Run — stage UI: boot, sequential poll loop, the run of covers, transport.
import { fmtTime, esc } from "./lib/format.js";
import { FALLBACK, extractColors } from "./lib/color.js";
import { buildRun, mergeHistory, measure, flip } from "./lib/timeline.js";
import { favoritesBy } from "./lib/favorites.js";
import { createIntents, nextRepeat, stepVolume } from "./lib/transport.js";
import { noteMixes } from "./lib/mixes.js";

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
const HOLD_MS = 1500; // keep a local seek position this long against polls that may lag
const PLAY_LAG_MS = 500; // Spotify can report the old play state this long after a command lands
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
  shuffle: false,
  repeat: "off", // "off" | "context" | "track"
  volume: null, // 0–100, null = unknown
  supportsVolume: false,
  contextUri: null, // what the current song plays from (playlist, album, mix)
  saved: null, // the current song is in Liked Songs; null = unknown
  progressMs: 0,
  progressAt: 0,
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
    title: "Reconnect Spotify for your library",
    sub: "Spotify needs a few new permissions. It takes a few seconds.",
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
  clearTimeout(volTimer); // nor a debounced volume change
  volTimer = null;
  savedChain = Promise.resolve(); // a heart command from the old session will never finish
  changesPending = 0;
  settleAfter = 0;
  intents.reset();
  heartGen++;
  devicesGen++;
  libraryDenied = false; // the next login may grant the library scopes
  queueDirty = false;
  resetLibrary();
  state.gen.search++;
  state.gen.detail++;
  // the next login may be another account: drop everything that belonged to this one
  playlists = null;
  listScroll = 0;
  $("libList").innerHTML = "";
  $("run").replaceChildren();
  Object.assign(state, {
    mode: "idle", now: null, device: null, devices: null, queue: [], recent: [], session: [], historyOk: false, error: null,
    shuffle: false, repeat: "off", volume: null, supportsVolume: false, contextUri: null, saved: null,
  });
  closeDevices();
  closeVolume();
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
  clearTimeout(seekTimer); // state.now is reset below: no seek may outlive it
  trackGen++;
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
  clearTimeout(seekTimer); // a seek made before the gap is for a track we may no longer have
  trackGen++;
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
  const startedAt = performance.now();
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
    // a click still queued (or just landed) outranks what this poll saw
    const settled = (key) => intents.settled(key, startedAt);
    if (settled("device")) state.device = { id: s.device_id, name: s.device_name };
    if (settled("play")) state.isPlaying = Boolean(s.is_playing);
    if (settled("shuffle")) state.shuffle = Boolean(s.shuffle);
    if (settled("repeat")) state.repeat = s.repeat || "off";
    // a volume hold belongs to one device: it must not hide a newly picked device's level
    if (settled("device") && settled(volKey(s.device_id))) state.volume = s.volume_percent ?? null;
    state.supportsVolume = Boolean(s.supports_volume);
    state.contextUri = s.context_uri || null;
    // a mix playing now is one Spotify won't list: remember it (once per context, not every poll)
    if (state.contextUri !== notedContext) {
      notedContext = state.contextUri;
      noteContexts([state.contextUri]);
    }
  } else {
    state.isPlaying = false;
    state.supportsVolume = false;
    state.contextUri = null;
  }
  // right after a local seek, Spotify may still report the old position: keep ours
  const seeking = track && state.now && track.uri === state.now.uri && performance.now() < state.seekHoldUntil;
  if (!seeking) {
    state.progressMs = track ? s.progress_ms || 0 : 0;
    state.progressAt = performance.now();
  }

  const changed = (state.now && state.now.uri) !== (track && track.uri);
  if (changed) trackGen++;
  if (changesPending === 0 && settleAfter && startedAt >= settleAfter) settleAfter = 0;
  if (changed && state.now) observe(state.now, state.listenedMs);
  if (changed) state.listenedMs = 0;
  state.now = track;
  if (changed) checkSaved(track);

  if (changed || modeChanged || !state.loaded) {
    // show the new track now: what's on screen is what a seek or a skip acts on.
    // (Idle copy waits for the device list on first load, or it would flicker.)
    if (track || state.loaded) {
      // until the fresh queue lands: the old one usually starts with the track that just began
      if (track && state.queue.length && state.queue[0].uri === track.uri) state.queue = state.queue.slice(1);
      if (!track) state.queue = [];
      renderNow();
      if (state.loaded) renderRun();
      renderChrome();
      if (track) paint(track.cover);
    }
    // a song needs its queue; with no song, the device list says who could play
    const [queue, recent, devices] = await Promise.all([
      track ? fetchOr("get_queue") : [],
      fetchOr("get_recently_played"),
      track ? null : fetchOr("list_devices"),
    ]);
    if (epoch !== pollEpoch) return;
    if (queue) state.queue = queue;
    if (queue) queueDirty = false;
    if (recent) state.recent = recent;
    if (recent) noteContexts([state.contextUri, ...recent.map((r) => r.context_uri)]);
    state.historyOk = Boolean(recent);
    if (devices) setDevices(devices, startedAt);
    state.loaded = true;
    renderNow();
    renderRun();
  } else if ((tick % QUEUE_EVERY === 0 || queueDirty) && mode !== "other") {
    // between changes only the queue (song) or the device list (idle) can move
    queueDirty = false;
    const [fresh, recent] = await Promise.all([
      fetchOr(track ? "get_queue" : "list_devices"),
      state.historyOk ? null : fetchOr("get_recently_played"),
    ]);
    if (epoch !== pollEpoch) return;
    if (recent) {
      state.recent = recent;
      state.historyOk = true;
      noteContexts([state.contextUri, ...recent.map((r) => r.context_uri)]);
      renderRun();
    }
    if (!fresh) return renderChrome();
    if (track && !sameUris(fresh, state.queue)) {
      state.queue = fresh;
      renderRun();
    } else if (!track && setDevices(fresh, startedAt)) {
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

let queueDirty = false; // a song was just added to the queue: the next poll fetches it

const sameUris = (a, b) => a.length === b.length && a.every((t, i) => t.uri === b[i].uri);

const history = () => mergeHistory(state.recent, state.session, SESSION_MATCH_MS);

/**
 * Store a device list; with nothing playing, show its active (or first) device, unless a device
 * pick is still pending (listed before startedAt). Returns true if it changed.
 */
function setDevices(list, startedAt = performance.now()) {
  const changed = JSON.stringify(list) !== JSON.stringify(state.devices);
  state.devices = list;
  if (state.mode === "idle" && intents.settled("device", startedAt)) {
    const d = list.find((x) => x.is_active) || list[0];
    state.device = d ? { id: d.id, name: d.name } : null;
  }
  if (changed && devicesOpen) renderDeviceList();
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
  setEl($(id), text);
}

function setEl(el, text) {
  el.textContent = text || "";
  el.hidden = !text;
}

let nowArtistHtml = ""; // rewritten only when it changes: a focused artist link keeps its focus

function setNowArtist(html) {
  if (html === nowArtistHtml) return;
  nowArtistHtml = html;
  $("nowArtist").innerHTML = html;
  $("nowArtist").hidden = !html;
}

function renderNow() {
  const t = state.now;
  const title = $("nowTitle");
  if (t) {
    title.textContent = t.name;
    title.title = t.name;
    setNowArtist(artistLinks(t));
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
  setNowArtist("");
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

  // hidden only until the first poll: with no device the button still opens the (empty) list
  const dev = $("deviceBtn");
  dev.hidden = !state.device && !state.loaded;
  dev.classList.toggle("is-none", !state.device);
  dev.querySelector(".device-name").textContent = state.device ? state.device.name : "No device";

  const song = mode === "track";
  const shuffle = $("shuffleBtn");
  shuffle.disabled = !song;
  shuffle.classList.toggle("is-on", state.shuffle);
  shuffle.setAttribute("aria-pressed", String(state.shuffle));
  const repeat = $("repeatBtn");
  repeat.disabled = !song;
  repeat.dataset.mode = state.repeat;
  repeat.classList.toggle("is-on", state.repeat !== "off");
  repeat.setAttribute("aria-pressed", String(state.repeat !== "off"));
  const label = REPEAT_LABEL[state.repeat] || REPEAT_LABEL.off;
  repeat.setAttribute("aria-label", label);
  repeat.title = label;

  const heart = $("heartBtn");
  const t = state.now;
  heart.hidden = !song || !t || !t.id || isLocal(t.uri) || libraryDenied;
  heart.disabled = state.saved === null; // unknown until is_saved answers
  heart.classList.toggle("is-on", state.saved === true);
  heart.setAttribute("aria-pressed", String(state.saved === true));
  const heartLabel = state.saved ? "Remove from Liked Songs" : "Save to Liked Songs";
  heart.setAttribute("aria-label", heartLabel);
  heart.title = heartLabel;

  renderVolume();
  renderProgress();
  startFrames();
}

const REPEAT_LABEL = { off: "Repeat", context: "Repeat: on", track: "Repeat: this song" };

function renderVolume() {
  const box = $("volume");
  const v = state.volume;
  box.hidden = state.mode === "idle" || !state.supportsVolume || v == null;
  if (box.hidden) return closeVolume();
  box.classList.toggle("is-muted", v === 0);
  $("volFill").style.width = `${v}%`;
  const slider = $("volSlider");
  slider.setAttribute("aria-valuenow", String(v));
  slider.setAttribute("aria-valuetext", `${v}%`);
  // wide: the speaker mutes; narrow: it opens the slider
  const btn = $("volBtn");
  const label = small.matches ? "Volume" : v === 0 ? "Unmute" : "Mute";
  btn.setAttribute("aria-label", label);
  btn.title = label;
  if (small.matches) btn.setAttribute("aria-expanded", String(volumeOpen));
  else btn.removeAttribute("aria-expanded");
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
  changesPending++;
  const sess = authSession; // a logout resets the count: an old change must not touch the new one
  return withDevice(async (id) => {
    await fn(id);
    trackGen++;
  }).finally(() => {
    if (sess === authSession && --changesPending === 0) settleAfter = performance.now();
  });
}

// After a track change the screen shows the old track until a poll that started after
// the change has returned. Until then a seek would carry the old track's position.
let changesPending = 0;
let settleAfter = 0; // 0 = settled; else a poll must start after this time to settle

const canSeek = () => changesPending === 0 && settleAfter === 0;

async function playUris(uris) {
  if (!uris.length) return false;
  const ok = await changeTrack(async (id) => invoke("play_on_device", { deviceId: await needDevice(id), uris }));
  kick();
  return ok;
}

// Play, shuffle, repeat, volume and a device pick flip the UI at once. A poll doesn't overwrite
// one while its command is queued, or for PLAY_LAG_MS after it lands. Only the latest may undo the UI.
// volume reads back 1-3s late (measured live 2026-10-02): hold it longer
const VOLUME_LAG_MS = 2500;
const intents = createIntents(PLAY_LAG_MS, { volume: VOLUME_LAG_MS });

/**
 * Send an optimistic setting through the player chain. The UI already shows it; if the latest
 * command for key fails, revert() puts back what the device still has. Returns true on success.
 */
async function sendIntent(key, fn, revert, seq = intents.start(key)) {
  const sess = authSession;
  const ok = await withDevice(fn);
  if (sess !== authSession) return false; // logged out meanwhile: the intents were reset
  intents.finish(key, performance.now());
  if (!ok && intents.latest(key, seq)) {
    revert();
    intents.drop(key);
    renderChrome();
  }
  return ok;
}

/** Flip play/pause at once in the UI; the command joins the player chain in click order. */
async function togglePlay() {
  if (state.mode === "idle") return;
  listen();
  state.progressMs = progress();
  state.progressAt = performance.now();
  const want = (state.isPlaying = !state.isPlaying);
  renderChrome();
  await sendIntent(
    "play",
    async (id) => (want ? invoke("resume", { deviceId: await needDevice(id) }) : invoke("pause")),
    () => (state.isPlaying = !want), // the device still has the state before this click
  );
  kick();
}

async function toggleShuffle() {
  if (state.mode !== "track") return;
  const want = (state.shuffle = !state.shuffle);
  renderChrome();
  await sendIntent("shuffle", () => invoke("set_shuffle", { on: want }), () => (state.shuffle = !want));
}

/** off → context → track → off. */
async function cycleRepeat() {
  if (state.mode !== "track") return;
  const before = state.repeat;
  const mode = (state.repeat = nextRepeat(before));
  renderChrome();
  await sendIntent("repeat", () => invoke("set_repeat", { mode }), () => (state.repeat = before));
}

// ---------- volume: the UI moves at once, one set_volume after 200ms of quiet ----------

const VOLUME_QUIET_MS = 200;
let volTimer = null;
let volSeq = 0; // the intent of the current burst of moves
let volBefore = null; // the volume before that burst: what a failure puts back
let volDevice = null; // the device the burst started on: a transfer meanwhile must not get its volume
let volPercent = null; // the level the burst asks for: a poll may replace state.volume meanwhile
const volKey = (deviceId) => `volume:${deviceId || ""}`; // volume intents are per device
let unmuteTo = 50;
let volumeOpen = false; // the narrow-screen slider popover

function setVolume(pct) {
  if (state.volume == null) return;
  const v = Math.round(Math.min(100, Math.max(0, pct)));
  if (v === state.volume) return;
  const deviceId = state.device && state.device.id;
  if (volTimer && volDevice !== deviceId) {
    // playback moved to another device mid-burst: the old burst goes to its device now
    clearTimeout(volTimer);
    sendVolume();
  }
  if (!volTimer) {
    // the first move of a burst: polls keep their hands off from now until its command lands
    volDevice = state.device && state.device.id;
    volSeq = intents.start(volKey(volDevice));
    volBefore = state.volume;
  }
  state.volume = volPercent = v;
  renderVolume();
  clearTimeout(volTimer);
  volTimer = setTimeout(sendVolume, VOLUME_QUIET_MS);
}

function sendVolume() {
  volTimer = null;
  const percent = volPercent;
  const before = volBefore;
  const deviceId = volDevice;
  // a failure puts the old level back only while that device is still the one on screen
  const revert = () => {
    if (state.device && state.device.id === deviceId) state.volume = before;
  };
  sendIntent(volKey(deviceId), () => invoke("set_volume", { percent, deviceId }), revert, volSeq);
}

function toggleMute() {
  if (state.volume == null) return;
  if (state.volume > 0) unmuteTo = state.volume;
  setVolume(state.volume > 0 ? 0 : unmuteTo);
}

function onVolBtn() {
  if (!small.matches) return toggleMute();
  if (volumeOpen) closeVolume(true);
  else {
    volumeOpen = true;
    $("volume").classList.add("is-open");
    renderVolume();
    $("volSlider").focus();
  }
}

function closeVolume(refocus = false) {
  if (!volumeOpen) return;
  volumeOpen = false;
  $("volume").classList.remove("is-open");
  $("volBtn").setAttribute("aria-expanded", "false");
  if (refocus) $("volBtn").focus();
}

function volumeAt(ev) {
  const r = $("volSlider").getBoundingClientRect();
  setVolume(((ev.clientX - r.left) / r.width) * 100);
}

function volumeDown(ev) {
  if (ev.button !== 0) return;
  ev.preventDefault();
  $("volSlider").focus();
  $("volSlider").setPointerCapture(ev.pointerId);
  volumeAt(ev);
}

function volumeMove(ev) {
  if ($("volSlider").hasPointerCapture(ev.pointerId)) volumeAt(ev);
}

/** Arrows ±5, Home/End: the ends. */
function volumeKey(ev) {
  const v = stepVolume(state.volume, ev.key);
  if (v === null) return;
  ev.preventDefault();
  setVolume(v);
}

// ---------- heart: the current song in Liked Songs (not a player command) ----------

// Bumped on each track change and heart click: an is_saved answer from before either is stale.
let heartGen = 0;
let libraryDenied = false; // the token lacks the library scopes: no heart this session
let savedChain = Promise.resolve(); // save/unsave in click order, so the last click wins

const isScopeError = (e) => /\b403\b|scope/i.test(String(e));

/** Liked Songs reads and writes, in order, for this login session only. */
function queueSaved(cmd, args) {
  const sess = authSession;
  const run = savedChain.then(() => (sess === authSession ? invoke(cmd, args) : null));
  savedChain = run.catch(() => {});
  return run;
}

/** The cached Liked Songs result after saving (newest first) or removing track t. */
const likedWith = (r, t, saved) => {
  const rest = (r.tracks || []).filter((x) => x.uri !== t.uri);
  return { tracks: saved ? [t, ...rest] : rest, total: Math.max(0, (r.total || 0) + (saved ? 1 : -1)) };
};

/** Ask once per track whether it's in Liked Songs. */
async function checkSaved(track) {
  const gen = ++heartGen;
  state.saved = null;
  if (!track || !track.id || isLocal(track.uri) || libraryDenied) return;
  try {
    // in the same queue as save/unsave: a read must not overtake a write still on its way
    const saved = await queueSaved("is_saved", { trackId: track.id });
    if (gen !== heartGen) return;
    state.saved = Boolean(saved);
  } catch (e) {
    if (gen !== heartGen) return;
    if (isCode(e, "AUTH_EXPIRED")) return expire();
    if (isScopeError(e)) libraryDenied = true;
    // other failures leave it unknown (disabled) until the next track
  }
  renderChrome();
}

async function toggleSaved() {
  const t = state.now;
  if (!t || !t.id || state.saved === null) return;
  const want = !state.saved;
  const gen = ++heartGen;
  state.saved = want;
  renderChrome();
  const sess = authSession;
  try {
    await queueSaved(want ? "save_track" : "unsave_track", { trackId: t.id });
    if (sess !== authSession) return;
    // Liked Songs changed: patch the cached list in place (no 20-page refetch), recount once
    const cached = lib.get("liked");
    if (cached) {
      const patched = cached.then((r) => r && likedWith(r, t, want));
      lib.set("liked", patched);
      // like libGet: a failed load must not stay cached, or Liked Songs never retries
      patched.catch(() => lib.get("liked") === patched && lib.delete("liked"));
    }
    lib.delete("likedCount");
    if (libOpened) fillLiked();
  } catch (e) {
    if (isCode(e, "AUTH_EXPIRED")) return expire();
    if (isScopeError(e)) libraryDenied = true;
    // still this click on this track: ask Spotify what is true (!want may itself be unconfirmed)
    if (gen === heartGen && state.now && state.now.id === t.id) checkSaved(t);
    renderChrome();
    toast(`Spotify didn't respond: ${reason(e)}`);
  }
}

// ---------- device picker ----------

let devicesOpen = false;
let devicesGen = 0; // the latest list_devices request: an older answer is dropped
let devicesNote = ""; // loading or error line while there is no list to show

function toggleDevices() {
  if (devicesOpen) closeDevices(true);
  else openDevices();
}

function openDevices() {
  closeVolume();
  devicesOpen = true;
  $("devicePop").hidden = false;
  $("deviceBtn").setAttribute("aria-expanded", "true");
  renderDeviceList();
  focusDevice(0);
  refreshDevices();
}

function closeDevices(refocus = false) {
  if (!devicesOpen) return;
  devicesOpen = false;
  devicesGen++; // a list still in flight would re-render a closed popover
  $("devicePop").hidden = true;
  $("deviceBtn").setAttribute("aria-expanded", "false");
  if (refocus) $("deviceBtn").focus();
}

async function refreshDevices() {
  const gen = ++devicesGen;
  if (!state.devices) devicesNote = "Looking for devices…";
  renderDeviceList();
  let list;
  try {
    list = (await invoke("list_devices")) || [];
  } catch (e) {
    if (gen !== devicesGen) return;
    if (isCode(e, "AUTH_EXPIRED")) return expire();
    devicesNote = state.devices ? "" : `Couldn't load devices — ${reason(e)}`;
    renderDeviceList();
    return;
  }
  if (gen !== devicesGen) return;
  devicesNote = "";
  if (setDevices(list) && state.mode === "idle") renderNow();
  renderDeviceList();
  renderChrome();
  // opened before the list was known: focus lands on its first row once there is one
  if (devicesOpen && !$("deviceList").contains(document.activeElement)) focusDevice(0);
}

function renderDeviceList() {
  const list = state.devices || [];
  const cur = state.device && state.device.id;
  const focused = document.activeElement && document.activeElement.dataset ? document.activeElement.dataset.device : null;
  $("deviceList").innerHTML = list
    .map((d, i) => {
      const active = cur ? d.id === cur : d.is_active;
      const tip = d.is_restricted ? "Spotify doesn't allow remote control of this device" : d.name;
      return (
        `<button class="device-row${active ? " is-active" : ""}" type="button" role="option" data-i="${i}" data-device="${esc(d.id)}"` +
        ` aria-selected="${active}" title="${esc(tip)}"${d.is_restricted ? ' aria-disabled="true"' : ""} tabindex="-1">` +
        `<span class="device-row-dot"></span><span class="device-row-name">${esc(d.name)}</span>` +
        `<span class="device-row-type">${esc(d.type || "")}</span></button>`
      );
    })
    .join("");
  $("deviceList").hidden = !list.length;
  const note = $("devicePop").querySelector(".device-note");
  note.textContent = devicesNote || (state.devices && !list.length ? "Open Spotify on a phone, computer or speaker to see it here." : "");
  note.hidden = !note.textContent;
  if (focused) $("deviceList").querySelector(`[data-device="${CSS.escape(focused)}"]`)?.focus();
}

const deviceRows = () => [...$("deviceList").querySelectorAll('.device-row:not([aria-disabled="true"])')];

/** Focus the row step rows away from the focused one; 0 = the active row (or the first). */
function focusDevice(step) {
  const rows = deviceRows();
  if (!rows.length) return;
  const at = rows.indexOf(document.activeElement);
  if (step === 0 || at < 0) return (rows.find((r) => r.classList.contains("is-active")) || rows[0]).focus();
  rows[(at + step + rows.length) % rows.length].focus();
}

/** ArrowDown on the chip opens the list; arrows move between rows (Enter presses the focused one). */
function onDeviceKey(ev) {
  const step = { ArrowDown: 1, ArrowUp: -1 }[ev.key];
  if (!step) return;
  ev.preventDefault();
  if (!devicesOpen) openDevices();
  else focusDevice(step);
}

function onDeviceClick(ev) {
  const row = ev.target.closest(".device-row");
  if (!row || row.getAttribute("aria-disabled") === "true") return;
  const d = state.devices && state.devices[Number(row.dataset.i)];
  if (d) pickDevice(d);
}

/** Move playback to d. The chip shows d at once; a failure puts the old device back. */
async function pickDevice(d) {
  closeDevices(true);
  if (state.device && state.device.id === d.id) return;
  const before = state.device;
  // a volume burst on the old device goes out now, to that device; the new one starts fresh
  if (volTimer) {
    clearTimeout(volTimer);
    sendVolume();
  }
  state.device = { id: d.id, name: d.name };
  state.volume = d.volume_percent ?? null;
  state.supportsVolume = Boolean(d.supports_volume);
  renderChrome();
  const seq = intents.start("device");
  const sess = authSession;
  let failed = null;
  // the error is handled here, not by withDevice: rediscovering would retry a device that's gone
  await changeTrack(() =>
    invoke("transfer_playback", { deviceId: d.id, play: state.isPlaying }).catch((e) => {
      if (isCode(e, "AUTH_EXPIRED")) throw e;
      failed = e;
    }),
  );
  if (sess !== authSession) return;
  intents.finish("device", performance.now());
  if (failed) {
    if (intents.latest("device", seq)) {
      state.device = before;
      intents.drop("device");
      renderChrome();
    }
    if (isCode(failed, "NO_ACTIVE_DEVICE") || /\b404\b/.test(String(failed))) {
      toast(`${d.name} isn't available any more`);
      refreshDevices();
    } else {
      toast(`Spotify didn't respond: ${reason(failed)}`);
    }
  }
  kick();
}

/** A press outside an open popover closes it. */
function onOutside(ev) {
  if (devicesOpen && !ev.target.closest(".device-wrap")) closeDevices();
  if (volumeOpen && !ev.target.closest("#volume")) closeVolume();
}

async function skip(cmd) {
  if (!state.now) return;
  await changeTrack(() => invoke(cmd));
  kick();
}

function seekClick(ev) {
  if (!state.now || !state.now.duration_ms || !canSeek()) return;
  const r = $("scrub").getBoundingClientRect();
  seekTo(Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width)) * state.now.duration_ms);
}

const SEEK_STEP_MS = 5000;

/** Arrow keys on the focused scrub bar: ±5s, Home/End jump to the ends. */
function seekKey(ev) {
  if (!state.now || !state.now.duration_ms || !canSeek()) return;
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
  if (gen !== trackGen || !state.now) return; // the track changed (or is unknown) since this seek was made
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
  closeDevices(); // popovers belong to the stage, which goes inert
  closeVolume();
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

const isLocal = (uri) => String(uri).startsWith("spotify:local:");

/** "A, B" as artist links; plain text when Spotify gave no artist ids (local files). */
function artistLinks(t) {
  const list = (t.artist_list || []).filter((a) => a && a.id && a.name);
  if (!list.length) return esc(t.artists);
  return list.map((a) => `<button class="artist-link" type="button" data-artist="${esc(a.id)}">${esc(a.name)}</button>`).join(", ");
}

const QUEUE_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M11 5h2v6h6v2h-6v6h-2v-6H5v-2h6z" /></svg>';

/**
 * A playable track row. num: show the position; art: show the cover (album rows skip it, it's the same every row).
 * The title button covers the whole row (CSS), so a click anywhere plays it; the artist links and "+" sit on top.
 */
function trackRow(t, i, { num, art }) {
  const local = isLocal(t.uri);
  const kind = `${num ? " has-num" : ""}${art ? " has-art" : ""}${local ? " is-local" : ""}`;
  const tip = local ? `${t.name} (a local file: play it in Spotify)` : t.name;
  return (
    `<div class="row row-track${kind}" data-i="${i}" title="${esc(tip)}">` +
    (num ? `<span class="row-num">${i + 1}</span>` : "") +
    (art ? `<span class="art row-art">${artHtml(t.cover, t.name)}</span>` : "") +
    `<span class="row-text"><button class="row-title row-play" type="button"${local ? " disabled" : ""}>${esc(t.name)}</button>` +
    `<span class="row-sub">${artistLinks(t)}</span></span>` +
    `<span class="row-time">${fmtTime(t.duration_ms)}</span>` +
    // Spotify can't queue a local file
    (local ? "<span></span>" : `<button class="row-queue" type="button" aria-label="Add to queue" title="Add to queue">${QUEUE_ICON}</button>`) +
    `</div>`
  );
}

/**
 * A click in a list of track rows: an artist link opens the artist, "+" queues the song, anything else
 * plays from that row. Returns true if the click was on a row or a link.
 */
function onTrackClick(e, tracks, play) {
  const link = e.target.closest(".artist-link");
  if (link) return openArtist(link), true;
  const row = e.target.closest(".row-track");
  if (!row) return false;
  const i = Number(row.dataset.i);
  if (!tracks[i]) return true;
  if (e.target.closest(".row-queue")) addToQueue(tracks[i]);
  else if (e.target.closest(".row-play")) play(i);
  return true;
}

/** A cover tile for a shelf or the artist page; round = an artist. sub is HTML. */
function tile(item, i, { round = false, sub = "", attrs = "" } = {}) {
  return (
    `<button class="album${round ? " is-artist" : ""}" type="button" data-i="${i}" title="${esc(item.name)}"${attrs}>` +
    `<span class="art">${artHtml(item.cover, item.name)}</span>` +
    `<span class="album-name">${esc(item.name)}</span>` +
    (sub ? `<span class="album-sub">${sub}</span>` : "") +
    `</button>`
  );
}

/** The tile behind a click in a shelf, or null. */
const tileAt = (e, list) => {
  const el = e.target.closest(".album[data-i]");
  return (el && list[Number(el.dataset.i)]) || null;
};

// ---------- add to queue (a player command, but it doesn't change the track) ----------

async function addToQueue(t) {
  if (!t || isLocal(t.uri)) return;
  const ok = await withDevice(async (id) => invoke("add_to_queue", { deviceId: await needDevice(id), uri: t.uri }));
  if (!ok) return;
  toast("Added to Up next");
  queueDirty = true;
}

// ---------- library: level 1 groups, level 2 details ----------

let playlists = null; // loaded once, then cached
let playlistsLoading = false;
let detailTracks = [];
let detailAlbums = []; // the artist page's albums and singles
let listScroll = 0;
let libOpened = false; // the groups were asked for this session: mix changes re-render theirs

const NAV_MAX = 5;
let navStack = []; // the details Back returns to, oldest first; empty = Back shows the list
let curDetail = null; // the detail on screen ({kind, id, name, cover, sub}), null = the list

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
  loadGroups();
}

function showList() {
  overlayRev++;
  state.gen.detail++; // drop any detail response still in flight
  detailTracks = [];
  detailAlbums = [];
  navStack = [];
  curDetail = null;
  $("libDetail").hidden = true;
  $("libBack").hidden = true;
  $("libLevel1").hidden = false;
  $("libTitle").hidden = false;
  $("sheetBody").scrollTop = listScroll;
}

function goBack() {
  const prev = navStack.pop();
  if (prev) openDetail(prev, false);
  else showList();
}

// Library groups: each loads on its own, once per login session. A missing scope (403) hides
// its group without a word; any other failure leaves one line in it and retries on the next open.
const lib = new Map(); // cache key → promise of the invoke result

function libGet(key, cmd, args) {
  let p = lib.get(key);
  if (!p) {
    p = invoke(cmd, args);
    lib.set(key, p);
    p.catch((e) => {
      if (!isScopeError(e) && lib.get(key) === p) lib.delete(key);
    });
  }
  return p;
}

function loadGroups() {
  libOpened = true;
  if (!playlists) loadPlaylists();
  fillLiked();
  fillTop();
  fillAlbums();
  fillFollowing();
  renderMixes();
}

/** Show one group's data via render(data); what names it in an error line. */
async function fillGroup(group, load, render, what) {
  try {
    const data = await load();
    const status = group.querySelector(".status");
    if (status) setEl(status, "");
    render(data);
  } catch (e) {
    if (overlayFailed(e)) return;
    group.hidden = isScopeError(e);
    // the Liked Songs row has no status line: its subtitle says it
    setEl(group.querySelector(".status") || group.querySelector(".row-sub"), `Couldn't load ${what} — ${reason(e)}`);
  }
}

function fillLiked() {
  const row = $("libLiked");
  const sub = row.querySelector(".row-sub");
  row.hidden = false;
  if (!sub.textContent) setEl(sub, "Loading…");
  // the count alone is one request; the list (up to 20 pages) loads only when opened
  fillGroup(row, () => libGet("likedCount", "liked_count"), (n) => setEl(sub, plural(n || 0, "song", "songs")), "your liked songs");
}

let savedAlbums = [];

function fillAlbums() {
  const group = $("libAlbums");
  fillGroup(
    group,
    () => libGet("albums", "get_saved_albums"),
    (list) => {
      savedAlbums = (list || []).filter((a) => a && a.id);
      group.hidden = !savedAlbums.length;
      group.querySelector(".albums").innerHTML = savedAlbums.map((a, i) => tile(a, i, { sub: esc(a.artists) })).join("");
    },
    "your albums",
  );
}

let followed = [];

function fillFollowing() {
  const group = $("libFollowing");
  fillGroup(
    group,
    () => libGet("following", "get_followed_artists"),
    (list) => {
      followed = (list || []).filter((a) => a && a.id);
      group.hidden = !followed.length;
      group.querySelector(".albums").innerHTML = followed.map((a, i) => tile({ name: a.name, cover: a.image }, i, { round: true })).join("");
    },
    "the artists you follow",
  );
}

// ---------- your top: 3 time ranges, each cached ----------

const TOP_TRACKS_SHOWN = 10; // of 20: the rest of the Library stays in reach
let topRange = "short_term";
let topGen = 0; // the latest tab: an older range's answer is dropped
let topTrackList = [];
let topArtistList = [];

async function fillTop() {
  const range = topRange;
  const gen = ++topGen;
  const group = $("libTop");
  for (const b of $("topTabs").querySelectorAll("[data-range]")) b.setAttribute("aria-selected", String(b.dataset.range === range));
  const results = await Promise.allSettled([topTracks(range), libGet(`top:artists:${range}`, "get_top", { kind: "artists", range })]);
  if (gen !== topGen) return;
  const failed = results.filter((r) => r.status === "rejected").map((r) => r.reason);
  if (failed.some((e) => isCode(e, "AUTH_EXPIRED"))) return void expire();
  if (failed.length === 2 && failed.every(isScopeError)) return void (group.hidden = true);
  const [tracks, artists] = results.map((r) => (r.status === "fulfilled" && r.value) || []);
  topTrackList = tracks.filter((t) => t && t.uri).slice(0, TOP_TRACKS_SHOWN);
  topArtistList = artists.filter((a) => a && a.id);
  group.hidden = false;
  $("topTracks").innerHTML = topTrackList.map((t, i) => trackRow(t, i, { num: false, art: true })).join("");
  $("topArtists").innerHTML = topArtistList.map((a, i) => tile({ name: a.name, cover: a.image }, i, { round: true })).join("");
  $("topArtists").hidden = !topArtistList.length;
  const err = failed.find((e) => !isScopeError(e));
  const empty = !topTrackList.length && !topArtistList.length;
  setEl(group.querySelector(".status"), err ? `Couldn't load your top — ${reason(err)}` : empty ? "Nothing here yet for this time range." : "");
}

function onTopTab(e) {
  const b = e.target.closest("[data-range]");
  if (!b || b.dataset.range === topRange) return;
  topRange = b.dataset.range;
  fillTop();
}

// ---------- Spotify mixes: Spotify doesn't list its own playlists, so remember the ones seen playing ----------

const MIXES_KEY = "therun.knownMixes";
const MIX_NOTE = "Spotify doesn't share the track list of its own mixes.";
let knownMixes = null; // [{id, seen}], newest first; null = not read from storage yet
let notedContext = null; // the last playback context noted, so a poll doesn't note it every second
const refusedMixes = new Set(); // mixes Spotify wouldn't start this session: history must not bring them back
const mixInfo = new Map(); // playlist id → promise of {name, cover} or null
let mixList = []; // the tiles on screen: {id, name, cover}
let mixesGen = 0;

function mixes() {
  if (!knownMixes) {
    try {
      knownMixes = noteMixes(JSON.parse(localStorage.getItem(MIXES_KEY) || "[]"), [], [], "");
    } catch {
      knownMixes = []; // storage blocked or broken: this session's mixes only
    }
  }
  return knownMixes;
}

const ownIds = () => [...(playlists || []).map((p) => p.id), ...refusedMixes];

/** Note playback contexts (newest first); a new mix is stored and shown. */
function noteContexts(uris) {
  const before = mixes();
  const next = noteMixes(before, uris, ownIds(), new Date().toISOString());
  // only a seen time moved: not worth a write
  if (next.length === before.length && next.every((m, i) => m.id === before[i].id)) return;
  knownMixes = next;
  try {
    localStorage.setItem(MIXES_KEY, JSON.stringify(next));
  } catch {
    /* kept in memory for this session */
  }
  if (libOpened) renderMixes();
}

function mixInfoFor(id) {
  let p = mixInfo.get(id);
  if (!p) {
    p = invoke("mix_info", { playlistId: id }).catch((e) => {
      if (isCode(e, "AUTH_EXPIRED")) expire();
      return null; // a letter tile named "Spotify mix"
    });
    mixInfo.set(id, p);
  }
  return p;
}

async function renderMixes() {
  const gen = ++mixesGen;
  const own = new Set(ownIds()); // the playlists may have loaded after a mix was noted
  const list = mixes().filter((m) => !own.has(m.id));
  const infos = await Promise.all(list.map((m) => mixInfoFor(m.id)));
  if (gen !== mixesGen) return;
  mixList = list.map((m, i) => ({ id: m.id, name: (infos[i] && infos[i].name) || "Spotify mix", cover: (infos[i] && infos[i].cover) || null }));
  const group = $("libMixes");
  group.hidden = !mixList.length;
  group.querySelector(".albums").innerHTML = mixList.map((m, i) => tile(m, i, { attrs: ` data-mix="${esc(m.id)}"` })).join("");
}

/** Spotify refuses some of its own mixes: 403, or a 404 that isn't about the device. */
// Rust already tags a device 404 as NO_ACTIVE_DEVICE: any other 403/404 is the mix itself
const mixRefused = (e) => /\b40[34]\b/.test(String(e)) && !isCode(e, "NO_ACTIVE_DEVICE");

async function playMix(src) {
  const rev = overlayRev;
  let refused = false;
  const ok = await changeTrack(async (id) => {
    const deviceId = await needDevice(id);
    try {
      await invoke("play_context", { deviceId, contextUri: `spotify:playlist:${src.id}` });
    } catch (e) {
      if (!mixRefused(e)) throw e;
      refused = true; // handled here: withDevice would retry it on another device
    }
  });
  kick();
  if (refused) {
    toast("Spotify won't start this mix from here");
    refusedMixes.add(src.id);
    noteContexts([]); // drops it from the stored list
    renderMixes();
    if (rev === overlayRev) $("detailPlay").disabled = true;
    return;
  }
  if (ok && rev === overlayRev) closeOverlay();
}

/** The next login may be another account: forget everything the Library loaded (not the stored mixes). */
function resetLibrary() {
  lib.clear();
  mixInfo.clear();
  refusedMixes.clear();
  knownMixes = null;
  notedContext = null;
  libOpened = false;
  navStack = [];
  curDetail = null;
  topRange = "short_term";
  topGen++;
  mixesGen++;
  savedAlbums = [];
  followed = [];
  topTrackList = [];
  topArtistList = [];
  mixList = [];
  detailAlbums = [];
  for (const id of ["libLiked", "libTop", "libAlbums", "libFollowing", "libMixes"]) $(id).hidden = true;
  for (const el of $("libLevel1").querySelectorAll(".albums, #topTracks")) el.innerHTML = "";
  for (const el of $("libLevel1").querySelectorAll(".group .status")) setEl(el, "");
  setEl($("libLiked").querySelector(".row-sub"), "");
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
  noteContexts([]); // own playlists noted before this load aren't mixes
  renderMixes();
}

const LIKED_ART = '<svg class="liked-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 20.3S3.8 15.5 3.8 9.4A4.4 4.4 0 0 1 12 7.2a4.4 4.4 0 0 1 8.2 2.2c0 6.1-8.2 10.9-8.2 10.9z" /></svg>';

const openArtist = (link) => openDetail({ kind: "artist", id: link.dataset.artist, name: link.textContent, cover: null, sub: "Artist" });
const openArtistTile = (a) => a && openDetail({ kind: "artist", id: a.id, name: a.name, cover: a.image, sub: "Artist" });

/**
 * Level 2: {kind, id, name, cover, sub}, kind = playlist, album, liked, mix or artist.
 * push: Back returns to what this replaced (false when Back itself opens it).
 */
async function openDetail(src, push = true) {
  if (state.overlay !== "library") {
    openOverlay("library");
    $("sheet").focus();
    navStack = []; // from the stage or Search: Back shows the list
    if (!curDetail) listScroll = $("sheetBody").scrollTop;
    loadGroups(); // so Back has a list to show
  } else if (push && curDetail) {
    navStack.push(curDetail);
    if (navStack.length > NAV_MAX) navStack.shift();
  } else if (push) {
    listScroll = $("sheetBody").scrollTop;
    navStack = [];
  }
  curDetail = src;
  overlayRev++;
  const gen = ++state.gen.detail;
  detailTracks = [];
  detailAlbums = [];
  $("libLevel1").hidden = true;
  $("libTitle").hidden = true;
  $("libBack").hidden = false;
  $("libBack").querySelector(".back-label").textContent = navStack.length ? "Back" : "Library";
  $("libDetail").hidden = false;
  const cover = $("detailCover");
  cover.classList.toggle("is-round", src.kind === "artist");
  cover.classList.toggle("is-liked", src.kind === "liked");
  cover.innerHTML = src.kind === "liked" ? LIKED_ART : artHtml(src.cover, src.name);
  $("detailName").textContent = src.name;
  $("detailName").title = src.name;
  setText("detailSub", src.sub);
  setText("detailNote", src.kind === "mix" ? MIX_NOTE : "");
  $("detailPlay").hidden = src.kind === "artist";
  $("detailPlay").disabled = src.kind !== "mix"; // a mix plays by its uri: nothing to load
  $("detailRows").innerHTML = "";
  setText("detailStatus", { mix: "", artist: "Loading albums…" }[src.kind] ?? "Loading tracks…");
  $("sheetBody").scrollTop = 0;
  if (src.kind === "mix") return;
  if (src.kind === "artist") return loadArtist(src, gen);

  let tracks;
  let total = 0;
  try {
    if (src.kind === "liked") ({ tracks, total } = (await libGet("liked", "get_saved_tracks")) || {});
    else if (src.kind === "album") tracks = await invoke("get_album_tracks", { albumId: src.id });
    else tracks = await invoke("get_playlist_tracks", { playlistId: src.id });
  } catch (e) {
    if (gen !== state.gen.detail) return;
    if (!overlayFailed(e)) setText("detailStatus", `Couldn't load tracks — ${reason(e)}`);
    return;
  }
  if (gen !== state.gen.detail) return; // a newer detail (or the list) took over

  detailTracks = (tracks || []).filter((t) => t && t.uri);
  const n = detailTracks.length;
  if (src.kind === "playlist") setText("detailSub", plural(n, "track", "tracks"));
  if (src.kind === "liked") {
    total = Math.max(total || 0, n);
    setText("detailSub", plural(total, "song", "songs"));
    setText("detailNote", total > n ? `Showing your newest ${n} of ${total}` : "");
  }
  const empty = { album: "This album is empty.", liked: "No liked songs yet." }[src.kind] || "This playlist is empty.";
  setText("detailStatus", n ? "" : empty);
  $("detailRows").innerHTML = detailTracks.map((t, i) => trackRow(t, i, { num: true, art: src.kind !== "album" })).join("");
  $("detailPlay").disabled = !detailTracks.some((t) => !isLocal(t.uri));
}

const kindLabel = (k) => (k ? k[0].toUpperCase() + k.slice(1) : "Album");

const TOP_RANGES = ["short_term", "medium_term", "long_term"];

/** Top tracks at Spotify's max of 50, one cache entry shared by the Library group and artist pages. */
const topTracks = (range) => libGet(`top50:tracks:${range}`, "get_top", { kind: "tracks", range, limit: 50 });

/** Your own top tracks (50 per range) and Liked Songs, best first; any that fails is just skipped. */
function favoriteSources(optional) {
  return Promise.all([
    ...TOP_RANGES.map((range) => topTracks(range).catch(optional)),
    libGet("liked", "get_saved_tracks").then((r) => (r && r.tracks) || null).catch(optional),
  ]);
}

/** The artist page: photo and name (best effort), albums and singles, then your favorites by them. */
async function loadArtist(src, gen) {
  const optional = (e) => (isCode(e, "AUTH_EXPIRED") ? Promise.reject(e) : null); // the page works without these
  // favorites come from your top tracks and Liked Songs: a cold Liked cache is many pages, so the
  // albums don't wait for it
  const sources = favoriteSources(optional);
  sources.catch(() => {}); // awaited below; until then a rejection must not count as unhandled
  let info, albums;
  try {
    [info, albums] = await Promise.all([
      invoke("get_artist", { artistId: src.id }).catch(optional),
      invoke("get_artist_albums", { artistId: src.id }),
    ]);
  } catch (e) {
    if (gen !== state.gen.detail) return;
    if (!overlayFailed(e)) setText("detailStatus", `Couldn't load albums — ${reason(e)}`);
    return;
  }
  if (gen !== state.gen.detail) return;
  if (info) {
    // kept on src: Back to this page shows them at once
    src.name = info.name || src.name;
    src.cover = info.image || src.cover;
    $("detailCover").innerHTML = artHtml(src.cover, src.name);
    $("detailName").textContent = src.name;
    $("detailName").title = src.name;
  }
  detailAlbums = (albums || []).filter((a) => a && a.id);
  detailTracks = [];
  renderArtist();
  let lists;
  try {
    lists = await sources;
  } catch (e) {
    return void (gen === state.gen.detail && overlayFailed(e));
  }
  if (gen !== state.gen.detail) return;
  // Spotify no longer gives an artist's top tracks or play counts: rank from your own listening
  detailTracks = favoritesBy(src.id, lists || []);
  renderArtist();
}

function renderArtist() {
  $("detailPlay").hidden = !detailTracks.length;
  $("detailPlay").disabled = !detailTracks.length;
  setText("detailStatus", detailAlbums.length || detailTracks.length ? "" : "No albums or singles.");
  const favorites = detailTracks.length
    ? `<h3 class="group-title">Your favorites</h3><div class="rows">${detailTracks.map((t, i) => trackRow(t, i, { num: true, art: true })).join("")}</div>` +
      (detailAlbums.length ? `<h3 class="group-title">Albums and singles</h3>` : "")
    : "";
  $("detailRows").innerHTML =
    favorites +
    `<div class="albums is-grid">` +
    detailAlbums.map((a, i) => tile(a, i, { sub: `<span class="album-kind">${esc(kindLabel(a.kind))}</span>${esc(a.year || "")}` })).join("") +
    `</div>`;
}

const openAlbum = (a, sub = a.artists) => openDetail({ kind: "album", id: a.id, name: a.name, cover: a.cover, sub });

function onDetailClick(e) {
  if (onTrackClick(e, detailTracks, playDetailFrom)) return;
  const a = tileAt(e, detailAlbums);
  if (a) openAlbum(a, curDetail ? curDetail.name : "");
}

function onDetailPlay() {
  if (curDetail && curDetail.kind === "mix") playMix(curDetail);
  else playDetailFrom(0);
}

/** Play tracks from row i on; the overlay closes only if the user is still on that view. */
const PLAY_URIS_MAX = 200;

async function playFrom(tracks, i) {
  // Spotify lists local files in playlists but rejects them in play requests
  // capped: Liked Songs can hold 1000 rows, and Spotify's limit for one play request is unknown
  const uris = tracks.slice(i).map((t) => t.uri).filter((u) => !isLocal(u)).slice(0, PLAY_URIS_MAX);
  const rev = overlayRev;
  if ((await playUris(uris)) && rev === overlayRev) closeOverlay();
}

const playDetailFrom = (i) => playFrom(detailTracks, i);

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

function onSearchClick(e) {
  // a song plays alone: the rest of the results aren't a playlist
  if (onTrackClick(e, searchHits.tracks, (i) => playFrom([searchHits.tracks[i]], 0))) return;
  const al = e.target.closest("[data-album]");
  const a = al && searchHits.albums[Number(al.dataset.album)];
  if (a) openAlbum(a);
}

// ---------- keyboard: Space = play/pause, Esc = close the overlay ----------

const typing = (t) => t && t.closest && t.closest("input, textarea, select, [contenteditable]");

function onKey(e) {
  // Esc closes the innermost thing: a popover first, then the overlay
  if (e.key === "Escape" && e.type === "keydown" && (devicesOpen || volumeOpen || state.overlay)) {
    e.preventDefault();
    if (devicesOpen) closeDevices(true);
    else if (volumeOpen) closeVolume(true);
    else closeOverlay();
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
  $("shuffleBtn").addEventListener("click", toggleShuffle);
  $("repeatBtn").addEventListener("click", cycleRepeat);
  $("heartBtn").addEventListener("click", toggleSaved);
  $("volBtn").addEventListener("click", onVolBtn);
  $("volSlider").addEventListener("pointerdown", volumeDown);
  $("volSlider").addEventListener("pointermove", volumeMove);
  $("volSlider").addEventListener("keydown", volumeKey);
  $("deviceBtn").addEventListener("click", toggleDevices);
  document.querySelector(".device-wrap").addEventListener("keydown", onDeviceKey);
  $("deviceList").addEventListener("click", onDeviceClick);
  document.addEventListener("pointerdown", onOutside);

  $("libraryBtn").addEventListener("click", openLibrary);
  $("searchBtn").addEventListener("click", openSearch);
  for (const id of ["library", "search"]) {
    $(id).addEventListener("click", (e) => e.target.closest("[data-close]") && closeOverlay());
    $(id).addEventListener("error", onImgError, true);
  }
  $("libBack").addEventListener("click", goBack);
  $("libList").addEventListener("click", (e) => {
    const row = e.target.closest("[data-id]");
    const p = row && playlists && playlists[Number(row.dataset.i)];
    if (p) openDetail({ kind: "playlist", id: p.id, name: p.name, cover: pickImage(p.images), sub: plural((p.tracks && p.tracks.total) || 0, "track", "tracks") });
  });
  $("libLiked").addEventListener("click", () => openDetail({ kind: "liked", id: "liked", name: "Liked Songs", cover: null, sub: "" }));
  $("topTabs").addEventListener("click", onTopTab);
  $("topTracks").addEventListener("click", (e) => onTrackClick(e, topTrackList, (i) => playFrom(topTrackList, i)));
  $("topArtists").addEventListener("click", (e) => openArtistTile(tileAt(e, topArtistList)));
  $("libFollowing").addEventListener("click", (e) => openArtistTile(tileAt(e, followed)));
  $("libAlbums").addEventListener("click", (e) => {
    const a = tileAt(e, savedAlbums);
    if (a) openAlbum(a);
  });
  $("libMixes").addEventListener("click", (e) => {
    const m = tileAt(e, mixList);
    if (m) openDetail({ kind: "mix", id: m.id, name: m.name, cover: m.cover, sub: "Made by Spotify" });
  });
  $("detailRows").addEventListener("click", onDetailClick);
  $("detailPlay").addEventListener("click", onDetailPlay);
  $("nowArtist").addEventListener("click", (e) => {
    const link = e.target.closest(".artist-link");
    if (link) openArtist(link);
  });
  $("searchInput").addEventListener("input", onSearchInput);
  $("searchResults").addEventListener("click", onSearchClick);
  document.addEventListener("keydown", onKey);
  document.addEventListener("keyup", onKey);
  addEventListener("resize", center);
  small.addEventListener("change", () => {
    closeVolume(); // the slider popover exists only on narrow screens
    renderVolume();
    if (state.loaded) renderRun();
  });
  $("run").addEventListener("error", onImgError, true);
  // dev harness only: the `artist` scenario opens a page by id
  if (window.__mock) window.__openArtist = (id) => openDetail({ kind: "artist", id, name: "", cover: null, sub: "Artist" });
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
