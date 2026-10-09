//! The MCP tools: their names, descriptions and argument schemas, and what each does on top of a
//! `Backend` (the app's data and control: mcp_app.rs; a stub in tests). Results are short JSON,
//! errors plain sentences. Names (playlists, mixes, albums, artists, devices) are matched
//! case-insensitively against the user's own lists first, then Spotify search.

use std::collections::HashMap;
use std::sync::Mutex;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use crate::control::{Cmd, Source};
use crate::links::{self, Kind};
use crate::nowplaying::lock;

pub type Fut<'a> = BoxFuture<'a, Result<Value, String>>;

fn unavailable<'a>() -> Fut<'a> {
    Box::pin(async { Err("not available".to_string()) })
}

/// Where the tools get their data and send their commands. Every method has a default that fails,
/// so a test stub implements only what it needs. Shapes are the app commands' (spotify.rs).
pub trait Backend: Send + Sync + 'static {
    /// `playback_state`'s shape (`{active, is_playing, progress_ms, device_id, device_name, track,
    /// shuffle, repeat, volume_percent, context_uri}`), `{active:false}` when nothing plays.
    fn now_playing(&self) -> Fut<'_> {
        unavailable()
    }
    /// `{tracks, albums, artists, playlists}`.
    fn search(&self, _query: String) -> Fut<'_> {
        unavailable()
    }
    fn playlists(&self) -> Fut<'_> {
        unavailable()
    }
    fn playlist_tracks(&self, _id: String) -> Fut<'_> {
        unavailable()
    }
    fn albums(&self) -> Fut<'_> {
        unavailable()
    }
    fn album_tracks(&self, _id: String) -> Fut<'_> {
        unavailable()
    }
    /// `{tracks, total}`: at least the newest `max` (all of them when cached).
    fn liked(&self, _max: usize) -> Fut<'_> {
        unavailable()
    }
    fn recent(&self) -> Fut<'_> {
        unavailable()
    }
    fn top(&self, _kind: String, _range: String) -> Fut<'_> {
        unavailable()
    }
    /// `{id, name, image, top_tracks}`.
    fn artist(&self, _id: String) -> Fut<'_> {
        unavailable()
    }
    fn artist_albums(&self, _id: String) -> Fut<'_> {
        unavailable()
    }
    fn followed(&self) -> Fut<'_> {
        unavailable()
    }
    /// A list the UI keeps in its disk cache ("playlists", "albums", "following", "liked"), no
    /// request; None when it isn't cached.
    fn cached(&self, _key: &'static str) -> BoxFuture<'_, Option<Value>> {
        Box::pin(async { None })
    }
    fn devices(&self) -> Fut<'_> {
        unavailable()
    }
    fn queue(&self) -> Fut<'_> {
        unavailable()
    }
    /// The Mixes tab (library.rs `mixes`).
    fn mixes(&self) -> Fut<'_> {
        unavailable()
    }
    /// The links added in the app (library.rs `saved_links`).
    fn links(&self) -> Fut<'_> {
        unavailable()
    }
    fn resolve_link(&self, _text: String) -> Fut<'_> {
        unavailable()
    }
    fn save_link(&self, _text: String) -> Fut<'_> {
        unavailable()
    }
    fn play(&self, _src: Source, _device: Option<String>) -> Fut<'_> {
        unavailable()
    }
    /// The uri of the album a track (its id) is on, a string.
    fn album_of_track(&self, _track_id: String) -> Fut<'_> {
        unavailable()
    }
    fn transport(&self, _cmd: Cmd) -> Fut<'_> {
        unavailable()
    }
    /// A number, 0–100.
    fn volume(&self, _device: Option<String>) -> Fut<'_> {
        unavailable()
    }
    fn set_volume(&self, _percent: u8, _device: Option<String>) -> Fut<'_> {
        unavailable()
    }
    fn queue_add(&self, _uri: String) -> Fut<'_> {
        unavailable()
    }
    fn transfer(&self, _device: String, _play: bool) -> Fut<'_> {
        unavailable()
    }
    fn like(&self, _track_id: String, _on: bool) -> Fut<'_> {
        unavailable()
    }
}

// ---- the tool list ------------------------------------------------------------------------------

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn none() -> Value {
    obj(json!({}), &[])
}

const DEVICE: &str = "Device name or id (see `devices`). Default: the active device, else This Mac.";

/// `(name, description, input schema)` of every tool.
pub fn tools() -> Vec<(&'static str, &'static str, Value)> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let n = |d: &str, min: i64, max: i64| json!({ "type": "integer", "description": d, "minimum": min, "maximum": max });
    vec![
        ("now_playing", "What plays now: track, position, playing or paused, device, shuffle, repeat, volume and the playlist/album it plays from.", none()),
        (
            "search",
            "Search Spotify. Returns items with their uris, for `play`, `queue_add` and the list tools.",
            obj(json!({ "query": s("What to look for"), "type": { "type": "string", "enum": ["track", "album", "artist", "playlist"], "description": "Default track" }, "limit": n("At most 10 (default 5)", 1, 10) }), &["query"]),
        ),
        (
            "play",
            "Start playback. Give one of: `uri` (a track, playlist, album or artist uri, or a Spotify share link), `name` (one of your playlists, mixes, saved albums or artists, by name), `query` (plays the top song hit), `uris` (a list of track uris), or `context_uri` with an optional `track_uri` to start inside a playlist or album.",
            obj(
                json!({
                    "uri": s("spotify:track|playlist|album|artist:… or an open.spotify.com link"),
                    "name": s("A playlist, mix, album or artist from your library, e.g. \"Bonobo Radio\""),
                    "query": s("A song to search for; its top hit plays"),
                    "uris": { "type": "array", "items": { "type": "string" }, "description": "Track uris, played in order" },
                    "context_uri": s("A playlist or album uri to play from"),
                    "track_uri": s("The track to start at inside context_uri"),
                    "device": s(DEVICE),
                }),
                &[],
            ),
        ),
        ("pause", "Pause.", none()),
        ("resume", "Resume playback.", none()),
        ("next", "Skip to the next track.", none()),
        ("previous", "Go to the previous track.", none()),
        ("seek", "Jump to a position in the current track.", obj(json!({ "position_ms": n("Position in ms", 0, 86_400_000), "seconds": n("Position in seconds", 0, 86_400) }), &[])),
        ("set_volume", "Set the volume, 0–100 % (ramped on This Mac).", obj(json!({ "percent": n("0–100", 0, 100), "device": s(DEVICE) }), &["percent"])),
        ("volume_step", "Turn the volume up or down by `delta` percentage points (e.g. 10 or -10).", obj(json!({ "delta": n("-100…100", -100, 100), "device": s(DEVICE) }), &["delta"])),
        ("mute", "Mute (volume 0); `unmute` restores the level from before.", obj(json!({ "device": s(DEVICE) }), &[])),
        ("unmute", "Restore the volume from before `mute`.", obj(json!({ "device": s(DEVICE) }), &[])),
        ("set_shuffle", "Shuffle on or off.", obj(json!({ "on": { "type": "boolean" } }), &["on"])),
        ("set_repeat", "Repeat mode.", obj(json!({ "mode": { "type": "string", "enum": ["off", "context", "track"] } }), &["mode"])),
        ("queue_add", "Add a song to the up-next queue: a track `uri` or a `query` (its top hit).", obj(json!({ "uri": s("Track uri or link"), "query": s("A song to search for") }), &[])),
        ("get_queue", "The next songs in the queue.", none()),
        ("list_playlists", "Your playlists, and playlists added in Stylus by link (saved_in_app).", none()),
        ("list_mixes", "Your Spotify mixes as Stylus's Mixes tab shows them: Made For You (Daily Mixes, Discover Weekly, Release Radar, artist radios…), mixes added by link, and mixes you played.", none()),
        (
            "playlist_tracks",
            "The songs of a playlist or mix.",
            obj(json!({ "playlist": s("Playlist uri, id, link or name"), "limit": n("Default 50", 1, 1000), "offset": n("Default 0", 0, 100_000) }), &["playlist"]),
        ),
        ("list_albums", "Your saved albums, and albums added in Stylus by link (saved_in_app).", none()),
        ("album_tracks", "The songs of an album.", obj(json!({ "album": s("Album uri, id, link or name") }), &["album"])),
        ("list_artists", "Artists you follow, and artists added in Stylus by link (saved_in_app).", none()),
        ("liked_songs", "Your Liked Songs, newest first.", obj(json!({ "limit": n("Default 20", 1, 1000), "offset": n("Default 0", 0, 1000) }), &[])),
        ("recently_played", "What you played lately (one entry per playlist/album played).", obj(json!({ "limit": n("Default 10", 1, 50) }), &[])),
        (
            "top",
            "Your top tracks or artists.",
            obj(json!({ "kind": { "type": "string", "enum": ["tracks", "artists"] }, "range": { "type": "string", "enum": ["short", "medium", "long"], "description": "4 weeks, 6 months, all time (default short)" } }), &["kind"]),
        ),
        ("artist", "An artist: popular tracks, albums, and your liked songs by them.", obj(json!({ "artist": s("Artist uri, id, link or name") }), &["artist"])),
        ("devices", "Spotify Connect devices; the active one and This Mac (Stylus's own player) are marked.", none()),
        ("transfer", "Move playback to another device.", obj(json!({ "device": s("Device name or id"), "play": { "type": "boolean", "description": "This Mac: start playing there (default true). Another device keeps its play/pause state." } }), &["device"])),
        ("like", "Add a song to Liked Songs (default: the current song).", obj(json!({ "uri": s("Track uri or link") }), &[])),
        ("unlike", "Remove a song from Liked Songs (default: the current song).", obj(json!({ "uri": s("Track uri or link") }), &[])),
        (
            "open_link",
            "Look up a Spotify share link or uri (playlist, album, artist, song). `save: true` adds it to Stylus's library (mixes to the Mixes tab); `play: true` plays it.",
            obj(json!({ "link": s("open.spotify.com link or spotify: uri"), "save": { "type": "boolean" }, "play": { "type": "boolean" }, "device": s(DEVICE) }), &["link"]),
        ),
    ]
}

// ---- helpers --------------------------------------------------------------------------------------

fn arg_str<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args[k].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn arg_u64(args: &Value, k: &str) -> Option<u64> {
    args[k].as_u64().or_else(|| args[k].as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
}

fn arg_i64(args: &Value, k: &str) -> Option<i64> {
    args[k].as_i64().or_else(|| args[k].as_f64().map(|f| f as i64))
}

/// A track for an agent: `{uri, name, artists, album, duration_ms}`.
pub fn slim_track(t: &Value) -> Value {
    json!({ "uri": t["uri"], "name": t["name"], "artists": t["artists"], "album": t["album"], "duration_ms": t["duration_ms"] })
}

fn slim_tracks(v: &Value) -> Vec<Value> {
    let list = v.as_array().or_else(|| v["tracks"].as_array());
    list.into_iter().flatten().filter(|t| t["uri"].is_string()).map(slim_track).collect()
}

fn uri_of(kind: &str, item: &Value) -> Value {
    if item["uri"].is_string() {
        return item["uri"].clone();
    }
    item["id"].as_str().map_or(Value::Null, |id| json!(format!("spotify:{kind}:{id}")))
}

/// A uri for `text` (a uri, a share link, or a bare id of `kind`), or None.
pub fn uri_from(text: &str, kind: Option<Kind>) -> Option<String> {
    if let Some(l) = links::parse(text) {
        return Some(l.uri());
    }
    let t = text.trim();
    match kind {
        Some(k) if links::is_id(t) => Some(format!("spotify:{}:{t}", k.as_str())),
        _ => None,
    }
}

/// One of `candidates` (`(name, item)`) for `query`: the exact name (any case), else the only
/// name containing it. Several → an error listing them; none → None.
pub fn pick(query: &str, candidates: &[(String, Value)]) -> Result<Option<Value>, String> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Ok(None);
    }
    fn dedup(found: Vec<&(String, Value)>) -> Vec<&(String, Value)> {
        let mut out: Vec<&(String, Value)> = Vec::new();
        for c in found {
            if !out.iter().any(|o| o.1["uri"] == c.1["uri"] && !c.1["uri"].is_null()) {
                out.push(c);
            }
        }
        out
    }
    let exact = dedup(candidates.iter().filter(|(n, _)| n.to_lowercase() == q).collect());
    let found = if exact.is_empty() { dedup(candidates.iter().filter(|(n, _)| n.to_lowercase().contains(&q)).collect()) } else { exact };
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found[0].1.clone())),
        n => {
            let list: Vec<String> = found.iter().take(10).map(|(name, it)| format!("\"{name}\" ({})", it["uri"].as_str().unwrap_or("?"))).collect();
            let more = if n > 10 { format!(" and {} more", n - 10) } else { String::new() };
            Err(format!("\"{query}\" matches {n}: {}{more}. Give the uri of the one you mean.", list.join(", ")))
        }
    }
}

fn named(list: &Value, kind: &str) -> Vec<(String, Value)> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let name = it["name"].as_str()?.to_string();
            let mut it = it.clone();
            it["uri"] = uri_of(kind, &it);
            Some((name, it))
        })
        .collect()
}

/// The links added in the app, of one tab.
fn links_of(links: &Value, tab: &str) -> Value {
    Value::Array(links.as_array().into_iter().flatten().filter(|i| i["tab"] == tab).cloned().collect())
}

/// Plain sentences for the app's error codes.
pub fn plain_error(e: &str) -> String {
    if let Some(rest) = e.strip_prefix("NOT_AVAILABLE_REMOTE: ") {
        let action = rest.strip_suffix(" on other devices").unwrap_or(rest);
        return format!("Not available for other devices: that device doesn't take {action} from Stylus. It works on This Mac (device \"This Mac\").");
    }
    if e.starts_with("ENGINE_NOT_READY") {
        return "Stylus's player isn't connected yet: try again in a moment.".into();
    }
    if e.starts_with("NO_ACTIVE_DEVICE") {
        return crate::control::NO_DEVICE.into();
    }
    if let Some(rest) = e.strip_prefix("BAD_ARGS: ") {
        return rest.to_string();
    }
    e.to_string()
}

/// The volume each device had before `mute`, for `unmute`.
static MUTED: Mutex<Option<HashMap<String, u8>>> = Mutex::new(None);

fn muted(device: &str) -> Option<u8> {
    lock(&MUTED).as_ref()?.get(device).copied()
}

fn set_muted(device: &str, level: Option<u8>) {
    let mut g = lock(&MUTED);
    let m = g.get_or_insert_with(HashMap::new);
    match level {
        Some(l) => m.insert(device.to_string(), l),
        None => m.remove(device),
    };
}

/// The `uri` argument as a track (`{uri}`), None when it isn't given; `bad` when it isn't a track.
fn track_uri_arg(a: &Value, bad: &str) -> Result<Option<Value>, String> {
    arg_str(a, "uri")
        .map(|u| uri_from(u, Some(Kind::Track)).filter(|u| u.starts_with("spotify:track:")).map(|u| json!({ "uri": u })).ok_or_else(|| bad.to_string()))
        .transpose()
}

/// Every Liked Song the app keeps (it caps the list itself).
const ALL: usize = usize::MAX;

// ---- the tools ------------------------------------------------------------------------------------

/// Runs tool `name` with `args` (a JSON object). Err is the sentence the agent sees.
pub async fn call(b: &dyn Backend, name: &str, args: &Value) -> Result<Value, String> {
    run(b, name, args).await.map_err(|e| plain_error(&e))
}

async fn run(b: &dyn Backend, name: &str, a: &Value) -> Result<Value, String> {
    match name {
        "now_playing" => now_playing(b).await,
        "search" => search(b, a).await,
        "play" => play(b, a).await,
        "pause" => b.transport(Cmd::Pause).await,
        "resume" => b.transport(Cmd::Resume).await,
        "next" => b.transport(Cmd::Next).await,
        "previous" => b.transport(Cmd::Previous).await,
        "seek" => {
            let ms = arg_u64(a, "position_ms").or_else(|| arg_u64(a, "seconds").map(|s| s * 1000)).ok_or("Give position_ms or seconds")?;
            b.transport(Cmd::Seek(ms.min(u64::from(u32::MAX)) as u32)).await
        }
        "set_volume" => {
            let p = arg_u64(a, "percent").ok_or("Give percent, 0–100")?.min(100) as u8;
            let device = device_arg(b, a).await?;
            b.set_volume(p, device).await.map(|r| with(r, "volume", json!(p)))
        }
        "volume_step" => {
            let delta = arg_i64(a, "delta").ok_or("Give delta, e.g. 10 or -10")?;
            let t = VolumeTarget::of(b, a).await?;
            let now = t.volume(b).await?;
            let next = (i64::from(now) + delta).clamp(0, 100) as u8;
            b.set_volume(next, Some(t.id)).await.map(|r| with(with(r, "from", json!(now)), "volume", json!(next)))
        }
        "mute" => {
            let t = VolumeTarget::of(b, a).await?;
            let now = t.volume(b).await?;
            if now == 0 {
                return Ok(json!({ "volume": 0, "note": "Already muted" }));
            }
            set_muted(&t.id, Some(now));
            b.set_volume(0, Some(t.id)).await.map(|r| with(with(r, "volume", json!(0)), "was", json!(now)))
        }
        "unmute" => {
            let t = VolumeTarget::of(b, a).await?;
            let level = match muted(&t.id) {
                Some(l) => l,
                None if t.volume(b).await? > 0 => return Ok(json!({ "note": "Not muted" })),
                None => 50,
            };
            let r = b.set_volume(level, Some(t.id.clone())).await?;
            set_muted(&t.id, None);
            Ok(with(r, "volume", json!(level)))
        }
        "set_shuffle" => {
            let on = a["on"].as_bool().ok_or("Give on: true or false")?;
            b.transport(Cmd::Shuffle(on)).await.map(|r| with(r, "shuffle", json!(on)))
        }
        "set_repeat" => {
            let mode = arg_str(a, "mode").ok_or("Give mode: off, context or track")?;
            if !matches!(mode, "off" | "context" | "track") {
                return Err(format!("Unknown repeat mode \"{mode}\": use off, context or track"));
            }
            b.transport(Cmd::Repeat(mode.into())).await.map(|r| with(r, "repeat", json!(mode)))
        }
        "queue_add" => {
            let t = match track_uri_arg(a, "queue_add takes a track uri or link")? {
                Some(t) => t,
                None => top_track(b, arg_str(a, "query").ok_or("Give uri or query")?).await?,
            };
            b.queue_add(t["uri"].as_str().unwrap_or("").to_string()).await?;
            Ok(json!({ "queued": if t["name"].is_null() { t["uri"].clone() } else { slim_track(&t) } }))
        }
        "get_queue" => Ok(json!({ "next": slim_tracks(&b.queue().await?).into_iter().take(20).collect::<Vec<_>>() })),
        "list_playlists" => {
            let own = |p: &Value| json!({ "name": p["name"], "uri": uri_of("playlist", p), "tracks": p["tracks"]["total"], "owner": p["owner"]["display_name"] });
            let link = |p: &Value| json!({ "name": p["name"], "uri": p["uri"], "tracks": p["total"], "owner": p["owner"] });
            list_tool(b, "playlists", b.playlists(), own, link).await
        }
        "list_mixes" => {
            let m = b.mixes().await?;
            Ok(json!({ "mixes": m.as_array().into_iter().flatten().map(|x| json!({ "name": x["name"], "uri": x["uri"], "source": x["source"], "cover": x["cover"] })).collect::<Vec<_>>() }))
        }
        "playlist_tracks" => {
            let q = arg_str(a, "playlist").ok_or("Give playlist: a uri, link or name")?;
            let p = find_by_name(b, q, Kind::Playlist).await?;
            let id = links::parse(p["uri"].as_str().unwrap_or("")).map(|l| l.id).ok_or("Not a playlist")?;
            let all = slim_tracks(&b.playlist_tracks(id).await?);
            let (offset, limit) = (arg_u64(a, "offset").unwrap_or(0) as usize, arg_u64(a, "limit").unwrap_or(50) as usize);
            Ok(json!({ "name": p["name"], "uri": p["uri"], "total": all.len(), "tracks": all.into_iter().skip(offset).take(limit).collect::<Vec<_>>() }))
        }
        "list_albums" => {
            let row = |x: &Value| json!({ "name": x["name"], "artists": x["artists"], "uri": uri_of("album", x) });
            list_tool(b, "albums", b.albums(), row, row).await
        }
        "album_tracks" => {
            let q = arg_str(a, "album").ok_or("Give album: a uri, link or name")?;
            let al = find_by_name(b, q, Kind::Album).await?;
            let id = links::parse(al["uri"].as_str().unwrap_or("")).map(|l| l.id).ok_or("Not an album")?;
            Ok(json!({ "name": al["name"], "uri": al["uri"], "tracks": slim_tracks(&b.album_tracks(id).await?) }))
        }
        "list_artists" => {
            let row = |x: &Value| json!({ "name": x["name"], "uri": uri_of("artist", x) });
            list_tool(b, "artists", b.followed(), row, row).await
        }
        "liked_songs" => {
            let (offset, limit) = (arg_u64(a, "offset").unwrap_or(0) as usize, arg_u64(a, "limit").unwrap_or(20) as usize);
            let v = b.liked(offset.saturating_add(limit)).await?;
            let tracks: Vec<Value> = slim_tracks(&v).into_iter().skip(offset).take(limit).collect();
            Ok(json!({ "total": v["total"], "tracks": tracks }))
        }
        "recently_played" => {
            let limit = arg_u64(a, "limit").unwrap_or(10) as usize;
            let v = b.recent().await?;
            let rows: Vec<Value> = v.as_array().into_iter().flatten().take(limit).map(|r| json!({ "track": slim_track(&r["track"]), "played_at": r["played_at"], "context_uri": r["context_uri"] })).collect();
            Ok(json!({ "recent": rows }))
        }
        "top" => {
            let kind = arg_str(a, "kind").unwrap_or("tracks");
            if !matches!(kind, "tracks" | "artists") {
                return Err("kind is tracks or artists".into());
            }
            let range = match arg_str(a, "range").unwrap_or("short") {
                "short" | "short_term" => "short_term",
                "medium" | "medium_term" => "medium_term",
                "long" | "long_term" => "long_term",
                r => return Err(format!("Unknown range \"{r}\": use short, medium or long")),
            };
            let v = b.top(kind.into(), range.into()).await?;
            let items: Vec<Value> = if kind == "tracks" { slim_tracks(&v) } else { v.as_array().into_iter().flatten().map(|x| json!({ "name": x["name"], "uri": uri_of("artist", x) })).collect() };
            Ok(json!({ "kind": kind, "range": range, "items": items }))
        }
        "artist" => artist(b, a).await,
        "devices" => {
            let (list, own) = devices(b).await?;
            Ok(json!({ "devices": list.iter().map(|d| json!({ "id": d["id"], "name": d["name"], "type": d["type"], "is_active": d["is_active"], "volume_percent": d["volume_percent"], "this_mac": own.as_deref() == d["id"].as_str() })).collect::<Vec<_>>() }))
        }
        "transfer" => {
            let q = arg_str(a, "device").ok_or("Give device: a name or id")?;
            let d = find_device(b, q).await?;
            let play = a["play"].as_bool().unwrap_or(true);
            b.transfer(d["id"].as_str().unwrap_or("").into(), play).await.map(|r| with(r, "device", d["name"].clone()))
        }
        "like" | "unlike" => {
            let on = name == "like";
            let t = match track_uri_arg(a, "Give a track uri or link")? {
                Some(t) => t,
                None => {
                    let s = b.now_playing().await?;
                    if s["track"]["uri"].is_null() {
                        return Err(crate::control::NOTHING_PLAYING.into());
                    }
                    s["track"].clone()
                }
            };
            let id = links::parse(t["uri"].as_str().unwrap_or("")).map(|l| l.id).ok_or("Not a track")?;
            b.like(id, on).await?;
            Ok(json!({ "liked": on, "track": if t["name"].is_null() { t["uri"].clone() } else { slim_track(&t) } }))
        }
        "open_link" => open_link(b, a).await,
        _ => Err(format!("Unknown tool \"{name}\"")),
    }
}

/// `r` (an object) with `k` set.
fn with(mut r: Value, k: &str, v: Value) -> Value {
    if !r.is_object() {
        r = json!({});
    }
    r[k] = v;
    r
}

/// Your `own` list (each item as `row`) and the links of the same tab added in the app (as
/// `link`, marked `saved_in_app`), under `tab` ("playlists" | "albums" | "artists").
async fn list_tool(b: &dyn Backend, tab: &str, own: Fut<'_>, row: impl Fn(&Value) -> Value, link: impl Fn(&Value) -> Value) -> Result<Value, String> {
    let (own, links) = futures_util::join!(own, b.links());
    let mut out: Vec<Value> = own?.as_array().into_iter().flatten().map(row).collect();
    out.extend(links_of(&links.unwrap_or_default(), tab).as_array().into_iter().flatten().map(|x| with(link(x), "saved_in_app", json!(true))));
    Ok(json!({ tab: out }))
}

/// The device a volume tool works on, from one device list per call: the `device` argument, else
/// the active device, else This Mac. A mute is saved under its id, so `mute` without a device and
/// `unmute` with that device's name meet on the same key.
struct VolumeTarget {
    id: String,
    list: Vec<Value>,
    own: Option<String>,
}

impl VolumeTarget {
    async fn of(b: &dyn Backend, a: &Value) -> Result<VolumeTarget, String> {
        let (list, own) = devices(b).await?;
        let id = match arg_str(a, "device") {
            Some(q) => find_device_in(&list, own.as_deref(), q)?["id"].as_str().map(str::to_string),
            None => list.iter().find(|d| d["is_active"] == true).and_then(|d| d["id"].as_str().map(str::to_string)).or_else(|| own.clone()),
        };
        Ok(VolumeTarget { id: id.ok_or(crate::control::NOTHING_PLAYING)?, list, own })
    }

    /// Its volume: This Mac's own (or the level waiting for its next load), else the list's.
    async fn volume(&self, b: &dyn Backend) -> Result<u8, String> {
        if self.own.as_deref() == Some(self.id.as_str()) {
            return Ok(b.volume(Some(self.id.clone())).await?.as_u64().unwrap_or(0).min(100) as u8);
        }
        crate::control::listed_volume(&self.list, &self.id)
    }
}

async fn now_playing(b: &dyn Backend) -> Result<Value, String> {
    let s = b.now_playing().await?;
    if s["active"] != true || s["track"].is_null() && s["device_id"].is_null() {
        let mut out = json!({ "playing": false, "note": "Nothing is playing" });
        if let Some(e) = s["unchecked"].as_str() {
            out["note"] = json!(format!("Nothing is playing on This Mac. Other devices can't be checked now: {}", plain_error(e)));
        }
        if let Some(t) = s["last_here"]["track_uri"].as_str() {
            let track = match b.resolve_link(t.into()).await {
                Ok(r) if r["track"].is_object() => slim_track(&r["track"]),
                _ => json!({ "uri": t }),
            };
            let context = context_of(b, s["last_here"]["context_uri"].as_str()).await;
            out["last_played_here"] = json!({ "track": track, "context": context, "position_ms": s["last_here"]["position_ms"] });
        }
        return Ok(out);
    }
    let context = context_of(b, s["context_uri"].as_str()).await;
    let mut out = json!({
        "track": if s["track"].is_null() { Value::Null } else { slim_track(&s["track"]) },
        "is_playing": s["is_playing"],
        "position_ms": s["progress_ms"],
        "device": { "id": s["device_id"], "name": s["device_name"] },
        "shuffle": s["shuffle"],
        "repeat": s["repeat"],
        "volume": s["volume_percent"],
        "context": context,
    });
    if s["loading"] == true {
        out["note"] = json!("The track is still loading: its name comes in a second");
    }
    Ok(out)
}

/// `{uri, name}` of a playing context, null without one.
async fn context_of(b: &dyn Backend, uri: Option<&str>) -> Value {
    match uri.filter(|c| !c.is_empty()) {
        Some(uri) => json!({ "uri": uri, "name": context_name(b, uri).await }),
        None => Value::Null,
    }
}

/// Context names found, for the process: `now_playing` asks for the same one again and again.
static NAMES: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);

/// A playing context's name, null when nothing knows it (`lookup_name`).
async fn context_name(b: &dyn Backend, uri: &str) -> Value {
    if let Some(name) = lock(&NAMES).as_ref().and_then(|m| m.get(uri).cloned()) {
        return name;
    }
    let name = lookup_name(b, uri).await;
    if !name.is_null() {
        lock(&NAMES).get_or_insert_with(HashMap::new).insert(uri.to_string(), name.clone());
    }
    name
}

/// Without a request first: the mixes and links the app keeps, then the lists the UI cached on
/// disk. Else looked up like a link (the internal API).
async fn lookup_name(b: &dyn Backend, uri: &str) -> Value {
    let kind = uri.split(':').nth(1).unwrap_or("playlist");
    let name_in = |list: &Value| list.as_array().into_iter().flatten().find(|it| uri_of(kind, it) == uri).map(|it| it["name"].clone());
    let (mixes, links) = futures_util::join!(b.mixes(), b.links());
    if let Some(n) = [mixes, links].iter().flatten().find_map(name_in) {
        return n;
    }
    for key in ["playlists", "albums", "following"] {
        if let Some(n) = b.cached(key).await.as_ref().and_then(name_in) {
            return n;
        }
    }
    b.resolve_link(uri.into()).await.map(|r| r["name"].clone()).unwrap_or(Value::Null)
}

async fn search(b: &dyn Backend, a: &Value) -> Result<Value, String> {
    let q = arg_str(a, "query").ok_or("Give query")?;
    let kind = arg_str(a, "type").unwrap_or("track");
    let limit = arg_u64(a, "limit").unwrap_or(5).clamp(1, 10) as usize;
    let r = b.search(q.into()).await?;
    let items: Vec<Value> = match kind {
        "track" => slim_tracks(&r["tracks"]),
        "album" => r["albums"].as_array().into_iter().flatten().map(|x| json!({ "name": x["name"], "artists": x["artists"], "uri": uri_of("album", x) })).collect(),
        "artist" => r["artists"].as_array().into_iter().flatten().map(|x| json!({ "name": x["name"], "uri": uri_of("artist", x) })).collect(),
        "playlist" => r["playlists"].as_array().into_iter().flatten().map(|x| json!({ "name": x["name"], "uri": uri_of("playlist", x), "owner": x["owner"]["display_name"] })).collect(),
        k => return Err(format!("Unknown type \"{k}\": use track, album, artist or playlist")),
    };
    Ok(json!({ "type": kind, "results": items.into_iter().take(limit).collect::<Vec<_>>() }))
}

async fn top_track(b: &dyn Backend, query: &str) -> Result<Value, String> {
    let r = b.search(query.into()).await?;
    r["tracks"].as_array().and_then(|t| t.iter().find(|t| t["uri"].is_string())).cloned().ok_or_else(|| format!("No song found for \"{query}\""))
}

/// The devices and This Mac's id: the local player's own device id when it's in the list (every
/// Stylus is named "This Mac", so the name alone can pick another computer), else the name.
async fn devices(b: &dyn Backend) -> Result<(Vec<Value>, Option<String>), String> {
    let list = b.devices().await?.as_array().cloned().unwrap_or_default();
    let own = crate::internal::own_device_id()
        .filter(|id| list.iter().any(|d| d["id"].as_str() == Some(id.as_str())))
        .or_else(|| list.iter().find(|d| d["name"] == crate::player::DEVICE_NAME).and_then(|d| d["id"].as_str().map(str::to_string)));
    Ok((list, own))
}

/// A device by id or name; "here", "mac", "stylus" (and the old name "needle") mean This Mac.
async fn find_device(b: &dyn Backend, q: &str) -> Result<Value, String> {
    let (list, own) = devices(b).await?;
    find_device_in(&list, own.as_deref(), q)
}

/// `find_device` in a device list (`own`: This Mac's id).
fn find_device_in(list: &[Value], own: Option<&str>, q: &str) -> Result<Value, String> {
    if let Some(d) = list.iter().find(|d| d["id"] == q) {
        return Ok(d.clone());
    }
    let alias = matches!(q.to_lowercase().as_str(), "here" | "mac" | "this mac" | "stylus" | "needle" | "this computer" | "computer");
    if alias {
        if let Some(d) = list.iter().find(|d| d["id"].as_str() == own) {
            return Ok(d.clone());
        }
        return Err("This Mac isn't listed: Stylus's player isn't connected yet".into());
    }
    let named: Vec<(String, Value)> = list.iter().filter_map(|d| Some((d["name"].as_str()?.to_string(), json!({ "uri": d["id"], "id": d["id"], "name": d["name"] })))).collect();
    pick(q, &named)?.ok_or_else(|| {
        let names: Vec<&str> = list.iter().filter_map(|d| d["name"].as_str()).collect();
        format!("No device called \"{q}\". Devices: {}", if names.is_empty() { "none".into() } else { names.join(", ") })
    })
}

/// The `device` argument as a device id, None when not given.
async fn device_arg(b: &dyn Backend, a: &Value) -> Result<Option<String>, String> {
    match arg_str(a, "device") {
        Some(q) => Ok(find_device(b, q).await?["id"].as_str().map(str::to_string)),
        None => Ok(None),
    }
}

/// A playlist (or mix), album or artist by uri, link, id or name: your own list (playlists: and
/// the mixes), the links added in the app, then search.
async fn find_by_name(b: &dyn Backend, q: &str, kind: Kind) -> Result<Value, String> {
    let k = kind.as_str();
    if let Some(uri) = uri_from(q, Some(kind)) {
        if !uri.starts_with(&format!("spotify:{k}:")) {
            let a = if kind == Kind::Playlist { "a" } else { "an" };
            return Err(format!("{q} isn't {a} {k}"));
        }
        return Ok(json!({ "uri": uri }));
    }
    let (own, mixes): (Fut<'_>, Fut<'_>) = match kind {
        Kind::Album => (b.albums(), Box::pin(async { Ok(json!([])) })),
        Kind::Artist => (b.followed(), Box::pin(async { Ok(json!([])) })),
        _ => (b.playlists(), b.mixes()),
    };
    let (own, mixes, links) = futures_util::join!(own, mixes, b.links());
    let mut c = named(&own.unwrap_or_default(), k);
    c.extend(named(&mixes.unwrap_or_default(), k));
    c.extend(named(&links_of(&links.unwrap_or_default(), &format!("{k}s")), k));
    if let Some(x) = pick(q, &c)? {
        return Ok(x);
    }
    let r = b.search(q.into()).await?;
    let hits = named(&r[format!("{k}s")], k);
    if kind == Kind::Playlist {
        return pick(q, &hits)?.ok_or_else(|| format!("No playlist called \"{q}\" in your playlists or mixes"));
    }
    match pick(q, &hits) {
        Ok(Some(x)) => Ok(x),
        // search ranks: its first hit is the best guess when names don't settle it
        _ => hits.first().map(|h| h.1.clone()).ok_or_else(|| format!("No {k} found for \"{q}\"")),
    }
}

/// Something in the user's library by name, for `play name:` — playlists, mixes, links added in
/// the app, saved albums, followed artists.
async fn find_in_library(b: &dyn Backend, q: &str) -> Result<Value, String> {
    let (pl, mixes, links, albums, artists) = futures_util::join!(b.playlists(), b.mixes(), b.links(), b.albums(), b.followed());
    let mut c = named(&pl.unwrap_or_default(), "playlist");
    c.extend(named(&mixes.unwrap_or_default(), "playlist"));
    c.extend(named(&links.unwrap_or_default(), "playlist"));
    c.extend(named(&albums.unwrap_or_default(), "album"));
    c.extend(named(&artists.unwrap_or_default(), "artist"));
    pick(q, &c)?.ok_or_else(|| format!("Nothing called \"{q}\" in your playlists, mixes, albums or artists. Try `search`."))
}

/// What a uri plays as: a context, or one track (`track_source`).
async fn source_of(b: &dyn Backend, uri: &str) -> Source {
    if uri.starts_with("spotify:track:") {
        track_source(b, uri).await
    } else {
        Source { context_uri: Some(uri.to_string()), ..Source::default() }
    }
}

/// One track plays inside its album, from that track, so next and previous work as in Spotify.
/// Just the track when its album can't be found.
async fn track_source(b: &dyn Backend, uri: &str) -> Source {
    let id = uri.strip_prefix("spotify:track:").unwrap_or_default().to_string();
    match b.album_of_track(id).await {
        Ok(Value::String(album)) if album.starts_with("spotify:album:") => in_album(album, uri),
        _ => Source { uris: vec![uri.to_string()], ..Source::default() },
    }
}

fn in_album(album: String, track_uri: &str) -> Source {
    Source { context_uri: Some(album), track_uri: Some(track_uri.to_string()), ..Source::default() }
}

async fn play(b: &dyn Backend, a: &Value) -> Result<Value, String> {
    let device = device_arg(b, a).await?;
    let (src, what) = if let Some(u) = arg_str(a, "uri") {
        let uri = uri_from(u, None).ok_or_else(|| format!("\"{u}\" isn't a Spotify uri or link"))?;
        (source_of(b, &uri).await, json!({ "uri": uri }))
    } else if let Some(ctx) = arg_str(a, "context_uri") {
        let uri = uri_from(ctx, None).filter(|u| !u.starts_with("spotify:track:")).ok_or("context_uri must be a playlist, album or artist")?;
        let track = arg_str(a, "track_uri").map(|t| uri_from(t, Some(Kind::Track)).ok_or("track_uri isn't a track uri")).transpose()?;
        (Source { context_uri: Some(uri.clone()), track_uri: track, ..Source::default() }, json!({ "uri": uri }))
    } else if let Some(list) = a["uris"].as_array().filter(|l| !l.is_empty()) {
        let uris: Vec<String> = list.iter().filter_map(|u| u.as_str().and_then(|u| uri_from(u, Some(Kind::Track)))).filter(|u| u.starts_with("spotify:track:")).collect();
        if uris.len() != list.len() {
            return Err("uris must all be track uris".into());
        }
        let src = if let [one] = uris.as_slice() { track_source(b, one).await } else { Source { uris: uris.clone(), ..Source::default() } };
        (src, json!({ "tracks": uris.len() }))
    } else if let Some(n) = arg_str(a, "name") {
        let it = find_in_library(b, n).await?;
        let uri = it["uri"].as_str().unwrap_or("").to_string();
        (source_of(b, &uri).await, json!({ "name": it["name"], "uri": uri }))
    } else if let Some(q) = arg_str(a, "query") {
        let t = top_track(b, q).await?;
        let uri = t["uri"].as_str().unwrap_or("");
        // a search hit names its album: no lookup
        let src = match t["album_uri"].as_str().filter(|u| u.starts_with("spotify:album:")) {
            Some(album) => in_album(album.to_string(), uri),
            None => source_of(b, uri).await,
        };
        (src, slim_track(&t))
    } else {
        return Err("Give uri, name, query, uris or context_uri".into());
    };
    let r = b.play(src, device).await?;
    Ok(play_result(what, &r))
}

/// `play`'s answer. This Mac confirms within 5 s (`confirmed`, with its state): `status:
/// playing` and the track. Not confirmed, or another device (no confirmation): `requested`.
fn play_result(what: Value, r: &Value) -> Value {
    if r["confirmed"] == true {
        let s = &r["state"];
        let track = if s["track"].is_null() { Value::Null } else { slim_track(&s["track"]) };
        return json!({ "status": "playing", "started": what, "track": track, "device_id": r["device_id"] });
    }
    let note = if r["confirmed"] == false {
        "Sent to This Mac, but it hasn't started within 5 s: check now_playing once in a few seconds"
    } else {
        "Sent: check now_playing once in a second or two"
    };
    json!({ "status": "requested", "requested": what, "device_id": r["device_id"], "note": note })
}

async fn artist(b: &dyn Backend, a: &Value) -> Result<Value, String> {
    let q = arg_str(a, "artist").ok_or("Give artist: a uri, link or name")?;
    let found = find_by_name(b, q, Kind::Artist).await?;
    let id = links::parse(found["uri"].as_str().unwrap_or("")).map(|l| l.id).ok_or("Not an artist")?;
    let (info, albums, liked) = futures_util::join!(b.artist(id.clone()), b.artist_albums(id.clone()), b.liked(ALL));
    let info = info?;
    let by_them: Vec<Value> = liked
        .ok()
        .map(|l| l["tracks"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default()
        .iter()
        .filter(|t| t["artist_list"].as_array().into_iter().flatten().any(|x| x["id"] == id.as_str()))
        .take(10)
        .map(slim_track)
        .collect();
    let albums: Vec<Value> = albums.unwrap_or_default().as_array().into_iter().flatten().take(20).map(|x| json!({ "name": x["name"], "uri": uri_of("album", x), "year": x["year"], "kind": x["kind"] })).collect();
    Ok(json!({
        "name": info["name"],
        "uri": format!("spotify:artist:{id}"),
        "popular": slim_tracks(&info["top_tracks"]),
        "albums": albums,
        "your_liked_songs": by_them,
    }))
}

async fn open_link(b: &dyn Backend, a: &Value) -> Result<Value, String> {
    let link = arg_str(a, "link").ok_or("Give link")?;
    let info = b.resolve_link(link.into()).await?;
    let mut out = json!({ "kind": info["kind"], "uri": info["uri"], "name": info["name"] });
    for k in ["owner", "artists", "total", "tab"] {
        if !info[k].is_null() {
            out[k] = info[k].clone();
        }
    }
    if info["kind"] == "track" {
        out["track"] = slim_track(&info["track"]);
    }
    out["saved_in_app"] = info["saved"].clone();
    if a["save"].as_bool() == Some(true) && info["kind"] != "track" {
        let r = b.save_link(link.into()).await?;
        out["saved_in_app"] = json!(true);
        out["already_saved"] = r["already"].clone();
    }
    if a["play"].as_bool() == Some(true) {
        let device = device_arg(b, a).await?;
        let r = b.play(source_of(b, info["uri"].as_str().unwrap_or("")).await, device).await?;
        // like `play`: playing only once confirmed, else requested with a note
        let result = play_result(info["uri"].clone(), &r);
        out["playing"] = json!(result["status"] == "playing");
        out["play"] = result;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, uri: &str) -> (String, Value) {
        (name.into(), json!({ "name": name, "uri": uri }))
    }

    #[test]
    fn pick_rules() {
        let list = vec![c("Bonobo Radio", "spotify:playlist:a"), c("Chill Mix", "spotify:playlist:b"), c("Chill Techno Mix", "spotify:playlist:c"), c("chill mix", "spotify:playlist:b")];
        // exact, any case
        assert_eq!(pick("bonobo radio", &list).unwrap().unwrap()["uri"], "spotify:playlist:a");
        // an exact name wins over the names that contain it; the same uri twice counts once
        assert_eq!(pick("Chill Mix", &list).unwrap().unwrap()["uri"], "spotify:playlist:b");
        // a unique substring
        assert_eq!(pick("techno", &list).unwrap().unwrap()["uri"], "spotify:playlist:c");
        // ambiguous: the error lists the matches with their uris
        let e = pick("chill", &list).unwrap_err();
        assert!(e.contains("matches 2") && e.contains("spotify:playlist:b") && e.contains("spotify:playlist:c"), "{e}");
        assert_eq!(pick("jazz", &list), Ok(None));
        assert_eq!(pick("  ", &list), Ok(None));
    }

    #[test]
    fn uris_from_text() {
        assert_eq!(uri_from("https://open.spotify.com/playlist/37i9dQZEVXcVV9hd3iqSgp?si=x", None).unwrap(), "spotify:playlist:37i9dQZEVXcVV9hd3iqSgp");
        assert_eq!(uri_from("37i9dQZEVXcVV9hd3iqSgp", Some(Kind::Playlist)).unwrap(), "spotify:playlist:37i9dQZEVXcVV9hd3iqSgp");
        assert_eq!(uri_from("37i9dQZEVXcVV9hd3iqSgp", None), None);
        assert_eq!(uri_from("Bonobo Radio", Some(Kind::Playlist)), None);
    }

    #[test]
    fn errors_are_sentences() {
        assert_eq!(
            plain_error("NOT_AVAILABLE_REMOTE: pause on other devices"),
            "Not available for other devices: that device doesn't take pause from Stylus. It works on This Mac (device \"This Mac\")."
        );
        assert_eq!(plain_error("NO_ACTIVE_DEVICE: Spotify API 404"), crate::control::NO_DEVICE);
        assert_eq!(plain_error("BAD_ARGS: bad repeat mode: x"), "bad repeat mode: x");
        assert!(plain_error("ENGINE_NOT_READY: the player is starting").starts_with("Stylus's player isn't connected"));
        assert_eq!(plain_error("Spotify API 500: boom"), "Spotify API 500: boom");
    }

    #[test]
    fn tool_table() {
        let t = tools();
        let names: Vec<&str> = t.iter().map(|x| x.0).collect();
        for want in ["now_playing", "search", "play", "pause", "resume", "next", "previous", "seek", "set_volume", "volume_step", "mute", "unmute", "set_shuffle", "set_repeat", "queue_add", "get_queue", "list_playlists", "list_mixes", "playlist_tracks", "list_albums", "album_tracks", "list_artists", "liked_songs", "recently_played", "top", "artist", "devices", "transfer", "like", "unlike", "open_link"] {
            assert!(names.contains(&want), "{want}");
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "no duplicate names");
        for (n, _, schema) in &t {
            assert_eq!(schema["type"], "object", "{n}");
            for r in schema["required"].as_array().unwrap() {
                assert!(schema["properties"].get(r.as_str().unwrap()).is_some(), "{n}: required {r} isn't a property");
            }
        }
    }

    /// A backend with a few lists and a recorder for commands.
    #[derive(Default)]
    struct Stub {
        sent: Mutex<Vec<String>>,
        volume: Mutex<u8>,
    }

    impl Backend for Stub {
        fn playlists(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([{ "id": "1A2b3C4d5E6f7G8h9I0jKl", "name": "Road trip", "tracks": { "total": 3 }, "owner": { "display_name": "me" } }])) })
        }
        fn mixes(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([{ "id": "37i9dQZF1E4yLltmVk3nyb", "uri": "spotify:playlist:37i9dQZF1E4yLltmVk3nyb", "name": "Bonobo Radio", "source": "made_for_you" }])) })
        }
        fn links(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([{ "uri": "spotify:album:6dVIqQ8qmQ5GBnJ9shOYGE", "name": "OK Computer", "tab": "albums" }])) })
        }
        fn albums(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([])) })
        }
        fn followed(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([])) })
        }
        fn devices(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([{ "id": "mac", "name": "This Mac", "is_active": true, "volume_percent": 40 }, { "id": "tv", "name": "Living Room TV", "is_active": false }])) })
        }
        fn play(&self, src: Source, device: Option<String>) -> Fut<'_> {
            self.sent.lock().unwrap().push(format!("play {:?} {:?} at {:?} on {device:?}", src.context_uri, src.uris, src.track_uri));
            Box::pin(async { Ok(json!({ "device_id": "mac" })) })
        }
        fn search(&self, _q: String) -> Fut<'_> {
            Box::pin(async { Ok(json!({ "tracks": [{ "uri": "spotify:track:5DAjrJqXqYtgr67pVhmUeR", "name": "Kerala", "album": "Migration" }] })) })
        }
        fn album_of_track(&self, id: String) -> Fut<'_> {
            // Kerala's album is known; any other track's lookup fails
            Box::pin(async move { if id == "5DAjrJqXqYtgr67pVhmUeR" { Ok(json!("spotify:album:3gBVdu4a1MMJVMy6vwPEb8")) } else { Err("no album".into()) } })
        }
        fn volume(&self, _d: Option<String>) -> Fut<'_> {
            let v = *self.volume.lock().unwrap();
            Box::pin(async move { Ok(json!(v)) })
        }
        fn set_volume(&self, p: u8, d: Option<String>) -> Fut<'_> {
            *self.volume.lock().unwrap() = p;
            self.sent.lock().unwrap().push(format!("volume {p} {d:?}"));
            Box::pin(async { Ok(json!({ "path": "this_mac" })) })
        }
    }

    #[tokio::test]
    async fn one_track_plays_inside_its_album() {
        // the observed bug: play query "Bonobo Kerala" loaded uris:[Kerala]; next then stopped playback
        let b = Stub::default();
        call(&b, "play", &json!({ "query": "Bonobo Kerala" })).await.unwrap();
        call(&b, "play", &json!({ "uri": "spotify:track:5DAjrJqXqYtgr67pVhmUeR" })).await.unwrap();
        call(&b, "play", &json!({ "uris": ["spotify:track:5DAjrJqXqYtgr67pVhmUeR"] })).await.unwrap();
        call(&b, "play", &json!({ "uris": ["spotify:track:5DAjrJqXqYtgr67pVhmUeR", "spotify:track:7c378mlmubSu7NGkLFa4sN"] })).await.unwrap();
        let sent = b.sent.lock().unwrap().clone();
        let in_album = "play Some(\"spotify:album:3gBVdu4a1MMJVMy6vwPEb8\") [] at Some(\"spotify:track:5DAjrJqXqYtgr67pVhmUeR\") on None";
        assert_eq!(sent[..3], [in_album, in_album, in_album]);
        assert_eq!(sent[3], "play None [\"spotify:track:5DAjrJqXqYtgr67pVhmUeR\", \"spotify:track:7c378mlmubSu7NGkLFa4sN\"] at None on None", "a list stays a list");
    }

    #[tokio::test]
    async fn play_by_name_finds_mixes_and_links() {
        let b = Stub::default();
        let r = call(&b, "play", &json!({ "name": "bonobo radio" })).await.unwrap();
        assert_eq!(r["requested"]["uri"], "spotify:playlist:37i9dQZF1E4yLltmVk3nyb");
        assert_eq!(r["status"], "requested", "no confirmation from the stub");
        call(&b, "play", &json!({ "name": "OK Computer", "device": "living room" })).await.unwrap();
        call(&b, "play", &json!({ "uri": "https://open.spotify.com/track/7c378mlmubSu7NGkLFa4sN?si=1" })).await.unwrap();
        let sent = b.sent.lock().unwrap().clone();
        assert_eq!(sent[0], "play Some(\"spotify:playlist:37i9dQZF1E4yLltmVk3nyb\") [] at None on None");
        assert_eq!(sent[1], "play Some(\"spotify:album:6dVIqQ8qmQ5GBnJ9shOYGE\") [] at None on Some(\"tv\")");
        // its album can't be found: the track on its own
        assert_eq!(sent[2], "play None [\"spotify:track:7c378mlmubSu7NGkLFa4sN\"] at None on None");
        let e = call(&b, "play", &json!({ "name": "jazz" })).await.unwrap_err();
        assert!(e.starts_with("Nothing called \"jazz\""), "{e}");
        let e = call(&b, "play", &json!({ "device": "kitchen", "name": "Road trip" })).await.unwrap_err();
        assert!(e.contains("Living Room TV"), "{e}");
    }

    #[tokio::test]
    async fn volume_step_mute_unmute() {
        let b = Stub::default();
        *b.volume.lock().unwrap() = 40;
        assert_eq!(call(&b, "volume_step", &json!({ "delta": 70 })).await.unwrap()["volume"], 100);
        assert_eq!(call(&b, "volume_step", &json!({ "delta": -30 })).await.unwrap()["volume"], 70);
        assert_eq!(call(&b, "mute", &json!({})).await.unwrap()["was"], 70);
        assert_eq!(*b.volume.lock().unwrap(), 0);
        assert_eq!(call(&b, "unmute", &json!({})).await.unwrap()["volume"], 70);
        assert_eq!(call(&b, "unmute", &json!({})).await.unwrap()["note"], "Not muted");
        assert!(call(&b, "set_volume", &json!({})).await.unwrap_err().contains("percent"));
    }

    #[tokio::test]
    async fn unknown_and_unavailable() {
        let b = Stub::default();
        assert_eq!(call(&b, "nope", &json!({})).await.unwrap_err(), "Unknown tool \"nope\"");
        assert_eq!(call(&b, "pause", &json!({})).await.unwrap_err(), "not available");
        let r = call(&b, "list_playlists", &json!({})).await.unwrap();
        assert_eq!(r["playlists"][0]["uri"], "spotify:playlist:1A2b3C4d5E6f7G8h9I0jKl");
    }

    const LISTS_DOWN: &str = "HTTP 503: the library lists are down";
    const NO_CLUSTER: &str = "ENGINE_NOT_READY: no Connect cluster yet";

    /// This Mac is up, the library lists fail. `now` is what mcp_app's now_playing answers; links
    /// resolve through the internal API.
    struct NoLists {
        now: Value,
        confirmed: Option<bool>,
    }

    impl Backend for NoLists {
        fn now_playing(&self) -> Fut<'_> {
            let s = self.now.clone();
            Box::pin(async move { Ok(s) })
        }
        fn playlists(&self) -> Fut<'_> {
            Box::pin(async { Err(LISTS_DOWN.to_string()) })
        }
        fn albums(&self) -> Fut<'_> {
            Box::pin(async { Err(LISTS_DOWN.to_string()) })
        }
        fn followed(&self) -> Fut<'_> {
            Box::pin(async { Err(LISTS_DOWN.to_string()) })
        }
        fn mixes(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([{ "uri": "spotify:playlist:37i9dQZF1E4yLltmVk3nyb", "name": "Bonobo Radio" }])) })
        }
        fn links(&self) -> Fut<'_> {
            Box::pin(async { Ok(json!([])) })
        }
        fn resolve_link(&self, text: String) -> Fut<'_> {
            Box::pin(async move {
                match text.as_str() {
                    "spotify:album:5pZ8vcpdqmJ1RvNcEuaNfs" => Ok(json!({ "kind": "album", "uri": text, "name": "Migration" })),
                    "spotify:track:5DAjrJqXqYtgr67pVhmUeR" => Ok(json!({ "kind": "track", "uri": text, "name": "Kerala", "track": { "uri": text, "name": "Kerala", "artists": "Bonobo" } })),
                    _ => Err("Spotify didn't return this playlist".into()),
                }
            })
        }
        fn play(&self, _src: Source, _device: Option<String>) -> Fut<'_> {
            let confirmed = self.confirmed;
            Box::pin(async move {
                Ok(match confirmed {
                    Some(true) => json!({ "device_id": "mac", "path": "this_mac", "confirmed": true, "state": { "active": true, "is_playing": true, "track": { "uri": "spotify:track:5UAVcondwGsdqnnumvEUXw", "name": "Cycles", "artists": "Bonobo" }, "context_uri": "spotify:playlist:37i9dQZF1E4yLltmVk3nyb" } }),
                    Some(false) => json!({ "device_id": "mac", "path": "this_mac", "confirmed": false, "state": null }),
                    None => json!({ "device_id": "tv", "path": "connect" }),
                })
            })
        }
    }

    fn here_playing(context: &str) -> Value {
        json!({
            "active": true, "is_playing": true, "progress_ms": 1000, "device_id": "mac", "device_name": "This Mac",
            "track": { "uri": "spotify:track:5DAjrJqXqYtgr67pVhmUeR", "name": "Kerala", "artists": "Bonobo" },
            "shuffle": false, "repeat": "off", "volume_percent": 40, "context_uri": context,
        })
    }

    #[tokio::test]
    async fn now_playing_names_the_context_while_the_lists_fail() {
        // a mix: from the Mixes list
        let b = NoLists { now: here_playing("spotify:playlist:37i9dQZF1E4yLltmVk3nyb"), confirmed: None };
        let r = call(&b, "now_playing", &json!({})).await.unwrap();
        assert_eq!(r["track"]["name"], "Kerala");
        assert_eq!(r["context"]["name"], "Bonobo Radio");
        // an album (saved albums fail): looked up like a link
        let b = NoLists { now: here_playing("spotify:album:5pZ8vcpdqmJ1RvNcEuaNfs"), confirmed: None };
        assert_eq!(call(&b, "now_playing", &json!({})).await.unwrap()["context"]["name"], "Migration");
        // unknown everywhere: the uri, name null, no error
        let b = NoLists { now: here_playing("spotify:playlist:0000000000000000000000"), confirmed: None };
        let r = call(&b, "now_playing", &json!({})).await.unwrap();
        assert_eq!((r["context"]["uri"].clone(), r["context"]["name"].clone()), (json!("spotify:playlist:0000000000000000000000"), Value::Null));
    }

    #[tokio::test]
    async fn now_playing_idle_without_a_cluster_is_not_an_error() {
        let now = json!({ "active": false, "unchecked": NO_CLUSTER, "last_here": { "track_uri": "spotify:track:5DAjrJqXqYtgr67pVhmUeR", "context_uri": "spotify:album:5pZ8vcpdqmJ1RvNcEuaNfs", "position_ms": 5000 } });
        let r = call(&NoLists { now, confirmed: None }, "now_playing", &json!({})).await.unwrap();
        assert_eq!(r["playing"], false);
        assert!(r["note"].as_str().unwrap().starts_with("Nothing is playing on This Mac. Other devices can't be checked now"), "{r}");
        assert_eq!(r["last_played_here"]["track"]["name"], "Kerala");
        assert_eq!(r["last_played_here"]["context"]["name"], "Migration");
        // nothing known at all
        let r = call(&NoLists { now: json!({ "active": false }), confirmed: None }, "now_playing", &json!({})).await.unwrap();
        assert_eq!(r, json!({ "playing": false, "note": "Nothing is playing" }));
        // the track's metadata still loading
        let mut s = here_playing("spotify:playlist:37i9dQZF1E4yLltmVk3nyb");
        s["loading"] = json!(true);
        s["track"] = json!({ "uri": "spotify:track:5DAjrJqXqYtgr67pVhmUeR" });
        let r = call(&NoLists { now: s, confirmed: None }, "now_playing", &json!({})).await.unwrap();
        assert_eq!(r["track"]["uri"], "spotify:track:5DAjrJqXqYtgr67pVhmUeR");
        assert!(r["note"].as_str().unwrap().contains("loading"));
    }

    #[tokio::test]
    async fn play_says_whether_this_mac_started() {
        let b = NoLists { now: json!({}), confirmed: Some(true) };
        let r = call(&b, "play", &json!({ "name": "Bonobo Radio" })).await.unwrap();
        assert_eq!(r["status"], "playing");
        assert_eq!(r["started"]["uri"], "spotify:playlist:37i9dQZF1E4yLltmVk3nyb");
        assert_eq!(r["track"]["name"], "Cycles");
        let b = NoLists { now: json!({}), confirmed: Some(false) };
        let r = call(&b, "play", &json!({ "name": "Bonobo Radio" })).await.unwrap();
        assert_eq!(r["status"], "requested");
        assert!(r["note"].as_str().unwrap().contains("hasn't started within 5 s"), "{r}");
        assert!(r.get("started").is_none());
        let b = NoLists { now: json!({}), confirmed: None };
        assert_eq!(call(&b, "play", &json!({ "uri": "spotify:album:5pZ8vcpdqmJ1RvNcEuaNfs" })).await.unwrap()["status"], "requested");
    }
}
