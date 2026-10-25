//! Free mode's speaker: Spotify's web player (open.spotify.com) in the hidden `webplayer` window.
//!
//! The window also hosts the player login for everyone (player.rs `engine_login` opens the OAuth
//! page here), so the web player inherits the session: one login. The page gets no Tauri IPC (no
//! capability lists the window). It talks to Rust one way, by navigating to
//! `https://stylus.invalid/webplayer?device_id=…&logged_out=…&token=…`, which `on_navigation`
//! cancels and parses: the page's Connect device id (from the `PUT …/connect-state/v1/devices/hobs_<id>`
//! it makes; it changes on every page load), whether it shows a login button, and the page's own
//! Bearer token (the `authorization` header of its requests; lyrics.rs needs it, Spotify's lyrics
//! endpoint answers to no other token). Control never reads the page: commands go over Connect
//! (internal.rs) to the reported id. The token is never logged.

use std::sync::Mutex;

use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::applog::AUTH;
use crate::nowplaying::lock;

pub const LABEL: &str = "webplayer";
const PLAYER_URL: &str = "https://open.spotify.com/";
/// The host the page reports to; the navigation is cancelled, so it never leaves the app.
pub(crate) const REPORT_HOST: &str = "stylus.invalid";
/// Safari's user agent: the web player serves unknown browsers a "not supported" page.
pub(crate) const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Safari/605.1.15";
/// Runs before the page's scripts on every load in the window (hence the host guard): takes the
/// device id from the page's own device registration and the Bearer token from the headers of its
/// requests, polls the login button, reports changes. Every report carries the whole state (id,
/// login, token): a later `location.href` replaces a navigation not yet handled, so a partial
/// report could drop another's news. A fresh page load reports an empty id (the old one is gone)
/// before its own registration reports the new one.
const INIT_JS: &str = r#"
(() => {
  if (location.host !== "open.spotify.com") return;
  let id = null, out = null, tok = null;
  const report = () => { location.href = "https://stylus.invalid/webplayer?device_id=" + encodeURIComponent(id || "") + "&logged_out=" + (out ? "true" : "false") + "&token=" + encodeURIComponent(tok || ""); };
  const seen = (u) => { const m = /connect-state\/v1\/devices\/hobs_([0-9a-f]+)/.exec(String(u)); if (m && m[1] !== id) { id = m[1]; report(); } };
  const take = (v) => { if (typeof v === "string" && v.startsWith("Bearer ") && v !== tok) { tok = v; report(); } };
  const grab = (h) => { try { take(new Headers(h).get("authorization")); } catch (_) {} };
  const f = window.fetch; window.fetch = function (i, o) { seen(i && i.url || i); grab(o && o.headers); grab(i && i.headers); return f.apply(this, arguments); };
  const xo = XMLHttpRequest.prototype.open; XMLHttpRequest.prototype.open = function (m, u) { seen(u); return xo.apply(this, arguments); };
  const xh = XMLHttpRequest.prototype.setRequestHeader; XMLHttpRequest.prototype.setRequestHeader = function (k, v) { if (String(k).toLowerCase() === "authorization") take(v); return xh.apply(this, arguments); };
  setInterval(() => { const now = !!document.querySelector('[data-testid="login-button"]'); if (now !== out) { out = now; report(); } }, 2000);
})();
"#;

/// What the page last reported. The newest report wins.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// The web player's Connect device id: Here in Free mode. None until the page registers.
    pub device_id: Option<String>,
    /// The page shows a login button: the web player's Spotify session is gone.
    pub logged_out: bool,
    /// The page's own Bearer (its `authorization` header, `Bearer …`), for the lyrics endpoint.
    /// None until the page sent one. Never logged.
    pub token: Option<String>,
}

/// `{:?}` never shows the token (the account's web session): the log is shareable.
impl std::fmt::Debug for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Report")
            .field("device_id", &self.device_id)
            .field("logged_out", &self.logged_out)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

static STATE: Mutex<Report> = Mutex::new(Report { device_id: None, logged_out: false, token: None });

/// The report in a `https://stylus.invalid/webplayer?device_id=…&logged_out=…&token=…` url (an
/// empty id or token is None); None for any other url.
pub fn parse_report(url: &Url) -> Option<Report> {
    if url.host_str() != Some(REPORT_HOST) {
        return None;
    }
    let mut r = Report::default();
    for (k, v) in url.query_pairs() {
        match &*k {
            "device_id" if !v.is_empty() => r.device_id = Some(v.into_owned()),
            "logged_out" => r.logged_out = v == "true",
            "token" if !v.is_empty() => r.token = Some(v.into_owned()),
            _ => {}
        }
    }
    Some(r)
}

/// Takes a report: logs what changed (the token only as received, renewed or gone) and, when the
/// id or the login changed, sends `engine-status` (Here is the reported id in Free mode; the status
/// carries no token). The lock is released before the engine reads the report back.
pub fn apply(report: Report) {
    let prev = std::mem::replace(&mut *lock(&STATE), report.clone());
    if prev == report {
        return;
    }
    if prev.token != report.token {
        let what = match (&prev.token, &report.token) {
            (None, Some(_)) => "received",
            (Some(_), Some(_)) => "renewed",
            _ => "gone",
        };
        log::info!(target: AUTH, "web player: token {what}");
    }
    if prev.device_id == report.device_id && prev.logged_out == report.logged_out {
        return;
    }
    if prev.device_id != report.device_id {
        match &report.device_id {
            Some(id) => log::info!(target: AUTH, "web player: device id {id}"),
            None => log::info!(target: AUTH, "web player: device id gone (page load)"),
        }
    }
    if prev.logged_out != report.logged_out {
        log::info!(target: AUTH, "web player: {}", if report.logged_out { "logged out" } else { "logged in" });
    }
    if let Some(engine) = crate::internal::engine() {
        engine.emit_status();
    }
}

/// The last report.
pub fn report() -> Report {
    lock(&STATE).clone()
}

/// The window, navigated to `url` when given; created hidden at `url` (else the web player) when
/// it doesn't exist. Off the main thread `build` blocks until the main thread made the window.
fn open(app: &AppHandle, url: Option<&str>) -> Result<WebviewWindow, String> {
    let parse = |u: &str| u.parse::<Url>().map_err(|e| format!("web player url: {e}"));
    if let Some(w) = app.get_webview_window(LABEL) {
        if let Some(u) = url {
            w.navigate(parse(u)?).map_err(|e| format!("web player navigate: {e}"))?;
        }
        return Ok(w);
    }
    let hide = app.clone();
    let w = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(parse(url.unwrap_or(PLAYER_URL))?))
        .title("Spotify login")
        .inner_size(1000.0, 720.0)
        .user_agent(SAFARI_UA)
        .initialization_script(INIT_JS)
        .on_navigation(move |url| {
            if let Some(r) = parse_report(url) {
                if !r.logged_out {
                    // the page is logged in: a window shown for "Log in to play here" is done (a no-op when hidden)
                    if let Some(w) = hide.get_webview_window(LABEL) {
                        let _ = w.hide();
                    }
                }
                apply(r);
                return false;
            }
            true
        })
        // the second logged-out signal: the web player sent the page to Spotify's login. A page load
        // is the main frame's (`on_navigation` sees every frame: an iframe from accounts.spotify.com
        // must not log Here out). The OAuth login passes here too, while the engine isn't ready:
        // nothing shows for it.
        .on_page_load(|_, load| {
            if matches!(load.event(), PageLoadEvent::Started) && load.url().host_str() == Some("accounts.spotify.com") {
                apply(Report { device_id: None, logged_out: true, ..Default::default() });
            }
        })
        .visible(false)
        .skip_taskbar(true)
        .build()
        .map_err(|e| format!("web player window: {e}"))?;
    #[cfg(target_os = "macos")]
    keep_awake(&w);
    Ok(w)
}

/// Keeps the hidden page responsive. WebKit lets the process of a page nobody sees nap, and a
/// napping web player that has not played since its load stops acting on Connect commands after a
/// minute or two idle (a page that played once stays responsive, even paused). The switch is a
/// private WKPreferences setter; a WebKit without it only logs.
#[cfg(target_os = "macos")]
fn keep_awake(w: &WebviewWindow) {
    let _ = w.with_webview(|pw| unsafe {
        use objc2::{msg_send, runtime::AnyObject, sel};
        let wk = pw.inner() as *mut AnyObject;
        let cfg: *mut AnyObject = msg_send![wk, configuration];
        let prefs: *mut AnyObject = msg_send![cfg, preferences];
        let supported: bool = msg_send![prefs, respondsToSelector: sel!(_setPageVisibilityBasedProcessSuppressionEnabled:)];
        if supported {
            let _: () = msg_send![prefs, _setPageVisibilityBasedProcessSuppressionEnabled: false];
        } else {
            log::warn!(target: AUTH, "web player: no process-suppression switch in this WebKit; a hidden page may nap");
        }
    });
}

/// Shows the window at `url`: the player login's OAuth page (player.rs `engine_login`).
pub fn show_login(app: &AppHandle, url: &str) -> Result<(), String> {
    let w = open(app, Some(url))?;
    let _ = w.show();
    let _ = w.set_focus();
    Ok(())
}

/// The web player, hidden: navigates there unless the window already shows it (a restart of the
/// engine keeps the page, so its device id stays).
pub fn start(app: &AppHandle) -> Result<(), String> {
    let at_player = |w: &WebviewWindow| w.url().ok().is_some_and(|u| u.host_str() == Some("open.spotify.com"));
    let w = match app.get_webview_window(LABEL) {
        Some(w) if at_player(&w) => w,
        Some(_) => open(app, Some(PLAYER_URL))?,
        None => open(app, None)?,
    };
    w.hide().map_err(|e| format!("web player hide: {e}"))
}

/// Closes the window for good (a Premium engine, a failed login) and forgets its report.
pub fn destroy(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        if let Err(e) = w.destroy() {
            log::warn!(target: AUTH, "web player: destroy failed: {e}");
        }
    }
    apply(Report::default());
}

/// Logout: forgets the Spotify session in the window's data store (the OAuth cookies live there;
/// without this the next login would silently pick the old account), then destroys the window.
/// The store is shared by every window and outlives this one (a Premium engine destroyed it at
/// Ready): without a window, a blank one is created for the clearing.
pub fn clear(app: &AppHandle) -> Result<(), String> {
    let w = match app.get_webview_window(LABEL) {
        Some(w) => w,
        None => open(app, Some("about:blank"))?,
    };
    let cleared = w.clear_all_browsing_data().map_err(|e| format!("web player data: {e}"));
    destroy(app);
    cleared
}

/// Shows the window (the device list's "Log in to play here" row): the page's own login.
#[tauri::command]
pub fn webplayer_show(app: AppHandle) -> Result<(), String> {
    let w = open(&app, None)?;
    let _ = w.show();
    let _ = w.set_focus();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_report_reads_the_report_url() {
        let url = |s: &str| s.parse::<tauri::Url>().unwrap();
        let r = parse_report(&url("https://stylus.invalid/webplayer?device_id=0a19054b&logged_out=false")).unwrap();
        assert_eq!(r, Report { device_id: Some("0a19054b".into()), logged_out: false, ..Default::default() });
        let out = parse_report(&url("https://stylus.invalid/webplayer?device_id=&logged_out=true")).unwrap();
        assert_eq!(out, Report { device_id: None, logged_out: true, ..Default::default() });
        assert!(parse_report(&url("https://open.spotify.com/")).is_none());
        assert!(parse_report(&url("http://127.0.0.1:5588/login?code=x")).is_none());
    }

    #[test]
    fn debug_output_hides_the_token() {
        let r = Report { device_id: Some("0a19054b".into()), logged_out: false, token: Some("Bearer secret-web-session".into()) };
        let shown = format!("{r:?}");
        assert!(!shown.contains("secret-web-session"), "{shown}");
        assert!(shown.contains("0a19054b") && shown.contains("redacted"), "{shown}");
        assert!(format!("{:?}", Report::default()).contains("None"));
    }

    #[test]
    fn parse_report_reads_the_token() {
        let url = |s: &str| s.parse::<tauri::Url>().unwrap();
        let r = parse_report(&url("https://stylus.invalid/webplayer?device_id=0a19054b&logged_out=false&token=Bearer%20abc")).unwrap();
        assert_eq!(r.token.as_deref(), Some("Bearer abc"));
        let none = parse_report(&url("https://stylus.invalid/webplayer?device_id=0a19054b&logged_out=false&token=")).unwrap();
        assert_eq!(none.token, None);
        assert_eq!(parse_report(&url("https://stylus.invalid/webplayer?device_id=0a19054b&logged_out=false")).unwrap().token, None);
    }
}
