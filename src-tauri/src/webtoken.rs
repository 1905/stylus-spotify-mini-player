//! Premium mode's lyrics token: the hidden `webtoken` window loads open.spotify.com for about a
//! second, only to read the Bearer token of the page's own `/api/token` response, and is destroyed.
//! Its script stubs the dealer WebSocket and rejects the Connect device registration, so the page
//! never shows up as a "Web Player" device anywhere (spikes/web-player, token-only mode). The page
//! reports once, by navigating to `https://stylus.invalid/webtoken?token=…&expires=…&anonymous=…`,
//! which `on_navigation` cancels and parses. The token is never logged.

use tauri::Url;

use crate::webplayer::REPORT_HOST;

pub const LABEL: &str = "webtoken";

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

    #[test]
    fn the_page_script_blocks_the_dealer_and_the_device_registration() {
        for needle in ["dealer", "EventTarget", r"connect-state\/v1\/(devices|connect)", r"\/api\/token", "stylus.invalid/webtoken?token=", "&expires=", "&anonymous="] {
            assert!(TOKEN_JS.contains(needle), "{needle}");
        }
        assert!(!TOKEN_JS.contains("throw new DOMException"), "a throwing socket kills the page's bootstrap (spike run 1)");
    }
}
