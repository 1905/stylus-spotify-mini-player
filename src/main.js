const { invoke } = window.__TAURI__.core;

/* ---------- tiny helpers ---------- */
const el = (id) => document.getElementById(id);
const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
  );
const fmt = (ms) => {
  const s = Math.round((ms || 0) / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};
const PH =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='80' height='80'%3E%3Crect width='80' height='80' fill='%2317171a'/%3E%3C/svg%3E";

/* ---------- app state ---------- */
const state = {
  deviceId: null,
  deviceName: null,
  playlists: [],
  // The queue we own: the list we handed to Spotify + index.
  context: [], // array of track objects currently loaded
  index: -1, // position in context
  history: [], // played tracks, most-recent first
  current: null, // currently playing track object
  paused: true,
  positionMs: 0,
  durationMs: 0,
  ticker: null,
};

/* ---------- view switching ---------- */
function showApp(name) {
  el("login").classList.add("hidden");
  el("app").classList.remove("hidden");
  for (const v of ["search", "library", "detail"]) {
    el(`view-${v}`).classList.toggle("hidden", v !== name);
  }
  el("navSearch").classList.toggle("active", name === "search");
  el("navLibrary").classList.toggle("active", name === "library");
}

/* ============================================================
   SEARCH
   ============================================================ */
let searchTimer = null;
el("searchInput").addEventListener("input", (e) => {
  clearTimeout(searchTimer);
  const q = e.target.value.trim();
  if (!q) {
    renderSearchIdle();
    return;
  }
  searchTimer = setTimeout(() => runSearch(q), 280);
});

/* Empty search isn't a dead screen — show the library as a jumping-off point. */
function renderSearchIdle() {
  const box = el("searchResults");
  if (!state.playlists.length) {
    box.innerHTML = '<div class="empty">Search for a song or album.</div>';
    return;
  }
  box.innerHTML = `<div class="group-title">Your playlists</div><div class="grid" id="idleGrid"></div>`;
  const grid = box.querySelector("#idleGrid");
  state.playlists.forEach((pl) => {
    const card = makeCard(pl.images?.[0]?.url, pl.name, `${pl.tracks?.total ?? 0} tracks`);
    card.addEventListener("click", () => openPlaylist(pl));
    grid.appendChild(card);
  });
}

async function runSearch(q) {
  try {
    const { tracks, albums } = await invoke("search", { query: q });
    renderSearch(tracks, albums);
  } catch (e) {
    el("searchResults").innerHTML = `<div class="error">${esc(e)}</div>`;
  }
}

function renderSearch(tracks, albums) {
  const box = el("searchResults");
  box.innerHTML = "";

  if (albums.length) {
    const g = document.createElement("div");
    g.className = "result-group";
    g.innerHTML = `<div class="group-title">Albums</div><div class="grid" id="albGrid"></div>`;
    box.appendChild(g);
    const grid = g.querySelector("#albGrid");
    albums.forEach((a) => {
      const card = makeCard(a.cover, a.name, a.artists + (a.year ? " · " + a.year : ""));
      card.addEventListener("click", () => openAlbum(a));
      grid.appendChild(card);
    });
  }

  if (tracks.length) {
    const g = document.createElement("div");
    g.className = "result-group";
    g.innerHTML = `<div class="group-title">Songs</div><div class="tracklist" id="trkList"></div>`;
    box.appendChild(g);
    const list = g.querySelector("#trkList");
    tracks.forEach((t, i) => list.appendChild(trackRow(t, i, tracks)));
  }

  if (!albums.length && !tracks.length) {
    box.innerHTML = '<div class="empty">No results.</div>';
  }
}

/* A square cover card with a hover play button. */
function makeCard(cover, name, sub) {
  const card = document.createElement("div");
  card.className = "card";
  card.innerHTML = `
    <div class="card-art">
      <img src="${cover || PH}" alt="" loading="lazy"/>
      <div class="card-play"><svg viewBox="0 0 24 24"><polygon points="6 4 20 12 6 20 6 4"/></svg></div>
    </div>
    <div class="card-name">${esc(name)}</div>
    <div class="card-sub">${esc(sub)}</div>`;
  return card;
}

/* ============================================================
   TRACK ROW  (shared by search, playlist, album)
   ============================================================ */
function trackRow(t, i, context) {
  const row = document.createElement("div");
  row.className = "track-row";
  row.dataset.uri = t.uri;
  row.innerHTML = `
    <span class="tr-num">${i + 1}</span>
    <img class="tr-art" src="${t.cover || PH}" alt="" loading="lazy"/>
    <span class="tr-name">${esc(t.name)}</span>
    <span class="tr-artist">${esc(t.artists)}</span>
    <span class="tr-dur">${fmt(t.duration_ms)}</span>`;
  row.addEventListener("click", () => playFrom(context, i));
  return row;
}

/* ============================================================
   LIBRARY (playlists)
   ============================================================ */
async function loadLibrary() {
  if (!state.playlists.length) {
    try {
      state.playlists = await invoke("get_playlists");
      if (el("view-search").classList.contains("hidden") === false) renderSearchIdle();
    } catch (e) {
      el("playlistGrid").innerHTML = `<div class="error">${esc(e)}</div>`;
      return;
    }
  }
  const grid = el("playlistGrid");
  grid.innerHTML = "";
  state.playlists.forEach((pl) => {
    const card = makeCard(pl.images?.[0]?.url, pl.name, `${pl.tracks?.total ?? 0} tracks`);
    card.addEventListener("click", () => openPlaylist(pl));
    grid.appendChild(card);
  });
}

/* ============================================================
   DETAIL (playlist or album)
   ============================================================ */
async function openPlaylist(pl) {
  showDetail("Playlist", pl.name, pl.images?.[0]?.url, `${pl.tracks?.total ?? 0} tracks`);
  try {
    const tracks = await invoke("get_playlist_tracks", { playlistId: pl.id });
    fillTrackList(tracks);
  } catch (e) {
    el("trackList").innerHTML = `<div class="error">${esc(e)}</div>`;
  }
}

async function openAlbum(a) {
  showDetail("Album", a.name, a.cover, esc(a.artists));
  try {
    const tracks = await invoke("get_album_tracks", { albumId: a.id });
    fillTrackList(tracks);
  } catch (e) {
    el("trackList").innerHTML = `<div class="error">${esc(e)}</div>`;
  }
}

let detailContext = [];
function showDetail(kind, name, cover, sub) {
  el("dKind").textContent = kind;
  el("dName").textContent = name;
  el("dSub").textContent = sub || "";
  el("dCover").src = cover || PH;
  el("trackList").innerHTML = '<div class="empty">Loading…</div>';
  showApp("detail");
}

function fillTrackList(tracks) {
  detailContext = tracks;
  const list = el("trackList");
  list.innerHTML = "";
  if (!tracks.length) {
    list.innerHTML = '<div class="empty">No tracks.</div>';
    return;
  }
  tracks.forEach((t, i) => list.appendChild(trackRow(t, i, tracks)));
  el("playAllBtn").onclick = () => playFrom(tracks, 0);
  markPlayingRow();
}

/* ============================================================
   PLAYBACK
   ============================================================ */
async function playFrom(context, index) {
  const device = await ensureDevice();
  if (!device) return;
  state.context = context;
  state.index = index;
  const uris = context.slice(index).map((t) => t.uri).filter(Boolean);
  try {
    await invoke("play_on_device", { deviceId: device, uris });
    renderQueue();
    startPolling();
  } catch (e) {
    flashDevice("play failed");
    banner("Couldn't start playback: " + e);
    console.error(e);
  }
}

function renderQueue() {
  // Up next = everything after current index in context.
  const q = el("queueList");
  const upcoming = state.context.slice(state.index + 1, state.index + 21);
  if (!upcoming.length) {
    q.innerHTML = '<div class="side-empty">Nothing queued.</div>';
  } else {
    q.innerHTML = "";
    upcoming.forEach((t) => q.appendChild(sideRow(t, false)));
  }
  renderHistory();
}

function renderHistory() {
  const h = el("historyList");
  if (!state.history.length) {
    h.innerHTML = '<div class="side-empty">No history yet.</div>';
    return;
  }
  h.innerHTML = "";
  state.history.slice(0, 20).forEach((t) => h.appendChild(sideRow(t, true)));
}

function sideRow(t, past) {
  const row = document.createElement("div");
  row.className = "side-row" + (past ? " past" : "");
  row.innerHTML = `
    <img src="${t.cover || PH}" alt="" loading="lazy"/>
    <div class="side-text">
      <div class="s-name">${esc(t.name)}</div>
      <div class="s-artist">${esc(t.artists)}</div>
    </div>`;
  return row;
}

function markPlayingRow() {
  document.querySelectorAll(".track-row").forEach((r) => {
    r.classList.toggle("playing", state.current && r.dataset.uri === state.current.uri);
  });
}

/* ---------- now-playing bar ---------- */
function renderNowPlaying() {
  const t = state.current;
  const cover = t?.cover || PH;
  if (el("npCover").src !== cover) {
    el("npCover").src = cover;
    if (t?.cover) extractAccent(t.cover);
  }
  el("npTitle").textContent = t?.name || "Nothing playing";
  el("npArtist").textContent = t?.artists || "";
  el("durTime").textContent = fmt(state.durationMs);
  el("playIco").classList.toggle("hidden", !state.paused);
  el("pauseIco").classList.toggle("hidden", state.paused);
  markPlayingRow();
}

/* Pull a vivid dominant color from the album art and set it as the live
   accent — the app's ambient color follows the music. */
const accentImg = new Image();
accentImg.crossOrigin = "anonymous";
function extractAccent(url) {
  accentImg.onload = () => {
    try {
      const n = 16;
      const c = document.createElement("canvas");
      c.width = c.height = n;
      const ctx = c.getContext("2d", { willReadFrequently: true });
      ctx.drawImage(accentImg, 0, 0, n, n);
      const d = ctx.getImageData(0, 0, n, n).data;
      // Pick the most saturated, mid-bright pixel — avoids muddy averages.
      let best = [139, 124, 240], bestScore = -1;
      for (let i = 0; i < d.length; i += 4) {
        const r = d[i], g = d[i + 1], b = d[i + 2];
        const mx = Math.max(r, g, b), mn = Math.min(r, g, b);
        const sat = mx === 0 ? 0 : (mx - mn) / mx;
        const lum = (mx + mn) / 2 / 255;
        const score = sat * (1 - Math.abs(lum - 0.55)); // vivid, not too dark/light
        if (score > bestScore) { bestScore = score; best = [r, g, b]; }
      }
      setAccent(best);
    } catch {
      /* canvas tainted / CORS — keep current accent */
    }
  };
  accentImg.src = url;
}

function setAccent([r, g, b]) {
  const root = document.documentElement.style;
  root.setProperty("--accent", `rgb(${r}, ${g}, ${b})`);
  root.setProperty("--accent-soft", `rgba(${r}, ${g}, ${b}, 0.14)`);
  root.setProperty("--accent-glow", `rgba(${r}, ${g}, ${b}, 0.26)`);
}

function tickProgress() {
  el("curTime").textContent = fmt(state.positionMs);
  const pct = state.durationMs ? (state.positionMs / state.durationMs) * 100 : 0;
  el("scrubFill").style.width = `${Math.min(100, pct)}%`;
}

/* ============================================================
   SPOTIFY CONNECT — control a real device
   In-app DRM playback doesn't work in this webview, so we drive
   playback on a real Spotify device (desktop app, speaker) and
   poll /me/player for the now-playing state.
   ============================================================ */

/// Resolve a device to play on: the active one, else the first available.
/// Returns its id, or null (with a banner) if none is open.
async function ensureDevice() {
  if (state.deviceId) return state.deviceId;
  try {
    const devices = await invoke("list_devices");
    if (!devices.length) {
      banner("No Spotify device found. Open the Spotify app on this Mac (or a speaker), then press play again.");
      flashDevice("no device");
      return null;
    }
    const active = devices.find((d) => d.is_active) || devices[0];
    state.deviceId = active.id;
    state.deviceName = active.name;
    el("deviceState").textContent = active.name;
    el("deviceState").classList.add("ready");
    el("playbackBanner").classList.add("hidden");
    return active.id;
  } catch (e) {
    banner("Couldn't list devices: " + e);
    return null;
  }
}

/// Poll playback state ~every second and reflect it in the UI.
function startPolling() {
  if (state.ticker) return;
  const poll = async () => {
    try {
      const s = await invoke("playback_state");
      if (!s.active) {
        state.paused = true;
        renderNowPlaying();
        return;
      }
      if (s.device_id) {
        state.deviceId = s.device_id;
        el("deviceState").textContent = s.device_name || "playing";
        el("deviceState").classList.add("ready");
      }
      const t = s.track;
      if (t && t.uri) {
        // Track changed → push previous to history.
        if (!state.current || state.current.uri !== t.uri) {
          if (state.current) state.history.unshift(state.current);
          state.current = t;
          const idx = state.context.findIndex((c) => c.uri === t.uri);
          if (idx >= 0) {
            state.index = idx;
            renderQueue();
          }
        }
        state.durationMs = t.duration_ms || 0;
      }
      state.paused = !s.is_playing;
      state.positionMs = s.progress_ms || 0;
      if (typeof s.volume === "number") el("volume").value = s.volume;
      renderNowPlaying();
      tickProgress();
    } catch (e) {
      console.warn("poll:", e);
    }
  };
  poll();
  state.ticker = setInterval(poll, 1000);
}

function flashDevice(msg) {
  const d = el("deviceState");
  d.textContent = msg;
  d.classList.remove("ready");
}

function banner(msg) {
  el("pbMsg").textContent = msg;
  el("playbackBanner").classList.remove("hidden");
}

/* ---------- transport (Web API commands) ---------- */
async function guard(fn) {
  try {
    await fn();
  } catch (e) {
    flashDevice("command failed");
    console.error(e);
  }
}

el("playBtn").addEventListener("click", () =>
  guard(async () => {
    const dev = await ensureDevice();
    if (!dev) return;
    if (state.paused) await invoke("resume", { deviceId: dev });
    else await invoke("pause");
    state.paused = !state.paused;
    renderNowPlaying();
    startPolling();
  })
);
el("prevBtn").addEventListener("click", () => guard(() => invoke("previous_track")));
el("nextBtn").addEventListener("click", () => guard(() => invoke("next_track")));

let volTimer = null;
el("volume").addEventListener("input", (e) => {
  const v = +e.target.value;
  clearTimeout(volTimer);
  volTimer = setTimeout(() => guard(() => invoke("set_volume", { percent: v })), 200);
});

el("scrubTrack").addEventListener("click", (e) => {
  if (!state.durationMs) return;
  const rect = e.currentTarget.getBoundingClientRect();
  const pct = (e.clientX - rect.left) / rect.width;
  const pos = Math.round(pct * state.durationMs);
  state.positionMs = pos;
  tickProgress();
  guard(() => invoke("seek", { positionMs: pos }));
});

/* ---------- nav ---------- */
el("navSearch").addEventListener("click", () => {
  showApp("search");
  el("searchInput").focus();
});
el("navLibrary").addEventListener("click", () => {
  showApp("library");
  loadLibrary();
});
el("backBtn").addEventListener("click", () => showApp("library"));

/* ============================================================
   BOOT
   ============================================================ */
async function login() {
  const btn = el("loginBtn");
  btn.disabled = true;
  el("loginError").classList.add("hidden");
  try {
    await invoke("login");
    boot();
  } catch (e) {
    const box = el("loginError");
    box.textContent = `${e}`;
    box.classList.remove("hidden");
    btn.disabled = false;
  }
}
el("loginBtn").addEventListener("click", login);

function boot() {
  showApp("search");
  el("searchInput").focus();
  loadLibrary(); // warm the playlist cache
  ensureDevice(); // find a Spotify device to control
  startPolling(); // reflect whatever is already playing
}

(async () => {
  try {
    if (await invoke("is_authenticated")) boot();
  } catch {
    /* stay on login */
  }
})();
