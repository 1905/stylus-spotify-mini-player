//! Playback control with the device routing in Rust (the UI keeps its own in src/lib/route.js):
//! when the target device is This Mac (the in-app player) and the engine is ready, Spirc
//! directly (player.rs, no Web API); any other device through the Web API (spotify.rs, quota-
//! guarded). Used by the MCP server (mcp_app.rs).

use serde_json::{json, Value};

use crate::spotify;

pub const NO_DEVICE: &str = "No device to play on: open Needle or Spotify somewhere";
pub const NOTHING_PLAYING: &str = "Nothing is playing";
pub const NOT_READY: &str = "This Mac isn't ready: the player is still connecting";

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

/// The routing view now: the engine, and the active device from the Connect cluster (the Web
/// API's player state while the player isn't up).
pub async fn view() -> View {
    let engine = crate::internal::engine();
    let ready = engine.as_ref().is_some_and(|e| e.is_ready());
    let own = crate::internal::own_device_id();
    let active = match crate::internal::cluster_state() {
        Ok(state) => state.and_then(|s| s["device_id"].as_str().map(str::to_string)),
        Err(_) => spotify::playback_state().await.ok().filter(|s| s["active"] == true).and_then(|s| s["device_id"].as_str().map(str::to_string)),
    };
    View { own, ready, active }
}

/// What to play: a context (playlist, album, artist; from `track_uri` when given) or a track list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Source {
    pub context_uri: Option<String>,
    pub uris: Vec<String>,
    pub track_uri: Option<String>,
}

/// The current shuffle and repeat on This Mac, so a load keeps them.
fn local_modes(engine: &crate::player::Engine) -> (Option<bool>, Option<String>) {
    let s = engine.now_state().unwrap_or(Value::Null);
    (s["shuffle"].as_bool(), s["repeat"].as_str().map(str::to_string))
}

/// Starts `src` on `device` (an id; None = the active device, else This Mac). `{device_id, path}`.
pub async fn play(src: Source, device: Option<String>) -> Result<Value, String> {
    let v = view().await;
    match route_play(&v, device.as_deref())? {
        Path::Local => {
            let engine = engine()?;
            let (shuffle, repeat) = local_modes(&engine);
            let uris = (!src.uris.is_empty()).then(|| src.uris.clone());
            engine.load(src.context_uri, uris, src.track_uri, 0, true, shuffle, repeat)?;
            Ok(json!({ "device_id": v.own, "path": "this_mac" }))
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

pub async fn transport(cmd: Cmd) -> Result<Value, String> {
    let v = view().await;
    let path = route_transport(&v, None)?;
    match &path {
        Path::Local => {
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

/// The volume (0–100) of `device` (None = the active one): This Mac's own, else the device list's.
pub async fn volume(device: Option<&str>) -> Result<u8, String> {
    let v = view().await;
    let t = device.map(str::to_string).or(v.active.clone()).ok_or(NOTHING_PLAYING)?;
    if v.ready && v.is_own(&t) {
        return Ok(engine()?.volume_percent());
    }
    let list = spotify::list_devices().await?;
    let d = list.as_array().into_iter().flatten().find(|d| d["id"] == t.as_str()).ok_or("That device isn't listed any more")?;
    if d["supports_volume"] == false {
        return Err(format!("{} doesn't let Spotify change its volume", d["name"].as_str().unwrap_or("That device")));
    }
    Ok(d["volume_percent"].as_u64().unwrap_or(0).min(100) as u8)
}

/// Sets the volume of `device` (None = the active one). This Mac: directly, active or not.
pub async fn set_volume(percent: u8, device: Option<String>) -> Result<Value, String> {
    let v = view().await;
    let t = device.or(v.active.clone()).ok_or(NOTHING_PLAYING)?;
    let path = if v.ready && v.is_own(&t) { Path::Local } else { Path::Remote(t.clone()) };
    match &path {
        Path::Local => engine()?.set_volume(percent.min(100))?,
        Path::Remote(id) => spotify::set_volume(percent.min(100), Some(id.clone())).await?,
    }
    Ok(outcome(&v, &path))
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
}
