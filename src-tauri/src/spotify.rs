//! Thin Spotify Web API client: playlists, albums, search, queue, history and
//! Spotify Connect playback control.
//!
//! Errors starting with `AUTH_EXPIRED` or `NO_ACTIVE_DEVICE` are codes the
//! frontend matches with `startsWith`.

use crate::auth::{http, urlencode, valid_access_token};
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

/// One Spotify request: the response body as text, or a mapped error.
async fn request(method: Method, path: &str, body: Option<Value>) -> Result<String, String> {
    let token = valid_access_token().await?;
    let req = http().request(method, format!("{API}{path}")).bearer_auth(token);
    let req = match body {
        Some(b) => req.json(&b),
        None => req.header("Content-Length", "0"),
    };
    let resp = req.send().await.map_err(|e| e.to_string())?;
    let status = resp.status();
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

async fn get(path: &str) -> Result<Value, String> {
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

// ---- Spotify Connect: control a real device -------------------------------

/// List the user's available Spotify Connect devices, unchanged:
/// `[{id, name, type, is_active, is_restricted, supports_volume, volume_percent, …}]`.
#[tauri::command]
pub async fn list_devices() -> Result<Value, String> {
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

/// Play a whole context (playlist, album, Spotify mix) on a device.
#[tauri::command]
pub async fn play_context(device_id: String, context_uri: String) -> Result<(), String> {
    let path = format!("/me/player/play?device_id={}", urlencode(&device_id));
    command(Method::PUT, &path, Some(json!({ "context_uri": context_uri }))).await
}

/// Append a track URI to the device's up-next queue.
#[tauri::command]
pub async fn add_to_queue(device_id: String, uri: String) -> Result<(), String> {
    let path = format!("/me/player/queue?uri={}&device_id={}", urlencode(&uri), urlencode(&device_id));
    command(Method::POST, &path, None).await
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
#[tauri::command]
pub async fn get_queue() -> Result<Value, String> {
    let raw = get("/me/player/queue").await?;
    Ok(Value::Array(parse_queue(&raw)))
}

/// Last 30 played tracks, newest first: `[{track, played_at, context_uri}]`.
#[tauri::command]
pub async fn get_recently_played() -> Result<Value, String> {
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
    let path = format!("/search?type=track,album&limit=10&q={}", urlencode(&query));
    let raw = get(&path).await?;

    let tracks: Vec<Value> = raw["tracks"]["items"]
        .as_array()
        .map(|items| items.iter().filter(|t| !t.is_null()).map(simplify_track).collect())
        .unwrap_or_default();

    let albums: Vec<Value> = raw["albums"]["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|a| !a.is_null())
                .map(|a| {
                    json!({
                        "id": a["id"],
                        "name": a["name"],
                        "artists": join_artists(&a["artists"]),
                        "cover": first_image(&a["images"]),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({ "tracks": tracks, "albums": albums }))
}

/// All of an album's tracks (follows `tracks.next`). Album tracks lack album
/// art, so the album name and cover are stamped onto each track.
#[tauri::command]
pub async fn get_album_tracks(album_id: String) -> Result<Value, String> {
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
    Ok(get("/me/tracks?limit=1").await?["total"].as_u64().unwrap_or(0))
}

/// Liked Songs, newest first, capped at 1000: `{tracks, total}`. `total` is
/// the full count, so the UI can say when the cap cut some off.
#[tauri::command]
pub async fn get_saved_tracks() -> Result<Value, String> {
    let first = get("/me/tracks?limit=50").await?;
    let total = first["total"].clone();
    let tracks: Vec<Value> = items_up_to(first, MAX_SAVED_TRACKS)
        .await?
        .iter()
        .map(track_of_row)
        .filter(|t| !t.is_null())
        .map(simplify_track)
        .collect();
    Ok(json!({ "tracks": tracks, "total": total }))
}

/// Saved albums, newest first, capped at 200.
#[tauri::command]
pub async fn get_saved_albums() -> Result<Value, String> {
    let first = get("/me/albums?limit=50").await?;
    let rows = items_up_to(first, MAX_SAVED_ALBUMS).await?;
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
    let v = get(&library_path("/me/library/contains", &track_id)).await?;
    Ok(v[0].as_bool().unwrap_or(false))
}

#[tauri::command]
pub async fn save_track(track_id: String) -> Result<(), String> {
    command(Method::PUT, &library_path("/me/library", &track_id), None).await
}

#[tauri::command]
pub async fn unsave_track(track_id: String) -> Result<(), String> {
    command(Method::DELETE, &library_path("/me/library", &track_id), None).await
}

/// Top tracks (`[Track]`) or artists (`[{id,name,image}]`), at most 20.
/// `kind`: "tracks"|"artists"; `range`: "short_term"|"medium_term"|"long_term".
#[tauri::command]
pub async fn get_top(kind: String, range: String, limit: Option<u8>) -> Result<Value, String> {
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
    let raw = get(&format!("/me/top/{kind}?time_range={range}&limit={limit}")).await?;
    let items = raw["items"].as_array().into_iter().flatten().filter(|x| !x.is_null()).map(simplify).collect();
    Ok(Value::Array(items))
}

/// `{id, name, image}` for one artist.
#[tauri::command]
pub async fn get_artist(artist_id: String) -> Result<Value, String> {
    Ok(simplify_artist(&get(&format!("/artists/{}", urlencode(&artist_id))).await?))
}

/// An artist's albums and singles (first 50): `[{id, name, cover, year, kind}]`.
/// Spotify caps this endpoint at 10 per page (11+ → 400 "Invalid limit",
/// checked live 2026-10-02), so it follows `next`.
#[tauri::command]
pub async fn get_artist_albums(artist_id: String) -> Result<Value, String> {
    let path = format!("/artists/{}/albums?include_groups=album,single&limit=10", urlencode(&artist_id));
    let items = items_up_to(get(&path).await?, 50).await?;
    Ok(Value::Array(parse_artist_albums(&json!({ "items": items }))))
}

/// Followed artists, capped at 200. This endpoint pages by cursor
/// (`artists.cursors.after`), not offset.
#[tauri::command]
pub async fn get_followed_artists() -> Result<Value, String> {
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

/// Best-effort name and cover of a Spotify-owned mix. Spotify hides these
/// playlists' details; only the images endpoint sometimes answers, and a
/// radio mix's image URL holds its seed artist.
#[tauri::command]
pub async fn mix_info(playlist_id: String) -> Result<Value, String> {
    let images = match get(&format!("/playlists/{}/images", urlencode(&playlist_id))).await {
        Ok(v) => v,
        Err(e) if e.starts_with("Spotify API 404") => Value::Null,
        Err(e) => return Err(e),
    };
    let cover = first_image(&images);
    let mut name = "Spotify mix".to_string();
    if let Some(artist_id) = cover.as_deref().and_then(mix_artist_id) {
        // best effort: a failed artist lookup keeps the generic name
        if let Ok(artist) = get_artist(artist_id).await {
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
            json!({
                "id": a["id"],
                "name": a["name"],
                "artists": join_artists(&a["artists"]),
                "cover": first_image(&a["images"]),
                "total_tracks": a["total_tracks"],
            })
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
/// Returns a flat JSON array of playlist objects.
#[tauri::command]
pub async fn get_playlists() -> Result<Value, String> {
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

/// Paginated playlist tracks. This account's API uses `/items` (the `/tracks`
/// endpoint 403s) and may nest each track under `item` instead of `track`.
/// We request both field spellings and read whichever the response provides.
#[tauri::command]
pub async fn get_playlist_tracks(playlist_id: String) -> Result<Value, String> {
    let fields = "next,items(added_at,item(id,uri,name,duration_ms,artists(id,name),album(name,images)),track(id,uri,name,duration_ms,artists(id,name),album(name,images)))";
    let first = get(&format!("/playlists/{playlist_id}/items?limit=50&fields={}", urlencode(fields))).await?;
    let all = all_items(first)
        .await?
        .iter()
        .map(track_of_row)
        .filter(|t| !t.is_null()) // null = removed/unavailable
        .map(simplify_track)
        .collect();
    Ok(Value::Array(all))
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
    fn api_path_strips_prefix() {
        assert_eq!(api_path("https://api.spotify.com/v1/albums/x/tracks?offset=50"), "/albums/x/tracks?offset=50");
    }
}
