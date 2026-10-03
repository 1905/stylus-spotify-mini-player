//! Pathfinder (internal GraphQL) answers → the JSON shapes the UI already reads from the
//! Web API commands (spotify.rs). Pure functions; the fixtures in `fixtures/` are trimmed
//! real answers captured 2026-10-03.
//!
//! UI shapes: `Track` = `{id, uri, name, artists, artist_list:[{id,name}], album, cover, duration_ms}`,
//! album = `{id, name, artists, cover}`, artist = `{id, name, image}`.

use serde_json::{json, Value};

/// The id part of a `spotify:<kind>:<id>` uri ("" for anything else).
pub fn id_of(uri: &str) -> &str {
    let mut parts = uri.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("spotify"), Some(_), Some(id), None) => id,
        _ => "",
    }
}

/// The biggest image of a `sources` list: by width; without widths, the 640 px cover/portrait
/// (its url names the size) or else the first.
pub fn best_source(sources: &Value) -> Option<String> {
    let list = sources.as_array()?;
    let url = |s: &Value| s["url"].as_str().map(str::to_string);
    let widest = list.iter().filter(|s| s["width"].as_u64().or_else(|| s["maxWidth"].as_u64()).is_some()).max_by_key(|s| s["width"].as_u64().or_else(|| s["maxWidth"].as_u64()));
    if let Some(s) = widest {
        return url(s);
    }
    const BIG: [&str; 2] = ["ab67616d0000b273", "ab6761610000e5eb"];
    list.iter().find(|s| s["url"].as_str().is_some_and(|u| BIG.iter().any(|b| u.contains(b)))).or(list.first()).and_then(url)
}

/// `images.items[].sources[]` (playlists) as the Web API's `images` list, biggest first.
pub fn images(v: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|i| i["sources"].as_array().cloned().unwrap_or_default())
        .filter(|s| s["url"].is_string())
        .map(|s| json!({ "url": s["url"], "width": s["width"], "height": s["height"] }))
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s["width"].as_u64().unwrap_or(0)));
    out
}

/// `artists.items[]` → `[{id, name}]`.
fn artist_list(v: &Value) -> Vec<Value> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let name = a["profile"]["name"].as_str()?;
            Some(json!({ "id": id_of(a["uri"].as_str().unwrap_or("")), "name": name }))
        })
        .collect()
}

fn join_names(list: &[Value]) -> String {
    list.iter().filter_map(|a| a["name"].as_str()).collect::<Vec<_>>().join(", ")
}

/// A pathfinder `Track` → the UI's Track. `uri_hint`: the wrapper's `_uri` when the data has none
/// (Liked Songs). None for anything that isn't a playable track (episodes, removed items).
pub fn track(d: &Value, uri_hint: Option<&str>) -> Option<Value> {
    if d["__typename"].as_str().is_some_and(|t| t != "Track") {
        return None;
    }
    let uri = d["uri"].as_str().or(uri_hint).filter(|u| u.starts_with("spotify:track:"))?;
    let artists = artist_list(&d["artists"]);
    let duration = d["trackDuration"]["totalMilliseconds"].as_u64().or_else(|| d["duration"]["totalMilliseconds"].as_u64());
    Some(json!({
        "id": id_of(uri),
        "uri": uri,
        "name": d["name"].as_str().unwrap_or(""),
        "artists": join_names(&artists),
        "artist_list": artists,
        "album": d["albumOfTrack"]["name"],
        "cover": best_source(&d["albumOfTrack"]["coverArt"]["sources"]),
        "duration_ms": duration,
    }))
}

/// A pathfinder `Album` → `{id, name, artists, cover}`.
pub fn album(d: &Value) -> Option<Value> {
    let uri = d["uri"].as_str().filter(|u| u.starts_with("spotify:album:"))?;
    Some(json!({
        "id": id_of(uri),
        "name": d["name"],
        "artists": join_names(&artist_list(&d["artists"])),
        "cover": best_source(&d["coverArt"]["sources"]),
    }))
}

/// A pathfinder `Artist` → `{id, name, image}`.
pub fn artist(d: &Value) -> Option<Value> {
    let uri = d["uri"].as_str().filter(|u| u.starts_with("spotify:artist:"))?;
    Some(json!({
        "id": id_of(uri),
        "name": d["profile"]["name"],
        "image": best_source(&d["visuals"]["avatarImage"]["sources"]),
    }))
}

// ---- search ----------------------------------------------------------------------

/// `searchDesktop` → `{tracks, albums}` (`search`'s shape).
pub fn search(data: &Value) -> Value {
    let s = &data["searchV2"];
    json!({ "tracks": search_tracks(s), "albums": search_albums(s) })
}

fn search_tracks(s: &Value) -> Vec<Value> {
    s["tracksV2"]["items"].as_array().into_iter().flatten().filter_map(|i| track(&i["item"]["data"], None)).collect()
}

fn search_albums(s: &Value) -> Vec<Value> {
    s["albumsV2"]["items"].as_array().into_iter().flatten().filter_map(|i| album(&i["data"])).collect()
}

/// `searchTracks` / `searchAlbums` → `(items, raw item count, totalCount)`.
pub fn search_page(data: &Value, kind: &str) -> (Vec<Value>, usize, u64) {
    let s = &data["searchV2"];
    let (list, items) = if kind == "track" { (&s["tracksV2"], search_tracks(s)) } else { (&s["albumsV2"], search_albums(s)) };
    let got = list["items"].as_array().map_or(0, Vec::len);
    (items, got, list["totalCount"].as_u64().unwrap_or(0))
}

// ---- library ---------------------------------------------------------------------

/// `libraryV3` items' `item.data`, with the wrapper's `_uri` filled in as `uri` when missing.
fn library_items(data: &Value) -> Vec<Value> {
    data["me"]["libraryV3"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            let mut d = i["item"]["data"].clone();
            if d["uri"].is_null() {
                d["uri"] = i["item"]["_uri"].clone();
            }
            d
        })
        .collect()
}

/// `libraryV3` → its `totalCount`.
pub fn library_total(data: &Value) -> u64 {
    data["me"]["libraryV3"]["totalCount"].as_u64().unwrap_or(0)
}

/// A pathfinder `Playlist` → the Web API playlist fields the UI reads:
/// `{id, uri, name, images, tracks:{total}, snapshot_id, owner:{id, display_name}}`.
/// `total`: the track count (pathfinder doesn't give one; the rootlist does).
pub fn playlist(d: &Value, total: u64) -> Option<Value> {
    if d["__typename"].as_str().is_some_and(|t| t != "Playlist") {
        return None;
    }
    let uri = d["uri"].as_str().filter(|u| u.starts_with("spotify:playlist:"))?;
    let owner = &d["ownerV2"]["data"];
    Some(json!({
        "id": id_of(uri),
        "uri": uri,
        "name": d["name"].as_str().unwrap_or(""),
        "images": images(&d["images"]),
        "tracks": { "total": total },
        "snapshot_id": d["revisionId"],
        "owner": { "id": owner["username"], "display_name": owner["name"] },
    }))
}

/// `libraryV3` ["Playlists"] → playlists (folders and Liked Songs skipped), track counts 0.
pub fn library_playlists(data: &Value) -> Vec<Value> {
    library_items(data).iter().filter_map(|d| playlist(d, 0)).collect()
}

/// `libraryV3` ["Albums"] → `[{id, name, artists, cover, total_tracks: null}]` (no count here).
pub fn library_albums(data: &Value) -> Vec<Value> {
    library_items(data)
        .iter()
        .filter_map(album)
        .map(|mut a| {
            a["total_tracks"] = Value::Null;
            a
        })
        .collect()
}

/// `libraryV3` ["Artists"] → `[{id, name, image}]`.
pub fn library_artists(data: &Value) -> Vec<Value> {
    library_items(data).iter().filter_map(artist).collect()
}

// ---- playlists, liked, albums -------------------------------------------------------

/// One `fetchPlaylist` page → `(tracks, totalCount, revisionId)`.
pub fn playlist_page(data: &Value) -> (Vec<Value>, u64, Option<String>) {
    let p = &data["playlistV2"];
    let tracks = p["content"]["items"].as_array().into_iter().flatten().filter_map(|i| track(&i["itemV2"]["data"], None)).collect();
    (tracks, p["content"]["totalCount"].as_u64().unwrap_or(0), p["revisionId"].as_str().map(str::to_string))
}

/// `fetchPlaylist` (any page) → `{name, cover}` (`mix_info`'s shape), None when it has no name.
pub fn playlist_info(data: &Value) -> Option<Value> {
    let p = &data["playlistV2"];
    let name = p["name"].as_str().filter(|n| !n.is_empty())?;
    let cover = images(&p["images"]).first().and_then(|i| i["url"].as_str().map(str::to_string));
    Some(json!({ "name": name, "cover": cover }))
}

/// One `fetchLibraryTracks` page → `(tracks, totalCount)`.
pub fn liked_page(data: &Value) -> (Vec<Value>, u64) {
    let t = &data["me"]["library"]["tracks"];
    let tracks = t["items"].as_array().into_iter().flatten().filter_map(|i| track(&i["track"]["data"], i["track"]["_uri"].as_str())).collect();
    (tracks, t["totalCount"].as_u64().unwrap_or(0))
}

/// `getAlbum` → `(tracks with the album's name and cover stamped on, totalCount)`.
pub fn album_tracks(data: &Value) -> (Vec<Value>, u64) {
    let a = &data["albumUnion"];
    let cover = best_source(&a["coverArt"]["sources"]);
    let tracks = a["tracksV2"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| track(&i["track"], None))
        .map(|mut t| {
            t["album"] = a["name"].clone();
            t["cover"] = json!(cover);
            t
        })
        .collect();
    (tracks, a["tracksV2"]["totalCount"].as_u64().unwrap_or(0))
}

// ---- artists ---------------------------------------------------------------------------

/// `queryArtistOverview` → `{id, name, image, top_tracks: [Track]}`. The top tracks' album has
/// no name here (cover only).
pub fn artist_overview(data: &Value) -> Option<Value> {
    let a = &data["artistUnion"];
    let uri = a["uri"].as_str().filter(|u| u.starts_with("spotify:artist:"))?;
    let top: Vec<Value> = a["discography"]["topTracks"]["items"].as_array().into_iter().flatten().filter_map(|i| track(&i["track"], None)).collect();
    Some(json!({
        "id": id_of(uri),
        "name": a["profile"]["name"],
        "image": best_source(&a["visuals"]["avatarImage"]["sources"]),
        "top_tracks": top,
    }))
}

/// "single" for singles and EPs (the Web API files EPs as singles), else "album".
fn release_kind(t: &str) -> &'static str {
    if t == "SINGLE" || t == "EP" {
        "single"
    } else {
        "album"
    }
}

/// `queryArtistDiscographyAll` → `[{id, name, cover, year, kind}]` (`get_artist_albums`' shape),
/// newest first as Spotify sends them. A release with regional variants counts once.
pub fn discography(data: &Value) -> Vec<Value> {
    data["artistUnion"]["discography"]["all"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            let r = i["releases"]["items"].get(0)?;
            let uri = r["uri"].as_str().filter(|u| u.starts_with("spotify:album:"))?;
            let year = r["date"]["year"].as_u64().map(|y| y.to_string()).or_else(|| r["date"]["isoString"].as_str().map(|d| d.chars().take(4).collect()));
            Some(json!({
                "id": id_of(uri),
                "name": r["name"],
                "cover": best_source(&r["coverArt"]["sources"]),
                "year": year,
                "kind": release_kind(r["type"].as_str().unwrap_or("")),
            }))
        })
        .collect()
}

// ---- taste ---------------------------------------------------------------------------------

/// `userTopContent` → top tracks (`kind` "tracks") or artists ("artists").
pub fn top(data: &Value, kind: &str) -> Vec<Value> {
    let p = &data["me"]["profile"];
    let list = if kind == "tracks" { &p["topTracks"] } else { &p["topArtists"] };
    list["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| if kind == "tracks" { track(&i["data"], None) } else { artist(&i["data"]) })
        .collect()
}

/// `areEntitiesInLibrary` → the first entity's `saved`.
pub fn first_saved(data: &Value) -> Option<bool> {
    data["lookup"][0]["data"]["saved"].as_bool()
}

/// The Web API's time range → userTopContent's.
pub fn time_range(range: &str) -> Option<&'static str> {
    match range {
        "short_term" => Some("SHORT_TERM"),
        "medium_term" => Some("MID_TERM"),
        "long_term" => Some("LONG_TERM"),
        _ => None,
    }
}

// ---- history -------------------------------------------------------------------------------

/// One play context of `recently-played/v3`: `(context uri, last played track uri, ms)`.
pub fn recent_contexts(v: &Value) -> Vec<(String, String, i64)> {
    v["playContexts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let track = c["lastPlayedTrackUri"].as_str().filter(|u| u.starts_with("spotify:track:"))?;
            Some((c["uri"].as_str().unwrap_or("").to_string(), track.to_string(), c["lastPlayedTime"].as_i64().unwrap_or(0)))
        })
        .collect()
}

/// `fetchEntitiesForRecentlyPlayed` with track uris → uri → Track.
pub fn lookup_tracks(data: &Value) -> std::collections::HashMap<String, Value> {
    data["lookup"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| track(&i["data"], i["_uri"].as_str()))
        .filter_map(|t| Some((t["uri"].as_str()?.to_string(), t)))
        .collect()
}

/// Recent contexts + their tracks → `get_recently_played`'s `[{track, played_at, context_uri}]`,
/// newest first; contexts whose track has no details are skipped.
pub fn recent(contexts: &[(String, String, i64)], tracks: &std::collections::HashMap<String, Value>) -> Vec<Value> {
    let mut rows: Vec<&(String, String, i64)> = contexts.iter().collect();
    rows.sort_by_key(|(_, _, at)| std::cmp::Reverse(*at));
    rows.into_iter()
        .filter_map(|(ctx, uri, at)| {
            let track = tracks.get(uri)?;
            let context = if ctx.is_empty() { Value::Null } else { json!(ctx) };
            Some(json!({ "track": track, "played_at": iso_utc(*at), "context_uri": context }))
        })
        .collect()
}

/// Unix milliseconds → `2026-10-01T15:02:11Z` (the Web API's `played_at`).
pub fn iso_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant), valid for every date this app sees
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> Value {
        let path = format!("{}/src/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))).unwrap()
    }

    fn assert_track_shape(t: &Value) {
        for k in ["id", "uri", "name", "artists", "artist_list", "album", "cover", "duration_ms"] {
            assert!(t.get(k).is_some(), "{k} missing in {t}");
        }
        assert!(t["uri"].as_str().unwrap().ends_with(t["id"].as_str().unwrap()));
    }

    #[test]
    fn ids_and_images() {
        assert_eq!(id_of("spotify:track:abc"), "abc");
        assert_eq!(id_of("spotify:user:u:collection"), "");
        assert_eq!(id_of("nope"), "");
        let s = json!([{"url": "s", "width": 64}, {"url": "l", "width": 640}, {"url": "m", "width": 300}]);
        assert_eq!(best_source(&s).as_deref(), Some("l"));
        let no_widths = json!([{"url": "https://i.scdn.co/image/ab67616d00001e02x"}, {"url": "https://i.scdn.co/image/ab67616d0000b273x"}]);
        assert_eq!(best_source(&no_widths).as_deref(), Some("https://i.scdn.co/image/ab67616d0000b273x"));
        assert_eq!(best_source(&json!([{"url": "a", "width": null}])).as_deref(), Some("a"));
        assert_eq!(best_source(&json!([])), None);
        assert_eq!(best_source(&Value::Null), None);
        let imgs = images(&json!({"items": [{"sources": [{"url": "a", "width": 60}, {"url": "b", "width": 640}]}]}));
        assert_eq!(imgs[0]["url"], "b");
    }

    #[test]
    fn search_desktop_fixture() {
        let r = search(&fx("searchDesktop"));
        let (t, a) = (r["tracks"].as_array().unwrap(), r["albums"].as_array().unwrap());
        assert_eq!((t.len(), a.len()), (3, 2));
        assert_track_shape(&t[0]);
        assert_eq!(t[0]["artist_list"][0]["id"], "4Z8W4fKeB5YxbusRsdQVPb");
        assert!(t[0]["cover"].as_str().unwrap().contains("ab67616d0000b273"), "640 px cover");
        assert!(t[0]["duration_ms"].as_u64().unwrap() > 0);
        assert_eq!(a[0]["artists"], "Radiohead");
        assert!(a[0]["cover"].as_str().unwrap().starts_with("https://i.scdn.co/"));
    }

    #[test]
    fn search_pages_fixture() {
        let (items, got, total) = search_page(&fx("searchTracks"), "track");
        assert_eq!((items.len(), got), (3, 3));
        assert!(total > 100);
        let (items, _, total) = search_page(&fx("searchAlbums"), "album");
        assert_eq!(items[0]["name"], "OK Computer");
        assert!(total > 10);
    }

    #[test]
    fn library_fixtures() {
        let pls = library_playlists(&fx("libraryPlaylists"));
        // the fixture has Liked Songs (pseudo), a folder and 3 playlists
        assert_eq!(pls.len(), 3);
        let p = &pls[0];
        assert!(p["id"].as_str().unwrap().len() == 22);
        assert!(p["images"][0]["url"].as_str().is_some());
        assert!(p["snapshot_id"].as_str().is_some());
        assert_eq!(p["tracks"]["total"], 0);
        assert!(p["owner"]["id"].as_str().is_some());
        let albums = library_albums(&fx("libraryAlbums"));
        assert_eq!(albums.len(), 2);
        assert!(albums[0]["cover"].as_str().is_some() && albums[0]["artists"].as_str().is_some());
        let artists = library_artists(&fx("libraryArtists"));
        assert_eq!(artists.len(), 2);
        assert!(artists[0]["image"].as_str().unwrap().contains("ab6761610000e5eb"));
        assert_eq!(library_total(&fx("libraryArtists")), 11);
    }

    #[test]
    fn playlist_fixture() {
        let (tracks, total, rev) = playlist_page(&fx("fetchPlaylist"));
        assert_eq!(tracks.len(), 2);
        assert_eq!(total, 50);
        assert_eq!(rev.as_deref(), Some("AAAAAOuaTAK0A/u5OHDTUPj+eUBv59Mk"));
        assert_track_shape(&tracks[0]);
        assert!(tracks[0]["album"].as_str().is_some());
        let info = playlist_info(&fx("fetchPlaylist")).unwrap();
        assert!(info["name"].as_str().is_some() && info["cover"].as_str().unwrap().starts_with("https://"));
        assert_eq!(playlist_info(&json!({"playlistV2": {"name": ""}})), None);
    }

    #[test]
    fn liked_fixture() {
        let (tracks, total) = liked_page(&fx("fetchLibraryTracks"));
        assert_eq!((tracks.len(), total), (2, 137));
        // the uri comes from the wrapper's _uri
        assert_eq!(tracks[0]["uri"], "spotify:track:3jPrWXnA9EXrMn4qAX68uf");
        assert_track_shape(&tracks[0]);
    }

    #[test]
    fn album_fixture() {
        let (tracks, total) = album_tracks(&fx("getAlbum"));
        assert_eq!((tracks.len(), total), (2, 12));
        assert_eq!(tracks[0]["album"], "OK Computer");
        assert_eq!(tracks[0]["name"], "Airbag");
        assert!(tracks[1]["cover"].as_str().unwrap().contains("ab67616d0000b273"));
    }

    #[test]
    fn artist_fixtures() {
        let a = artist_overview(&fx("queryArtistOverview")).unwrap();
        assert_eq!(a["name"], "Radiohead");
        assert_eq!(a["id"], "4Z8W4fKeB5YxbusRsdQVPb");
        assert!(a["image"].as_str().unwrap().contains("ab6761610000e5eb"));
        let top = a["top_tracks"].as_array().unwrap();
        assert_eq!(top[0]["name"], "Creep");
        assert!(top[0]["cover"].as_str().unwrap().contains("ab67616d0000b273"), "no widths: the 640 url");
        let d = discography(&fx("queryArtistDiscographyAll"));
        assert_eq!(d.len(), 3);
        assert_eq!(d[0], json!({"id": "5BLrEOEDKoDDg5T8PzdIHN", "name": "Hail to the Thief (Live Recordings 2003-2009)",
            "cover": d[0]["cover"], "year": "2025", "kind": "album"}));
        assert!(d.iter().any(|x| x["kind"] == "single"));
        assert_eq!(release_kind("EP"), "single");
        assert_eq!(release_kind("COMPILATION"), "album");
    }

    #[test]
    fn top_fixture() {
        let v = fx("userTopContent");
        let t = top(&v, "tracks");
        assert_eq!(t.len(), 2);
        assert_track_shape(&t[0]);
        let a = top(&v, "artists");
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["name"], "Carbon Based Lifeforms");
        assert_eq!(time_range("medium_term"), Some("MID_TERM"));
        assert_eq!(time_range("x"), None);
    }

    #[test]
    fn saved_and_recent() {
        assert_eq!(first_saved(&fx("areEntitiesInLibrary")), Some(false));
        assert_eq!(first_saved(&json!({})), None);
        let r = recent_contexts(&fx("recentlyPlayed"));
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "spotify:playlist:37i9dQZF1E4yLltmVk3nyb");
        assert_eq!(r[0].1, "spotify:track:2zMAyNJpHXrFmeQ5b4VPS3");
        assert_eq!(iso_utc(r[0].2), "2026-10-03T00:31:25Z");
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400_000), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn recent_rows() {
        let contexts = recent_contexts(&fx("recentlyPlayed"));
        let lookup = lookup_tracks(&fx("entitiesTracks"));
        assert_eq!(lookup.len(), 1);
        let creep = lookup.values().next().unwrap().clone();
        assert_eq!(creep["name"], "Creep");
        // both contexts' tracks known: rows newest first, with the context
        let mut tracks = std::collections::HashMap::new();
        tracks.insert(contexts[0].1.clone(), creep.clone());
        tracks.insert(contexts[1].1.clone(), creep);
        let rows = recent(&contexts, &tracks);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["context_uri"], "spotify:playlist:37i9dQZF1E4yLltmVk3nyb");
        assert_eq!(rows[0]["played_at"], "2026-10-03T00:31:25Z");
        assert!(rows[0]["played_at"].as_str() > rows[1]["played_at"].as_str());
        // a track without details is skipped
        tracks.remove(&contexts[1].1);
        assert_eq!(recent(&contexts, &tracks).len(), 1);
    }

    #[test]
    fn non_tracks_are_skipped() {
        assert_eq!(track(&json!({"__typename": "Episode", "uri": "spotify:episode:x"}), None), None);
        assert_eq!(track(&json!({"__typename": "Track", "name": "no uri"}), None), None);
        assert_eq!(track(&json!({"__typename": "Track", "uri": "spotify:local:a:b:c:1"}), None), None);
        let t = track(&json!({"uri": "spotify:track:x", "name": "n"}), None).unwrap();
        assert!(t["cover"].is_null() && t["duration_ms"].is_null() && t["artists"] == "");
    }
}
