//! The MCP tools' `Backend` in the app: the same commands the UI calls (spotify.rs, internal API
//! first, the Web API last), control.rs for playback, library.rs for mixes and added links.
//! List calls pass the account so they read and fill the same disk cache as the UI.

use futures_util::FutureExt;
use serde_json::{json, Value};

use crate::control::{self, Cmd, Source};
use crate::mcp_tools::{Backend, Fut};
use crate::spotify;

pub struct App;

/// The account the list cache is scoped to (None: no cache).
async fn account() -> Option<String> {
    spotify::me_id().await.ok()
}

fn unit(r: Result<(), String>) -> Result<Value, String> {
    r.map(|()| json!({ "ok": true }))
}

/// Now playing: This Mac's own state while it plays here, else the Connect cluster's active
/// device (track names from the internal API), else the Web API.
async fn now_playing() -> Result<Value, String> {
    if let Some(engine) = crate::internal::engine() {
        if let Some(s) = engine.now_state().filter(|s| s["active"] == true) {
            return Ok(s);
        }
    }
    match crate::internal::cluster_state() {
        Ok(None) => Ok(json!({ "active": false })),
        Ok(Some(c)) => {
            let track = match c["track_uri"].as_str() {
                Some(uri) => crate::internal::Api::current()?.tracks(&[uri.to_string()]).await?.into_iter().next().unwrap_or(Value::Null),
                None => Value::Null,
            };
            let devices = spotify::list_devices().await.unwrap_or_default();
            let dev = devices.as_array().into_iter().flatten().find(|d| d["id"] == c["device_id"]).cloned().unwrap_or_default();
            Ok(json!({
                "active": true, "is_playing": c["is_playing"], "progress_ms": c["position_ms"],
                "device_id": c["device_id"], "device_name": dev["name"], "track": track,
                "shuffle": c["shuffle"], "repeat": c["repeat"], "volume_percent": dev["volume_percent"], "context_uri": c["context_uri"],
            }))
        }
        Err(_) => spotify::playback_state().await,
    }
}

impl Backend for App {
    fn now_playing(&self) -> Fut<'_> {
        now_playing().boxed()
    }
    fn search(&self, query: String) -> Fut<'_> {
        spotify::search(query).boxed()
    }
    fn playlists(&self) -> Fut<'_> {
        async { spotify::get_playlists(account().await).await }.boxed()
    }
    fn playlist_tracks(&self, id: String) -> Fut<'_> {
        async move { spotify::get_playlist_tracks(id, None, account().await).await }.boxed()
    }
    fn albums(&self) -> Fut<'_> {
        async { spotify::get_saved_albums(account().await).await }.boxed()
    }
    fn album_tracks(&self, id: String) -> Fut<'_> {
        async move { spotify::get_album_tracks(id, account().await).await }.boxed()
    }
    fn liked(&self) -> Fut<'_> {
        async { spotify::get_saved_tracks(account().await).await }.boxed()
    }
    fn recent(&self) -> Fut<'_> {
        spotify::get_recently_played().boxed()
    }
    fn top(&self, kind: String, range: String) -> Fut<'_> {
        async move { spotify::get_top(kind, range, Some(20), account().await).await }.boxed()
    }
    fn artist(&self, id: String) -> Fut<'_> {
        spotify::get_artist(id).boxed()
    }
    fn artist_albums(&self, id: String) -> Fut<'_> {
        spotify::get_artist_albums(id).boxed()
    }
    fn followed(&self) -> Fut<'_> {
        async { spotify::get_followed_artists(account().await).await }.boxed()
    }
    fn devices(&self) -> Fut<'_> {
        spotify::list_devices().boxed()
    }
    fn queue(&self) -> Fut<'_> {
        spotify::get_queue().boxed()
    }
    fn mixes(&self) -> Fut<'_> {
        async { Ok(Value::Array(crate::library::mixes(false).await)) }.boxed()
    }
    fn links(&self) -> Fut<'_> {
        async { Ok(Value::Array(crate::library::saved_links())) }.boxed()
    }
    fn resolve_link(&self, text: String) -> Fut<'_> {
        crate::library::link_resolve(text).boxed()
    }
    fn save_link(&self, text: String) -> Fut<'_> {
        crate::library::link_save(text).boxed()
    }
    fn play(&self, src: Source, device: Option<String>) -> Fut<'_> {
        control::play(src, device).boxed()
    }
    fn transport(&self, cmd: Cmd) -> Fut<'_> {
        control::transport(cmd).boxed()
    }
    fn volume(&self, device: Option<String>) -> Fut<'_> {
        async move { control::volume(device.as_deref()).await.map(Value::from) }.boxed()
    }
    fn set_volume(&self, percent: u8, device: Option<String>) -> Fut<'_> {
        control::set_volume(percent, device).boxed()
    }
    fn queue_add(&self, uri: String) -> Fut<'_> {
        control::queue_add(uri).boxed()
    }
    fn transfer(&self, device: String, play: bool) -> Fut<'_> {
        control::transfer(device, play).boxed()
    }
    fn like(&self, track_id: String, on: bool) -> Fut<'_> {
        async move { unit(if on { spotify::save_track(track_id).await } else { spotify::unsave_track(track_id).await }) }.boxed()
    }
}
