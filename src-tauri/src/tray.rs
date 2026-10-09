//! The menu-bar icon and its mini player. The main webview stays the only brain: it pushes a
//! compact now-playing payload here (`mini_push`), Rust relays it to the popover (`mini-state`),
//! the right-click menu and the menu-bar title. Popover and menu buttons go back to the main
//! webview as `mini-command`, which runs the main window's own handlers (routing, spinners).
//!
//! Payload (src/lib/mini.js miniPayload): `{mode, title, artist, cover, status, playing, pending,
//! skipping, loading, positionMs, durationMs, sentAt, volume, heart}`.

use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const MINI: &str = "mini";
const TRAY_ID: &str = "stylus";
/// The popover, logical px.
const MINI_W: f64 = 320.0;
const MINI_H: f64 = 140.0;
/// Between the menu bar and the popover, logical px.
const GAP: f64 = 6.0;
/// A click this soon after the popover hid was the click that hid it (the blur came first).
const REOPEN_GUARD: Duration = Duration::from_millis(250);
/// The song in the menu bar, chars.
const TITLE_MAX: usize = 24;
/// What the popover and the menu may ask the main webview for.
const ACTIONS: [&str; 6] = ["toggle", "next", "previous", "volume", "mute", "heart"];
/// The "Log Out" item in the app menu and the tray menu; the main webview runs the logout.
const LOGOUT: &str = "logout";
/// File → Show Anonymized Logs in Finder (logshare.rs).
const SHARE_LOGS: &str = "share_logs";
pub const LOGOUT_EVENT: &str = "logout-requested";

struct Tray {
    /// The last payload from the main webview (None until the first push).
    state: Option<Value>,
    /// "Show song in menu bar".
    show_title: bool,
    /// When the popover last hid.
    hidden_at: Option<Instant>,
}

static TRAY: Mutex<Tray> = Mutex::new(Tray { state: None, show_title: false, hidden_at: None });

fn tray() -> std::sync::MutexGuard<'static, Tray> {
    crate::nowplaying::lock(&TRAY)
}

/// The right-click menu's items whose text or state follows the music.
struct MenuItems<R: Runtime> {
    toggle: MenuItem<R>,
    next: MenuItem<R>,
    previous: MenuItem<R>,
}

// ---------- pure parts ----------

/// A rectangle in physical px: x, y (top left), width, height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Where the popover's top left goes: centred under the icon, `gap` below it, kept inside `screen`
/// (the monitor's work area). All physical px.
pub fn anchor(icon: Area, win: (f64, f64), screen: Area, gap: f64) -> (f64, f64) {
    let (w, h) = win;
    let max_x = (screen.x + screen.w - w).max(screen.x);
    let x = (icon.x + icon.w / 2.0 - w / 2.0).clamp(screen.x, max_x);
    let max_y = (screen.y + screen.h - h).max(screen.y);
    let y = (icon.y + icon.h + gap).clamp(screen.y, max_y);
    (x.round(), y.round())
}

/// The song for the menu bar: trimmed, at most `max` chars with "…" when cut; None when empty.
pub fn menu_title(title: Option<&str>, max: usize) -> Option<String> {
    let t = title.map(str::trim).filter(|t| !t.is_empty())?;
    if t.chars().count() <= max {
        return Some(t.to_string());
    }
    let cut: String = t.chars().take(max.saturating_sub(1)).collect();
    Some(format!("{}…", cut.trim_end()))
}

/// A left click may open the popover: it isn't the click whose blur just hid it.
pub fn reopen_ok(hidden_at: Option<Instant>, now: Instant) -> bool {
    hidden_at.is_none_or(|t| now.saturating_duration_since(t) >= REOPEN_GUARD)
}

/// The menu's play item text, and whether play/pause and next/previous do anything.
#[derive(Debug, PartialEq, Eq)]
pub struct Labels {
    pub toggle: &'static str,
    pub toggle_on: bool,
    pub skip_on: bool,
}

pub fn menu_labels(state: Option<&Value>) -> Labels {
    let s = state.cloned().unwrap_or(Value::Null);
    let mode = s["mode"].as_str().unwrap_or("idle");
    let playing = s["playing"].as_bool().unwrap_or(false);
    Labels { toggle: if playing { "Pause" } else { "Play" }, toggle_on: mode != "idle", skip_on: mode == "track" }
}

/// The text the menu bar shows next to the icon (None = the icon alone).
fn title_for(state: Option<&Value>, show: bool) -> Option<String> {
    let s = state?;
    if !show || s["mode"] != "track" {
        return None;
    }
    menu_title(s["title"].as_str(), TITLE_MAX)
}

// ---------- setup ----------

/// The stored UI settings: (show the icon, show the song). The UI's defaults: on, off.
fn stored_flags() -> (bool, bool) {
    let s = crate::store::get("settings").unwrap_or(Value::Null);
    (s["menuBar"].as_bool() != Some(false), s["menuBarTitle"].as_bool() == Some(true))
}

/// Builds the menu-bar icon and its menu. Runs in `setup`. The popover is built on the first
/// click (`toggle_mini`): none while the icon is off or never clicked.
pub fn init<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Play", false, None::<&str>)?;
    let next = MenuItem::with_id(app, "next", "Next", false, None::<&str>)?;
    let previous = MenuItem::with_id(app, "previous", "Previous", false, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show Stylus", true, None::<&str>)?;
    let logout = MenuItem::with_id(app, LOGOUT, "Log Out", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Stylus", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&toggle, &next, &previous, &sep, &show, &sep2, &logout, &quit])?;
    app.manage(MenuItems { toggle, next, previous });

    let (visible, show_title) = stored_flags();
    tray().show_title = show_title;
    let icon = TrayIconBuilder::with_id(TRAY_ID)
        .icon(tauri::include_image!("icons/tray-template.png"))
        .icon_as_template(true)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|icon, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
                toggle_mini(icon, rect);
            }
        })
        .build(app)?;
    icon.set_visible(visible)?;
    Ok(())
}

/// The macOS app menu with "Log Out" above Quit, and the one handler for every "Log Out" item
/// (Tauri gives each menu event, the tray menu's too, to the app's menu handlers). Runs in `setup`.
pub fn init_app_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    app.on_menu_event(|app, event| match event.id().as_ref() {
        LOGOUT => {
            log::info!(target: crate::applog::AUTH, "logout: requested from the menu");
            show_main(app);
            let _ = app.emit(LOGOUT_EVENT, ());
        }
        SHARE_LOGS => {
            let app = app.clone();
            // reads and masks up to 4 MB of log: not on the main thread
            std::thread::spawn(move || {
                let shown = crate::logshare::export(&app)
                    .and_then(|path| tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string()));
                if let Err(e) = shown {
                    log::warn!(target: "stylus::logs", "anonymized log: {e}");
                    let _ = app.emit("toast", format!("Couldn't save the anonymized log: {e}"));
                }
            });
        }
        _ => {}
    });
    let menu = Menu::default(app)?;
    let submenus: Vec<_> = menu
        .items()?
        .into_iter()
        .filter_map(|item| match item {
            tauri::menu::MenuItemKind::Submenu(s) => Some(s),
            _ => None,
        })
        .collect();
    // the first submenu is the app menu: About, Services, Hide…, then Quit last
    if let Some(app_menu) = submenus.first() {
        let quit_at = app_menu.items()?.len().saturating_sub(1);
        let logout = MenuItem::with_id(app, LOGOUT, "Log Out", true, None::<&str>)?;
        app_menu.insert_items(&[&logout, &PredefinedMenuItem::separator(app)?], quit_at)?;
    }
    match submenus.iter().find(|s| s.text().is_ok_and(|t| t == "File")) {
        Some(file) => file.append(&MenuItem::with_id(app, SHARE_LOGS, "Show Anonymized Logs in Finder", true, None::<&str>)?)?,
        None => log::warn!(target: "stylus::logs", "no File menu: Show Anonymized Logs in Finder not added"),
    }
    app.set_menu(menu)?;
    Ok(())
}

fn build_mini<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    use tauri::window::{Effect, EffectState, EffectsBuilder};
    // dark vibrancy (HUD: dark in light mode too, like the app) under mini.css's translucent panel; the radius rounds both
    let effects = EffectsBuilder::new().effect(Effect::HudWindow).state(EffectState::Active).radius(12.0).build();
    WebviewWindowBuilder::new(app, MINI, WebviewUrl::App("mini.html".into()))
        .title("Stylus")
        .inner_size(MINI_W, MINI_H)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .effects(effects)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .accept_first_mouse(true)
        .focused(false)
        .visible(false)
        .build()
}

// ---------- events ----------

fn on_menu<R: Runtime>(app: &AppHandle<R>, id: &str) {
    match id {
        "toggle" | "next" | "previous" => send_command(app, id, None),
        "show" => show_main(app),
        // the Cmd+Q path: RunEvent::Exit shuts the engine down (lib.rs)
        "quit" => app.exit(0),
        _ => {}
    }
}

/// Shows and focuses the main window.
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    hide_mini(app);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn toggle_mini<R: Runtime>(icon: &TrayIcon<R>, rect: tauri::Rect) {
    let app = icon.app_handle();
    let win = match app.get_webview_window(MINI) {
        Some(w) => w,
        // the first click: the popover loads hidden and asks for the state (`mini_get`)
        None => match build_mini(app) {
            Ok(w) => w,
            Err(e) => {
                log::warn!(target: "stylus::tray", "mini player not built: {e}");
                return;
            }
        },
    };
    if win.is_visible().unwrap_or(false) {
        hide_mini(app);
        return;
    }
    if !reopen_ok(tray().hidden_at, Instant::now()) {
        return;
    }
    // the icon's rect comes in physical px; the monitor under it gives the scale and the bounds
    let scale = win.scale_factor().unwrap_or(1.0);
    let pos = rect.position.to_physical::<f64>(scale);
    let size = rect.size.to_physical::<f64>(scale);
    let monitor = app.monitor_from_point(pos.x + size.width / 2.0, pos.y + size.height / 2.0).ok().flatten();
    let scale = monitor.as_ref().map_or(scale, |m| m.scale_factor());
    let screen = monitor.map_or(Area { x: 0.0, y: 0.0, w: f64::MAX / 4.0, h: f64::MAX / 4.0 }, |m| {
        let a = m.work_area();
        Area { x: a.position.x.into(), y: a.position.y.into(), w: a.size.width.into(), h: a.size.height.into() }
    });
    let icon_area = Area { x: pos.x, y: pos.y, w: size.width, h: size.height };
    let (x, y) = anchor(icon_area, (MINI_W * scale, MINI_H * scale), screen, GAP * scale);
    let _ = win.set_position(PhysicalPosition::new(x, y));
    let _ = win.show();
    let _ = win.set_focus();
    let _ = app.emit_to("main", "mini-visible", json!({ "open": true }));
}

/// Hides the popover (blur, Esc, a click on the icon, Show Stylus).
pub fn hide_mini<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = app.get_webview_window(MINI) else { return };
    if !win.is_visible().unwrap_or(false) {
        return;
    }
    tray().hidden_at = Some(Instant::now());
    let _ = win.hide();
    let _ = app.emit_to("main", "mini-visible", json!({ "open": false }));
}

fn send_command<R: Runtime>(app: &AppHandle<R>, action: &str, value: Option<f64>) {
    let _ = app.emit_to("main", "mini-command", json!({ "action": action, "value": value }));
}

/// The menu and the menu-bar title for `state`.
fn apply<R: Runtime>(app: &AppHandle<R>, state: Option<&Value>, show_title: bool) {
    if let Some(items) = app.try_state::<MenuItems<R>>() {
        let l = menu_labels(state);
        let _ = items.toggle.set_text(l.toggle);
        let _ = items.toggle.set_enabled(l.toggle_on);
        let _ = items.next.set_enabled(l.skip_on);
        let _ = items.previous.set_enabled(l.skip_on);
    }
    if let Some(icon) = app.tray_by_id(TRAY_ID) {
        let _ = icon.set_title(title_for(state, show_title));
    }
}

// ---------- commands ----------

/// The main webview's now-playing payload: kept, sent to the popover, shown in the menu.
#[tauri::command]
pub fn mini_push(app: AppHandle, state: Value) {
    let show_title = {
        let mut t = tray();
        t.state = Some(state.clone());
        t.show_title
    };
    apply(&app, Some(&state), show_title);
    let _ = app.emit_to(MINI, "mini-state", state);
}

/// The last payload, for the popover when it loads.
#[tauri::command]
pub fn mini_get() -> Option<Value> {
    tray().state.clone()
}

/// A popover button: `show` opens the main window, the rest go to the main webview.
#[tauri::command]
pub fn mini_command(app: AppHandle, action: String, value: Option<f64>) -> Result<(), String> {
    if action == "show" {
        show_main(&app);
        return Ok(());
    }
    if !ACTIONS.contains(&action.as_str()) {
        return Err(format!("BAD_ARGS: unknown action {action}"));
    }
    send_command(&app, &action, value.filter(|v| v.is_finite()));
    Ok(())
}

#[tauri::command]
pub fn mini_hide(app: AppHandle) {
    hide_mini(&app);
}

/// The two settings: the icon in the menu bar, the song next to it.
#[tauri::command]
pub fn tray_config(app: AppHandle, show: bool, title: bool) -> Result<(), String> {
    let state = {
        let mut t = tray();
        t.show_title = title;
        t.state.clone()
    };
    if let Some(icon) = app.tray_by_id(TRAY_ID) {
        icon.set_visible(show).map_err(|e| e.to_string())?;
    }
    if !show {
        hide_mini(&app);
    }
    apply(&app, state.as_ref(), title);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Area = Area { x: 0.0, y: 50.0, w: 2880.0, h: 1700.0 };

    #[test]
    fn anchor_centres_under_the_icon() {
        let icon = Area { x: 2000.0, y: 0.0, w: 44.0, h: 48.0 };
        assert_eq!(anchor(icon, (640.0, 296.0), SCREEN, 12.0), (1702.0, 60.0));
    }

    #[test]
    fn anchor_stays_on_screen() {
        // an icon near the right edge: the popover's right edge meets the screen's
        let icon = Area { x: 2850.0, y: 0.0, w: 30.0, h: 48.0 };
        assert_eq!(anchor(icon, (640.0, 296.0), SCREEN, 12.0), (2240.0, 60.0));
        // near the left edge (a second monitor at a negative x)
        let left = Area { x: -1440.0, y: 0.0, w: 1440.0, h: 900.0 };
        let icon = Area { x: -1430.0, y: 0.0, w: 22.0, h: 24.0 };
        assert_eq!(anchor(icon, (320.0, 148.0), left, 6.0), (-1440.0, 30.0));
        // a popover bigger than the screen sticks to its top left
        let tiny = Area { x: 0.0, y: 0.0, w: 200.0, h: 100.0 };
        assert_eq!(anchor(Area { x: 90.0, y: 0.0, w: 20.0, h: 20.0 }, (320.0, 148.0), tiny, 6.0), (0.0, 0.0));
    }

    #[test]
    fn titles_are_cut_with_an_ellipsis() {
        assert_eq!(menu_title(Some("Intro"), 24), Some("Intro".into()));
        assert_eq!(menu_title(Some("  Intro  "), 24), Some("Intro".into()));
        assert_eq!(menu_title(Some(""), 24), None);
        assert_eq!(menu_title(Some("   "), 24), None);
        assert_eq!(menu_title(None, 24), None);
        let long = "Everything In Its Right Place";
        let cut = menu_title(Some(long), 24).unwrap();
        assert_eq!(cut, "Everything In Its Right…");
        assert_eq!(cut.chars().count(), 24);
        // exactly at the limit: whole
        assert_eq!(menu_title(Some(&"a".repeat(24)), 24), Some("a".repeat(24)));
        // chars, not bytes; no space before the ellipsis
        assert_eq!(menu_title(Some("Ölümüne Ölümüne Ölümüne Ölümüne"), 9), Some("Ölümüne…".into()));
    }

    #[test]
    fn reopen_waits_out_the_blur() {
        let now = Instant::now();
        assert!(reopen_ok(None, now));
        assert!(!reopen_ok(Some(now), now));
        assert!(!reopen_ok(Some(now), now + Duration::from_millis(100)));
        assert!(reopen_ok(Some(now), now + REOPEN_GUARD));
    }

    #[test]
    fn menu_follows_the_music() {
        let l = |v: Value| menu_labels(Some(&v));
        assert_eq!(menu_labels(None), Labels { toggle: "Play", toggle_on: false, skip_on: false });
        assert_eq!(l(json!({"mode": "track", "playing": true})), Labels { toggle: "Pause", toggle_on: true, skip_on: true });
        assert_eq!(l(json!({"mode": "track", "playing": false})), Labels { toggle: "Play", toggle_on: true, skip_on: true });
        // an ad or a podcast: pausable, not skippable (the main window's rule)
        assert_eq!(l(json!({"mode": "other", "playing": true})), Labels { toggle: "Pause", toggle_on: true, skip_on: false });
        assert_eq!(l(json!({"mode": "idle"})), Labels { toggle: "Play", toggle_on: false, skip_on: false });
    }

    #[test]
    fn menu_bar_title_only_for_songs_when_on() {
        let s = json!({"mode": "track", "title": "Intro"});
        assert_eq!(title_for(Some(&s), true), Some("Intro".into()));
        assert_eq!(title_for(Some(&s), false), None);
        assert_eq!(title_for(Some(&json!({"mode": "other", "title": "Ad"})), true), None);
        assert_eq!(title_for(None, true), None);
    }
}
