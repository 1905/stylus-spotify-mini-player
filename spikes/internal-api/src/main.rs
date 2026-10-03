//! Spike: which Spotify internal endpoints a librespot 0.8.0 Session can reach.
//! Logs in with Needle's stored credentials under a fresh device id (Session only, no
//! Spirc / Connect device), probes each endpoint once and prints status + a short summary.
//! Never prints tokens or credentials.
//!
//! Usage: cargo run --release -- [probe ...]   (no args = all probes)

use std::collections::BTreeMap;

use librespot_core::{authentication::Credentials, Session, SessionConfig, SpotifyId};
use librespot_protocol::{
    extended_metadata::{BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery},
    extension_kind::ExtensionKind,
    metadata,
    playlist4_external::SelectedListContent,
};
use protobuf::{EnumOrUnknown, Message as _};
use prost::Message as _;
use serde_json::{json, Value};

// ---- collection v2 messages (collection2v2.proto, not compiled by librespot-protocol) ----
#[derive(Clone, PartialEq, prost::Message)]
struct PageRequest {
    #[prost(string, tag = "1")]
    username: String,
    #[prost(string, tag = "2")]
    set: String,
    #[prost(string, tag = "3")]
    pagination_token: String,
    #[prost(int32, tag = "4")]
    limit: i32,
}
#[derive(Clone, PartialEq, prost::Message)]
struct CollectionItem {
    #[prost(string, tag = "1")]
    uri: String,
    #[prost(int32, tag = "2")]
    added_at: i32,
    #[prost(bool, tag = "3")]
    is_removed: bool,
}
#[derive(Clone, PartialEq, prost::Message)]
struct PageResponse {
    #[prost(message, repeated, tag = "1")]
    items: Vec<CollectionItem>,
    #[prost(string, tag = "2")]
    next_page_token: String,
    #[prost(string, tag = "3")]
    sync_token: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct WriteRequest {
    #[prost(string, tag = "1")]
    username: String,
    #[prost(string, tag = "2")]
    set: String,
    #[prost(message, repeated, tag = "3")]
    items: Vec<CollectionItem>,
    #[prost(string, tag = "4")]
    client_update_id: String,
}
const PATHFINDER: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const ARTIST: &str = "4Z8W4fKeB5YxbusRsdQVPb"; // Radiohead
const ALBUM: &str = "6dVIqQ8qmQ5GBnJ9shOYGE"; // OK Computer
const EDITORIAL: &str = "37i9dQZF1DXcBWIGoYBM5M"; // Today's Top Hits (Spotify-owned)

struct Api {
    session: Session,
    http: reqwest::Client,
    base: String,
    user: String,
    hashes: BTreeMap<String, String>,
}

struct Resp {
    status: u16,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn snippet(&self) -> String {
        let s = String::from_utf8_lossy(&self.body);
        s.chars().take(300).collect::<String>().replace('\n', " ")
    }
}

impl Api {
    async fn call(&self, method: reqwest::Method, url: &str, ctype: Option<&str>, accept: Option<&str>, body: Option<Vec<u8>>) -> Resp {
        let token = match self.session.login5().auth_token().await {
            Ok(t) => t,
            Err(e) => return Resp { status: 0, body: format!("login5 token error: {e}").into_bytes() },
        };
        let mut rb = self
            .http
            .request(method, url)
            .header("authorization", format!("Bearer {}", token.access_token))
            .header("app-platform", "OSX")
            .header("spotify-app-version", "1.2.52.442");
        match self.session.spclient().client_token().await {
            Ok(ct) => rb = rb.header("client-token", ct),
            Err(e) => eprintln!("   (no client token: {e})"),
        }
        if let Some(c) = ctype {
            rb = rb.header("content-type", c);
        }
        if let Some(a) = accept {
            rb = rb.header("accept", a);
        }
        if let Some(b) = body {
            rb = rb.body(b);
        }
        match rb.send().await {
            Ok(r) => {
                let status = r.status().as_u16();
                let body = r.bytes().await.map(|b| b.to_vec()).unwrap_or_default();
                Resp { status, body }
            }
            Err(e) => Resp { status: 0, body: format!("transport error: {e}").into_bytes() },
        }
    }

    async fn sp_get(&self, path: &str, accept: Option<&str>) -> Resp {
        self.call(reqwest::Method::GET, &format!("{}{}", self.base, path), None, accept, None).await
    }

    async fn sp_post_pb(&self, path: &str, body: Vec<u8>) -> Resp {
        // collection v2 rejects plain application/x-protobuf with a bare 400
        let ct = if path.starts_with("/collection/v2/") { "application/vnd.collection-v2.spotify.proto" } else { "application/x-protobuf" };
        self.call(reqwest::Method::POST, &format!("{}{}", self.base, path), Some(ct), Some(ct), Some(body))
            .await
    }

    async fn pathfinder(&self, op: &str, vars: Value) -> Resp {
        let Some(hash) = self.hashes.get(op) else {
            return Resp { status: 0, body: format!("no hash for {op}").into_bytes() };
        };
        let body = json!({
            "variables": vars,
            "operationName": op,
            "extensions": {"persistedQuery": {"version": 1, "sha256Hash": hash}},
        });
        self.call(reqwest::Method::POST, PATHFINDER, Some("application/json;charset=UTF-8"), Some("application/json"), Some(body.to_string().into_bytes()))
            .await
    }

    /// extended-metadata batch: (uri, raw bytes) for one kind
    async fn ext_meta(&self, uris: &[String], kind: ExtensionKind) -> (u16, Vec<(String, Vec<u8>)>) {
        let req = BatchedEntityRequest {
            entity_request: uris
                .iter()
                .map(|u| EntityRequest {
                    entity_uri: u.clone(),
                    query: vec![ExtensionQuery { extension_kind: EnumOrUnknown::new(kind), ..Default::default() }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let r = self.sp_post_pb("/extended-metadata/v0/extended-metadata", req.write_to_bytes().unwrap()).await;
        if r.status != 200 {
            return (r.status, vec![]);
        }
        let Ok(parsed) = BatchedExtensionResponse::parse_from_bytes(&r.body) else {
            return (r.status, vec![]);
        };
        let mut out = vec![];
        for arr in parsed.extended_metadata {
            for d in arr.extension_data {
                if let Some(any) = d.extension_data.into_option() {
                    out.push((d.entity_uri, any.value));
                }
            }
        }
        (r.status, out)
    }

    async fn track_names(&self, uris: &[String]) -> Vec<String> {
        let (_, data) = self.ext_meta(uris, ExtensionKind::TRACK_V4).await;
        data.iter()
            .filter_map(|(_, b)| metadata::Track::parse_from_bytes(b).ok())
            .map(|t| format!("{} — {}", t.name(), t.artist.first().map(|a| a.name()).unwrap_or("?")))
            .collect()
    }
}

fn gid_b62(gid: &[u8]) -> String {
    SpotifyId::from_raw(gid).ok().and_then(|i| i.to_base62().ok()).unwrap_or_default()
}

fn head(label: &str) {
    println!("\n=== {label}");
}

fn line(name: &str, r: &Resp, summary: impl std::fmt::Display) {
    println!("-- {name}: HTTP {} {} bytes | {summary}", r.status, r.body.len());
    if r.status != 200 && r.status != 0 {
        println!("   body: {}", r.snippet());
    } else if r.status == 0 {
        println!("   {}", r.snippet());
    }
}

/// short JSON shape: top-level keys, or errors
fn shape(v: &Value) -> String {
    if let Some(errs) = v.get("errors") {
        return format!("errors: {}", errs.to_string().chars().take(300).collect::<String>());
    }
    match v {
        Value::Object(m) => format!("keys {:?}", m.keys().take(8).collect::<Vec<_>>()),
        _ => "non-object".into(),
    }
}

fn names_at(v: &Value, ptr_list: &str, ptr_name: &str, n: usize) -> Vec<String> {
    v.pointer(ptr_list)
        .and_then(Value::as_array)
        .map(|a| a.iter().take(n).filter_map(|i| i.pointer(ptr_name).and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default()
}

fn count_at(v: &Value, ptr: &str) -> String {
    v.pointer(ptr).map(|x| x.to_string()).unwrap_or("-".into())
}

// ---- probes ----

async fn search(api: &Api) {
    head("1. search 'radiohead'");
    let r = api
        .pathfinder(
            "searchDesktop",
            json!({"searchTerm": "radiohead", "offset": 0, "limit": 10, "numberOfTopResults": 5, "includeAudiobooks": false,
                   "includeArtistHasConcertsField": false, "includePreReleases": false, "includeLocalConcertsField": false, "includeAuthors": false}),
        )
        .await;
    let v = r.json();
    let s = "/data/searchV2";
    line(
        "pathfinder searchDesktop",
        &r,
        format!(
            "{} | tracks total {} {:?} | albums total {} {:?} | artists total {} {:?} | playlists total {} {:?}",
            shape(&v),
            count_at(&v, &format!("{s}/tracksV2/totalCount")),
            names_at(&v, &format!("{s}/tracksV2/items"), "/item/data/name", 3),
            count_at(&v, &format!("{s}/albumsV2/totalCount")),
            names_at(&v, &format!("{s}/albumsV2/items"), "/data/name", 3),
            count_at(&v, &format!("{s}/artists/totalCount")),
            names_at(&v, &format!("{s}/artists/items"), "/data/profile/name", 3),
            count_at(&v, &format!("{s}/playlists/totalCount")),
            names_at(&v, &format!("{s}/playlists/items"), "/data/name", 3),
        ),
    );
    // per-type search with paging (tracks page 2)
    let r = api
        .pathfinder("searchTracks", json!({"searchTerm": "radiohead", "offset": 10, "limit": 10, "numberOfTopResults": 20, "includeAudiobooks": false, "includePreReleases": false, "includeAuthors": false}))
        .await;
    let v = r.json();
    line(
        "pathfinder searchTracks offset=10",
        &r,
        format!("{} | total {} {:?}", shape(&v), count_at(&v, "/data/searchV2/tracksV2/totalCount"), names_at(&v, "/data/searchV2/tracksV2/items", "/item/data/name", 3)),
    );

    match api.session.spclient().get_context("spotify:search:radiohead").await {
        Ok(ctx) => {
            let n: usize = ctx.pages.iter().map(|p| p.tracks.len()).sum();
            let first: Vec<_> = ctx.pages.iter().flat_map(|p| p.tracks.iter()).take(3).map(|t| t.uri().to_string()).collect();
            println!("-- context-resolve spotify:search:radiohead: OK | pages {} tracks {} first {:?}", ctx.pages.len(), n, first);
        }
        Err(e) => println!("-- context-resolve spotify:search:radiohead: ERR {e}"),
    }
    let country = api.session.country();
    let r = api
        .sp_get(&format!("/searchview/km/v4/search/radiohead?limit=10&entityVersion=2&catalogue=premium&country={country}&locale=en&username={}", api.user), Some("application/json"))
        .await;
    line("spclient searchview/km/v4", &r, shape(&r.json()));
}

async fn rootlist(api: &Api) -> Vec<(String, String)> {
    head("2. rootlist (own playlists)");
    let r = api
        .sp_get(&format!("/playlist/v2/user/{}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from=0&length=120", api.user), None)
        .await;
    let mut out = vec![];
    match SelectedListContent::parse_from_bytes(&r.body) {
        Ok(c) if r.status == 200 => {
            let items = &c.contents.items;
            let metas = &c.contents.meta_items;
            let n_pl = items.iter().filter(|i| i.uri().starts_with("spotify:playlist:")).count();
            let n_folder = items.iter().filter(|i| i.uri().contains(":start-group:")).count();
            let with_pic = metas.iter().filter(|m| m.attributes.picture.is_some() || !m.attributes.picture_size.is_empty()).count();
            let sample: Vec<String> = items
                .iter()
                .zip(metas.iter())
                .filter(|(i, _)| i.uri().starts_with("spotify:playlist:"))
                .take(3)
                .map(|(_, m)| {
                    format!(
                        "{} (len {}, owner {}, pic_sizes {})",
                        m.attributes.name(),
                        m.length(),
                        if m.owner_username() == api.user { "me" } else { "other" },
                        m.attributes.picture_size.iter().map(|p| p.target_name()).collect::<Vec<_>>().join("/")
                    )
                })
                .collect();
            line("spclient playlist/v2/user/{me}/rootlist", &r, format!(
                "protobuf SelectedListContent | total length {} | items {} (playlists {}, folder markers {}) | meta_items {} with picture {} | {:?}",
                c.length(), items.len(), n_pl, n_folder, metas.len(), with_pic, sample
            ));
            for (i, m) in items.iter().zip(metas.iter()) {
                if i.uri().starts_with("spotify:playlist:") {
                    out.push((i.uri().to_string(), m.owner_username().to_string()));
                }
            }
        }
        _ => line("spclient rootlist", &r, "parse failed / not 200"),
    }
    let r = api
        .pathfinder(
            "libraryV3",
            json!({"filters": ["Playlists"], "order": null, "textFilter": "", "features": ["LIKED_SONGS", "YOUR_EPISODES"], "limit": 50, "offset": 0, "flatten": false, "expandedFolders": [], "folderUri": null, "includeFoldersWhenFlattening": true}),
        )
        .await;
    let v = r.json();
    line(
        "pathfinder libraryV3 (Playlists)",
        &r,
        format!("{} | total {} {:?}", shape(&v), count_at(&v, "/data/me/libraryV3/totalCount"), names_at(&v, "/data/me/libraryV3/items", "/item/data/name", 3)),
    );
    if let Some(items) = v.pointer("/data/me/libraryV3/items").and_then(Value::as_array) {
        for it in items.iter().take(5) {
            let d = &it["item"]["data"];
            let img = d.pointer("/images/items/0/sources/0/url").and_then(Value::as_str).map(|u| u.split('/').take(3).collect::<Vec<_>>().join("/"));
            println!("   libraryV3 item {} | type {} | image host {:?} | data keys {:?}", d["name"].as_str().unwrap_or("?"), d["__typename"], img, d.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default());
        }
    }
    out
}

async fn playlist(api: &Api, pls: &[(String, String)]) {
    head("3. playlist tracks");
    let mine = pls.iter().find(|(_, o)| *o == api.user).map(|(u, _)| u.clone());
    let other = pls.iter().find(|(u, o)| *o != api.user && *o != "spotify" && !u.contains(":37i9")).map(|(u, _)| u.clone());
    let personal = pls.iter().find(|(u, _)| u.contains(":37i9dQZF1E")).map(|(u, _)| u.clone());
    let mut targets = vec![];
    if let Some(u) = mine { targets.push(("own", u)); }
    if let Some(u) = other { targets.push(("other user's (followed)", u)); }
    targets.push(("Spotify editorial 37i9…", format!("spotify:playlist:{EDITORIAL}")));
    if let Some(u) = personal { targets.push(("personal algorithmic 37i9dQZF1E…", u)); }
    for (label, uri) in targets {
        let id = uri.rsplit(':').next().unwrap();
        let r = api.sp_get(&format!("/playlist/v2/playlist/{id}?from=0&length=100"), None).await;
        match SelectedListContent::parse_from_bytes(&r.body) {
            Ok(c) if r.status == 200 => {
                let uris: Vec<String> = c.contents.items.iter().take(3).map(|i| i.uri().to_string()).collect();
                let names = api.track_names(&uris).await;
                line(&format!("spclient playlist/v2/playlist ({label})"), &r, format!(
                    "name '{}' length {} items-in-page {} truncated {} pic_sizes {} | first {:?}",
                    c.attributes.name(), c.length(), c.contents.items.len(), c.contents.truncated(), c.attributes.picture_size.len(), names
                ));
            }
            _ => line(&format!("spclient playlist/v2/playlist ({label})"), &r, "not 200 / parse failed"),
        }
    }
    let r = api.pathfinder("fetchPlaylist", json!({"uri": format!("spotify:playlist:{EDITORIAL}"), "offset": 0, "limit": 25, "enableWatchFeedEntrypoint": false})).await;
    let v = r.json();
    line("pathfinder fetchPlaylist (editorial)", &r, format!(
        "{} | name {} total {} {:?}",
        shape(&v), count_at(&v, "/data/playlistV2/name"), count_at(&v, "/data/playlistV2/content/totalCount"),
        names_at(&v, "/data/playlistV2/content/items", "/itemV2/data/name", 3)
    ));
}

async fn collection_page(api: &Api, set: &str, token: &str, limit: i32) -> (Resp, Option<PageResponse>) {
    let req = PageRequest { username: api.user.clone(), set: set.into(), pagination_token: token.into(), limit };
    let r = api.sp_post_pb("/collection/v2/paging", req.encode_to_vec()).await;
    let p = if r.status == 200 { PageResponse::decode(r.body.as_slice()).ok() } else { None };
    (r, p)
}

fn kinds(items: &[CollectionItem]) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for i in items {
        *m.entry(i.uri.split(':').nth(1).unwrap_or("?").to_string()).or_insert(0) += 1;
    }
    m
}

async fn liked(api: &Api) {
    head("4/5/6. collection: liked songs, saved albums, followed artists");
    let (r, p) = collection_page(api, "collection", "", 50).await;
    if let Some(p) = p {
        let first: Vec<String> = p.items.iter().filter(|i| i.uri.starts_with("spotify:track:")).take(3).map(|i| i.uri.clone()).collect();
        let names = api.track_names(&first).await;
        line("collection/v2/paging set=collection page1", &r, format!(
            "protobuf PageResponse | items {} kinds {:?} next_token? {} | first tracks {:?}",
            p.items.len(), kinds(&p.items), !p.next_page_token.is_empty(), names
        ));
        if !p.next_page_token.is_empty() {
            let (r2, p2) = collection_page(api, "collection", &p.next_page_token, 50).await;
            let p2 = p2.unwrap_or_default();
            let overlap = p2.items.iter().filter(|i| p.items.iter().any(|j| j.uri == i.uri)).count();
            let ts = |v: &[CollectionItem]| (v.iter().map(|i| i.added_at).min().unwrap_or(0), v.iter().map(|i| i.added_at).max().unwrap_or(0));
            line("collection/v2/paging set=collection page2", &r2, format!("items {} overlap with page1 {} | added_at range p1 {:?} p2 {:?} | p1 first added_at {} last {}", p2.items.len(), overlap, ts(&p.items), ts(&p2.items), p.items.first().map(|i| i.added_at).unwrap_or(0), p.items.last().map(|i| i.added_at).unwrap_or(0)));
        }
    } else {
        line("collection/v2/paging set=collection", &r, "-");
    }
    for set in ["artist", "album", "albums"] {
        let (r, p) = collection_page(api, set, "", 50).await;
        line(&format!("collection/v2/paging set={set}"), &r, match p {
            Some(p) => format!("items {} kinds {:?} next_token? {} first {:?}", p.items.len(), kinds(&p.items), !p.next_page_token.is_empty(), p.items.iter().take(3).map(|i| i.uri.clone()).collect::<Vec<_>>()),
            None => "-".into(),
        });
    }
    let r = api.pathfinder("fetchLibraryTracks", json!({"uri": format!("spotify:user:{}:collection", api.user), "offset": 0, "limit": 50})).await;
    let v = r.json();
    line("pathfinder fetchLibraryTracks", &r, format!(
        "{} | total {} {:?}", shape(&v), count_at(&v, "/data/me/library/tracks/totalCount"), names_at(&v, "/data/me/library/tracks/items", "/track/data/name", 3)
    ));
    for (filter, label) in [("Albums", "albums"), ("Artists", "artists")] {
        let r = api
            .pathfinder("libraryV3", json!({"filters": [filter], "order": null, "textFilter": "", "features": ["LIKED_SONGS", "YOUR_EPISODES"], "limit": 50, "offset": 0, "flatten": false, "expandedFolders": [], "folderUri": null, "includeFoldersWhenFlattening": true}))
            .await;
        let v = r.json();
        line(&format!("pathfinder libraryV3 ({label})"), &r, format!("{} | total {} {:?}", shape(&v), count_at(&v, "/data/me/libraryV3/totalCount"), names_at(&v, "/data/me/libraryV3/items", if label == "artists" { "/item/data/profile/name" } else { "/item/data/name" }, 3)));
    }
}

async fn artist(api: &Api) {
    head("7. artist page (Radiohead)");
    let (st, data) = api.ext_meta(&[format!("spotify:artist:{ARTIST}")], ExtensionKind::ARTIST_V4).await;
    match data.first().and_then(|(_, b)| metadata::Artist::parse_from_bytes(b).ok()) {
        Some(a) => {
            let country = api.session.country();
            let top = a.top_track.iter().find(|t| t.country() == country).or(a.top_track.first());
            let top_uris: Vec<String> = top.map(|t| t.track.iter().take(3).map(|tr| format!("spotify:track:{}", gid_b62(tr.gid()))).collect()).unwrap_or_default();
            let names = api.track_names(&top_uris).await;
            let albums: usize = a.album_group.iter().map(|g| g.album.len()).sum();
            let singles: usize = a.single_group.iter().map(|g| g.album.len()).sum();
            println!(
                "-- extended-metadata ARTIST_V4: HTTP {st} | name '{}' top_track countries {} (top for {country}: {} tracks) | album_groups {} albums {} singles {} portraits {} | top3 {:?}",
                a.name(), a.top_track.len(), top.map(|t| t.track.len()).unwrap_or(0), a.album_group.len(), albums, singles, a.portrait.len(), names
            );
            let alb_uris: Vec<String> = a.album_group.iter().filter_map(|g| g.album.first()).take(3).map(|al| format!("spotify:album:{}", gid_b62(al.gid()))).collect();
            let (_, ad) = api.ext_meta(&alb_uris, ExtensionKind::ALBUM_V4).await;
            let an: Vec<String> = ad.iter().filter_map(|(_, b)| metadata::Album::parse_from_bytes(b).ok()).map(|a| a.name().to_string()).collect();
            println!("   first album names via ALBUM_V4 batch: {an:?}");
        }
        None => println!("-- extended-metadata ARTIST_V4: HTTP {st} | no data"),
    }
    match api.session.spclient().get_context(&format!("spotify:artist:{ARTIST}")).await {
        Ok(ctx) => println!("-- context-resolve artist: OK | pages {} first page tracks {}", ctx.pages.len(), ctx.pages.first().map(|p| p.tracks.len()).unwrap_or(0)),
        Err(e) => println!("-- context-resolve artist: ERR {e}"),
    }
    let r = api.pathfinder("queryArtistOverview", json!({"uri": format!("spotify:artist:{ARTIST}"), "locale": "", "includePrerelease": true})).await;
    let v = r.json();
    line("pathfinder queryArtistOverview", &r, format!(
        "{} | name {} top {:?} albums total {}",
        shape(&v), count_at(&v, "/data/artistUnion/profile/name"),
        names_at(&v, "/data/artistUnion/discography/topTracks/items", "/track/name", 3),
        count_at(&v, "/data/artistUnion/discography/albums/totalCount")
    ));
}

async fn recent(api: &Api) {
    head("8. recently played");
    let r = api
        .sp_get(&format!("/recently-played/v3/user/{}/recently-played?format=json&offset=0&limit=20&filter=default,collection-new-episodes", api.user), Some("application/json"))
        .await;
    let v = r.json();
    let uris: Vec<String> = names_at(&v, "/playContexts", "/uri", 5);
    line("spclient recently-played/v3", &r, format!("JSON {} | contexts {} first {:?}", shape(&v), v["playContexts"].as_array().map(|a| a.len()).unwrap_or(0), uris));
    if !uris.is_empty() {
        let r = api.pathfinder("fetchEntitiesForRecentlyPlayed", json!({"uris": uris})).await;
        let v = r.json();
        line("pathfinder fetchEntitiesForRecentlyPlayed", &r, format!("{} | lookup {}", shape(&v), v.pointer("/data/lookup").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0)));
    }
}

async fn album(api: &Api) -> Vec<String> {
    head("9. album tracks (OK Computer)");
    let (st, data) = api.ext_meta(&[format!("spotify:album:{ALBUM}")], ExtensionKind::ALBUM_V4).await;
    let mut uris = vec![];
    match data.first().and_then(|(_, b)| metadata::Album::parse_from_bytes(b).ok()) {
        Some(a) => {
            uris = a.disc.iter().flat_map(|d| d.track.iter()).map(|t| format!("spotify:track:{}", gid_b62(t.gid()))).collect();
            let names = api.track_names(&uris[..3.min(uris.len())]).await;
            println!(
                "-- extended-metadata ALBUM_V4: HTTP {st} | '{}' by {} discs {} tracks {} covers {} | first {:?} (names via TRACK_V4 batch)",
                a.name(), a.artist.first().map(|x| x.name()).unwrap_or("?"), a.disc.len(), uris.len(), a.cover_group.image.len().max(a.cover.len()), names
            );
        }
        None => println!("-- extended-metadata ALBUM_V4: HTTP {st} | no data"),
    }
    let r = api.pathfinder("getAlbum", json!({"uri": format!("spotify:album:{ALBUM}"), "locale": "", "offset": 0, "limit": 50})).await;
    let v = r.json();
    line("pathfinder getAlbum", &r, format!("{} | name {} tracks {} {:?}", shape(&v), count_at(&v, "/data/albumUnion/name"), count_at(&v, "/data/albumUnion/tracksV2/totalCount"), names_at(&v, "/data/albumUnion/tracksV2/items", "/track/name", 3)));
    uris
}

async fn contains(api: &Api, uri: &str) -> Option<bool> {
    // /collection/v2/contains with a guessed {username,set,items} body answers 400; pathfinder instead
    let r = api.pathfinder("areEntitiesInLibrary", json!({"uris": [uri]})).await;
    let v = r.json();
    let found = v.pointer("/data/lookup/0/data/saved").and_then(Value::as_bool);
    if found.is_none() {
        line("pathfinder areEntitiesInLibrary", &r, shape(&v));
    }
    found
}

async fn write(api: &Api, uri: &str, removed: bool) -> u16 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i32;
    let req = WriteRequest {
        username: api.user.clone(),
        set: "collection".into(),
        items: vec![CollectionItem { uri: uri.into(), added_at: if removed { 0 } else { now }, is_removed: removed }],
        client_update_id: format!("{:x}", rand_u64()),
    };
    let r = api.sp_post_pb("/collection/v2/write", req.encode_to_vec()).await;
    line(&format!("collection/v2/write {}", if removed { "remove" } else { "add" }), &r, "");
    r.status
}

fn rand_u64() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

async fn save(api: &Api, album_tracks: &[String]) {
    head("10. save/unsave a track (reversible: only a track that is NOT liked; add then remove)");
    let mut target = None;
    for u in album_tracks.iter().rev().take(4) {
        match contains(api, u).await {
            Some(false) => {
                target = Some(u.clone());
                break;
            }
            Some(true) => println!("   {u} already liked, skipping"),
            None => return,
        }
    }
    let Some(t) = target else {
        println!("   no un-liked candidate; skipped write test");
        return;
    };
    println!("-- contains({t}) = false (verified)");
    if write(api, &t, false).await != 200 {
        return;
    }
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    println!("   after add: contains = {:?}", contains(api, &t).await);
    write(api, &t, true).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    println!("   after remove: contains = {:?}", contains(api, &t).await);
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let want = |p: &str| args.is_empty() || args.iter().any(|a| a == p);

    let path = dirs::data_dir().unwrap().join("needle/player-credentials.json");
    let creds: Credentials = serde_json::from_str(&std::fs::read_to_string(&path).expect("credentials file")).expect("credentials json");
    // a fresh device id, not the app's player-device-id
    let config = SessionConfig::default();
    let session = Session::new(config, None);
    if let Err(e) = session.connect(creds, false).await {
        eprintln!("session connect failed: {e}");
        std::process::exit(1);
    }
    let user = session.username();
    println!("session up | country {} | client_id {} (librespot default for {})", session.country(), session.client_id(), std::env::consts::OS);
    let base = session.spclient().base_url().await.expect("spclient base url");
    println!("spclient base {base}");
    match session.login5().auth_token().await {
        Ok(t) => println!("login5 token ok (type {}, expires in {}s)", t.token_type, t.expires_in.as_secs()),
        Err(e) => println!("login5 token ERR {e}"),
    }
    match session.spclient().client_token().await {
        Ok(_) => println!("client token ok"),
        Err(e) => println!("client token ERR {e}"),
    }
    let hashes: BTreeMap<String, String> = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/hashes.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<BTreeMap<String, Value>>(&s).ok())
        .map(|m| m.into_iter().filter_map(|(k, v)| v["hash"].as_str().map(|h| (k, h.to_string()))).collect())
        .unwrap_or_default();
    println!("pathfinder hashes loaded: {}", hashes.len());
    let api = Api { session, http: reqwest::Client::new(), base, user, hashes };

    if want("search") { search(&api).await; }
    let pls = if want("rootlist") || want("playlist") { rootlist(&api).await } else { vec![] };
    if want("playlist") { playlist(&api, &pls).await; }
    if want("collection") { liked(&api).await; }
    if want("artist") { artist(&api).await; }
    if want("recent") { recent(&api).await; }
    let tracks = if want("album") || want("save") { album(&api).await } else { vec![] };
    if want("save") { save(&api, &tracks).await; }
    api.session.shutdown();
}
