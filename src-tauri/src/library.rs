//! The app's own library, owned here so the UI and the MCP server (mcp.rs) see the same thing:
//! - links the user added (a pasted share link): store key `savedLinks`, `[LinkItem]`, newest first;
//! - the Mixes tab: those added mixes, the Made For You mixes on the user's Spotify home feed
//!   (pathfinder `home`, kept under `homeMixes` per account, refreshed every 6 h), and the mixes the
//!   UI saw playing (`knownMixes`, written by the UI).
//!
//! Every change emits `library-changed` so the open UI redraws (a save from MCP too).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use futures_util::FutureExt;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::internal::{serve, with_api, Source::Fallback, Source::Primary};
use crate::links::{self, Kind, Link};

const LOG: &str = "stylus::library";
pub const EVENT: &str = "library-changed";
const LINKS_KEY: &str = "savedLinks";
const KNOWN_KEY: &str = "knownMixes";
const HOME_KEY: &str = "homeMixes";
/// How long the home feed's mixes count as fresh.
const HOME_FRESH_SECS: u64 = 6 * 3600;
/// Most links kept.
const MAX_LINKS: usize = 200;

static APP: OnceLock<AppHandle> = OnceLock::new();

/// Where `library-changed` goes. Set once, in `setup`.
pub fn attach(app: AppHandle) {
    let _ = APP.set(app);
}

fn changed() {
    if let Some(app) = APP.get() {
        let _ = app.emit(EVENT, ());
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---- added links ---------------------------------------------------------------------------

/// The Library tab an added item shows in: Spotify's own playlists (editorial, Made For You,
/// radio) under Mixes, anyone else's under Playlists.
pub fn tab_of(kind: Kind, id: &str, owner_id: Option<&str>) -> &'static str {
    match kind {
        Kind::Playlist if links::is_spotify_playlist(id) || owner_id == Some("spotify") => "mixes",
        Kind::Playlist => "playlists",
        Kind::Album => "albums",
        Kind::Artist => "artists",
        Kind::Track => "",
    }
}

/// The stored links, valid ones only.
pub fn saved_links() -> Vec<Value> {
    parse_links(crate::store::get(LINKS_KEY))
}

fn parse_links(v: Option<Value>) -> Vec<Value> {
    v.and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i["uri"].as_str().and_then(links::parse).is_some_and(|l| l.kind != Kind::Track) && i["name"].is_string())
        .collect()
}

/// `item` first, any older copy of the same uri dropped, at most MAX_LINKS.
fn with_link(list: Vec<Value>, item: Value) -> Vec<Value> {
    let mut out = vec![item.clone()];
    out.extend(list.into_iter().filter(|i| i["uri"] != item["uri"]));
    out.truncate(MAX_LINKS);
    out
}

/// Change the saved list in one locked read-modify-write (two adds at once can't lose one).
/// `f` returns the new list (None: unchanged) and a result.
fn update_links<T>(f: impl FnOnce(Vec<Value>) -> (Option<Vec<Value>>, T)) -> Result<T, String> {
    let (wrote, out) = crate::store::update(LINKS_KEY, |cur| {
        let (next, out) = f(parse_links(cur.cloned()));
        let wrote = next.is_some();
        (next.map(Value::Array), (wrote, out))
    })?;
    if wrote {
        changed();
    }
    Ok(out)
}

/// What a link names, fetched from Spotify (internal API): for the UI's detail head and the add
/// field. Playlists `{kind, id, uri, name, cover, owner, owner_id, total, tab}`, albums `{…, artists}`,
/// artists `{…, image→cover}`, tracks `{kind, id, uri, track}`. `saved`: already in the app's library.
pub async fn resolve(link: &Link) -> Result<Value, String> {
    let id = link.id.as_str();
    let mut v = match link.kind {
        Kind::Playlist => {
            let meta = serve("link playlist", vec![with_api(Primary, move |api| async move { api.playlist_meta(id).await })]).await;
            // the protobuf fallback has the name and cover only
            match meta {
                Ok(m) => m,
                Err(e) => {
                    let info = serve("link playlist", vec![with_api(Fallback, move |api| async move { api.playlist_info_pb(id).await })]).await.map_err(|_| e)?;
                    json!({"id": id, "uri": link.uri(), "name": info["name"], "cover": info["cover"], "owner": null, "owner_id": null, "total": null})
                }
            }
        }
        Kind::Album => serve("link album", vec![with_api(Primary, move |api| async move { api.album_meta(id).await })]).await?,
        Kind::Artist => {
            let a = crate::spotify::get_artist(id.to_string()).await?;
            json!({"id": id, "uri": link.uri(), "name": a["name"], "cover": a["image"]})
        }
        Kind::Track => {
            let uri = link.uri();
            let t = serve("link track", vec![(Primary, async move { crate::internal::Api::current()?.tracks(&[uri]).await }.boxed())]).await?;
            let t = t.into_iter().next().ok_or("Spotify didn't return this song")?;
            return Ok(json!({"kind": "track", "id": id, "uri": link.uri(), "name": t["name"], "track": t}));
        }
    };
    if v["name"].as_str().is_none_or(str::is_empty) {
        return Err(format!("Spotify didn't return this {}", link.kind.as_str()));
    }
    v["kind"] = json!(link.kind.as_str());
    v["uri"] = json!(link.uri());
    v["tab"] = json!(tab_of(link.kind, id, v["owner_id"].as_str()));
    v["saved"] = json!(saved_links().iter().any(|i| i["uri"] == v["uri"]));
    Ok(v)
}

/// The stored item for a resolved link.
fn item_of(v: &Value) -> Value {
    json!({
        "kind": v["kind"], "id": v["id"], "uri": v["uri"], "name": v["name"], "cover": v["cover"],
        "owner": v["owner"], "owner_id": v["owner_id"], "artists": v["artists"], "total": v["total"],
        "tab": v["tab"], "added": unix_now(),
    })
}

/// Adds what `text` (a share link or URI) names to the app's library. `{item, already}`;
/// already: it was there (the item comes back unchanged).
pub async fn save(text: &str) -> Result<Value, String> {
    let link = links::parse(text).ok_or(NOT_A_LINK)?;
    if link.kind == Kind::Track {
        return Err("A song can't be added to the library here: open it or play it instead".into());
    }
    let uri = link.uri();
    if let Some(item) = saved_links().into_iter().find(|i| i["uri"] == uri.as_str()) {
        return Ok(json!({ "item": item, "already": true }));
    }
    let item = item_of(&resolve(&link).await?);
    // checked again under the lock: the same link may have been added while it resolved
    let already = update_links(|list| match list.iter().find(|i| i["uri"] == uri.as_str()) {
        Some(i) => (None, Some(i.clone())),
        None => (Some(with_link(list, item.clone())), None),
    })?;
    if let Some(item) = already {
        return Ok(json!({ "item": item, "already": true }));
    }
    log::info!(target: LOG, "added {uri} ({})", item["name"].as_str().unwrap_or(""));
    Ok(json!({ "item": item, "already": false }))
}

/// Takes `uri` out of the app's library. True when it was there.
pub fn remove(uri: &str) -> Result<bool, String> {
    let removed = update_links(|list| {
        let next: Vec<Value> = list.iter().filter(|i| i["uri"] != uri).cloned().collect();
        if next.len() == list.len() {
            (None, false)
        } else {
            (Some(next), true)
        }
    })?;
    if !removed {
        return Ok(false);
    }
    log::info!(target: LOG, "removed {uri}");
    Ok(true)
}

pub const NOT_A_LINK: &str = "That link isn't a Spotify playlist, album, artist or song";

// ---- the Mixes tab ---------------------------------------------------------------------------

/// Names and covers of played mixes not on the home feed (mix_info), kept for the process.
static INFO: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);

fn info_cached(id: &str) -> Option<Value> {
    INFO.lock().ok()?.as_ref()?.get(id).cloned()
}

fn remember_info(id: &str, v: &Value) {
    if let Ok(mut g) = INFO.lock() {
        g.get_or_insert_with(HashMap::new).insert(id.to_string(), v.clone());
    }
}

/// The home feed's mixes for `account`: the stored copy while fresh (or when the feed fails),
/// else the feed. Empty without the player's session and no stored copy.
async fn home_mixes(account: &str, refresh: bool) -> Vec<Value> {
    let stored = crate::store::get(HOME_KEY).filter(|v| v["account"] == account);
    let items = |v: &Value| v["items"].as_array().cloned().unwrap_or_default();
    if let Some(s) = &stored {
        if !refresh && unix_now().saturating_sub(s["at"].as_u64().unwrap_or(0)) < HOME_FRESH_SECS {
            return items(s);
        }
    }
    match crate::internal::Api::current() {
        Ok(api) => match api.home_mixes().await {
            Ok(list) => {
                let _ = crate::store::store_set(HOME_KEY.into(), json!({ "account": account, "at": unix_now(), "items": list }));
                log::info!(target: LOG, "home feed: {} mixes", list.len());
                list
            }
            Err(e) => {
                log::warn!(target: LOG, "home feed failed: {e}");
                stored.as_ref().map(items).unwrap_or_default()
            }
        },
        Err(_) => stored.as_ref().map(items).unwrap_or_default(),
    }
}

/// The Mixes tab, in order: mixes the user added, the home feed's Made For You mixes, then mixes
/// seen playing that are in neither; each once. `[{id, uri, name, cover, source}]`, source
/// "added" | "made_for_you" | "played". `refresh`: ask the home feed now.
pub async fn mixes(refresh: bool) -> Vec<Value> {
    let account = crate::internal::Api::current().map(|a| a.username()).ok().or_else(|| crate::store::get(HOME_KEY).and_then(|v| v["account"].as_str().map(str::to_string))).unwrap_or_default();
    let added: Vec<Value> = saved_links().into_iter().filter(|i| i["tab"] == "mixes").collect();
    let home = home_mixes(&account, refresh).await;
    let played: Vec<String> = crate::store::get(KNOWN_KEY)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["id"].as_str().filter(|id| links::is_id(id)).map(str::to_string))
        .collect();
    let mut out = merge(&added, &home, &played);
    // played mixes only have an id: their names come from Spotify, once per process
    let missing: Vec<String> = out.iter().filter(|m| m["name"].is_null()).filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
    if !missing.is_empty() {
        use futures_util::stream::{self, StreamExt};
        let infos: Vec<(String, Option<Value>)> = stream::iter(missing)
            .map(|id| async move {
                if let Some(v) = info_cached(&id) {
                    return (id, Some(v));
                }
                let got = crate::spotify::mix_info(id.clone()).await.ok();
                if let Some(v) = &got {
                    remember_info(&id, v);
                }
                (id, got)
            })
            .buffered(4)
            .collect()
            .await;
        for m in out.iter_mut().filter(|m| m["name"].is_null()) {
            if let Some((_, Some(info))) = infos.iter().find(|(id, _)| m["id"] == id.as_str()) {
                m["name"] = info["name"].clone();
                m["cover"] = info["cover"].clone();
            }
        }
    }
    for m in &mut out {
        if m["name"].is_null() {
            m["name"] = json!("Spotify mix");
        }
    }
    out
}

/// The Mixes tab's order and dedup, before names are looked up (played-only mixes have a null name).
pub fn merge(added: &[Value], home: &[Value], played: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut push = |id: &str, name: &Value, cover: &Value, source: &str| {
        if !id.is_empty() && !out.iter().any(|m| m["id"] == id) {
            out.push(json!({ "id": id, "uri": format!("spotify:playlist:{id}"), "name": name, "cover": cover, "source": source }));
        }
    };
    for m in added {
        push(m["id"].as_str().unwrap_or(""), &m["name"], &m["cover"], "added");
    }
    for m in home {
        push(m["id"].as_str().unwrap_or(""), &m["name"], &m["cover"], "made_for_you");
    }
    for id in played {
        push(id, &Value::Null, &Value::Null, "played");
    }
    out
}

// ---- Tauri commands ----------------------------------------------------------------------------

/// The Mixes tab (see `mixes`).
#[tauri::command]
pub async fn mixes_list(refresh: Option<bool>) -> Result<Value, String> {
    Ok(Value::Array(mixes(refresh.unwrap_or(false)).await))
}

/// The links the user added: `[{kind, id, uri, name, cover, owner, artists, total, tab, added}]`.
#[tauri::command]
pub fn links_list() -> Value {
    Value::Array(saved_links())
}

/// What a pasted link or URI names (see `resolve`). Errors are sentences for the UI.
#[tauri::command]
pub async fn link_resolve(link: String) -> Result<Value, String> {
    let parsed = links::parse(&link).ok_or(NOT_A_LINK)?;
    resolve(&parsed).await.map_err(|e| friendly(&parsed, &e))
}

#[tauri::command]
pub async fn link_save(link: String) -> Result<Value, String> {
    let parsed = links::parse(&link).ok_or(NOT_A_LINK)?;
    save(&link).await.map_err(|e| friendly(&parsed, &e))
}

#[tauri::command]
pub fn link_remove(uri: String) -> Result<bool, String> {
    remove(&uri)
}

/// A failed lookup as the UI says it: the player not being up yet, or Spotify not having it.
fn friendly(link: &Link, err: &str) -> String {
    if err.starts_with("ENGINE_NOT_READY") {
        return "The player isn't connected yet: try again in a moment".into();
    }
    if err.starts_with("Spotify didn't") || err.starts_with("A song") {
        return err.to_string();
    }
    log::warn!(target: LOG, "link {} failed: {err}", link.uri());
    format!("Spotify didn't return this {}", link.kind.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs() {
        assert_eq!(tab_of(Kind::Playlist, "37i9dQZEVXcVV9hd3iqSgp", Some("spotify")), "mixes");
        assert_eq!(tab_of(Kind::Playlist, "37i9dQZF1DXcBWIGoYBM5M", None), "mixes");
        assert_eq!(tab_of(Kind::Playlist, "1A2b3C4d5E6f7G8h9I0jKl", Some("spotify")), "mixes");
        assert_eq!(tab_of(Kind::Playlist, "1A2b3C4d5E6f7G8h9I0jKl", Some("bob")), "playlists");
        assert_eq!(tab_of(Kind::Album, "x", None), "albums");
        assert_eq!(tab_of(Kind::Artist, "x", None), "artists");
    }

    #[test]
    fn links_list_keeps_valid_items_newest_first() {
        let a = json!({"uri": "spotify:album:6dVIqQ8qmQ5GBnJ9shOYGE", "name": "A"});
        let p = json!({"uri": "spotify:playlist:37i9dQZEVXcVV9hd3iqSgp", "name": "DW"});
        let raw = json!([a, {"uri": "nope", "name": "x"}, {"uri": "spotify:track:7c378mlmubSu7NGkLFa4sN", "name": "song"}, {"uri": "spotify:artist:4Z8W4fKeB5YxbusRsdQVPb"}, p]);
        assert_eq!(parse_links(Some(raw)), vec![a.clone(), p.clone()]);
        assert!(parse_links(Some(json!({"a": 1}))).is_empty());
        assert!(parse_links(None).is_empty());
        let next = with_link(vec![a.clone(), p.clone()], json!({"uri": p["uri"], "name": "DW 2"}));
        assert_eq!(next.len(), 2);
        assert_eq!(next[0]["name"], "DW 2");
        assert_eq!(next[1], a);
    }

    #[test]
    fn mixes_order_and_dedup() {
        let added = vec![json!({"id": "37i9dQZF1E4qxgJU46pFLr", "name": "Moderat Radio", "cover": "c1"})];
        let home = vec![
            json!({"id": "37i9dQZF1E4yLltmVk3nyb", "name": "Bonobo Radio", "cover": "c2"}),
            json!({"id": "37i9dQZF1E4qxgJU46pFLr", "name": "Moderat Radio", "cover": "c1"}),
        ];
        let played = vec!["37i9dQZF1E4yLltmVk3nyb".to_string(), "37i9dQZF1E4Anh2JxXd4Al".to_string()];
        let m = merge(&added, &home, &played);
        let ids: Vec<&str> = m.iter().map(|x| x["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["37i9dQZF1E4qxgJU46pFLr", "37i9dQZF1E4yLltmVk3nyb", "37i9dQZF1E4Anh2JxXd4Al"]);
        let sources: Vec<&str> = m.iter().map(|x| x["source"].as_str().unwrap()).collect();
        assert_eq!(sources, ["added", "made_for_you", "played"]);
        assert_eq!(m[1]["uri"], "spotify:playlist:37i9dQZF1E4yLltmVk3nyb");
        assert!(m[2]["name"].is_null(), "named later");
    }
}
