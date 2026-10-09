//! Playback control with the device routing in Rust (the UI keeps its own in src/lib/route.js):
//! when the target device is This Mac (the in-app player) and the engine is ready, Spirc
//! directly (player.rs). Any other device gives `NOT_AVAILABLE_REMOTE` for now. Used by the MCP
//! server (mcp_app.rs) and the UI's playback commands (spotify.rs).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::nowplaying::Now;
use crate::player::LoadSpec;
use crate::spotify;

pub const NO_DEVICE: &str = "No device to play on: open Stylus or Spotify somewhere";
pub const NOTHING_PLAYING: &str = "Nothing is playing";
pub const NOT_READY: &str = "This Mac isn't ready: the player is still connecting";

/// The error for a command that this app can't send to another device yet.
pub fn not_available_remote(action: &str) -> String {
    format!("NOT_AVAILABLE_REMOTE: {action} on other devices")
}

/// Where a command goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Path {
    /// This Mac's Spirc.
    Local,
    /// Another device, by its id.
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

/// This Mac is the active Connect device, by its own events.
pub fn here_active() -> bool {
    crate::internal::engine().is_some_and(|e| e.now_playing().engine_active())
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

/// The routing view for a command on `device`: a named device needs no active one, so the
/// Connect cluster (or the Web API) isn't asked.
async fn view_for(device: Option<&str>) -> View {
    if device.is_none() {
        return view().await;
    }
    let ready = crate::internal::engine().is_some_and(|e| e.is_ready());
    View { own: crate::internal::own_device_id(), ready, active: None }
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
    // a requested context must be the one playing: else the old track, unchanged and early in
    // its play, would "confirm" a load that hasn't happened yet
    let context_ok = match &src.context_uri {
        Some(c) => s["context_uri"].as_str() == Some(c.as_str()),
        None => true,
    };
    wanted && context_ok && (before != Some(track) || s["progress_ms"].as_u64().is_some_and(|p| p <= waited_ms + 1_500))
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
            let here = engine.now_playing().now();
            let (shuffle, repeat) = local_modes(Some(&here));
            let uris = (!src.uris.is_empty()).then(|| src.uris.clone());
            engine.load(LoadSpec { context_uri: src.context_uri.clone(), uris, track_uri: src.track_uri.clone(), position_ms: 0, play: true, shuffle, repeat })?;
            let state = confirm_load(&engine, here.track_uri.as_deref(), &src).await;
            let mut out = outcome(&v, &Path::Local);
            out["confirmed"] = json!(state.is_some());
            out["state"] = json!(state);
            Ok(out)
        }
        Path::Remote(_) => Err(not_available_remote("play")),
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

impl Cmd {
    /// The command's name in a `NOT_AVAILABLE_REMOTE` error.
    fn action(&self) -> &'static str {
        match self {
            Cmd::Pause => "pause",
            Cmd::Resume => "resume",
            Cmd::Next => "next",
            Cmd::Previous => "previous",
            Cmd::Seek(_) => "seek",
            Cmd::Shuffle(_) => "shuffle",
            Cmd::Repeat(_) => "repeat",
        }
    }
}

pub async fn transport(cmd: Cmd) -> Result<Value, String> {
    let v = view().await;
    let path = route_transport(&v, None)?;
    match &path {
        Path::Local => {
            // next/previous refuse when they would only stop playback (Engine::next, prev)
            let e = engine()?;
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
        Path::Remote(_) => return Err(not_available_remote(cmd.action())),
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
    let v = view_for(device).await;
    let t = target(&v, device.map(str::to_string)).ok_or(NOTHING_PLAYING)?;
    if v.ready && v.is_own(&t) {
        return Ok(engine()?.volume_percent());
    }
    let list = spotify::list_devices().await?;
    listed_volume(list.as_array().map_or(&[], Vec::as_slice), &t)
}

/// The volume of device `id` in a device list, or why there is none.
pub fn listed_volume(list: &[Value], id: &str) -> Result<u8, String> {
    let d = list.iter().find(|d| d["id"] == id).ok_or("That device isn't listed any more")?;
    if d["supports_volume"] == false {
        return Err(format!("{} doesn't let Spotify change its volume", d["name"].as_str().unwrap_or("That device")));
    }
    Ok(d["volume_percent"].as_u64().unwrap_or(0).min(100) as u8)
}

/// Sets the volume of `device` (None = the active one, else This Mac). This Mac: directly while
/// active, else at its next load here (`Engine::set_volume`).
pub async fn set_volume(percent: u8, device: Option<String>) -> Result<Value, String> {
    let v = view_for(device.as_deref()).await;
    let t = target(&v, device).ok_or(NOTHING_PLAYING)?;
    let path = if v.ready && v.is_own(&t) { Path::Local } else { Path::Remote(t.clone()) };
    let mut out = outcome(&v, &path);
    match &path {
        Path::Local => {
            if !engine()?.set_volume(percent.min(100))? {
                out["note"] = json!("This Mac isn't playing: the level applies when it starts playing here");
            }
        }
        Path::Remote(_) => return Err(not_available_remote("volume")),
    }
    Ok(out)
}

/// Adds a track to the active device's queue (This Mac: a Connect command, no Web API).
pub async fn queue_add(uri: String) -> Result<Value, String> {
    let v = view().await;
    let id = target(&v, None).ok_or(NOTHING_PLAYING)?;
    spotify::add_to_queue(id.clone(), uri).await?;
    Ok(json!({ "device_id": id }))
}

/// Moves playback to `device`. To This Mac: it loads what the active device plays, at its
/// position; with nothing playing there is nothing to load (a `note` says so). Other devices:
/// `NOT_AVAILABLE_REMOTE`.
pub async fn transfer(device: String, play: bool) -> Result<Value, String> {
    let v = view().await;
    if let Path::Local = route_play(&v, Some(&device))? {
        if let Ok(Some(s)) = crate::internal::cluster_state() {
            if let Some(track) = s["track_uri"].as_str().filter(|t| t.starts_with("spotify:track:")) {
                let ctx = s["context_uri"].as_str().and_then(crate::session::loadable_context).map(str::to_string);
                let uris = ctx.is_none().then(|| vec![track.to_string()]);
                let (shuffle, repeat) = (s["shuffle"].as_bool(), s["repeat"].as_str().map(str::to_string));
                let position_ms = s["position_ms"].as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32;
                engine()?.load(LoadSpec { context_uri: ctx, uris, track_uri: Some(track.to_string()), position_ms, play, shuffle, repeat })?;
                return Ok(outcome(&v, &Path::Local));
            }
        }
        let mut out = outcome(&v, &Path::Local);
        out["note"] = json!("Nothing was playing: This Mac plays what you start next");
        return Ok(out);
    }
    Err(not_available_remote("transfer"))
}

/// The UI's device pick to This Mac (`transfer`).
#[tauri::command]
pub async fn control_transfer(device: String, play: bool) -> Result<Value, String> {
    transfer(device, play).await
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

    fn state(track: &str, playing: bool, progress: u64) -> Value {
        state_in(track, playing, progress, None)
    }

    fn state_in(track: &str, playing: bool, progress: u64, context: Option<&str>) -> Value {
        json!({ "active": true, "is_playing": playing, "progress_ms": progress, "track": { "uri": track }, "context_uri": context })
    }

    #[test]
    fn load_confirmation() {
        let ctx = Source { context_uri: Some("spotify:playlist:bonobo".into()), ..Source::default() };
        // the observed bug: right after play, the old track still plays — not confirmed
        assert!(!load_confirmed(Some("spotify:track:kerala"), &ctx, &state("spotify:track:kerala", true, 9_000), 140));
        // loading (not playing yet), then the new track plays
        assert!(!load_confirmed(Some("spotify:track:kerala"), &ctx, &state("spotify:track:cycles", false, 0), 1_000));
        assert!(load_confirmed(Some("spotify:track:kerala"), &ctx, &state_in("spotify:track:cycles", true, 200, Some("spotify:playlist:bonobo")), 2_800));
        // the same track restarted: its position shows it
        assert!(load_confirmed(Some("spotify:track:cycles"), &ctx, &state_in("spotify:track:cycles", true, 300, Some("spotify:playlist:bonobo")), 1_000));
        // Astra round 2: the old track early in its play, still in the OLD context, doesn't confirm
        assert!(!load_confirmed(Some("spotify:track:a"), &ctx, &state_in("spotify:track:a", true, 500, Some("spotify:album:old")), 300));
        // a list: one of its tracks; an asked-for track: that one
        let list = Source { uris: vec!["spotify:track:a".into(), "spotify:track:b".into()], ..Source::default() };
        assert!(load_confirmed(None, &list, &state("spotify:track:b", true, 0), 500));
        assert!(!load_confirmed(None, &list, &state("spotify:track:x", true, 0), 500));
        let at = Source { track_uri: Some("spotify:track:b".into()), ..ctx.clone() };
        assert!(!load_confirmed(None, &at, &state("spotify:track:a", true, 0), 500));
        assert!(!load_confirmed(None, &ctx, &json!({ "active": false }), 500));
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
