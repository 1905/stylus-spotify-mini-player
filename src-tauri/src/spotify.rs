//! The commands the UI calls for lists, search, library and Spotify Connect playback control.
//! Library, search and detail commands try Spotify's internal endpoints first (internal.rs,
//! with the player's own session) and fall back to the public Web API, which is rate-limited
//! for this app (quota.rs); playback control still uses the Web API.
//!
//! Errors starting with `AUTH_EXPIRED`, `NO_ACTIVE_DEVICE` or `RATE_LIMITED` are codes the
//! frontend matches with `startsWith`.

use crate::auth::{http, urlencode, valid_access_token};
use crate::internal::{
    self, serve, web, with_api,
    Source::{Fallback, Primary},
};
use futures_util::FutureExt;
use reqwest::Method;
use serde_json::{json, Value};

const API: &str = "https://api.spotify.com/v1";

/// Maps a failed HTTP status to an error string, adding a code prefix where
/// the frontend needs one.
fn api_error(status: u16, path: &str, body: &str) -> String {
    match status {
        401 => format!("AUTH_EXPIRED: Spotify API 401: {body}"),
        // only a 404 that is about the device: a player 404 can also mean "that context
        // doesn't exist" (a refused mix), which must not look like a missing device
        404 if path.starts_with("/me/player") && is_device_404(body) => {
            format!("NO_ACTIVE_DEVICE: Spotify API 404: {body}")
        }
        _ => format!("Spotify API {status}: {body}"),
    }
}

/// Spotify's no-device 404 says `"reason": "NO_ACTIVE_DEVICE"` or names the device
/// ("No active device found", "Device not found"); an empty body counts as one too.
fn is_device_404(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.trim().is_empty() || b.contains("no_active_device") || b.contains("device")
}

/// Turns an absolute `next` URL into a path for `get`.
fn api_path(url: &str) -> &str {
    url.strip_prefix(API).unwrap_or(url)
}

/// One Spotify request: the response body as text, or a mapped error. Every Web API call
/// comes through here: while Spotify rate-limits the app it fails at once with
/// `RATE_LIMITED:<secs>: …` and sends nothing (quota.rs); each request sent is counted.
async fn request(method: Method, path: &str, body: Option<Value>) -> Result<String, String> {
    crate::quota::check()?;
    let token = valid_access_token().await?;
    let req = http().request(method, format!("{API}{path}")).bearer_auth(token);
    let req = match body {
        Some(b) => req.json(&b),
        None => req.header("Content-Length", "0"),
    };
    crate::quota::record(path);
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.as_u16() == 429 {
        let retry_after = resp.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(str::to_string);
        return Err(crate::quota::on_429(retry_after.as_deref(), path));
    }
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(api_error(status.as_u16(), path, &text));
    }
    Ok(text)
}

/// GET returning the parsed body, or None on an empty body (204 No Content).
async fn get_opt(path: &str) -> Result<Option<Value>, String> {
    let text = request(Method::GET, path, None).await?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&text).map(Some).map_err(|e| e.to_string())
}

pub(crate) async fn get(path: &str) -> Result<Value, String> {
    Ok(get_opt(path).await?.unwrap_or(Value::Null))
}

/// A player command: whatever body comes back is ignored.
async fn command(method: Method, path: &str, body: Option<Value>) -> Result<(), String> {
    request(method, path, body).await.map(|_| ())
}

/// Every item of a paged list, following `next` from the first page.
async fn all_items(page: Value) -> Result<Vec<Value>, String> {
    items_up_to(page, usize::MAX).await
}

/// The first `max` items of a paged list, following `next` from the first page.
async fn items_up_to(mut page: Value, max: usize) -> Result<Vec<Value>, String> {
    let mut all = Vec::new();
    loop {
        all.extend(page["items"].as_array().into_iter().flatten().cloned());
        match page["next"].as_str() {
            Some(next_url) if all.len() < max => page = get(api_path(next_url)).await?,
            _ => {
                all.truncate(max);
                return Ok(all);
            }
        }
    }
}

/// How many page requests a list fetch keeps in flight.
const PAGE_CONCURRENCY: usize = 4;

/// The offsets still to fetch after the first page (offset 0) of an offset-paged
/// list holding `total` items, up to `max_items`.
fn page_offsets(total: usize, page_size: usize, max_items: usize) -> Vec<usize> {
    (page_size..total.min(max_items)).step_by(page_size.max(1)).collect()
}

/// Up to `max_items` items of an offset-paged list. The remaining pages are fetched
/// in parallel (`PAGE_CONCURRENCY` at a time) from `path_for_offset`, using the
/// first page's `total`; without a `total` it falls back to the serial `next` walk.
/// Any failed page fails the whole call.
async fn pages_parallel(
    first: Value,
    path_for_offset: impl Fn(usize) -> String,
    page_size: usize,
    max_items: usize,
) -> Result<Vec<Value>, String> {
    if first["total"].as_u64().is_none() {
        return items_up_to(first, max_items).await;
    }
    let fetch = |offset| {
        let path = path_for_offset(offset);
        async move { get(&path).await }
    };
    pages_with(first, page_size, max_items, PAGE_CONCURRENCY, fetch).await
}

/// `pages_parallel` with the page fetch injected: `fetch(offset)` returns that page.
/// Items come back in offset order whatever order the pages complete in.
pub(crate) async fn pages_with<F, Fut>(first: Value, page_size: usize, max_items: usize, concurrency: usize, fetch: F) -> Result<Vec<Value>, String>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    use futures_util::stream::{self, StreamExt, TryStreamExt};
    let total = first["total"].as_u64().unwrap_or(0) as usize;
    let mut all: Vec<Value> = first["items"].as_array().cloned().unwrap_or_default();
    let pages: Vec<Value> = stream::iter(page_offsets(total, page_size, max_items))
        .map(fetch)
        .buffered(concurrency.max(1))
        .try_collect()
        .await?;
    for page in pages {
        all.extend(page["items"].as_array().into_iter().flatten().cloned());
    }
    all.truncate(max_items);
    Ok(all)
}

/// Stores a list under the account's `key` in the disk cache, off the async thread.
/// No account, no cache: the key must be scoped to the verified account.
fn cache_store(account: &Option<String>, key: String, value: &Value) {
    let Some(account) = account.clone().filter(|a| !a.is_empty()) else { return };
    let value = value.clone();
    tokio::task::spawn_blocking(move || crate::cache::lists().put(&account, &key, &value));
}

/// The account's cached `key`, or None without an account.
async fn cached(account: &Option<String>, key: &str) -> Option<Value> {
    cache_get(account.clone()?, key.to_string()).await
}

/// The cached list under the account's `key`, or null. Never errors.
#[tauri::command]
pub async fn cache_get(account: String, key: String) -> Option<Value> {
    tokio::task::spawn_blocking(move || crate::cache::lists().get(&account, &key)).await.ok().flatten()
}

/// The signed-in user's Spotify id (`/me` id): the account the cache is scoped to. The player's
/// session knows it without a request; the Web API's `/me` while the player isn't up.
#[tauri::command]
pub async fn me_id() -> Result<String, String> {
    serve("me_id", vec![with_api(Primary, |api| async move { Ok(api.username()) }), web(web_me_id())]).await
}

async fn web_me_id() -> Result<String, String> {
    let me = get("/me").await?;
    me["id"].as_str().map(str::to_string).ok_or_else(|| "Spotify /me has no id".to_string())
}

// ---- Spotify Connect: control a real device -------------------------------

/// List the user's available Spotify Connect devices, unchanged:
/// `[{id, name, type, is_active, is_restricted, supports_volume, volume_percent, …}]`.
/// From the player's Connect cluster while it's up (this Mac always listed), else the Web API;
/// with the Web API blocked too, this Mac alone.
#[tauri::command]
pub async fn list_devices() -> Result<Value, String> {
    let cluster = (Primary, async { internal::devices() }.boxed());
    let own = (Fallback, async { internal::own_device_only() }.boxed());
    serve("list_devices", vec![cluster, web(web_list_devices()), own]).await
}

async fn web_list_devices() -> Result<Value, String> {
    let raw = get("/me/player/devices").await?;
    Ok(raw["devices"].clone())
}

/// Current playback state on the active device, simplified for the UI.
/// `{active:false}` when nothing is playing (204).
#[tauri::command]
pub async fn playback_state() -> Result<Value, String> {
    Ok(match get_opt("/me/player").await? {
        Some(s) => simplify_state(&s),
        None => json!({ "active": false }),
    })
}

/// Move playback to `device_id`; `play` starts it there, false keeps the current state.
#[tauri::command]
pub async fn transfer_playback(device_id: String, play: bool) -> Result<(), String> {
    command(Method::PUT, "/me/player", Some(json!({ "device_ids": [device_id], "play": play }))).await
}

#[tauri::command]
pub async fn set_volume(percent: u8, device_id: Option<String>) -> Result<(), String> {
    // with a device id, a transfer queued in between can't redirect it to the new device
    let device = device_id.map(|d| format!("&device_id={}", urlencode(&d))).unwrap_or_default();
    command(Method::PUT, &format!("/me/player/volume?volume_percent={}{device}", percent.min(100)), None).await
}

#[tauri::command]
pub async fn set_shuffle(on: bool) -> Result<(), String> {
    command(Method::PUT, &format!("/me/player/shuffle?state={on}"), None).await
}

/// `mode` is "off", "context" or "track".
#[tauri::command]
pub async fn set_repeat(mode: String) -> Result<(), String> {
    if !matches!(mode.as_str(), "off" | "context" | "track") {
        return Err(format!("bad repeat mode: {mode}"));
    }
    command(Method::PUT, &format!("/me/player/repeat?state={mode}"), None).await
}

/// Play a whole context (playlist, album, Spotify mix) on a device, from
/// `track_uri` when given (it must be in the context, or Spotify starts it from the top).
#[tauri::command]
pub async fn play_context(device_id: String, context_uri: String, track_uri: Option<String>) -> Result<(), String> {
    let path = format!("/me/player/play?device_id={}", urlencode(&device_id));
    command(Method::PUT, &path, Some(play_context_body(context_uri, track_uri))).await
}

fn play_context_body(context_uri: String, track_uri: Option<String>) -> Value {
    match track_uri {
        Some(uri) => json!({ "context_uri": context_uri, "offset": { "uri": uri } }),
        None => json!({ "context_uri": context_uri }),
    }
}

/// Append a track URI to the device's up-next queue.
/// This Mac: a Connect player command (no Web API); other devices: the Web API.
#[tauri::command]
pub async fn add_to_queue(device_id: String, uri: String) -> Result<(), String> {
    let path = format!("/me/player/queue?uri={}&device_id={}", urlencode(&uri), urlencode(&device_id));
    let mut steps = Vec::new();
    if internal::own_device_id().as_deref() == Some(device_id.as_str()) {
        steps.push((Primary, internal::add_to_queue(&device_id, &uri).boxed()));
    }
    steps.push(web(command(Method::POST, &path, None)));
    serve("add_to_queue", steps).await
}

/// Start playback of the given URIs on a specific device.
#[tauri::command]
pub async fn play_on_device(device_id: String, uris: Vec<String>) -> Result<(), String> {
    command(Method::PUT, &format!("/me/player/play?device_id={device_id}"), Some(json!({ "uris": uris }))).await
}

#[tauri::command]
pub async fn resume(device_id: String) -> Result<(), String> {
    command(Method::PUT, &format!("/me/player/play?device_id={device_id}"), None).await
}

/// Start `uri` at `position_ms` on a device: the way back when a plain resume is refused
/// because the session it would resume has expired (403 "Player command failed", or
/// "Device not found"). Inside its album/playlist context when there is one, so what
/// plays next stays the same; a context that won't take the offset falls back to the song.
#[tauri::command]
pub async fn resume_at(device_id: String, context_uri: Option<String>, uri: String, position_ms: u64) -> Result<(), String> {
    let path = format!("/me/player/play?device_id={}", urlencode(&device_id));
    let offsettable = |c: &str| c.starts_with("spotify:playlist:") || c.starts_with("spotify:album:");
    if let Some(ctx) = context_uri.filter(|c| offsettable(c)) {
        let body = json!({ "context_uri": ctx, "offset": { "uri": uri }, "position_ms": position_ms });
        match command(Method::PUT, &path, Some(body)).await {
            Err(e) if !e.starts_with("AUTH_EXPIRED") && !e.starts_with("NO_ACTIVE_DEVICE") => {}
            done => return done,
        }
    }
    command(Method::PUT, &path, Some(json!({ "uris": [uri], "position_ms": position_ms }))).await
}
#[tauri::command]
pub async fn pause() -> Result<(), String> {
    command(Method::PUT, "/me/player/pause", None).await
}
#[tauri::command]
pub async fn next_track() -> Result<(), String> {
    command(Method::POST, "/me/player/next", None).await
}
#[tauri::command]
pub async fn previous_track() -> Result<(), String> {
    command(Method::POST, "/me/player/previous", None).await
}
#[tauri::command]
pub async fn seek(position_ms: u64) -> Result<(), String> {
    command(Method::PUT, &format!("/me/player/seek?position_ms={position_ms}"), None).await
}

/// The user's real up-next queue (excludes the current track).
/// From the player's Connect cluster (the active device's next tracks) while it's up.
#[tauri::command]
pub async fn get_queue() -> Result<Value, String> {
    serve("get_queue", vec![(Primary, internal::queue().boxed()), web(web_get_queue())]).await
}

async fn web_get_queue() -> Result<Value, String> {
    let raw = get("/me/player/queue").await?;
    Ok(Value::Array(parse_queue(&raw)))
}

/// Last 30 played tracks, newest first: `[{track, played_at, context_uri}]`.
/// Internal: the last track of each recently played context (Spotify's own history keeps
/// contexts, not every track).
#[tauri::command]
pub async fn get_recently_played() -> Result<Value, String> {
    let internal = with_api(Primary, |api| async move { Ok(Value::Array(api.recently_played().await?)) });
    serve("get_recently_played", vec![internal, web(web_recently_played())]).await
}

async fn web_recently_played() -> Result<Value, String> {
    let raw = get("/me/player/recently-played?limit=30").await?;
    Ok(Value::Array(parse_recent(&raw)))
}

/// Search tracks and albums. Returns { tracks: [...], albums: [...] } with
/// only the fields the UI needs. Spotify rejects limit > 10 here (400).
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
            web(web_search(q)),
        ],
    )
    .await
}

async fn web_search(query: &str) -> Result<Value, String> {
    let path = format!("/search?type=track,album&limit=10&q={}", urlencode(query));
    let raw = get(&path).await?;
    Ok(json!({
        "tracks": search_items(&raw, "track"),
        "albums": search_items(&raw, "album"),
    }))
}

/// One search page's size: Spotify rejects limit > 10 on /search (400).
const SEARCH_PAGE: u32 = 10;
/// Spotify serves search results only while offset + limit <= 1000.
const SEARCH_MAX: u32 = 1000;

/// One page of search results of one kind ("track" | "album"), for the full
/// results page. Returns `{items: [Track] | [Album], has_more}`; albums have the
/// shape `search` gives them.
#[tauri::command]
pub async fn search_page(query: String, kind: String, offset: u32) -> Result<Value, String> {
    let path = search_page_path(&query, &kind, offset)?;
    if query.trim().is_empty() || offset.saturating_add(SEARCH_PAGE) > SEARCH_MAX {
        return Ok(json!({ "items": [], "has_more": false }));
    }
    let (q, k) = (query.as_str(), kind.as_str());
    let internal = with_api(Primary, move |api| async move {
        let (items, got, total) = api.search_page(q, k, offset).await?;
        Ok(json!({ "items": items, "has_more": search_has_more(got, offset) && u64::from(offset) + (got as u64) < total }))
    });
    serve("search_page", vec![internal, web(web_search_page(path, kind.clone(), offset))]).await
}

async fn web_search_page(path: String, kind: String, offset: u32) -> Result<Value, String> {
    let raw = get(&path).await?;
    // counted before nulls are dropped: a full page with a null in it still has a next page
    let got = raw[format!("{kind}s")]["items"].as_array().map_or(0, |a| a.len());
    Ok(json!({
        "items": search_items(&raw, &kind),
        "has_more": search_has_more(got, offset),
    }))
}

/// The /search path of one page; an unknown kind is refused.
fn search_page_path(query: &str, kind: &str, offset: u32) -> Result<String, String> {
    if kind != "track" && kind != "album" {
        return Err(format!("BAD_ARGS: unknown search kind {kind}"));
    }
    Ok(format!("/search?type={kind}&limit={SEARCH_PAGE}&offset={offset}&q={}", urlencode(query)))
}

/// A page of `got` raw items at `offset` has a next page: it was full and the
/// next one still fits under Spotify's 1000-result cap.
fn search_has_more(got: usize, offset: u32) -> bool {
    got == SEARCH_PAGE as usize && offset.saturating_add(SEARCH_PAGE) < SEARCH_MAX
}

/// The simplified, non-null items of one kind from a /search answer.
fn search_items(raw: &Value, kind: &str) -> Vec<Value> {
    let items = raw[format!("{kind}s")]["items"].as_array().into_iter().flatten().filter(|v| !v.is_null());
    if kind == "track" {
        items.map(simplify_track).collect()
    } else {
        items.map(simplify_album).collect()
    }
}

/// Spotify album object → `{id, name, artists, cover}`.
fn simplify_album(a: &Value) -> Value {
    json!({
        "id": a["id"],
        "name": a["name"],
        "artists": join_artists(&a["artists"]),
        "cover": first_image(&a["images"]),
    })
}

/// All of an album's tracks (follows `tracks.next`). Album tracks lack album
/// art, so the album name and cover are stamped onto each track. With an
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
            web(web_album_tracks(id)),
        ],
    )
    .await?;
    // an empty answer (an album gone from the catalogue) isn't kept forever
    if all.as_array().is_some_and(|a| !a.is_empty()) {
        cache_store(&account, key, &all);
    }
    Ok(all)
}

async fn web_album_tracks(album_id: &str) -> Result<Value, String> {
    let album = get(&format!("/albums/{album_id}")).await?;
    let cover = first_image(&album["images"]);
    let album_name = album["name"].clone();

    let all: Vec<Value> = all_items(album["tracks"].clone())
        .await?
        .iter()
        .filter(|t| !t.is_null())
        .map(|t| {
            let mut track = simplify_track(t);
            track["album"] = album_name.clone();
            track["cover"] = json!(cover);
            track
        })
        .collect();
    Ok(Value::Array(all))
}

// ---- library, taste, artists, mixes ------------------------------------------

const MAX_SAVED_TRACKS: usize = 1000;
const MAX_SAVED_ALBUMS: usize = 200;
const MAX_FOLLOWED: usize = 200;

/// How many songs are in Liked Songs: one request, for the Library row.
#[tauri::command]
pub async fn liked_count() -> Result<u64, String> {
    let web_count = async { Ok(get("/me/tracks?limit=1").await?["total"].as_u64().unwrap_or(0)) };
    serve("liked_count", vec![with_api(Primary, |api| async move { api.liked_count().await }), web(web_count)]).await
}

/// Liked Songs, newest first, capped at 1000: `{tracks, total}`. `total` is
/// the full count, so the UI can say when the cap cut some off. Cached as `liked`.
#[tauri::command]
pub async fn get_saved_tracks(account: Option<String>) -> Result<Value, String> {
    let shape = |(tracks, total): (Vec<Value>, u64)| json!({ "tracks": tracks, "total": total });
    let out = serve(
        "get_saved_tracks",
        vec![
            with_api(Primary, move |api| async move { api.liked(MAX_SAVED_TRACKS).await.map(shape) }),
            with_api(Fallback, move |api| async move { api.liked_pb(MAX_SAVED_TRACKS).await.map(shape) }),
            web(web_saved_tracks()),
        ],
    )
    .await?;
    cache_store(&account, "liked".into(), &out);
    Ok(out)
}

async fn web_saved_tracks() -> Result<Value, String> {
    let first = get("/me/tracks?limit=50").await?;
    let total = first["total"].clone();
    let tracks: Vec<Value> = pages_parallel(first, |o| format!("/me/tracks?limit=50&offset={o}"), 50, MAX_SAVED_TRACKS)
        .await?
        .iter()
        .map(track_of_row)
        .filter(|t| !t.is_null())
        .map(simplify_track)
        .collect();
    Ok(json!({ "tracks": tracks, "total": total }))
}

/// Saved albums, newest first, capped at 200. Cached as `albums`.
#[tauri::command]
pub async fn get_saved_albums(account: Option<String>) -> Result<Value, String> {
    let out = serve(
        "get_saved_albums",
        vec![with_api(Primary, |api| async move { Ok(Value::Array(api.saved_albums(MAX_SAVED_ALBUMS).await?)) }), web(web_saved_albums())],
    )
    .await?;
    cache_store(&account, "albums".into(), &out);
    Ok(out)
}

async fn web_saved_albums() -> Result<Value, String> {
    let first = get("/me/albums?limit=50").await?;
    let rows = pages_parallel(first, |o| format!("/me/albums?limit=50&offset={o}"), 50, MAX_SAVED_ALBUMS).await?;
    Ok(Value::Array(parse_saved_albums(&json!({ "items": rows }))))
}

// Liked Songs. Since Feb 2026 the per-type /me/tracks writes and /contains are 403 for
// development-mode apps; /me/library takes Spotify URIs, and only in the query string
// (a JSON body gives 400 "Missing required field: uris"). Checked live 2026-10-02.
fn library_path(path: &str, track_id: &str) -> String {
    format!("{path}?uris={}", urlencode(&format!("spotify:track:{track_id}")))
}

/// Whether a track is in Liked Songs.
#[tauri::command]
pub async fn is_saved(track_id: String) -> Result<bool, String> {
    let id = track_id.as_str();
    let web_contains = async move {
        let v = get(&library_path("/me/library/contains", id)).await?;
        Ok(v[0].as_bool().unwrap_or(false))
    };
    serve("is_saved", vec![with_api(Primary, move |api| async move { api.is_saved(id).await }), web(web_contains)]).await
}

#[tauri::command]
pub async fn save_track(track_id: String) -> Result<(), String> {
    set_saved(track_id, true).await
}

#[tauri::command]
pub async fn unsave_track(track_id: String) -> Result<(), String> {
    set_saved(track_id, false).await
}

/// Liked Songs add/remove: collection v2 write (internal), else the Web API.
async fn set_saved(track_id: String, saved: bool) -> Result<(), String> {
    let id = track_id.as_str();
    let method = if saved { Method::PUT } else { Method::DELETE };
    let op = if saved { "save_track" } else { "unsave_track" };
    serve(op, vec![with_api(Primary, move |api| async move { api.set_saved(id, saved).await }), web(command(method, &library_path("/me/library", id), None))]).await
}

/// Top tracks (`[Track]`) or artists (`[{id,name,image}]`), at most 20.
/// `kind`: "tracks"|"artists"; `range`: "short_term"|"medium_term"|"long_term".
/// Cached as `top:<kind>:<range>:<limit>`, with the limit actually used (20 when none given).
#[tauri::command]
pub async fn get_top(kind: String, range: String, limit: Option<u8>, account: Option<String>) -> Result<Value, String> {
    let simplify: fn(&Value) -> Value = match kind.as_str() {
        "tracks" => simplify_track,
        "artists" => simplify_artist,
        _ => return Err(format!("bad top kind: {kind}")),
    };
    if !matches!(range.as_str(), "short_term" | "medium_term" | "long_term") {
        return Err(format!("bad top range: {range}"));
    }
    // 20 for the Library group; the artist page asks for 50, Spotify's max (51 → 400)
    let limit = limit.unwrap_or(20).clamp(1, 50);
    let (k, r) = (kind.as_str(), range.as_str());
    let web_top = async move {
        let raw = get(&format!("/me/top/{k}?time_range={r}&limit={limit}")).await?;
        Ok(Value::Array(raw["items"].as_array().into_iter().flatten().filter(|x| !x.is_null()).map(simplify).collect()))
    };
    let items = serve("get_top", vec![with_api(Primary, move |api| async move { Ok(Value::Array(api.top(k, r, limit).await?)) }), web(web_top)]).await?;
    cache_store(&account, format!("top:{kind}:{range}:{limit}"), &items);
    Ok(items)
}

/// `{id, name, image, top_tracks}` for one artist. `top_tracks` (Spotify's popular tracks, at
/// most 10) is empty from the Web API, which no longer serves them.
#[tauri::command]
pub async fn get_artist(artist_id: String) -> Result<Value, String> {
    let id = artist_id.as_str();
    serve(
        "get_artist",
        vec![
            with_api(Primary, move |api| async move { api.artist(id).await }),
            with_api(Fallback, move |api| async move { api.artist_pb(id).await }),
            web(web_artist(id)),
        ],
    )
    .await
}

async fn web_artist(artist_id: &str) -> Result<Value, String> {
    let mut a = simplify_artist(&get(&format!("/artists/{}", urlencode(artist_id))).await?);
    a["top_tracks"] = json!([]);
    Ok(a)
}

/// An artist's albums and singles (first 50): `[{id, name, cover, year, kind}]`.
/// Spotify caps this endpoint at 10 per page (11+ → 400 "Invalid limit",
/// checked live 2026-10-02), so it follows `next`.
#[tauri::command]
pub async fn get_artist_albums(artist_id: String) -> Result<Value, String> {
    let id = artist_id.as_str();
    serve(
        "get_artist_albums",
        vec![
            with_api(Primary, move |api| async move { Ok(Value::Array(api.artist_albums(id, MAX_ARTIST_ALBUMS).await?)) }),
            with_api(Fallback, move |api| async move { Ok(Value::Array(api.artist_albums_pb(id, MAX_ARTIST_ALBUMS).await?)) }),
            web(web_artist_albums(id)),
        ],
    )
    .await
}

/// Albums and singles on an artist page.
const MAX_ARTIST_ALBUMS: usize = 50;

async fn web_artist_albums(artist_id: &str) -> Result<Value, String> {
    let path = format!("/artists/{}/albums?include_groups=album,single&limit=10", urlencode(artist_id));
    let items = items_up_to(get(&path).await?, MAX_ARTIST_ALBUMS).await?;
    Ok(Value::Array(parse_artist_albums(&json!({ "items": items }))))
}

/// Followed artists, capped at 200. This endpoint pages by cursor
/// (`artists.cursors.after`), not offset, so it stays serial. Cached as `following`.
#[tauri::command]
pub async fn get_followed_artists(account: Option<String>) -> Result<Value, String> {
    let all = serve(
        "get_followed_artists",
        vec![
            with_api(Primary, |api| async move { Ok(Value::Array(api.followed_artists(MAX_FOLLOWED).await?)) }),
            with_api(Fallback, |api| async move { Ok(Value::Array(api.followed_artists_pb(MAX_FOLLOWED).await?)) }),
            web(web_followed_artists()),
        ],
    )
    .await?;
    cache_store(&account, "following".into(), &all);
    Ok(all)
}

async fn web_followed_artists() -> Result<Value, String> {
    const FIRST: &str = "/me/following?type=artist&limit=50";
    let mut page = get(FIRST).await?;
    let mut all = Vec::new();
    loop {
        let artists = &page["artists"];
        all.extend(artists["items"].as_array().into_iter().flatten().filter(|a| !a.is_null()).map(simplify_artist));
        let after = artists["cursors"]["after"].as_str();
        match (artists["next"].as_str(), after) {
            (Some(_), Some(after)) if all.len() < MAX_FOLLOWED => {
                page = get(&format!("{FIRST}&after={}", urlencode(after))).await?;
            }
            _ => break,
        }
    }
    all.truncate(MAX_FOLLOWED);
    Ok(Value::Array(all))
}

/// Name and cover of a Spotify-owned mix: internal endpoints give both. The Web API hides these
/// playlists' details (best effort there: only the images endpoint sometimes answers, and a
/// radio mix's image URL holds its seed artist).
#[tauri::command]
pub async fn mix_info(playlist_id: String) -> Result<Value, String> {
    let id = playlist_id.as_str();
    serve(
        "mix_info",
        vec![
            with_api(Primary, move |api| async move { api.playlist_info(id).await }),
            with_api(Fallback, move |api| async move { api.playlist_info_pb(id).await }),
            web(web_mix_info(id)),
        ],
    )
    .await
}

async fn web_mix_info(playlist_id: &str) -> Result<Value, String> {
    let images = match get(&format!("/playlists/{}/images", urlencode(playlist_id))).await {
        Ok(v) => v,
        Err(e) if e.starts_with("Spotify API 404") => Value::Null,
        Err(e) => return Err(e),
    };
    let cover = first_image(&images);
    let mut name = "Spotify mix".to_string();
    if let Some(artist_id) = cover.as_deref().and_then(mix_artist_id) {
        // best effort: a failed artist lookup keeps the generic name
        if let Ok(artist) = web_artist(&artist_id).await {
            if let Some(n) = artist["name"].as_str() {
                name = format!("{n} Radio");
            }
        }
    }
    Ok(json!({ "name": name, "cover": cover }))
}

// ---- field simplifiers -----------------------------------------------------

fn join_artists(v: &Value) -> String {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn first_image(v: &Value) -> Option<String> {
    v.as_array()
        .and_then(|imgs| imgs.first())
        .and_then(|img| img["url"].as_str())
        .map(|s| s.to_string())
}

/// Spotify track object → `Track`
/// `{id, uri, name, artists, artist_list:[{id,name}], album, cover, duration_ms}`.
fn simplify_track(t: &Value) -> Value {
    let artist_list: Vec<Value> = t["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|a| json!({ "id": a["id"], "name": a["name"] }))
        .collect();
    json!({
        "id": t["id"],
        "uri": t["uri"],
        "name": t["name"],
        "artists": join_artists(&t["artists"]),
        "artist_list": artist_list,
        "album": t["album"]["name"],
        "cover": first_image(&t["album"]["images"]),
        "duration_ms": t["duration_ms"],
    })
}

/// The track inside a playlist/history row. This account sometimes serves it
/// under `item` (new) instead of the documented `track`.
fn track_of_row(row: &Value) -> &Value {
    if !row["item"].is_null() {
        &row["item"]
    } else {
        &row["track"]
    }
}

/// `/me/player/queue` body → simplified tracks from `queue` (nulls skipped).
fn parse_queue(v: &Value) -> Vec<Value> {
    v["queue"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| !t.is_null())
        .map(simplify_track)
        .collect()
}

/// `/me/player/recently-played` body → `[{track, played_at, context_uri}]`,
/// rows without a track skipped. `context_uri` is how Spotify mixes show up.
fn parse_recent(v: &Value) -> Vec<Value> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let t = track_of_row(row);
            (!t.is_null()).then(|| json!({
                "track": simplify_track(t),
                "played_at": row["played_at"],
                "context_uri": row["context"]["uri"],
            }))
        })
        .collect()
}

/// `/me/player` body → the UI's playback state. `track` is null during ads
/// and podcast episodes: Spotify sends no item for them here.
fn simplify_state(s: &Value) -> Value {
    json!({
        "active": true,
        "is_playing": s["is_playing"],
        "progress_ms": s["progress_ms"],
        "device_id": s["device"]["id"],
        "device_name": s["device"]["name"],
        "track": if s["item"].is_null() { Value::Null } else { simplify_track(&s["item"]) },
        "shuffle": s["shuffle_state"].as_bool().unwrap_or(false),
        "repeat": s["repeat_state"].as_str().unwrap_or("off"),
        "volume_percent": s["device"]["volume_percent"],
        "supports_volume": s["device"]["supports_volume"].as_bool().unwrap_or(false),
        "context_uri": s["context"]["uri"],
    })
}

/// `/me/albums` body (rows `{added_at, album}`) → `[{id, name, artists, cover, total_tracks}]`.
fn parse_saved_albums(v: &Value) -> Vec<Value> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| &row["album"])
        .filter(|a| !a.is_null())
        .map(|a| {
            let mut album = simplify_album(a);
            album["total_tracks"] = a["total_tracks"].clone();
            album
        })
        .collect()
}

/// Spotify artist object → `{id, name, image}` (first image, or null).
fn simplify_artist(a: &Value) -> Value {
    json!({ "id": a["id"], "name": a["name"], "image": first_image(&a["images"]) })
}

/// `/artists/{id}/albums` body → `[{id, name, cover, year, kind}]`. `kind` is
/// "single" or "album" (compilations count as albums).
fn parse_artist_albums(v: &Value) -> Vec<Value> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|a| !a.is_null())
        .map(|a| {
            let group = a["album_group"].as_str().or_else(|| a["album_type"].as_str());
            let kind = if group == Some("single") { "single" } else { "album" };
            let year = a["release_date"].as_str().map(|d| d.chars().take(4).collect::<String>());
            json!({
                "id": a["id"],
                "name": a["name"],
                "cover": first_image(&a["images"]),
                "year": year,
                "kind": kind,
            })
        })
        .collect()
}

/// The seed artist id in a radio mix's image URL (`…/radio/artist/<id>/…`).
fn mix_artist_id(image_url: &str) -> Option<String> {
    let rest = image_url.split_once("radio/artist/")?.1;
    let id = rest.split(['/', '?', '#']).next().unwrap_or("");
    (!id.is_empty()).then(|| id.to_string())
}

/// GET /me/playlists, following `next` until all pages are collected.
/// Returns a flat JSON array of playlist objects. Cached as `playlists`.
#[tauri::command]
pub async fn get_playlists(account: Option<String>) -> Result<Value, String> {
    let all = serve(
        "get_playlists",
        vec![
            with_api(Primary, |api| async move { Ok(Value::Array(api.playlists().await?)) }),
            with_api(Fallback, |api| async move { Ok(Value::Array(api.rootlist().await?.iter().map(crate::pb::root_playlist).collect())) }),
            web(web_playlists()),
        ],
    )
    .await?;
    cache_store(&account, "playlists".into(), &all);
    Ok(all)
}

async fn web_playlists() -> Result<Value, String> {
    let first = get("/me/playlists?limit=50").await?;
    let all = all_items(first)
        .await?
        .into_iter()
        .map(|mut pl| {
            // This account's API serves the track count under `items.total`,
            // not the documented `tracks.total`. Normalize so the frontend
            // always reads `tracks.total`.
            let total = pl["tracks"]["total"]
                .as_u64()
                .or_else(|| pl["items"]["total"].as_u64())
                .unwrap_or(0);
            pl["tracks"] = json!({ "total": total });
            pl
        })
        .collect();
    Ok(Value::Array(all))
}

/// Rows per playlist page: `limit=100` on `/items` is a 403 for this app.
const PLAYLIST_PAGE: usize = 50;

/// One page of a playlist's `/items`. `total` is in the projection: the parallel
/// paging needs it to know the offsets.
fn playlist_items_path(playlist_id: &str, offset: usize) -> String {
    let fields = "total,next,items(added_at,item(id,uri,name,duration_ms,artists(id,name),album(name,images)),track(id,uri,name,duration_ms,artists(id,name),album(name,images)))";
    format!("/playlists/{playlist_id}/items?limit={PLAYLIST_PAGE}&offset={offset}&fields={}", urlencode(fields))
}

/// Paginated playlist tracks. This account's API uses `/items` (the `/tracks`
/// endpoint 403s) and may nest each track under `item` instead of `track`.
/// We request both field spellings and read whichever the response provides.
/// With `account`, the list is cached under `playlist:<id>:<snapshot_id>` (a snapshot never
/// changes). The current snapshot is asked first (one tiny request), so a playlist edited in
/// another app misses the cache; the caller's `snapshot_id` is the fallback when that fails.
#[tauri::command]
pub async fn get_playlist_tracks(playlist_id: String, snapshot_id: Option<String>, account: Option<String>) -> Result<Value, String> {
    let (id, acc, snap) = (playlist_id.as_str(), &account, snapshot_id.clone());
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
    let (all, key) = serve("get_playlist_tracks", vec![internal, fallback, web(web_playlist_tracks(id, snapshot_id, acc))]).await?;
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

/// Web API: the list and the cache key to store it under (None after a cache hit).
async fn web_playlist_tracks(playlist_id: &str, snapshot_id: Option<String>, account: &Option<String>) -> Result<(Value, Option<String>), String> {
    let current = get(&format!("/playlists/{playlist_id}?fields=snapshot_id"))
        .await
        .ok()
        .and_then(|v| v["snapshot_id"].as_str().map(String::from));
    let key = current.or(snapshot_id).map(|s| playlist_key(playlist_id, &s));
    if let Some(hit) = hit(account, &key).await {
        return Ok((hit, None));
    }
    let first = get(&playlist_items_path(playlist_id, 0)).await?;
    let all = pages_parallel(first, |o| playlist_items_path(playlist_id, o), PLAYLIST_PAGE, usize::MAX)
        .await?
        .iter()
        .map(track_of_row)
        .filter(|t| !t.is_null()) // null = removed/unavailable
        .map(simplify_track)
        .collect();
    Ok((Value::Array(all), key))
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw_track(n: &str) -> Value {
        json!({
            "id": n, "uri": format!("spotify:track:{n}"), "name": format!("Song {n}"),
            "duration_ms": 1000,
            "artists": [{"id": "a1", "name": "A"}, {"id": "b2", "name": "B"}],
            "album": {"name": "Alb", "images": [{"url": "https://i.scdn.co/x"}, {"url": "small"}]}
        })
    }

    #[test]
    fn library_path_uses_track_uri_in_query() {
        assert_eq!(library_path("/me/library", "abc"), "/me/library?uris=spotify%3Atrack%3Aabc");
    }

    #[test]
    fn search_page_path_shape() {
        assert_eq!(
            search_page_path("the xx", "track", 20).unwrap(),
            "/search?type=track&limit=10&offset=20&q=the%20xx"
        );
        assert_eq!(search_page_path("a&b", "album", 0).unwrap(), "/search?type=album&limit=10&offset=0&q=a%26b");
        assert!(search_page_path("x", "artist", 0).unwrap_err().starts_with("BAD_ARGS"));
    }

    #[test]
    fn search_has_more_rules() {
        assert!(search_has_more(10, 0));
        assert!(search_has_more(10, 980));
        assert!(!search_has_more(10, 990)); // the next page would pass the 1000 cap
        assert!(!search_has_more(9, 0)); // a short page is the last
        assert!(!search_has_more(0, 0));
        assert!(!search_has_more(10, u32::MAX)); // no overflow
    }

    #[test]
    fn search_items_drop_nulls() {
        let raw = json!({
            "tracks": {"items": [raw_track("1"), null, raw_track("2")]},
            "albums": {"items": [null, {"id": "al", "name": "Alb", "artists": [{"name": "A"}], "images": [{"url": "u"}]}]}
        });
        let t = search_items(&raw, "track");
        assert_eq!(t.len(), 2);
        assert_eq!(t[1]["uri"], "spotify:track:2");
        let a = search_items(&raw, "album");
        assert_eq!(a, vec![json!({"id": "al", "name": "Alb", "artists": "A", "cover": "u"})]);
        assert!(search_items(&json!({}), "track").is_empty());
    }

    #[test]
    fn urlencode_basic() {
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn join_artists_basic() {
        assert_eq!(join_artists(&json!([{"name":"A"},{"name":"B"}])), "A, B");
        assert_eq!(join_artists(&Value::Null), "");
    }

    #[test]
    fn simplify_track_shape() {
        let t = simplify_track(&raw_track("1"));
        assert_eq!(
            t,
            json!({"id":"1","uri":"spotify:track:1","name":"Song 1","artists":"A, B",
                   "artist_list":[{"id":"a1","name":"A"},{"id":"b2","name":"B"}],
                   "album":"Alb","cover":"https://i.scdn.co/x","duration_ms":1000})
        );
    }

    #[test]
    fn simplify_track_no_images() {
        let t = simplify_track(&json!({"id":"x","album":{"name":"N","images":[]}}));
        assert!(t["cover"].is_null());
        assert_eq!(t["artist_list"], json!([]));
    }

    #[test]
    fn simplify_state_full() {
        let v = json!({
            "is_playing": true, "progress_ms": 42,
            "shuffle_state": true, "repeat_state": "context",
            "device": {"id": "d1", "name": "Mac", "volume_percent": 55, "supports_volume": true},
            "context": {"uri": "spotify:playlist:p1"},
            "item": raw_track("1")
        });
        let s = simplify_state(&v);
        assert_eq!(s["active"], true);
        assert_eq!(s["is_playing"], true);
        assert_eq!(s["progress_ms"], 42);
        assert_eq!(s["device_id"], "d1");
        assert_eq!(s["device_name"], "Mac");
        assert_eq!(s["track"]["id"], "1");
        assert_eq!(s["shuffle"], true);
        assert_eq!(s["repeat"], "context");
        assert_eq!(s["volume_percent"], 55);
        assert_eq!(s["supports_volume"], true);
        assert_eq!(s["context_uri"], "spotify:playlist:p1");
    }

    #[test]
    fn simplify_state_sparse() {
        let v = json!({
            "is_playing": false, "progress_ms": 0,
            "shuffle_state": false, "repeat_state": "track",
            "device": {"id": "d2", "name": "Amp", "volume_percent": null},
            "context": null,
            "item": null
        });
        let s = simplify_state(&v);
        assert!(s["track"].is_null());
        assert_eq!(s["shuffle"], false);
        assert_eq!(s["repeat"], "track");
        assert!(s["volume_percent"].is_null());
        assert_eq!(s["supports_volume"], false);
        assert!(s["context_uri"].is_null());
        // missing fields entirely
        let s = simplify_state(&json!({"device": {}}));
        assert_eq!(s["shuffle"], false);
        assert_eq!(s["repeat"], "off");
        assert_eq!(s["supports_volume"], false);
    }

    #[test]
    fn parse_recent_context_uri() {
        let v = json!({"items": [
            {"track": raw_track("1"), "played_at": "t1", "context": {"uri": "spotify:playlist:m1"}},
            {"track": raw_track("2"), "played_at": "t2", "context": null},
            {"track": raw_track("3"), "played_at": "t3"}
        ]});
        let r = parse_recent(&v);
        assert_eq!(r[0]["context_uri"], "spotify:playlist:m1");
        assert!(r[1]["context_uri"].is_null());
        assert!(r[2]["context_uri"].is_null());
    }

    #[test]
    fn parse_saved_albums_shape() {
        let v = json!({"items": [
            {"added_at": "x", "album": {"id": "al1", "name": "Black Sands",
              "artists": [{"name": "Bonobo"}], "images": [{"url": "c1"}], "total_tracks": 12}},
            {"added_at": "y", "album": null}
        ]});
        let a = parse_saved_albums(&v);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0], json!({"id":"al1","name":"Black Sands","artists":"Bonobo","cover":"c1","total_tracks":12}));
        assert!(parse_saved_albums(&json!({})).is_empty());
    }

    #[test]
    fn simplify_artist_shape() {
        let a = simplify_artist(&json!({"id": "x", "name": "Bonobo", "images": [{"url": "big"}, {"url": "small"}], "genres": []}));
        assert_eq!(a, json!({"id":"x","name":"Bonobo","image":"big"}));
        let a = simplify_artist(&json!({"id": "y", "name": "Nobody", "images": []}));
        assert!(a["image"].is_null());
    }

    #[test]
    fn parse_artist_albums_shape() {
        let v = json!({"items": [
            {"id": "1", "name": "LP", "images": [{"url": "c"}], "release_date": "2010-03-29",
             "album_group": "album", "album_type": "album"},
            {"id": "2", "name": "One", "images": [], "release_date": "2019",
             "album_type": "single"},
            {"id": "3", "name": "Best of", "images": [], "release_date": "2015-01",
             "album_type": "compilation"},
            {"id": "4", "name": "Grp", "images": [], "release_date": "2001-01-01",
             "album_group": "single", "album_type": "album"},
            null
        ]});
        let a = parse_artist_albums(&v);
        assert_eq!(a.len(), 4);
        assert_eq!(a[0], json!({"id":"1","name":"LP","cover":"c","year":"2010","kind":"album"}));
        assert_eq!(a[1]["kind"], "single");
        assert_eq!(a[1]["year"], "2019");
        assert!(a[1]["cover"].is_null());
        assert_eq!(a[2]["kind"], "album");
        assert_eq!(a[3]["kind"], "single");
    }

    #[test]
    fn mix_artist_id_cases() {
        assert_eq!(
            mix_artist_id("https://seeded-session-images.scdn.co/v2/img/radio/artist/0cmWgDlu9CwTgxPhf403hb/en?x=1"),
            Some("0cmWgDlu9CwTgxPhf403hb".to_string())
        );
        assert_eq!(mix_artist_id("https://x/radio/artist/abc123?size=640"), Some("abc123".to_string()));
        assert_eq!(mix_artist_id("https://x/radio/artist/abc123"), Some("abc123".to_string()));
        assert_eq!(mix_artist_id("https://mosaic.scdn.co/640/ab67"), None);
        assert_eq!(mix_artist_id("https://x/radio/artist/"), None);
        assert_eq!(mix_artist_id(""), None);
    }

    #[test]
    fn track_of_row_prefers_item() {
        let item_row = json!({"item": raw_track("i"), "track": raw_track("t")});
        assert_eq!(track_of_row(&item_row)["id"], "i");
        let track_row = json!({"item": null, "track": raw_track("t")});
        assert_eq!(track_of_row(&track_row)["id"], "t");
        let old_row = json!({"track": raw_track("t")});
        assert_eq!(track_of_row(&old_row)["id"], "t");
        assert!(track_of_row(&json!({})).is_null());
    }

    #[test]
    fn parse_queue_sample() {
        let v = json!({"currently_playing": raw_track("now"), "queue": [raw_track("1"), null, raw_track("2")]});
        let q = parse_queue(&v);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0]["id"], "1");
        assert_eq!(q[1]["artists"], "A, B");
        assert!(parse_queue(&json!({})).is_empty());
    }

    #[test]
    fn parse_recent_both_shapes() {
        let v = json!({"items": [
            {"track": raw_track("1"), "played_at": "2026-10-01T15:02:11Z", "context": null},
            {"item": raw_track("2"), "played_at": "2026-10-01T15:00:00Z"},
            {"track": null, "played_at": "2026-10-01T14:00:00Z"}
        ]});
        let r = parse_recent(&v);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["track"]["id"], "1");
        assert_eq!(r[0]["played_at"], "2026-10-01T15:02:11Z");
        assert_eq!(r[1]["track"]["id"], "2");
        assert_eq!(r[1]["track"]["cover"], "https://i.scdn.co/x");
    }

    #[test]
    fn api_error_codes() {
        assert!(api_error(401, "/me/playlists", "x").starts_with("AUTH_EXPIRED"));
        let no_device = r#"{"error":{"status":404,"message":"Player command failed: No active device found","reason":"NO_ACTIVE_DEVICE"}}"#;
        assert!(api_error(404, "/me/player/play?device_id=1", no_device).starts_with("NO_ACTIVE_DEVICE"));
        assert!(api_error(404, "/me/player", "").starts_with("NO_ACTIVE_DEVICE"));
        let gone = r#"{"error":{"status":404,"message":"Resource not found"}}"#;
        assert_eq!(api_error(404, "/me/player/play?device_id=1", gone), format!("Spotify API 404: {gone}"));
        assert_eq!(api_error(404, "/albums/x", "nope"), "Spotify API 404: nope");
        assert_eq!(api_error(500, "/me/player", "boom"), "Spotify API 500: boom");
    }

    #[test]
    fn page_offsets_from_total() {
        assert_eq!(page_offsets(759, 50, usize::MAX).len(), 15);
        assert_eq!(page_offsets(759, 50, usize::MAX)[..3], [50, 100, 150]);
        assert_eq!(*page_offsets(759, 50, usize::MAX).last().unwrap(), 750);
        assert_eq!(page_offsets(100, 50, usize::MAX), vec![50]);
        assert_eq!(page_offsets(50, 50, usize::MAX), Vec::<usize>::new());
        assert_eq!(page_offsets(0, 50, usize::MAX), Vec::<usize>::new());
        // capped: Liked Songs stops at 1000
        assert_eq!(*page_offsets(5000, 50, 1000).last().unwrap(), 950);
        assert_eq!(page_offsets(5000, 50, 1000).len(), 19);
    }

    /// A page of `n` numbered items starting at `offset`.
    fn page_at(offset: usize, n: usize) -> Value {
        json!({ "items": (offset..offset + n).collect::<Vec<_>>() })
    }

    #[tokio::test]
    async fn pages_keep_offset_order_when_completing_out_of_order() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        let total = 230;
        let first = json!({ "total": total, "items": (0..50).collect::<Vec<_>>() });
        let asked = Arc::new(Mutex::new(Vec::new()));
        let (in_flight, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let fetch = |offset: usize| {
            let (asked, in_flight, peak) = (asked.clone(), in_flight.clone(), peak.clone());
            async move {
                asked.lock().unwrap().push(offset);
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // later offsets finish first
                tokio::time::sleep(std::time::Duration::from_millis(60 - offset as u64 / 5)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(page_at(offset, (total - offset).min(50)))
            }
        };
        let items = pages_with(first, 50, usize::MAX, 4, fetch).await.unwrap();
        assert_eq!(items, (0..total).map(|i| json!(i)).collect::<Vec<_>>());
        let mut asked = asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, vec![50, 100, 150, 200]);
        assert!(peak.load(Ordering::SeqCst) <= 4);
        assert!(peak.load(Ordering::SeqCst) > 1, "pages run in parallel");
    }

    #[tokio::test]
    async fn pages_concurrency_is_capped() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let (in_flight, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let fetch = |offset: usize| {
            let (in_flight, peak) = (in_flight.clone(), peak.clone());
            async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(page_at(offset, 50))
            }
        };
        let first = json!({ "total": 1000, "items": (0..50).collect::<Vec<_>>() });
        let items = pages_with(first, 50, 1000, 4, fetch).await.unwrap();
        assert_eq!(items.len(), 1000);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn pages_cap_and_error() {
        let first = json!({ "total": 500, "items": (0..50).collect::<Vec<_>>() });
        let items = pages_with(first.clone(), 50, 120, 4, |o| async move { Ok(page_at(o, 50)) }).await.unwrap();
        assert_eq!(items.len(), 120);
        assert_eq!(items[119], 119);
        let err = pages_with(first, 50, usize::MAX, 4, |o| async move {
            if o == 200 { Err("Spotify API 500: boom".to_string()) } else { Ok(page_at(o, 50)) }
        })
        .await;
        assert_eq!(err, Err("Spotify API 500: boom".to_string()));
        // total ≤ one page: no requests
        let one = json!({ "total": 3, "items": [0, 1, 2] });
        let items = pages_with(one, 50, usize::MAX, 4, |_| async { Err::<Value, _>("no".to_string()) }).await.unwrap();
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn playlist_items_path_has_total_and_offset() {
        let p = playlist_items_path("pl1", 100);
        assert!(p.starts_with("/playlists/pl1/items?limit=50&offset=100&fields="), "{p}");
        let fields = p.split("fields=").nth(1).unwrap();
        assert!(fields.starts_with("total%2Cnext%2Citems%28"), "{fields}");
    }

    #[test]
    fn play_context_body_offset() {
        assert_eq!(play_context_body("spotify:album:a".into(), None), json!({"context_uri": "spotify:album:a"}));
        assert_eq!(
            play_context_body("spotify:album:a".into(), Some("spotify:track:t".into())),
            json!({"context_uri": "spotify:album:a", "offset": {"uri": "spotify:track:t"}})
        );
    }

    #[test]
    fn api_path_strips_prefix() {
        assert_eq!(api_path("https://api.spotify.com/v1/albums/x/tracks?offset=50"), "/albums/x/tracks?offset=50");
    }
}
