//! The lyrics token, in both modes: the hidden `webtoken` window loads open.spotify.com for about
//! a second, only to read the Bearer token of the page's own `/api/token` response, and is destroyed.
//! Its script stubs the dealer WebSocket and rejects the Connect device registration, so the page
//! never shows up as a "Web Player" device anywhere (spikes/web-player, token-only mode). The page
//! reports once, by navigating to `https://stylus.invalid/webtoken?token=…&expires=…&anonymous=…`,
//! which `on_navigation` cancels and parses. The token is never logged.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::applog::AUTH;
use crate::nowplaying::lock;
use crate::paths::now_ms;
use crate::player::blocking;
use crate::webplayer::{PLAYER_URL, REPORT_HOST, SAFARI_UA};

pub const LABEL: &str = "webtoken";
/// How long the page gets to report (the spike saw 1.0–1.1 s after the load started).
const PAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs before the page's scripts (hence the host guard). The dealer WebSocket becomes a socket
/// that never opens, so the page keeps waiting for its Connect connection id and never registers a
/// device (a constructor that throws kills the page's bootstrap instead: spike run 1); the
/// registration endpoints are rejected as well. The page's own `/api/token` response is read once
/// and reported; an anonymous one (no web session) reports no token.
const TOKEN_JS: &str = r#"
(() => {
  if (location.host !== "open.spotify.com") return;
  const WS = window.WebSocket;
  window.WebSocket = function (u, p) {
    if (/dealer/.test(String(u))) return Object.assign(new EventTarget(), { readyState: 0, url: String(u), protocol: "", extensions: "", binaryType: "blob", bufferedAmount: 0, send() {}, close() {} });
    return p === undefined ? new WS(u) : new WS(u, p);
  };
  window.WebSocket.prototype = WS.prototype; Object.assign(window.WebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
  const blocked = (u) => /connect-state\/v1\/(devices|connect)/.test(String(u));
  let sent = false;
  const report = (tok, exp, anon) => { if (sent) return; sent = true; location.href = "https://stylus.invalid/webtoken?token=" + encodeURIComponent(tok || "") + "&expires=" + (exp || "") + "&anonymous=" + (anon ? "true" : "false"); };
  const f = window.fetch; window.fetch = function (i, o) {
    const u = String(i && i.url || i);
    if (blocked(u)) return Promise.reject(new TypeError("blocked by host"));
    const r = f.apply(this, arguments);
    if (/\/api\/token/.test(u)) r.then((x) => x.clone().json()).then((j) => report(j.isAnonymous ? "" : "Bearer " + (j.accessToken || ""), j.accessTokenExpirationTimestampMs, !!j.isAnonymous)).catch(() => {});
    return r;
  };
  const xo = XMLHttpRequest.prototype.open; XMLHttpRequest.prototype.open = function (m, u) { if (blocked(u)) arguments[1] = "https://stylus.invalid/blocked"; return xo.apply(this, arguments); };
})();
"#;

/// What the page reported: the Bearer (`Bearer …`), when it expires (ms since the Unix epoch, the
/// page's `accessTokenExpirationTimestampMs`) and whether the page had no web session.
#[derive(Clone, PartialEq, Eq, Default)]
pub(crate) struct TokenReport {
    pub token: Option<String>,
    pub expires_ms: Option<u64>,
    pub anonymous: bool,
}

/// `{:?}` never shows the token (the account's web session): the log is shareable.
impl std::fmt::Debug for TokenReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenReport")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("expires_ms", &self.expires_ms)
            .field("anonymous", &self.anonymous)
            .finish()
    }
}

/// `get_lyrics` fails with this while the page's account has no web session: the lyrics need a new
/// login (the OAuth page in the webview creates the cookies). Until then no page opens.
pub const NO_WEB_SESSION: &str = "LYRICS_NO_WEB_SESSION: no Spotify web session; log in again";
/// `get_lyrics` fails with this for anything else that is not a token yet (a page that failed or
/// timed out holds new pages back for `HOLD_MS`).
pub const NO_TOKEN: &str = "LYRICS_NO_TOKEN: no web token yet";
/// A token is fresh while it has more than this left.
const MARGIN_MS: u64 = 60_000;
/// After a failed page, how long no new page opens.
const HOLD_MS: u64 = 300_000;

/// The token in memory and what holds a new page back: `hold` is `(until_ms, the error code)`, the
/// no-session hold never ends on its own (`u64::MAX`; a login or logout forgets the cache).
/// `rejected` is the bearer the lyrics endpoint last refused: the page handing it back again is a
/// failure, not a token (the page returns the same token with its remaining time all hour).
#[derive(Default)]
pub(crate) struct Cache {
    token: Option<(String, u64)>,
    hold: Option<(u64, &'static str)>,
    rejected: Option<String>,
}

impl Cache {
    /// The token while it has more than `MARGIN_MS` left.
    fn fresh(&self, now_ms: u64) -> Option<&str> {
        self.token.as_ref().filter(|(_, exp)| now_ms + MARGIN_MS < *exp).map(|(t, _)| t.as_str())
    }

    /// The error code of a hold still in force.
    fn held(&self, now_ms: u64) -> Option<&'static str> {
        self.hold.filter(|(until, _)| now_ms < *until).map(|(_, code)| code)
    }

    /// Takes what the page reported: a usable token lifts every hold; no session holds until a
    /// login; the refused token again, or one without a usable expiry, is a failed page.
    fn took(&mut self, r: TokenReport, now_ms: u64) -> Result<(), &'static str> {
        if r.anonymous {
            self.token = None;
            self.hold = Some((u64::MAX, NO_WEB_SESSION));
            return Err(NO_WEB_SESSION);
        }
        match (r.token, r.expires_ms) {
            (Some(t), Some(exp)) if exp > now_ms + MARGIN_MS && self.rejected.as_deref() != Some(t.as_str()) => {
                self.token = Some((t, exp));
                self.hold = None;
                self.rejected = None;
                Ok(())
            }
            _ => {
                self.failed(now_ms);
                Err(NO_TOKEN)
            }
        }
    }

    /// A page that failed or timed out: no new page for `HOLD_MS`.
    fn failed(&mut self, now_ms: u64) {
        self.hold = Some((now_ms + HOLD_MS, NO_TOKEN));
    }

    /// The lyrics endpoint refused `bearer`: dropped if it is still the cached one (another call
    /// may have replaced it already), and remembered so the page cannot hand it back. True when
    /// something was dropped.
    fn reject(&mut self, bearer: &str) -> bool {
        let dropped = self.token.as_ref().is_some_and(|(t, _)| t == bearer);
        if dropped {
            self.token = None;
            self.rejected = Some(bearer.to_owned());
        }
        dropped
    }
}

/// The one cache. `token` holds it for its whole run: a second caller waits and then finds the
/// fresh token, and a second window with the label can never be built.
static CACHE: tokio::sync::Mutex<Cache> = tokio::sync::Mutex::const_new(Cache { token: None, hold: None, rejected: None });

/// The Bearer for the lyrics endpoint and whether a page fetched it in this call: the cached one
/// while fresh, else the page's. Err with `NO_WEB_SESSION` or `NO_TOKEN` while a hold is in force,
/// and when the page failed (a hold starts then). Never logs the token.
pub async fn token() -> Result<(String, bool), String> {
    let mut cache = CACHE.lock().await;
    let now = now_ms();
    if let Some(code) = cache.held(now) {
        return Err(code.into());
    }
    if let Some(t) = cache.fresh(now) {
        return Ok((t.to_owned(), false));
    }
    let fetched = match crate::internal::engine().and_then(|e| e.app()) {
        Some(app) => fetch(app).await,
        None => Err("no app handle".to_string()),
    };
    let now = now_ms();
    match fetched {
        Ok(report) => match cache.took(report, now) {
            Ok(()) => {
                let (t, exp) = cache.token.as_ref().map(|(t, e)| (t.clone(), *e)).expect("a taken token is stored");
                log::info!(target: AUTH, "web token: fetched, expires in {} min", exp.saturating_sub(now) / 60_000);
                Ok((t, true))
            }
            Err(code) => {
                if code == NO_WEB_SESSION {
                    log::warn!(target: AUTH, "web token: no Spotify web session; lyrics need a new login");
                } else {
                    log::warn!(target: AUTH, "web token: the page's token is unusable (refused before, or no expiry)");
                }
                Err(code.into())
            }
        },
        Err(e) => {
            cache.failed(now);
            log::warn!(target: AUTH, "web token: {e}");
            Err(NO_TOKEN.into())
        }
    }
}

/// The lyrics endpoint refused `bearer` with `status`: dropped while it is still the cached one,
/// and never taken back from the page.
pub async fn reject(bearer: &str, status: u16) {
    if CACHE.lock().await.reject(bearer) {
        log::info!(target: AUTH, "web token: dropped (HTTP {status})");
    }
}

/// Login and logout: the token (the old account's) and the holds go. tokio's mutex is FIFO: a
/// reset queued behind a page in flight runs after it and clears its result.
pub async fn forget() {
    *CACHE.lock().await = Cache::default();
}

/// Builds the hidden window, waits for the page's one report, destroys the window: after the
/// report, the time limit or a build error alike. Both window operations run through `blocking`
/// (they block on the main thread, `webplayer::open`); the destroy never runs inside the
/// navigation callback, which would block the main thread on itself.
async fn fetch(app: AppHandle) -> Result<TokenReport, String> {
    let (tx, rx) = oneshot::channel::<TokenReport>();
    let sender = Mutex::new(Some(tx));
    let build_app = app.clone();
    let built = blocking(move || {
        let url = Url::parse(PLAYER_URL).map_err(|e| format!("web token url: {e}"))?;
        WebviewWindowBuilder::new(&build_app, LABEL, WebviewUrl::External(url))
            .user_agent(SAFARI_UA)
            .initialization_script(TOKEN_JS)
            .on_navigation(move |url| match parse_token_report(url) {
                Some(r) => {
                    if let Some(tx) = lock(&sender).take() {
                        let _ = tx.send(r);
                    }
                    false
                }
                None => true,
            })
            .visible(false)
            .skip_taskbar(true)
            .build()
            .map(|_| ())
            .map_err(|e| format!("web token window: {e}"))
    })
    .await;
    let result = match built {
        Ok(()) => match tokio::time::timeout(PAGE_TIMEOUT, rx).await {
            Ok(Ok(report)) => Ok(report),
            Ok(Err(_)) => Err("the page went away before it reported".to_string()),
            Err(_) => Err(format!("page gave no token in {} s", PAGE_TIMEOUT.as_secs())),
        },
        Err(e) => Err(e),
    };
    let destroyed = blocking(move || match app.get_webview_window(LABEL) {
        Some(w) => w.destroy().map_err(|e| format!("web token destroy: {e}")),
        None => Ok(()),
    })
    .await;
    if let Err(e) = destroyed {
        log::warn!(target: AUTH, "web token: {e}");
    }
    result
}

/// The report in a `https://stylus.invalid/webtoken?token=…&expires=…&anonymous=…` url (an empty
/// token or expiry is None); None for any other url, the Free page's report included.
pub(crate) fn parse_token_report(url: &Url) -> Option<TokenReport> {
    if url.host_str() != Some(REPORT_HOST) || url.path() != "/webtoken" {
        return None;
    }
    let mut r = TokenReport::default();
    for (k, v) in url.query_pairs() {
        match &*k {
            "token" if !v.is_empty() => r.token = Some(v.into_owned()),
            "expires" => r.expires_ms = v.parse().ok(),
            "anonymous" => r.anonymous = v == "true",
            _ => {}
        }
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url { s.parse().unwrap() }

    #[test]
    fn parse_token_report_reads_token_expiry_and_anonymous() {
        let r = parse_token_report(&url("https://stylus.invalid/webtoken?token=Bearer%20abc&expires=1761400000000&anonymous=false")).unwrap();
        assert_eq!(r.token.as_deref(), Some("Bearer abc"));
        assert_eq!(r.expires_ms, Some(1761400000000));
        assert!(!r.anonymous);
        assert!(!format!("{r:?}").contains("abc"), "Debug hides the token");
        let anon = parse_token_report(&url("https://stylus.invalid/webtoken?token=&expires=&anonymous=true")).unwrap();
        assert_eq!(anon, TokenReport { token: None, expires_ms: None, anonymous: true });
        assert!(parse_token_report(&url("https://stylus.invalid/webplayer?device_id=x&logged_out=false")).is_none());
        assert!(parse_token_report(&url("https://open.spotify.com/api/token")).is_none());
    }

    const T0: u64 = 1_761_400_000_000;
    const HOUR: u64 = 3_600_000;
    fn report(token: &str, expires_ms: Option<u64>, anonymous: bool) -> TokenReport { TokenReport { token: (!token.is_empty()).then(|| token.to_owned()), expires_ms, anonymous } }

    #[test]
    fn a_fresh_token_is_used_until_a_minute_before_it_expires() {
        let mut c = Cache::default();
        assert_eq!(c.took(report("Bearer a", Some(T0 + HOUR), false), T0), Ok(()));
        assert_eq!(c.fresh(T0), Some("Bearer a"));
        assert_eq!(c.fresh(T0 + HOUR - MARGIN_MS - 1), Some("Bearer a"));
        assert_eq!(c.fresh(T0 + HOUR - MARGIN_MS), None);
        assert_eq!(c.held(T0), None);
    }

    #[test]
    fn an_anonymous_report_sets_the_login_hold() {
        let mut c = Cache::default();
        assert_eq!(c.took(report("", None, true), T0), Err(NO_WEB_SESSION));
        assert_eq!(c.fresh(T0), None);
        assert_eq!(c.held(T0 + 30 * 24 * HOUR), Some(NO_WEB_SESSION), "the login hold has no clock");
    }

    #[test]
    fn a_report_without_a_usable_expiry_is_a_transient_failure() {
        for exp in [None, Some(T0 - 1), Some(T0 + MARGIN_MS)] {
            let mut c = Cache::default();
            assert_eq!(c.took(report("Bearer a", exp, false), T0), Err(NO_TOKEN), "{exp:?}");
            assert_eq!(c.fresh(T0), None);
            assert_eq!(c.held(T0 + HOLD_MS - 1), Some(NO_TOKEN));
            assert_eq!(c.held(T0 + HOLD_MS), None);
        }
    }

    #[test]
    fn a_rejected_token_is_dropped_only_while_it_is_the_cached_one() {
        let mut c = Cache::default();
        c.took(report("Bearer a", Some(T0 + HOUR), false), T0).unwrap();
        c.reject("Bearer other");
        assert_eq!(c.fresh(T0), Some("Bearer a"), "another call's token stays");
        c.reject("Bearer a");
        assert_eq!(c.fresh(T0), None);
        assert_eq!(c.held(T0), None, "a 401 is not a hold: the next call fetches");
        c.took(report("Bearer b", Some(T0 + HOUR), false), T0 + 1).unwrap();
        assert_eq!(c.fresh(T0 + 1), Some("Bearer b"));
    }

    #[test]
    fn the_page_handing_back_a_rejected_token_is_a_transient_failure() {
        let mut c = Cache::default();
        c.took(report("Bearer a", Some(T0 + HOUR), false), T0).unwrap();
        c.reject("Bearer a");
        assert_eq!(c.took(report("Bearer a", Some(T0 + HOUR), false), T0 + 1), Err(NO_TOKEN));
        assert_eq!(c.fresh(T0 + 1), None);
        assert_eq!(c.held(T0 + 1), Some(NO_TOKEN));
        assert_eq!(c.held(T0 + 1 + HOLD_MS), None);
    }

    #[test]
    fn a_good_report_lifts_a_hold() {
        let mut c = Cache::default();
        c.failed(T0);
        assert_eq!(c.held(T0), Some(NO_TOKEN));
        c.took(report("Bearer a", Some(T0 + 2 * HOUR), false), T0 + HOLD_MS).unwrap();
        assert_eq!(c.held(T0 + HOLD_MS), None);
    }

    // the one test on the static cache: no window is built, the hold decides before `fetch`
    #[tokio::test]
    async fn token_fails_fast_while_held_and_forget_lifts_the_hold() {
        forget().await;
        CACHE.lock().await.took(report("", None, true), crate::paths::now_ms()).unwrap_err();
        assert!(token().await.unwrap_err().starts_with("LYRICS_NO_WEB_SESSION"));
        forget().await;
        CACHE.lock().await.failed(crate::paths::now_ms());
        assert!(token().await.unwrap_err().starts_with("LYRICS_NO_TOKEN"));
        forget().await;
        assert_eq!(CACHE.lock().await.held(crate::paths::now_ms()), None);
    }

    #[test]
    fn the_page_script_blocks_the_dealer_and_the_device_registration() {
        for needle in ["dealer", "EventTarget", r"connect-state\/v1\/(devices|connect)", r"\/api\/token", "stylus.invalid/webtoken?token=", "&expires=", "&anonymous="] {
            assert!(TOKEN_JS.contains(needle), "{needle}");
        }
        assert!(!TOKEN_JS.contains("throw new DOMException"), "a throwing socket kills the page's bootstrap (spike run 1)");
    }
}
