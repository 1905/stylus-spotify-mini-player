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

  if (!s || !s.active) {
    const hadNow = Boolean(state.now);
    if (hadNow) observe(state.now);
    state.now = null;
    state.isPlaying = false;
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
  if (state.error && !state.loaded) {
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
  stage.classList.toggle("is-playing", state.isPlaying && !idle);
  stage.classList.toggle("is-idle", idle);
  $("playBtn").setAttribute("aria-label", state.isPlaying ? "Pause" : "Play");
  for (const id of ["prevBtn", "playBtn", "nextBtn"]) $(id).disabled = idle;

  const noDevice = state.devices && state.devices.length === 0 && idle;
  $("libraryBtn").classList.toggle("is-primary", idle && !noDevice && state.loaded);

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

async function togglePlay() {
  if (!state.now) return;
  const was = state.isPlaying;
  state.progressMs = progress();
  state.progressAt = performance.now();
  state.isPlaying = !was;
  state.holdUntil = performance.now() + HOLD_MS;
  renderChrome();
  const ok = await withDevice(async (id) => {
    if (was) return invoke("pause");
    const deviceId = id || (await discover())?.id;
    if (!deviceId) throw "NO_ACTIVE_DEVICE: no device";
    return invoke("resume", { deviceId });
  });
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

async function seekTo(ev) {
  if (!state.now || !state.now.duration_ms) return;
  const r = $("scrub").getBoundingClientRect();
  const ratio = Math.min(1, Math.max(0, (ev.clientX - r.left) / r.width));
  const positionMs = Math.round(ratio * state.now.duration_ms);
  state.progressMs = positionMs;
  state.progressAt = performance.now();
  renderProgress();
  await withDevice(() => invoke("seek", { positionMs }));
  kick();
}

// ---------- keyboard: Space = play/pause ----------

const typing = (t) => t && t.closest && t.closest("input, textarea, select, [contenteditable]");

function onKey(e) {
  if (e.code !== "Space" || $("stage").hidden || typing(e.target)) return;
  e.preventDefault(); // also stops a focused button from activating
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
  $("scrub").addEventListener("click", seekTo);
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
