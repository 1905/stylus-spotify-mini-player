// The menu-bar mini player (Rust tray.rs). A view only: the main window sends what it shows
// (mini-state, src/lib/mini.js miniPayload); every button goes back to it as mini_command, so
// routing and spinners are the main window's own.
import { ICONS } from "./lib/icons.js";
import { miniProgress, volumeIcon } from "./lib/mini.js";
import { stepVolume } from "./lib/transport.js";

const $ = (id) => document.getElementById(id);
const invoke = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);

let s = null; // the last mini-state, null = none yet
let drag = null; // a volume drag: its pointer id
let framing = false;
let coverUrl = null;

const send = (action, value) => invoke("mini_command", value === undefined ? { action } : { action, value }).catch(() => {});

/** The button's icon (icons.js name), swapped only when it changes. */
function setIcon(el, name) {
  if (el.dataset.icon === name) return;
  el.dataset.icon = name;
  el.innerHTML = ICONS[name];
}

function setText(el, text) {
  if (el.textContent !== text) el.textContent = text;
}

function render() {
  const st = s || { mode: "idle", status: "Nothing playing" };
  const song = Boolean(st.title);
  $("mini").dataset.mode = st.mode;
  $("mini").classList.toggle("is-playing", st.playing);
  $("mini").classList.toggle("is-loading", st.loading || Boolean(st.skipping));

  setText($("miniTitle"), song ? st.title : st.loading ? "" : st.status || "Nothing playing");
  setText($("miniArtist"), song ? st.artist || "" : "");
  $("miniArtist").hidden = !song;
  $("miniDevice").hidden = !st.device;
  setText($("miniDevice"), st.device ? `On ${st.device}` : "");
  $("miniOpen").setAttribute("aria-label", song ? `${st.title}, ${st.artist || ""}. Show Stylus` : "Show Stylus");
  renderCover(st.cover);

  const play = $("miniPlay");
  setIcon(play, st.playing ? "pause" : "play");
  play.setAttribute("aria-label", st.playing ? "Pause" : "Play");
  play.disabled = st.mode === "idle";
  play.classList.toggle("is-pending", st.pending);
  if (st.pending) play.setAttribute("aria-busy", "true");
  else play.removeAttribute("aria-busy");
  for (const [id, dir] of [["miniPrev", "previous"], ["miniNext", "next"]]) {
    const b = $(id);
    b.disabled = st.mode !== "track";
    const busy = st.skipping === dir;
    b.classList.toggle("is-pending", busy);
    if (busy) b.setAttribute("aria-busy", "true");
    else b.removeAttribute("aria-busy");
  }

  const heart = $("miniHeart");
  heart.hidden = st.heart == null;
  heart.disabled = st.heart === "unknown";
  const saved = st.heart === true;
  setIcon(heart, saved ? "heartFilled" : "heart");
  heart.classList.toggle("is-on", saved);
  heart.setAttribute("aria-pressed", String(saved));
  heart.setAttribute("aria-label", saved ? "Remove from Liked Songs" : "Save to Liked Songs");

  $("miniVol").hidden = st.volume == null;
  if (st.volume != null && drag == null) showVolume(st.volume);

  renderBar();
  if (st.playing && !framing) {
    framing = true;
    requestAnimationFrame(frame);
  }
}

function renderCover(url) {
  if (url === coverUrl) return;
  coverUrl = url || null;
  const img = $("miniCover");
  img.hidden = !coverUrl;
  $("miniGlyph").hidden = Boolean(coverUrl);
  if (coverUrl) img.src = coverUrl;
  else img.removeAttribute("src");
  $("miniBg").style.backgroundImage = coverUrl ? `url("${coverUrl.replace(/["\\]/g, "")}")` : "";
}

function renderBar() {
  $("miniFill").style.transform = `scaleX(${miniProgress(s, Date.now())})`;
}

function frame() {
  if (!s || !s.playing) {
    framing = false;
    return;
  }
  renderBar();
  requestAnimationFrame(frame);
}

function showVolume(v) {
  $("miniVolFill").style.width = `${v}%`;
  const slider = $("miniSlider");
  slider.setAttribute("aria-valuenow", String(v));
  slider.setAttribute("aria-valuetext", `${v}%`);
  const icon = volumeIcon(v);
  const mute = $("miniMute");
  setIcon(mute, icon);
  mute.setAttribute("aria-label", v === 0 ? "Unmute" : "Mute");
}

/** A level from the user: shown at once, sent to the main window (which coalesces the burst). */
function setVolume(v) {
  const pct = Math.round(Math.min(100, Math.max(0, v)));
  showVolume(pct);
  if (s) s.volume = pct;
  send("volume", pct);
}

function volumeAt(ev) {
  const r = $("miniSlider").querySelector(".mini-slider-track").getBoundingClientRect();
  return ((ev.clientX - r.left) / r.width) * 100;
}

function onKey(ev) {
  if (ev.key === "Escape") {
    ev.preventDefault();
    invoke("mini_hide").catch(() => {});
    return;
  }
  // the main window's slider keys: arrows step, Home and End go to 0 and 100
  const v = ev.target === $("miniSlider") && s && s.volume != null ? stepVolume(s.volume, ev.key) : null;
  if (v !== null) {
    ev.preventDefault();
    setVolume(v);
    return;
  }
  // space anywhere but on a button plays or pauses, as in the main window
  if (ev.key === " " && !ev.target.closest("button") && s && s.mode !== "idle") {
    ev.preventDefault();
    send("toggle");
  }
}

function boot() {
  setIcon($("miniGlyph"), "library");
  setIcon($("miniPrev"), "previous");
  setIcon($("miniNext"), "next");
  $("miniOpen").addEventListener("click", () => send("show"));
  $("miniPlay").addEventListener("click", () => send("toggle"));
  $("miniPrev").addEventListener("click", () => send("previous"));
  $("miniNext").addEventListener("click", () => send("next"));
  $("miniHeart").addEventListener("click", () => send("heart"));
  $("miniMute").addEventListener("click", () => send("mute"));
  const slider = $("miniSlider");
  slider.addEventListener("pointerdown", (ev) => {
    drag = ev.pointerId;
    slider.setPointerCapture(ev.pointerId);
    slider.classList.add("is-dragging");
    setVolume(volumeAt(ev));
  });
  // a drag moves the level only while the button is down: a lost pointerup (the popover hid
  // mid-drag) must not leave hovering over the slider changing the volume (the mouse's id is reused)
  slider.addEventListener("pointermove", (ev) => {
    if (drag !== ev.pointerId) return;
    if (ev.buttons & 1) setVolume(volumeAt(ev));
    else endDrag();
  });
  const endDrag = () => {
    drag = null;
    slider.classList.remove("is-dragging");
  };
  const end = (ev) => drag === ev.pointerId && endDrag();
  slider.addEventListener("pointerup", end);
  slider.addEventListener("pointercancel", end);
  slider.addEventListener("lostpointercapture", end);
  window.addEventListener("blur", endDrag);
  $("miniCover").addEventListener("error", () => renderCover(null));
  document.addEventListener("keydown", onKey);
  // no focus ring after a click: only keyboard focus shows one (:focus-visible)
  document.addEventListener("mousedown", (ev) => ev.target.closest("button") && ev.preventDefault());

  window.__TAURI__.event.listen("mini-state", (e) => {
    s = e.payload || null;
    render();
  });
  invoke("mini_get")
    .then((v) => {
      if (v && !s) {
        s = v;
        render();
      }
    })
    .catch(() => {});
  render();
}

boot();
