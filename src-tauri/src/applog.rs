//! The app log: `<app dir>/logs/stylus.log`, rolled to `stylus.1.log` at 2 MB.
//! Gets librespot's own log lines (info and up: track loads, unavailable tracks, connection drops),
//! ours (warn and up), and lines the UI sends through `app_log` (play attempts and failures).

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

struct FileLog {
    file: Mutex<Option<std::fs::File>>,
    path: std::path::PathBuf,
    repeats: Mutex<Repeats>,
}

impl FileLog {
    fn open(path: &std::path::Path) -> Option<std::fs::File> {
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
    }

    fn write(&self, line: &str) {
        let Ok(mut guard) = self.file.lock() else { return };
        let too_big = guard.as_ref().and_then(|f| f.metadata().ok()).is_some_and(|m| m.len() > MAX_BYTES);
        if too_big {
            let _ = std::fs::rename(&self.path, self.path.with_file_name("stylus.1.log"));
            *guard = Self::open(&self.path);
        }
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        let ours = m.target().starts_with("librespot") || m.target().starts_with("stylus");
        m.level() <= if ours { log::Level::Info } else { log::Level::Warn }
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let (level, target, msg) = (r.level(), r.target(), r.args().to_string());
        if dropped(target, &msg) {
            return;
        }
        let mut suffix = String::new();
        if !target.starts_with("stylus") {
            let key = format!("{level} {target} {msg}");
            let Ok(mut repeats) = self.repeats.lock() else { return };
            match repeats.admit(&key, crate::paths::now_ms()) {
                Some(skipped) => suffix = repeat_suffix(skipped),
                None => return,
            }
        }
        self.write(&format!("{} {:<5} {}: {}{}", stamp(), level, target, msg, suffix));
    }

    fn flush(&self) {}
}

/// Known third-party noise that the log never gets (see the spec's drop table).
fn dropped(target: &str, msg: &str) -> bool {
    target.starts_with("symphonia")
        || (target == "librespot_audio::fetch::receive"
            && (msg.starts_with("Time to first byte") || msg.starts_with("Throughput")))
        || (target == "librespot_connect::state::context" && msg.starts_with("couldn't load context info"))
        || (target == "librespot_connect::spirc"
            && (msg.starts_with("SpircCommand::Activate will be ignored while already active")
                || msg.starts_with("failed filling up next_track during stopping")))
}

const REPEAT_WINDOW_MS: u64 = 60_000;
const REPEAT_KEYS_MAX: usize = 256;

struct Seen {
    written_ms: u64,
    skipped: u32,
}

/// One line a minute for a third-party line that repeats; the next write carries the count.
#[derive(Default)]
struct Repeats {
    seen: HashMap<String, Seen>,
}

impl Repeats {
    /// `Some(n)`: write now, `n` lines were skipped since the last write. `None`: skip.
    fn admit(&mut self, key: &str, now_ms: u64) -> Option<u32> {
        if let Some(s) = self.seen.get_mut(key) {
            if now_ms.saturating_sub(s.written_ms) < REPEAT_WINDOW_MS {
                s.skipped += 1;
                return None;
            }
            let skipped = s.skipped;
            *s = Seen { written_ms: now_ms, skipped: 0 };
            return Some(skipped);
        }
        if self.seen.len() >= REPEAT_KEYS_MAX {
            self.seen.clear();
        }
        self.seen.insert(key.to_string(), Seen { written_ms: now_ms, skipped: 0 });
        Some(0)
    }
}

fn repeat_suffix(skipped: u32) -> String {
    if skipped == 0 { String::new() } else { format!(" (+{skipped} same in 60 s)") }
}

static LOGGER: std::sync::OnceLock<FileLog> = std::sync::OnceLock::new();

/// Start logging to the file. Called once, first thing in `run()`.
pub fn init() {
    let dir = crate::paths::app_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("stylus.log");
    let logger = LOGGER.get_or_init(|| FileLog { file: Mutex::new(FileLog::open(&path)), path, repeats: Mutex::default() });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    log::info!(target: "stylus", "--- start v{}", env!("CARGO_PKG_VERSION"));
    if let Some((level, note)) = crate::paths::app_dir_note() {
        log::log!(target: "stylus", *level, "app folder: {note}");
    }
}

/// Target of every auth-flow line (Rust and UI).
pub(crate) const AUTH: &str = "stylus::auth";

/// A line from the UI (play attempts, failed commands). `area: "auth"` sends it to the auth flow.
#[tauri::command]
pub fn app_log(level: String, msg: String, area: Option<String>) {
    let level = if level == "error" { log::Level::Error } else if level == "warn" { log::Level::Warn } else { log::Level::Info };
    let target = ui_target(area.as_deref());
    if target == AUTH {
        log::log!(target: AUTH, level, "ui: {msg}");
    } else {
        log::log!(target: "stylus::ui", level, "{msg}");
    }
}

fn ui_target(area: Option<&str>) -> &'static str {
    if area == Some("auth") { AUTH } else { "stylus::ui" }
}

/// Local time is not worth a dependency: UTC date and time with millis.
pub(crate) fn stamp() -> String {
    format_stamp(crate::paths::now_ms())
}

/// `2026-10-09 14:03:16.243Z` from Unix millis.
fn format_stamp(ms: u64) -> String {
    let s = ms / 1000;
    let (y, m, d) = civil_from_days((s / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}Z", s / 3600 % 24, s / 60 % 60, s % 60, ms % 1000)
}

/// Year, month, day from days since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
        assert_eq!(civil_from_days(20735), (2026, 10, 9));
        assert_eq!(civil_from_days(47541), (2100, 3, 1));
    }

    #[test]
    fn stamp_has_the_date() {
        assert_eq!(format_stamp(1_791_554_596_243), "2026-10-09 14:03:16.243Z");
    }

    #[test]
    fn drop_table() {
        assert!(dropped("symphonia_bundle_mp3::demuxer", "skipping junk at 1234 bytes"));
        assert!(dropped("symphonia_core::probe", "anything"));
        assert!(dropped("librespot_audio::fetch::receive", "Time to first byte: 120 ms"));
        assert!(dropped("librespot_audio::fetch::receive", "Throughput 1.2 MB/s"));
        assert!(dropped("librespot_connect::state::context", "couldn't load context info because: x"));
        assert!(dropped("librespot_connect::spirc", "SpircCommand::Activate will be ignored while already active"));
        assert!(dropped("librespot_connect::spirc", "failed filling up next_track during stopping: x"));
        assert!(!dropped("librespot_core::dealer", "Websocket peer does not respond."));
        assert!(!dropped("stylus::ui", "pause local: ok"));
        assert!(!dropped("librespot_audio::fetch::receive", "other text"));
    }

    #[test]
    fn repeat_limit_one_line_a_minute() {
        let mut r = Repeats::default();
        assert_eq!(r.admit("a", 0), Some(0));
        assert_eq!(r.admit("a", 1_000), None);
        assert_eq!(r.admit("a", 2_000), None);
        assert_eq!(r.admit("b", 2_000), Some(0), "the first key has no effect on a different key");
        assert_eq!(r.admit("a", 61_000), Some(2));
        assert_eq!(r.admit("a", 62_000), None);
    }

    #[test]
    fn repeat_limit_clears_at_256_keys() {
        let mut r = Repeats::default();
        for i in 0..REPEAT_KEYS_MAX {
            assert_eq!(r.admit(&format!("k{i}"), 0), Some(0));
        }
        assert_eq!(r.admit("k0", 1_000), None);
        assert_eq!(r.admit("new", 1_000), Some(0));
        assert_eq!(r.seen.len(), 1);
        assert_eq!(r.admit("k0", 1_000), Some(0));
    }

    #[test]
    fn ui_area_to_target() {
        assert_eq!(ui_target(Some("auth")), AUTH);
        assert_eq!(ui_target(None), "stylus::ui");
        assert_eq!(ui_target(Some("x")), "stylus::ui");
    }

    #[test]
    fn repeat_suffixes() {
        assert_eq!(repeat_suffix(0), "");
        assert_eq!(repeat_suffix(5), " (+5 same in 60 s)");
    }
}
