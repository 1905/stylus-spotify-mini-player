//! Silent updates from GitHub Releases (plans/2026-10-24-fixes-autoupdate), neo-md's behaviour on
//! tauri-plugin-updater: a check 10 s after launch, then every 6 h; a newer signed
//! `Stylus.app.tar.gz` downloads into memory and installs when Stylus quits (`RunEvent::Exit`).
//! No dialogs: the status shows in Settings (`update_status`, event `update-status`).
//!
//! The setting is the store key `settings`, field `autoUpdate` (missing = on). With it off, a
//! check only reports a newer version; nothing downloads or installs.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

const LOG: &str = "stylus::update";
pub const EVENT: &str = "update-status";
/// The first check after launch.
const FIRST_CHECK: Duration = Duration::from_secs(10);
/// The timer's checks.
const INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// Nothing but Check now checks sooner than this after the last check.
const MIN_GAP: Duration = Duration::from_secs(3600);
/// How often the timer looks at the clock. Wall time, not a 6 h sleep: a sleeping Mac stops
/// monotonic clocks, so a 6 h sleep could last days.
const TICK: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Available(String),
    Downloading(String),
    Staged(String),
    Failed(String),
    NotInstallable,
}

impl Status {
    fn name(&self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Checking => "checking",
            Status::UpToDate => "upToDate",
            Status::Available(_) => "available",
            Status::Downloading(_) => "downloading",
            Status::Staged(_) => "staged",
            Status::Failed(_) => "failed",
            Status::NotInstallable => "notInstallable",
        }
    }

    fn version(&self) -> Option<&str> {
        match self {
            Status::Available(v) | Status::Downloading(v) | Status::Staged(v) => Some(v),
            _ => None,
        }
    }

    fn message(&self) -> Option<&str> {
        match self {
            Status::Failed(m) => Some(m),
            _ => None,
        }
    }
}

struct State {
    status: Status,
    /// The verified update and its archive, waiting for the quit.
    staged: Option<(Update, Vec<u8>)>,
    /// A check runs (feed, download).
    busy: bool,
    /// When the last check started.
    last_check: Option<SystemTime>,
    /// The setting as the UI last sent it (`update_set_auto`); None = read the store.
    auto: Option<bool>,
}

static STATE: Mutex<State> =
    Mutex::new(State { status: Status::Idle, staged: None, busy: false, last_check: None, auto: None });

fn state() -> std::sync::MutexGuard<'static, State> {
    crate::nowplaying::lock(&STATE)
}

// ---------- pure parts ----------

/// The `.app` that holds `exe` (`<bundle>.app/Contents/MacOS/<exe>`); None for a bare binary.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents").then(|| bundle.to_path_buf())
}

/// The bundle may replace itself: an `.app` directly in a folder named Applications
/// (`/Applications` or `~/Applications`), not on a disk image, not translocated.
pub fn installable_path(bundle: &Path) -> bool {
    let s = bundle.to_string_lossy();
    bundle.extension().is_some_and(|e| e == "app")
        && bundle.parent().and_then(Path::file_name).is_some_and(|n| n == "Applications")
        && !s.contains("/AppTranslocation/")
        && !s.starts_with("/Volumes/")
}

/// The UI setting from the stored `settings` object: on unless it says false.
pub fn auto_from(settings: &Value) -> bool {
    settings["autoUpdate"].as_bool() != Some(false)
}

/// A check is due: none ran yet, or the last one started at least `gap` ago (a clock that went
/// back counts as due).
pub fn due(last: Option<SystemTime>, now: SystemTime, gap: Duration) -> bool {
    last.is_none_or(|t| now.duration_since(t).map_or(true, |d| d >= gap))
}

/// What a check does with a newer version `v` on the feed.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Report(Status),
    Download,
}

pub fn next_step(v: &str, staged: Option<&str>, auto: bool) -> Next {
    if staged == Some(v) {
        Next::Report(shown(v, auto))
    } else if auto {
        Next::Download
    } else {
        Next::Report(Status::Available(v.to_string()))
    }
}

/// A staged `v` as Settings shows it: it installs at the quit only while the setting is on.
fn shown(v: &str, auto: bool) -> Status {
    if auto {
        Status::Staged(v.to_string())
    } else {
        Status::Available(v.to_string())
    }
}

/// The `update_status` answer and the `update-status` payload.
pub fn status_json(status: &Status, current: &str, auto: bool) -> Value {
    json!({
        "status": status.name(),
        "version": status.version(),
        "message": status.message(),
        "current": current,
        "auto": auto,
    })
}

// ---------- state ----------

fn auto_on() -> bool {
    let set = state().auto;
    set.unwrap_or_else(|| auto_from(&crate::store::get("settings").unwrap_or(Value::Null)))
}

fn writable(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    // SAFETY: c is a NUL-terminated path that lives for the call
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// This copy can update itself: an installed bundle whose folder and itself are writable (the
/// plugin swaps the bundle with renames; a read-only one would need an admin prompt).
fn installable() -> bool {
    let Some(bundle) = tauri::utils::platform::current_exe().ok().as_deref().and_then(bundle_of) else {
        return false;
    };
    installable_path(&bundle) && bundle.parent().is_some_and(writable) && writable(&bundle)
}

fn staged_version() -> Option<String> {
    state().staged.as_ref().map(|(u, _)| u.version.clone())
}

fn current(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

fn publish(app: &AppHandle) {
    let status = state().status.clone();
    let _ = app.emit(EVENT, status_json(&status, &current(app), auto_on()));
}

/// Sets the status; a change is logged once and sent to the UI.
fn set(app: &AppHandle, status: Status) {
    {
        let mut s = state();
        if s.status == status {
            return;
        }
        s.status = status.clone();
    }
    match (&status, status.version()) {
        (Status::Failed(m), _) => log::warn!(target: LOG, "status failed: {m}"),
        (_, Some(v)) => log::info!(target: LOG, "status {} {v}", status.name()),
        _ => log::info!(target: LOG, "status {}", status.name()),
    }
    publish(app);
}

// ---------- check, download ----------

/// Starts the timer. Runs in `setup`. A copy that can't update itself never checks.
pub fn start(app: AppHandle) {
    if !installable() {
        set(&app, Status::NotInstallable);
        return;
    }
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        check(app.clone(), MIN_GAP, false).await;
        loop {
            tokio::time::sleep(TICK).await;
            check(app.clone(), INTERVAL, false).await;
        }
    });
}

/// One check, unless one runs, this copy can't update, or (not `force`) the setting is off or
/// the last check is less than `gap` ago. `force` = Check now.
async fn check(app: AppHandle, gap: Duration, force: bool) {
    let auto = auto_on();
    {
        let mut s = state();
        if s.busy || s.status == Status::NotInstallable {
            return;
        }
        if !force && (!auto || !due(s.last_check, SystemTime::now(), gap)) {
            return;
        }
        s.busy = true;
        s.last_check = Some(SystemTime::now());
    }
    set(&app, Status::Checking);
    let status = match run_check(&app, auto).await {
        Ok(status) => status,
        Err(e) => match staged_version() {
            // a failed re-check (offline, say) leaves the staged update as it was
            Some(v) => {
                log::warn!(target: LOG, "check failed, {v} stays staged: {e}");
                shown(&v, auto_on())
            }
            None => Status::Failed(e),
        },
    };
    set(&app, status);
    state().busy = false;
}

/// Feed, then (setting on) download and verify. The verified archive is staged.
async fn run_check(app: &AppHandle, auto: bool) -> Result<Status, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(Status::UpToDate);
    };
    let v = update.version.clone();
    match next_step(&v, staged_version().as_deref(), auto) {
        Next::Report(status) => return Ok(status),
        Next::Download => {}
    }
    set(app, Status::Downloading(v.clone()));
    // download() checks the signature against plugins.updater.pubkey: a bad one stages nothing
    let bytes = update.download(|_, _| {}, || {}).await.map_err(|e| e.to_string())?;
    log::info!(target: LOG, "downloaded {v}: {} bytes, signature ok", bytes.len());
    state().staged = Some((update, bytes));
    // the setting may have changed during the download
    Ok(shown(&v, auto_on()))
}

/// Installs the staged update. Runs on `RunEvent::Exit`, while the setting is on. Errors are
/// logged; the old bundle stays and the next launch checks again.
pub fn install_at_exit() {
    if !auto_on() {
        return;
    }
    let Some((update, bytes)) = state().staged.take() else { return };
    match update.install(&bytes) {
        Ok(()) => log::info!(target: LOG, "installed {} at quit", update.version),
        Err(e) => log::warn!(target: LOG, "install of {} at quit failed: {e}", update.version),
    }
}

// ---------- commands ----------

#[tauri::command]
pub fn update_status(app: AppHandle) -> Value {
    let status = state().status.clone();
    status_json(&status, &current(&app), auto_on())
}

/// Settings → Check now: ignores the 1 h gap. With the setting off it only reports.
#[tauri::command]
pub fn update_check_now(app: AppHandle) {
    tauri::async_runtime::spawn(check(app, MIN_GAP, true));
}

/// The setting changed (the UI writes the store itself). On: a staged update installs at the
/// quit again, else a check runs if one is due. Off: nothing downloads or installs.
#[tauri::command]
pub fn update_set_auto(app: AppHandle, on: bool) {
    state().auto = Some(on);
    let staged = staged_version();
    log::info!(target: LOG, "automatic updates {}", if on { "on" } else { "off" });
    match staged {
        Some(v) if !state().busy => set(&app, shown(&v, on)),
        None if on => {
            tauri::async_runtime::spawn(check(app.clone(), MIN_GAP, false));
        }
        _ => {}
    }
    publish(&app);
}

/// Settings → Restart now: installs the staged update, then relaunches. The relaunch goes
/// through `RunEvent::Exit` (async command = off the main thread), so the engine shuts down.
#[tauri::command]
pub async fn update_restart(app: AppHandle) -> Result<(), String> {
    let Some((update, bytes)) = state().staged.take() else {
        return Err("no update is staged".into());
    };
    let v = update.version.clone();
    let installed = tauri::async_runtime::spawn_blocking(move || update.install(&bytes))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    if let Err(e) = installed {
        set(&app, Status::Failed(e.clone()));
        return Err(e);
    }
    log::info!(target: LOG, "installed {v}, restarting");
    app.request_restart();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_from_the_executable() {
        let b = bundle_of(Path::new("/Applications/Stylus.app/Contents/MacOS/stylus"));
        assert_eq!(b, Some(PathBuf::from("/Applications/Stylus.app")));
        // make run: a bare binary
        assert_eq!(bundle_of(Path::new("/tmp/stylus-build/stylus")), None);
        assert_eq!(bundle_of(Path::new("/x/Stylus.app/Contents/Resources/stylus")), None);
        assert_eq!(bundle_of(Path::new("stylus")), None);
    }

    #[test]
    fn installable_only_in_applications() {
        assert!(installable_path(Path::new("/Applications/Stylus.app")));
        assert!(installable_path(Path::new("/Users/kass/Applications/Stylus.app")));
        // build output, a disk image, a translocated copy, not a bundle
        assert!(!installable_path(Path::new("/Users/kass/dev/target/release/bundle/macos/Stylus.app")));
        assert!(!installable_path(Path::new("/Volumes/Stylus/Stylus.app")));
        assert!(!installable_path(Path::new("/Volumes/Data/Applications/Stylus.app")));
        assert!(!installable_path(Path::new(
            "/private/var/folders/x/AppTranslocation/1F2E/d/Applications/Stylus.app"
        )));
        assert!(!installable_path(Path::new("/Applications/stylus")));
        assert!(!installable_path(Path::new("/Applications/Utilities/Stylus.app")));
    }

    #[test]
    fn setting_defaults_to_on() {
        assert!(auto_from(&Value::Null));
        assert!(auto_from(&json!({})));
        assert!(auto_from(&json!({"autoUpdate": true})));
        assert!(!auto_from(&json!({"autoUpdate": false})));
        // a broken value is the default, like the UI's parseSettings
        assert!(auto_from(&json!({"autoUpdate": "no"})));
    }

    #[test]
    fn due_after_the_gap() {
        let now = SystemTime::now();
        assert!(due(None, now, MIN_GAP));
        assert!(!due(Some(now), now, MIN_GAP));
        assert!(!due(Some(now - Duration::from_secs(3599)), now, MIN_GAP));
        assert!(due(Some(now - MIN_GAP), now, MIN_GAP));
        assert!(!due(Some(now - Duration::from_secs(5 * 3600)), now, INTERVAL));
        assert!(due(Some(now - INTERVAL), now, INTERVAL));
        // the clock went back
        assert!(due(Some(now + Duration::from_secs(60)), now, MIN_GAP));
    }

    #[test]
    fn found_update_transitions() {
        let s = |v: &str| v.to_string();
        // setting on: download a new version; a staged one shows as staged
        assert_eq!(next_step("0.4.1", None, true), Next::Download);
        assert_eq!(next_step("0.4.1", Some("0.4.1"), true), Next::Report(Status::Staged(s("0.4.1"))));
        // a newer one than the staged: download again
        assert_eq!(next_step("0.4.2", Some("0.4.1"), true), Next::Download);
        // setting off: only report
        assert_eq!(next_step("0.4.1", None, false), Next::Report(Status::Available(s("0.4.1"))));
        assert_eq!(next_step("0.4.1", Some("0.4.1"), false), Next::Report(Status::Available(s("0.4.1"))));
    }

    #[test]
    fn status_payload() {
        assert_eq!(
            status_json(&Status::Staged("0.4.1".into()), "0.4.0", true),
            json!({"status": "staged", "version": "0.4.1", "message": null, "current": "0.4.0", "auto": true})
        );
        assert_eq!(
            status_json(&Status::Failed("HTTP 404".into()), "0.4.0", false),
            json!({"status": "failed", "version": null, "message": "HTTP 404", "current": "0.4.0", "auto": false})
        );
        let names: Vec<_> = [
            Status::Idle,
            Status::Checking,
            Status::UpToDate,
            Status::Available("1".into()),
            Status::Downloading("1".into()),
            Status::Staged("1".into()),
            Status::Failed("x".into()),
            Status::NotInstallable,
        ]
        .iter()
        .map(|s| status_json(s, "0", true)["status"].as_str().unwrap().to_string())
        .collect();
        assert_eq!(
            names,
            ["idle", "checking", "upToDate", "available", "downloading", "staged", "failed", "notInstallable"]
        );
        assert_eq!(status_json(&Status::Downloading("0.4.1".into()), "0.4.0", true)["version"], "0.4.1");
    }
}
