//! App-wide helpers: the data folder (and its one-time migration), private file
//! writes, the clock and the shared HTTP client.

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix time, seconds.
pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Unix time, ms.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The app's data folder name under `~/Library/Application Support`.
const APP_DIR_NAME: &str = "stylus";
/// Older folder names, newest first: the app was Needle, and rust-spotify before that.
const OLD_APP_DIR_NAMES: [&str; 2] = ["needle", "rust-spotify"];

static APP_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
/// What the one-time folder migration did, for the log (the logger starts after it).
static APP_DIR_NOTE: std::sync::OnceLock<(log::Level, String)> = std::sync::OnceLock::new();

/// The app's data folder (`~/Library/Application Support/stylus`), created if missing.
/// The first call moves an old `needle` or `rust-spotify` folder there (see `resolve_app_dir`).
pub(crate) fn app_dir() -> std::path::PathBuf {
    let dir = APP_DIR.get_or_init(|| {
        // tests never touch the real folder
        let base = if cfg!(test) {
            std::env::temp_dir().join(format!("stylus-test-appdir-{}", std::process::id()))
        } else {
            dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
        };
        resolve_app_dir(&base)
    });
    let _ = std::fs::create_dir_all(dir);
    dir.clone()
}

/// The migration's log line, if it did anything. Taken once by `applog::init`.
pub(crate) fn app_dir_note() -> Option<&'static (log::Level, String)> {
    APP_DIR_NOTE.get()
}

#[derive(Debug, PartialEq)]
enum DirAction {
    /// Use the new folder (it exists, or there is nothing to move).
    UseNew,
    /// Move the old folder at this index of `OLD_APP_DIR_NAMES` to the new name.
    Migrate(usize),
}

/// Move only when the new folder doesn't exist: never merge, never overwrite.
/// The newest old folder that exists wins (`needle` before `rust-spotify`).
fn dir_action(new_exists: bool, old_exists: &[bool]) -> DirAction {
    if new_exists {
        return DirAction::UseNew;
    }
    match old_exists.iter().position(|&e| e) {
        Some(i) => DirAction::Migrate(i),
        None => DirAction::UseNew,
    }
}

/// The data folder under `base`. Moves `needle` (or, failing that, `rust-spotify`) to `stylus`
/// once (one atomic rename: tokens, player login, device id, cache, settings and logs move
/// together). A failed move keeps the old folder in use, so nothing is lost.
fn resolve_app_dir(base: &std::path::Path) -> std::path::PathBuf {
    let new = base.join(APP_DIR_NAME);
    let olds: Vec<std::path::PathBuf> = OLD_APP_DIR_NAMES.iter().map(|n| base.join(n)).collect();
    let old_exists: Vec<bool> = olds.iter().map(|o| o.is_dir()).collect();
    match dir_action(new.exists(), &old_exists) {
        DirAction::UseNew => new,
        DirAction::Migrate(i) => {
            let old = &olds[i];
            match std::fs::rename(old, &new) {
                Ok(()) => {
                    let _ = APP_DIR_NOTE.set((log::Level::Info, format!("moved {} to {}", old.display(), new.display())));
                    new
                }
                Err(e) => {
                    let note = format!("could not move {} to {}: {e}; using the old folder", old.display(), new.display());
                    let _ = APP_DIR_NOTE.set((log::Level::Warn, note));
                    old.clone()
                }
            }
        }
    }
}

/// Writes a secret file readable by this user only (0600), replacing it atomically.
pub(crate) fn write_private(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(data.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// One HTTP client for every Spotify call. Finite deadlines: a stalled request
/// must fail, or it would hold the TOKENS lock and freeze every command behind it.
pub fn http() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client")
        })
        .clone()
}

pub(crate) fn urlencode(s: &str) -> String {
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
    #[test]
    fn urlencode_basic() {
        assert_eq!(super::urlencode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn app_dir_moves_only_into_a_free_name() {
        use super::{dir_action, DirAction};
        // [needle, rust-spotify]
        assert_eq!(dir_action(false, &[true, false]), DirAction::Migrate(0));
        assert_eq!(dir_action(false, &[false, true]), DirAction::Migrate(1));
        assert_eq!(dir_action(false, &[true, true]), DirAction::Migrate(0));
        assert_eq!(dir_action(false, &[false, false]), DirAction::UseNew);
        assert_eq!(dir_action(true, &[true, true]), DirAction::UseNew);
        assert_eq!(dir_action(true, &[true, false]), DirAction::UseNew);
        assert_eq!(dir_action(true, &[false, false]), DirAction::UseNew);
    }

    fn migrate_base(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("stylus-migrate-{name}-{}-{}", std::process::id(), super::now()))
    }

    fn fill(dir: &std::path::Path) {
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        for f in ["tokens.json", "player-credentials.json", "session.json", "settings.json", "state.json"] {
            std::fs::write(dir.join(f), f).unwrap();
        }
        std::fs::write(dir.join("player-device-id"), "dev-1").unwrap();
        std::fs::write(dir.join("cache").join("x"), "c").unwrap();
        std::fs::write(dir.join("logs").join("needle.log"), "l").unwrap();
    }

    fn assert_filled(dir: &std::path::Path) {
        for f in ["tokens.json", "player-credentials.json", "session.json", "settings.json", "state.json"] {
            assert_eq!(std::fs::read_to_string(dir.join(f)).unwrap(), f);
        }
        assert_eq!(std::fs::read_to_string(dir.join("player-device-id")).unwrap(), "dev-1");
        assert_eq!(std::fs::read_to_string(dir.join("cache").join("x")).unwrap(), "c");
        assert_eq!(std::fs::read_to_string(dir.join("logs").join("needle.log")).unwrap(), "l");
    }

    #[test]
    fn app_dir_migrates_needle_and_leaves_rust_spotify() {
        let base = migrate_base("needle");
        let needle = base.join("needle");
        let older = base.join("rust-spotify");
        fill(&needle);
        std::fs::create_dir_all(&older).unwrap();
        let dir = super::resolve_app_dir(&base);
        assert_eq!(dir, base.join("stylus"));
        assert!(!needle.exists());
        assert!(older.exists());
        assert_filled(&dir);
        // stylus exists now: it wins, the leftovers are left alone
        std::fs::create_dir_all(&needle).unwrap();
        assert_eq!(super::resolve_app_dir(&base), base.join("stylus"));
        assert!(needle.exists());
    }

    #[test]
    fn app_dir_migrates_rust_spotify_when_needle_is_missing() {
        let base = migrate_base("rust-spotify");
        let older = base.join("rust-spotify");
        fill(&older);
        let dir = super::resolve_app_dir(&base);
        assert_eq!(dir, base.join("stylus"));
        assert!(!older.exists());
        assert_filled(&dir);
    }

    #[test]
    fn app_dir_without_old_folders_is_the_new_one() {
        let base = migrate_base("fresh");
        std::fs::create_dir_all(&base).unwrap();
        assert_eq!(super::resolve_app_dir(&base), base.join("stylus"));
    }
}
