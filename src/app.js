// The Run — stage UI: boot, sequential poll loop, the run of covers, transport.
import { fmtTime, esc } from "./lib/format.js";
import { FALLBACK, extractColors } from "./lib/color.js";
import { buildRun, mergeHistory, measure, flip, coverTarget } from "./lib/timeline.js";
import { favoritesBy } from "./lib/favorites.js";
import { createIntents, nextRepeat, stepVolume } from "./lib/transport.js";
import { noteMixes } from "./lib/mixes.js";
import { CONNECTING, NEEDS_LOGIN, isTheRun, thisMacRow, preferredDevice } from "./lib/engine.js";
import { mediaAction, mediaChanged, mediaPayload } from "./lib/media.js";
import { ERROR_POLL_MS, GIVE_UP_FAILURES, gaveUp, pollDelay } from "./lib/poll.js";
import { isEngineDevice, isLocal, volumeTiming } from "./lib/route.js";
import { SESSION_KEY, parseSession, playSession, sessionToSave, resumeSource, originUri } from "./lib/session.js";
import { PENDING_MS, createPending } from "./lib/pending.js";
import { skeletonRows, skeletonTiles } from "./lib/skeleton.js";
import { PAGE_SIZE, pageOffsets, foldPages } from "./lib/paging.js";

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
  gen: { detail: 0, search: 0, page: 0 },
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
  const loggedOut = !$("stage").hidden; // leaving a session (not the first screen at launch)
  stopPolling();
  authSession++;
  playlistsLoading = false; // a load from the old session will never finish
  playerChain = Promise.resolve(); // a player command from the old session will never finish either
  thisMacBusy = "";
  engine = null; // the next stage reads it fresh: the player re-checks the account on restart
  accountP = null; // the next login may be another account: ask /me again
  accountNow = null;
  activeId = null;
  lastPoll = null;
  movingTo = null;
  pending.reset(); // the stage re-renders its loaders on the next start
  renderPending(false);
  clearMedia();
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
  state.gen.page++; // a results page still loading must not land in the next session
  page.lists = {};
  // the next login may be another account: drop everything that belonged to this one
  playlists = null;
  playlistsStale = false;
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
    if (status === "ok") {
      restartEngine(); // maybe another account now: the player re-checks it
      return startStage();
    }
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
let failures = 0; // polls failed in a row: retried quietly until gaveUp()
let pollEpoch = 0; // bumped on every start/stop: a poll from an older epoch must not touch state

function startPolling() {
  pollEpoch++;
  clearTimeout(seekTimer); // state.now is reset below: no seek may outlive it
  trackGen++;
  polling = true;
  inFlight = false;
  pollAgain = false;
  tick = 0;
  failures = 0;
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
  try {
    await refresh(epoch);
    if (epoch !== pollEpoch) return;
    state.error = null;
    failures = 0;
  } catch (e) {
    if (epoch !== pollEpoch) return; // stale: the session it belonged to is gone
    inFlight = false;
    if (isCode(e, "AUTH_EXPIRED")) return expire();
    failures++;
    state.error = reason(e);
    // a blip retries quietly (the last view stays, or a loader before the first load); only a
    // run of failures is an error worth showing
    if (failures === GIVE_UP_FAILURES && state.loaded) toast("Can't reach Spotify. Retrying.");
    renderNow();
  }
  inFlight = false;
  if (!polling) return;
  // hidden: 3s while something plays (Now Playing, media keys), 10s when idle; visible again restarts
  let delay = pollDelay({ hidden: document.hidden, mode: state.mode, failures });
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
  lastPoll = s || { active: false };
  activeId = active ? s.device_id || null : null; // routes player commands (isLocal)
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
  // a play the user started is confirmed by a poll that began after it landed
  if (pending.onPoll({ isPlaying: Boolean(active && s.is_playing), trackUri: track && track.uri, at: startedAt })) clearPending();
  noteSession(false);

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
  maybeResume();
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
    const d = preferredDevice(list);
    state.device = d ? { id: d.id, name: d.name } : null;
  }
  if (changed && devicesOpen) renderDeviceList();
  return changed;
}

/** Fetch devices for a transport command: this Mac first (see preferredDevice), or null. */
async function discover() {
  const list = await fetchOr("list_devices");
  if (!list) return null;
  setDevices(list);
  const d = preferredDevice(list);
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
  // the play button shows on hover/focus for past and next covers (CSS hides it on "now")
  el.innerHTML =
    `<div class="art">${artHtml(t.cover, t.name, 'crossorigin="anonymous"')}</div>` +
    (isLocalFile(t.uri) ? "" : `<button class="cover-play" type="button" aria-label="${esc(`Play ${t.name}`)}">${PLAY_ICON}</button>`) +
    `<figcaption><span class="ct">${esc(t.name)}</span><span class="ca">${esc(t.artists)}</span></figcaption>`;
  return el;
}

const PLAY_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.4v13.2c0 .8.9 1.3 1.6.8l10-6.6a1 1 0 0 0 0-1.6l-10-6.6C8.9 4.1 8 4.6 8 5.4z" /></svg>';

let runItems = new Map(); // the covers on screen: key → buildRun item

function renderRun() {
  const run = $("run");
  const prev = measure(run);
  const limits = small.matches ? { maxPast: 2, maxNext: 4 } : { maxPast: 4, maxNext: 8 };
  const items = buildRun({ history: history(), now: state.now, queue: state.queue }, limits);
  runItems = new Map(items.map((it) => [it.key, it]));

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

/** A play button on a past or next cover: jump to that song (see coverTarget). */
function onRunClick(e) {
  const btn = e.target.closest(".cover-play");
  const cover = btn && btn.closest(".cover");
  if (cover) playCover(runItems.get(cover.dataset.key));
}

async function playCover(item) {
  if (!item || item.role === "now" || isLocalFile(item.track.uri)) return;
  const last = readSession();
  // Spotify can report no context for a load we started (uris, or a slow state update):
  // the saved origin playlist/album is then the context
  const ctx = state.contextUri || (last && (last.contextUri || originUri(last.origin))) || null;
  // what's known to be in the playing context: its loaded rows, or the list the saved play used
  const sameAsLast = last && ctx && (originUri(last.origin) === ctx || last.contextUri === ctx);
  const target = coverTarget(item, {
    contextUri: ctx,
    members: (ctx && knownRows.get(ctx)) || (sameAsLast && last.uris) || null,
    listUris: last && last.uris,
    nowUri: state.now && state.now.uri,
    nextUris: [...runItems.values()].filter((it) => it.role === "next").map((it) => it.track.uri),
    historyContext: item.role === "past" ? (history().find((h) => h.track && h.track.uri === item.track.uri) || {}).context_uri || null : null,
  });
  if (!target) return;
  if (target.uris) target.uris = target.uris.filter((u) => !isLocalFile(u));
  // a jump inside the playing playlist/album keeps where it came from: its origin and its full
  // track list, so the next jump still knows every member (not just the visible covers)
  // only a jump inside the same playlist/album keeps the saved origin: a past cover from another
  // one starts its own, even when that playlist shares the track
  const sameSource = target.contextUri
    ? target.contextUri === ctx && Boolean(sameAsLast)
    : Boolean(last && last.uris && last.uris.includes(target.trackUri));
  // keep the full member list for the next jump (never sent with a context: Spirc takes one source)
  await startPlay(target, { kind: "cover", origin: sameSource ? last.origin : null, members: sameSource ? last.uris : null });
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
    title.classList.remove("is-connecting");
    title.textContent = t.name;
    title.title = t.name;
    setNowArtist(playPending() ? STARTING : artistLinks(t));
    setText("nowAlbum", t.album);
    setText("emptyState", "");
    return;
  }
  let head = "Nothing playing";
  let line = "Pick a playlist from your library to start.";
  if (state.mode === "other" && state.device) {
    head = `Playing on ${state.device.name}`;
    line = "An ad or a podcast is on. Songs show up here.";
  } else if (!state.loaded && !gaveUp(failures)) {
    head = "Connecting…"; // first load or quiet retries: a loader, not an error
    line = "";
  } else if (!state.loaded) {
    head = "Can't reach Spotify";
    line = "Check your connection. Retrying every few seconds.";
  } else if (state.devices && state.devices.length === 0) {
    head = "Open Spotify on a device";
    line = "Your Mac, phone, or speaker — then press play.";
  }
  title.textContent = head;
  title.title = "";
  title.classList.toggle("is-connecting", !state.loaded && !gaveUp(failures));
  setNowArtist(playPending() ? STARTING : "");
  setText("nowAlbum", "");
  setText("emptyState", line);
}

function renderChrome() {
  const stage = $("stage");
  const mode = state.mode;
  stage.dataset.mode = mode;
  stage.classList.toggle("is-playing", state.isPlaying && mode !== "idle");
  $("playBtn").setAttribute("aria-label", state.isPlaying ? "Pause" : "Play");
  const starting = playPending();
  $("playBtn").classList.toggle("is-pending", starting);
  if (starting) $("playBtn").setAttribute("aria-busy", "true");
  else $("playBtn").removeAttribute("aria-busy");
  $("playBtn").disabled = mode === "idle"; // an ad or a podcast can still be paused
  for (const id of ["prevBtn", "nextBtn"]) $(id).disabled = mode !== "track";
  $("scrub").tabIndex = mode === "track" ? 0 : -1;

  const noDevice = state.devices && state.devices.length === 0 && mode === "idle";
  $("libraryBtn").classList.toggle("is-primary", mode === "idle" && !noDevice && state.loaded);

  // hidden only until the first poll: with no device the button still opens the (empty) list
  const dev = $("deviceBtn");
  dev.hidden = !state.device && !state.loaded;
  dev.classList.toggle("is-none", !state.device && !movingTo);
  dev.classList.toggle("is-pending", Boolean(movingTo));
  if (movingTo) dev.setAttribute("aria-busy", "true");
  else dev.removeAttribute("aria-busy");
  dev.querySelector(".device-name").textContent = movingTo ? `Moving to ${movingTo.name}…` : state.device ? state.device.name : "No device";

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
  heart.hidden = !song || !t || !t.id || isLocalFile(t.uri) || libraryDenied;
  heart.disabled = state.saved === null; // unknown until is_saved answers
  heart.classList.toggle("is-on", state.saved === true);
  heart.setAttribute("aria-pressed", String(state.saved === true));
  const heartLabel = state.saved ? "Remove from Liked Songs" : "Save to Liked Songs";
  heart.setAttribute("aria-label", heartLabel);
  heart.title = heartLabel;

  renderVolume();
  renderProgress();
  startFrames();
  syncMedia();
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

/** Play these uris from the first; opts as startPlay. Returns true on success. */
async function playUris(uris, opts = {}) {
  if (!uris.length) return false;
  return startPlay({ uris, trackUri: uris[0] }, { kind: "list", ...opts });
}

// ---------- routing: the in-app player directly, or the Web API ----------

let activeId = null; // the device the last poll showed active, null = none

/**
 * A play/pause/seek/next/prev/volume for deviceId (the device it was made for): local() when that's
 * the in-app player and it's active, else remote(). A local call that finds no engine retries once remotely.
 */
async function routed(deviceId, local, remote) {
  if (isLocal(engine, deviceId, activeId)) {
    try {
      return await local();
    } catch (e) {
      if (!isCode(e, "ENGINE_NOT_READY")) throw e;
    }
  }
  return remote();
}

/**
 * Start src ({contextUri, trackUri?} or {uris, trackUri}) on deviceId. The in-app player loads it
 * itself, active or not (local_load activates it); any other device goes through the Web API.
 */
async function playSource(deviceId, src) {
  if (isEngineDevice(engine, deviceId)) {
    try {
      return await invoke("local_load", { ...src, positionMs: 0, play: true, shuffle: state.shuffle, repeat: state.repeat });
    } catch (e) {
      if (!isCode(e, "ENGINE_NOT_READY")) throw e;
    }
  }
  if (src.contextUri) return invoke("play_context", { deviceId, contextUri: src.contextUri, trackUri: src.trackUri });
  // the Web API play has no start offset here: the list starts at the track
  return invoke("play_on_device", { deviceId, uris: src.uris });
}

/**
 * A play the user started: loaders from the click until a poll shows it playing, the last session
 * written once it lands. origin: the detail view it came from ({kind, id}) or null; row: the clicked
 * row; refused(e): true for an error the caller handles itself (the play then counts as failed).
 */
async function startPlay(src, { kind, origin = null, row = null, refused = null, members = null } = {}) {
  const token = startPending(kind, src.trackUri || null, row);
  let handled = false;
  const sent = await changeTrack(async (id) => {
    const deviceId = await needDevice(id);
    try {
      await playSource(deviceId, src);
    } catch (e) {
      if (!(refused && refused(e))) throw e;
      handled = true; // withDevice would retry it on another device
    }
  });
  const ok = sent && !handled;
  settlePending(token, ok);
  if (ok) writeSession(playSession(accountNow, src, origin, Date.now(), members || (src.contextUri && knownRows.get(src.contextUri)) || null));
  kick();
  return ok;
}

// ---------- pending play: spinner, "Starting…", the clicked row, an 8s timeout ----------

const pending = createPending();
const STARTING = "Starting…";
let pendingTimer = null;
let pendingRow = null; // the clicked row, dimmed with a small spinner

/** A play the user is waiting for (a resume loads quietly: no loaders). */
const playPending = () => {
  const cur = pending.current();
  return Boolean(cur && cur.kind !== "resume");
};

function startPending(kind, trackUri, row = null, needPlaying = true) {
  const token = pending.start(kind, { trackUri, needPlaying });
  clearTimeout(pendingTimer);
  pendingTimer = setTimeout(() => {
    if (!pending.timeout(token)) return;
    if (kind !== "resume") toast("Spotify is slow to respond");
    renderPending();
  }, PENDING_MS);
  if (pendingRow) pendingRow.classList.remove("is-pending");
  pendingRow = row;
  if (row) row.classList.add("is-pending");
  renderPending();
  return token;
}

/** The command for token returned: on success polls may confirm it from now; a failure ends it. */
function settlePending(token, ok) {
  if (ok) pending.landed(token, performance.now());
  else if (pending.cancel(token)) renderPending();
}

/** No play pending any more (confirmed, cancelled, or a logout). */
function clearPending() {
  pending.reset();
  renderPending();
}

function renderPending(render = true) {
  if (!pending.current()) {
    clearTimeout(pendingTimer);
    pendingTimer = null;
    if (pendingRow) pendingRow.classList.remove("is-pending");
    pendingRow = null;
  }
  if (!render || $("stage").hidden) return;
  renderNow();
  renderChrome();
}

// ---------- the last session: written while playing, loaded paused once per launch ----------

let accountP = null; // promise of the /me id for this login session
let accountNow = null; // that id once known, else null
let sessionCache; // the stored last session (undefined = not read yet)

/** The signed-in account's id (scopes the list cache and the last session), or null on failure. */
function accountId() {
  if (!accountP) {
    const p = invoke("me_id").then(
      (id) => {
        accountNow = id || null;
        const last = readSession();
        if (last && accountNow && last.accountId !== accountNow) clearSession(); // another account's
        return accountNow;
      },
      (e) => {
        if (isCode(e, "AUTH_EXPIRED")) expire();
        if (accountP === p) accountP = null; // retry on the next ask
        return null;
      },
    );
    accountP = p;
  }
  return accountP;
}

function readSession() {
  if (sessionCache === undefined) {
    try {
      sessionCache = parseSession(localStorage.getItem(SESSION_KEY));
    } catch {
      sessionCache = null;
    }
  }
  return sessionCache;
}

function writeSession(s) {
  if (!s || !s.accountId) return;
  sessionCache = s;
  try {
    localStorage.setItem(SESSION_KEY, JSON.stringify(s));
  } catch {
    /* kept in memory for this run */
  }
}

function clearSession() {
  sessionCache = null;
  try {
    localStorage.removeItem(SESSION_KEY);
  } catch {
    /* nothing stored */
  }
}

/** Save where the current song is: from polls (throttled), on pause and on quit (force). */
function noteSession(force) {
  const t = state.now;
  if (!t || isLocalFile(t.uri)) return;
  const poll = { accountId: accountNow, trackUri: t.uri, contextUri: state.contextUri, positionMs: progress() };
  writeSession(sessionToSave(readSession(), poll, Date.now(), force));
}

let resumeTried = false; // once per launch
let lastPoll = null; // the last playback_state

/**
 * Once the engine is ready and a poll has completed: load the last session on "The Run", paused.
 * Nothing when something plays, when the user already started a play, or when there is nothing to load.
 */
async function maybeResume() {
  if (resumeTried || !state.loaded || !lastPoll || !engine || engine.state !== "ready" || !engine.device_id) return;
  resumeTried = true;
  const sess = authSession;
  const account = await accountId();
  const runId = engine && engine.device_id;
  if (sess !== authSession || !account || !runId || !lastPoll) return;
  const poll = lastPoll;
  // (a paused session Spotify still shows on "The Run" is loaded too: the restarted player holds nothing)
  if (poll.is_playing || changesPending || pending.current()) return;
  const src = resumeSource(poll, readSession(), account);
  if (!src || isLocalFile(src.trackUri)) return;
  const token = startPending("resume", src.trackUri, null, false);
  let failed = false;
  await changeTrack(() =>
    invoke("local_load", { ...src, play: false, shuffle: state.shuffle, repeat: state.repeat }).catch((e) => {
      if (isCode(e, "AUTH_EXPIRED")) throw e;
      failed = true; // quietly: the app starts idle
    }),
  );
  if (sess !== authSession) return;
  settlePending(token, !failed);
  if (failed) return;
  selectTheRun(runId);
  kick();
}

/** Make d the shown device. A volume burst on the old device goes out now, to that device. */
function showDevice(d) {
  if (volTimer) {
    clearTimeout(volTimer);
    sendVolume();
  }
  state.device = { id: d.id, name: d.name };
  state.volume = d.volume_percent ?? null;
  state.supportsVolume = Boolean(d.supports_volume);
}

/** Show "The Run" as the device, like a pick, without a transfer: the load made it active. */
function selectTheRun(runId) {
  if (state.device && state.device.id === runId) return;
  const d = (state.devices || []).find((x) => x.id === runId);
  showDevice(d || { id: runId, name: "The Run" });
  intents.start("device"); // a poll already in flight must not put the old device back
  intents.finish("device", performance.now());
  renderChrome();
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
async function sendIntent(key, fn, revert, seq = intents.start(key), lag = undefined) {
  const sess = authSession;
  const ok = await withDevice(fn);
  if (sess !== authSession) return false; // logged out meanwhile: the intents were reset
  intents.finish(key, performance.now(), lag);
  if (!ok && intents.latest(key, seq)) {
    revert();
    intents.drop(key);
    renderChrome();
  }
  return ok;
}

/** Resume; when Spotify refuses because the session expired, start the same song where it was. */
async function resumeOrRestart(deviceId) {
  try {
    await invoke("resume", { deviceId });
  } catch (e) {
    const t = state.now;
    if (!t || !t.uri || isLocalFile(t.uri) || !(/\b40[34]\b/.test(String(e)) || isCode(e, "NO_ACTIVE_DEVICE"))) throw e;
    await invoke("resume_at", { deviceId, contextUri: state.contextUri, uri: t.uri, positionMs: Math.round(progress()) });
  }
}

/** Flip play/pause at once in the UI; the command joins the player chain in click order. */
async function togglePlay() {
  if (state.mode === "idle") return;
  listen();
  state.progressMs = progress();
  state.progressAt = performance.now();
  const want = (state.isPlaying = !state.isPlaying);
  const dev = state.device && state.device.id; // the device this click is for
  let token = 0;
  if (want) token = startPending("play", state.now && state.now.uri);
  else {
    clearPending(); // a pause ends any wait for sound
    noteSession(true);
  }
  renderChrome();
  const ok = await sendIntent(
    "play",
    (id) =>
      routed(
        dev,
        () => invoke(want ? "local_play" : "local_pause"),
        async () => (want ? resumeOrRestart(await needDevice(id)) : invoke("pause")),
      ),
    () => (state.isPlaying = !want), // the device still has the state before this click
  );
  if (token) settlePending(token, ok);
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

// ---------- volume: the UI moves at once, one command after 200ms of quiet (30ms on the in-app player) ----------

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
  // the in-app player applies a level at once: a short pause still coalesces a drag
  volTimer = setTimeout(sendVolume, volumeTiming(isLocal(engine, volDevice, activeId)).quietMs);
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
  const { lagMs } = volumeTiming(isLocal(engine, deviceId, activeId));
  sendIntent(
    volKey(deviceId),
    () => routed(deviceId, () => invoke("local_volume", { percent }), () => invoke("set_volume", { percent, deviceId })),
    revert,
    volSeq,
    lagMs,
  );
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
  if (!track || !track.id || isLocalFile(track.uri) || libraryDenied) return;
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
let devicesTimer = null; // refreshes the open menu while "The Run" isn't listed
const DEVICES_REFRESH_MS = 3000;
let runMissingSince = 0; // menu open, engine ready, The Run not listed: since when (0 = not missing)

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
  clearInterval(devicesTimer);
  devicesTimer = setInterval(() => {
    // The Run may be listed any second now; one request at a time, so a slow answer isn't
    // discarded by the next tick's newer generation
    if (!devicesBusy && !thisMacBusy && !(state.devices || []).some(isTheRun)) refreshDevices();
  }, DEVICES_REFRESH_MS);
}

function closeDevices(refocus = false) {
  if (!devicesOpen) return;
  devicesOpen = false;
  clearInterval(devicesTimer);
  devicesTimer = null;
  runMissingSince = 0;
  devicesGen++; // a list still in flight would re-render a closed popover
  $("devicePop").hidden = true;
  $("deviceBtn").setAttribute("aria-expanded", "false");
  if (refocus) $("deviceBtn").focus();
}

let devicesBusy = false;

async function refreshDevices() {
  const gen = ++devicesGen;
  devicesBusy = true;
  try {
    await loadDeviceList(gen);
  } finally {
    devicesBusy = false;
  }
}

async function loadDeviceList(gen) {
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
    .join("") + renderThisMacRow(list);
  $("deviceList").hidden = !$("deviceList").children.length;
  const note = $("devicePop").querySelector(".device-note");
  note.textContent = devicesNote || (state.devices && !list.length ? "Open Spotify on a phone or speaker, or play on this Mac." : "");
  note.hidden = !note.textContent;
  if (focused) $("deviceList").querySelector(`[data-device="${CSS.escape(focused)}"]`)?.focus();
}

/** The in-app player ("The Run") isn't listed yet: offer this Mac with the engine's state. */
function renderThisMacRow(list) {
  const missing = devicesOpen && engine && engine.state === "ready" && state.devices && !list.some(isTheRun);
  if (!missing) runMissingSince = 0;
  else if (!runMissingSince) runMissingSince = performance.now();
  const missingMs = runMissingSince ? performance.now() - runMissingSince : 0;
  const row = thisMacRow(engine, state.devices && list, thisMacBusy, missingMs);
  if (!row) return "";
  return (
    `<button class="device-row" type="button" role="option" data-this-mac="1" tabindex="-1" title="${esc(row.title)}">` +
    `<span class="device-row-dot"></span><span class="device-row-name">This Mac</span>` +
    `<span class="device-row-type">${esc(row.type)}</span></button>`
  );
}

// ---------- the in-app player ("The Run" on this Mac) ----------

let engine = null; // the last engine_status / engine-status payload, null = unknown
let thisMacBusy = ""; // a click on "This Mac" is working: "login" | "connecting" | ""
const ENGINE_WAIT_MS = 20000; // for a starting/reconnecting player to be ready
const THE_RUN_WAIT_MS = 20000; // for a ready player to show up in the device list
const THE_RUN_POLL_MS = 1500;

/** Tauri events; a no-op where there is no event API. Resolves to an unlisten function. */
function listenEvent(name, fn) {
  const ev = window.__TAURI__ && window.__TAURI__.event;
  if (!ev || !ev.listen) return Promise.resolve(() => {});
  return ev.listen(name, (e) => fn(e && e.payload)).catch(() => () => {});
}

function setEngine(st) {
  if (!st || !st.state) return;
  const wasReady = engine && engine.state === "ready";
  engine = st;
  if (st.state === "account_mismatch") clearSession(); // the player is on another account
  if (st.state === "ready") maybeResume();
  if (!devicesOpen) return;
  renderDeviceList();
  // ready now: The Run registers with Spotify, so the open list should show it
  if (st.state === "ready" && !wasReady) refreshDevices();
}

async function refreshEngine() {
  try {
    setEngine(await invoke("engine_status"));
  } catch (e) {
    setEngine({ state: "failed", reason: reason(e) });
  }
}

/** Logged out or in: the player re-checks its account. Not session-tagged: it belongs to no session. */
function restartEngine() {
  window.__TAURI__.core.invoke("engine_restart").catch(() => {});
}

/**
 * The engine state once it isn't starting/reconnecting, or null after ms. Subscribes to engine-status
 * before it reads engine_status, so a change between the two can't be missed.
 */
function waitEngine(ms) {
  return new Promise((resolve) => {
    let over = false;
    let unlisten = null;
    const finish = (st) => {
      if (over) return;
      over = true;
      clearTimeout(timer);
      if (unlisten) unlisten();
      resolve(st);
    };
    const settle = (st) => st && st.state && !CONNECTING.has(st.state) && finish(st);
    const timer = setTimeout(() => finish(null), ms);
    listenEvent("engine-status", settle).then((u) => {
      if (over) return u();
      unlisten = u;
      invoke("engine_status").then(
        (st) => (setEngine(st), settle(st)),
        (e) => finish({ state: "failed", reason: reason(e) }),
      );
    });
  });
}

/** Poll the device list until "The Run" shows up; null after ms. */
async function findTheRun(ms, sess) {
  const until = performance.now() + ms;
  for (;;) {
    const list = await fetchOr("list_devices");
    if (sess !== authSession) return null;
    if (list) {
      setDevices(list);
      const run = list.find(isTheRun);
      if (run) return run;
    }
    if (performance.now() + THE_RUN_POLL_MS > until) return null;
    await new Promise((r) => setTimeout(r, THE_RUN_POLL_MS));
    if (sess !== authSession) return null;
  }
}

function setBusy(busy) {
  thisMacBusy = busy;
  if (devicesOpen) renderDeviceList();
}

/** Get the in-app player ready (logging it in if it must), wait for "The Run", then move playback there. */
async function playOnThisMac() {
  if (thisMacBusy) return;
  const sess = authSession;
  runMissingSince = 0; // a retry gets a fresh 20s before it says "not showing up" again
  setBusy("connecting");
  try {
    const st = await waitEngine(ENGINE_WAIT_MS);
    if (sess !== authSession) return;
    if (!st) return void toast("This Mac is still connecting to Spotify. Try again in a moment.");
    if (NEEDS_LOGIN.has(st.state)) {
      setBusy("login");
      await invoke("engine_login"); // resolved = logged in and ready (no event to wait for)
      if (sess !== authSession) return;
      setEngine({ ...st, state: "ready", reason: undefined });
      refreshEngine(); // its device id
      setBusy("connecting");
    } else if (st.state !== "ready") {
      return void toast(`This Mac isn't available right now${st.reason ? `: ${st.reason}` : ""}`);
    }
    const run = await findTheRun(THE_RUN_WAIT_MS, sess);
    if (sess !== authSession) return;
    if (run) return void pickDevice(run);
    toast("This Mac didn't show up in Spotify. Try again in a moment.");
  } catch (e) {
    if (sess !== authSession) return;
    if (isCode(e, "AUTH_EXPIRED")) return void expire();
    if (isCode(e, "LOGIN_IN_PROGRESS")) return void toast("Finish the player login in your browser");
    toast(`Couldn't log in the player on this Mac: ${reason(e)}`);
  } finally {
    if (sess === authSession) setBusy("");
  }
}

// ---------- OS media controls: Now Playing + media keys ----------

let lastMedia = null; // the payload Now Playing has, null = cleared
let lastMediaAt = 0;

/**
 * Tell Now Playing about a track or play-state change, or a position jump; clear it when idle.
 * force: a local seek, sent even when it moved less than the jump threshold. Never from frames.
 */
function syncMedia(force = false) {
  const next = mediaPayload({ mode: state.mode, now: state.now, isPlaying: state.isPlaying, positionMs: progress() });
  const t = performance.now();
  if (!(force && next) && !mediaChanged(lastMedia, next, t - lastMediaAt)) return;
  lastMedia = next;
  lastMediaAt = t;
  invoke(next ? "media_update" : "media_clear", next || undefined).catch(() => {});
}

function clearMedia() {
  lastMedia = null;
  window.__TAURI__.core.invoke("media_clear").catch(() => {});
}

/** A media key or Now Playing control: the same functions the buttons use, so their guards apply. */
function onMediaCommand(cmd) {
  if ($("stage").hidden) return;
  const act = mediaAction(cmd, state.isPlaying);
  if (act === "toggle") togglePlay();
  else if (act === "next_track" || act === "previous_track") skip(act);
  else if (act && act.seek !== undefined && state.now && canSeek()) {
    seekTo(Math.min(act.seek, state.now.duration_ms || act.seek));
  }
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
  if (row.dataset.thisMac) return void playOnThisMac();
  const d = state.devices && state.devices[Number(row.dataset.i)];
  if (d) pickDevice(d);
}

let movingTo = null; // a device pick on its way: {seq, name}, the chip says "Moving to <name>…"

/** Move playback to d. The chip shows d at once; a failure puts the old device back. */
async function pickDevice(d) {
  closeDevices(true);
  if (state.device && state.device.id === d.id) return;
  const before = state.device;
  showDevice(d);
  const seq = intents.start("device");
  movingTo = { seq, name: d.name };
  renderChrome();
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
  if (movingTo && movingTo.seq === seq) movingTo = null;
  renderChrome();
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
  const dev = state.device && state.device.id; // the device this click is for
  const local = cmd === "next_track" ? "local_next" : "local_prev";
  await changeTrack(() => routed(dev, () => invoke(local), () => invoke(cmd)));
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
  syncMedia(true); // Now Playing moves with the seek
  const positionMs = state.progressMs;
  const dev = state.device && state.device.id; // the device this seek is for
  await withDevice(() =>
    gen === trackGen ? routed(dev, () => invoke("local_seek", { positionMs }), () => invoke("seek", { positionMs })) : null,
  );
  kick();
}

// ---------- overlays: one open at a time ----------

let returnFocus = null;

let overlayRev = 0; // bumped on every overlay view change: a slow Play closes only the view it came from

/** back: returning to a view the user already saw: no entry animation. */
function openOverlay(name, back = false) {
  if (state.overlay === name) return;
  $(name).classList.toggle("is-back", back);
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

const isLocalFile = (uri) => String(uri).startsWith("spotify:local:");

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
  const local = isLocalFile(t.uri);
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
 * plays from that row (play(i, rowElement)). Returns true if the click was on a row or a link.
 */
function onTrackClick(e, tracks, play) {
  const link = e.target.closest(".artist-link");
  if (link) return openArtist(link), true;
  const row = e.target.closest(".row-track");
  if (!row) return false;
  const i = Number(row.dataset.i);
  if (!tracks[i]) return true;
  if (e.target.closest(".row-queue")) addToQueue(tracks[i]);
  else if (e.target.closest(".row-play")) play(i, row);
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
  if (!t || isLocalFile(t.uri)) return;
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
  fitShelves(); // the width may have changed while a detail or a page was on screen
  $("sheetBody").scrollTop = listScroll;
}

function goBack() {
  const prev = navStack.pop();
  if (prev === PAGE_ENTRY) returnToPage();
  else if (prev) openDetail(prev, false);
  else showList();
}

// Library groups: each loads on its own, once per login session. A missing scope (403) hides
// its group without a word; any other failure leaves one line in it and retries on the next open.
const lib = new Map(); // cache key → promise of the invoke result
const knownRows = new Map(); // context uri → its track uris, from detail views loaded this session

// list commands the backend caches on disk, per account
const CACHED_CMDS = new Set(["get_playlists", "get_playlist_tracks", "get_album_tracks", "get_saved_tracks", "get_saved_albums", "get_followed_artists", "get_top"]);

/** A list command; a cached one gets the account, so the backend reads and writes its disk cache. */
const listInvoke = (cmd, args) =>
  CACHED_CMDS.has(cmd) ? accountId().then((account) => invoke(cmd, { ...args, account })) : invoke(cmd, args);

/** The disk-cached copy of a list for this account, or null. Never read before the account is known. */
async function diskGet(key) {
  const account = await accountId();
  if (!account) return null;
  try {
    return await invoke("cache_get", { account, key });
  } catch {
    return null;
  }
}

function libGet(key, cmd, args) {
  let p = lib.get(key);
  if (!p) {
    p = listInvoke(cmd, args);
    lib.set(key, p);
    p.catch((e) => {
      if (!isScopeError(e) && lib.get(key) === p) lib.delete(key);
    });
  }
  return p;
}

function loadGroups() {
  libOpened = true;
  if (!playlists || playlistsStale) loadPlaylists();
  fillLiked();
  fillTop();
  fillAlbums();
  fillFollowing();
  renderMixes();
}

const fillGen = new Map(); // group → its latest fill: an older one's cached copy must not land

/**
 * Show one group's data via render(data); what names it in an error line. key: its list cache key.
 * The first load of a session shows the disk copy at once (or skeleton() while there is none),
 * marked stale, then the fresh data if it differs.
 */
async function fillGroup(group, load, render, what, key = null, skeleton = null) {
  const gen = (fillGen.get(group) || 0) + 1;
  fillGen.set(group, gen);
  const live = () => fillGen.get(group) === gen;
  let fresh = false;
  let shown = null; // the cached copy on screen, as JSON
  if (key && !lib.has(key)) {
    if (skeleton) skeleton();
    group.setAttribute("aria-busy", "true");
    diskGet(key).then((v) => {
      if (fresh || v == null || !live()) return;
      shown = JSON.stringify(v);
      render(v);
    });
  }
  try {
    const data = await load();
    fresh = true;
    if (!live()) return;
    const status = group.querySelector(".status");
    if (status) setEl(status, "");
    if (shown === null || JSON.stringify(data) !== shown) render(data);
  } catch (e) {
    fresh = true;
    if (!live() || overlayFailed(e)) return;
    if (shown !== null && !isScopeError(e)) return; // keep the cached copy
    for (const el of group.querySelectorAll(".is-skeleton")) el.remove();
    group.hidden = isScopeError(e);
    // the Liked Songs row has no status line: its subtitle says it
    setEl(group.querySelector(".status") || group.querySelector(".row-sub"), `Couldn't load ${what} — ${reason(e)}`);
  } finally {
    if (live()) group.removeAttribute("aria-busy");
  }
}

/** A shelf group's skeleton: shown while it has nothing yet. */
const shelfSkeleton = (group) => () => {
  const shelf = group.querySelector(".albums");
  if (shelf.children.length) return;
  group.hidden = false;
  shelf.innerHTML = skeletonTiles(gridCols(shelf));
};

// A Library shelf shows its first rows, as many tiles a row as the width fits; "See all" opens the
// full page. Never a sideways-scrolling strip.
const SHELF_ROWS = { topArtists: 1, albums: 2, following: 1, mixes: 1 };
const SHELF_MAX = 24; // tiles put in a shelf: 2 rows of the widest grid (CSS: 150px tiles, 1200px content)
const SHELVES = {
  topArtists: { title: "Your top artists", list: () => topArtistList, one: "artist", many: "artists" },
  albums: { title: "Albums", list: () => savedAlbums, one: "album", many: "albums" },
  following: { title: "Following", list: () => followed, one: "artist", many: "artists" },
  mixes: { title: "Spotify mixes", list: () => mixList, one: "mix", many: "mixes" },
};
const SHELF_EL = { topArtists: "topArtists", albums: "libAlbums", following: "libFollowing", mixes: "libMixes" };

/** One shelf item's tile: artists round, albums and mixes square. */
function shelfTile(kind, it, i) {
  if (kind === "albums") return tile(it, i, { sub: esc(it.artists) });
  if (kind === "mixes") return tile(it, i, { attrs: ` data-mix="${esc(it.id)}"` });
  return tile({ name: it.name, cover: it.image }, i, { round: true });
}

const shelfGrid = (kind) => {
  const el = $(SHELF_EL[kind]);
  return el.classList.contains("albums") ? el : el.querySelector(".albums");
};

/** The columns of a laid-out grid; 4 while it isn't on screen (fitShelves runs again when it is). */
function gridCols(el) {
  const v = getComputedStyle(el).gridTemplateColumns;
  return v && v !== "none" ? v.trim().split(/\s+/).length : 4;
}

/** Show a shelf's first rows at the current width; "See all" when some are left out. */
function fitShelf(kind) {
  const grid = shelfGrid(kind);
  const limit = gridCols(grid) * SHELF_ROWS[kind];
  [...grid.children].forEach((el, i) => (el.hidden = i >= limit));
  $("libLevel1").querySelector(`[data-see="${kind}"]`).hidden = SHELVES[kind].list().length <= limit;
}

/** The window (or the list coming back) changed the columns: refit every shelf on screen. */
function fitShelves() {
  if (state.overlay !== "library" || $("libLevel1").hidden) return;
  for (const kind of Object.keys(SHELVES)) fitShelf(kind);
}

/** Render a shelf's tiles and fit them to its rows. An open full page of it follows. */
function renderShelf(kind) {
  const list = SHELVES[kind].list();
  shelfGrid(kind).innerHTML = list
    .slice(0, SHELF_MAX)
    .map((it, i) => shelfTile(kind, it, i))
    .join("");
  fitShelf(kind);
  // fresh data for the shelf open on the full page: re-render it only if it changed (keeps focus)
  if (state.overlay === "browse" && page.kind === kind && page.shelfSig !== JSON.stringify(list)) renderPageList();
}

function fillLiked() {
  const row = $("libLiked");
  const sub = row.querySelector(".row-sub");
  row.hidden = false;
  if (!sub.textContent) {
    setEl(sub, "Loading…");
    // the cached Liked Songs list knows the count
    diskGet("liked").then((v) => v && v.total != null && sub.textContent === "Loading…" && setEl(sub, plural(v.total, "song", "songs")));
  }
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
      renderShelf("albums");
    },
    "your albums",
    "albums",
    shelfSkeleton(group),
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
      renderShelf("following");
    },
    "the artists you follow",
    "following",
    shelfSkeleton(group),
  );
}

// ---------- your top: 3 time ranges, each cached ----------

const TOP_TRACKS_SHOWN = 10; // of 20: the rest of the Library stays in reach
let topRange = "short_term";
let topGen = 0; // the latest tab: an older range's answer is dropped
let topTrackList = [];
let topArtistList = [];

// the backend caches a top list under top:<kind>:<range>:<limit>
const TOP_ARTISTS_LIMIT = 20;
const topArtistsKey = (range) => `top:artists:${range}:${TOP_ARTISTS_LIMIT}`;
const topTracksKey = (range) => `top:tracks:${range}:50`;

async function fillTop() {
  const range = topRange;
  const gen = ++topGen;
  const group = $("libTop");
  for (const b of $("topTabs").querySelectorAll("[data-range]")) b.setAttribute("aria-selected", String(b.dataset.range === range));
  const aKey = topArtistsKey(range);
  let fresh = false;
  let shown = null;
  if (!lib.has(aKey) || !lib.has(topTracksKey(range))) {
    group.setAttribute("aria-busy", "true");
    if (!topTrackList.length && !topArtistList.length) {
      group.hidden = false;
      $("topArtists").hidden = false;
      $("topArtists").innerHTML = skeletonTiles(gridCols($("topArtists")));
      $("topTracks").innerHTML = skeletonRows(8);
    }
    Promise.all([diskGet(topTracksKey(range)), diskGet(aKey)]).then(([t, a]) => {
      if (fresh || gen !== topGen || (!t && !a)) return;
      shown = JSON.stringify([t || [], a || []]);
      renderTop(t || [], a || [], []);
    });
  }
  const results = await Promise.allSettled([topTracks(range), libGet(aKey, "get_top", { kind: "artists", range, limit: TOP_ARTISTS_LIMIT })]);
  fresh = true;
  if (gen !== topGen) return;
  group.removeAttribute("aria-busy");
  const failed = results.filter((r) => r.status === "rejected").map((r) => r.reason);
  if (failed.some((e) => isCode(e, "AUTH_EXPIRED"))) return void expire();
  if (shown !== null && failed.length === 2 && !failed.every(isScopeError)) return; // keep the cached copy
  if (failed.length === 2 && failed.every(isScopeError)) return void (group.hidden = true);
  const [tracks, artists] = results.map((r) => (r.status === "fulfilled" && r.value) || []);
  if (!failed.length && JSON.stringify([tracks, artists]) === shown) return;
  renderTop(tracks, artists, failed);
}

function renderTop(tracks, artists, failed) {
  const group = $("libTop");
  topTrackList = tracks.filter((t) => t && t.uri).slice(0, TOP_TRACKS_SHOWN);
  topArtistList = artists.filter((a) => a && a.id);
  group.hidden = false;
  $("topTracks").innerHTML = topTrackList.map((t, i) => trackRow(t, i, { num: false, art: true })).join("");
  $("topArtists").hidden = !topArtistList.length;
  $("topArtistsHead").hidden = !topArtistList.length;
  renderShelf("topArtists"); // on screen first: it fits its tiles to the columns it has
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
  renderShelf("mixes");
}

/** Spotify refuses some of its own mixes: 403, or a 404 that isn't about the device. */
// Rust already tags a device 404 as NO_ACTIVE_DEVICE: any other 403/404 is the mix itself
const mixRefused = (e) => /\b40[34]\b/.test(String(e)) && !isCode(e, "NO_ACTIVE_DEVICE");

async function playMix(src) {
  const rev = overlayRev;
  let refused = false;
  const ok = await startPlay(
    { contextUri: `spotify:playlist:${src.id}` },
    { kind: "mix", refused: (e) => mixRefused(e) && (refused = true) },
  );
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
  knownRows.clear();
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
  for (const el of $("libLevel1").querySelectorAll("[data-see]")) el.hidden = true;
  $("topArtistsHead").hidden = true;
  setEl($("libLiked").querySelector(".row-sub"), "");
}

let playlistsStale = false; // showing the disk copy after a failed refresh: the next open retries

/** The playlists: the disk copy at once (marked stale) or a skeleton, then the fresh list if it differs. */
async function loadPlaylists() {
  if (playlistsLoading) return;
  playlistsLoading = true;
  const list = $("libList");
  let fresh = false;
  let shown = playlists ? JSON.stringify(playlists) : null;
  list.setAttribute("aria-busy", "true");
  if (!playlists) {
    setText("listStatus", "");
    list.innerHTML = skeletonRows(8);
    diskGet("playlists").then((v) => {
      if (fresh || !Array.isArray(v) || playlists) return;
      shown = JSON.stringify(v);
      playlists = v;
      renderPlaylists();
    });
  }
  try {
    const next = (await listInvoke("get_playlists")) || [];
    fresh = true;
    playlistsStale = false;
    if (shown === null || JSON.stringify(next) !== shown) {
      playlists = next;
      renderPlaylists();
    }
  } catch (e) {
    fresh = true;
    if (overlayFailed(e)) return;
    if (shown !== null) playlistsStale = true; // keep the cached copy
    else {
      list.innerHTML = "";
      setText("listStatus", `Couldn't load your playlists — ${reason(e)}`);
    }
  } finally {
    playlistsLoading = false;
    list.removeAttribute("aria-busy");
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
const openMix = (m) => m && openDetail({ kind: "mix", id: m.id, name: m.name, cover: m.cover, sub: "Made by Spotify" });

/**
 * Level 2: {kind, id, name, cover, sub}, kind = playlist, album, liked, mix or artist.
 * push: Back returns to what this replaced (false when Back itself opens it).
 */
async function openDetail(src, push = true) {
  if (state.overlay !== "library") {
    const fromPage = state.overlay === "browse";
    if (fromPage) page.scroll = $("pageBody").scrollTop;
    openOverlay("library");
    $("sheet").focus();
    // from the stage or Search: Back shows the list; from a full page: Back returns there
    navStack = fromPage ? [PAGE_ENTRY] : [];
    if (!curDetail && !fromPage) listScroll = $("sheetBody").scrollTop;
    loadGroups(); // so Back has a list to show
  } else if (push && curDetail) {
    navStack.push(curDetail);
    // the oldest detail goes; the full page under them all stays
    if (navStack.length > NAV_MAX) navStack.splice(navStack[0] === PAGE_ENTRY ? 1 : 0, 1);
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
  const rows = $("detailRows");
  setText("detailStatus", "");
  $("sheetBody").scrollTop = 0;
  if (src.kind === "mix") {
    rows.innerHTML = "";
    rows.removeAttribute("aria-busy");
    return;
  }
  rows.setAttribute("aria-busy", "true");
  if (src.kind === "artist") {
    rows.innerHTML = `<div class="albums is-grid">${skeletonTiles(4)}</div>`;
    return loadArtist(src, gen);
  }
  // the disk copy at once (stale until the fresh list lands), then the fresh list if it differs;
  // the skeleton only when there's no copy, so a cached reopen never flashes it
  const key = detailKey(src);
  let fresh = false;
  let shown = null;
  if (key && !(src.kind === "liked" && lib.has("liked"))) {
    rows.innerHTML = "";
    diskGet(key).then((v) => {
      if (fresh || gen !== state.gen.detail) return;
      if (v == null) return void (rows.innerHTML = skeletonRows(8));
      shown = rowsSig(v);
      showDetail(src, v);
    });
  } else {
    rows.innerHTML = skeletonRows(8);
  }
  let data;
  try {
    if (src.kind === "liked") data = await libGet("liked", "get_saved_tracks");
    else if (src.kind === "album") data = await listInvoke("get_album_tracks", { albumId: src.id });
    else data = await listInvoke("get_playlist_tracks", { playlistId: src.id, snapshotId: src.snapshotId || null });
  } catch (e) {
    fresh = true;
    if (gen !== state.gen.detail) return;
    if (overlayFailed(e)) return;
    rows.removeAttribute("aria-busy");
    if (shown !== null) return; // keep the cached rows
    rows.innerHTML = "";
    setText("detailStatus", `Couldn't load tracks — ${reason(e)}`);
    return;
  }
  fresh = true;
  if (gen !== state.gen.detail) return; // a newer detail (or the list) took over
  rows.removeAttribute("aria-busy");
  if (rowsSig(data) !== shown) showDetail(src, data);
}

/** The list cache key of a detail view, or null when it has none. */
function detailKey(src) {
  if (src.kind === "album") return `album:${src.id}`;
  if (src.kind === "liked") return "liked";
  if (src.kind === "playlist" && src.snapshotId) return `playlist:${src.id}:${src.snapshotId}`;
  return null;
}

const listTracks = (v) => (Array.isArray(v) ? v : (v && v.tracks) || []);

/** What decides a re-render: the uris in order (and Liked Songs' total). */
const rowsSig = (v) => `${listTracks(v).map((t) => t && t.uri).join("|")}#${(v && v.total) || ""}`;

/** Render a detail view's tracks (an array, or Liked Songs' {tracks, total}). */
function showDetail(src, data) {
  const tracks = listTracks(data);
  let total = (data && data.total) || 0;
  detailTracks = (tracks || []).filter((t) => t && t.uri);
  const ctx = originUri(src);
  if (ctx) knownRows.set(ctx, detailTracks.map((t) => t.uri)); // a cover click can trust a track is in it
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
  $("detailPlay").disabled = !detailTracks.some((t) => !isLocalFile(t.uri));
}

/** A detail view as a play's origin: playlists and albums only. */
const originOf = (src) => (src && (src.kind === "playlist" || src.kind === "album") ? { kind: src.kind, id: src.id } : null);

const kindLabel = (k) => (k ? k[0].toUpperCase() + k.slice(1) : "Album");

const TOP_RANGES = ["short_term", "medium_term", "long_term"];

/** Top tracks at Spotify's max of 50, one cache entry shared by the Library group and artist pages. */
const topTracks = (range) => libGet(topTracksKey(range), "get_top", { kind: "tracks", range, limit: 50 });

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
    if (overlayFailed(e)) return;
    $("detailRows").innerHTML = "";
    $("detailRows").removeAttribute("aria-busy");
    setText("detailStatus", `Couldn't load albums — ${reason(e)}`);
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
  $("detailRows").removeAttribute("aria-busy");
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

/** opts: {origin, row} — the detail view it came from and the clicked row (see startPlay). */
async function playFrom(tracks, i, opts = {}) {
  // Spotify lists local files in playlists but rejects them in play requests
  // capped: Liked Songs can hold 1000 rows, and Spotify's limit for one play request is unknown
  const uris = tracks.slice(i).map((t) => t.uri).filter((u) => !isLocalFile(u)).slice(0, PLAY_URIS_MAX);
  const rev = overlayRev;
  if ((await playUris(uris, opts)) && rev === overlayRev) closeOverlay();
}

const playDetailFrom = (i, row = null) => playFrom(detailTracks, i, { origin: originOf(curDetail), row });

// ---------- search: songs + albums, debounced, last request wins ----------

const SEARCH_DEBOUNCE_MS = 250;
const SEARCH_PREVIEW = { track: 5, album: 6 }; // the palette's first songs and one row of albums
let searchTimer = null;
const NO_HITS = { q: "", tracks: [], albums: [] };
let searchHits = NO_HITS;

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
    searchHits = NO_HITS;
    $("searchResults").innerHTML = "";
    $("searchResults").hidden = true;
    $("searchResults").removeAttribute("aria-busy");
    return;
  }
  searchTimer = setTimeout(() => runSearch(q, gen), SEARCH_DEBOUNCE_MS);
}

function searchMessage(text) {
  overlayRev++;
  searchHits = NO_HITS;
  const box = $("searchResults");
  box.innerHTML = `<p class="status">${esc(text)}</p>`;
  box.hidden = false;
}

async function runSearch(q, gen) {
  const box = $("searchResults");
  if (!searchHits.tracks.length && !searchHits.albums.length) {
    // nothing on screen to keep: placeholders until the answer
    box.innerHTML = `<div class="rows">${skeletonRows(8)}</div>`;
    box.hidden = false;
  }
  box.setAttribute("aria-busy", "true");
  let res;
  try {
    res = await invoke("search", { query: q });
  } catch (e) {
    if (gen !== state.gen.search) return;
    box.removeAttribute("aria-busy");
    if (!overlayFailed(e)) searchMessage(`Search failed — ${reason(e)}`);
    return;
  }
  if (gen !== state.gen.search) return;
  box.removeAttribute("aria-busy");
  const tracks = ((res && res.tracks) || []).filter((t) => t && t.uri).slice(0, 10);
  const albums = ((res && res.albums) || []).filter((a) => a && a.id).slice(0, 10);
  if (!tracks.length && !albums.length) return searchMessage(`No songs or albums for "${q}".`);
  searchHits = { q, tracks, albums };

  // a preview: the top songs and one row of albums; "See all" opens the full results page
  const see = (kind, what) =>
    `<button class="see-all" type="button" data-see="${kind}" aria-label="${esc(`See all ${what} for “${q}”`)}">See all</button>`;
  let html = "";
  if (tracks.length) {
    html += `<section class="group"><div class="group-head"><h3 class="group-title">Songs</h3>`;
    if (tracks.length > SEARCH_PREVIEW.track) html += see("track", "songs");
    html += `</div><div class="rows">`;
    html += tracks.slice(0, SEARCH_PREVIEW.track).map((t, i) => trackRow(t, i, { num: false, art: true })).join("");
    html += `</div></section>`;
  }
  if (albums.length) {
    html += `<section class="group"><div class="group-head"><h3 class="group-title">Albums</h3>`;
    if (albums.length > SEARCH_PREVIEW.album) html += see("album", "albums");
    html += `</div><div class="albums">`;
    html += albums.slice(0, SEARCH_PREVIEW.album).map((a, i) => tile(a, i, { sub: esc(a.artists) })).join("");
    html += `</div></section>`;
  }
  overlayRev++; // replaced results are a new view
  box.innerHTML = html;
  box.hidden = false;
  box.scrollTop = 0;
}

function onSearchClick(e) {
  const see = e.target.closest("[data-see]");
  if (see) return openSearchPage(see.dataset.see);
  // a song plays alone: the rest of the results aren't a playlist
  if (onTrackClick(e, searchHits.tracks, (i, row) => playFrom([searchHits.tracks[i]], 0, { row }))) return;
  const a = tileAt(e, searchHits.albums);
  if (a) openAlbum(a);
}

// ---------- the full page: "See all" from Search or a Library shelf ----------

const FIRST_PAGES = 3; // 30 results up front, then 10 per "Load more"
const PAGE_ENTRY = { kind: "page" }; // in the detail Back stack: Back returns to the full page
const RANGE_LABEL = { short_term: "Last 4 weeks", medium_term: "Last 6 months", long_term: "All time" };

const page = {
  from: null, // "search" | "library": where Back goes
  kind: null, // "search", or the shelf: "topArtists" | "albums" | "following" | "mixes"
  query: "",
  tab: "track", // search: "track" | "album"
  lists: {}, // search: tab → {items, next, hasMore, loading, error, started}
  opener: null, // the data-see of the "See all" that opened it: Back puts focus there
  shelfSig: null, // the shelf items on screen, as JSON
  scroll: 0, // the page's scroll when an item on it opened a detail
  returnScroll: 0, // the search results' scroll when the page opened
};

/** A search list; seeded with the palette's first page (Spotify gives 10 a page). */
const newList = (seed) => ({
  items: seed ? seed.items : [],
  next: seed ? PAGE_SIZE : 0,
  hasMore: seed ? seed.full : true,
  loading: false,
  error: null,
  started: false,
});

function openSearchPage(tab) {
  if (!searchHits.q) return;
  Object.assign(page, { from: "search", kind: "search", query: searchHits.q, tab, opener: tab });
  page.returnScroll = $("searchResults").scrollTop;
  // the palette asked for 10 of each: a full 10 means Spotify has more
  page.lists = {
    track: newList({ items: searchHits.tracks, full: searchHits.tracks.length >= PAGE_SIZE }),
    album: newList({ items: searchHits.albums, full: searchHits.albums.length >= PAGE_SIZE }),
  };
  showPage();
}

function openShelfPage(kind) {
  if (!SHELVES[kind]) return;
  Object.assign(page, { from: "library", kind, query: "", opener: kind, lists: {} });
  listScroll = $("sheetBody").scrollTop; // Back puts the Library where it was
  showPage();
}

function showPage() {
  state.gen.page++; // loads of an earlier page must not land here
  openOverlay("browse");
  page.scroll = 0;
  renderPage();
  $("pageBody").scrollTop = 0;
  $("page").focus();
  if (page.kind === "search") ensureLoaded(page.tab);
}

/** Back to the page from a detail opened on it: as it was. Its loads kept landing meanwhile. */
function returnToPage() {
  showList(); // the Library sheet underneath goes back to its list
  openOverlay("browse", true);
  renderPage();
  $("pageBody").scrollTop = page.scroll;
  $("page").focus({ preventScroll: true });
}

/** Back (or Esc): to the search palette with its query and results, or to the Library list. */
function pageBack() {
  state.gen.page++;
  const from = page.from;
  openOverlay(from, true);
  if (from === "library") showList();
  else $("searchResults").scrollTop = page.returnScroll;
  const box = from === "library" ? $("libLevel1") : $("searchResults");
  const see = box.querySelector(`[data-see="${page.opener}"]`);
  const to = see && !see.hidden ? see : from === "library" ? $("sheet") : $("searchInput");
  to.focus({ preventScroll: true });
}

/** What the page lists now: {kind: "track" | "album" | a shelf kind, items, list (search only)}. */
function pageView() {
  if (page.kind !== "search") return { kind: page.kind, items: SHELVES[page.kind].list(), list: null };
  const list = page.lists[page.tab];
  return { kind: page.tab, items: list.items, list };
}

function pageItemHtml(kind, it, i) {
  if (kind === "track") return trackRow(it, i, { num: true, art: true });
  if (kind === "album") return tile(it, i, { sub: esc(it.artists) });
  return shelfTile(kind, it, i);
}

/** Skeletons for pages on their way: a full screen at first, a few under the list after. */
const pageSkeleton = (kind, empty) => (kind === "track" ? skeletonRows(empty ? 10 : 4) : skeletonTiles(empty ? 12 : 6));

function renderPage() {
  overlayRev++; // a new view: a slow Play from the one before must not close it
  const search = page.kind === "search";
  $("pageBack").querySelector(".back-label").textContent = search ? "Search" : "Library";
  $("pageBack").setAttribute("aria-label", search ? "Back to search" : "Back to Library");
  setText("pageKicker", search ? "Search results for" : page.kind === "topArtists" ? "Your top" : "Your library");
  const title = search ? `“${page.query}”` : SHELVES[page.kind].title;
  $("pageTitle").textContent = title;
  $("pageTitle").title = title;
  $("pageTabs").hidden = !search;
  for (const b of $("pageTabs").querySelectorAll("[data-tab]")) {
    const on = b.dataset.tab === page.tab;
    b.setAttribute("aria-selected", String(on));
    b.tabIndex = on ? 0 : -1;
  }
  const list = $("pageList");
  if (search) {
    list.setAttribute("role", "tabpanel");
    list.setAttribute("aria-labelledby", page.tab === "track" ? "pageTabTrack" : "pageTabAlbum");
  } else {
    list.removeAttribute("role");
    list.removeAttribute("aria-labelledby");
  }
  renderPageList();
}

function renderPageList() {
  const { kind, items, list } = pageView();
  page.shelfSig = list ? null : JSON.stringify(items);
  const el = $("pageList");
  el.className = `page-list ${kind === "track" ? "rows" : "albums page-grid"}`;
  el.innerHTML = items.map((it, i) => pageItemHtml(kind, it, i)).join("") + (list && list.loading ? pageSkeleton(kind, !items.length) : "");
  if (list && list.loading && !items.length) el.setAttribute("aria-busy", "true");
  else el.removeAttribute("aria-busy");
  renderPageFoot();
}

/** The line under the list (empty, failed) and the Load more button. */
function renderPageFoot() {
  const { kind, items, list } = pageView();
  const n = items.length;
  const more = $("pageMore");
  if (!list) {
    const shelf = SHELVES[page.kind];
    setText("pageSub", n ? plural(n, shelf.one, shelf.many) + (page.kind === "topArtists" ? ` · ${RANGE_LABEL[topRange]}` : "") : "");
    setText("pageStatus", n ? "" : "Nothing here yet.");
    more.hidden = true;
    return;
  }
  setText("pageSub", "");
  const noun = kind === "track" ? "songs" : "albums";
  let status = "";
  if (list.error) status = n ? `Couldn't load more — ${reason(list.error)}` : `Couldn't load ${noun} — ${reason(list.error)}`;
  else if (!n && !list.loading && !list.hasMore) status = `No ${noun} for “${page.query}”.`;
  setText("pageStatus", status);
  // while the first pages load, the skeletons say it; after that the button does
  more.hidden = !list.hasMore || (list.loading && !n) || (!list.started && !n);
  more.textContent = list.loading ? "Loading…" : list.error ? "Try again" : "Load more";
  if (list.loading) more.setAttribute("aria-disabled", "true");
  else more.removeAttribute("aria-disabled");
}

/** The first pages of a search tab, once per page: 30 results (the palette's 10 count as the first page). */
function ensureLoaded(tab) {
  const list = page.lists[tab];
  if (!list || list.started) return;
  list.started = true;
  loadPages(tab, list.next ? FIRST_PAGES - 1 : FIRST_PAGES);
}

const isShown = (tab, list) => state.overlay === "browse" && page.kind === "search" && page.tab === tab && page.lists[tab] === list;

/** Load n more pages of a search tab, in parallel; they land in order (see foldPages). */
async function loadPages(tab, n) {
  const list = page.lists[tab];
  if (!list || list.loading || !list.hasMore) return;
  const offsets = pageOffsets(list.next, n);
  if (!offsets.length) {
    list.hasMore = false;
    if (isShown(tab, list)) renderPageFoot();
    return;
  }
  const gen = state.gen.page;
  list.loading = true;
  list.error = null;
  if (isShown(tab, list)) {
    $("pageList").insertAdjacentHTML("beforeend", pageSkeleton(tab, !list.items.length));
    if (!list.items.length) $("pageList").setAttribute("aria-busy", "true");
    renderPageFoot();
  }
  const query = page.query;
  const results = await Promise.allSettled(offsets.map((offset) => invoke("search_page", { query, kind: tab, offset })));
  if (gen !== state.gen.page) return; // the page closed (or another opened) meanwhile
  list.loading = false;
  const dead = results.find((r) => r.status === "rejected" && isCode(r.reason, "AUTH_EXPIRED"));
  if (dead) return void overlayFailed(dead.reason);
  const r = foldPages(list.items, offsets, results, tab === "track" ? (t) => t.uri : (a) => a.id);
  const from = list.items.length;
  list.items = list.items.concat(r.added);
  list.next = r.next;
  list.hasMore = r.hasMore;
  list.error = r.error;
  if (!isShown(tab, list)) return; // a detail or the other tab is on screen: rendered when it comes back
  // appended, not re-rendered: focus and a pending row's spinner stay where they are
  const el = $("pageList");
  for (const sk of el.querySelectorAll(".is-skeleton")) sk.remove();
  el.removeAttribute("aria-busy");
  el.insertAdjacentHTML("beforeend", r.added.map((it, k) => pageItemHtml(tab, it, from + k)).join(""));
  renderPageFoot();
}

function loadMore() {
  loadPages(page.tab, 1); // a shelf page has no list for the tab: loadPages returns
}

/** The Load more button scrolled into view: load the next page (not after an error: the button retries). */
function onMoreSeen(entries) {
  const list = page.kind === "search" && page.lists[page.tab];
  if (!list || list.loading || list.error || !list.hasMore || !list.items.length) return;
  if (entries.some((en) => en.isIntersecting)) loadMore();
}

function switchPageTab(tab) {
  if (page.kind !== "search" || tab === page.tab || !page.lists[tab]) return;
  page.tab = tab;
  renderPage();
  $("pageBody").scrollTop = 0;
  ensureLoaded(tab);
}

/** Tabs: a click picks one; arrows move between them (and pick, like the Library's range tabs). */
function onPageTabKey(e) {
  const step = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
  if (!step || !e.target.closest("[data-tab]")) return;
  e.preventDefault();
  const tabs = [...$("pageTabs").querySelectorAll("[data-tab]")];
  const at = tabs.findIndex((b) => b.dataset.tab === page.tab);
  const next = tabs[(at + step + tabs.length) % tabs.length];
  switchPageTab(next.dataset.tab);
  next.focus();
}

function onPageClick(e) {
  const { kind, items } = pageView();
  if (kind === "track") {
    // a song plays alone, as in the palette: search results aren't a playlist
    onTrackClick(e, items, (i, row) => playFrom([items[i]], 0, { row }));
    return;
  }
  const it = tileAt(e, items);
  if (!it) return;
  if (kind === "album" || kind === "albums") openAlbum(it);
  else if (kind === "mixes") openMix(it);
  else openArtistTile(it);
}

// ---------- keyboard: Space = play/pause, Esc = close the overlay ----------

const typing = (t) => t && t.closest && t.closest("input, textarea, select, [contenteditable]");

function onKey(e) {
  // Esc closes the innermost thing: a popover first, then a full page (back), then the overlay
  if (e.key === "Escape" && e.type === "keydown" && (devicesOpen || volumeOpen || state.overlay)) {
    e.preventDefault();
    if (devicesOpen) closeDevices(true);
    else if (volumeOpen) closeVolume(true);
    else if (state.overlay === "browse") pageBack(); // a full page returns to where it came from
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
  refreshEngine();
  accountId(); // the list cache and the last session are per account: ask once, early
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
  for (const id of ["library", "search", "browse"]) {
    $(id).addEventListener("click", (e) => e.target.closest("[data-close]") && closeOverlay());
    $(id).addEventListener("error", onImgError, true);
  }
  $("libBack").addEventListener("click", goBack);
  $("libList").addEventListener("click", (e) => {
    const row = e.target.closest("[data-id]");
    const p = row && playlists && playlists[Number(row.dataset.i)];
    if (p) {
      const sub = plural((p.tracks && p.tracks.total) || 0, "track", "tracks");
      openDetail({ kind: "playlist", id: p.id, name: p.name, cover: pickImage(p.images), sub, snapshotId: p.snapshot_id || null });
    }
  });
  $("libLiked").addEventListener("click", () => openDetail({ kind: "liked", id: "liked", name: "Liked Songs", cover: null, sub: "" }));
  $("topTabs").addEventListener("click", onTopTab);
  $("topTracks").addEventListener("click", (e) => onTrackClick(e, topTrackList, (i, row) => playFrom(topTrackList, i, { row })));
  $("topArtists").addEventListener("click", (e) => openArtistTile(tileAt(e, topArtistList)));
  $("libFollowing").addEventListener("click", (e) => openArtistTile(tileAt(e, followed)));
  $("libAlbums").addEventListener("click", (e) => {
    const a = tileAt(e, savedAlbums);
    if (a) openAlbum(a);
  });
  $("libMixes").addEventListener("click", (e) => openMix(tileAt(e, mixList)));
  $("libLevel1").addEventListener("click", (e) => {
    const see = e.target.closest("[data-see]");
    if (see) openShelfPage(see.dataset.see);
  });
  $("pageBack").addEventListener("click", pageBack);
  $("pageTabs").addEventListener("click", (e) => {
    const b = e.target.closest("[data-tab]");
    if (b) switchPageTab(b.dataset.tab);
  });
  $("pageTabs").addEventListener("keydown", onPageTabKey);
  $("pageList").addEventListener("click", onPageClick);
  $("pageMore").addEventListener("click", () => $("pageMore").getAttribute("aria-disabled") !== "true" && loadMore());
  if (window.IntersectionObserver) {
    new IntersectionObserver(onMoreSeen, { root: $("pageBody"), rootMargin: "0px 0px 240px 0px" }).observe($("pageMore"));
  }
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
  let fitFrame = 0; // a window drag fires resize ~60/s: refit once per frame
  addEventListener("resize", () => {
    cancelAnimationFrame(fitFrame);
    fitFrame = requestAnimationFrame(fitShelves);
  });
  small.addEventListener("change", () => {
    closeVolume(); // the slider popover exists only on narrow screens
    renderVolume();
    if (state.loaded) renderRun();
  });
  $("run").addEventListener("error", onImgError, true);
  $("run").addEventListener("click", onRunClick);
  // quitting mid-song: the next launch starts here
  addEventListener("beforeunload", () => !$("stage").hidden && noteSession(true));
  // dev harness only: the `artist` scenario opens a page by id
  if (window.__mock) window.__openArtist = (id) => openDetail({ kind: "artist", id, name: "", cover: null, sub: "Artist" });
  document.addEventListener("visibilitychange", () => {
    if ($("stage").hidden) return;
    // hidden keeps polling, slower (pollDelay); visible again restarts with a fresh poll
    if (!document.hidden) startPolling();
  });
  listenEvent("engine-status", setEngine);
  listenEvent("media-command", onMediaCommand);

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
