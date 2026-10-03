// Needle — stage UI: boot, sequential poll loop, the run of covers, transport.
import { fmtTime, esc } from "./lib/format.js";
import { FALLBACK } from "./lib/color.js";
import { extractGlow, glowVars, fallbackVars } from "./lib/glow.js";
import { buildRun, mergeHistory, measure, flip, coverTarget } from "./lib/timeline.js";
import { panelRows } from "./lib/playlist.js";
import { SETTINGS_KEY, parseSettings, isQuality } from "./lib/settings.js";
import { favoritesBy } from "./lib/favorites.js";
import { createIntents, nextRepeat, stepVolume } from "./lib/transport.js";
import { noteMixes } from "./lib/mixes.js";
import { CONNECTING, NEEDS_LOGIN, HERE, isHere, deviceLabel, thisMacRow, preferredDevice } from "./lib/engine.js";
import { mediaAction, mediaChanged, mediaPayload } from "./lib/media.js";
import { GIVE_UP_FAILURES, HIDDEN_POLL_MS, gaveUp, pollDelay, pollMode, modeReason, sanityDue, listDue } from "./lib/poll.js";
import { isWebOnly, rateLimitedSecs, rateLimitedError, quotaNotice, quotaStatus, waitText } from "./lib/quota.js";
import { isEngineDevice, isLocal, volumeTiming } from "./lib/route.js";
import { originUri, offsettable } from "./lib/source.js";
import { rubberBand, rubberRaw, WHEEL_SCALE } from "./lib/pan.js";
import { PENDING_MS, createPending } from "./lib/pending.js";
import { skeletonRows, skeletonTiles } from "./lib/skeleton.js";
import { ICONS } from "./lib/icons.js";
import { PAGE_SIZE, pageOffsets, foldPages } from "./lib/paging.js";
import { parseLink, looksLikeLink } from "./lib/links.js";
import { mcpStatusLine, MCP_COPY } from "./lib/mcp.js";

// Every call belongs to a login session. A result or error from an older session
// (still in flight across a logout) never settles, so it can't touch the new one.
let authSession = 0;
const STALE = new Promise(() => {});
// While Spotify rate-limits the app, a command with no source but the Web API fails here without
// reaching Rust (quota.rs would refuse it too, without a request): RATE_LIMITED:<secs>: …
// The rest go to Rust, which tries Spotify's internal API first.
function invoke(cmd, args) {
  const sess = authSession;
  if (isWebOnly(cmd) && blockedMs() > 0) return blockedReject(cmd, sess);
  return window.__TAURI__.core.invoke(cmd, args).then(
    (v) => (sess === authSession ? v : STALE),
    (e) => {
      if (sess !== authSession) return STALE;
      noteRateLimited(e);
      return Promise.reject(e);
    },
  );
}
const $ = (id) => document.getElementById(id);

const HOLD_MS = 1500; // keep a local seek position this long against polls that may lag
const PLAY_LAG_MS = 500; // Spotify can report the old play state this long after a command lands
// played_at is when a play ended, the same moment we see a track leave: closer than this = same play
const SESSION_MATCH_MS = 2 * 60 * 1000;
const PLAYED_MS = 30 * 1000; // Spotify counts a play after 30s; a track skipped sooner isn't history
const small = matchMedia("(max-width: 899px)");
const RUN_LIMITS = { maxPast: 12, maxNext: 20 }; // more than fit: a pan shows what played and more of the queue

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
/** A line in the app log file (<app dir>/logs/needle.log). Never throws. */
const applog = (level, msg) => invoke("app_log", { level, msg }).catch(() => {});
const reason = (e) =>
  rateLimitedSecs(e) ? `Spotify limits this for ${waitText(rateLimitedSecs(e))}` : String(e).replace(/^[A-Z_]+:\s*/, "").slice(0, 80) || "unknown error";

/** Run a player command and log one line: what, the path (local or remote, the device), ok or the error. */
async function logged(what, fn) {
  try {
    const v = await fn();
    applog("info", `${what}: ok`);
    return v;
  } catch (e) {
    applog("warn", `${what}: ${e}`);
    throw e;
  }
}

// ---------- the store: Rust's state.json, read once at startup, written through ----------

let stored = {}; // every stored key → value (store_all), kept in step with each write

/** Read the whole store once, before anything reads a key. A failure leaves it empty: the defaults. */
async function loadStore() {
  try {
    const all = await window.__TAURI__.core.invoke("store_all");
    stored = all && typeof all === "object" ? all : {};
  } catch (e) {
    stored = {};
    applog("warn", `store_all failed: ${e}`);
  }
}

const storeGet = (key) => (Object.prototype.hasOwnProperty.call(stored, key) ? stored[key] : null);

/** Store value under key (null removes it): in memory now, on disk in the background. Not session-tagged. */
function storeSet(key, value) {
  if (value == null) delete stored[key];
  else stored[key] = value;
  window.__TAURI__.core.invoke("store_set", { key, value: value ?? null }).catch((e) => applog("warn", `store_set ${key} failed: ${e}`));
}

// ---------- toast ----------

let toastTimer = null;
function toast(msg) {
  const el = $("toast");
  el.textContent = msg;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 3200);
}

// ---------- Spotify's rate limit on the Web API (see lib/quota.js, Rust quota.rs) ----------

let blockedUntil = 0; // performance.now() when the Web API block ends; 0 = open
const quotaToasted = new Set(); // the kinds ("remote", "library") whose notice this run already showed
// disk-cache reads in flight: a blocked call fails only after them, so a cached copy lands before the error
const diskPending = new Set();

const blockedMs = () => (blockedUntil ? Math.max(0, blockedUntil - performance.now()) : 0);

/** The RATE_LIMITED rejection of a blocked Web API command, after the disk reads already asked for. */
function blockedReject(cmd, sess) {
  const err = rateLimitedError(blockedMs() / 1000);
  // me_id: the disk reads wait for the account, so it must not wait for them
  const wait = cmd === "me_id" ? Promise.resolve() : Promise.allSettled([...diskPending]);
  return wait.then(() => (sess === authSession ? Promise.reject(err) : STALE));
}

/** A RATE_LIMITED error from Rust: block every Web API call from here for that long. */
function noteRateLimited(e) {
  const secs = rateLimitedSecs(e);
  if (!secs) return;
  const was = blockedMs() > 0;
  blockedUntil = performance.now() + secs * 1000;
  if (was) return;
  applog("warn", `Web API rate-limited for ${secs} s: no Web-API-only calls until then; playback here and the internal API keep working`);
  renderQuota();
}

/**
 * An action failed because it needs the blocked Web API: the notice, once per run for each kind
 * ("remote": another device; "library": a library read or write whose internal source failed too).
 * True when e is that error, so the caller shows nothing else.
 */
function limitedToast(kind, e) {
  const secs = rateLimitedSecs(e);
  if (!secs) return false;
  if (!quotaToasted.has(kind)) {
    quotaToasted.add(kind);
    toast(quotaNotice(kind, secs));
  }
  return true;
}

/** reason(e) for a library status line; a rate limit also gets its once-a-run notice. */
const libReason = (e) => (limitedToast("library", e), reason(e));

/** The block ran out: allowed again. Called every loop tick. */
function syncQuota() {
  if (blockedUntil && blockedMs() === 0) {
    blockedUntil = 0;
    applog("info", "Web API block over: polling allowed again");
  }
  renderQuota();
}

/** The quiet line in Settings while blocked. Nothing else shows the block until an action needs it. */
function renderQuota() {
  const ms = blockedMs();
  const text = ms > 0 ? quotaStatus(ms / 1000) : "";
  const el = $("quotaStatus");
  if (el.textContent !== text) el.textContent = text;
  el.hidden = !text;
}

/** Rust's block at startup (it survives a relaunch: state.json `apiBlockedUntil`). */
async function loadQuota() {
  try {
    const st = await invoke("api_status");
    if (st && st.blockedForSecs > 0) noteRateLimited(rateLimitedError(st.blockedForSecs));
  } catch {
    /* unknown: the first Web API call tells */
  }
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
  lastSrc = null;
  seenDevice = null;
  restoring = null;
  clearTimeout(restoreTimer);
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
  endSkip();
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
  listScroll = 0;
  $("libList").innerHTML = "";
  $("runTrack").replaceChildren();
  runNowUri = null;
  resetPan();
  lastList = null;
  quality = null;
  qualityBusy = 0;
  restartHoldUntil = 0;
  if (loggedOut) resetDockArt(); // the next account's songs set it again
  Object.assign(state, {
    mode: "idle", now: null, device: null, devices: null, queue: [], recent: [], session: [], historyOk: false, error: null,
    shuffle: false, repeat: "off", volume: null, supportsVolume: false, contextUri: null, saved: null,
  });
  closeDevices();
  closeVolume();
  closePanel();
  closeSettings();
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
let failures = 0; // polls failed in a row: retried quietly until gaveUp()
let pollEpoch = 0; // bumped on every start/stop: a poll from an older epoch must not touch state
let seenDevice = null; // the active device the last poll saw (for the log: switches only)

function startPolling() {
  pollEpoch++;
  clearTimeout(seekTimer); // state.now is reset below: no seek may outlive it
  trackGen++;
  polling = true;
  inFlight = false;
  pollAgain = false;
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
    if (gaveUp(failures)) applog("info", `poll: back after ${failures} failures`);
    failures = 0;
  } catch (e) {
    if (epoch !== pollEpoch) return; // stale: the session it belonged to is gone
    inFlight = false;
    if (isCode(e, "AUTH_EXPIRED")) return expire();
    // rate-limited: not a failure; the next tick runs blocked (no requests) and shows the notice
    if (!isCode(e, "RATE_LIMITED")) {
      failures++;
      state.error = reason(e);
      if (failures === GIVE_UP_FAILURES) applog("warn", `poll: ${failures} failures in a row, last: ${e}`);
      // a blip retries quietly (the last view stays, or a loader before the first load); only a
      // run of failures is an error worth showing
      if (failures === GIVE_UP_FAILURES && state.loaded) toast("Can't reach Spotify. Retrying.");
    }
    renderNow();
  }
  inFlight = false;
  if (!polling) return;
  // events: a local re-render each second; another device: 5 s (30 s hidden); blocked: no requests
  let delay = pollDelay({ hidden: document.hidden, mode: loopMode(), failures });
  if (pollAgain) {
    pollAgain = false;
    delay = 0;
  }
  schedule(delay);
}

// ---------- where the state comes from: player-state events (this Mac) or the Web API ----------

let local = null; // the last player-state payload from Rust (this Mac's player): {s, at}
let distrust = false; // a Web API check showed another device active since that payload
let lastSanityAt = 0; // events mode: the last playback_state check
let shownMode = null; // the loop's mode the log last named

/** Rust's player-state: this Mac's player changed (track, play/pause, seek, volume, modes, active). */
function onPlayerState(p) {
  if (!p || typeof p !== "object") return;
  const wasActive = Boolean(local && local.s.engine_active);
  local = { s: p, at: performance.now() };
  // this Mac just became the active device (picked here, or from a phone): trust it again
  if (p.engine_active && !wasActive) distrust = false;
  if (!$("stage").hidden) kick();
}

/** The first paint: this Mac's state before its next event. */
async function loadLocal() {
  try {
    const p = await invoke("local_state");
    if (p && !local) onPlayerState(p);
  } catch {
    /* an older backend without it: polls only */
  }
}

const loopMode = () => pollMode({ local: local && local.s, distrust, blockedMs: blockedMs() });

/** One log line when the loop switches between events, polling and blocked. */
function noteMode(mode) {
  if (mode === shownMode) return;
  applog("info", `state source: ${shownMode || "start"} → ${mode} (${modeReason({ mode, blockedMs: blockedMs(), distrust })})`);
  shownMode = mode;
}

/** The last player-state, its position moved on by the time since it came. */
function localSnapshot() {
  const s = { ...local.s };
  if (s.is_playing && s.track) {
    const p = (s.progress_ms || 0) + (performance.now() - local.at);
    s.progress_ms = s.track.duration_ms ? Math.min(p, s.track.duration_ms) : p;
  }
  return s;
}

/** Up next from the player-state (this Mac's own queue), or null when it doesn't carry one. */
const queueOf = (s) => (s && Array.isArray(s.queue) ? s.queue.filter((t) => t && t.uri) : null);

/**
 * The state for this tick. Events: the last player-state (no request), and once a minute a
 * playback_state to catch a missed hand-over. Poll: playback_state. Blocked: null (no request).
 */
async function readState(epoch, mode, startedAt) {
  // blocked: only this Mac's own state can still be read (a hand-over to another device shows as idle)
  if (mode === "blocked") return local && state.device && engine && state.device.id === engine.device_id ? localSnapshot() : null;
  if (mode === "poll") {
    const s = await invoke("playback_state");
    // the Web API shows this Mac active again: its events are the source from here
    if (distrust && s && s.active && engine && s.device_id && s.device_id === engine.device_id) distrust = false;
    return s;
  }
  if (sanityDue({ lastAt: lastSanityAt, now: startedAt, blockedMs: blockedMs() })) {
    lastSanityAt = startedAt;
    let remote = null;
    try {
      remote = await invoke("playback_state");
    } catch (e) {
      if (isCode(e, "AUTH_EXPIRED")) throw e;
    }
    if (epoch !== pollEpoch) return null;
    const here = engine && engine.device_id;
    if (remote && remote.active && here && remote.device_id && remote.device_id !== here) {
      distrust = true; // another device took over and no event said so: poll from here
      noteMode(loopMode());
      return remote;
    }
  }
  return localSnapshot();
}

// a list (queue, recently played, devices) waits this long between fetches; a skipped need is kept
const listAt = { get_queue: 0, get_recently_played: 0, list_devices: 0 };
const listWant = { get_queue: false, get_recently_played: false, list_devices: false };

/**
 * A list from Spotify when it's due: the result (null on failure), or undefined when skipped
 * (not needed, fetched under 30 s ago, or blocked). A skipped need is fetched once it's due.
 */
function fetchList(cmd, { need = false, force = false } = {}) {
  if (need) listWant[cmd] = true;
  const now = performance.now();
  if (!listDue({ lastAt: listAt[cmd], now, need: listWant[cmd], force, blockedMs: blockedMs() })) return undefined;
  listAt[cmd] = now;
  listWant[cmd] = false;
  return fetchOr(cmd).then((v) => {
    if (v === null) listWant[cmd] = true; // failed: again once it's due
    return v;
  });
}

async function refresh(epoch) {
  const startedAt = performance.now();
  syncQuota();
  const source = loopMode();
  noteMode(source);
  const s = await readState(epoch, source, startedAt);
  if (epoch !== pollEpoch) return; // a newer session took over while this one waited
  if (s === null) return blockedTick();
  // the in-app player restarts for a quality change and plays nothing for a moment: keep the song on screen
  if (restartHoldUntil > performance.now() && !(s && s.active)) return;
  listen(); // close the old "now"'s listening time before this poll overwrites it
  const active = Boolean(s && s.active);
  const track = active && s.track && s.track.uri ? s.track : null;
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
  if (changed) {
    const where = active ? devName(s.device_id) : "no device";
    applog("info", `now: ${(state.now && state.now.uri) || "nothing"} → ${track ? `${track.uri} "${track.name}"` : mode} from ${state.contextUri || "no context"} on ${where}, ${active && s.is_playing ? "playing" : "paused"}`);
  }
  const deviceNow = active ? s.device_id || null : null;
  if (deviceNow !== seenDevice) {
    applog("info", `device: ${devName(seenDevice)} → ${devName(deviceNow)}`);
    seenDevice = deviceNow;
  }
  if (changesPending === 0 && settleAfter && startedAt >= settleAfter) settleAfter = 0;
  if (changed && state.now) observe(state.now, state.listenedMs);
  if (changed) state.listenedMs = 0;
  state.now = track;
  if (changed) checkSaved(track);
  if (skipWait && (changed || (skipWait.landedAt && startedAt >= skipWait.landedAt + SKIP_SETTLE_MS))) endSkip();
  // a play the user started is confirmed by a poll that began after it landed
  if (pending.onPoll({ isPlaying: Boolean(active && s.is_playing), trackUri: track && track.uri, at: startedAt })) clearPending();
  if (restoring && (track || mode === "other")) endRestoring(track && track.uri === restoring.trackUri ? "" : `replaced by ${track ? track.uri : "an ad or a podcast"}`);

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
      syncDockArt();
    }
    // a song needs its queue (this Mac's comes with its state); with no song, the device list says
    // who could play. Each list at most every 30 s: a skipped one is fetched once it's due.
    const own = queueOf(s);
    const [queue, recent, devices] = await Promise.all([
      track ? own || fetchList("get_queue", { need: true, force: queueDirty }) : [],
      fetchList("get_recently_played", { need: true }),
      track ? undefined : fetchList("list_devices", { need: true }),
    ]);
    if (epoch !== pollEpoch) return;
    if (queue) state.queue = queue;
    if (queue) queueDirty = false;
    if (recent) state.recent = recent;
    if (recent) noteContexts([state.contextUri, ...recent.map((r) => r.context_uri)]);
    if (recent !== undefined) state.historyOk = Boolean(recent);
    if (devices) setDevices(devices, startedAt);
    state.loaded = true;
    renderNow();
    renderRun();
  } else if (mode !== "other") {
    // between changes only the queue (song) or the device list (idle) can move: this Mac's queue
    // comes with its state; a remote queue only while the panel shows it (or after an add), the
    // device list only while its menu is open; each at most every 30 s
    const own = track ? queueOf(s) : null;
    const force = queueDirty;
    if (own) queueDirty = false;
    const [fresh, recent] = await Promise.all([
      own || (track ? fetchList("get_queue", { need: panelOpen, force }) : fetchList("list_devices", { need: devicesOpen })),
      fetchList("get_recently_played", { need: !state.historyOk }),
    ]);
    if (epoch !== pollEpoch) return;
    if (track && fresh) queueDirty = false;
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

/**
 * Blocked, and nothing of this Mac's to show: no request. The last view stays; with nothing
 * known, this Mac is the device, so a play from the Library (its disk copy) still starts here.
 */
function blockedTick() {
  const here = engine && engine.state === "ready" && engine.device_id;
  if (!state.device && here && state.mode === "idle") state.device = { id: engine.device_id, name: "This Mac" };
  state.loaded = true;
  renderNow();
  renderChrome();
}

const LISTEN_STEP_CAP_MS = HIDDEN_POLL_MS + 1000; // a longer step is a stall, not listening

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
    const d = preferredDevice(list, engine);
    state.device = d ? { id: d.id, name: d.name } : null;
  }
  if (changed && devicesOpen) renderDeviceList();
  return changed;
}

/** Fetch devices for a transport command: this Mac first (see preferredDevice), or null. */
async function discover() {
  const list = await fetchOr("list_devices");
  // rate-limited: no device list, but this Mac's player needs none
  if (!list && blockedMs() > 0 && engine && engine.state === "ready" && engine.device_id) return { id: engine.device_id, name: "This Mac" };
  if (!list) return null;
  setDevices(list);
  const d = preferredDevice(list, engine);
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
    (isLocalFile(t.uri) ? "" : `<button class="cover-play" type="button" aria-label="${esc(`Play ${t.name}`)}">${ICONS.play}</button>`) +
    `<figcaption><span class="ct">${esc(t.name)}</span><span class="ca">${esc(t.artists)}</span></figcaption>`;
  return el;
}


let runItems = new Map(); // the covers on screen: key → buildRun item
let runNowUri = null; // the song the run was last built around: a new one returns a pan to rest

function renderRun() {
  const track = $("runTrack");
  const prev = measure(track);
  // while a play loads, its preview is the big cover and the old queue is gone
  const loading = isLoading();
  const all = buildRun({ history: history(), now: shownTrack(), queue: loading ? [] : state.queue }, RUN_LIMITS);
  // the cover row off in settings: the current cover alone
  const items = soloRun() ? all.filter((it) => it.role === "now") : all;
  runItems = new Map(items.map((it) => [it.key, it]));

  const old = new Map([...track.querySelectorAll(".cover")].map((el) => [el.dataset.key, el]));
  const els = [];
  for (const it of items) {
    const el = old.get(it.key) || makeCover(it);
    el.dataset.role = it.role;
    el.dataset.offset = String(it.offset);
    els.push(el);
  }
  if (!shownTrack()) els.splice(items.filter((i) => i.role === "past").length, 0, slot);

  track.replaceChildren(...els);
  // another song: back to rest (FLIP animates the covers there); the same song keeps a pan
  const nowUri = state.now ? state.now.uri : null;
  if (nowUri !== runNowUri) resetPan();
  runNowUri = nowUri;
  placeRun();
  flip(track, prev);
  if (panelOpen) renderPanel();
}

// ---------- the run's pan: drag or swipe sideways, rubber band past the ends, back to rest after a pause ----------

const PAN_RETURN_MS = 2500; // no input this long: the run slides back to rest
const DRAG_SLOP_PX = 6; // a press that moves less is a click; the drag starts from there (no jump)
const WHEEL_END_MS = 140; // no wheel event this long: the swipe (and the trackpad's own momentum) ended
let restX = 0; // the track's offset that puts the current cover (or the empty slot) at its rest place
let pan = 0; // px from rest, as shown (past an end: rubber-banded); > 0 shows what played
let panRaw = 0; // the input behind pan: rubberBand(panRaw) = pan
let panTimer = null;
let wheelTimer = null;
let drag = null; // a press on the run: {id, x0, raw0, moved, lastX, lastT, v}
let dragged = false; // the last press panned: its click must not play a cover
let glide = 0; // the inertia frame after a release

const reducedMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
/** The cover row is off in settings: the current cover alone, centred, no pan. */
const soloRun = () => !getSettings().coverRow;

/** The left content edge, px (the gutter). */
const gutterPx = () => parseFloat(getComputedStyle($("run")).getPropertyValue("--gutter")) || 48;

/** How far the run may pan: the first played cover to the edge, the last next cover into view. */
function panBounds() {
  const track = $("runTrack");
  const anchor = track.querySelector('[data-role="now"], .slot');
  const first = track.firstElementChild;
  const last = track.lastElementChild;
  if (!anchor || !first || soloRun()) return { min: 0, max: 0 };
  const max = Math.max(0, anchor.offsetLeft - first.offsetLeft);
  const end = restX + last.offsetLeft + last.offsetWidth; // the last cover's right edge at rest
  const min = Math.min(0, $("run").clientWidth * 0.72 - end);
  return { min, max };
}

/**
 * Put the track at rest + rubberBand(raw). motion: "return" slides back to rest, "spring" snaps
 * back from past an end, null jumps (a drag or a swipe follows the input).
 */
function setPan(raw, motion = null) {
  const { min, max } = panBounds();
  panRaw = raw;
  pan = rubberBand(raw, min, max);
  const track = $("runTrack");
  track.classList.toggle("is-returning", motion === "return");
  track.classList.toggle("is-springing", motion === "spring");
  track.style.transform = `translateX(${restX + pan}px)`;
}

/** Past an end: the nearest end, else null. */
function panEnd() {
  const { min, max } = panBounds();
  return panRaw > max ? max : panRaw < min ? min : null;
}

/** Input stopped: spring back from past an end, then wait for the return to rest. */
function settlePan() {
  const end = panEnd();
  if (end !== null) setPan(end, "spring");
  holdPan();
}

/** Measure the rest offset (the window or the covers changed) and apply it with the current pan, inside the limits. */
function placeRun() {
  const track = $("runTrack");
  const anchor = track.querySelector('[data-role="now"], .slot');
  const at = anchor ? anchor.offsetLeft : 0;
  // solo: the cover centred in the stage; else at the left content edge
  restX = soloRun() && anchor ? ($("run").clientWidth - anchor.offsetWidth) / 2 - at : gutterPx() - at;
  const { min, max } = panBounds();
  setPan(Math.min(max, Math.max(min, pan)));
}

function resetPan() {
  cancelAnimationFrame(glide);
  clearTimeout(panTimer);
  clearTimeout(wheelTimer);
  drag = null;
  pan = 0;
  panRaw = 0;
  $("run").classList.remove("is-dragging");
}

/** Slide back to rest once input has stopped for PAN_RETURN_MS. */
function holdPan() {
  clearTimeout(panTimer);
  if (pan) panTimer = setTimeout(() => setPan(0, "return"), PAN_RETURN_MS);
}

/** Where the track is on screen now, as an input offset (mid-slide it isn't where `pan` says). */
function shownRaw() {
  const m = new DOMMatrixReadOnly(getComputedStyle($("runTrack")).transform);
  const { min, max } = panBounds();
  return rubberRaw(m.m41 - restX, min, max);
}

/** Input caught the track mid-slide: hold it where it is. */
function catchPan() {
  cancelAnimationFrame(glide);
  clearTimeout(panTimer);
  const track = $("runTrack");
  if (track.classList.contains("is-returning") || track.classList.contains("is-springing")) setPan(shownRaw());
}

function runDown(ev) {
  if (ev.button !== 0 || soloRun()) return;
  catchPan();
  dragged = false;
  drag = { id: ev.pointerId, x0: ev.clientX, raw0: panRaw, moved: false, lastX: ev.clientX, lastT: ev.timeStamp, v: 0 };
}

function runMove(ev) {
  if (!drag || ev.pointerId !== drag.id) return;
  if (!drag.moved) {
    const dx = ev.clientX - drag.x0;
    if (Math.abs(dx) < DRAG_SLOP_PX) return;
    drag.moved = true;
    drag.x0 += Math.sign(dx) * DRAG_SLOP_PX; // 1:1 from the dead zone's edge: the row doesn't jump by the slop
    $("run").setPointerCapture(ev.pointerId);
    $("run").classList.add("is-dragging");
  }
  const dt = ev.timeStamp - drag.lastT;
  if (dt > 0) drag.v = (ev.clientX - drag.lastX) / dt; // px/ms, for the glide after release
  drag.lastX = ev.clientX;
  drag.lastT = ev.timeStamp;
  setPan(drag.raw0 + ev.clientX - drag.x0);
}

function runUp(ev) {
  if (!drag || ev.pointerId !== drag.id) return;
  const d = drag;
  drag = null;
  $("run").classList.remove("is-dragging");
  if (!d.moved) return;
  dragged = true;
  // a quick flick keeps going for a short moment; a held stop or a release past an end doesn't
  let v = ev.timeStamp - d.lastT < 80 ? d.v * 0.8 : 0;
  if (Math.abs(v) < 0.2 || panEnd() !== null || reducedMotion()) return void settlePan();
  let t = performance.now();
  const step = (now) => {
    const dt = Math.min(32, now - t);
    t = now;
    setPan(panRaw + v * dt);
    // past an end the row brakes hard, then springs back
    v *= Math.pow(panEnd() === null ? 0.92 : 0.6, dt / 16);
    if (Math.abs(v) > 0.03) glide = requestAnimationFrame(step);
    else settlePan();
  };
  glide = requestAnimationFrame(step);
}

/** A trackpad swipe sideways (or Shift + wheel) pans, scaled down; a plain vertical wheel is left alone. */
function runWheel(ev) {
  if (soloRun()) return;
  const sideways = Math.abs(ev.deltaX) > Math.abs(ev.deltaY);
  const dx = sideways ? ev.deltaX : ev.shiftKey ? ev.deltaY : 0;
  if (!dx) return;
  ev.preventDefault();
  catchPan();
  setPan(panRaw - dx * WHEEL_SCALE * (ev.deltaMode === 1 ? 16 : 1));
  clearTimeout(wheelTimer);
  wheelTimer = setTimeout(settlePan, WHEEL_END_MS);
}

/** A play button on a past or next cover: jump to that song (see coverTarget). Not after a drag. */
function onRunClick(e) {
  if (dragged) {
    dragged = false;
    return;
  }
  const btn = e.target.closest(".cover-play");
  const cover = btn && btn.closest(".cover");
  if (cover) playCover(runItems.get(cover.dataset.key));
}

/** Play a run item ({role, offset, track}) from a cover or a panel row (row: the clicked row, for its spinner). */
async function playCover(item, row = null) {
  if (!item || item.role === "now" || isLocalFile(item.track.uri)) return;
  const last = lastSrc;
  // Spotify can report no context for a load we started (a slow state update): the last one is then the context
  const ctx = state.contextUri || (last && last.contextUri) || null;
  const sameAsLast = Boolean(last && ctx && last.contextUri === ctx);
  // the last list only describes what plays when nothing else is named: another client may
  // have started a different playlist since
  const lastFits = !state.contextUri || sameAsLast;
  const target = coverTarget(item, {
    contextUri: ctx,
    // what's known to be in the playing context: its loaded rows, or the members the last play knew
    members: (ctx && knownRows.get(ctx)) || (sameAsLast && last.uris) || null,
    listUris: lastFits && last ? last.uris : null,
    nowUri: state.now && state.now.uri,
    nextUris: state.queue.map((t) => t.uri), // the next covers are its first ones
    historyContext: item.role === "past" ? (history().find((h) => h.track && h.track.uri === item.track.uri) || {}).context_uri || null : null,
  });
  if (!target) return;
  if (target.uris) target.uris = target.uris.filter((u) => !isLocalFile(u));
  // a jump inside the playing playlist/album keeps its full member list, so the next jump still
  // knows every member (not just the visible covers); never sent with a context: Spirc takes one source
  const sameSource = target.contextUri ? target.contextUri === ctx && sameAsLast : false;
  await startPlay(target, { kind: "cover", row, members: sameSource ? last.uris : null });
}

// ---------- now block, chrome, progress ----------

function setText(id, text) {
  setEl($(id), text);
}

function setEl(el, text) {
  el.textContent = text || "";
  el.hidden = !text;
}

function setHtml(el, html) {
  el.innerHTML = html || "";
  el.hidden = !html;
}

let nowArtistHtml = ""; // rewritten only when it changes: a focused artist link keeps its focus

function setNowArtist(html) {
  if (html === nowArtistHtml) return;
  nowArtistHtml = html;
  $("nowArtist").innerHTML = html;
  $("nowArtist").hidden = !html;
}

function renderNow() {
  const t = shownTrack();
  $("stage").classList.toggle("is-loading", isLoading());
  $("stage").classList.toggle("is-starting", (playPending() || restoringNow()) && !shownTrack());
  const title = $("nowTitle");
  if (t) {
    title.classList.remove("is-connecting");
    title.textContent = t.name;
    setNowArtist(artistLinks(t));
    setText("nowAlbum", t.album);
    setText("emptyState", "");
    return;
  }
  let head = "Nothing playing";
  let line = "Pick a playlist from your library to start.";
  // a play of an unknown track, or the restore of one: a loader, never "Nothing playing"
  const starting = (playPending() && !t) || restoringNow();
  if (starting) {
    head = "Loading…";
    line = "";
  } else if (state.mode === "other" && state.device) {
    const name = labelOf(state.device);
    head = name === HERE ? "Playing here" : `Playing on ${name}`;
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
  title.classList.toggle("is-connecting", starting || (!state.loaded && !gaveUp(failures)));
  // starting: blank artist and album lines hold the song's layout, so nothing jumps when it shows
  setNowArtist(starting ? "&nbsp;" : "");
  setText("nowAlbum", starting ? "\u00a0" : "");
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
  $("panelBtn").disabled = mode !== "track";
  if (mode !== "track") closePanel();
  else if (panelOpen) renderPanel(); // shuffle may have flipped
  $("scrub").tabIndex = mode === "track" ? 0 : -1;

  const noDevice = state.devices && state.devices.length === 0 && mode === "idle";
  $("libraryBtn").classList.toggle("is-primary", mode === "idle" && !noDevice && state.loaded && !starting && !restoringNow());

  // hidden only until the first poll: with no device the button still opens the (empty) list
  const dev = $("deviceBtn");
  dev.hidden = !state.device && !state.loaded;
  dev.classList.toggle("is-none", !state.device && !movingTo);
  dev.classList.toggle("is-pending", Boolean(movingTo));
  if (movingTo) dev.setAttribute("aria-busy", "true");
  else dev.removeAttribute("aria-busy");
  const shown = movingTo ? (movingTo.name === HERE ? "Moving here…" : `Moving to ${movingTo.name}…`) : state.device ? labelOf(state.device) : "No device";
  dev.querySelector(".device-name").textContent = shown;

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
  box.classList.toggle("is-low", v > 0 && v < 40);
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
  // next / previous or a play on its way: the bar stops at once and runs the loading line until the
  // new song shows (a known song's length shows already)
  const loading = isLoading();
  const frozen = Boolean(skipWait) || loading;
  const t = loading ? shownTrack() : state.now;
  const dur = t && !skipWait ? t.duration_ms || 0 : 0;
  const p = frozen ? 0 : progress();
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

/** vars: CSS custom properties from glow.js (glowVars / fallbackVars) */
function setLayer(el, vars) {
  for (const [k, v] of Object.entries(vars)) el.style.setProperty(k, v);
}

async function paint(url) {
  const token = ++colorToken;
  const g = await extractGlow(url);
  if (token !== colorToken || !g) return; // keep previous colours on failure
  const vars = glowVars(g);
  const key = Object.values(vars).join("|");
  if (activeBg.dataset.colors === key) return;
  const next = activeBg === $("bgA") ? $("bgB") : $("bgA");
  const old = activeBg;
  // a skip within the fade: the layer coming back is still on, so restart its fade from 0 (no jump cut)
  if (next.classList.contains("is-on")) {
    next.classList.remove("is-on");
    void next.offsetWidth;
  }
  setLayer(next, vars);
  next.dataset.colors = key;
  next.style.zIndex = "1";
  old.style.zIndex = "0";
  next.classList.add("is-on");
  activeBg = next;
  document.documentElement.style.setProperty("--i", vars["--i"]);
  document.documentElement.style.setProperty("--v", vars["--v"]);
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
      if (!limitedToast("remote", e)) toast(`Spotify didn't respond: ${reason(e)}`);
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
async function routed(deviceId, local, remote, what = "command") {
  if (isLocal(engine, deviceId, activeId)) {
    try {
      return await logged(`${what} local`, local);
    } catch (e) {
      if (!isCode(e, "ENGINE_NOT_READY")) throw e;
    }
  }
  return logged(`${what} remote on ${devName(deviceId)}`, remote);
}

/** A device id as the log names it: its label and id. */
function devName(id) {
  const d = (state.devices || []).find((x) => x.id === id) || (state.device && state.device.id === id ? state.device : null);
  return d ? `"${labelOf(d)}" (${id})` : String(id || "no device");
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
  const at = src.trackUri ? src.uris.indexOf(src.trackUri) : 0;
  return invoke("play_on_device", { deviceId, uris: at > 0 ? src.uris.slice(at) : src.uris });
}

/**
 * A play the user started: loaders from the click until a poll shows it playing; once it lands it is
 * the last source (cover jumps, the panel). row: the clicked row; refused(e): true for an error the
 * caller handles itself (the play then counts as failed); members: the context's known track uris.
 */
async function startPlay(src, { kind, row = null, refused = null, members = null } = {}) {
  preview = kind === "resume" ? null : previewOf(src);
  const token = startPending(kind, src.trackUri || null, row);
  applog("info", `play ${kind}: ${JSON.stringify({ ...src, uris: src.uris && src.uris.length })} on ${state.device ? devName(state.device.id) : "no device yet"}`);
  let handled = false;
  const sent = await changeTrack(async (id) => {
    const deviceId = await needDevice(id);
    try {
      await playSource(deviceId, src);
    } catch (e) {
      applog("warn", `play ${kind} failed: ${e}`);
      if (!(refused && refused(e))) throw e;
      handled = true; // withDevice would retry it on another device
    }
  });
  const ok = sent && !handled;
  settlePending(token, ok);
  if (ok) setLastSrc(src, members || (src.contextUri && knownRows.get(src.contextUri)) || null);
  kick();
  return ok;
}

// ---------- pending play: spinner, the clicked row, an 8s timeout ----------

const seenTracks = new Map(); // uri → a track some list showed: what a play is about to start
let preview = null; // the track a pending play starts, shown at once (or {} when unknown)

/** What a play of src starts, as far as the lists on screen know. */
function previewOf(src) {
  const uri = src.trackUri || (src.uris && src.uris[0]) || (src.contextUri && !state.shuffle && (knownRows.get(src.contextUri) || [])[0]);
  return (uri && seenTracks.get(uri)) || {};
}

/** The track on screen: a pending play's preview until Spotify reports that track, else the real one. */
function shownTrack() {
  if (preview && playPending() && !(state.now && preview.uri && state.now.uri === preview.uri)) return preview.uri ? preview : null;
  if (!state.now && restoring && restoring.track) return restoring.track; // the session Rust loads back
  return state.now;
}

/** Rust is loading the last session back and no poll has shown a song yet. */
const restoringNow = () => Boolean(restoring && !state.now && !playPending());

/** A play (or the restore) is starting and Spotify hasn't reported its track yet. */
const isLoading = () => Boolean(((preview && playPending()) || restoringNow()) && shownTrack() !== state.now);

const pending = createPending();
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
    applog("warn", `play ${kind} not confirmed after ${PENDING_MS}ms: want ${trackUri}, now ${state.now && state.now.uri}, playing ${state.isPlaying}`);
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
    preview = null;
    clearTimeout(pendingTimer);
    pendingTimer = null;
    if (pendingRow) pendingRow.classList.remove("is-pending");
    pendingRow = null;
  }
  if (!render || $("stage").hidden) return;
  renderNow();
  renderRun(); // the preview's cover comes in big, or leaves when the play ends
  renderChrome();
}

// ---------- the last source, and the session Rust restores at launch ----------

let accountP = null; // promise of the /me id for this login session
let accountNow = null; // that id once known, else null
// what the last play started from: {contextUri, uris, trackUri}. uris: the list played, or with a
// context its known members. From this run's plays, else Rust's saved session (session_get).
let lastSrc = null;
// Rust's saved session while it loads back at launch: {trackUri, track (null = unknown), until}
let restoring = null;
const RESTORE_WAIT_MS = 20000; // the player connects and loads it; longer = it isn't coming

const ACCOUNT_KEY = "account"; // the last /me id, for the disk cache while the Web API is rate-limited

/** The signed-in account's id (scopes the list cache), or null on failure. */
function accountId() {
  if (!accountP) {
    const p = invoke("me_id").then(
      (id) => {
        if (id) storeSet(ACCOUNT_KEY, id);
        return (accountNow = id || null);
      },
      (e) => {
        if (isCode(e, "AUTH_EXPIRED")) expire();
        // rate-limited: the account this Mac last saw, so the disk cache still shows
        const known = isCode(e, "RATE_LIMITED") ? storeGet(ACCOUNT_KEY) : null;
        if (accountP === p && !known) accountP = null; // retry on the next ask
        return (accountNow = known || null);
      },
    );
    accountP = p;
  }
  return accountP;
}

/** A play landed: remember its source. With a context, uris are only its known members. */
function setLastSrc(src, members = null) {
  const uris = src.contextUri ? members : src.uris;
  lastSrc = {
    contextUri: src.contextUri || null,
    uris: Array.isArray(uris) && uris.length ? uris.slice(0, PLAY_URIS_MAX) : null,
    trackUri: src.trackUri || (src.uris && src.uris[0]) || null,
  };
}

/**
 * Rust's saved session ({contextUri, uris, trackUri, …}): the last source when this run has none yet,
 * and, before any poll showed a song, the song it restores, on screen at once (no "Nothing playing").
 */
function noteRestored(s) {
  if (!s || typeof s.trackUri !== "string" || !s.trackUri) return;
  if (!lastSrc) setLastSrc({ contextUri: s.contextUri || null, uris: s.uris || null, trackUri: s.trackUri });
  if (state.now || playPending() || (restoring && restoring.trackUri === s.trackUri)) return;
  const track = seenTracks.get(s.trackUri) || null;
  restoring = { trackUri: s.trackUri, track, until: performance.now() + RESTORE_WAIT_MS };
  applog("info", `session: restoring ${s.trackUri} from ${s.contextUri || `${(s.uris || []).length} uris`}${track ? "" : " (track not known yet)"}`);
  if (!track) restoredTrack(s).then((t) => {
    if (!t || !restoring || restoring.trackUri !== s.trackUri) return;
    restoring.track = t;
    if (!$("stage").hidden) renderPending();
  });
  clearTimeout(restoreTimer);
  restoreTimer = setTimeout(() => endRestoring("not loaded in time"), RESTORE_WAIT_MS);
  if (!$("stage").hidden) renderPending();
}

let restoreTimer = null;

/** The restored song shows as itself now (a poll has it), or isn't coming. */
function endRestoring(why = "") {
  if (!restoring) return;
  if (why) applog("info", `session: restore ${why}`);
  restoring = null;
  clearTimeout(restoreTimer);
  if (!$("stage").hidden) renderPending();
}

/** The restored song's track from the disk copy of its list (no network), or null. */
async function restoredTrack(s) {
  const find = (v) => listTracks(v).find((t) => t && t.uri === s.trackUri) || null;
  const [, kind, id] = String(s.contextUri || "").split(":");
  let key = null;
  if (kind === "album" && id) key = `album:${id}`;
  else if (/:collection$/.test(s.contextUri || "")) key = "liked";
  else if (kind === "playlist" && id) {
    const p = (await diskGet("playlists") || []).find((x) => x && x.id === id);
    if (p && p.snapshot_id) key = `playlist:${id}:${p.snapshot_id}`;
  }
  if (key) return find(await diskGet(key));
  // a uris play: Liked Songs and the top lists hold most of them
  for (const k of ["liked", topTracksKey("short_term")]) {
    const t = find(await diskGet(k));
    if (t) return t;
  }
  return null;
}

/** Ask Rust for the saved session once per stage start; the event covers a restore that lands later. */
async function loadRestored() {
  try {
    noteRestored(await invoke("session_get"));
  } catch (e) {
    applog("warn", `session_get failed: ${e}`);
  }
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

/** Show the in-app player as the device, like a pick, without a transfer: the load made it active. */
function selectTheRun(runId) {
  if (state.device && state.device.id === runId) return;
  const d = (state.devices || []).find((x) => x.id === runId);
  showDevice(d || { id: runId, name: "This Mac" }); // Spotify's name for it; shown as "Here"
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
  else clearPending(); // a pause ends any wait for sound
  renderChrome();
  const ok = await sendIntent(
    "play",
    (id) =>
      routed(
        dev,
        () => invoke(want ? "local_play" : "local_pause"),
        async () => (want ? resumeOrRestart(await needDevice(id)) : invoke("pause")),
        want ? "play" : "pause",
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
  const dev = state.device && state.device.id;
  await sendIntent(
    "shuffle",
    () => routed(dev, () => invoke("local_shuffle", { on: want }), () => invoke("set_shuffle", { on: want }), `shuffle ${want ? "on" : "off"}`),
    () => (state.shuffle = !want),
  );
}

/** off → context → track → off. */
async function cycleRepeat() {
  if (state.mode !== "track") return;
  const before = state.repeat;
  const mode = (state.repeat = nextRepeat(before));
  renderChrome();
  const dev = state.device && state.device.id;
  await sendIntent(
    "repeat",
    () => routed(dev, () => invoke("local_repeat", { mode }), () => invoke("set_repeat", { mode }), `repeat ${mode}`),
    () => (state.repeat = before),
  );
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
    () => routed(deviceId, () => invoke("local_volume", { percent }), () => invoke("set_volume", { percent, deviceId }), `volume ${percent}%`),
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
    if (!limitedToast("library", e)) toast(`Spotify didn't respond: ${reason(e)}`);
  }
}

// ---------- device picker ----------

let devicesOpen = false;
let devicesGen = 0; // the latest list_devices request: an older answer is dropped
let devicesNote = ""; // loading or error line while there is no list to show
let devicesTimer = null; // refreshes the open menu while the in-app player isn't listed
const DEVICES_REFRESH_MS = 10000; // Rust lists the in-app player itself once it's ready: this is only a safety net
let runMissingSince = 0; // menu open, engine ready, the in-app player not listed: since when (0 = not missing)

function toggleDevices() {
  if (devicesOpen) closeDevices(true);
  else openDevices();
}

function openDevices() {
  closeVolume();
  closePanel();
  closeSettings();
  devicesOpen = true;
  $("devicePop").hidden = false;
  $("deviceBtn").setAttribute("aria-expanded", "true");
  renderDeviceList();
  focusDevice(0);
  refreshDevices();
  clearInterval(devicesTimer);
  devicesTimer = setInterval(() => {
    // the in-app player may be listed any second now; one request at a time, so a slow answer
    // isn't discarded by the next tick's newer generation
    if (!devicesBusy && !thisMacBusy && !(state.devices || []).some((d) => isHere(d, engine))) refreshDevices();
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
      const tip = d.is_restricted ? "Spotify doesn't allow remote control of this device" : "";
      return (
        `<button class="device-row${active ? " is-active" : ""}" type="button" role="option" data-i="${i}" data-device="${esc(d.id)}"` +
        ` aria-selected="${active}"${tip ? ` title="${esc(tip)}"` : ""}${d.is_restricted ? ' aria-disabled="true"' : ""} tabindex="-1">` +
        `<span class="device-row-dot"></span><span class="device-row-name">${esc(labelOf(d))}</span>` +
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

/** The in-app player isn't listed yet: offer it ("Here") with the engine's state. */
function renderThisMacRow(list) {
  const missing = devicesOpen && engine && engine.state === "ready" && state.devices && !list.some((d) => isHere(d, engine));
  if (!missing) runMissingSince = 0;
  else if (!runMissingSince) runMissingSince = performance.now();
  const missingMs = runMissingSince ? performance.now() - runMissingSince : 0;
  const row = thisMacRow(engine, state.devices && list, thisMacBusy, missingMs);
  if (!row) return "";
  return (
    `<button class="device-row" type="button" role="option" data-this-mac="1" tabindex="-1" title="${esc(row.title)}">` +
    `<span class="device-row-dot"></span><span class="device-row-name">${HERE}</span>` +
    `<span class="device-row-type">${esc(row.type)}</span></button>`
  );
}

// ---------- the in-app player (Spotify lists it as "This Mac"; our UI says "Here") ----------

let engine = null; // the last engine_status / engine-status payload, null = unknown
let hereId = null; // the player's last known device id: a restart (no id for a moment) still shows "Here"

/** A device's name in the UI: "Here" for the in-app player, else Spotify's. */
const labelOf = (d) => deviceLabel(d, { device_id: (engine && engine.device_id) || hereId });
let thisMacBusy = ""; // a click on "This Mac" is working: "login" | "connecting" | ""
const ENGINE_WAIT_MS = 20000; // for a starting/reconnecting player to be ready
const THE_RUN_WAIT_MS = 20000; // for a ready player to show up in the device list (by its device id)
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
  const wasId = engine && engine.device_id;
  if (!engine || engine.state !== st.state || engine.device_id !== st.device_id) {
    applog("info", `engine: ${engine ? engine.state : "unknown"} → ${st.state}${st.reason ? ` (${st.reason})` : ""}${st.device_id ? ` id ${st.device_id}` : ""}`);
  }
  engine = st;
  if (st.device_id) hereId = st.device_id;
  // a player that can't run won't load the saved session back
  if (restoring && (NEEDS_LOGIN.has(st.state) || st.state === "failed" || st.state === "account_mismatch")) endRestoring(`stopped: engine ${st.state}`);
  if (settingsOpen) renderSettings(); // quality needs a ready player
  if (st.device_id !== wasId && !$("stage").hidden) renderChrome(); // the chip may be the player: "Here"
  if (!devicesOpen) return;
  renderDeviceList();
  // ready now: the player registers with Spotify, so the open list should show it
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

/** Poll the device list until the in-app player shows up (by the engine's device id); null after ms. */
async function findTheRun(ms, sess) {
  const until = performance.now() + ms;
  for (;;) {
    const list = await fetchOr("list_devices");
    if (sess !== authSession) return null;
    if (list) {
      setDevices(list);
      const run = list.find((d) => isHere(d, engine));
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

/** Get the in-app player ready (logging it in if it must), wait for Spotify to list it, then move playback there. */
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
      await refreshEngine(); // its device id: how the list shows it
      if (sess !== authSession) return;
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
  movingTo = { seq, name: labelOf(d) };
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
  applog(failed ? "warn" : "info", `device pick ${devName(d.id)}: ${failed ? failed : "ok"}`);
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
    } else if (!limitedToast("remote", failed)) {
      toast(`Spotify didn't respond: ${reason(failed)}`);
    }
  }
  kick();
}

/** A press outside an open popover closes it. */
function onOutside(ev) {
  if (devicesOpen && !ev.target.closest(".device-wrap")) closeDevices();
  if (volumeOpen && !ev.target.closest("#volume")) closeVolume();
  if (settingsOpen && !ev.target.closest(".settings-wrap")) closeSettings();
}

// next / previous: a ring on the pressed button from the click until a poll shows another song,
// a poll that began SKIP_SETTLE_MS after the command landed (previous may restart the same song),
// or SKIP_MS, whichever comes first
const SKIP_MS = 5000;
const SKIP_SETTLE_MS = 1500;
let skipWait = null; // {btn, landedAt, timer}

function startSkip(btn) {
  endSkip();
  const wait = { btn, landedAt: 0, timer: setTimeout(endSkip, SKIP_MS) };
  btn.classList.add("is-pending");
  btn.setAttribute("aria-busy", "true");
  skipWait = wait;
  $("stage").classList.add("is-skipping");
  renderProgress();
  return wait;
}

function endSkip() {
  if (!skipWait) return;
  clearTimeout(skipWait.timer);
  skipWait.btn.classList.remove("is-pending");
  skipWait.btn.removeAttribute("aria-busy");
  skipWait = null;
  $("stage").classList.remove("is-skipping");
  renderProgress();
}

async function skip(cmd) {
  if (!state.now) return;
  const dev = state.device && state.device.id; // the device this click is for
  const local = cmd === "next_track" ? "local_next" : "local_prev";
  const wait = startSkip($(cmd === "next_track" ? "nextBtn" : "prevBtn"));
  const ok = await changeTrack(() => routed(dev, () => invoke(local), () => invoke(cmd), cmd === "next_track" ? "next" : "previous"));
  if (skipWait === wait) {
    if (ok) wait.landedAt = performance.now();
    else endSkip();
  }
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
    gen === trackGen ? routed(dev, () => invoke("local_seek", { positionMs }), () => invoke("seek", { positionMs }), `seek ${fmtTime(positionMs)}`) : null,
  );
  kick();
}

// ---------- the playlist panel: the playing list around the current song ----------

let panelOpen = false;
// the source the current song plays from: {key, ctx, name, tracks (null = loading or none), loading}
let panelSrc = null;
let panelGen = 0; // the latest source load: an older one's answer is dropped
let panelShown = ""; // the rows on screen, as a signature: an unchanged render keeps scroll, focus and a spinner
let panelView = { mode: "queue", rows: [] };
let panelNowKey = null; // the current song's row on screen: when it moves, the panel scrolls to it
let lastList = null; // the list the last list play here started from: {tracks, name}

function togglePanel() {
  if (panelOpen) closePanel(true);
  else openPanel();
}

function openPanel() {
  if (state.mode !== "track") return;
  closeDevices();
  closeVolume();
  closeSettings();
  panelOpen = true;
  panelShown = "";
  panelNowKey = null;
  $("panelLayer").hidden = false;
  $("panelBtn").setAttribute("aria-expanded", "true");
  $("stage").inert = true; // a modal sheet over the stage
  renderPanel();
  $("panel").focus({ preventScroll: true });
}

function closePanel(refocus = false) {
  if (!panelOpen) return;
  panelOpen = false;
  panelGen++; // a list still loading must not land in a closed panel
  panelSrc = null; // the next open asks again (the list cache makes it quick)
  $("panelLayer").hidden = true;
  $("panelBtn").setAttribute("aria-expanded", "false");
  if (!state.overlay) $("stage").inert = false;
  if (refocus) $("panelBtn").focus();
}

/** A playback context as the panel lists it, or null when Spotify won't list it (artist, radio…). */
function contextSource(ctx) {
  const [, kind, id] = String(ctx).split(":");
  if (kind === "playlist" && id) {
    return {
      key: ctx,
      ctx,
      name: null,
      fetch: async (shown, named) => {
        // the name and the snapshot id (it keys the disk copy): the loaded playlists, their disk copy, or a fetch
        const find = (list) => (Array.isArray(list) ? list : []).find((x) => x && x.id === id) || null;
        let p = find(playlists) || find(await diskGet("playlists"));
        if (!p && !playlists) p = find(await listInvoke("get_playlists").catch(() => null));
        if (p) named(p.name);
        else mixInfoFor(id).then((info) => info && named(info.name)); // a Spotify mix: not in the playlists
        const snap = (p && p.snapshot_id) || null;
        const key = snap ? `playlist:${id}:${snap}` : `playlist:${id}`;
        if (snap && !lib.has(key)) diskGet(key).then((v) => v && shown(listTracks(v)));
        return listTracks(await libGet(key, "get_playlist_tracks", { playlistId: id, snapshotId: snap }));
      },
    };
  }
  if (kind === "album" && id) {
    const key = `album:${id}`;
    return {
      key: ctx,
      ctx,
      name: state.now && state.now.album, // the song plays from its own album
      fetch: async (shown) => {
        if (!lib.has(key)) diskGet(key).then((v) => v && shown(listTracks(v)));
        return listTracks(await libGet(key, "get_album_tracks", { albumId: id }));
      },
    };
  }
  if (/:collection$/.test(ctx)) {
    return {
      key: ctx,
      ctx,
      name: "Liked Songs",
      fetch: async (shown) => {
        if (!lib.has("liked")) diskGet("liked").then((v) => v && shown(listTracks(v)));
        return listTracks(await libGet("liked", "get_saved_tracks"));
      },
    };
  }
  return null;
}

/** What the current song plays from: the context, else the list a play here started from, else the last source's context. */
function panelSource() {
  const t = state.now;
  if (!t) return null;
  if (state.contextUri) return contextSource(state.contextUri);
  // a play by uris: Spotify names no context
  if (lastList && lastList.tracks.some((x) => x.uri === t.uri)) {
    const l = lastList;
    return { key: l, ctx: null, name: l.name, fetch: async () => l.tracks };
  }
  // a context load Spotify doesn't name yet: the last source's
  const last = lastSrc;
  const ctx = last && offsettable(last.contextUri) ? last.contextUri : null;
  if (ctx && (last.trackUri === t.uri || (last.uris && last.uris.includes(t.uri)))) return contextSource(ctx);
  return null; // the queue view: recent plays, now, Spotify's queue
}

/** Start loading src's tracks; each answer (disk copy, then fresh) re-renders the open panel. */
function loadPanelSource(src) {
  const gen = ++panelGen;
  panelSrc = src ? { ...src, tracks: null, loading: true } : null;
  if (!src) return;
  const cur = panelSrc;
  const live = () => gen === panelGen && panelOpen;
  const named = (name) => {
    if (!live() || !name || cur.name) return;
    cur.name = name;
    renderPanel();
  };
  const shown = (tracks) => {
    if (!live() || !cur.loading) return; // the fresh list already landed
    cur.tracks = tracks;
    renderPanel();
  };
  src.fetch(shown, named).then(
    (tracks) => {
      if (!live()) return;
      cur.loading = false;
      cur.tracks = (tracks || []).filter((x) => x && x.uri);
      if (cur.ctx && offsettable(cur.ctx)) knownRows.set(cur.ctx, cur.tracks.map((x) => x.uri)); // for cover clicks
      renderPanel();
    },
    (e) => {
      if (!live()) return;
      if (isCode(e, "AUTH_EXPIRED")) return void expire();
      cur.loading = false; // the queue view instead (a mix, a list Spotify won't give, a network blip)
      renderPanel();
    },
  );
}

function renderPanel() {
  if (!panelOpen || !state.now) return;
  const src = panelSource();
  if ((src && src.key) !== (panelSrc && panelSrc.key)) loadPanelSource(src);
  const name = panelSrc && panelSrc.name;
  $("panelTitle").textContent = name || "Now playing";
  $("panel").querySelector(".panel-kicker").hidden = !name;
  const list = $("panelList");
  // shuffle off: the list is on its way; skeletons, not a queue view that jumps to the list
  if (panelSrc && panelSrc.loading && !panelSrc.tracks && !state.shuffle) {
    if (panelShown !== "loading") {
      list.innerHTML = skeletonRows(6);
      list.setAttribute("aria-busy", "true");
      panelShown = "loading";
    }
    return;
  }
  list.removeAttribute("aria-busy");
  const view = panelRows({ list: panelSrc && panelSrc.tracks, now: state.now, history: history(), queue: state.queue, shuffle: state.shuffle });
  const sig = `${view.mode}#${view.rows.map((r) => `${r.key}:${r.role}:${r.track.uri}`).join("|")}`;
  if (sig === panelShown) return;
  panelShown = sig;
  panelView = view;
  list.innerHTML = panelHtml(view);
  // a play clicked in the panel keeps its spinner across a re-render
  if (pendingRow && !pendingRow.isConnected && pendingRow.dataset.key) {
    const row = list.querySelector(`[data-key="${CSS.escape(pendingRow.dataset.key)}"]`);
    if (row) {
      pendingRow = row;
      row.classList.add("is-pending");
    }
  }
  const nowRow = view.rows.find((r) => r.role === "now");
  const nowKey = nowRow ? nowRow.key : null;
  if (nowKey !== panelNowKey) {
    panelNowKey = nowKey;
    centerPanelNow();
  }
}

/** Played rows (with a heading in the queue view), the current one, what's next. */
function panelHtml(view) {
  let html = "";
  let role = "";
  view.rows.forEach((r, n) => {
    if (view.mode === "queue" && r.role !== role && r.role !== "now") {
      html += `<p class="panel-label">${r.role === "played" ? "Played" : "Up next"}</p>`;
    }
    role = r.role;
    const t = r.track;
    const local = isLocalFile(t.uri);
    html +=
      `<button class="row is-${r.role}${local ? " is-local" : ""}" type="button" data-n="${n}" data-key="${r.key}"` +
      `${r.role === "now" ? ' aria-current="true"' : ""}${local ? " disabled" : ""}>` +
      `<span class="art row-art">${artHtml(t.cover, t.name)}</span>` +
      `<span class="row-text"><span class="row-title">${esc(t.name)}</span><span class="row-sub">${esc(t.artists)}</span></span>` +
      `<span class="row-time">${fmtTime(t.duration_ms)}</span></button>`;
  });
  return html;
}

/** Scroll the panel's list (only it) so the current row sits in the middle. */
function centerPanelNow() {
  const list = $("panelList");
  const row = list.querySelector('[aria-current="true"]');
  if (row) list.scrollTop = row.offsetTop - list.offsetTop - (list.clientHeight - row.offsetHeight) / 2;
}

function onPanelClick(e) {
  const el = e.target.closest(".row[data-n]");
  const r = el && panelView.rows[Number(el.dataset.n)];
  if (!r || r.role === "now" || isLocalFile(r.track.uri)) return;
  applog("info", `panel: play ${r.role} row ${r.track.uri} "${r.track.name}" (${panelView.mode} view${state.shuffle ? ", shuffle" : ""})`);
  if (panelView.mode === "list") return void playPanelRow(r, el);
  // the queue view: a played or a queued song, as its cover in the run would play it
  playCover({ role: r.role === "played" ? "past" : "next", offset: r.role === "next" ? r.i + 1 : -1, track: r.track }, el);
}

/** A row of the playing list: the context from that song, else the list from that row on (like the Library). */
function playPanelRow(r, el) {
  const src = panelSrc;
  if (!src || !src.tracks) return;
  if (src.ctx && offsettable(src.ctx)) {
    startPlay({ contextUri: src.ctx, trackUri: r.track.uri }, { kind: "panel", row: el });
    return;
  }
  const uris = src.tracks.slice(r.i).map((t) => t.uri).filter((u) => !isLocalFile(u)).slice(0, PLAY_URIS_MAX);
  if (!uris.length) return;
  // Spotify names no context for a uris play: the panel keeps showing this list
  lastList = { tracks: src.tracks, name: src.name };
  startPlay({ uris, trackUri: uris[0] }, { kind: "panel", row: el });
}

// ---------- settings: album art as the app icon, the in-app player's audio quality ----------

let settings = null; // parseSettings of the stored "settings", read once
let settingsOpen = false;
let quality = null; // the in-app player's bitrate (96 | 160 | 320), null = unknown
let qualityBusy = 0; // the kbps being applied while the player restarts, 0 = none
let restartHoldUntil = 0; // until then (or the reload), polls that see nothing playing are ignored
const RESTART_SEEN_MS = 2500; // after engine_set_quality: a restart that hasn't shown up by now isn't coming

function getSettings() {
  if (!settings) settings = parseSettings(storeGet(SETTINGS_KEY));
  return settings;
}

function saveSettings() {
  storeSet(SETTINGS_KEY, { ...getSettings() });
}

function toggleSettings() {
  if (settingsOpen) closeSettings(true);
  else openSettings();
}

function openSettings() {
  closeDevices();
  closeVolume();
  closePanel();
  settingsOpen = true;
  $("settingsPop").hidden = false;
  $("settingsBtn").setAttribute("aria-expanded", "true");
  renderSettings();
  $("dockArtSwitch").focus();
  loadQuality();
  loadMcp();
}

function closeSettings(refocus = false) {
  if (!settingsOpen) return;
  settingsOpen = false;
  $("mcpResetAsk").hidden = true;
  $("settingsPop").hidden = true;
  $("settingsBtn").setAttribute("aria-expanded", "false");
  if (refocus) $("settingsBtn").focus();
}

const engineReady = () => Boolean(engine && engine.state === "ready");

function renderSettings() {
  $("dockArtSwitch").setAttribute("aria-checked", String(getSettings().dockArt));
  $("coverRowSwitch").setAttribute("aria-checked", String(getSettings().coverRow));
  const ready = engineReady();
  for (const b of $("qualityOpts").querySelectorAll("[data-kbps]")) {
    const kbps = Number(b.dataset.kbps);
    const on = qualityBusy ? kbps === qualityBusy : kbps === quality;
    b.setAttribute("aria-checked", String(on));
    b.tabIndex = on || (!quality && !qualityBusy && kbps === 160) ? 0 : -1;
    b.disabled = !ready || Boolean(qualityBusy);
    b.classList.toggle("is-pending", kbps === qualityBusy);
    if (kbps === qualityBusy) b.setAttribute("aria-busy", "true");
    else b.removeAttribute("aria-busy");
  }
  $("qualityNote").textContent = qualityBusy
    ? "Restarting the player…"
    : ready
      ? "Changing restarts the player for a moment."
      : "Available once the player here is ready.";
}

// ---------- settings: the local MCP server (Rust mcp.rs) for AI tools on this Mac ----------

let mcp = null; // mcp_status: {enabled, running, port, error, callsToday}; null = not known
let mcpBusy = false; // a switch flip is on its way

async function loadMcp() {
  try {
    mcp = await invoke("mcp_status");
  } catch {
    mcp = null;
  }
  if (settingsOpen) renderMcp();
}

function renderMcp() {
  const on = Boolean(mcp && mcp.enabled);
  const asking = !$("mcpResetAsk").hidden;
  $("mcpSwitch").setAttribute("aria-checked", String(on));
  $("mcpSwitch").disabled = mcpBusy;
  setText("mcpStatus", mcpBusy ? (on ? "Stopping…" : "Starting…") : mcpStatusLine(mcp));
  $("mcpActions").hidden = !on || asking;
  if (!on) $("mcpResetAsk").hidden = true;
}

async function toggleMcp() {
  if (mcpBusy) return;
  const on = !(mcp && mcp.enabled);
  mcpBusy = true;
  renderMcp();
  try {
    mcp = await invoke("mcp_set_enabled", { on });
    applog("info", `mcp server ${on ? "on" : "off"}: ${mcpStatusLine(mcp)}`);
  } catch (e) {
    toast(`Couldn't turn the MCP server ${on ? "on" : "off"}: ${reason(e)}`);
  } finally {
    mcpBusy = false;
    if (settingsOpen) renderMcp();
  }
}

/** Copy a connect text or the skill (Rust makes them, with the key) to the clipboard. */
async function copyMcp(kind) {
  const [cmd, args, what] = MCP_COPY[kind] || [];
  if (!cmd) return;
  try {
    const text = await invoke(cmd, args);
    await navigator.clipboard.writeText(text);
    toast(`Copied: ${what}`);
  } catch (e) {
    toast(`Couldn't copy: ${reason(e)}`);
  }
}

/** Reset key asks first, inline: the copied connect texts stop working. */
function askResetKey(ask) {
  $("mcpResetAsk").hidden = !ask;
  renderMcp();
  (ask ? $("mcpResetNo") : $("mcpReset")).focus();
}

async function resetMcpKey() {
  try {
    mcp = await invoke("mcp_reset_key");
    toast("New key made: copy the connect text again");
  } catch (e) {
    toast(`Couldn't reset the key: ${reason(e)}`);
  }
  askResetKey(false);
}

async function loadQuality() {
  try {
    const q = await invoke("engine_get_quality");
    if (isQuality(q) && !qualityBusy) quality = q;
  } catch {
    /* unknown: no option marked */
  }
  if (settingsOpen) renderSettings();
}

function toggleDockArt() {
  const s = getSettings();
  s.dockArt = !s.dockArt;
  saveSettings();
  applog("info", `setting: album art as app icon ${s.dockArt ? "on" : "off"}`);
  renderSettings();
  if (s.dockArt) syncDockArt();
  else resetDockArt();
}

function toggleCoverRow() {
  const s = getSettings();
  s.coverRow = !s.coverRow;
  saveSettings();
  applog("info", `setting: cover row ${s.coverRow ? "on" : "off"}`);
  renderSettings();
  applyCoverRow();
}

/** The cover row on or off: off = the current cover alone, centred, with its title under it. */
function applyCoverRow() {
  const solo = soloRun();
  $("stage").classList.toggle("is-solo", solo);
  resetPan();
  if (!$("stage").hidden) renderRun();
}

let dockUrl = null; // the cover set as the app icon, null = the app's own icon

/** The current song's cover as the app icon (when on), once per cover. Errors are logged in Rust. */
function syncDockArt() {
  if (!getSettings().dockArt || !state.now) return;
  const url = state.now.cover || null;
  if (url === dockUrl) return;
  dockUrl = url;
  invoke("set_dock_art", { url }).catch(() => {});
}

/** Back to the app's own icon. Not session-tagged: a logout resets it after the session is gone. */
function resetDockArt() {
  dockUrl = null;
  window.__TAURI__.core.invoke("set_dock_art", { url: null }).catch(() => {});
}

/**
 * Watch engine-status across a player restart. done: the state once the player is back (a starting or
 * reconnecting, then anything else), or null after ms. arm(): the restart command returned; when no
 * restart shows up within RESTART_SEEN_MS, the engine's current state counts. stop(): give up.
 */
function watchRestart(ms) {
  let over = false;
  let saw = false;
  let unlisten = null;
  let quiet = null;
  let resolve;
  const done = new Promise((r) => (resolve = r));
  const finish = (st) => {
    if (over) return;
    over = true;
    clearTimeout(timer);
    clearTimeout(quiet);
    if (unlisten) unlisten();
    resolve(st);
  };
  const timer = setTimeout(() => finish(null), ms);
  const ready = listenEvent("engine-status", (st) => {
    if (!st || !st.state) return;
    if (CONNECTING.has(st.state)) {
      saw = true;
      clearTimeout(quiet);
    } else if (saw) finish(st);
  }).then((u) => (over ? u() : (unlisten = u)));
  const arm = () => {
    if (!saw) quiet = setTimeout(() => !saw && waitEngine(ms).then(finish), RESTART_SEEN_MS);
  };
  return { ready, done, arm, stop: () => finish(null) };
}

/** The current song's source for a reload on the in-app player: like a resume, where it is now. */
function sourceNow() {
  const t = state.now;
  const last = lastSrc;
  if (state.contextUri) return { contextUri: state.contextUri, trackUri: t.uri };
  if (last && last.uris && last.uris.includes(t.uri)) return { uris: last.uris, trackUri: t.uri };
  return { uris: [t.uri], trackUri: t.uri };
}

/**
 * Pick a bitrate: the player restarts on it. When it was playing (or holding) the current song,
 * that song is loaded again where it was, playing or paused as before.
 */
async function setQuality(kbps) {
  if (qualityBusy || kbps === quality || !engineReady() || !isQuality(kbps)) return;
  const sess = authSession;
  const t = state.now;
  const here = Boolean(t && !isLocalFile(t.uri) && state.device && isLocal(engine, state.device.id, activeId));
  const back = here ? { ...sourceNow(), positionMs: Math.round(progress()), play: state.isPlaying } : null;
  qualityBusy = kbps;
  applog("info", `quality: ${quality || "?"} → ${kbps} kbps${back ? `, reloading ${back.trackUri} at ${fmtTime(back.positionMs)}` : ""}`);
  renderSettings();
  if (back) restartHoldUntil = performance.now() + ENGINE_WAIT_MS + RESTART_SEEN_MS;
  try {
    await restartOnQuality(kbps, sess, back);
  } finally {
    if (sess === authSession) restartHoldUntil = 0;
  }
}

/** setQuality's restart: the command, the wait for the player to be back, the reload of back (or nothing). */
async function restartOnQuality(kbps, sess, back) {
  const watch = watchRestart(ENGINE_WAIT_MS);
  await watch.ready;
  try {
    await invoke("engine_set_quality", { kbps });
  } catch (e) {
    watch.stop();
    if (sess !== authSession) return;
    qualityBusy = 0;
    renderSettings();
    if (isCode(e, "AUTH_EXPIRED")) return void expire();
    applog("warn", `quality ${kbps}: ${e}`);
    toast(`Couldn't change the quality: ${reason(e)}`);
    return;
  }
  if (sess !== authSession) return void watch.stop();
  quality = kbps;
  watch.arm();
  const st = await watch.done;
  if (sess !== authSession) return;
  qualityBusy = 0;
  renderSettings();
  applog(st && st.state === "ready" ? "info" : "warn", `quality ${kbps}: player ${st ? st.state : "not back in time"}`);
  if (!st || st.state !== "ready") return void toast("The player here is still restarting. Try again in a moment.");
  // the user started something else meanwhile: that wins
  if (!back || changesPending || pending.current()) return;
  const runId = st.device_id || (engine && engine.device_id);
  const token = startPending("resume", back.trackUri, null, back.play);
  let failed = false;
  await changeTrack(() =>
    invoke("local_load", { ...back, shuffle: state.shuffle, repeat: state.repeat }).catch((e) => {
      if (isCode(e, "AUTH_EXPIRED")) throw e;
      failed = true;
    }),
  );
  if (sess !== authSession) return;
  settlePending(token, !failed);
  if (failed) {
    applog("warn", `quality ${kbps}: reload of ${back.trackUri} failed`);
    return void toast("Couldn't load the song again here");
  }
  if (runId) selectTheRun(runId);
  kick();
}

function onQualityClick(e) {
  const b = e.target.closest("[data-kbps]");
  if (b && !b.disabled) setQuality(Number(b.dataset.kbps));
}

/** Arrows move the choice inside the radio group, like the page tabs. */
function onQualityKey(e) {
  const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
  if (!step) return;
  e.preventDefault();
  const opts = [...$("qualityOpts").querySelectorAll("[data-kbps]")];
  const at = Math.max(0, opts.indexOf(document.activeElement));
  opts[(at + step + opts.length) % opts.length].focus();
}

// ---------- overlays: one open at a time ----------

let returnFocus = null;

/** back: returning to a view the user already saw: no entry animation. */
function openOverlay(name, back = false) {
  if (state.overlay === name) return;
  $(name).classList.toggle("is-back", back);
  closeDevices(); // popovers belong to the stage, which goes inert
  closeVolume();
  closePanel();
  closeSettings();
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


/**
 * A playable track row. num: show the position; art: show the cover (album rows skip it, it's the same every row).
 * The title button covers the whole row (CSS), so a click anywhere plays it; the artist links and "+" sit on top.
 */
function trackRow(t, i, { num, art }) {
  seenTracks.set(t.uri, t);
  const local = isLocalFile(t.uri);
  const kind = `${num ? " has-num" : ""}${art ? " has-art" : ""}${local ? " is-local" : ""}`;
  const tip = local ? "A local file: play it in Spotify" : "";
  return (
    `<div class="row row-track${kind}" data-i="${i}"${tip ? ` title="${esc(tip)}"` : ""}>` +
    (num ? `<span class="row-num">${i + 1}</span>` : "") +
    (art ? `<span class="art row-art">${artHtml(t.cover, t.name)}</span>` : "") +
    `<span class="row-text"><button class="row-title row-play" type="button"${local ? " disabled" : ""}>${esc(t.name)}</button>` +
    `<span class="row-sub">${artistLinks(t)}</span></span>` +
    `<span class="row-time">${fmtTime(t.duration_ms)}</span>` +
    // Spotify can't queue a local file
    (local ? "<span></span>" : `<button class="row-queue" type="button" aria-label="Add to queue" title="Add to queue">${ICONS.addToQueue}</button>`) +
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
    `<button class="album${round ? " is-artist" : ""}" type="button" data-i="${i}"${attrs}>` +
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
  state.gen.detail++; // drop any detail response still in flight
  detailTracks = [];
  detailAlbums = [];
  navStack = [];
  curDetail = null;
  $("libDetail").hidden = true;
  $("libBack").hidden = true;
  $("libLevel1").hidden = false;
  $("libTitle").hidden = false;
  $("libTabs").hidden = false;
  renderLibTabs();
  $("sheetBody").scrollTop = listScroll;
}

function goBack() {
  const prev = navStack.pop();
  if (prev === STAGE_ENTRY) closeOverlay(); // opened from the main screen: back to it
  else if (prev === PAGE_ENTRY) returnToPage();
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
function diskGet(key) {
  const p = (async () => {
    const account = await accountId();
    if (!account) return null;
    try {
      return await invoke("cache_get", { account, key });
    } catch {
      return null;
    }
  })();
  // a rate-limited list call waits for this read, so the copy shows instead of its error
  diskPending.add(p);
  p.finally(() => diskPending.delete(p));
  return p;
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
  loadLinks();
  loadPlaylists(); // every open: the list on screen stays, and redraws only if a playlist changed
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
    setEl(group.querySelector(".status") || group.querySelector(".row-sub"), `Couldn't load ${what} — ${libReason(e)}`);
  } finally {
    if (live()) group.removeAttribute("aria-busy");
  }
}

/** A shelf group's skeleton: shown while it has nothing yet. */
const shelfSkeleton = (group) => () => {
  const shelf = group.querySelector(".albums");
  if (shelf.children.length) return;
  group.hidden = false;
  shelf.innerHTML = skeletonTiles(gridCols(shelf) * 2);
};

// A Library tab shows its whole group: tiles in a grid that wraps, never a sideways-scrolling strip.
const SHELVES = {
  topArtists: { list: () => topArtistList, el: "topArtists" },
  albums: { list: () => albumShelf(), el: "libAlbums" },
  following: { list: () => artistShelf(), el: "libFollowing" },
  mixes: { list: () => mixList, el: "libMixes" },
};

/** One shelf item's tile: artists round, albums and mixes square. */
function shelfTile(kind, it, i) {
  if (kind === "mixes") return tile(it, i, { attrs: ` data-mix="${esc(it.id)}"`, sub: it.source === "added" ? "Added" : "" });
  if (kind === "albums") return tile(it, i, { sub: esc(it.added ? `Added · ${it.artists || ""}` : it.artists) });
  return tile({ name: it.name, cover: it.image }, i, { round: true, sub: it.added ? "Added" : "" });
}

const shelfGrid = (kind) => {
  const el = $(SHELVES[kind].el);
  return el.classList.contains("albums") ? el : el.querySelector(".albums");
};

/** The columns of a laid-out grid; 4 while it isn't on screen. */
function gridCols(el) {
  const v = getComputedStyle(el).gridTemplateColumns;
  return /^[\d.]+px( [\d.]+px)*$/.test(v || "") ? v.split(" ").length : 4;
}

/** Render a shelf's tiles, all of them. */
function renderShelf(kind) {
  shelfGrid(kind).innerHTML = SHELVES[kind].list().map((it, i) => shelfTile(kind, it, i)).join("");
}

// ---------- Library tabs: one group at a time; a group that's empty or not allowed has no tab ----------

const LIB_TABS = { playlists: "libPlaylists", liked: "libLiked", albums: "libAlbums", following: "libFollowing", top: "libTop", mixes: "libMixes" };
const LIB_TAB_KEY = "libraryTab";

/** The tab on screen: the last one picked (stored), or Playlists while that one's group is hidden. */
function libTabShown() {
  const want = storeGet(LIB_TAB_KEY);
  return LIB_TABS[want] && !$(LIB_TABS[want]).hidden ? want : "playlists";
}

/** Tabs follow their groups (hidden with them); the current one is marked and its group shown. */
function renderLibTabs() {
  const shown = libTabShown();
  $("libAdd").hidden = shown !== "playlists" && shown !== "mixes";
  for (const b of $("libTabs").querySelectorAll("[data-tab]")) {
    const t = b.dataset.tab;
    const on = t === shown;
    if (b.hidden !== $(LIB_TABS[t]).hidden) b.hidden = $(LIB_TABS[t]).hidden;
    b.setAttribute("aria-selected", String(on));
    b.tabIndex = on ? 0 : -1;
    $(LIB_TABS[t]).classList.toggle("is-current", on);
  }
}

function selectLibTab(t) {
  if (!LIB_TABS[t] || t === libTabShown()) return;
  applog("info", `library: tab ${t}`);
  storeSet(LIB_TAB_KEY, t);
  renderLibTabs();
  $("sheetBody").scrollTop = 0;
  listScroll = 0;
  if (t === "liked") loadLikedRows();
}

/** Arrows move between the visible tabs and pick, like the time range tabs. */
function onLibTabKey(e) {
  const step = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
  if (!step || !e.target.closest("[data-tab]")) return;
  e.preventDefault();
  const tabs = [...$("libTabs").querySelectorAll("[data-tab]:not([hidden])")];
  const at = tabs.findIndex((b) => b.dataset.tab === libTabShown());
  const next = tabs[(at + step + tabs.length) % tabs.length];
  selectLibTab(next.dataset.tab);
  next.focus();
}

// ---------- Liked Songs: the tab lists them (up to Spotify's 20 pages); the count decides the tab ----------

let likedTracks = [];
let likedCount = null; // liked_count's answer, null = not known yet

function fillLiked() {
  const group = $("libLiked");
  if (likedCount !== 0) group.hidden = false; // a tab at once; it goes when the count says there's nothing
  libGet("likedCount", "liked_count").then(
    (n) => {
      likedCount = n || 0;
      group.hidden = !likedCount;
    },
    (e) => {
      if (overlayFailed(e)) return;
      if (isScopeError(e)) group.hidden = true; // no library scope: no tab
    },
  );
  if (libTabShown() === "liked") loadLikedRows();
}

/** The Liked Songs rows: the disk copy at once, or a skeleton, then the fresh list. */
function loadLikedRows() {
  const group = $("libLiked");
  const skeleton = () => {
    if (!likedTracks.length) $("likedRows").innerHTML = skeletonRows(8);
  };
  fillGroup(group, () => libGet("liked", "get_saved_tracks"), renderLiked, "your liked songs", "liked", skeleton);
}

function renderLiked(data) {
  likedTracks = listTracks(data).filter((t) => t && t.uri);
  const n = likedTracks.length;
  const total = Math.max((data && data.total) || 0, n);
  $("likedRows").innerHTML = likedTracks.map((t, i) => trackRow(t, i, { num: true, art: true })).join("");
  setText("likedNote", total > n ? `Showing your newest ${n} of ${total}` : "");
  setEl($("libLiked").querySelector(".status"), n ? "" : "No liked songs yet.");
}

let savedAlbums = [];

function fillAlbums() {
  const group = $("libAlbums");
  fillGroup(
    group,
    () => libGet("albums", "get_saved_albums"),
    (list) => {
      savedAlbums = (list || []).filter((a) => a && a.id);
      group.hidden = !albumShelf().length;
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
      group.hidden = !artistShelf().length;
      renderShelf("following");
    },
    "the artists you follow",
    "following",
    shelfSkeleton(group),
  );
}

// ---------- your top: 3 time ranges, each cached ----------

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
  topTrackList = tracks.filter((t) => t && t.uri);
  topArtistList = artists.filter((a) => a && a.id);
  group.hidden = false;
  $("topTracks").innerHTML = topTrackList.map((t, i) => trackRow(t, i, { num: true, art: true })).join("");
  $("topTracks").style.setProperty("--rows", String(Math.max(1, Math.ceil(topTrackList.length / 2)))); // 2 columns, ranked down
  $("topTracksHead").hidden = !topTrackList.length;
  $("topArtists").hidden = !topArtistList.length;
  $("topArtistsHead").hidden = !topArtistList.length;
  renderShelf("topArtists");
  const err = failed.find((e) => !isScopeError(e));
  const empty = !topTrackList.length && !topArtistList.length;
  setEl(group.querySelector(".status"), err ? `Couldn't load your top — ${libReason(err)}` : empty ? "Nothing here yet for this time range." : "");
}

function onTopTab(e) {
  const b = e.target.closest("[data-range]");
  if (!b || b.dataset.range === topRange) return;
  topRange = b.dataset.range;
  fillTop();
}

// ---------- Spotify mixes: Rust lists them (library.rs): the Made For You mixes on the user's Spotify home,
// the ones added by link, and the ones seen playing here (noted below, store key knownMixes) ----------

const MIXES_KEY = "knownMixes";
const MIX_NOTE = "Spotify didn't share this mix's track list. Play still starts it.";
let knownMixes = null; // [{id, seen}], newest first; null = not read from storage yet
let notedContext = null; // the last playback context noted, so a poll doesn't note it every second
const refusedMixes = new Set(); // mixes Spotify wouldn't start this session: history must not bring them back
const mixInfo = new Map(); // playlist id → promise of {name, cover} or null
let mixList = []; // the tiles on screen: {id, name, cover, source}
let mixesGen = 0;

function mixes() {
  if (!knownMixes) {
    const raw = storeGet(MIXES_KEY);
    knownMixes = noteMixes(Array.isArray(raw) ? raw : [], [], [], "");
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
  storeSet(MIXES_KEY, next);
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

/** The Mixes tab from Rust's mixes_list ({id, uri, name, cover, source}); refresh asks Spotify's home feed now. */
async function renderMixes(refresh = false) {
  const gen = ++mixesGen;
  let list;
  try {
    list = await invoke("mixes_list", { refresh });
  } catch (e) {
    if (gen === mixesGen && !overlayFailed(e)) applog("warn", `mixes_list failed: ${e}`);
    return; // the tiles on screen stay
  }
  if (gen !== mixesGen) return;
  const own = new Set(ownIds()); // a playlist of the user's own isn't a mix
  mixList = (list || []).filter((m) => m && m.id && !own.has(m.id)).map((m) => ({ id: m.id, name: m.name || "Spotify mix", cover: m.cover || null, source: m.source }));
  for (const m of mixList) if (!mixInfo.has(m.id)) mixInfo.set(m.id, Promise.resolve({ name: m.name, cover: m.cover }));
  $("libMixes").hidden = !mixList.length;
  renderShelf("mixes");
}

/** Spotify refuses some of its own mixes: 403, or a 404 that isn't about the device. */
// Rust already tags a device 404 as NO_ACTIVE_DEVICE: any other 403/404 is the mix itself
const mixRefused = (e) => /\b40[34]\b/.test(String(e)) && !isCode(e, "NO_ACTIVE_DEVICE");

async function playMix(src) {
  let refused = false;
  const played = startPlay(
    { contextUri: `spotify:playlist:${src.id}` },
    { kind: "mix", refused: (e) => mixRefused(e) && (refused = true) },
  );
  closeOverlay(); // the main screen: "Loading…" until the mix's first song shows
  await played;
  if (refused) {
    toast("Spotify won't start this mix from here");
    refusedMixes.add(src.id);
    noteContexts([]); // drops it from the stored list
    renderMixes();
  }
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
  $("topArtistsHead").hidden = true;
  $("topTracksHead").hidden = true;
  likedTracks = [];
  likedCount = null;
  $("likedRows").innerHTML = "";
  setText("likedNote", "");
}


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
    if (shown === null || JSON.stringify(next) !== shown) {
      playlists = next;
      renderPlaylists();
    }
  } catch (e) {
    fresh = true;
    if (overlayFailed(e)) return;
    if (shown === null) {
      list.innerHTML = "";
      setText("listStatus", `Couldn't load your playlists — ${libReason(e)}`);
    }
  } finally {
    playlistsLoading = false;
    list.removeAttribute("aria-busy");
  }
}

function renderPlaylists() {
  const added = addedIn("playlists", playlists);
  setText("listStatus", playlists.length || added.length ? "" : "No playlists yet.");
  $("libList").innerHTML =
    playlists
      .map(
        (p, i) =>
          `<button class="row row-playlist" type="button" data-id="${esc(p.id)}" data-i="${i}">` +
          `<span class="art row-art">${artHtml(pickImage(p.images), p.name)}</span>` +
          `<span class="row-text"><span class="row-title">${esc(p.name)}</span>` +
          `<span class="row-sub">${plural((p.tracks && p.tracks.total) || 0, "track", "tracks")}</span></span></button>`,
      )
      .join("") +
    // playlists added by link: someone else's, kept in the app (Rust savedLinks), marked "Added"
    added
      .map(
        (l, i) =>
          `<button class="row row-playlist is-added" type="button" data-added="${i}">` +
          `<span class="art row-art">${artHtml(l.cover, l.name)}</span>` +
          `<span class="row-text"><span class="row-title">${esc(l.name)}</span>` +
          `<span class="row-sub"><span class="added-mark">Added</span>${esc(l.owner ? ` · ${l.owner}` : "")}${l.total ? ` · ${plural(l.total, "track", "tracks")}` : ""}</span></span></button>`,
      )
      .join("");
  noteContexts([]); // own playlists noted before this load aren't mixes
  renderMixes();
}


// ---------- links added in the app: a pasted Spotify link kept in Rust's savedLinks (library.rs) ----------

let appLinks = []; // [{kind, id, uri, name, cover, owner, artists, total, tab}], newest first
const LINK_TAB_NAMES = { playlists: "Playlists", albums: "Albums", artists: "Artists", mixes: "Mixes" };
const LINK_LIB_TAB = { playlists: "playlists", albums: "albums", artists: "following", mixes: "mixes" }; // savedLinks tab → Library tab

/** Added links of one tab that the Spotify list (own) doesn't have already. */
const addedIn = (tab, own) => appLinks.filter((l) => l.tab === tab && !(own || []).some((x) => x && x.id === l.id));
const albumShelf = () => [...savedAlbums, ...addedIn("albums", savedAlbums).map((l) => ({ id: l.id, name: l.name, artists: l.artists, cover: l.cover, added: true }))];
const artistShelf = () => [...followed, ...addedIn("artists", followed).map((l) => ({ id: l.id, name: l.name, image: l.cover, added: true }))];

/** Read the added links and redraw what shows them. Also on `library-changed` (an add from MCP too). */
async function loadLinks() {
  try {
    const list = await invoke("links_list");
    appLinks = Array.isArray(list) ? list.filter((l) => l && l.uri && l.id) : [];
  } catch {
    return;
  }
  if (playlists) renderPlaylists(); // it redraws the mixes too
  else renderMixes();
  if (albumShelf().length) $("libAlbums").hidden = false;
  if (artistShelf().length) $("libFollowing").hidden = false;
  renderShelf("albums");
  renderShelf("following");
  if (curDetail) renderDetailSave();
}

/** A detail view for a link's info (link_resolve) or a stored link. */
function detailSrcOf(info) {
  if (info.kind === "album") return { kind: "album", id: info.id, name: info.name, cover: info.cover, sub: info.artists || "Album" };
  if (info.kind === "artist") return { kind: "artist", id: info.id, name: info.name, cover: info.cover, sub: "Artist" };
  const own = (playlists || []).find((p) => p.id === info.id);
  if (own) return { kind: "playlist", id: own.id, name: own.name, cover: pickImage(own.images), sub: plural((own.tracks && own.tracks.total) || 0, "track", "tracks"), snapshotId: own.snapshot_id || null };
  if (info.tab === "mixes") return { kind: "mix", id: info.id, name: info.name, cover: info.cover, sub: "Made by Spotify" };
  return { kind: "playlist", id: info.id, name: info.name, cover: info.cover, sub: info.owner ? `Playlist · ${info.owner}` : "Playlist" };
}

/** The uri a detail view can be added under, or null (Liked Songs). */
function detailUri(src) {
  const kind = src && { playlist: "playlist", mix: "playlist", album: "album", artist: "artist" }[src.kind];
  return kind && src.id ? `spotify:${kind}:${src.id}` : null;
}

const isMixId = (id) => /^37i9/.test(String(id || ""));

/** The detail head's Add/Remove button: shown for what isn't already in the user's Spotify library. */
function renderDetailSave() {
  const btn = $("detailSave");
  const src = curDetail;
  const uri = detailUri(src);
  const saved = Boolean(uri && appLinks.some((l) => l.uri === uri));
  const inSpotify =
    src &&
    ((src.kind === "playlist" && (playlists || []).some((p) => p.id === src.id)) ||
      (src.kind === "album" && savedAlbums.some((a) => a.id === src.id)) ||
      (src.kind === "artist" && followed.some((a) => a.id === src.id)));
  btn.hidden = !uri || (inSpotify && !saved);
  if (btn.hidden) return;
  const mix = src.kind === "mix" || (src.kind === "playlist" && isMixId(src.id));
  btn.textContent = saved ? (mix ? "Remove from Mixes" : "Remove from library") : mix ? "Save to Mixes" : "Add to library";
  btn.dataset.saved = saved ? "1" : "";
}

async function onDetailSave() {
  const btn = $("detailSave");
  const uri = detailUri(curDetail);
  if (!uri || btn.getAttribute("aria-busy")) return;
  btn.setAttribute("aria-busy", "true");
  btn.disabled = true;
  try {
    if (btn.dataset.saved) {
      await invoke("link_remove", { uri });
      toast("Removed from your library");
    } else {
      const r = await invoke("link_save", { link: uri });
      toast(r.already ? "Already in your library" : `Added to ${LINK_TAB_NAMES[r.item.tab] || "your library"}`);
    }
    applog("info", `library: ${btn.dataset.saved ? "removed" : "added"} ${uri}`);
    await loadLinks();
  } catch (e) {
    if (!overlayFailed(e)) toast(reason(e));
  } finally {
    btn.removeAttribute("aria-busy");
    btn.disabled = false;
    renderDetailSave();
  }
}

/** The Library's Add field: paste a link, Add fetches it (Rust) and keeps it in the app. */
function showAddForm(show) {
  $("libAddForm").hidden = !show;
  $("libAddBtn").hidden = show;
  $("libAddBtn").setAttribute("aria-expanded", String(show));
  setText("libAddMsg", "");
  if (show) {
    $("libAddInput").value = "";
    $("libAddInput").focus();
  } else {
    $("libAddBtn").focus();
  }
}

async function onAddSubmit(e) {
  e.preventDefault();
  const go = $("libAddGo");
  if (go.getAttribute("aria-busy")) return;
  const link = parseLink($("libAddInput").value);
  if (!link) return void setText("libAddMsg", "That link isn't a Spotify playlist, album or artist");
  if (link.kind === "track") return void setText("libAddMsg", "That's a song: paste it into Search to play it");
  setText("libAddMsg", "");
  go.setAttribute("aria-busy", "true");
  go.disabled = true;
  go.textContent = "Adding…";
  try {
    const r = await invoke("link_save", { link: link.uri });
    applog("info", `library: add ${link.uri}: ${r.already ? "already there" : "added"}`);
    await loadLinks();
    showAddForm(false);
    if (r.already) {
      toast("Already in your library");
      openDetail(detailSrcOf(r.item));
    } else {
      selectLibTab(LINK_LIB_TAB[r.item.tab]);
      toast(`Added to ${LINK_TAB_NAMES[r.item.tab] || "your library"}`);
    }
  } catch (err) {
    if (!overlayFailed(err)) setText("libAddMsg", reason(err));
  } finally {
    go.removeAttribute("aria-busy");
    go.disabled = false;
    go.textContent = "Add";
  }
}

/** Search got a Spotify link: a song plays, a playlist/album/artist opens. */
async function openLink(link, gen) {
  const box = $("searchResults");
  searchMessage("Opening the link…");
  box.setAttribute("aria-busy", "true");
  let info;
  try {
    info = await invoke("link_resolve", { link: link.uri });
  } catch (e) {
    if (gen !== state.gen.search) return;
    box.removeAttribute("aria-busy");
    if (!overlayFailed(e)) searchMessage(reason(e));
    return;
  }
  if (gen !== state.gen.search) return;
  box.removeAttribute("aria-busy");
  applog("info", `search: link ${link.uri} (${info.kind})`);
  if (info.kind === "track") return void playFrom([info.track], 0);
  openDetail(detailSrcOf(info));
}

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
    const fromStage = !state.overlay;
    if (fromPage) page.scroll = $("pageBody").scrollTop;
    openOverlay("library");
    $("sheet").focus();
    // from the main screen: Back returns there; from a full page: Back returns to it; from Search: the list
    navStack = fromPage ? [PAGE_ENTRY] : fromStage ? [STAGE_ENTRY] : [];
    if (!curDetail && !fromPage) listScroll = $("sheetBody").scrollTop;
    loadGroups(); // so Back has a list to show
  } else if (push && curDetail) {
    navStack.push(curDetail);
    // the oldest detail goes; the full page or the main screen under them all stays
    if (navStack.length > NAV_MAX) navStack.splice(navStack[0] === PAGE_ENTRY || navStack[0] === STAGE_ENTRY ? 1 : 0, 1);
  } else if (push) {
    listScroll = $("sheetBody").scrollTop;
    navStack = [];
  }
  curDetail = src;
  const gen = ++state.gen.detail;
  detailTracks = [];
  detailAlbums = [];
  $("libLevel1").hidden = true;
  $("libTitle").hidden = true;
  $("libTabs").hidden = true;
  $("libBack").hidden = false;
  $("libBack").querySelector(".back-label").textContent = navStack.length ? "Back" : "Library";
  $("libDetail").hidden = false;
  const cover = $("detailCover");
  cover.classList.toggle("is-round", src.kind === "artist");
  cover.classList.toggle("is-liked", src.kind === "liked");
  cover.innerHTML = src.kind === "liked" ? ICONS.heartFilled : artHtml(src.cover, src.name);
  $("detailName").textContent = src.name;
  setText("detailSub", src.sub);
  setText("detailNote", "");
  $("detailPlay").hidden = src.kind === "artist";
  $("detailPlay").disabled = src.kind !== "mix"; // a mix plays by its uri, its tracks loaded or not
  renderDetailSave();
  const rows = $("detailRows");
  setText("detailStatus", "");
  $("sheetBody").scrollTop = 0;
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
    if (src.kind === "mix") setText("detailNote", MIX_NOTE);
    else setText("detailStatus", `Couldn't load tracks — ${libReason(e)}`);
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
  if (src.kind === "mix" && n) setText("detailSub", `Made by Spotify · ${plural(n, "track", "tracks")}`);
  if (src.kind === "liked") {
    total = Math.max(total || 0, n);
    setText("detailSub", plural(total, "song", "songs"));
    setText("detailNote", total > n ? `Showing your newest ${n} of ${total}` : "");
  }
  if (src.kind === "album") setHtml($("detailSub"), albumArtistsHtml(src.sub, detailTracks));
  const empty = { album: "This album is empty.", liked: "No liked songs yet.", mix: "" }[src.kind] ?? "This playlist is empty.";
  setText("detailStatus", n ? "" : empty);
  if (src.kind === "mix") setText("detailNote", n ? "" : MIX_NOTE);
  $("detailRows").innerHTML = detailTracks.map((t, i) => trackRow(t, i, { num: true, art: src.kind !== "album" })).join("");
  $("detailPlay").disabled = src.kind !== "mix" && !detailTracks.some((t) => !isLocalFile(t.uri));
}

/**
 * An album head's artists as links: the names in sub ("A, B"), each with its id from the tracks'
 * artist lists (the album itself has only the joined names); a name with no known id stays text.
 */
function albumArtistsHtml(sub, tracks) {
  const ids = new Map();
  for (const t of tracks) for (const a of t.artist_list || []) if (a && a.id && a.name && !ids.has(a.name)) ids.set(a.name, a.id);
  const names = sub ? String(sub).split(", ") : ((tracks[0] && tracks[0].artist_list) || []).map((a) => a && a.name).filter(Boolean);
  return names.map((n) => (ids.has(n) ? `<button class="artist-link" type="button" data-artist="${esc(ids.get(n))}">${esc(n)}</button>` : esc(n))).join(", ");
}

/** A detail view as a play's origin: playlists and albums only. */
const originOf = (src) =>
  src && (src.kind === "playlist" || src.kind === "album" || src.kind === "mix") ? { kind: src.kind === "mix" ? "playlist" : src.kind, id: src.id } : null;

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

let artistTracksTitle = "Popular"; // the artist page's track list: Spotify's popular tracks, or your favorites

/**
 * The artist page: photo and name (best effort), albums and singles, then Spotify's popular tracks
 * (get_artist's top_tracks), or, when there are none (the Web API has none), your favorites by them.
 */
async function loadArtist(src, gen) {
  const optional = (e) => (isCode(e, "AUTH_EXPIRED") ? Promise.reject(e) : null); // the page works without these
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
    setText("detailStatus", `Couldn't load albums — ${libReason(e)}`);
    return;
  }
  if (gen !== state.gen.detail) return;
  if (info) {
    // kept on src: Back to this page shows them at once
    src.name = info.name || src.name;
    src.cover = info.image || src.cover;
    $("detailCover").innerHTML = artHtml(src.cover, src.name);
    $("detailName").textContent = src.name;
  }
  detailAlbums = (albums || []).filter((a) => a && a.id);
  const popular = ((info && info.top_tracks) || []).filter((t) => t && t.uri);
  detailTracks = popular;
  artistTracksTitle = "Popular";
  renderArtist();
  if (popular.length) return;
  // favorites come from your top tracks and Liked Songs: a cold Liked cache is many pages, so the
  // albums above don't wait for it
  const sources = favoriteSources(optional);
  let lists;
  try {
    lists = await sources;
  } catch (e) {
    return void (gen === state.gen.detail && overlayFailed(e));
  }
  if (gen !== state.gen.detail) return;
  // no popular tracks (the Web API no longer gives them): rank from your own listening
  detailTracks = favoritesBy(src.id, lists || []);
  artistTracksTitle = "Your favorites";
  renderArtist();
}

function renderArtist() {
  $("detailRows").removeAttribute("aria-busy");
  $("detailPlay").hidden = !detailTracks.length;
  $("detailPlay").disabled = !detailTracks.length;
  setText("detailStatus", detailAlbums.length || detailTracks.length ? "" : "No albums or singles.");
  const favorites = detailTracks.length
    ? `<h3 class="group-title">${esc(artistTracksTitle)}</h3><div class="rows">${detailTracks.map((t, i) => trackRow(t, i, { num: true, art: true })).join("")}</div>` +
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

/**
 * origin: the detail view it came from ({kind, id}) or null; name: the list's name, for the playlist
 * panel (Spotify names no context for a uris play); opts: as startPlay (row).
 */
async function playFrom(tracks, i, { name = null, origin = null, ...opts } = {}) {
  const t = tracks[i];
  if (!t || isLocalFile(t.uri)) return;
  // the whole list, so the panel shows what came before the clicked song too (one song is no list)
  if (tracks.length > 1) lastList = { tracks: tracks.filter((x) => x && x.uri), name };
  const ctx = originUri(origin);
  let src;
  if (ctx && offsettable(ctx)) {
    // a playlist/album plays as itself from the clicked song: nothing is cut, and Back has
    // the songs before it (a list from the clicked song on had none: Back did nothing)
    src = { contextUri: ctx, trackUri: t.uri };
  } else {
    // a list without a context: the songs around the clicked one, so Back works here too.
    // Spotify lists local files but rejects them in play requests; the cap is for Liked's 1000 rows
    const all = tracks.map((x) => x.uri).filter((u) => !isLocalFile(u));
    const at = all.indexOf(t.uri);
    const from = Math.max(0, Math.min(at - PLAY_URIS_BEFORE, all.length - PLAY_URIS_MAX));
    src = { uris: all.slice(from, from + PLAY_URIS_MAX), trackUri: t.uri };
  }
  const played = startPlay(src, { kind: "list", ...opts });
  // the main screen shows the song at once (its cover, title and loaders); a failure says so in a toast
  closeOverlay();
  await played;
}

const PLAY_URIS_BEFORE = 50; // songs kept before the clicked one in a uris play

const playDetailFrom = (i, row = null) => playFrom(detailTracks, i, { origin: originOf(curDetail), row, name: curDetail && curDetail.name });

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
  const gen = ++state.gen.search;
  const q = $("searchInput").value.trim();
  const link = parseLink(q);
  if (link) return void openLink(link, gen);
  if (looksLikeLink(q)) return void searchMessage("Needle opens Spotify links to playlists, albums, artists and songs. This one isn't one of those.");
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
    if (!overlayFailed(e)) searchMessage(`Search failed — ${libReason(e)}`);
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

// ---------- the full page: "See all" from Search ----------

const FIRST_PAGES = 3; // 30 results up front, then 10 per "Load more"
const PAGE_ENTRY = { kind: "page" }; // in the detail Back stack: Back returns to the full page
const STAGE_ENTRY = { kind: "stage" }; // in the detail Back stack: Back closes the Library, back to the main screen

const page = {
  kind: null, // "search" while a page is open
  query: "",
  tab: "track", // "track" | "album"
  lists: {}, // tab → {items, next, hasMore, loading, error, started}
  opener: null, // the data-see of the "See all" that opened it: Back puts focus there
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
  Object.assign(page, { kind: "search", query: searchHits.q, tab, opener: tab });
  page.returnScroll = $("searchResults").scrollTop;
  // the palette asked for 10 of each: a full 10 means Spotify has more
  page.lists = {
    track: newList({ items: searchHits.tracks, full: searchHits.tracks.length >= PAGE_SIZE }),
    album: newList({ items: searchHits.albums, full: searchHits.albums.length >= PAGE_SIZE }),
  };
  showPage();
}

function showPage() {
  state.gen.page++; // loads of an earlier page must not land here
  openOverlay("browse");
  page.scroll = 0;
  renderPage();
  $("pageBody").scrollTop = 0;
  $("page").focus();
  ensureLoaded(page.tab);
}

/** Back to the page from a detail opened on it: as it was. Its loads kept landing meanwhile. */
function returnToPage() {
  showList(); // the Library sheet underneath goes back to its list
  openOverlay("browse", true);
  renderPage();
  $("pageBody").scrollTop = page.scroll;
  $("page").focus({ preventScroll: true });
}

/** Back (or Esc): to the search palette with its query and results. */
function pageBack() {
  state.gen.page++;
  openOverlay("search", true);
  $("searchResults").scrollTop = page.returnScroll;
  const see = $("searchResults").querySelector(`[data-see="${page.opener}"]`);
  (see && !see.hidden ? see : $("searchInput")).focus({ preventScroll: true });
}

/** What the page lists now: {kind: "track" | "album", items, list}. */
function pageView() {
  const list = page.lists[page.tab];
  return { kind: page.tab, items: list.items, list };
}

function pageItemHtml(kind, it, i) {
  if (kind === "track") return trackRow(it, i, { num: true, art: true });
  return tile(it, i, { sub: esc(it.artists) });
}

/** Skeletons for pages on their way: a full screen at first, a few under the list after. */
const pageSkeleton = (kind, empty) => (kind === "track" ? skeletonRows(empty ? 10 : 4) : skeletonTiles(empty ? 12 : 6));

function renderPage() {
  $("pageBack").querySelector(".back-label").textContent = "Search";
  $("pageBack").setAttribute("aria-label", "Back to search");
  setText("pageKicker", "Search results for");
  $("pageTitle").textContent = `“${page.query}”`;
  $("pageTabs").hidden = false;
  for (const b of $("pageTabs").querySelectorAll("[data-tab]")) {
    const on = b.dataset.tab === page.tab;
    b.setAttribute("aria-selected", String(on));
    b.tabIndex = on ? 0 : -1;
  }
  $("pageList").setAttribute("aria-labelledby", page.tab === "track" ? "pageTabTrack" : "pageTabAlbum");
  renderPageList();
}

function renderPageList() {
  const { kind, items, list } = pageView();
  const el = $("pageList");
  el.className = `page-list ${kind === "track" ? "rows" : "albums page-grid"}`;
  el.innerHTML = items.map((it, i) => pageItemHtml(kind, it, i)).join("") + (list.loading ? pageSkeleton(kind, !items.length) : "");
  if (list.loading && !items.length) el.setAttribute("aria-busy", "true");
  else el.removeAttribute("aria-busy");
  renderPageFoot();
}

/** The line under the list (empty, failed) and the Load more button. */
function renderPageFoot() {
  const { kind, items, list } = pageView();
  const n = items.length;
  const more = $("pageMore");
  setText("pageSub", "");
  const noun = kind === "track" ? "songs" : "albums";
  let status = "";
  if (list.error) status = n ? `Couldn't load more — ${libReason(list.error)}` : `Couldn't load ${noun} — ${libReason(list.error)}`;
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
  loadPages(page.tab, 1);
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
  if (it) openAlbum(it);
}

// ---------- keyboard: Space = play/pause, Esc = close the overlay ----------

const typing = (t) => t && t.closest && t.closest("input, textarea, select, [contenteditable]");

function onKey(e) {
  // Esc closes the innermost thing: a popover first, then a full page (back), then the overlay
  if (e.key === "Escape" && e.type === "keydown" && (devicesOpen || volumeOpen || panelOpen || settingsOpen || state.overlay)) {
    e.preventDefault();
    if (devicesOpen) closeDevices(true);
    else if (volumeOpen) closeVolume(true);
    else if (panelOpen) closePanel(true);
    else if (settingsOpen) closeSettings(true);
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
  $("stage").classList.toggle("is-solo", soloRun());
  renderNow();
  renderChrome();
  // first: a rate limit from the last run (no Web API calls), and this Mac's player state (no
  // playback_state needed when it plays here); then the loop
  const sess = authSession;
  Promise.all([loadQuota(), loadLocal()]).finally(() => sess === authSession && !$("stage").hidden && startPolling());
  refreshEngine();
  accountId(); // the list cache is per account: ask once, early
  loadRestored();
}

async function boot() {
  activeBg = $("bgA");
  setLayer($("bgA"), fallbackVars(FALLBACK));
  setLayer($("bgB"), fallbackVars(FALLBACK));

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
  $("settingsBtn").addEventListener("click", toggleSettings);
  $("dockArtSwitch").addEventListener("click", toggleDockArt);
  $("coverRowSwitch").addEventListener("click", toggleCoverRow);
  $("qualityOpts").addEventListener("click", onQualityClick);
  $("qualityOpts").addEventListener("keydown", onQualityKey);
  $("mcpSwitch").addEventListener("click", toggleMcp);
  $("mcpActions").addEventListener("click", (e) => {
    const b = e.target.closest("[data-copy]");
    if (b) copyMcp(b.dataset.copy);
  });
  $("mcpReset").addEventListener("click", () => askResetKey(true));
  $("mcpResetNo").addEventListener("click", () => askResetKey(false));
  $("mcpResetYes").addEventListener("click", resetMcpKey);
  $("panelBtn").addEventListener("click", togglePanel);
  $("panelList").addEventListener("click", onPanelClick);
  $("panel").addEventListener("error", onImgError, true);
  $("panelLayer").addEventListener("click", (e) => e.target.closest("[data-close]") && closePanel(true));
  document.addEventListener("pointerdown", onOutside);
  // focus rings only for the keyboard: a click must not leave a ring (WebKit can match :focus-visible on one)
  document.addEventListener("pointerdown", () => (document.documentElement.dataset.input = "pointer"), true);
  document.addEventListener("keydown", () => (document.documentElement.dataset.input = "key"), true);

  $("libraryBtn").addEventListener("click", openLibrary);
  $("searchBtn").addEventListener("click", openSearch);
  for (const id of ["library", "search", "browse"]) {
    $(id).addEventListener("click", (e) => e.target.closest("[data-close]") && closeOverlay());
    $(id).addEventListener("error", onImgError, true);
  }
  $("libBack").addEventListener("click", goBack);
  $("libList").addEventListener("click", (e) => {
    const added = e.target.closest("[data-added]");
    const l = added && addedIn("playlists", playlists || [])[Number(added.dataset.added)];
    if (l) return void openDetail(detailSrcOf(l));
    const row = e.target.closest("[data-id]");
    const p = row && playlists && playlists[Number(row.dataset.i)];
    if (p) {
      const sub = plural((p.tracks && p.tracks.total) || 0, "track", "tracks");
      openDetail({ kind: "playlist", id: p.id, name: p.name, cover: pickImage(p.images), sub, snapshotId: p.snapshot_id || null });
    }
  });
  $("libTabs").addEventListener("click", (e) => {
    const b = e.target.closest("[data-tab]");
    if (b) selectLibTab(b.dataset.tab);
  });
  $("libTabs").addEventListener("keydown", onLibTabKey);
  // a group shown or hidden (loaded, empty, not allowed): its tab follows
  const groupsSeen = new MutationObserver(renderLibTabs);
  for (const id of Object.values(LIB_TABS)) groupsSeen.observe($(id), { attributes: true, attributeFilter: ["hidden"] });
  $("likedRows").addEventListener("click", (e) => onTrackClick(e, likedTracks, (i, row) => playFrom(likedTracks, i, { row, name: "Liked Songs" })));
  $("topTabs").addEventListener("click", onTopTab);
  $("topTracks").addEventListener("click", (e) => onTrackClick(e, topTrackList, (i, row) => playFrom(topTrackList, i, { row, name: "Your top songs" })));
  $("topArtists").addEventListener("click", (e) => openArtistTile(tileAt(e, topArtistList)));
  $("libFollowing").addEventListener("click", (e) => openArtistTile(tileAt(e, artistShelf())));
  $("libAlbums").addEventListener("click", (e) => {
    const a = tileAt(e, albumShelf());
    if (a) openAlbum(a);
  });
  $("libMixes").addEventListener("click", (e) => openMix(tileAt(e, mixList)));
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
  $("detailSub").addEventListener("click", (e) => {
    const link = e.target.closest(".artist-link");
    if (link) openArtist(link);
  });
  $("detailPlay").addEventListener("click", onDetailPlay);
  $("detailSave").addEventListener("click", onDetailSave);
  $("libAddBtn").addEventListener("click", () => showAddForm(true));
  $("libAddCancel").addEventListener("click", () => showAddForm(false));
  $("libAddForm").addEventListener("submit", onAddSubmit);
  $("nowArtist").addEventListener("click", (e) => {
    const link = e.target.closest(".artist-link");
    if (link) openArtist(link);
  });
  $("searchInput").addEventListener("input", onSearchInput);
  $("searchResults").addEventListener("click", onSearchClick);
  document.addEventListener("keydown", onKey);
  document.addEventListener("keyup", onKey);
  addEventListener("resize", placeRun);
  small.addEventListener("change", () => {
    closeVolume(); // the slider popover exists only on narrow screens
    renderVolume();
  });
  $("run").addEventListener("error", onImgError, true);
  $("run").addEventListener("click", onRunClick);
  $("run").addEventListener("pointerdown", runDown);
  $("run").addEventListener("pointermove", runMove);
  $("run").addEventListener("pointerup", runUp);
  $("run").addEventListener("pointercancel", runUp);
  $("run").addEventListener("wheel", runWheel, { passive: false });
  // dev harness only: the `artist` scenario opens a page by id
  if (window.__mock) window.__openArtist = (id) => openDetail({ kind: "artist", id, name: "", cover: null, sub: "Artist" });
  document.addEventListener("visibilitychange", () => {
    if ($("stage").hidden) return;
    // hidden keeps polling, slower (pollDelay); visible again restarts with a fresh poll
    if (!document.hidden) startPolling();
  });
  listenEvent("engine-status", setEngine);
  // Rust loaded the saved session back (paused): show it, and let a poll pick it up now
  listenEvent("session-restored", (p) => {
    if ($("stage").hidden) return;
    applog("info", `session restored: ${p && p.trackUri}`);
    noteRestored(p);
    kick();
  });
  listenEvent("media-command", onMediaCommand);
  // this Mac's player: what plays, from librespot (no Web API); the loop renders from it
  listenEvent("player-state", onPlayerState);
  // an add or remove in the app's own library (also from MCP): the Library redraws
  listenEvent("library-changed", () => loadLinks());

  await loadStore(); // settings and known mixes are read from it
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
