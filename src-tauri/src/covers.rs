//! Cover images on disk: the `cover://` scheme (plans/2026-10-10-cover-cache).
//!
//! The webview loads `cover://localhost/<percent-encoded https CDN URL>`. A file
//! `<app dir>/cache/covers/<sha256 hex of the URL>` answers it; on a miss the image is
//! downloaded one time, written through `<key>.tmp` + a rename, then served. Only Spotify's
//! image CDNs (`*.scdn.co`, `*.spotifycdn.com`, https) are fetched; anything else is a 404.
//! Each served hit touches the file's mtime (= last shown); a sweep at launch, at most once
//! per 24 h, deletes covers not shown for `COVER_TTL`.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tauri::http::{header, Response, StatusCode};

const TARGET: &str = "stylus::covers";
/// A cover not shown for this long is deleted by the sweep.
const COVER_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// The sweep runs at most once per this long (the `.last-sweep` file's mtime).
const SWEEP_EVERY: Duration = Duration::from_secs(24 * 3600);
const SWEEP_MARK: &str = ".last-sweep";
/// Largest image download accepted (as dock.rs; Spotify covers are ~50–300 KB).
const MAX_BYTES: usize = 8 * 1024 * 1024;

/// An image and its Content-Type.
type Cover = (Vec<u8>, &'static str);

/// The file name of a cover: sha256 of its URL, lower hex.
pub(crate) fn cover_key(url: &str) -> String {
    format!("{:x}", Sha256::digest(url.as_bytes()))
}

/// Only https on Spotify's image CDNs, default port: `i.scdn.co`, `mosaic.scdn.co`, `*.spotifycdn.com`.
/// The same rule as `src/lib/cover.js`.
pub(crate) fn allowed(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else { return false };
    let host = u.host_str().unwrap_or_default();
    let cdn = |root: &str| host == root || host.ends_with(&format!(".{root}"));
    u.scheme() == "https" && u.port().is_none() && (cdn("scdn.co") || cdn("spotifycdn.com"))
}

/// The image type from the first bytes: JPEG, PNG or WebP; None for anything else (an HTML error page).
pub(crate) fn sniff_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// True when a cover last shown at `mtime` is older than the TTL at `now`.
pub(crate) fn expired(mtime: SystemTime, now: SystemTime) -> bool {
    now.duration_since(mtime).map(|age| age > COVER_TTL).unwrap_or(false)
}

/// Deletes every cover file in `dir` not shown since `now - COVER_TTL`. Skips dot files
/// (`.last-sweep`) and `.tmp` files (a download in progress). Returns (deleted, covers seen).
pub(crate) fn sweep(dir: &Path, now: SystemTime) -> (usize, usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (0, 0) };
    let (mut deleted, mut seen) = (0, 0);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.ends_with(".tmp") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        seen += 1;
        if meta.modified().map(|m| expired(m, now)).unwrap_or(false) {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => deleted += 1,
                Err(e) => log::warn!(target: TARGET, "covers: could not delete {name}: {e}"),
            }
        }
    }
    (deleted, seen)
}

/// The sweep, when the last one is older than `SWEEP_EVERY` (or never ran). Writes the marker after.
/// Returns the sweep's counts, or None when it was not due.
fn sweep_if_due(dir: &Path, now: SystemTime) -> Option<(usize, usize)> {
    let mark = dir.join(SWEEP_MARK);
    let last = std::fs::metadata(&mark).and_then(|m| m.modified()).ok();
    if last.is_some_and(|t| now.duration_since(t).map(|age| age < SWEEP_EVERY).unwrap_or(true)) {
        return None;
    }
    if !dir.is_dir() {
        return None; // no covers yet: nothing to sweep
    }
    let counts = sweep(dir, now);
    if let Err(e) = std::fs::write(&mark, b"") {
        log::warn!(target: TARGET, "covers: could not write {SWEEP_MARK}: {e}");
    }
    Some(counts)
}

/// At launch, in the background: the TTL sweep of the app's cover folder, when due.
pub(crate) fn sweep_at_launch() {
    if let Some((deleted, seen)) = sweep_if_due(&store().dir, SystemTime::now()) {
        log::info!(target: TARGET, "covers: swept {deleted} of {seen}");
    }
}

/// A stored cover, its mtime touched (= shown now). None on a miss or an unreadable/foreign file.
fn read_hit(path: &Path) -> Option<Cover> {
    let bytes = std::fs::read(path).ok()?;
    let ty = sniff_type(&bytes)?;
    if let Ok(f) = std::fs::File::options().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
    Some((bytes, ty))
}

/// Writes `<dir>/<key>` through `<key>.tmp` and a rename, so a reader never sees half a file.
fn write_atomic(dir: &Path, key: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{key}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dir.join(key)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// File I/O off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

/// The disk store plus a per-key lock, so one cover downloads one time even when it shows in 2 places.
pub(crate) struct Store {
    dir: PathBuf,
    inflight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Store {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Store { dir, inflight: Mutex::new(HashMap::new()) }
    }

    /// The cover for `url`: from disk, else from `fetch` (then stored). Err = the HTTP status to answer.
    pub(crate) async fn get<F, Fut>(&self, url: &str, fetch: F) -> Result<Cover, StatusCode>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<Vec<u8>, String>>,
    {
        if !allowed(url) {
            return Err(StatusCode::NOT_FOUND);
        }
        let key = cover_key(url);
        let path = self.dir.join(&key);
        if let Some(hit) = blocking({
            let path = path.clone();
            move || read_hit(&path)
        })
        .await
        .flatten()
        {
            return Ok(hit);
        }
        let lock = self.inflight.lock().unwrap().entry(key.clone()).or_default().clone();
        let result = {
            let _guard = lock.lock().await;
            self.fetch_and_store(url, &key, &path, fetch).await
        };
        // last one out drops the map entry (the map holds 1, we hold 1)
        let mut map = self.inflight.lock().unwrap();
        if Arc::strong_count(&lock) == 2 {
            map.remove(&key);
        }
        result
    }

    /// Under the key's lock: a download that finished while we waited is on disk now.
    async fn fetch_and_store<F, Fut>(&self, url: &str, key: &str, path: &Path, fetch: F) -> Result<Cover, StatusCode>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<Vec<u8>, String>>,
    {
        if let Some(hit) = blocking({
            let path = path.to_path_buf();
            move || read_hit(&path)
        })
        .await
        .flatten()
        {
            return Ok(hit);
        }
        let bytes = fetch(url.to_string()).await.map_err(|e| {
            log::info!(target: TARGET, "covers: download failed: {e}");
            StatusCode::BAD_GATEWAY
        })?;
        let Some(ty) = sniff_type(&bytes) else {
            log::info!(target: TARGET, "covers: not an image ({} bytes)", bytes.len());
            return Err(StatusCode::BAD_GATEWAY);
        };
        let (dir, key_owned) = (self.dir.clone(), key.to_string());
        let (bytes, written) = blocking(move || {
            let r = write_atomic(&dir, &key_owned, &bytes);
            (bytes, r)
        })
        .await
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Err(e) = written {
            // serve the downloaded bytes anyway
            log::warn!(target: TARGET, "covers: could not store a cover: {e}");
        }
        Ok((bytes, ty))
    }
}

/// The app's cover store: `<app dir>/cache/covers`.
fn store() -> &'static Store {
    static STORE: OnceLock<Store> = OnceLock::new();
    STORE.get_or_init(|| Store::new(crate::paths::app_dir().join("cache").join("covers")))
}

/// GET with the shared client: a 2xx body up to `MAX_BYTES`, else an error.
async fn fetch(url: String) -> Result<Vec<u8>, String> {
    let resp = crate::paths::http().get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(format!("{} bytes", bytes.len()));
    }
    Ok(bytes.to_vec())
}

/// `%XX` → byte; anything else as is. None when the result is not UTF-8.
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// The CDN URL inside a `cover://localhost/<percent-encoded URL>` request path.
fn url_from_path(path: &str) -> Option<String> {
    percent_decode(path.strip_prefix('/').unwrap_or(path))
}

/// Every response, also the errors: the CORS header (the glow reads the pixels with
/// `crossorigin="anonymous"`) and a Content-Type.
fn response(result: Result<Cover, StatusCode>) -> Response<Vec<u8>> {
    let (status, ty, body) = match result {
        Ok((bytes, ty)) => (StatusCode::OK, ty, bytes),
        Err(status) => (status, "text/plain", Vec::new()),
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, ty)
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// The `cover` scheme handler. Never blocks the caller: the answer comes from a spawned task.
pub(crate) fn handle(path: String, responder: tauri::UriSchemeResponder) {
    tauri::async_runtime::spawn(async move {
        let result = match url_from_path(&path) {
            Some(url) => store().get(&url, fetch).await,
            None => Err(StatusCode::NOT_FOUND),
        };
        responder.respond(response(result));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F'];
    const DAY: Duration = Duration::from_secs(24 * 3600);

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stylus-covers-{name}-{}-{}", std::process::id(), crate::paths::now_ms()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn set_mtime(path: &Path, t: SystemTime) {
        std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
    }

    fn mtime(path: &Path) -> SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn cover_key_is_stable_hex() {
        let k = cover_key("https://i.scdn.co/image/abc");
        assert_eq!(k, cover_key("https://i.scdn.co/image/abc"));
        assert_ne!(k, cover_key("https://i.scdn.co/image/abd"));
        assert_eq!(k.len(), 64);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn allowed_only_cdn_https() {
        for ok in [
            "https://i.scdn.co/image/ab67616d0000b273",
            "https://mosaic.scdn.co/640/ab67616d0000b273",
            "https://image-cdn-ak.spotifycdn.com/image/ab67706c",
        ] {
            assert!(allowed(ok), "{ok}");
        }
        for bad in [
            "http://i.scdn.co/image/abc",
            "https://example.com/a.jpg",
            "https://scdn.co.evil.com/a.jpg",
            "https://notscdn.co/a.jpg",
            "https://i.scdn.co:8443/image/abc",
            "file:///etc/passwd",
            "",
        ] {
            assert!(!allowed(bad), "{bad}");
        }
    }

    #[test]
    fn sniff_type_by_magic_bytes() {
        assert_eq!(sniff_type(JPEG), Some("image/jpeg"));
        assert_eq!(sniff_type(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]), Some("image/png"));
        assert_eq!(sniff_type(b"RIFF\x10\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_type(b"<!DOCTYPE html><html>"), None);
        assert_eq!(sniff_type(b""), None);
    }

    #[test]
    fn expired_after_30_days() {
        let now = SystemTime::now();
        assert!(!expired(now - 29 * DAY, now));
        assert!(expired(now - 31 * DAY, now));
        assert!(!expired(now + DAY, now)); // a clock jump back never deletes
    }

    #[test]
    fn sweep_deletes_only_old_covers() {
        let dir = temp_dir("sweep");
        let now = SystemTime::now();
        for (name, age) in [("old", 31), ("fresh", 29), (".last-sweep", 40), ("dl.tmp", 40)] {
            let p = dir.join(name);
            std::fs::write(&p, JPEG).unwrap();
            set_mtime(&p, now - age * DAY);
        }
        assert_eq!(sweep(&dir, now), (1, 2));
        assert!(!dir.join("old").exists());
        assert!(dir.join("fresh").exists());
        assert!(dir.join(".last-sweep").exists());
        assert!(dir.join("dl.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sweep_runs_at_most_once_a_day() {
        let dir = temp_dir("due");
        let now = SystemTime::now();
        assert_eq!(sweep_if_due(&dir, now), Some((0, 0)));
        assert!(dir.join(SWEEP_MARK).exists());
        assert_eq!(sweep_if_due(&dir, now + Duration::from_secs(23 * 3600)), None);
        assert_eq!(sweep_if_due(&dir, now + DAY + Duration::from_secs(60)), Some((0, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn url_from_path_decodes() {
        assert_eq!(
            url_from_path("/https%3A%2F%2Fi.scdn.co%2Fimage%2Fab67%3Fx%3D1").as_deref(),
            Some("https://i.scdn.co/image/ab67?x=1")
        );
        assert_eq!(url_from_path("/a%2").as_deref(), Some("a%2"));
        assert_eq!(url_from_path("/%FF").as_deref(), None);
    }

    #[test]
    fn response_has_cors_and_type() {
        let ok = response(Ok((JPEG.to_vec(), "image/jpeg")));
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(ok.headers()[header::CONTENT_TYPE], "image/jpeg");
        assert_eq!(ok.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        let err = response(Err(StatusCode::BAD_GATEWAY));
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(err.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    }

    const URL: &str = "https://i.scdn.co/image/ab67616d0000b273";

    #[tokio::test]
    async fn store_writes_through_tmp_and_reads_back_from_disk() {
        let dir = temp_dir("store");
        let store = Store::new(dir.clone());
        let calls = AtomicUsize::new(0);
        let fetch = |_: String| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(JPEG.to_vec()) }
        };
        assert_eq!(store.get(URL, &fetch).await, Ok((JPEG.to_vec(), "image/jpeg")));
        let key = cover_key(URL);
        assert_eq!(std::fs::read(dir.join(&key)).unwrap(), JPEG);
        assert!(!dir.join(format!("{key}.tmp")).exists());
        // the second read comes from disk, and touches the mtime
        set_mtime(&dir.join(&key), SystemTime::now() - 10 * DAY);
        assert_eq!(store.get(URL, &fetch).await, Ok((JPEG.to_vec(), "image/jpeg")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(SystemTime::now().duration_since(mtime(&dir.join(&key))).unwrap() < DAY);
        assert!(store.inflight.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn store_refuses_non_cdn_and_non_images() {
        let dir = temp_dir("refuse");
        let store = Store::new(dir.clone());
        let calls = AtomicUsize::new(0);
        let html = |_: String| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(b"<html>error</html>".to_vec()) }
        };
        assert_eq!(store.get("https://example.com/a.jpg", &html).await, Err(StatusCode::NOT_FOUND));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(store.get(URL, &html).await, Err(StatusCode::BAD_GATEWAY));
        assert!(!dir.join(cover_key(URL)).exists());
        let offline = |_: String| async { Err::<Vec<u8>, _>("offline".to_string()) };
        assert_eq!(store.get(URL, offline).await, Err(StatusCode::BAD_GATEWAY));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn store_serves_bytes_when_the_write_fails() {
        // the store dir is a file: create_dir_all fails, the download is served anyway
        let file = temp_dir("nowrite").join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let store = Store::new(file.clone());
        let fetch = |_: String| async { Ok(JPEG.to_vec()) };
        assert_eq!(store.get(URL, fetch).await, Ok((JPEG.to_vec(), "image/jpeg")));
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[tokio::test]
    async fn one_download_for_two_views_at_once() {
        let dir = temp_dir("once");
        let store = Store::new(dir.clone());
        let calls = AtomicUsize::new(0);
        let fetch = |_: String| {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(JPEG.to_vec())
            }
        };
        let (a, b) = tokio::join!(store.get(URL, &fetch), store.get(URL, &fetch));
        assert_eq!(a, Ok((JPEG.to_vec(), "image/jpeg")));
        assert_eq!(b, a);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(store.inflight.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
