//! Disk cache for list responses (playlists, albums, Liked Songs …), so a list
//! the user opened before shows at once.
//!
//! Every key is scoped to the verified Spotify account (`<account>/<key>`): one
//! account never reads another's lists. A file is
//! `<app_dir>/cache/lists/<hash of the full key>.json` holding
//! `{key, saved_at, value}`; the stored key is checked on read, so a hash clash
//! reads as a miss. The cache never fails a command: every I/O error is logged
//! and ignored.

use serde_json::{json, Value};
use std::path::PathBuf;

/// Over this, a `put` trims the cache dir…
const MAX_BYTES: u64 = 50 * 1024 * 1024;
/// …down to this, oldest files first.
const TARGET_BYTES: u64 = 40 * 1024 * 1024;

pub(crate) struct Cache {
    dir: PathBuf,
    max_bytes: u64,
    target_bytes: u64,
}

/// The app's list cache.
pub(crate) fn lists() -> Cache {
    Cache::new(crate::auth::app_dir().join("cache").join("lists"))
}

impl Cache {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Cache { dir, max_bytes: MAX_BYTES, target_bytes: TARGET_BYTES }
    }

    fn path(&self, full_key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", hash_hex(full_key)))
    }

    /// The cached value, or None on a miss, a key mismatch, an empty account or a corrupt file.
    pub(crate) fn get(&self, account: &str, key: &str) -> Option<Value> {
        let full = full_key(account, key)?;
        let data = std::fs::read_to_string(self.path(&full)).ok()?;
        let mut body: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("cache: corrupt entry for {full}: {e}");
                return None;
            }
        };
        if body["key"].as_str() != Some(full.as_str()) {
            return None;
        }
        Some(body["value"].take())
    }

    /// Stores `value` under `key`, then trims the dir if it grew past the cap.
    pub(crate) fn put(&self, account: &str, key: &str, value: &Value) {
        let Some(full) = full_key(account, key) else { return };
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            return eprintln!("cache: could not create {}: {e}", self.dir.display());
        }
        let body = json!({ "key": full, "saved_at": crate::auth::now(), "value": value });
        let path = self.path(&full);
        if let Err(e) = crate::auth::write_private(&path, &body.to_string()) {
            return eprintln!("cache: could not write {}: {e}", path.display());
        }
        self.evict();
    }

    /// Deletes the oldest-mtime files until the dir is at most `target_bytes`,
    /// when it's over `max_bytes`.
    fn evict(&self) {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) => return eprintln!("cache: could not list {}: {e}", self.dir.display()),
        };
        let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                meta.is_file().then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), e.path()))
            })
            .collect();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= self.max_bytes {
            return;
        }
        files.sort_by_key(|f| f.0);
        for (_, len, path) in files {
            if total <= self.target_bytes {
                break;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => total -= len,
                Err(e) => eprintln!("cache: could not evict {}: {e}", path.display()),
            }
        }
    }
}

/// `<account>/<key>`, or None without an account (nothing is cached unscoped).
fn full_key(account: &str, key: &str) -> Option<String> {
    (!account.is_empty()).then(|| format!("{account}/{key}"))
}

/// Hex of the first 20 bytes of the key's SHA-256: a file name that is safe for any key.
fn hash_hex(key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(key.as_bytes())[..20].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    /// A fresh, empty dir under the system temp dir.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stylus-cache-{name}-{}-{}", std::process::id(), crate::auth::now()));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trip() {
        let c = Cache::new(temp_dir("rt"));
        let v = json!([{"id": "1"}, {"id": "2"}]);
        assert_eq!(c.get("acc", "liked"), None);
        c.put("acc", "liked", &v);
        assert_eq!(c.get("acc", "liked"), Some(v.clone()));
        // another account never sees it
        assert_eq!(c.get("other", "liked"), None);
        // overwrite
        c.put("acc", "liked", &json!([]));
        assert_eq!(c.get("acc", "liked"), Some(json!([])));
    }

    #[test]
    fn file_layout() {
        let dir = temp_dir("layout");
        let c = Cache::new(dir.clone());
        c.put("acc", "album:x", &json!(1));
        let path = dir.join(format!("{}.json", hash_hex("acc/album:x")));
        let body: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(body["key"], "acc/album:x");
        assert_eq!(body["value"], 1);
        assert!(body["saved_at"].as_u64().unwrap() > 0);
        assert_eq!(hash_hex("acc/album:x").len(), 40);
    }

    #[test]
    fn empty_account_is_never_cached() {
        let dir = temp_dir("noacc");
        let c = Cache::new(dir.clone());
        c.put("", "liked", &json!(1));
        assert_eq!(c.get("", "liked"), None);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn key_mismatch_is_a_miss() {
        let dir = temp_dir("mismatch");
        let c = Cache::new(dir.clone());
        // a file at the right path whose stored key is another one (a hash clash)
        let body = json!({"key": "acc/other", "saved_at": 1, "value": 5});
        std::fs::write(dir.join(format!("{}.json", hash_hex("acc/liked"))), body.to_string()).unwrap();
        assert_eq!(c.get("acc", "liked"), None);
    }

    #[test]
    fn corrupt_file_is_a_miss() {
        let dir = temp_dir("corrupt");
        let c = Cache::new(dir.clone());
        std::fs::write(dir.join(format!("{}.json", hash_hex("acc/liked"))), "{not json").unwrap();
        assert_eq!(c.get("acc", "liked"), None);
    }

    #[test]
    fn missing_dir_is_harmless() {
        let c = Cache::new(temp_dir("gone").join("nested").join("deeper"));
        assert_eq!(c.get("acc", "k"), None);
        c.put("acc", "k", &json!(2));
        assert_eq!(c.get("acc", "k"), Some(json!(2)));
    }

    #[test]
    fn evicts_oldest_first_down_to_target() {
        let dir = temp_dir("evict");
        let mut c = Cache::new(dir.clone());
        c.max_bytes = 5000;
        c.target_bytes = 4000;
        // 5 files of 1000 bytes, mtimes 1..=5 (oldest = 1)
        for i in 1..=5u64 {
            let p = dir.join(format!("f{i}.json"));
            std::fs::write(&p, vec![b'x'; 1000]).unwrap();
            let f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(UNIX_EPOCH + Duration::from_secs(1_000_000 + i)).unwrap();
        }
        // a put of ~100 bytes takes the dir over 5000 → trim to ≤ 4000
        c.put("acc", "new", &json!("v"));
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(!left.contains(&"f1.json".to_string()), "oldest goes first: {left:?}");
        assert!(!left.contains(&"f2.json".to_string()), "still over target after f1: {left:?}");
        for keep in ["f3.json", "f4.json", "f5.json"] {
            assert!(left.contains(&keep.to_string()), "{keep} kept: {left:?}");
        }
        assert_eq!(c.get("acc", "new"), Some(json!("v")));
        let total: u64 = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum();
        assert!(total <= 4000);
    }

    #[test]
    fn no_eviction_under_cap() {
        let dir = temp_dir("undercap");
        let mut c = Cache::new(dir.clone());
        c.max_bytes = 5000;
        c.target_bytes = 4000;
        std::fs::write(dir.join("old.json"), vec![b'x'; 4500]).unwrap();
        c.put("acc", "k", &json!(1));
        assert!(dir.join("old.json").exists());
    }
}
