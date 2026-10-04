//! Playback control with the device routing in Rust (the UI keeps its own in src/lib/route.js):
//! when the target device is This Mac (the in-app player) and the engine is ready, Spirc
//! directly (player.rs, no Web API); any other device through the Web API (spotify.rs, quota-
//! guarded). Used by the MCP server (mcp_app.rs).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::nowplaying::Now;
use crate::spotify;

pub const NO_DEVICE: &str = "No device to play on: open Stylus or Spotify somewhere";
pub const NOTHING_PLAYING: &str = "Nothing is playing";
pub const NOT_READY: &str = "This Mac isn't ready: the player is still connecting";
pub const NOTHING_AFTER: &str = "Nothing after this track: next would stop playback";
pub const NOTHING_BEFORE: &str = "Nothing before this track: previous would stop playback";

/// Where a command goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Path {
    /// This Mac's Spirc.
    Local,
    /// The Web API, for this device id.
    Remote(String),
}

/// What routing needs: This Mac's device id (while the engine is ready), whether the engine is
/// ready, and the active device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub own: Option<String>,
    pub ready: bool,
    pub active: Option<String>,
}

impl View {
    fn is_own(&self, id: &str) -> bool {
        self.own.as_deref() == Some(id)
    }
}

/// A transport command (pause, resume, next, previous, seek, shuffle, repeat, volume, queue) for
/// `target`, else the active device. This Mac directly only while it's ready and active: an
/// inactive Spirc ignores these (the UI's `isLocal` rule).
pub fn route_transport(v: &View, target: Option<&str>) -> Result<Path, String> {
    let t = target.or(v.active.as_deref()).ok_or(NOTHING_PLAYING)?;
    if v.ready && v.is_own(t) && v.active.as_deref() == Some(t) {
        Ok(Path::Local)
    } else {
        Ok(Path::Remote(t.to_string()))
    }
}

/// A play (or a transfer) on `target`, else the active device, else This Mac when it's ready.
/// This Mac loads directly, active or not (a load activates it).
pub fn route_play(v: &View, target: Option<&str>) -> Result<Path, String> {
    let fallback = if v.ready { v.own.as_deref() } else { None };
    let t = target.or(v.active.as_deref()).or(fallback).ok_or(NO_DEVICE)?;
    if v.is_own(t) {
        return if v.ready { Ok(Path::Local) } else { Err(NOT_READY.into()) };
    }
    Ok(Path::Remote(t.to_string()))
}

fn engine() -> Result<crate::player::Engine, String> {
    crate::internal::engine().ok_or_else(|| NOT_READY.to_string())
}

/// This Mac's state from the player's own events (nowplaying.rs), None while the player isn't up.
/// Live even while a track's metadata loads, when `Engine::now_state` says nothing.
pub fn here() -> Option<Now> {
    crate::nowplaying::live().map(|n| n.now())
}

/// This Mac is the active Connect device, by its own events.
fn here_active() -> bool {
    here().is_some_and(|n| n.engine_active)
}

/// The active device: This Mac when its own events say so (no Connect cluster or Web API
/// needed); else the cluster's (or the Web API's) answer, minus This Mac: the cluster lags
/// behind its events, and an inactive Spirc ignores commands.
pub fn active_device(own: Option<&str>, ready: bool, here_active: bool, remote: Option<String>) -> Option<String> {
    match own {
        Some(o) if ready && here_active => Some(o.to_string()),
        _ => remote.filter(|id| !(ready && own == Some(id.as_str()))),
    }
}

/// The routing view now: the engine, and the active device (`active_device`; the Connect
/// cluster, the Web API's player state only without a cluster).
pub async fn view() -> View {
    let engine = crate::internal::engine();
    let ready = engine.as_ref().is_some_and(|e| e.is_ready());
    let own = crate::internal::own_device_id();
    let here = ready && here_active();
    let remote = if here {
        None
    } else {
        match crate::internal::cluster_state() {
            Ok(state) => state.and_then(|s| s["device_id"].as_str().map(str::to_string)),
            Err(_) => spotify::playback_state().await.ok().filter(|s| s["active"] == true).and_then(|s| s["device_id"].as_str().map(str::to_string)),
        }
    };
    View { active: active_device(own.as_deref(), ready, here, remote), own, ready }
}

/// The device a volume command is for: `device`, else the active one, else This Mac when ready.
fn target(v: &View, device: Option<String>) -> Option<String> {
    device.or_else(|| v.active.clone()).or_else(|| if v.ready { v.own.clone() } else { None })
}

/// The device list with This Mac's `is_active` from its own events (the cluster lags behind them).
pub fn mark_here(list: Value, own: &str, here_active: bool) -> Value {
    let Value::Array(mut items) = list else { return list };
    for d in &mut items {
        let mine = d["id"] == own;
        if here_active || mine {
            d["is_active"] = json!(here_active && mine);
        }
    }
    Value::Array(items)
}

/// `list_devices`, with This Mac's activity from its own events while the player is up.
pub async fn devices() -> Result<Value, String> {
    let list = spotify::list_devices().await?;
    let ready = crate::internal::engine().is_some_and(|e| e.is_ready());
    Ok(match crate::internal::own_device_id() {
        Some(own) if ready => mark_here(list, &own, here_active()),
        _ => list,
    })
}

// ---- This Mac's volume ------------------------------------------------------------------------

/// This Mac's volume as last set while it was inactive. Spirc ignores volume then, and takes
/// its old level back when it activates: the level waits here, reads report it, and the next
/// load here sends it.
#[derive(Debug, Default, PartialEq)]
pub struct PendingVolume(Option<u8>);

impl PendingVolume {
    /// Sets `percent`: true when it goes to Spirc now (This Mac active), false when it waits.
    pub fn set(&mut self, active: bool, percent: u8) -> bool {
        self.0 = (!active).then_some(percent);
        active
    }

    /// The level to report: the waiting one while inactive, else the player's (`live`).
    pub fn read(&mut self, active: bool, live: u8) -> u8 {
        if active {
            // activated some other way (the UI, a phone): the player's level counts
            self.0 = None;
            return live;
        }
        self.0.unwrap_or(live)
    }

    pub fn take(&mut self) -> Option<u8> {
        self.0.take()
    }
}

static PENDING_VOLUME: Mutex<PendingVolume> = Mutex::new(PendingVolume(None));

fn pending_volume() -> std::sync::MutexGuard<'static, PendingVolume> {
    PENDING_VOLUME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Sets This Mac's volume. True: applied now; false: waits for the next load here.
fn set_here_volume(e: &crate::player::Engine, percent: u8) -> Result<bool, String> {
    let now = pending_volume().set(here_active(), percent);
    if now {
        e.set_volume(percent)?;
        // the player's VolumeChanged follows in a moment: reads right after see the level already
        if let Some(n) = crate::nowplaying::live() {
            n.set_volume(crate::nowplaying::volume_from_percent(percent));
        }
    }
    Ok(now)
}

fn here_volume(e: &crate::player::Engine) -> u8 {
    pending_volume().read(here_active(), e.volume_percent())
}

/// After a load here (it activates This Mac): the level set while inactive, if any.
fn send_pending_volume(e: &crate::player::Engine) {
    let pending = pending_volume().take();
    if let Some(p) = pending {
        if let Err(err) = e.set_volume(p) {
            log::warn!(target: "stylus::cmd", "pending volume {p} not sent: {err}");
        }
    }
}

/// What to play: a context (playlist, album, artist; from `track_uri` when given) or a track list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    pub context_uri: Option<String>,
    pub uris: Vec<String>,
    pub track_uri: Option<String>,
}

/// The current shuffle and repeat on This Mac while it's active, so a load keeps them.
fn local_modes(here: Option<&Now>) -> (Option<bool>, Option<String>) {
    match here.filter(|n| n.engine_active) {
        Some(n) => (Some(n.shuffle), Some(n.repeat.as_str().to_string())),
        None => (None, None),
    }
}

/// How long `play` waits for This Mac to start what it loaded: measured 2026-10-03, load to
/// Playing took 3.7 s cold (activation + first track) and ~3 s warm.
const CONFIRM_WAIT: Duration = Duration::from_secs(5);

/// `s` (This Mac's `now_state`) shows the load of `src`, `waited_ms` after it was sent: playing,
/// the asked-for track (or one of the list), and a new track or the same one restarted.
pub fn load_confirmed(before: Option<&str>, src: &Source, s: &Value, waited_ms: u64) -> bool {
    if s["active"] != true || s["is_playing"] != true {
        return false;
    }
    let Some(track) = s["track"]["uri"].as_str() else { return false };
    let wanted = match &src.track_uri {
        Some(t) => t == track,
        None => src.uris.is_empty() || src.uris.iter().any(|u| u == track),
    };
    wanted && (before != Some(track) || s["progress_ms"].as_u64().is_some_and(|p| p <= waited_ms + 1_500))
}

/// This Mac's state once it plays what was just loaded, None after `CONFIRM_WAIT`.
async fn confirm_load(e: &crate::player::Engine, before: Option<&str>, src: &Source) -> Option<Value> {
    let t0 = Instant::now();
    loop {
        let waited = t0.elapsed();
        if let Some(s) = e.now_state().filter(|s| load_confirmed(before, src, s, waited.as_millis() as u64)) {
            return Some(s);
        }
        if waited >= CONFIRM_WAIT {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

/// Starts `src` on `device` (an id; None = the active device, else This Mac). `{device_id, path}`;
/// This Mac adds `confirmed` and, when confirmed, `state` (its `now_state`).
pub async fn play(src: Source, device: Option<String>) -> Result<Value, String> {
    let v = view().await;
    match route_play(&v, device.as_deref())? {
        Path::Local => {
            let engine = engine()?;
            let here = here();
            let (shuffle, repeat) = local_modes(here.as_ref());
            let before = here.and_then(|n| n.track_uri);
            let uris = (!src.uris.is_empty()).then(|| src.uris.clone());
            engine.load(src.context_uri.clone(), uris, src.track_uri.clone(), 0, true, shuffle, repeat)?;
            send_pending_volume(&engine);
            let state = confirm_load(&engine, before.as_deref(), &src).await;
            Ok(json!({ "device_id": v.own, "path": "this_mac", "confirmed": state.is_some(), "state": state }))
        }
        Path::Remote(id) => {
            match src.context_uri {
                Some(ctx) => spotify::play_context(id.clone(), ctx, src.track_uri).await?,
                None => {
                    let at = src.track_uri.as_ref().and_then(|t| src.uris.iter().position(|u| u == t)).unwrap_or(0);
                    spotify::play_on_device(id.clone(), src.uris[at..].to_vec()).await?
                }
            }
            Ok(json!({ "device_id": id, "path": "web_api" }))
        }
    }
}

/// A transport command, routed (`route_transport`). `{device_id, path}`.
pub enum Cmd {
    Pause,
    Resume,
    Next,
    Previous,
    Seek(u32),
    Shuffle(bool),
    Repeat(String),
}

/// Under this position `previous` goes to the track before (Spirc: 3 s); a margin for the trip.
const PREV_RESTARTS_AFTER_MS: u32 = 2_500;

/// Why a next/previous on This Mac would only stop playback (Spirc stops when there is no track
/// to go to), None when it may go. Only when the cluster's up-next is known for this track and
/// repeat is off; anything uncertain goes through.
pub fn skip_blocked(cmd: &Cmd, n: &Now, now: Instant) -> Option<&'static str> {
    if !n.skips_fresh || n.repeat != crate::session::Repeat::Off {
        return None;
    }
    match cmd {
        Cmd::Next if n.next.as_ref().is_some_and(Vec::is_empty) => Some(NOTHING_AFTER),
        Cmd::Previous if n.prev == Some(false) && n.position(now) < PREV_RESTARTS_AFTER_MS => Some(NOTHING_BEFORE),
        _ => None,
    }
}

pub async fn transport(cmd: Cmd) -> Result<Value, String> {
    let v = view().await;
    let path = route_transport(&v, None)?;
    match &path {
        Path::Local => {
            let e = engine()?;
            if let Some(why) = here().and_then(|n| skip_blocked(&cmd, &n, Instant::now())) {
                return Err(why.into());
            }
            match cmd {
                Cmd::Pause => e.pause(),
                Cmd::Resume => e.play(),
                Cmd::Next => e.next(),
                Cmd::Previous => e.prev(),
                Cmd::Seek(ms) => e.seek(ms),
                Cmd::Shuffle(on) => e.set_shuffle(on),
                Cmd::Repeat(mode) => e.set_repeat(&mode),
            }?
        }
        Path::Remote(id) => match cmd {
            Cmd::Pause => spotify::pause().await?,
            Cmd::Resume => spotify::resume(id.clone()).await?,
            Cmd::Next => spotify::next_track().await?,
            Cmd::Previous => spotify::previous_track().await?,
            Cmd::Seek(ms) => spotify::seek(u64::from(ms)).await?,
            Cmd::Shuffle(on) => spotify::set_shuffle(on).await?,
            Cmd::Repeat(mode) => spotify::set_repeat(mode).await?,
        },
    }
    Ok(outcome(&v, &path))
}

fn outcome(v: &View, path: &Path) -> Value {
    match path {
        Path::Local => json!({ "device_id": v.own, "path": "this_mac" }),
        Path::Remote(id) => json!({ "device_id": id, "path": "web_api" }),
    }
}

/// The volume (0–100) of `device` (None = the active one, else This Mac): This Mac's own (or the
/// level waiting for its next load), else the device list's.
pub async fn volume(device: Option<&str>) -> Result<u8, String> {
    let v = view().await;
    let t = target(&v, device.map(str::to_string)).ok_or(NOTHING_PLAYING)?;
    if v.ready && v.is_own(&t) {
        return Ok(here_volume(&engine()?));
    }
    let list = spotify::list_devices().await?;
    let d = list.as_array().into_iter().flatten().find(|d| d["id"] == t.as_str()).ok_or("That device isn't listed any more")?;
    if d["supports_volume"] == false {
        return Err(format!("{} doesn't let Spotify change its volume", d["name"].as_str().unwrap_or("That device")));
    }
    Ok(d["volume_percent"].as_u64().unwrap_or(0).min(100) as u8)
}

/// Sets the volume of `device` (None = the active one, else This Mac). This Mac: directly while
/// active, else at its next load here (`PendingVolume`).
pub async fn set_volume(percent: u8, device: Option<String>) -> Result<Value, String> {
    let v = view().await;
    let t = target(&v, device).ok_or(NOTHING_PLAYING)?;
    let path = if v.ready && v.is_own(&t) { Path::Local } else { Path::Remote(t.clone()) };
    let mut out = outcome(&v, &path);
    match &path {
        Path::Local => {
            if !set_here_volume(&engine()?, percent.min(100))? {
                out["note"] = json!("This Mac isn't playing: the level applies when it starts playing here");
            }
        }
        Path::Remote(id) => spotify::set_volume(percent.min(100), Some(id.clone())).await?,
    }
    Ok(out)
}

/// Adds a track to the active device's queue (This Mac: a Connect command, no Web API).
pub async fn queue_add(uri: String) -> Result<Value, String> {
    let v = view().await;
    let id = v.active.clone().or_else(|| if v.ready { v.own.clone() } else { None }).ok_or(NOTHING_PLAYING)?;
    spotify::add_to_queue(id.clone(), uri).await?;
    Ok(json!({ "device_id": id }))
}

/// Moves playback to `device`. To This Mac: it loads what the active device plays, at its
/// position (no Web API); anything else: the Web API transfer.
pub async fn transfer(device: String, play: bool) -> Result<Value, String> {
    let v = view().await;
    if let Path::Local = route_play(&v, Some(&device))? {
        if let Ok(Some(s)) = crate::internal::cluster_state() {
            if let Some(track) = s["track_uri"].as_str().filter(|t| t.starts_with("spotify:track:")) {
                let ctx = s["context_uri"].as_str().and_then(crate::session::loadable_context).map(str::to_string);
                let uris = ctx.is_none().then(|| vec![track.to_string()]);
                let e = engine()?;
                let (shuffle, repeat) = (s["shuffle"].as_bool(), s["repeat"].as_str().map(str::to_string));
                let pos = s["position_ms"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32;
                e.load(ctx, uris, Some(track.to_string()), pos, play, shuffle, repeat)?;
                send_pending_volume(&e);
                return Ok(json!({ "device_id": v.own, "path": "this_mac" }));
            }
        }
    }
    spotify::transfer_playback(device.clone(), play).await?;
    Ok(json!({ "device_id": device, "path": "web_api" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(own: Option<&str>, ready: bool, active: Option<&str>) -> View {
        View { own: own.map(Into::into), ready, active: active.map(Into::into) }
    }

    #[test]
    fn transport_goes_local_only_when_this_mac_is_ready_and_active() {
        let r = |v: &View, t: Option<&str>| route_transport(v, t);
        assert_eq!(r(&view(Some("mac"), true, Some("mac")), None), Ok(Path::Local));
        // another device plays: the Web API, for it
        assert_eq!(r(&view(Some("mac"), true, Some("phone")), None), Ok(Path::Remote("phone".into())));
        // This Mac named but inactive: Spirc would ignore it
        assert_eq!(r(&view(Some("mac"), true, Some("phone")), Some("mac")), Ok(Path::Remote("mac".into())));
        // engine not ready: never local
        assert_eq!(r(&view(Some("mac"), false, Some("mac")), None), Ok(Path::Remote("mac".into())));
        assert_eq!(r(&view(Some("mac"), true, None), None), Err(NOTHING_PLAYING.into()));
    }

    #[test]
    fn play_falls_back_to_this_mac() {
        let r = |v: &View, t: Option<&str>| route_play(v, t);
        // nothing active: This Mac when it's ready
        assert_eq!(r(&view(Some("mac"), true, None), None), Ok(Path::Local));
        assert_eq!(r(&view(None, false, None), None), Err(NO_DEVICE.into()));
        // the active device by default, an explicit one when named
        assert_eq!(r(&view(Some("mac"), true, Some("phone")), None), Ok(Path::Remote("phone".into())));
        assert_eq!(r(&view(Some("mac"), true, Some("phone")), Some("mac")), Ok(Path::Local), "a load activates This Mac");
        assert_eq!(r(&view(Some("mac"), true, Some("mac")), Some("tv")), Ok(Path::Remote("tv".into())));
        // This Mac named while not ready
        assert_eq!(r(&view(Some("mac"), false, None), Some("mac")), Err(NOT_READY.into()));
    }

    #[test]
    fn this_mac_active_by_its_own_events_without_cluster_or_web_api() {
        // the observed bug: This Mac plays, no cluster yet, the Web API rate-limited (remote None)
        let active = active_device(Some("mac"), true, true, None);
        assert_eq!(active.as_deref(), Some("mac"));
        assert_eq!(route_transport(&View { own: Some("mac".into()), ready: true, active }, None), Ok(Path::Local), "next goes to Spirc");
        // its events win over a lagging cluster naming another device
        assert_eq!(active_device(Some("mac"), true, true, Some("phone".into())).as_deref(), Some("mac"));
        // the cluster still names This Mac after its events said it went inactive: stale
        assert_eq!(active_device(Some("mac"), true, false, Some("mac".into())), None);
        assert_eq!(active_device(Some("mac"), true, false, Some("phone".into())).as_deref(), Some("phone"));
        // player not up: only the remote answer
        assert_eq!(active_device(None, false, false, Some("phone".into())).as_deref(), Some("phone"));
        assert_eq!(active_device(Some("mac"), false, true, Some("mac".into())).as_deref(), Some("mac"));
    }

    #[test]
    fn volume_target_falls_back_to_this_mac() {
        assert_eq!(target(&view(Some("mac"), true, None), None).as_deref(), Some("mac"));
        assert_eq!(target(&view(Some("mac"), true, Some("tv")), None).as_deref(), Some("tv"));
        assert_eq!(target(&view(Some("mac"), true, Some("tv")), Some("mac".into())).as_deref(), Some("mac"));
        assert_eq!(target(&view(Some("mac"), false, None), None), None);
    }

    #[test]
    fn volume_set_while_inactive_waits_for_the_load() {
        // the observed bug: set_volume 35 while inactive was dropped by Spirc; the read said 73
        let mut p = PendingVolume::default();
        assert!(!p.set(false, 35), "inactive: not sent now");
        assert_eq!(p.read(false, 73), 35, "reads report the level set");
        assert_eq!(p.read(false, 73) as i64 + 10, 45, "volume_step +10 starts from it");
        assert_eq!(p.take(), Some(35), "the load sends it");
        assert_eq!(p.read(false, 73), 73);
        // active: straight to Spirc, nothing waits
        assert!(p.set(true, 30));
        assert_eq!(p.read(true, 30), 30);
        // activated some other way: the player's level counts, the old one is dropped
        p.set(false, 20);
        assert_eq!(p.read(true, 60), 60);
        assert_eq!(p.take(), None);
    }

    fn state(track: &str, playing: bool, progress: u64) -> Value {
        json!({ "active": true, "is_playing": playing, "progress_ms": progress, "track": { "uri": track }, "context_uri": null })
    }

    #[test]
    fn load_confirmation() {
        let ctx = Source { context_uri: Some("spotify:playlist:bonobo".into()), ..Source::default() };
        // the observed bug: right after play, the old track still plays — not confirmed
        assert!(!load_confirmed(Some("spotify:track:kerala"), &ctx, &state("spotify:track:kerala", true, 9_000), 140));
        // loading (not playing yet), then the new track plays
        assert!(!load_confirmed(Some("spotify:track:kerala"), &ctx, &state("spotify:track:cycles", false, 0), 1_000));
        assert!(load_confirmed(Some("spotify:track:kerala"), &ctx, &state("spotify:track:cycles", true, 200), 2_800));
        // the same track restarted: its position shows it
        assert!(load_confirmed(Some("spotify:track:cycles"), &ctx, &state("spotify:track:cycles", true, 300), 1_000));
        // a list: one of its tracks; an asked-for track: that one
        let list = Source { uris: vec!["spotify:track:a".into(), "spotify:track:b".into()], ..Source::default() };
        assert!(load_confirmed(None, &list, &state("spotify:track:b", true, 0), 500));
        assert!(!load_confirmed(None, &list, &state("spotify:track:x", true, 0), 500));
        let at = Source { track_uri: Some("spotify:track:b".into()), ..ctx.clone() };
        assert!(!load_confirmed(None, &at, &state("spotify:track:a", true, 0), 500));
        assert!(!load_confirmed(None, &ctx, &json!({ "active": false }), 500));
    }

    #[test]
    fn next_and_previous_refuse_only_when_they_would_stop() {
        let t0 = Instant::now();
        let at = |next: Option<Vec<&str>>, prev: Option<bool>, fresh: bool, pos: u32| {
            let mut n = Now::new(t0);
            n.track_uri = Some("spotify:track:kerala".into());
            n.next = next.map(|l| l.into_iter().map(String::from).collect());
            n.prev = prev;
            n.skips_fresh = fresh;
            n.position_ms = pos;
            n
        };
        // the observed bug: Kerala played on its own (uris:[track]), next stopped it silently
        assert_eq!(skip_blocked(&Cmd::Next, &at(Some(vec![]), Some(false), true, 6_000), t0), Some(NOTHING_AFTER));
        assert_eq!(skip_blocked(&Cmd::Next, &at(Some(vec!["spotify:track:b"]), Some(false), true, 6_000), t0), None);
        // up-next not known, or from before this track loaded: Spirc decides
        assert_eq!(skip_blocked(&Cmd::Next, &at(None, None, true, 6_000), t0), None);
        assert_eq!(skip_blocked(&Cmd::Next, &at(Some(vec![]), Some(false), false, 6_000), t0), None);
        // repeat on: Spirc wraps or repeats
        let mut rep = at(Some(vec![]), Some(false), true, 0);
        rep.repeat = crate::session::Repeat::Context;
        assert_eq!(skip_blocked(&Cmd::Next, &rep, t0), None);
        // previous: no track before and under 3 s stops; later it restarts the track
        assert_eq!(skip_blocked(&Cmd::Previous, &at(Some(vec![]), Some(false), true, 1_000), t0), Some(NOTHING_BEFORE));
        assert_eq!(skip_blocked(&Cmd::Previous, &at(Some(vec![]), Some(false), true, 6_000), t0), None);
        assert_eq!(skip_blocked(&Cmd::Previous, &at(Some(vec![]), Some(true), true, 1_000), t0), None);
        assert_eq!(skip_blocked(&Cmd::Pause, &at(Some(vec![]), Some(false), true, 0), t0), None);
    }

    #[test]
    fn a_load_makes_up_next_stale_until_the_cluster_speaks() {
        let t0 = Instant::now();
        let mut n = Now::new(t0);
        n.next = Some(vec![]);
        n.skips_fresh = true;
        let id = librespot_core::SpotifyUri::from_uri("spotify:track:5DAjrJqXqYtgr67pVhmUeR").unwrap();
        n.on_event(&librespot_playback::player::PlayerEvent::Loading { play_request_id: 2, track_id: id, position_ms: 0 }, t0);
        assert!(!n.skips_fresh, "the old track's up-next must not block the new one");
        assert_eq!(skip_blocked(&Cmd::Next, &n, t0), None);
    }

    #[test]
    fn devices_mark_this_mac_by_its_events() {
        // the observed bug: the cluster (stale) said nothing/another device is active while This Mac played
        let list = json!([{ "id": "mac", "is_active": false }, { "id": "phone", "is_active": true }]);
        assert_eq!(mark_here(list.clone(), "mac", true), json!([{ "id": "mac", "is_active": true }, { "id": "phone", "is_active": false }]));
        let stale = json!([{ "id": "mac", "is_active": true }, { "id": "phone", "is_active": false }]);
        assert_eq!(mark_here(stale, "mac", false), json!([{ "id": "mac", "is_active": false }, { "id": "phone", "is_active": false }]));
        assert_eq!(mark_here(list.clone(), "mac", false), list, "another device plays: unchanged");
    }
}
