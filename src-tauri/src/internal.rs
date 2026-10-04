//! Spotify's internal endpoints, reached with the in-app player's own librespot Session
//! (login5 Bearer token + client-token), so the app's rate-limited Web API client id is not used.
//! See spikes/internal-api/REPORT.md for what was probed.

use std::sync::OnceLock;

use librespot_core::Session;
use serde_json::{json, Value};

pub const PATHFINDER: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const LOG: &str = "stylus::internal";

static ENGINE: OnceLock<crate::player::Engine> = OnceLock::new();

/// The engine whose session serves internal calls. Set once, in `run()`.
pub fn attach(engine: crate::player::Engine) {
    let _ = ENGINE.set(engine);
}

/// One caller of the internal endpoints: a live Session and an HTTP client.
#[derive(Clone)]
pub struct Api {
    pub session: Session,
    http: reqwest::Client,
}

/// `ENGINE_NOT_READY: …`: the internal endpoints need the player's session.
pub fn not_ready(why: &str) -> String {
    format!("ENGINE_NOT_READY: {why}")
}

impl Api {
    pub fn new(session: Session) -> Api {
        Api { session, http: crate::auth::http() }
    }

    /// The engine's live session, or `ENGINE_NOT_READY`.
    pub fn current() -> Result<Api, String> {
        let engine = ENGINE.get().ok_or_else(|| not_ready("no engine"))?;
        engine.live_session().map(Api::new).ok_or_else(|| not_ready("the player isn't connected"))
    }

    pub fn username(&self) -> String {
        self.session.username()
    }

    /// One request with the session's tokens. Ok = the body of a 2xx answer; Err names the
    /// HTTP status and the start of the body. Tokens are never logged.
    pub async fn send(&self, method: reqwest::Method, url: &str, ctype: Option<&str>, accept: Option<&str>, body: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
        let token = self.session.login5().auth_token().await.map_err(|e| format!("login5 token: {e}"))?;
        let mut rb = self
            .http
            .request(method, url)
            .header("authorization", format!("Bearer {}", token.access_token))
            .header("app-platform", "OSX")
            .header("spotify-app-version", "1.2.52.442");
        match self.session.spclient().client_token().await {
            Ok(ct) => rb = rb.header("client-token", ct),
            Err(e) => log::debug!(target: LOG, "no client token: {e}"),
        }
        if let Some(c) = ctype {
            rb = rb.header("content-type", c);
        }
        if let Some(a) = accept {
            rb = rb.header("accept", a);
        }
        rb = match body {
            Some(b) => rb.body(b),
            None => rb.header("content-length", "0"),
        };
        let resp = rb.send().await.map_err(|e| format!("transport: {e}"))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| format!("transport: {e}"))?.to_vec();
        if !status.is_success() {
            return Err(http_error(status.as_u16(), &bytes));
        }
        Ok(bytes)
    }

    /// GET/POST on the session's spclient host (`path` starts with `/`).
    pub async fn spclient(&self, method: reqwest::Method, path: &str, ctype: Option<&str>, accept: Option<&str>, body: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
        let base = self.session.spclient().base_url().await.map_err(|e| format!("spclient base url: {e}"))?;
        self.send(method, &format!("{base}{path}"), ctype, accept, body).await
    }

    /// One persisted GraphQL query: its `data`, or an error naming the GraphQL errors.
    pub async fn pathfinder(&self, op: &str, variables: Value) -> Result<Value, String> {
        let hash = crate::hashes::get(op).ok_or_else(|| format!("no pathfinder hash for {op}"))?;
        let body = pathfinder_body(op, &hash, variables);
        let bytes = self
            .send(reqwest::Method::POST, PATHFINDER, Some("application/json;charset=UTF-8"), Some("application/json"), Some(body.to_string().into_bytes()))
            .await?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("pathfinder {op}: bad JSON: {e}"))?;
        graphql_data(v).map_err(|e| format!("pathfinder {op}: {e}"))
    }
}

pub fn pathfinder_body(op: &str, hash: &str, variables: Value) -> Value {
    json!({
        "variables": variables,
        "operationName": op,
        "extensions": {"persistedQuery": {"version": 1, "sha256Hash": hash}},
    })
}

/// `HTTP <status>: <first 200 chars of the body>`.
pub fn http_error(status: u16, body: &[u8]) -> String {
    let text: String = String::from_utf8_lossy(body).chars().take(200).collect::<String>().replace('\n', " ");
    format!("HTTP {status}: {text}")
}

/// A GraphQL answer's `data`. Errors without data fail; errors next to data are partial
/// answers and pass (the missing parts read as null).
pub fn graphql_data(mut v: Value) -> Result<Value, String> {
    let data = v["data"].take();
    if data.is_null() || data.as_object().is_some_and(|o| o.values().all(Value::is_null)) {
        let errs = v["errors"]
            .as_array()
            .map(|a| a.iter().filter_map(|e| e["message"].as_str()).collect::<Vec<_>>().join("; "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "no data".into());
        return Err(format!("GraphQL: {errs}"));
    }
    Ok(data)
}

// ---- the fallback chain ---------------------------------------------------------------

/// Which source served (or failed) a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// The recommended internal endpoint (REPORT.md).
    Primary,
    /// The internal protobuf path (spclient / extended metadata), or another internal fallback.
    Fallback,
    /// The public Web API (spotify.rs), quota-guarded.
    Web,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Source::Primary => "internal",
            Source::Fallback => "internal fallback",
            Source::Web => "Web API",
        })
    }
}

/// One step of a chain: a source and its (lazy) request.
pub type Attempt<'a, T> = (Source, futures_util::future::BoxFuture<'a, Result<T, String>>);

/// The error a failed chain returns. The Web API's error, unless it only says the app is
/// rate-limited while an internal source failed for a real reason: then that reason, which says
/// more. With no Web API step, the first real internal error.
pub fn final_error(errs: &[(Source, String)]) -> String {
    let real_internal = errs.iter().find(|(s, e)| *s != Source::Web && !e.starts_with("ENGINE_NOT_READY")).map(|(_, e)| e);
    match errs.iter().rev().find(|(s, _)| *s == Source::Web) {
        Some((_, w)) if w.starts_with("RATE_LIMITED") => real_internal.unwrap_or(w).clone(),
        Some((_, w)) => w.clone(),
        None => real_internal.or(errs.first().map(|(_, e)| e)).cloned().unwrap_or_else(|| "no source".into()),
    }
}

/// Lets one warning per key through per window (`WARN_EVERY` by default).
#[derive(Debug)]
pub struct WarnGate {
    seen: std::collections::HashMap<String, std::time::Instant>,
    every: std::time::Duration,
}

pub const WARN_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

impl Default for WarnGate {
    fn default() -> Self {
        WarnGate::every(WARN_EVERY)
    }
}

impl WarnGate {
    pub fn every(every: std::time::Duration) -> Self {
        WarnGate { seen: std::collections::HashMap::new(), every }
    }

    pub fn allow(&mut self, key: &str, now: std::time::Instant) -> bool {
        match self.seen.get(key) {
            Some(t) if now.saturating_duration_since(*t) < self.every => false,
            _ => {
                self.seen.insert(key.to_string(), now);
                true
            }
        }
    }
}

fn warn_failure(op: &str, src: Source, err: &str) {
    static GATE: std::sync::Mutex<Option<WarnGate>> = std::sync::Mutex::new(None);
    // the player not being up yet is normal at launch: no warning
    if src == Source::Web || err.starts_with("ENGINE_NOT_READY") {
        log::debug!(target: LOG, "{op}: {src} failed: {err}");
        return;
    }
    let allowed = GATE.lock().map(|mut g| g.get_or_insert_with(WarnGate::default).allow(&format!("{op}/{src}"), std::time::Instant::now())).unwrap_or(true);
    if allowed {
        log::warn!(target: LOG, "{op}: {src} failed, falling back: {err}");
    }
}

/// Runs the steps in order until one succeeds. Logs which source served (debug) and each
/// internal failure (warn, once per op and source per 10 min).
pub async fn serve<T>(op: &str, steps: Vec<Attempt<'_, T>>) -> Result<T, String> {
    let mut errs = Vec::new();
    for (src, attempt) in steps {
        match attempt.await {
            Ok(v) => {
                log::debug!(target: LOG, "{op}: served by {src}");
                return Ok(v);
            }
            Err(e) => {
                warn_failure(op, src, &e);
                errs.push((src, e));
            }
        }
    }
    Err(final_error(&errs))
}

/// A step that runs `f` with the engine's live session, or fails with ENGINE_NOT_READY.
pub fn with_api<'a, T, F, Fut>(src: Source, f: F) -> Attempt<'a, T>
where
    F: FnOnce(Api) -> Fut + Send + 'a,
    Fut: std::future::Future<Output = Result<T, String>> + Send + 'a,
    T: 'a,
{
    use futures_util::FutureExt;
    (src, async move { f(Api::current()?).await }.boxed())
}

/// A Web API step.
pub fn web<'a, T, Fut>(fut: Fut) -> Attempt<'a, T>
where
    Fut: std::future::Future<Output = Result<T, String>> + Send + 'a,
{
    use futures_util::FutureExt;
    (Source::Web, fut.boxed())
}

// ---- operations ----------------------------------------------------------------------------

use crate::spotify::PAGE_CONCURRENCY;
/// Tracks per extended-metadata request.
const META_BATCH: usize = 100;
const COLLECTION_CT: &str = "application/vnd.collection-v2.spotify.proto";
const PROTOBUF_CT: &str = "application/x-protobuf";
/// Search pages: 10, as the UI's paging expects (the Web API's cap).
pub const SEARCH_PAGE: u32 = 10;
/// libraryV3 page size.
const LIBRARY_PAGE: usize = 50;

fn lib_vars(filter: &str, offset: usize, limit: usize) -> Value {
    json!({"filters": [filter], "order": null, "textFilter": "", "features": ["LIKED_SONGS", "YOUR_EPISODES"], "limit": limit, "offset": offset,
           "flatten": true, "expandedFolders": [], "folderUri": null, "includeFoldersWhenFlattening": false})
}

fn search_vars(query: &str, offset: u32, limit: u32) -> Value {
    json!({"searchTerm": query, "offset": offset, "limit": limit, "numberOfTopResults": 5, "includeAudiobooks": false,
           "includeArtistHasConcertsField": false, "includePreReleases": false, "includeLocalConcertsField": false, "includeAuthors": false})
}

/// `{total, items}` for spotify.rs's offset pager.
fn page_value(items: Vec<Value>, total: u64) -> Value {
    json!({ "total": total, "items": items })
}

/// Every item of an offset-paged list up to `max`: `fetch(offset)` returns `{total, items}`.
async fn all_pages<F, Fut>(page: usize, max: usize, fetch: F) -> Result<(Vec<Value>, u64), String>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let first = fetch(0).await?;
    let total = first["total"].as_u64().unwrap_or(0);
    let items = crate::spotify::pages_with(first, page, max, PAGE_CONCURRENCY, fetch).await?;
    Ok((items, total))
}

impl Api {
    async fn post_pb(&self, path: &str, body: Vec<u8>) -> Result<Vec<u8>, String> {
        // collection v2 refuses plain application/x-protobuf with a bare 400 (REPORT.md)
        let ct = if path.starts_with("/collection/v2/") { COLLECTION_CT } else { PROTOBUF_CT };
        self.spclient(reqwest::Method::POST, path, Some(ct), Some(ct), Some(body)).await
    }

    /// Extended metadata of one kind for `uris`: (uri, payload), unordered.
    pub async fn ext(&self, uris: &[String], kind: librespot_protocol::extension_kind::ExtensionKind) -> Result<Vec<(String, Vec<u8>)>, String> {
        use futures_util::stream::{self, StreamExt, TryStreamExt};
        let chunks: Vec<Vec<String>> = uris.chunks(META_BATCH).map(<[String]>::to_vec).collect();
        let parts: Vec<Vec<(String, Vec<u8>)>> = stream::iter(chunks)
            .map(|c| async move { crate::pb::ext_response(&self.post_pb("/extended-metadata/v0/extended-metadata", crate::pb::ext_request(&c, kind)).await?) })
            .buffered(PAGE_CONCURRENCY)
            .try_collect()
            .await?;
        Ok(parts.into_iter().flatten().collect())
    }

    /// The UI's Tracks for `uris`, in order (TRACK_V4); uris without metadata are skipped.
    pub async fn tracks(&self, uris: &[String]) -> Result<Vec<Value>, String> {
        if uris.is_empty() {
            return Ok(vec![]);
        }
        let payloads = self.ext(uris, librespot_protocol::extension_kind::ExtensionKind::TRACK_V4).await?;
        Ok(crate::pb::tracks_in_order(uris, payloads))
    }

    // ---- search

    pub async fn search(&self, query: &str) -> Result<Value, String> {
        Ok(crate::parse::search(&self.pathfinder("searchDesktop", search_vars(query, 0, SEARCH_PAGE)).await?))
    }

    /// Fallback: `context-resolve` of a search uri gives its top tracks (no albums).
    pub async fn search_context(&self, query: &str) -> Result<Value, String> {
        let ctx = self.session.spclient().get_context(&format!("spotify:search:{}", crate::auth::urlencode(query))).await.map_err(|e| format!("context-resolve: {e}"))?;
        let uris: Vec<String> = ctx.pages.iter().flat_map(|p| p.tracks.iter()).map(|t| t.uri().to_string()).filter(|u| u.starts_with("spotify:track:")).take(SEARCH_PAGE as usize).collect();
        Ok(json!({ "tracks": self.tracks(&uris).await?, "albums": [] }))
    }

    /// One page of one kind: (items, raw count, total).
    pub async fn search_page(&self, query: &str, kind: &str, offset: u32) -> Result<(Vec<Value>, usize, u64), String> {
        let op = if kind == "track" { "searchTracks" } else { "searchAlbums" };
        let mut vars = search_vars(query, offset, SEARCH_PAGE);
        vars["numberOfTopResults"] = json!(20);
        Ok(crate::parse::search_page(&self.pathfinder(op, vars).await?, kind))
    }

    // ---- library

    /// A whole libraryV3 list (`filter` "Playlists" | "Albums" | "Artists"), up to `max` items.
    async fn library(&self, filter: &str, max: usize, parse: fn(&Value) -> Vec<Value>) -> Result<Vec<Value>, String> {
        let (items, _) = all_pages(LIBRARY_PAGE, max, |offset| async move {
            let data = self.pathfinder("libraryV3", lib_vars(filter, offset, LIBRARY_PAGE)).await?;
            // the page's raw item count drives the paging; parse drops folders and pseudo playlists
            Ok(page_value(parse(&data), crate::parse::library_total(&data)))
        })
        .await?;
        Ok(items)
    }

    pub async fn rootlist(&self) -> Result<Vec<crate::pb::RootEntry>, String> {
        let user = self.username();
        let path = format!("/playlist/v2/user/{}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from=0&length=1000", crate::auth::urlencode(&user));
        crate::pb::rootlist(&self.spclient(reqwest::Method::GET, &path, None, None, None).await?)
    }

    /// The playlists: libraryV3 (covers, names) with the rootlist's track counts and order.
    /// The rootlist is best effort: without it the counts are 0, in libraryV3's order.
    pub async fn playlists(&self) -> Result<Vec<Value>, String> {
        let (lib, root) = futures_util::join!(self.library("Playlists", 1000, crate::parse::library_playlists), self.rootlist());
        let root = root.unwrap_or_else(|e| {
            warn_failure("get_playlists rootlist", Source::Fallback, &e);
            vec![]
        });
        Ok(merge_playlists(lib?, &root))
    }

    pub async fn saved_albums(&self, max: usize) -> Result<Vec<Value>, String> {
        self.library("Albums", max, crate::parse::library_albums).await
    }

    pub async fn followed_artists(&self, max: usize) -> Result<Vec<Value>, String> {
        self.library("Artists", max, crate::parse::library_artists).await
    }

    /// Fallback: collection set "artist" + ARTIST_V4 for names and photos.
    pub async fn followed_artists_pb(&self, max: usize) -> Result<Vec<Value>, String> {
        let uris: Vec<String> = crate::pb::liked_uris_of(self.collection("artist", max).await?, "spotify:artist:");
        let payloads = self.ext(&uris, librespot_protocol::extension_kind::ExtensionKind::ARTIST_V4).await?;
        let by: std::collections::HashMap<String, Vec<u8>> = payloads.into_iter().collect();
        Ok(uris.iter().filter_map(|u| by.get(u).and_then(|b| crate::pb::artist(u, b, "")).map(|(info, _, _)| info)).collect())
    }

    // ---- playlists

    /// One fetchPlaylist page: `{total, items}` plus the revision (snapshot id).
    pub async fn playlist_page(&self, playlist_id: &str, offset: usize, limit: usize) -> Result<(Value, Option<String>), String> {
        let data = self.pathfinder("fetchPlaylist", json!({"uri": format!("spotify:playlist:{playlist_id}"), "offset": offset, "limit": limit, "enableWatchFeedEntrypoint": false})).await?;
        let (tracks, total, rev) = crate::parse::playlist_page(&data);
        Ok((page_value(tracks, total), rev))
    }

    /// The rest of a playlist after its first page.
    pub async fn playlist_rest(&self, playlist_id: &str, first: Value, page: usize) -> Result<Vec<Value>, String> {
        crate::spotify::pages_with(first, page, usize::MAX, PAGE_CONCURRENCY, |offset| async move { Ok(self.playlist_page(playlist_id, offset, page).await?.0) }).await
    }

    /// A playlist's details (`parse::playlist_meta`): a pasted link, a mix's name and cover.
    pub async fn playlist_meta(&self, playlist_id: &str) -> Result<Value, String> {
        let data = self.pathfinder("fetchPlaylist", json!({"uri": format!("spotify:playlist:{playlist_id}"), "offset": 0, "limit": 1, "enableWatchFeedEntrypoint": false})).await?;
        crate::parse::playlist_meta(&data).ok_or_else(|| "Spotify didn't return this playlist".into())
    }

    /// A pasted album link's details (`parse::album_meta`).
    pub async fn album_meta(&self, album_id: &str) -> Result<Value, String> {
        let data = self.pathfinder("getAlbum", json!({"uri": format!("spotify:album:{album_id}"), "locale": "", "offset": 0, "limit": 1})).await?;
        crate::parse::album_meta(&data).ok_or_else(|| "Spotify didn't return this album".into())
    }

    /// The Made For You mixes on the user's home feed (`parse::home_mixes`).
    pub async fn home_mixes(&self) -> Result<Vec<Value>, String> {
        let vars = json!({"timeZone": "UTC", "sp_t": "", "facet": "", "sectionItemsLimit": 20, "homeEndUserIntegration": "INTEGRATION_WEB_PLAYER"});
        Ok(crate::parse::home_mixes(&self.pathfinder("home", vars).await?))
    }

    /// One `playlist/v2` page (protobuf).
    async fn playlist_pb_page(&self, playlist_id: &str, from: usize, length: usize) -> Result<crate::pb::PlaylistPage, String> {
        let path = format!("/playlist/v2/playlist/{}?from={from}&length={length}", crate::auth::urlencode(playlist_id));
        crate::pb::playlist(&self.spclient(reqwest::Method::GET, &path, None, None, None).await?)
    }

    /// Fallback: `{name, cover}` from playlist/v2.
    pub async fn playlist_info_pb(&self, playlist_id: &str) -> Result<Value, String> {
        let p = self.playlist_pb_page(playlist_id, 0, 1).await?;
        if p.name.is_empty() {
            return Err("playlist/v2: no name".into());
        }
        Ok(json!({ "name": p.name, "cover": p.cover }))
    }

    /// Fallback: a whole playlist from playlist/v2 (uris) + TRACK_V4, and its snapshot id.
    pub async fn playlist_pb(&self, playlist_id: &str) -> Result<(Vec<Value>, Option<String>), String> {
        const PAGE: usize = 100;
        let first = self.playlist_pb_page(playlist_id, 0, PAGE).await?;
        let snapshot = first.snapshot_id.clone();
        let total = first.length.max(0) as u64;
        let first = page_value(first.uris.into_iter().map(Value::from).collect(), total);
        let uris: Vec<String> = crate::spotify::pages_with(first, PAGE, usize::MAX, PAGE_CONCURRENCY, |from| async move {
            let p = self.playlist_pb_page(playlist_id, from, PAGE).await?;
            Ok(page_value(p.uris.into_iter().map(Value::from).collect(), total))
        })
        .await?
        .iter()
        .filter_map(|u| u.as_str().map(str::to_string))
        .collect();
        Ok((self.tracks(&uris).await?, snapshot))
    }

    // ---- Liked Songs

    fn collection_uri(&self) -> String {
        format!("spotify:user:{}:collection", self.username())
    }

    /// Liked Songs newest first, up to `max`, and the full count.
    pub async fn liked(&self, max: usize) -> Result<(Vec<Value>, u64), String> {
        const PAGE: usize = 100;
        let uri = self.collection_uri();
        let uri = &uri;
        all_pages(PAGE, max, |offset| async move {
            let data = self.pathfinder("fetchLibraryTracks", json!({"uri": uri, "offset": offset, "limit": PAGE})).await?;
            let (tracks, total) = crate::parse::liked_page(&data);
            Ok(page_value(tracks, total))
        })
        .await
    }

    pub async fn liked_count(&self) -> Result<u64, String> {
        let data = self.pathfinder("fetchLibraryTracks", json!({"uri": self.collection_uri(), "offset": 0, "limit": 1})).await?;
        Ok(crate::parse::liked_page(&data).1)
    }

    /// Up to `max` items of a collection set ("collection" = Liked Songs + albums, "artist").
    async fn collection(&self, set: &str, max: usize) -> Result<Vec<crate::pb::CollectionItem>, String> {
        let user = self.username();
        let mut token = String::new();
        let mut all = Vec::new();
        loop {
            let body = crate::pb::page_request(&user, set, &token, 300);
            let (items, next) = crate::pb::page_response(&self.post_pb("/collection/v2/paging", body).await?)?;
            all.extend(items);
            if next.is_empty() || next == token || all.len() >= max {
                return Ok(all);
            }
            token = next;
        }
    }

    /// Fallback: Liked Songs from collection paging (unsorted: sorted here) + TRACK_V4.
    pub async fn liked_pb(&self, max: usize) -> Result<(Vec<Value>, u64), String> {
        // the whole set: it isn't sorted, so the newest can be on any page
        let uris = crate::pb::liked_uris(self.collection("collection", 20_000).await?);
        let total = uris.len() as u64;
        let tracks = self.tracks(&uris[..uris.len().min(max)]).await?;
        Ok((tracks, total))
    }

    pub async fn is_saved(&self, track_id: &str) -> Result<bool, String> {
        let data = self.pathfinder("areEntitiesInLibrary", json!({"uris": [format!("spotify:track:{track_id}")]})).await?;
        crate::parse::first_saved(&data).ok_or_else(|| "areEntitiesInLibrary: no answer".into())
    }

    /// Adds (`saved`) or removes a track from Liked Songs. Changes real data.
    pub async fn set_saved(&self, track_id: &str, saved: bool) -> Result<(), String> {
        let item = crate::pb::CollectionItem { uri: format!("spotify:track:{track_id}"), added_at: if saved { crate::auth::now() as i64 } else { 0 }, is_removed: !saved };
        let update_id = format!("{:016x}", rand::random::<u64>());
        self.post_pb("/collection/v2/write", crate::pb::write_request(&self.username(), "collection", &[item], &update_id)).await.map(|_| ())
    }

    // ---- albums

    pub async fn album(&self, album_id: &str) -> Result<Vec<Value>, String> {
        const PAGE: usize = 50;
        let uri = format!("spotify:album:{album_id}");
        let uri = &uri;
        let (tracks, _) = all_pages(PAGE, usize::MAX, |offset| async move {
            let data = self.pathfinder("getAlbum", json!({"uri": uri, "locale": "", "offset": offset, "limit": PAGE})).await?;
            let (tracks, total) = crate::parse::album_tracks(&data);
            Ok(page_value(tracks, total))
        })
        .await?;
        Ok(tracks)
    }

    /// The album-info card (`parse::album_info`), its total length summed over every track page.
    pub async fn album_info(&self, album_id: &str) -> Result<Value, String> {
        const PAGE: u64 = 50;
        let uri = format!("spotify:album:{album_id}");
        let page = |offset: u64| self.pathfinder("getAlbum", json!({"uri": uri, "locale": "", "offset": offset, "limit": PAGE}));
        let first = page(0).await?;
        let mut info = crate::parse::album_info(&first).ok_or("Spotify didn't return this album")?;
        let total = info["total_tracks"].as_u64().unwrap_or(0);
        let mut ms = info["duration_ms"].as_u64().unwrap_or(0);
        let mut offset = PAGE;
        while offset < total {
            ms += crate::parse::album_page_ms(&page(offset).await?);
            offset += PAGE;
        }
        info["duration_ms"] = json!(ms);
        Ok(info)
    }

    /// The id of the album a track is on.
    pub async fn album_id_of_track(&self, track_id: &str) -> Result<String, String> {
        let data = self.pathfinder("fetchEntitiesForRecentlyPlayed", json!({"uris": [format!("spotify:track:{track_id}")]})).await?;
        crate::parse::album_id_of_track(&data).ok_or_else(|| "fetchEntitiesForRecentlyPlayed: no album".into())
    }

    /// Fallback: ALBUM_V4 + TRACK_V4, the album's name and cover stamped on each track.
    pub async fn album_pb(&self, album_id: &str) -> Result<Vec<Value>, String> {
        let uri = format!("spotify:album:{album_id}");
        let payloads = self.ext(std::slice::from_ref(&uri), librespot_protocol::extension_kind::ExtensionKind::ALBUM_V4).await?;
        let (name, cover, uris) = payloads.first().and_then(|(_, b)| crate::pb::album(b)).ok_or("ALBUM_V4: no album")?;
        Ok(self
            .tracks(&uris)
            .await?
            .into_iter()
            .map(|mut t| {
                t["album"] = json!(name);
                t["cover"] = json!(cover);
                t
            })
            .collect())
    }

    // ---- artists

    /// `{id, name, image, top_tracks}`.
    pub async fn artist(&self, artist_id: &str) -> Result<Value, String> {
        let data = self.pathfinder("queryArtistOverview", json!({"uri": format!("spotify:artist:{artist_id}"), "locale": "", "includePrerelease": true})).await?;
        crate::parse::artist_overview(&data).ok_or_else(|| "queryArtistOverview: no artist".into())
    }

    async fn artist_v4(&self, artist_id: &str) -> Result<crate::pb::ArtistMeta, String> {
        let uri = format!("spotify:artist:{artist_id}");
        let payloads = self.ext(std::slice::from_ref(&uri), librespot_protocol::extension_kind::ExtensionKind::ARTIST_V4).await?;
        let country = self.session.country();
        payloads.first().and_then(|(_, b)| crate::pb::artist(&uri, b, &country)).ok_or_else(|| "ARTIST_V4: no artist".into())
    }

    /// Fallback: ARTIST_V4 (+ TRACK_V4 for its top 10).
    pub async fn artist_pb(&self, artist_id: &str) -> Result<Value, String> {
        let (mut info, top, _) = self.artist_v4(artist_id).await?;
        info["top_tracks"] = json!(self.tracks(&top[..top.len().min(10)]).await.unwrap_or_default());
        Ok(info)
    }

    pub async fn artist_albums(&self, artist_id: &str, max: usize) -> Result<Vec<Value>, String> {
        let data = self.pathfinder("queryArtistDiscographyAll", json!({"uri": format!("spotify:artist:{artist_id}"), "offset": 0, "limit": max, "order": "DATE_DESC"})).await?;
        Ok(crate::parse::discography(&data))
    }

    /// Fallback: ARTIST_V4's album and single groups + ALBUM_V4 for names, covers and years.
    pub async fn artist_albums_pb(&self, artist_id: &str, max: usize) -> Result<Vec<Value>, String> {
        let (_, _, mut albums) = self.artist_v4(artist_id).await?;
        albums.truncate(max);
        let uris: Vec<String> = albums.iter().map(|(u, _)| u.clone()).collect();
        let by: std::collections::HashMap<String, Vec<u8>> = self.ext(&uris, librespot_protocol::extension_kind::ExtensionKind::ALBUM_V4).await?.into_iter().collect();
        Ok(albums.iter().filter_map(|(u, kind)| by.get(u).and_then(|b| crate::pb::album_tile(u, b, kind))).collect())
    }

    // ---- taste and history

    /// Top tracks or artists for a Web API time range.
    pub async fn top(&self, kind: &str, range: &str, limit: u8) -> Result<Vec<Value>, String> {
        let range = crate::parse::time_range(range).ok_or_else(|| format!("bad top range: {range}"))?;
        let input = json!({"offset": 0, "limit": limit, "sortBy": "AFFINITY", "timeRange": range});
        let tracks = kind == "tracks";
        let vars = json!({"includeTopArtists": !tracks, "topArtistsInput": input, "includeTopTracks": tracks, "topTracksInput": input});
        Ok(crate::parse::top(&self.pathfinder("userTopContent", vars).await?, kind))
    }

    /// The last played track of each recent context, newest first: `[{track, played_at, context_uri}]`.
    /// Track details from pathfinder's lookup; extended metadata when that fails.
    pub async fn recently_played(&self) -> Result<Vec<Value>, String> {
        let path = format!("/recently-played/v3/user/{}/recently-played?format=json&offset=0&limit=50&filter=default,collection-new-episodes", crate::auth::urlencode(&self.username()));
        let body = self.spclient(reqwest::Method::GET, &path, None, Some("application/json"), None).await?;
        let v: Value = serde_json::from_slice(&body).map_err(|e| format!("recently-played: {e}"))?;
        let contexts = crate::parse::recent_contexts(&v);
        let mut uris: Vec<String> = contexts.iter().map(|(_, t, _)| t.clone()).collect();
        uris.dedup();
        let tracks: std::collections::HashMap<String, Value> = match self.pathfinder("fetchEntitiesForRecentlyPlayed", json!({"uris": uris})).await {
            Ok(data) => crate::parse::lookup_tracks(&data),
            Err(e) => {
                warn_failure("get_recently_played lookup", Source::Primary, &e);
                self.tracks(&uris).await?.into_iter().filter_map(|t| Some((t["uri"].as_str()?.to_string(), t))).collect()
            }
        };
        Ok(crate::parse::recent(&contexts, &tracks))
    }
}

/// The playlists in the rootlist's order (the user's own), with its track counts; covers, names
/// and snapshot ids from libraryV3. Playlists only one side knows are kept: libraryV3-only ones
/// at the end, rootlist-only ones (no cover) in place.
pub fn merge_playlists(lib: Vec<Value>, root: &[crate::pb::RootEntry]) -> Vec<Value> {
    if root.is_empty() {
        return lib;
    }
    let mut by_uri: std::collections::HashMap<String, Value> = lib.iter().filter_map(|p| Some((p["uri"].as_str()?.to_string(), p.clone()))).collect();
    let mut out: Vec<Value> = root
        .iter()
        .map(|e| match by_uri.remove(&e.uri) {
            Some(mut p) => {
                p["tracks"] = json!({ "total": e.length });
                if p["images"].as_array().is_none_or(Vec::is_empty) {
                    p["images"] = crate::pb::root_playlist(e)["images"].clone();
                }
                p
            }
            None => crate::pb::root_playlist(e),
        })
        .collect();
    out.extend(lib.into_iter().filter(|p| p["uri"].as_str().is_some_and(|u| by_uri.contains_key(u))));
    out
}

// ---- Spotify Connect (the player's cluster) --------------------------------------------------

/// The engine's view of Connect: (cluster, this Mac's device id, its volume %), when ready.
fn connect() -> Result<(std::sync::Arc<librespot_protocol::connect::Cluster>, String, u8), String> {
    let engine = ENGINE.get().ok_or_else(|| not_ready("no engine"))?;
    engine.connect_view().ok_or_else(|| not_ready("no Connect cluster yet"))
}

/// The engine, once `run()` attached it.
pub fn engine() -> Option<crate::player::Engine> {
    ENGINE.get().cloned()
}

/// What the Connect cluster's active device plays now (`pb::cluster_state`); Ok(None) when no
/// device is active. Err without a cluster (the player isn't up).
pub fn cluster_state() -> Result<Option<Value>, String> {
    let (cluster, _, _) = connect()?;
    Ok(crate::pb::cluster_state(&cluster, crate::auth::now_ms() as i64))
}

/// This Mac's id while the engine is ready.
pub fn own_device_id() -> Option<String> {
    ENGINE.get()?.live_session().map(|s| s.device_id().to_string())
}

/// `list_devices` from the cluster, with this Mac listed even before Spotify lists it.
pub fn devices() -> Result<Value, String> {
    let (cluster, own, volume) = connect()?;
    Ok(Value::Array(with_own_device(crate::pb::devices(&cluster), &own, &cluster.active_device_id, volume)))
}

/// Last resort while the engine is ready: this Mac alone.
pub fn own_device_only() -> Result<Value, String> {
    let engine = ENGINE.get().ok_or_else(|| not_ready("no engine"))?;
    let own = own_device_id().ok_or_else(|| not_ready("the player isn't connected"))?;
    Ok(Value::Array(with_own_device(vec![], &own, "", engine.volume_percent())))
}

/// `list` plus this Mac when it's missing (Spotify lists a new device a few seconds late).
pub fn with_own_device(mut list: Vec<Value>, own: &str, active: &str, volume: u8) -> Vec<Value> {
    if !own.is_empty() && !list.iter().any(|d| d["id"] == own) {
        list.push(json!({
            "id": own, "name": crate::player::DEVICE_NAME, "type": "Computer", "is_active": active == own,
            "is_private_session": false, "is_restricted": false, "supports_volume": true, "volume_percent": volume,
        }));
    }
    list
}

/// The up-next tracks of the active device from the cluster (`get_queue`'s shape).
pub async fn queue() -> Result<Value, String> {
    let (cluster, _, _) = connect()?;
    if cluster.active_device_id.is_empty() {
        return Err("no active device in the cluster".into());
    }
    let uris = crate::pb::next_track_uris(&cluster, crate::nowplaying::QUEUE_MAX);
    Ok(Value::Array(Api::current()?.tracks(&uris).await?))
}

/// Queues `uri` on this Mac through Spotify Connect (a player command from this device to itself;
/// Spirc handles `add_to_queue`). Other devices: Err, the Web API does those.
pub async fn add_to_queue(device_id: &str, uri: &str) -> Result<(), String> {
    let own = own_device_id().ok_or_else(|| not_ready("the player isn't connected"))?;
    if device_id != own {
        return Err(format!("NOT_THIS_MAC: {device_id} is another device"));
    }
    let api = Api::current()?;
    let body = queue_command(uri);
    let path = format!("/connect-state/v1/player/command/from/{own}/to/{own}");
    api.spclient(reqwest::Method::POST, &path, Some("application/json"), None, Some(body.to_string().into_bytes())).await.map(|_| ())
}

/// The Connect player command that queues one track.
pub fn queue_command(uri: &str) -> Value {
    json!({"command": {"endpoint": "add_to_queue", "track": {"uri": uri, "metadata": {"is_queued": "true"}, "provider": "queue"}}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_error_rules() {
        let e = |s: Source, m: &str| (s, m.to_string());
        // the Web API's own error wins (AUTH_EXPIRED etc. keep their meaning)
        assert_eq!(final_error(&[e(Source::Primary, "HTTP 500: x"), e(Source::Web, "AUTH_EXPIRED: 401")]), "AUTH_EXPIRED: 401");
        // rate-limited Web API: the real internal reason says more
        assert_eq!(final_error(&[e(Source::Primary, "GraphQL: PersistedQueryNotFound"), e(Source::Fallback, "HTTP 404: y"), e(Source::Web, "RATE_LIMITED:9: z")]), "GraphQL: PersistedQueryNotFound");
        // …but a player that isn't up is no reason: the rate limit is the news
        assert_eq!(final_error(&[e(Source::Primary, "ENGINE_NOT_READY: x"), e(Source::Web, "RATE_LIMITED:9: z")]), "RATE_LIMITED:9: z");
        // no Web API step: the first real internal error, else the first
        assert_eq!(final_error(&[e(Source::Primary, "ENGINE_NOT_READY: x"), e(Source::Fallback, "HTTP 403")]), "HTTP 403");
        assert_eq!(final_error(&[e(Source::Primary, "ENGINE_NOT_READY: x")]), "ENGINE_NOT_READY: x");
        assert_eq!(final_error(&[]), "no source");
    }

    #[tokio::test]
    async fn serve_stops_at_the_first_success() {
        use futures_util::FutureExt;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ran = AtomicUsize::new(0);
        let step = |src: Source, r: Result<u8, String>| -> Attempt<'_, u8> {
            let ran = &ran;
            (src, async move {
                ran.fetch_add(1, Ordering::SeqCst);
                r
            }
            .boxed())
        };
        let got = serve("t", vec![step(Source::Primary, Err("HTTP 500".into())), step(Source::Fallback, Ok(2)), step(Source::Web, Ok(3))]).await;
        assert_eq!(got, Ok(2));
        assert_eq!(ran.load(Ordering::SeqCst), 2, "the Web API step never ran");
        let got = serve("t", vec![step(Source::Primary, Err("ENGINE_NOT_READY: x".into())), step(Source::Web, Err("RATE_LIMITED:5: y".into()))]).await;
        assert_eq!(got, Err("RATE_LIMITED:5: y".into()));
    }

    #[tokio::test]
    async fn with_api_without_engine_is_not_ready() {
        let (src, fut) = with_api(Source::Primary, |api: Api| async move { Ok(api.username()) });
        assert_eq!(src, Source::Primary);
        assert!(fut.await.unwrap_err().starts_with("ENGINE_NOT_READY"));
    }

    #[test]
    fn warn_gate_once_per_window() {
        let t0 = std::time::Instant::now();
        let mut g = WarnGate::default();
        assert!(g.allow("search/internal", t0));
        assert!(!g.allow("search/internal", t0 + std::time::Duration::from_secs(599)));
        assert!(g.allow("liked/internal", t0), "per op");
        assert!(g.allow("search/internal", t0 + WARN_EVERY));
        let mut minute = WarnGate::every(std::time::Duration::from_secs(60));
        assert!(minute.allow("bad Host", t0));
        assert!(!minute.allow("bad Host", t0 + std::time::Duration::from_secs(59)));
        assert!(minute.allow("bad Host", t0 + std::time::Duration::from_secs(60)), "a window of its own");
    }

    #[test]
    fn merge_keeps_rootlist_order_and_counts() {
        use crate::pb::RootEntry;
        let lib = vec![
            json!({"id": "b", "uri": "spotify:playlist:b", "name": "B", "images": [{"url": "https://i.scdn.co/b"}], "tracks": {"total": 0}}),
            json!({"id": "a", "uri": "spotify:playlist:a", "name": "A", "images": [], "tracks": {"total": 0}}),
            json!({"id": "n", "uri": "spotify:playlist:n", "name": "New", "images": [], "tracks": {"total": 0}}),
        ];
        let root = |u: &str, n: i64, pic: Option<&str>| RootEntry { uri: format!("spotify:playlist:{u}"), name: u.to_uppercase(), length: n, owner: "me".into(), snapshot_id: None, picture: pic.map(str::to_string) };
        let merged = merge_playlists(lib.clone(), &[root("a", 5, Some("https://mosaic.scdn.co/a")), root("b", 7, None), root("r", 2, None)]);
        let ids: Vec<&str> = merged.iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "r", "n"]);
        assert_eq!(merged[0]["tracks"]["total"], 5);
        assert_eq!(merged[0]["images"][0]["url"], "https://mosaic.scdn.co/a", "rootlist picture fills a missing cover");
        assert_eq!(merged[1]["images"][0]["url"], "https://i.scdn.co/b");
        assert_eq!(merged[2]["name"], "R");
        // no rootlist: libraryV3 as is
        assert_eq!(merge_playlists(lib.clone(), &[]), lib);
    }

    #[test]
    fn own_device_is_added_once() {
        let listed = vec![json!({"id": "mac", "name": "This Mac"})];
        assert_eq!(with_own_device(listed.clone(), "mac", "", 50).len(), 1);
        let l = with_own_device(vec![json!({"id": "phone"})], "mac", "mac", 40);
        assert_eq!(l.len(), 2);
        assert_eq!(l[1]["name"], "This Mac");
        assert_eq!(l[1]["is_active"], true);
        assert_eq!(l[1]["volume_percent"], 40);
        assert!(with_own_device(vec![], "", "", 0).is_empty());
    }

    #[test]
    fn queue_command_shape() {
        let c = queue_command("spotify:track:x");
        assert_eq!(c["command"]["endpoint"], "add_to_queue");
        assert_eq!(c["command"]["track"]["uri"], "spotify:track:x");
    }

    #[test]
    fn graphql_errors() {
        assert_eq!(graphql_data(json!({"data": {"a": 1}})), Ok(json!({"a": 1})));
        assert_eq!(graphql_data(json!({"errors": [{"message": "PersistedQueryNotFound"}]})), Err("GraphQL: PersistedQueryNotFound".into()));
        assert_eq!(graphql_data(json!({"data": {"me": null}, "errors": [{"message": "x"}]})), Err("GraphQL: x".into()));
        assert_eq!(graphql_data(json!({})), Err("GraphQL: no data".into()));
        // partial: errors next to real data pass
        assert_eq!(graphql_data(json!({"data": {"a": 1, "b": null}, "errors": [{"message": "x"}]})), Ok(json!({"a": 1, "b": null})));
        assert_eq!(http_error(429, b"slow\ndown"), "HTTP 429: slow down");
        let b = pathfinder_body("op", "h", json!({"x": 1}));
        assert_eq!(b["extensions"]["persistedQuery"], json!({"version": 1, "sha256Hash": "h"}));
    }
}

/// Live probes against Spotify (ignored by default; never part of a normal test run).
/// `cargo test --lib internal::live -- --ignored --nocapture` with `PROBE=name,name`.
/// Logs in a Session (no Spirc, a fresh device id) with the stored player credentials and
/// writes the raw answers to /tmp/stylus-probe. Prints statuses and sizes, never tokens.
#[cfg(test)]
mod live {
    use super::*;
    use librespot_core::{authentication::Credentials, SessionConfig};

    pub async fn api() -> Api {
        let path = dirs::config_dir().unwrap().join("stylus/player-credentials.json");
        let creds: Credentials = serde_json::from_str(&std::fs::read_to_string(path).expect("credentials")).expect("credentials json");
        let session = Session::new(SessionConfig::default(), None);
        session.connect(creds, false).await.expect("session connect");
        Api::new(session)
    }

    fn want(name: &str) -> bool {
        std::env::var("PROBE").map(|p| p.split(',').any(|x| x == name || x == "all")).unwrap_or(false)
    }

    fn dump(name: &str, r: &Result<Value, String>) {
        let dir = std::path::Path::new("/tmp/stylus-probe");
        std::fs::create_dir_all(dir).unwrap();
        match r {
            Ok(v) => {
                let s = serde_json::to_string_pretty(v).unwrap();
                println!("{name}: OK {} bytes", s.len());
                std::fs::write(dir.join(format!("{name}.json")), s).unwrap();
            }
            Err(e) => println!("{name}: ERR {e}"),
        }
    }

    fn dump_raw(name: &str, r: &Result<Vec<u8>, String>) {
        let dir = std::path::Path::new("/tmp/stylus-probe");
        std::fs::create_dir_all(dir).unwrap();
        match r {
            Ok(b) => {
                println!("{name}: OK {} bytes", b.len());
                std::fs::write(dir.join(format!("{name}.bin")), b).unwrap();
            }
            Err(e) => println!("{name}: ERR {e}"),
        }
    }

    const ARTIST: &str = "spotify:artist:4Z8W4fKeB5YxbusRsdQVPb";
    const ALBUM: &str = "spotify:album:6dVIqQ8qmQ5GBnJ9shOYGE";
    const EDITORIAL: &str = "spotify:playlist:37i9dQZF1DXcBWIGoYBM5M";

    fn lib_vars(filter: &str, limit: u32) -> Value {
        json!({"filters": [filter], "order": null, "textFilter": "", "features": ["LIKED_SONGS", "YOUR_EPISODES"], "limit": limit, "offset": 0, "flatten": false, "expandedFolders": [], "folderUri": null, "includeFoldersWhenFlattening": true})
    }

    fn show(name: &str, r: Result<Value, String>) {
        match r {
            Ok(v) => {
                let n = v.as_array().map(Vec::len).or_else(|| v["tracks"].as_array().map(Vec::len));
                let first = v.as_array().and_then(|a| a.first()).cloned().unwrap_or(v.clone());
                let s: String = first.to_string().chars().take(260).collect();
                println!("{name}: OK n={n:?} | {s}");
            }
            Err(e) => println!("{name}: ERR {e}"),
        }
    }

    /// The real operations (what the commands call), one call each.
    #[tokio::test]
    #[ignore]
    async fn probe_ops() {
        let api = api().await;
        let arr = |r: Result<Vec<Value>, String>| r.map(Value::Array);
        if want("ops") {
            show("search", api.search("radiohead").await);
            show("search_context", api.search_context("radiohead").await);
            show("search_page track", api.search_page("radiohead", "track", 10).await.map(|(i, g, t)| json!({"n": i.len(), "got": g, "total": t, "first": i.first()})));
            show("search_page album", api.search_page("radiohead", "album", 0).await.map(|(i, g, t)| json!({"n": i.len(), "got": g, "total": t, "first": i.first()})));
            show("playlists", arr(api.playlists().await));
            show("rootlist", api.rootlist().await.map(|r| Value::Array(r.iter().map(crate::pb::root_playlist).collect())));
            show("saved_albums", arr(api.saved_albums(200).await));
            show("followed", arr(api.followed_artists(200).await));
            show("followed_pb", arr(api.followed_artists_pb(200).await));
            let ed = "37i9dQZF1DXcBWIGoYBM5M";
            show("playlist_page", api.playlist_page(ed, 0, 100).await.map(|(v, rev)| json!({"total": v["total"], "n": v["items"].as_array().map(Vec::len), "rev": rev})));
            show("playlist_pb", api.playlist_pb(ed).await.map(|(t, rev)| json!({"n": t.len(), "rev": rev, "first": t.first()})));
            show("playlist_meta", api.playlist_meta("37i9dQZF1E4yLltmVk3nyb").await);
            show("playlist_info_pb", api.playlist_info_pb("37i9dQZF1E4yLltmVk3nyb").await);
            show("liked", api.liked(1000).await.map(|(t, total)| json!({"n": t.len(), "total": total, "first": t.first()})));
            show("liked_pb", api.liked_pb(1000).await.map(|(t, total)| json!({"n": t.len(), "total": total, "first": t.first()})));
            show("liked_count", api.liked_count().await.map(Value::from));
            show("album", arr(api.album("6dVIqQ8qmQ5GBnJ9shOYGE").await));
            show("album_pb", arr(api.album_pb("6dVIqQ8qmQ5GBnJ9shOYGE").await));
            show("artist", api.artist("4Z8W4fKeB5YxbusRsdQVPb").await.map(|mut a| { let n = a["top_tracks"].as_array().map(Vec::len); a["top_tracks"] = json!(n); a }));
            show("artist_pb", api.artist_pb("4Z8W4fKeB5YxbusRsdQVPb").await.map(|mut a| { let n = a["top_tracks"].as_array().map(Vec::len); a["top_tracks"] = json!(n); a }));
            show("artist_albums", arr(api.artist_albums("4Z8W4fKeB5YxbusRsdQVPb", 50).await));
            show("artist_albums_pb", arr(api.artist_albums_pb("4Z8W4fKeB5YxbusRsdQVPb", 50).await));
            for r in ["short_term", "medium_term", "long_term"] {
                show(&format!("top tracks {r}"), arr(api.top("tracks", r, 50).await));
                show(&format!("top artists {r}"), arr(api.top("artists", r, 20).await));
            }
            show("recently_played", arr(api.recently_played().await));
            show("is_saved", api.is_saved("70LcF31zb1H0PyJoS1Sx1r").await.map(Value::from));
        }
        if want("midterm") {
            for range in ["MID_TERM", "MEDIUM"] {
                let v = json!({"includeTopArtists": false, "topArtistsInput": {"offset": 0, "limit": 1, "sortBy": "AFFINITY", "timeRange": range}, "includeTopTracks": true, "topTracksInput": {"offset": 0, "limit": 1, "sortBy": "AFFINITY", "timeRange": range}});
                show(range, api.pathfinder("userTopContent", v).await.map(|d| json!(crate::parse::top(&d, "tracks").len())));
            }
        }
        if want("queuecmd") {
            // a queue command to a device id that doesn't exist: shows whether the endpoint and body are
            // accepted without touching any real device's queue
            let from = api.session.device_id().to_string();
            let path = format!("/connect-state/v1/player/command/from/{from}/to/0000000000000000000000000000000000000000");
            let r = api.spclient(reqwest::Method::POST, &path, Some("application/json"), None, Some(queue_command("spotify:track:7c378mlmubSu7NGkLFa4sN").to_string().into_bytes())).await;
            println!("queue command to a missing device: {:?}", r.map(|b| String::from_utf8_lossy(&b).chars().take(200).collect::<String>()));
        }
        if want("save") {
            // reversible: only a track that isn't liked; add, check, remove, check
            let id = "7c378mlmubSu7NGkLFa4sN"; // Radiohead - Airbag
            let before = api.is_saved(id).await;
            println!("is_saved before: {before:?}");
            if before == Ok(false) {
                println!("save: {:?}", api.set_saved(id, true).await);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                println!("is_saved after save: {:?}", api.is_saved(id).await);
                println!("unsave: {:?}", api.set_saved(id, false).await);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                println!("is_saved after unsave: {:?}", api.is_saved(id).await);
            }
        }
        api.session.shutdown();
    }

    #[tokio::test]
    #[ignore]
    async fn probe_raw() {
        let api = api().await;
        println!("session up");
        let pf = |op: &'static str, v: Value| {
            let api = api.clone();
            async move { api.pathfinder(op, v).await }
        };
        if want("search") {
            dump("searchDesktop", &pf("searchDesktop", json!({"searchTerm": "radiohead", "offset": 0, "limit": 10, "numberOfTopResults": 5, "includeAudiobooks": false, "includeArtistHasConcertsField": false, "includePreReleases": false, "includeLocalConcertsField": false, "includeAuthors": false})).await);
            dump("searchTracks", &pf("searchTracks", json!({"searchTerm": "radiohead", "offset": 10, "limit": 10, "numberOfTopResults": 20, "includeAudiobooks": false, "includePreReleases": false, "includeAuthors": false})).await);
            dump("searchAlbums", &pf("searchAlbums", json!({"searchTerm": "radiohead", "offset": 0, "limit": 10, "numberOfTopResults": 20, "includeAudiobooks": false, "includePreReleases": false, "includeAuthors": false})).await);
        }
        if want("library") {
            dump("libraryPlaylists", &pf("libraryV3", lib_vars("Playlists", 50)).await);
            dump("libraryAlbums", &pf("libraryV3", lib_vars("Albums", 50)).await);
            dump("libraryArtists", &pf("libraryV3", lib_vars("Artists", 50)).await);
        }
        if want("playlist") {
            dump("fetchPlaylist", &pf("fetchPlaylist", json!({"uri": EDITORIAL, "offset": 0, "limit": 5, "enableWatchFeedEntrypoint": false})).await);
        }
        if want("liked") {
            let user = api.username();
            dump("fetchLibraryTracks", &pf("fetchLibraryTracks", json!({"uri": format!("spotify:user:{user}:collection"), "offset": 0, "limit": 5})).await);
        }
        if want("album") {
            dump("getAlbum", &pf("getAlbum", json!({"uri": ALBUM, "locale": "", "offset": 0, "limit": 50})).await);
        }
        if want("albuminfo") {
            // the album-info card: a 3-track page for the fixture, then the real ops
            dump("getAlbumInfo", &pf("getAlbum", json!({"uri": ALBUM, "locale": "", "offset": 0, "limit": 3})).await);
            dump("album_info", &api.album_info("6dVIqQ8qmQ5GBnJ9shOYGE").await);
            dump("album_id_of_track", &api.album_id_of_track("70LcF31zb1H0PyJoS1Sx1r").await.map(Value::String));
        }
        if want("artist") {
            dump("queryArtistOverview", &pf("queryArtistOverview", json!({"uri": ARTIST, "locale": "", "includePrerelease": true})).await);
            dump("queryArtistDiscographyAll", &pf("queryArtistDiscographyAll", json!({"uri": ARTIST, "offset": 0, "limit": 50, "order": "DATE_DESC"})).await);
        }
        if want("recent") {
            let user = api.username();
            let r = api.spclient(reqwest::Method::GET, &format!("/recently-played/v3/user/{user}/recently-played?format=json&offset=0&limit=50&filter=default,collection-new-episodes"), None, Some("application/json"), None).await;
            let v = r.and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()));
            dump("recentlyPlayed", &v);
            if let Ok(v) = &v {
                let uris: Vec<Value> = v["playContexts"].as_array().into_iter().flatten().take(4).map(|c| c["uri"].clone()).collect();
                dump("fetchEntitiesForRecentlyPlayed", &pf("fetchEntitiesForRecentlyPlayed", json!({"uris": uris})).await);
            }
        }
        if want("contains") {
            dump("areEntitiesInLibrary", &pf("areEntitiesInLibrary", json!({"uris": ["spotify:track:6LgJvl0Xdtc73RJ1mmpotq", "spotify:album:6dVIqQ8qmQ5GBnJ9shOYGE"]})).await);
        }
        if want("top") {
            for range in ["SHORT_TERM"] {
                dump(&format!("userTopContent_{range}"), &pf("userTopContent", json!({"includeTopArtists": true, "topArtistsInput": {"offset": 0, "limit": 10, "sortBy": "AFFINITY", "timeRange": range}, "includeTopTracks": true, "topTracksInput": {"offset": 0, "limit": 10, "sortBy": "AFFINITY", "timeRange": range}})).await);
            }
        }
        if want("pb") {
            let user = api.username();
            dump_raw("rootlist", &api.spclient(reqwest::Method::GET, &format!("/playlist/v2/user/{user}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from=0&length=120"), None, None, None).await);
            dump_raw("playlistV2", &api.spclient(reqwest::Method::GET, "/playlist/v2/playlist/37i9dQZF1DXcBWIGoYBM5M?from=0&length=5", None, None, None).await);
        }
        if want("pb2") {
            let user = api.username();
            let pb = |path: &'static str, body: Vec<u8>, ct: &'static str| {
                let api = api.clone();
                async move { api.spclient(reqwest::Method::POST, path, Some(ct), Some(ct), Some(body)).await }
            };
            const COLL: &str = "application/vnd.collection-v2.spotify.proto";
            const XPB: &str = "application/x-protobuf";
            dump_raw("collectionPage", &pb("/collection/v2/paging", crate::pb::page_request(&user, "collection", "", 3), COLL).await);
            dump_raw("collectionArtists", &pb("/collection/v2/paging", crate::pb::page_request(&user, "artist", "", 3), COLL).await);
            use librespot_protocol::extension_kind::ExtensionKind as K;
            let uris = vec!["spotify:track:70LcF31zb1H0PyJoS1Sx1r".to_string(), "spotify:track:7c378mlmubSu7NGkLFa4sN".to_string()];
            dump_raw("extTracks", &pb("/extended-metadata/v0/extended-metadata", crate::pb::ext_request(&uris, K::TRACK_V4), XPB).await);
            dump_raw("extAlbum", &pb("/extended-metadata/v0/extended-metadata", crate::pb::ext_request(&[ALBUM.to_string()], K::ALBUM_V4), XPB).await);
            dump_raw("extArtist", &pb("/extended-metadata/v0/extended-metadata", crate::pb::ext_request(&[ARTIST.to_string()], K::ARTIST_V4), XPB).await);
        }
        if want("home") {
            // the mixes: the home feed (Made For You shelves) and the two share-link test playlists
            show("home_mixes", api.home_mixes().await.map(Value::Array));
            for id in ["37i9dQZEVXcVV9hd3iqSgp", "37i9dQZF1E4qxgJU46pFLr"] {
                show(&format!("playlist_meta {id}"), api.playlist_meta(id).await);
                show(&format!("playlist_page {id}"), api.playlist_page(id, 0, 3).await.map(|(v, rev)| json!({"total": v["total"], "n": v["items"].as_array().map(Vec::len), "rev": rev})));
            }
        }
        if want("vars") {
            let mut v = lib_vars("Playlists", 50);
            v["flatten"] = json!(true);
            v["includeFoldersWhenFlattening"] = json!(false);
            dump("libraryPlaylistsFlat", &pf("libraryV3", v).await);
            dump("fetchPlaylist100", &pf("fetchPlaylist", json!({"uri": EDITORIAL, "offset": 0, "limit": 100, "enableWatchFeedEntrypoint": false})).await);
            let user = api.username();
            dump("fetchLibraryTracks100", &pf("fetchLibraryTracks", json!({"uri": format!("spotify:user:{user}:collection"), "offset": 0, "limit": 100})).await);
            dump("entitiesTracks", &pf("fetchEntitiesForRecentlyPlayed", json!({"uris": ["spotify:track:70LcF31zb1H0PyJoS1Sx1r"]})).await);
            for range in ["MEDIUM_TERM", "LONG_TERM"] {
                dump(&format!("userTop50_{range}"), &pf("userTopContent", json!({"includeTopArtists": true, "topArtistsInput": {"offset": 0, "limit": 50, "sortBy": "AFFINITY", "timeRange": range}, "includeTopTracks": true, "topTracksInput": {"offset": 0, "limit": 50, "sortBy": "AFFINITY", "timeRange": range}})).await);
            }
        }
        api.session.shutdown();
    }
}
