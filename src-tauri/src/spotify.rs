//! Thin Spotify Web API client: playlists, albums, search, queue, history and
//! Spotify Connect playback control.
//!
//! Errors starting with `AUTH_EXPIRED` or `NO_ACTIVE_DEVICE` are codes the
//! frontend matches with `startsWith`.

use crate::auth::valid_access_token;
use serde_json::{json, Value};

const API: &str = "https://api.spotify.com/v1";

/// Maps a failed HTTP status to an error string, adding a code prefix where
/// the frontend needs one.
fn api_error(status: u16, path: &str, body: &str) -> String {
    match status {
        401 => format!("AUTH_EXPIRED: Spotify API 401: {body}"),
        404 if path.starts_with("/me/player") => format!("NO_ACTIVE_DEVICE: Spotify API 404: {body}"),
        _ => format!("Spotify API {status}: {body}"),
    }
}

/// Turns an absolute `next` URL into a path for `get`.
fn api_path(url: &str) -> &str {
    url.strip_prefix(API).unwrap_or(url)
}

/// GET returning the parsed body, or None on 204 No Content.
async fn get_opt(path: &str) -> Result<Option<Value>, String> {
    let token = valid_access_token().await?;
    let resp = reqwest::Client::new()
        .get(format!("{API}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(api_error(status.as_u16(), path, &body));
    }
    if status.as_u16() == 204 || body.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&body).map(Some).map_err(|e| e.to_string())
}

async fn get(path: &str) -> Result<Value, String> {
    Ok(get_opt(path).await?.unwrap_or(Value::Null))
}

/// PUT with a JSON body to a player endpoint. Spotify replies 204 on success.
async fn put(path: &str, body: Value) -> Result<(), String> {
    let token = valid_access_token().await?;
    let resp = reqwest::Client::new()
        .put(format!("{API}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let text = resp.text().await.unwrap_or_default();
    Err(api_error(status.as_u16(), path, &text))
}

/// PUT or POST with no body to a player endpoint (pause, next, previous, seek…).
/// `method` is "PUT" or "POST". 204/202/200 all count as success.
async fn send_empty(method: &str, path: &str) -> Result<(), String> {
    let token = valid_access_token().await?;
    let client = reqwest::Client::new();
    let req = match method {
        "POST" => client.post(format!("{API}{path}")),
        _ => client.put(format!("{API}{path}")),
    };
    let resp = req
        .bearer_auth(token)
        .header("Content-Length", "0")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }
    let text = resp.text().await.unwrap_or_default();
    Err(api_error(status.as_u16(), path, &text))
}

// ---- Spotify Connect: control a real device -------------------------------

/// List the user's available Spotify Connect devices.
pub async fn list_devices() -> Result<Value, String> {
    let raw = get("/me/player/devices").await?;
    Ok(raw["devices"].clone())
}

/// Current playback state on the active device, simplified for the UI.
/// `{active:false}` when nothing is playing (204).
pub async fn playback_state() -> Result<Value, String> {
    let Some(s) = get_opt("/me/player").await? else {
        return Ok(json!({ "active": false }));
    };
    Ok(json!({
        "active": true,
        "is_playing": s["is_playing"],
        "progress_ms": s["progress_ms"],
        "device_id": s["device"]["id"],
        "device_name": s["device"]["name"],
        // null during ads and podcast episodes: Spotify sends no item for them here
        "track": if s["item"].is_null() { Value::Null } else { simplify_track(&s["item"]) },
    }))
}

/// Start playback of the given URIs on a specific device.
pub async fn play_on_device(device_id: String, uris: Vec<String>) -> Result<(), String> {
    put(
        &format!("/me/player/play?device_id={device_id}"),
        json!({ "uris": uris }),
    )
    .await
}

pub async fn resume(device_id: String) -> Result<(), String> {
    send_empty("PUT", &format!("/me/player/play?device_id={device_id}")).await
}
pub async fn pause() -> Result<(), String> {
    send_empty("PUT", "/me/player/pause").await
}
pub async fn next_track() -> Result<(), String> {
    send_empty("POST", "/me/player/next").await
}
pub async fn previous_track() -> Result<(), String> {
    send_empty("POST", "/me/player/previous").await
}
pub async fn seek(position_ms: u64) -> Result<(), String> {
    send_empty("PUT", &format!("/me/player/seek?position_ms={position_ms}")).await
}

/// The user's real up-next queue (excludes the current track).
pub async fn get_queue() -> Result<Value, String> {
    let raw = get("/me/player/queue").await?;
    Ok(Value::Array(parse_queue(&raw)))
}

/// Last 30 played tracks, newest first: `[{track, played_at}]`.
pub async fn get_recently_played() -> Result<Value, String> {
    let raw = get("/me/player/recently-played?limit=30").await?;
    Ok(Value::Array(parse_recent(&raw)))
}

/// Search tracks and albums. Returns { tracks: [...], albums: [...] } with
/// only the fields the UI needs. Spotify rejects limit > 10 here (400).
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
                        "uri": a["uri"],
                        "name": a["name"],
                        "artists": join_artists(&a["artists"]),
                        "cover": first_image(&a["images"]),
                        "year": a["release_date"].as_str().map(|d| d.get(0..4).unwrap_or("")),
                        "total_tracks": a["total_tracks"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(json!({ "tracks": tracks, "albums": albums }))
}

/// All of an album's tracks (follows `tracks.next`). Album tracks lack album
/// art, so the album name and cover are stamped onto each track.
pub async fn get_album_tracks(album_id: String) -> Result<Value, String> {
    let album = get(&format!("/albums/{album_id}")).await?;
    let cover = first_image(&album["images"]);
    let album_name = album["name"].clone();

    let mut all: Vec<Value> = Vec::new();
    let mut page = album["tracks"].clone();
    loop {
        for t in page["items"].as_array().into_iter().flatten() {
            if t.is_null() {
                continue;
            }
            let mut track = simplify_track(t);
            track["album"] = album_name.clone();
            track["cover"] = json!(cover);
            all.push(track);
        }
        match page["next"].as_str() {
            Some(next_url) => page = get(api_path(next_url)).await?,
            None => break,
        }
    }
    Ok(Value::Array(all))
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

/// Spotify track object → `Track` `{id, uri, name, artists, album, cover, duration_ms}`.
fn simplify_track(t: &Value) -> Value {
    json!({
        "id": t["id"],
        "uri": t["uri"],
        "name": t["name"],
        "artists": join_artists(&t["artists"]),
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

/// `/me/player/recently-played` body → `[{track, played_at}]`, rows without
/// a track skipped.
fn parse_recent(v: &Value) -> Vec<Value> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let t = track_of_row(row);
            (!t.is_null()).then(|| json!({ "track": simplify_track(t), "played_at": row["played_at"] }))
        })
        .collect()
}

/// GET /me/playlists, following `next` until all pages are collected.
/// Returns a flat JSON array of playlist objects.
pub async fn get_playlists() -> Result<Value, String> {
    let mut all: Vec<Value> = Vec::new();
    let mut path = "/me/playlists?limit=50".to_string();

    loop {
        let page = get(&path).await?;
        if let Some(items) = page.get("items").and_then(|v| v.as_array()) {
            for pl in items {
                let mut pl = pl.clone();
                // This account's API serves the track count under `items.total`,
                // not the documented `tracks.total`. Normalize so the frontend
                // always reads `tracks.total`.
                let total = pl
                    .get("tracks")
                    .and_then(|t| t.get("total"))
                    .or_else(|| pl.get("items").and_then(|t| t.get("total")))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                pl["tracks"] = json!({ "total": total });
                all.push(pl);
            }
        }
        match page.get("next").and_then(|v| v.as_str()) {
            Some(next_url) => path = api_path(next_url).to_string(),
            None => break,
        }
    }
    Ok(Value::Array(all))
}

/// Paginated playlist tracks. This account's API uses `/items` (the `/tracks`
/// endpoint 403s) and may nest each track under `item` instead of `track`.
/// We request both field spellings and read whichever the response provides.
pub async fn get_playlist_tracks(playlist_id: String) -> Result<Value, String> {
    let mut all: Vec<Value> = Vec::new();
    let fields = "next,items(added_at,item(id,uri,name,duration_ms,artists(name),album(name,images)),track(id,uri,name,duration_ms,artists(name),album(name,images)))";
    let mut path = format!(
        "/playlists/{}/items?limit=100&fields={}",
        playlist_id,
        urlencode(fields)
    );

    loop {
        let page = get(&path).await?;
        for row in page["items"].as_array().into_iter().flatten() {
            let track = track_of_row(row);
            if !track.is_null() {
                all.push(simplify_track(track)); // null = removed/unavailable
            }
        }
        match page.get("next").and_then(|v| v.as_str()) {
            Some(next_url) => path = api_path(next_url).to_string(),
            None => break,
        }
    }
    Ok(Value::Array(all))
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw_track(n: &str) -> Value {
        json!({
            "id": n, "uri": format!("spotify:track:{n}"), "name": format!("Song {n}"),
            "duration_ms": 1000,
            "artists": [{"name": "A"}, {"name": "B"}],
            "album": {"name": "Alb", "images": [{"url": "https://i.scdn.co/x"}, {"url": "small"}]}
        })
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
                   "album":"Alb","cover":"https://i.scdn.co/x","duration_ms":1000})
        );
    }

    #[test]
    fn simplify_track_no_images() {
        let t = simplify_track(&json!({"id":"x","album":{"name":"N","images":[]}}));
        assert!(t["cover"].is_null());
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
        assert!(api_error(404, "/me/player/play?device_id=1", "x").starts_with("NO_ACTIVE_DEVICE"));
        assert!(api_error(404, "/me/player", "x").starts_with("NO_ACTIVE_DEVICE"));
        assert_eq!(api_error(404, "/albums/x", "nope"), "Spotify API 404: nope");
        assert_eq!(api_error(500, "/me/player", "boom"), "Spotify API 500: boom");
    }

    #[test]
    fn api_path_strips_prefix() {
        assert_eq!(api_path("https://api.spotify.com/v1/albums/x/tracks?offset=50"), "/albums/x/tracks?offset=50");
    }
}
