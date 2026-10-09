//! The app log: `<app dir>/logs/stylus.log`, rolled to `stylus.1.log` at 2 MB.
//! Gets librespot's own log lines (info and up: track loads, unavailable tracks, connection drops),
//! ours (warn and up), and lines the UI sends through `app_log` (play attempts and failures).

use std::io::Write;
use std::sync::Mutex;

const MAX_BYTES: u64 = 2 * 1024 * 1024;

struct FileLog {
    file: Mutex<Option<std::fs::File>>,
    path: std::path::PathBuf,
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
        if self.enabled(r.metadata()) {
            self.write(&format!("{} {:<5} {}: {}", stamp(), r.level(), r.target(), r.args()));
        }
    }

    fn flush(&self) {}
}

static LOGGER: std::sync::OnceLock<FileLog> = std::sync::OnceLock::new();

/// Start logging to the file. Called once, first thing in `run()`.
pub fn init() {
    let dir = crate::paths::app_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("stylus.log");
    let logger = LOGGER.get_or_init(|| FileLog { file: Mutex::new(FileLog::open(&path)), path });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    log::info!(target: "stylus", "--- start v{}", env!("CARGO_PKG_VERSION"));
    if let Some((level, note)) = crate::paths::app_dir_note() {
        log::log!(target: "stylus", *level, "app folder: {note}");
    }
}

/// A line from the UI (play attempts, failed commands).
#[tauri::command]
pub fn app_log(level: String, msg: String) {
    let level = if level == "error" { log::Level::Error } else if level == "warn" { log::Level::Warn } else { log::Level::Info };
    log::log!(target: "stylus::ui", level, "{msg}");
}

/// Local time is not worth a dependency: UTC date and time with millis.
fn stamp() -> String {
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
}
