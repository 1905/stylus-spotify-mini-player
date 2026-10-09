//! The commands the UI calls for lists, search, library and Spotify Connect playback control.
//! Library, search and detail commands use Spotify's internal endpoints (internal.rs, with the
//! player's own session). Playback commands go through control.rs: This Mac over Spirc; other
//! devices give `NOT_AVAILABLE_REMOTE` for now.
//!
//! Errors starting with `ENGINE_NOT_READY`, `NOT_AVAILABLE_REMOTE` or `BAD_ARGS` are codes the
//! frontend matches with `startsWith`.

use crate::control::{self, Cmd};
use crate::internal::{
    self, serve, with_api,
    Source::{Fallback, Primary},
};
use futures_util::FutureExt;
use serde_json::{json, Value};

/// Stores a list under the account's `key` in the disk cache, off the async thread.
/// No account, no cache: the key must be scoped to the verified account.
fn cache_store(account: &Option<String>, key: String, value: &Value) {
    let Some(account) = account.clone().filter(|a| !a.is_empty()) else { return };
    let value = value.clone();
    tokio::task::spawn_blocking(move || crate::cache::lists().put(&account, &key, &value));
}

/// The account's cached `key`, or None without an account.
pub(crate) async fn cached(account: &Option<String>, key: &str) -> Option<Value> {
    cache_get(account.clone()?, key.to_string()).await
}

/// The cached list under the account's `key`, or null. Never errors.
#[tauri::command]
pub async fn cache_get(account: String, key: String) -> Option<Value> {
    tokio::task::spawn_blocking(move || crate::cache::lists().get(&account, &key)).await.ok().flatten()
}

/// The signed-in user's Spotify id: the account the cache is scoped to. The player's session
/// knows it without a request.
#[tauri::command]
pub async fn me_id() -> Result<String, String> {
    serve("me_id", vec![with_api(Primary, |api| async move { Ok(api.username()) })]).await
}

// ---- Spotify Connect: control a real device -------------------------------

/// List the user's available Spotify Connect devices, unchanged:
/// `[{id, name, type, is_active, is_restricted, supports_volume, volume_percent, …}]`.
/// From the player's Connect cluster (this Mac always listed), else this Mac alone.
#[tauri::command]
pub async fn list_devices() -> Result<Value, String> {
    let cluster = (Primary, async { internal::devices() }.boxed());
    let own = (Fallback, async { internal::own_device_only() }.boxed());
    serve("list_devices", vec![cluster, own]).await
}

/// Current playback state on the active device, simplified for the UI
/// (`internal::playback_snapshot`). `{active:false}` when nothing is playing.
#[tauri::command]
pub async fn playback_state() -> Result<Value, String> {
    internal::playback_snapshot().await
}

/// Move playback to `device_id`; `play` starts it there, false keeps the current state.
#[tauri::command]
pub async fn transfer_playback(device_id: String, play: bool) -> Result<(), String> {
    control::transfer(device_id, play).await.map(drop)
}

#[tauri::command]
pub async fn set_volume(percent: u8, device_id: Option<String>) -> Result<(), String> {
    control::set_volume(percent.min(100), device_id).await.map(drop)
}

#[tauri::command]
pub async fn set_shuffle(on: bool) -> Result<(), String> {
    control::transport(Cmd::Shuffle(on)).await.map(drop)
}

/// `mode` is "off", "context" or "track".
#[tauri::command]
pub async fn set_repeat(mode: String) -> Result<(), String> {
    if !matches!(mode.as_str(), "off" | "context" | "track") {
        return Err(format!("bad repeat mode: {mode}"));
    }
    control::transport(Cmd::Repeat(mode)).await.map(drop)
}

/// Play a whole context (playlist, album, Spotify mix) on a device, from
/// `track_uri` when given (it must be in the context, or Spotify starts it from the top).
#[tauri::command]
pub async fn play_context(device_id: String, context_uri: String, track_uri: Option<String>) -> Result<(), String> {
    let src = control::Source { context_uri: Some(context_uri), uris: vec![], track_uri };
    control::play(src, Some(device_id)).await.map(drop)
}

/// Append a track URI to the device's up-next queue: a Connect player command to This Mac.
#[tauri::command]
pub async fn add_to_queue(device_id: String, uri: String) -> Result<(), String> {
    if internal::own_device_id().as_deref() != Some(device_id.as_str()) {
        return Err(control::not_available_remote("queue"));
    }
    serve("add_to_queue", vec![(Primary, internal::add_to_queue(&device_id, &uri).boxed())]).await
}

/// Start playback of the given URIs on a specific device.
#[tauri::command]
pub async fn play_on_device(device_id: String, uris: Vec<String>) -> Result<(), String> {
    control::play(control::Source { uris, ..control::Source::default() }, Some(device_id)).await.map(drop)
}

#[tauri::command]
pub async fn resume(device_id: String) -> Result<(), String> {
    if internal::own_device_id().as_deref() != Some(device_id.as_str()) {
        return Err(control::not_available_remote("resume"));
    }
    control::transport(Cmd::Resume).await.map(drop)
}

/// Start `uri` at `position_ms` on a device: the way back when a plain resume is refused.
/// Inside its album/playlist context when there is one, so what plays next stays the same.
#[tauri::command]
pub async fn resume_at(device_id: String, context_uri: Option<String>, uri: String, position_ms: u64) -> Result<(), String> {
    if internal::own_device_id().as_deref() != Some(device_id.as_str()) {
        return Err(control::not_available_remote("resume"));
    }
    let engine = internal::engine().filter(|e| e.is_ready()).ok_or(control::NOT_READY)?;
    let offsettable = |c: &String| c.starts_with("spotify:playlist:") || c.starts_with("spotify:album:");
    let context_uri = context_uri.filter(offsettable);
    let uris = context_uri.is_none().then(|| vec![uri.clone()]);
    let position_ms = position_ms.min(u64::from(u32::MAX)) as u32;
    engine.load(crate::player::LoadSpec { context_uri, uris, track_uri: Some(uri), position_ms, play: true, shuffle: None, repeat: None })
}

#[tauri::command]
pub async fn pause() -> Result<(), String> {
    control::transport(Cmd::Pause).await.map(drop)
}
#[tauri::command]
pub async fn next_track() -> Result<(), String> {
    control::transport(Cmd::Next).await.map(drop)
}
#[tauri::command]
pub async fn previous_track() -> Result<(), String> {
    control::transport(Cmd::Previous).await.map(drop)
}
#[tauri::command]
pub async fn seek(position_ms: u64) -> Result<(), String> {
    control::transport(Cmd::Seek(position_ms.min(u64::from(u32::MAX)) as u32)).await.map(drop)
}

/// The user's real up-next queue (excludes the current track).
/// From the player's Connect cluster (the active device's next tracks).
#[tauri::command]
pub async fn get_queue() -> Result<Value, String> {
    serve("get_queue", vec![(Primary, internal::queue().boxed())]).await
}

/// Last 30 played tracks, newest first: `[{track, played_at, context_uri}]`.
/// Internal: the last track of each recently played context (Spotify's own history keeps
/// contexts, not every track).
#[tauri::command]
pub async fn get_recently_played() -> Result<Value, String> {
    let internal = with_api(Primary, |api| async move { Ok(Value::Array(api.recently_played().await?)) });
    serve("get_recently_played", vec![internal]).await
}

/// Search tracks and albums. Returns { tracks: [...], albums: [...] } with
/// only the fields the UI needs.
#[tauri::command]
pub async fn search(query: String) -> Result<Value, String> {
    if query.trim().is_empty() {
        return Ok(json!({ "tracks": [], "albums": [] }));
    }
    let q = query.as_str();
    serve(
        "search",
        vec![
            with_api(Primary, move |api| async move { api.search(q).await }),
            with_api(Fallback, move |api| async move { api.search_context(q).await }),
        ],
    )
    .await
}

/// One search page's size.
const SEARCH_PAGE: u32 = 10;
/// Spotify serves search results only while offset + limit <= 1000.
const SEARCH_MAX: u32 = 1000;

/// One page of search results of one kind ("track" | "album"), for the full
/// results page. Returns `{items: [Track] | [Album], has_more}`; albums have the
/// shape `search` gives them.
#[tauri::command]
pub async fn search_page(query: String, kind: String, offset: u32) -> Result<Value, String> {
    if kind != "track" && kind != "album" {
        return Err(format!("BAD_ARGS: unknown search kind {kind}"));
    }
    if query.trim().is_empty() || offset.saturating_add(SEARCH_PAGE) > SEARCH_MAX {
        return Ok(json!({ "items": [], "has_more": false }));
    }
    let (q, k) = (query.as_str(), kind.as_str());
    let internal = with_api(Primary, move |api| async move {
        let (items, got, total) = api.search_page(q, k, offset).await?;
        Ok(json!({ "items": items, "has_more": search_has_more(got, offset) && u64::from(offset) + (got as u64) < total }))
    });
    serve("search_page", vec![internal]).await
}

/// A page of `got` raw items at `offset` has a next page: it was full and the
/// next one still fits under Spotify's 1000-result cap.
fn search_has_more(got: usize, offset: u32) -> bool {
    got == SEARCH_PAGE as usize && offset.saturating_add(SEARCH_PAGE) < SEARCH_MAX
}

/// All of an album's tracks, with the album name and cover on each track. With an
/// `account`, cached forever under `album:<id>` (an album doesn't change): a hit
/// makes no request.
#[tauri::command]
pub async fn get_album_tracks(album_id: String, account: Option<String>) -> Result<Value, String> {
    let key = format!("album:{album_id}");
    if let Some(hit) = cached(&account, &key).await {
        return Ok(hit);
    }
    let id = album_id.as_str();
    let all = serve(
        "get_album_tracks",
        vec![
            with_api(Primary, move |api| async move { Ok(Value::Array(api.album(id).await?)) }),
            with_api(Fallback, move |api| async move { Ok(Value::Array(api.album_pb(id).await?)) }),
        ],
    )
    .await?;
    // an empty answer (an album gone from the catalogue) isn't kept forever
    if all.as_array().is_some_and(|a| !a.is_empty()) {
        cache_store(&account, key, &all);
    }
    Ok(all)
}

/// The album-info card's details (`parse::album_info`) of `album_id`, or of the album
/// `track_id` is on (the now-playing track carries no album id). With an `account`, cached
/// forever under `album-info:<id>` (and the track's album under `album-of:<track id>`).
#[tauri::command]
pub async fn get_album_info(album_id: Option<String>, track_id: Option<String>, account: Option<String>) -> Result<Value, String> {
    let is_id = |s: &String| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    let album_id = match album_id.filter(is_id) {
        Some(id) => id,
        None => album_of_track(&track_id.filter(is_id).ok_or("BAD_ARGS: no album or track id")?, &account).await?,
    };
    let key = format!("album-info:{album_id}");
    if let Some(hit) = cached(&account, &key).await {
        return Ok(hit);
    }
    let id = album_id.as_str();
    let info = serve("get_album_info", vec![with_api(Primary, move |api| async move { api.album_info(id).await })]).await?;
    cache_store(&account, key, &info);
    Ok(info)
}

/// The id of the album `track_id` is on, cached under `album-of:<track id>`.
pub(crate) async fn album_of_track(track_id: &str, account: &Option<String>) -> Result<String, String> {
    let key = format!("album-of:{track_id}");
    if let Some(Value::String(hit)) = cached(account, &key).await {
        return Ok(hit);
    }
    let id = serve("album_of_track", vec![with_api(Primary, move |api| async move { api.album_id_of_track(track_id).await })]).await?;
    cache_store(account, key, &json!(id));
    Ok(id)
}

// ---- library, taste, artists, mixes ------------------------------------------

pub(crate) const MAX_SAVED_TRACKS: usize = 1000;
const MAX_SAVED_ALBUMS: usize = 200;
const MAX_FOLLOWED: usize = 200;

/// How many songs are in Liked Songs: one request, for the Library row.
#[tauri::command]
pub async fn liked_count() -> Result<u64, String> {
    serve("liked_count", vec![with_api(Primary, |api| async move { api.liked_count().await })]).await
}

/// Liked Songs, newest first, capped at 1000: `{tracks, total}`. `total` is
/// the full count, so the UI can say when the cap cut some off. Cached as `liked`.
#[tauri::command]
pub async fn get_saved_tracks(account: Option<String>) -> Result<Value, String> {
    saved_tracks(MAX_SAVED_TRACKS, account).await
}

/// The newest `max` Liked Songs (at most 1000) and the full count. Only the whole list is cached.
pub(crate) async fn saved_tracks(max: usize, account: Option<String>) -> Result<Value, String> {
    let max = max.min(MAX_SAVED_TRACKS);
    let shape = |(tracks, total): (Vec<Value>, u64)| json!({ "tracks": tracks, "total": total });
    let out = serve(
        "get_saved_tracks",
        vec![
            with_api(Primary, move |api| async move { api.liked(max).await.map(shape) }),
            with_api(Fallback, move |api| async move { api.liked_pb(max).await.map(shape) }),
        ],
    )
    .await?;
    if max == MAX_SAVED_TRACKS {
        cache_store(&account, "liked".into(), &out);
    }
    Ok(out)
}

/// Saved albums, newest first, capped at 200. Cached as `albums`.
#[tauri::command]
pub async fn get_saved_albums(account: Option<String>) -> Result<Value, String> {
    let out = serve("get_saved_albums", vec![with_api(Primary, |api| async move { Ok(Value::Array(api.saved_albums(MAX_SAVED_ALBUMS).await?)) })]).await?;
    cache_store(&account, "albums".into(), &out);
    Ok(out)
}

/// Whether a track is in Liked Songs.
#[tauri::command]
pub async fn is_saved(track_id: String) -> Result<bool, String> {
    let id = track_id.as_str();
    serve("is_saved", vec![with_api(Primary, move |api| async move { api.is_saved(id).await })]).await
}

#[tauri::command]
pub async fn save_track(track_id: String) -> Result<(), String> {
    set_saved(track_id, true).await
}

#[tauri::command]
pub async fn unsave_track(track_id: String) -> Result<(), String> {
    set_saved(track_id, false).await
}

/// Liked Songs add/remove: a collection v2 write.
async fn set_saved(track_id: String, saved: bool) -> Result<(), String> {
    let id = track_id.as_str();
    let op = if saved { "save_track" } else { "unsave_track" };
    serve(op, vec![with_api(Primary, move |api| async move { api.set_saved(id, saved).await })]).await
}

/// Top tracks (`[Track]`) or artists (`[{id,name,image}]`), at most 50.
/// `kind`: "tracks"|"artists"; `range`: "short_term"|"medium_term"|"long_term".
/// Cached as `top:<kind>:<range>:<limit>`, with the limit actually used (20 when none given).
#[tauri::command]
pub async fn get_top(kind: String, range: String, limit: Option<u8>, account: Option<String>) -> Result<Value, String> {
    if !matches!(kind.as_str(), "tracks" | "artists") {
        return Err(format!("bad top kind: {kind}"));
    }
    if !matches!(range.as_str(), "short_term" | "medium_term" | "long_term") {
        return Err(format!("bad top range: {range}"));
    }
    // 20 for the Library group; the artist page asks for 50
    let limit = limit.unwrap_or(20).clamp(1, 50);
    let (k, r) = (kind.as_str(), range.as_str());
    let items = serve("get_top", vec![with_api(Primary, move |api| async move { Ok(Value::Array(api.top(k, r, limit).await?)) })]).await?;
    cache_store(&account, format!("top:{kind}:{range}:{limit}"), &items);
    Ok(items)
}

/// `{id, name, image, top_tracks}` for one artist. `top_tracks`: Spotify's popular tracks, at
/// most 10.
#[tauri::command]
pub async fn get_artist(artist_id: String) -> Result<Value, String> {
    let id = artist_id.as_str();
    serve(
        "get_artist",
        vec![
            with_api(Primary, move |api| async move { api.artist(id).await }),
            with_api(Fallback, move |api| async move { api.artist_pb(id).await }),
        ],
    )
    .await
}

/// An artist's albums and singles (first 50): `[{id, name, cover, year, kind}]`.
#[tauri::command]
pub async fn get_artist_albums(artist_id: String) -> Result<Value, String> {
    let id = artist_id.as_str();
    serve(
        "get_artist_albums",
        vec![
            with_api(Primary, move |api| async move { Ok(Value::Array(api.artist_albums(id, MAX_ARTIST_ALBUMS).await?)) }),
            with_api(Fallback, move |api| async move { Ok(Value::Array(api.artist_albums_pb(id, MAX_ARTIST_ALBUMS).await?)) }),
        ],
    )
    .await
}

/// Albums and singles on an artist page.
const MAX_ARTIST_ALBUMS: usize = 50;

/// Followed artists, capped at 200. Cached as `following`.
#[tauri::command]
pub async fn get_followed_artists(account: Option<String>) -> Result<Value, String> {
    let all = serve(
        "get_followed_artists",
        vec![
            with_api(Primary, |api| async move { Ok(Value::Array(api.followed_artists(MAX_FOLLOWED).await?)) }),
            with_api(Fallback, |api| async move { Ok(Value::Array(api.followed_artists_pb(MAX_FOLLOWED).await?)) }),
        ],
    )
    .await?;
    cache_store(&account, "following".into(), &all);
    Ok(all)
}

/// Name and cover of a Spotify-owned mix.
#[tauri::command]
pub async fn mix_info(playlist_id: String) -> Result<Value, String> {
    let id = playlist_id.as_str();
    serve(
        "mix_info",
        vec![
            with_api(Primary, move |api| async move { api.playlist_meta(id).await.map(|m| json!({ "name": m["name"], "cover": m["cover"] })) }),
            with_api(Fallback, move |api| async move { api.playlist_info_pb(id).await }),
        ],
    )
    .await
}

/// The user's playlists, as a flat JSON array of playlist objects. Cached as `playlists`.
#[tauri::command]
pub async fn get_playlists(account: Option<String>) -> Result<Value, String> {
    let all = serve(
        "get_playlists",
        vec![
            with_api(Primary, |api| async move { Ok(Value::Array(api.playlists().await?)) }),
            with_api(Fallback, |api| async move { Ok(Value::Array(api.rootlist().await?.iter().map(crate::pb::root_playlist).collect())) }),
        ],
    )
    .await?;
    cache_store(&account, "playlists".into(), &all);
    Ok(all)
}

/// A playlist's tracks. With `account`, the list is cached under `playlist:<id>:<snapshot_id>`
/// (a snapshot never changes). The current snapshot is asked first (the first page names it), so
/// a playlist edited in another app misses the cache; the caller's `snapshot_id` is the fallback
/// when the protobuf path gives none.
#[tauri::command]
pub async fn get_playlist_tracks(playlist_id: String, snapshot_id: Option<String>, account: Option<String>) -> Result<Value, String> {
    let (id, acc, snap) = (playlist_id.as_str(), &account, snapshot_id);
    // internal: the first page names the current snapshot, so a cache hit costs one request
    let internal = with_api(Primary, move |api| async move {
        let (first, rev) = api.playlist_page(id, 0, INTERNAL_PLAYLIST_PAGE).await?;
        let key = rev.map(|s| playlist_key(id, &s));
        if let Some(hit) = hit(acc, &key).await {
            return Ok((hit, None));
        }
        Ok((Value::Array(api.playlist_rest(id, first, INTERNAL_PLAYLIST_PAGE).await?), key))
    });
    let fallback = with_api(Fallback, move |api| async move {
        let (tracks, rev) = api.playlist_pb(id).await?;
        Ok((Value::Array(tracks), rev.or(snap).map(|s| playlist_key(id, &s))))
    });
    let (all, key) = serve("get_playlist_tracks", vec![internal, fallback]).await?;
    if let Some(key) = key {
        cache_store(&account, key, &all);
    }
    Ok(all)
}

/// Rows per internal playlist page.
const INTERNAL_PLAYLIST_PAGE: usize = 100;

fn playlist_key(playlist_id: &str, snapshot_id: &str) -> String {
    format!("playlist:{playlist_id}:{snapshot_id}")
}

/// The cached list under `key`, if any.
async fn hit(account: &Option<String>, key: &Option<String>) -> Option<Value> {
    cached(account, key.as_deref()?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_has_more_rules() {
        assert!(search_has_more(10, 0));
        assert!(search_has_more(10, 980));
        assert!(!search_has_more(10, 990)); // the next page would pass the 1000 cap
        assert!(!search_has_more(9, 0)); // a short page is the last
        assert!(!search_has_more(0, 0));
        assert!(!search_has_more(10, u32::MAX)); // no overflow
    }
}
