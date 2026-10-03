// Browser-only stub of window.__TAURI__ for the mini player harness (dev/mini.html). Never shipped.
// It plays the main window's part: mini_command changes a fake stage and pushes mini-state back,
// with the main window's waits (a play spinner until the sound starts, a ring on next / previous).
// Scenario from ?s= : playing (default), paused, pending (a play starting), skipping (next on its way),
// loading (a new play's preview), saved (heart on), remote (plays on "Kitchen"), long (long titles),
// no-volume, ad, idle (nothing playing), login (logged out), nocover (the cover fails to load).
// QA hook: window.__mini = { scenario, state, calls, push }: calls = every invoke ({cmd, args}).
(function () {
  "use strict";

  const SCENARIOS = ["playing", "paused", "pending", "skipping", "loading", "saved", "remote", "long", "no-volume", "ad", "idle", "login", "nocover"];
  const requested = new URLSearchParams(location.search).get("s") || "playing";
  const scenario = SCENARIOS.includes(requested) ? requested : "playing";

  const TRACKS = [
    { name: "Do You Mind? - Bonus", artists: "The xx", cover: "https://i.scdn.co/image/ab67616d0000b273789657ec664daa222cab1e5a", duration_ms: 217186 },
    { name: "Intro", artists: "The xx", cover: "https://i.scdn.co/image/ab67616d0000b273789657ec664daa222cab1e5a", duration_ms: 127880 },
  ];
  if (scenario === "long") {
    TRACKS[0] = { ...TRACKS[0], name: "Everything In Its Right Place (Live From The Basement, Remastered 2024)", artists: "Radiohead, Thom Yorke, Jonny Greenwood, Ed O'Brien" };
  }
  if (scenario === "nocover") TRACKS[0] = { ...TRACKS[0], cover: "https://i.scdn.co/image/missing" };

  let at = 0; // the track index
  const state = {
    mode: { idle: "idle", login: "idle", ad: "other" }[scenario] || "track",
    playing: !["paused", "idle", "login"].includes(scenario),
    pending: scenario === "pending" || scenario === "loading",
    skipping: scenario === "skipping" ? "next" : null,
    loading: scenario === "loading",
    positionMs: 74000,
    volume: scenario === "no-volume" || scenario === "idle" || scenario === "login" ? null : 64,
    heart: scenario === "saved" ? true : ["idle", "login", "ad"].includes(scenario) ? null : false,
    device: scenario === "remote" ? "Kitchen" : null,
    status: { idle: "Nothing playing", login: "Log in to Spotify in Needle", ad: "Playing here" }[scenario] || null,
    sentAt: 0,
  };
  const calls = [];
  const listeners = [];

  function payload() {
    const t = state.mode === "track" || state.loading ? TRACKS[at] : null;
    return {
      mode: state.mode,
      title: t ? t.name : null,
      artist: t ? t.artists : null,
      cover: t ? t.cover : null,
      status: t ? null : state.status,
      playing: state.playing && state.mode !== "idle",
      pending: state.pending,
      skipping: state.skipping,
      loading: state.loading,
      positionMs: t && !state.loading && !state.skipping ? state.positionMs : 0,
      durationMs: t ? t.duration_ms : 0,
      sentAt: (state.sentAt = Date.now()),
      volume: state.volume,
      heart: state.heart,
      device: state.device,
    };
  }

  /** Where the position is now, then re-anchor it (a push sends it from here). */
  function settle() {
    if (state.playing && state.sentAt) state.positionMs += Date.now() - state.sentAt;
  }

  function push() {
    settle();
    const p = payload();
    for (const fn of listeners) fn({ event: "mini-state", payload: p });
  }

  const later = (ms, fn) => setTimeout(() => (fn(), push()), ms);

  function command({ action, value }) {
    if (scenario === "login") return;
    if (action === "toggle" && state.mode !== "idle") {
      settle();
      state.playing = !state.playing;
      state.pending = state.playing; // the main window waits for the sound
      if (state.pending) later(1200, () => (state.pending = false));
    } else if ((action === "next" || action === "previous") && state.mode === "track") {
      state.skipping = action;
      later(900, () => {
        state.skipping = null;
        at = (at + 1) % TRACKS.length;
        state.positionMs = 0;
        state.heart = false;
      });
    } else if (action === "volume" && state.volume != null) state.volume = Math.round(value);
    else if (action === "mute" && state.volume != null) state.volume = state.volume ? 0 : 50;
    else if (action === "heart" && state.heart != null) state.heart = !state.heart;
    push();
  }

  const handlers = {
    mini_get: () => payload(),
    mini_command: (args) => command(args),
    mini_hide: () => {},
  };

  window.__TAURI__ = {
    core: {
      invoke(cmd, args) {
        calls.push({ cmd, args });
        const h = handlers[cmd];
        return h ? Promise.resolve(h(args || {})) : Promise.reject(`mock: unknown command ${cmd}`);
      },
    },
    event: {
      listen(name, fn) {
        if (name === "mini-state") listeners.push(fn);
        return Promise.resolve(() => {});
      },
    },
  };
  window.__mini = { scenario, state, calls, push };
})();
