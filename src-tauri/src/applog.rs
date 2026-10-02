//! The app log: `<app dir>/logs/needle.log`, rolled to `needle.1.log` at 2 MB.
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
            let _ = std::fs::rename(&self.path, self.path.with_file_name("needle.1.log"));
            *guard = Self::open(&self.path);
        }
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

impl log::Log for FileLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        let ours = m.target().starts_with("librespot") || m.target().starts_with("needle");
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
    let dir = crate::auth::app_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("needle.log");
    let logger = LOGGER.get_or_init(|| FileLog { file: Mutex::new(FileLog::open(&path)), path });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    log::info!(target: "needle", "--- start v{}", env!("CARGO_PKG_VERSION"));
    if let Some((level, note)) = crate::auth::app_dir_note() {
        log::log!(target: "needle", *level, "app folder: {note}");
    }
}

/// A line from the UI (play attempts, failed commands).
#[tauri::command]
pub fn app_log(level: String, msg: String) {
    let level = if level == "error" { log::Level::Error } else if level == "warn" { log::Level::Warn } else { log::Level::Info };
    log::log!(target: "needle::ui", level, "{msg}");
}

/// Local time is not worth a dependency: UTC seconds with millis.
fn stamp() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600 % 24, s / 60 % 60, s % 60, d.subsec_millis())
}
