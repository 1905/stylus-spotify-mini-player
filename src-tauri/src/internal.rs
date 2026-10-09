//! Spotify's internal endpoints, reached with the in-app player's own librespot Session
//! (login5 Bearer token + client-token). The app has no other data source.
//! See spikes/internal-api/REPORT.md for what was probed.

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use librespot_core::Session;
use serde_json::{json, Value};

pub const PATHFINDER: &str = "https://api-partner.spotify.com/pathfinder/v2/query";
const LOG: &str = "stylus::internal";
/// One request (tokens included) may take this long, like `paths::http()`'s timeout.
const SEND_TIMEOUT: Duration = Duration::from_secs(15);

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
        Api { session, http: crate::paths::http() }
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
    /// HTTP status and the start of the body. Tokens are never logged. Each try has
    /// `SEND_TIMEOUT` (`transport: timeout`); an HTTP 401 drops the login5 token and tries once more.
    pub async fn send(&self, method: reqwest::Method, url: &str, ctype: Option<&str>, accept: Option<&str>, body: Option<Vec<u8>>) -> Result<Vec<u8>, String> {
        send_retrying(
            || self.attempt(method.clone(), url, ctype, accept, body.clone()),
            |token| self.session.login5().invalidate(token),
            SEND_TIMEOUT,
        )
        .await
    }

    /// One try: the token it used, the HTTP status and the body.
    async fn attempt(&self, method: reqwest::Method, url: &str, ctype: Option<&str>, accept: Option<&str>, body: Option<Vec<u8>>) -> Result<(String, u16, Vec<u8>), String> {
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
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await.map_err(|e| format!("transport: {e}"))?.to_vec();
        Ok((token.access_token, status, bytes))
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

/// `fut`, or `transport: timeout` when it takes longer than `limit`.
async fn with_deadline<T>(limit: Duration, fut: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::time::timeout(limit, fut).await.unwrap_or_else(|_| Err("transport: timeout".into()))
}

/// `Api::send`'s rules around one try (`attempt`: token, status, body): each try within `limit`;
/// on HTTP 401 `invalidate` the token and try once more. Ok = a 2xx body; else `http_error`.
async fn send_retrying<F, Fut>(attempt: F, invalidate: impl FnOnce(&str), limit: Duration) -> Result<Vec<u8>, String>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(String, u16, Vec<u8>), String>>,
{
    let (token, mut status, mut bytes) = with_deadline(limit, attempt()).await?;
    if status == 401 {
        log::info!(target: LOG, "HTTP 401: a new login5 token, one more try");
        invalidate(&token);
        (_, status, bytes) = with_deadline(limit, attempt()).await?;
    }
    if !(200..300).contains(&status) {
        return Err(http_error(status, &bytes));
    }
    Ok(bytes)
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
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Source::Primary => "internal",
            Source::Fallback => "internal fallback",
        })
    }
}

/// One step of a chain: a source and its (lazy) request.
pub type Attempt<'a, T> = (Source, futures_util::future::BoxFuture<'a, Result<T, String>>);

/// The error a failed chain returns: the first error that is not `ENGINE_NOT_READY`, else the
/// first error, else "no source".
pub fn final_error(errs: &[(Source, String)]) -> String {
    let real = errs.iter().find(|(_, e)| !e.starts_with("ENGINE_NOT_READY"));
    real.or(errs.first()).map(|(_, e)| e.clone()).unwrap_or_else(|| "no source".into())
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
    if err.starts_with("ENGINE_NOT_READY") {
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

/// `serve` with one step: `f` with the engine's live session (`with_api`, Primary).
pub async fn serve_one<T, F, Fut>(op: &str, f: F) -> Result<T, String>
where
    F: FnOnce(Api) -> Fut + Send,
    Fut: std::future::Future<Output = Result<T, String>> + Send,
{
    serve(op, vec![with_api(Source::Primary, f)]).await
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

// ---- operations ----------------------------------------------------------------------------

/// How many page requests a list fetch keeps in flight.
pub(crate) const PAGE_CONCURRENCY: usize = 4;

/// The offsets still to fetch after the first page (offset 0) of an offset-paged
/// list holding `total` items, up to `max_items`.
fn page_offsets(total: usize, page_size: usize, max_items: usize) -> Vec<usize> {
    (page_size..total.min(max_items)).step_by(page_size.max(1)).collect()
}

/// Up to `max_items` items of an offset-paged list: `first` is the page at offset 0 (`{total,
/// items}`), `fetch(offset)` returns the page at that offset. The remaining pages are fetched
/// `concurrency` at a time; items come back in offset order whatever order the pages complete in.
/// Any failed page fails the whole call.
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

/// `{total, items}` for the offset pager (`pages_with`).
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
    let items = pages_with(first, page, max, PAGE_CONCURRENCY, fetch).await?;
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
        let ctx = self.session.spclient().get_context(&format!("spotify:search:{}", crate::paths::urlencode(query))).await.map_err(|e| format!("context-resolve: {e}"))?;
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
        let path = format!("/playlist/v2/user/{}/rootlist?decorate=revision,attributes,length,owner,capabilities,status_code&from=0&length=1000", crate::paths::urlencode(&user));
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
        pages_with(first, page, usize::MAX, PAGE_CONCURRENCY, |offset| async move { Ok(self.playlist_page(playlist_id, offset, page).await?.0) }).await
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
        let path = format!("/playlist/v2/playlist/{}?from={from}&length={length}", crate::paths::urlencode(playlist_id));
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
        let uris: Vec<String> = pages_with(first, PAGE, usize::MAX, PAGE_CONCURRENCY, |from| async move {
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
        let item = crate::pb::CollectionItem { uri: format!("spotify:track:{track_id}"), added_at: if saved { crate::paths::now() as i64 } else { 0 }, is_removed: !saved };
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
        let path = format!("/recently-played/v3/user/{}/recently-played?format=json&offset=0&limit=50&filter=default,collection-new-episodes", crate::paths::urlencode(&self.username()));
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
pub(crate) fn connect() -> Result<(std::sync::Arc<librespot_protocol::connect::Cluster>, String, u8), String> {
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
    Ok(crate::pb::cluster_state(&cluster, crate::paths::now_ms() as i64))
}

/// The UI's playback state from the Connect cluster: `{active:false}` when no device is active,
/// else `{active, is_playing, progress_ms, device_id, device_name, track, shuffle, repeat,
/// volume_percent, supports_volume, context_uri}`. Track names come from the internal API (just the
/// uri when that fails), the device name and volume from the device list.
pub async fn playback_snapshot() -> Result<Value, String> {
    match cluster_state()? {
        Some(c) => Ok(playback_snapshot_from(&c).await),
        None => Ok(json!({ "active": false })),
    }
}

/// `playback_snapshot` from a `cluster_state` the caller already has.
pub async fn playback_snapshot_from(c: &Value) -> Value {
    let track = match c["track_uri"].as_str() {
        Some(uri) => track_meta(uri).await,
        None => Value::Null,
    };
    snapshot_shape(c, track, &devices().unwrap_or_default())
}

/// The metadata of track `uri` (just the uri when the fetch fails). The last answer is kept:
/// the UI polls the same track many times, so it is fetched once per track change.
async fn track_meta(uri: &str) -> Value {
    static LAST: std::sync::Mutex<Option<(String, Value)>> = std::sync::Mutex::new(None);
    let hit = crate::nowplaying::lock(&LAST).as_ref().filter(|(u, _)| u == uri).map(|(_, t)| t.clone());
    if let Some(t) = hit {
        return t;
    }
    let fetched = match Api::current() {
        Ok(api) => api.tracks(&[uri.to_string()]).await.ok().and_then(|t| t.into_iter().next()),
        Err(_) => None,
    };
    match fetched {
        Some(t) => {
            *crate::nowplaying::lock(&LAST) = Some((uri.to_string(), t.clone()));
            t
        }
        None => json!({ "uri": uri }),
    }
}

/// `playback_snapshot`'s shape from a `cluster_state`, its track and the device list.
fn snapshot_shape(c: &Value, track: Value, devices: &Value) -> Value {
    let none = Value::Null;
    let dev = devices.as_array().into_iter().flatten().find(|d| d["id"] == c["device_id"]).unwrap_or(&none);
    json!({
        "active": true,
        "is_playing": c["is_playing"].as_bool().unwrap_or(false),
        "progress_ms": c["position_ms"],
        "device_id": c["device_id"],
        "device_name": dev["name"],
        "track": track,
        "shuffle": c["shuffle"].as_bool().unwrap_or(false),
        "repeat": c["repeat"].as_str().unwrap_or("off"),
        "volume_percent": dev["volume_percent"],
        "supports_volume": dev["supports_volume"].as_bool().unwrap_or(false),
        "context_uri": c["context_uri"],
    })
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
/// Spirc handles `add_to_queue`). Other devices: Err.
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

// ---- remote control: connect-state commands to another device -------------------------------
// The bodies are the ones that passed the P1 probe (plans/2026-10-09-drop-web-api/spike-connect-state.md).

pub use crate::session::Repeat as RepeatMode;

/// A command for another Connect device.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteCmd {
    Pause,
    Resume,
    Next,
    Previous,
    Seek(u64),
    Shuffle(bool),
    Repeat(RepeatMode),
    /// Percent, 0–100.
    Volume(u8),
    /// A context (from `track` when given) or a track list (from `track`, else the first).
    Play { context: Option<String>, uris: Vec<String>, track: Option<String>, position_ms: u64 },
}

impl RemoteCmd {
    /// The command's name in a `NOT_AVAILABLE_REMOTE` error.
    pub fn action(&self) -> &'static str {
        match self {
            RemoteCmd::Pause => "pause",
            RemoteCmd::Resume => "resume",
            RemoteCmd::Next => "next",
            RemoteCmd::Previous => "previous",
            RemoteCmd::Seek(_) => "seek",
            RemoteCmd::Shuffle(_) => "shuffle",
            RemoteCmd::Repeat(_) => "repeat",
            RemoteCmd::Volume(_) => "volume",
            RemoteCmd::Play { .. } => "play",
        }
    }
}

/// A player command body: `{"command": {"endpoint": ..., <fields>}}`.
fn player_command(endpoint: &str, fields: Value) -> (reqwest::Method, String, Value) {
    let mut cmd = json!({ "endpoint": endpoint });
    if let (Some(c), Value::Object(f)) = (cmd.as_object_mut(), fields) {
        c.extend(f);
    }
    (reqwest::Method::POST, "player/command".into(), json!({ "command": cmd }))
}

/// The requests (method, path kind under `/connect-state/v1/`, body) that send `cmd`, in order.
/// Repeat takes two (context flag, track flag). Empty = not sendable.
pub fn command_bodies(cmd: &RemoteCmd) -> Vec<(reqwest::Method, String, Value)> {
    let flag = |endpoint: &str, on: bool| player_command(endpoint, json!({ "value": on }));
    match cmd {
        RemoteCmd::Pause => vec![player_command("pause", json!({}))],
        RemoteCmd::Resume => vec![player_command("resume", json!({}))],
        RemoteCmd::Next => vec![player_command("skip_next", json!({}))],
        RemoteCmd::Previous => vec![player_command("skip_prev", json!({}))],
        RemoteCmd::Seek(ms) => vec![player_command("seek_to", json!({ "value": ms }))],
        RemoteCmd::Shuffle(on) => vec![flag("set_shuffling_context", *on)],
        // track off before context off, context on before track on: no step shows a mode nobody asked for
        RemoteCmd::Repeat(RepeatMode::Off) => vec![flag("set_repeating_track", false), flag("set_repeating_context", false)],
        RemoteCmd::Repeat(RepeatMode::Context) => vec![flag("set_repeating_context", true), flag("set_repeating_track", false)],
        RemoteCmd::Repeat(RepeatMode::Track) => vec![flag("set_repeating_context", true), flag("set_repeating_track", true)],
        RemoteCmd::Volume(p) => vec![(reqwest::Method::PUT, "connect/volume".into(), json!({ "volume": crate::nowplaying::volume_from_percent(*p) }))],
        RemoteCmd::Play { context, uris, track, position_ms } => {
            let (ctx, skip_to) = match context {
                Some(c) => (json!({ "uri": c, "url": format!("context://{c}") }), track.as_ref().map(|t| json!({ "track_uri": t }))),
                None => {
                    if uris.is_empty() {
                        return vec![];
                    }
                    let index = track.as_ref().and_then(|t| uris.iter().position(|u| u == t)).unwrap_or(0);
                    let tracks: Vec<Value> = uris.iter().map(|u| json!({ "uri": u })).collect();
                    (json!({ "pages": [{ "tracks": tracks }] }), Some(json!({ "track_index": index })))
                }
            };
            let mut options = json!({});
            if let Some(s) = skip_to {
                options["skip_to"] = s;
            }
            // not probed in P1: librespot's PlayOptions reads it (dealer/protocol/request.rs)
            if *position_ms > 0 {
                options["seek_to"] = json!(position_ms);
            }
            let mut fields = json!({ "context": ctx });
            if options.as_object().is_some_and(|o| !o.is_empty()) {
                fields["options"] = options;
            }
            vec![player_command("play", fields)]
        }
    }
}

/// A failed remote request as the caller sees it: a 4xx "refused" answer (400, 403, 404, 405, 501)
/// is `NOT_AVAILABLE_REMOTE`; other failures (transport, 5xx, 429) keep their text.
pub fn remote_error(action: &str, err: String) -> String {
    match err.strip_prefix("HTTP ").and_then(|r| r.get(..3)) {
        Some("400" | "403" | "404" | "405" | "501") => crate::control::not_available_remote(action),
        _ => err,
    }
}

/// This Mac's id and the API, for a request from This Mac to another device.
fn remote_caller() -> Result<(String, Api), String> {
    let own = own_device_id().ok_or_else(|| not_ready("the player isn't connected"))?;
    Ok((own, Api::current()?))
}

/// One connect-state request from This Mac to `target`; a refusal is logged with its HTTP text.
async fn remote_send(api: &Api, own: &str, target: &str, action: &str, (method, kind, body): (reqwest::Method, String, Value)) -> Result<(), String> {
    let path = format!("/connect-state/v1/{kind}/from/{own}/to/{target}");
    let res = api.spclient(method, &path, Some("application/json"), None, Some(body.to_string().into_bytes())).await;
    res.map(drop).map_err(|e| {
        log::warn!(target: LOG, "remote {action} to {target}: {e}");
        remote_error(action, e)
    })
}

/// Sends `cmd` to the Connect device `target`. Ok = Spotify took it (HTTP 200 + ack_id); the
/// device decides later, and the UI's poll shows what it did.
pub async fn remote_command(target: &str, cmd: RemoteCmd) -> Result<(), String> {
    let action = cmd.action();
    let reqs = command_bodies(&cmd);
    if reqs.is_empty() {
        return Err(crate::control::not_available_remote(action));
    }
    let (own, api) = remote_caller()?;
    for req in reqs {
        remote_send(&api, &own, target, action, req).await?;
    }
    Ok(())
}

/// The transfer body: the target restores the play/pause state of the device it takes over.
pub fn transfer_body() -> Value {
    json!({ "transfer_options": { "restore_paused": "restore" } })
}

/// Moves playback from the active device to `target` (the `SpClient::transfer` endpoint, sent
/// with the same headers as the other commands so a refusal reads the same).
pub async fn remote_transfer(target: &str) -> Result<(), String> {
    let (own, api) = remote_caller()?;
    remote_send(&api, &own, target, "transfer", (reqwest::Method::POST, "connect/transfer".into(), transfer_body())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `send_retrying` over canned answers (token, status, body), one per try.
    async fn tries(answers: Vec<Result<(String, u16, Vec<u8>), String>>) -> (Result<Vec<u8>, String>, usize, Vec<String>) {
        let answers = std::sync::Mutex::new(answers.into_iter());
        let count = std::sync::atomic::AtomicUsize::new(0);
        let mut dropped = Vec::new();
        let r = send_retrying(
            || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let next = answers.lock().unwrap().next().expect("no more answers");
                async move { next }
            },
            |t| dropped.push(t.to_string()),
            SEND_TIMEOUT,
        )
        .await;
        (r, count.into_inner(), dropped)
    }

    fn ok(token: &str, status: u16, body: &str) -> Result<(String, u16, Vec<u8>), String> {
        Ok((token.into(), status, body.as_bytes().to_vec()))
    }

    #[tokio::test]
    async fn a_rejected_token_is_dropped_and_tried_once_more() {
        assert_eq!(tries(vec![ok("t1", 200, "a")]).await, (Ok(b"a".to_vec()), 1, vec![]));
        assert_eq!(tries(vec![ok("t1", 401, "no"), ok("t2", 200, "b")]).await, (Ok(b"b".to_vec()), 2, vec!["t1".to_string()]));
        // a second 401 is the answer: no loop
        assert_eq!(tries(vec![ok("t1", 401, "no"), ok("t2", 401, "still no")]).await, (Err("HTTP 401: still no".into()), 2, vec!["t1".to_string()]));
        // other errors: no retry
        assert_eq!(tries(vec![ok("t1", 403, "forbidden")]).await, (Err("HTTP 403: forbidden".into()), 1, vec![]));
        assert_eq!(tries(vec![Err("login5 token: x".into())]).await, (Err("login5 token: x".into()), 1, vec![]));
    }

    #[tokio::test]
    async fn a_stalled_request_times_out() {
        let started = std::time::Instant::now();
        let r = with_deadline(Duration::from_millis(100), std::future::pending::<Result<(), String>>()).await;
        assert_eq!(r, Err("transport: timeout".into()));
        assert!(started.elapsed() < Duration::from_secs(1));
        // a stalled try inside send_retrying times out the same way
        let r = send_retrying(|| std::future::pending::<Result<(String, u16, Vec<u8>), String>>(), |_| {}, Duration::from_millis(100)).await;
        assert_eq!(r, Err("transport: timeout".into()));
    }

    #[test]
    fn final_error_rules() {
        let e = |s: Source, m: &str| (s, m.to_string());
        // the first error that is not ENGINE_NOT_READY, else the first, else "no source"
        assert_eq!(final_error(&[e(Source::Primary, "ENGINE_NOT_READY: x"), e(Source::Fallback, "HTTP 403")]), "HTTP 403");
        assert_eq!(final_error(&[e(Source::Primary, "ENGINE_NOT_READY: x")]), "ENGINE_NOT_READY: x");
        assert_eq!(final_error(&[]), "no source");
        assert_eq!(final_error(&[e(Source::Primary, "GraphQL: PersistedQueryNotFound"), e(Source::Fallback, "HTTP 404: y")]), "GraphQL: PersistedQueryNotFound");
    }

    #[test]
    fn playback_snapshot_shape_full() {
        let c = json!({ "device_id": "d1", "track_uri": "spotify:track:1", "context_uri": "spotify:playlist:p1",
            "is_playing": true, "position_ms": 42, "duration_ms": 1000, "shuffle": true, "repeat": "context" });
        let devices = json!([{ "id": "d0", "name": "Phone" }, { "id": "d1", "name": "Mac", "volume_percent": 55, "supports_volume": true }]);
        let track = json!({ "id": "1", "uri": "spotify:track:1" });
        let s = snapshot_shape(&c, track, &devices);
        assert_eq!(
            s,
            json!({ "active": true, "is_playing": true, "progress_ms": 42, "device_id": "d1", "device_name": "Mac",
                "track": { "id": "1", "uri": "spotify:track:1" }, "shuffle": true, "repeat": "context",
                "volume_percent": 55, "supports_volume": true, "context_uri": "spotify:playlist:p1" })
        );
    }

    #[test]
    fn playback_snapshot_shape_sparse() {
        let c = json!({ "device_id": "d2", "track_uri": null, "context_uri": null, "is_playing": false, "position_ms": 0,
            "shuffle": false, "repeat": "track" });
        let s = snapshot_shape(&c, Value::Null, &json!([{ "id": "d2", "name": "Amp", "volume_percent": null }]));
        assert!(s["track"].is_null());
        assert_eq!(s["shuffle"], false);
        assert_eq!(s["repeat"], "track");
        assert!(s["volume_percent"].is_null());
        assert_eq!(s["supports_volume"], false);
        assert!(s["context_uri"].is_null());
        // missing fields entirely, and a device that isn't listed
        let s = snapshot_shape(&json!({ "device_id": "gone" }), Value::Null, &Value::Null);
        assert_eq!(s["active"], true);
        assert_eq!(s["is_playing"], false);
        assert_eq!(s["shuffle"], false);
        assert_eq!(s["repeat"], "off");
        assert_eq!(s["supports_volume"], false);
        assert!(s["device_name"].is_null());
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
            if o == 200 { Err("HTTP 500: boom".to_string()) } else { Ok(page_at(o, 50)) }
        })
        .await;
        assert_eq!(err, Err("HTTP 500: boom".to_string()));
        // total ≤ one page: no requests
        let one = json!({ "total": 3, "items": [0, 1, 2] });
        let items = pages_with(one, 50, usize::MAX, 4, |_| async { Err::<Value, _>("no".to_string()) }).await.unwrap();
        assert_eq!(items.len(), 3);
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
        let got = serve("t", vec![step(Source::Primary, Err("HTTP 500".into())), step(Source::Fallback, Ok(2)), step(Source::Fallback, Ok(3))]).await;
        assert_eq!(got, Ok(2));
        assert_eq!(ran.load(Ordering::SeqCst), 2, "the third step never ran");
        let got = serve("t", vec![step(Source::Primary, Err("ENGINE_NOT_READY: x".into())), step(Source::Fallback, Err("HTTP 404: y".into()))]).await;
        assert_eq!(got, Err("HTTP 404: y".into()));
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

    /// The one request of `cmd` as (method, path kind, body).
    fn one(cmd: RemoteCmd) -> (String, String, Value) {
        let mut reqs = command_bodies(&cmd);
        assert_eq!(reqs.len(), 1, "{cmd:?}");
        let (m, k, b) = reqs.remove(0);
        (m.to_string(), k, b)
    }

    fn cmd_json(s: &str) -> (String, String, Value) {
        ("POST".into(), "player/command".into(), serde_json::from_str(s).unwrap())
    }

    // the bodies that passed the P1 probe (spike-connect-state.md, "Bodies")
    #[test]
    fn remote_transport_bodies_match_the_spike() {
        assert_eq!(one(RemoteCmd::Pause), cmd_json(r#"{"command":{"endpoint":"pause"}}"#));
        assert_eq!(one(RemoteCmd::Resume), cmd_json(r#"{"command":{"endpoint":"resume"}}"#));
        assert_eq!(one(RemoteCmd::Next), cmd_json(r#"{"command":{"endpoint":"skip_next"}}"#));
        assert_eq!(one(RemoteCmd::Previous), cmd_json(r#"{"command":{"endpoint":"skip_prev"}}"#));
        assert_eq!(one(RemoteCmd::Seek(60000)), cmd_json(r#"{"command":{"endpoint":"seek_to","value":60000}}"#));
        assert_eq!(one(RemoteCmd::Shuffle(true)), cmd_json(r#"{"command":{"endpoint":"set_shuffling_context","value":true}}"#));
        assert_eq!(one(RemoteCmd::Shuffle(false)), cmd_json(r#"{"command":{"endpoint":"set_shuffling_context","value":false}}"#));
    }

    #[test]
    fn remote_repeat_sends_both_flags() {
        let bodies = |m: RepeatMode| command_bodies(&RemoteCmd::Repeat(m)).into_iter().map(|(_, _, b)| b).collect::<Vec<_>>();
        let flag = |e: &str, on: bool| json!({"command": {"endpoint": e, "value": on}});
        assert_eq!(bodies(RepeatMode::Off), vec![flag("set_repeating_track", false), flag("set_repeating_context", false)]);
        assert_eq!(bodies(RepeatMode::Context), vec![flag("set_repeating_context", true), flag("set_repeating_track", false)]);
        assert_eq!(bodies(RepeatMode::Track), vec![flag("set_repeating_context", true), flag("set_repeating_track", true)]);
        assert!(command_bodies(&RemoteCmd::Repeat(RepeatMode::Track)).iter().all(|(m, k, _)| *m == reqwest::Method::POST && k == "player/command"));
    }

    #[test]
    fn remote_volume_is_a_put_of_0_to_65535() {
        assert_eq!(one(RemoteCmd::Volume(50)), ("PUT".into(), "connect/volume".into(), json!({"volume": 32768})));
        assert_eq!(one(RemoteCmd::Volume(100)).2, json!({"volume": 65535}));
        assert_eq!(one(RemoteCmd::Volume(0)).2, json!({"volume": 0}));
        assert_eq!(one(RemoteCmd::Volume(250)).2, json!({"volume": 65535}), "capped at 100 %");
    }

    #[test]
    fn remote_play_bodies_match_the_spike() {
        let album = "spotify:album:6N9PS4QXF1D0OWPk0Sxtb4";
        let ctx = RemoteCmd::Play { context: Some(album.into()), uris: vec![], track: Some("spotify:track:4uLU6hMCjMI75M1A2tKUQC".into()), position_ms: 0 };
        assert_eq!(
            one(ctx),
            cmd_json(r#"{"command":{"context":{"uri":"spotify:album:6N9PS4QXF1D0OWPk0Sxtb4","url":"context://spotify:album:6N9PS4QXF1D0OWPk0Sxtb4"},"endpoint":"play","options":{"skip_to":{"track_uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC"}}}}"#)
        );
        let uris = vec!["spotify:track:4uLU6hMCjMI75M1A2tKUQC".to_string(), "spotify:track:6aiKIFjPwa3UvDCD5ecoJj".to_string()];
        let list = RemoteCmd::Play { context: None, uris: uris.clone(), track: Some(uris[1].clone()), position_ms: 0 };
        assert_eq!(
            one(list),
            cmd_json(r#"{"command":{"context":{"pages":[{"tracks":[{"uri":"spotify:track:4uLU6hMCjMI75M1A2tKUQC"},{"uri":"spotify:track:6aiKIFjPwa3UvDCD5ecoJj"}]}]},"endpoint":"play","options":{"skip_to":{"track_index":1}}}}"#)
        );
        // no track: the context from its top (no options); a list from its first
        let top = one(RemoteCmd::Play { context: Some(album.into()), uris: vec![], track: None, position_ms: 0 }).2;
        assert!(top["command"].get("options").is_none());
        assert_eq!(one(RemoteCmd::Play { context: None, uris: uris.clone(), track: None, position_ms: 0 }).2["command"]["options"], json!({"skip_to": {"track_index": 0}}));
        // a start position: options.seek_to
        let at = one(RemoteCmd::Play { context: None, uris: uris.clone(), track: None, position_ms: 61_000 }).2;
        assert_eq!(at["command"]["options"]["seek_to"], 61_000);
        // nothing to play: no request
        assert!(command_bodies(&RemoteCmd::Play { context: None, uris: vec![], track: None, position_ms: 0 }).is_empty());
    }

    #[test]
    fn remote_transfer_body_matches_the_spike() {
        assert_eq!(transfer_body(), json!({"transfer_options": {"restore_paused": "restore"}}));
    }

    #[test]
    fn remote_refusals_are_not_available_remote() {
        for s in [400, 403, 404, 405, 501] {
            assert_eq!(remote_error("pause", http_error(s, b"no")), "NOT_AVAILABLE_REMOTE: pause on other devices", "{s}");
        }
        // other failures keep their own text
        assert_eq!(remote_error("pause", http_error(500, b"boom")), "HTTP 500: boom");
        assert_eq!(remote_error("pause", http_error(429, b"")), "HTTP 429: ");
        assert_eq!(remote_error("pause", "transport: timed out".into()), "transport: timed out");
        assert_eq!(remote_error("pause", "ENGINE_NOT_READY: x".into()), "ENGINE_NOT_READY: x");
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
