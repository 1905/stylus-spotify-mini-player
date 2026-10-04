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

/// Where `now_playing` reads from.
#[derive(Debug, PartialEq)]
enum NowFrom {
    /// This Mac's own events.
    Here,
    /// The Connect cluster's active device (not This Mac).
    Cluster(Value),
    /// Nothing plays.
    Idle,
    /// No cluster: the Web API.
    WebApi,
}

/// This Mac's events first while they say it's active (live; the cluster lags behind them).
/// Else the cluster's active device, unless that's This Mac (stale: its events say it isn't
/// active). The Web API only without a cluster.
fn now_from(here_active: bool, own: Option<&str>, cluster: Result<Option<Value>, String>) -> NowFrom {
    if here_active {
        return NowFrom::Here;
    }
    match cluster {
        Ok(Some(c)) if own.is_none_or(|o| c["device_id"] != o) => NowFrom::Cluster(c),
        Ok(_) => NowFrom::Idle,
        Err(_) => NowFrom::WebApi,
    }
}

/// Nothing plays (as far as can be told): `{active:false}`, with what was last loaded here
/// (`last_here`) and, when other devices couldn't be checked, why (`unchecked`).
fn idle(unchecked: Option<String>) -> Value {
    let last = crate::nowplaying::live().and_then(|n| n.last_session()).map(|s| {
        let context = match &s.source {
            Some(crate::session::Source::Context { context_uri }) => Some(context_uri.clone()),
            _ => None,
        };
        json!({ "track_uri": s.track_uri, "context_uri": context, "position_ms": s.position_ms })
    });
    json!({ "active": false, "last_here": last, "unchecked": unchecked })
}

/// This Mac's state (`now_state`). While a new track's metadata loads (a second or two) it
/// waits up to 1.5 s, then answers without the names. None with nothing loaded here.
async fn here_state(e: &crate::player::Engine, own: Option<String>) -> Option<Value> {
    for _ in 0..10 {
        if let Some(s) = e.now_state().filter(|s| s["active"] == true) {
            return Some(s);
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    let n = control::here().filter(|n| n.engine_active)?;
    let track = n.track_uri.clone()?;
    Some(json!({
        "active": true, "loading": true, "is_playing": n.playing, "progress_ms": n.position(std::time::Instant::now()),
        "device_id": own, "device_name": crate::player::DEVICE_NAME, "track": { "uri": track },
        "shuffle": n.shuffle, "repeat": n.repeat.as_str(), "volume_percent": e.volume_percent(), "context_uri": n.context_uri,
    }))
}

/// Now playing (`now_from`). This Mac and the cluster need no Web API; a rate-limited Web API
/// gives "nothing plays here, other devices unchecked" rather than an error.
async fn now_playing() -> Result<Value, String> {
    let engine = crate::internal::engine().filter(|e| e.is_ready());
    let here_active = engine.is_some() && control::here().is_some_and(|n| n.engine_active);
    let own = crate::internal::own_device_id();
    match now_from(here_active, own.as_deref(), crate::internal::cluster_state()) {
        NowFrom::Here => match &engine {
            Some(e) => Ok(here_state(e, own).await.unwrap_or_else(|| idle(None))),
            None => Ok(idle(None)),
        },
        NowFrom::Idle => Ok(idle(None)),
        NowFrom::WebApi => match spotify::playback_state().await {
            Ok(s) => Ok(s),
            Err(e) if e.starts_with("RATE_LIMITED") => Ok(idle(Some(e))),
            Err(e) => Err(e),
        },
        NowFrom::Cluster(c) => {
            // names from the internal API (no Web API); just the uri when that fails
            let track = match c["track_uri"].as_str() {
                Some(uri) => match crate::internal::Api::current() {
                    Ok(api) => api.tracks(&[uri.to_string()]).await.ok().and_then(|t| t.into_iter().next()).unwrap_or_else(|| json!({ "uri": uri })),
                    Err(_) => json!({ "uri": uri }),
                },
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
        control::devices().boxed()
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
    fn album_of_track(&self, track_id: String) -> Fut<'_> {
        // the internal API (the album-info card's lookup): works while the Web API is rate-limited
        async move { Ok(json!(format!("spotify:album:{}", crate::internal::Api::current()?.album_id_of_track(&track_id).await?))) }.boxed()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_playing_reads_this_mac_without_the_web_api() {
        let no_cluster = || Err::<Option<Value>, String>("ENGINE_NOT_READY: no Connect cluster yet".into());
        // the observed bug: This Mac plays, no cluster yet, Web API rate-limited → its own events
        assert_eq!(now_from(true, Some("mac"), no_cluster()), NowFrom::Here);
        assert_eq!(now_from(true, Some("mac"), Ok(None)), NowFrom::Here, "a lagging cluster doesn't hide it");
        // another device plays: the cluster (works while rate-limited)
        let phone = json!({ "device_id": "phone", "track_uri": "spotify:track:x" });
        assert_eq!(now_from(false, Some("mac"), Ok(Some(phone.clone()))), NowFrom::Cluster(phone.clone()));
        assert_eq!(now_from(false, None, Ok(Some(phone.clone()))), NowFrom::Cluster(phone));
        // the cluster still names This Mac after it went inactive: stale, nothing plays
        assert_eq!(now_from(false, Some("mac"), Ok(Some(json!({ "device_id": "mac" })))), NowFrom::Idle);
        assert_eq!(now_from(false, Some("mac"), Ok(None)), NowFrom::Idle);
        // no cluster and This Mac idle: only then the Web API
        assert_eq!(now_from(false, Some("mac"), no_cluster()), NowFrom::WebApi);
    }
}
