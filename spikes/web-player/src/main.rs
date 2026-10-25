//! Spike: the Spotify web player (open.spotify.com) inside Tauri's WebKit layer (wry/WKWebView)
//! on macOS with a Free account. Two modes:
//!
//! - default: one window with open.spotify.com in wry's persistent data store. The user logs in
//!   and presses play (or Stylus starts playback over Connect). Once the page has reported
//!   "playing" for `HIDE_AFTER_POLLS` polls, the window is hidden: does playback go on?
//! - `SPIKE_MODE=oauth`: an incognito store, starting at Stylus's own PKCE authorize URL
//!   (same client id, scopes and redirect as src-tauri/src/auth.rs). The redirect to
//!   127.0.0.1:5588 is caught by the navigation handler; then open.spotify.com is loaded in the
//!   same store: is the web player logged in without a second login?
//!
//! Every 3 s the spike prints what the page reports (title, play/pause button, position, login
//! state, any "protected content" notice); the page reports its EME key-system requests, console
//! errors and page errors over wry's IPC. `SPIKE_UA` overrides the Safari user agent.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::Rng;
use sha2::{Digest, Sha256};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

/// Safari's user agent: the web player serves unknown browsers a "not supported" page.
const SAFARI_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.6 Safari/605.1.15";
const POLL: Duration = Duration::from_secs(3);
/// "playing" polls before the window is hidden (default mode): about 20 s of audible playback first.
const HIDE_AFTER_POLLS: usize = 7;
/// Stylus's player login (src-tauri/src/player.rs, auth.rs): librespot's client id, the same redirect and scopes.
const CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
const REDIRECT_URI: &str = "http://127.0.0.1:5588/login";
const SCOPES: &str = "app-remote-control playlist-modify playlist-modify-private playlist-modify-public playlist-read playlist-read-collaborative playlist-read-private streaming ugc-image-upload user-follow-modify user-follow-read user-library-modify user-library-read user-modify user-modify-playback-state user-modify-private user-personalized user-read-birthdate user-read-currently-playing user-read-email user-read-play-history user-read-playback-position user-read-playback-state user-read-private user-read-recently-played user-top-read";

/// Runs before the page's own scripts: reports EME key-system requests (standard and WebKit's
/// legacy `WebKitMediaKeys`), console errors and page errors to the host.
const INIT_JS: &str = r#"
(() => {
  const send = (kind, data) => { try { window.ipc.postMessage(JSON.stringify({ kind, data: kind === 'tokdump' ? String(data) : String(data).slice(0, 300) })); } catch (_) {} };
  const rmksa = navigator.requestMediaKeySystemAccess && navigator.requestMediaKeySystemAccess.bind(navigator);
  if (rmksa) navigator.requestMediaKeySystemAccess = (ks, cfg) => {
    send('eme', 'requestMediaKeySystemAccess ' + ks);
    return rmksa(ks, cfg).then(a => { send('eme', 'granted ' + ks); return a; }, e => { send('eme', 'refused ' + ks + ': ' + e); throw e; });
  };
  if (window.WebKitMediaKeys) {
    const its = WebKitMediaKeys.isTypeSupported;
    WebKitMediaKeys.isTypeSupported = (ks, t) => { const r = its.call(WebKitMediaKeys, ks, t); send('eme', 'WebKitMediaKeys.isTypeSupported ' + ks + ' ' + (t || '') + ' -> ' + r); return r; };
  }
  const seen = new Set();
  const net = (u) => { try { u = String(u); const k = u.split('?')[0]; if (/connect-state\/v1\/(devices|connect|player)|hobs_/.test(u) && !seen.has(k)) { seen.add(k); send('net', u.slice(0, 200)); } } catch (_) {} };
  // lyrics spike: take the Bearer token (and client-token) the page sends to spclient, then ask the
  // lyrics endpoint with them from the page's own context
  let tok = null, ctok = null, tried = false;
  const hv = (h, name) => { try { if (!h) return null; if (h instanceof Headers) return h.get(name); if (Array.isArray(h)) { const p = h.find(x => String(x[0]).toLowerCase() === name); return p && p[1]; } for (const k in h) if (k.toLowerCase() === name) return h[k]; } catch (_) {} return null; };
  const grab = (h) => { const a = hv(h, 'authorization'), c = hv(h, 'client-token'); if (a && /^Bearer /.test(a) && a !== tok) { tok = a; send('token', 'bearer captured, ' + a.length + ' chars'); } if (c && c !== ctok) { ctok = c; send('token', 'client-token captured'); } if (tok && ctok) send('tokdump', JSON.stringify({ authorization: tok, 'client-token': ctok })); };
  const f = window.fetch; if (f) window.fetch = function (i, o) { net(i && i.url || i); grab(o && o.headers); if (i && i.headers) grab(i.headers); return f.apply(this, arguments); };
  const xo = XMLHttpRequest.prototype.open; XMLHttpRequest.prototype.open = function (m, u) { net(u); return xo.apply(this, arguments); };
  const xs = XMLHttpRequest.prototype.setRequestHeader; XMLHttpRequest.prototype.setRequestHeader = function (k, v) { grab({ [k]: v }); return xs.apply(this, arguments); };
  setInterval(() => {
    if (!tok || tried) return; tried = true;
    const h = { authorization: tok, 'app-platform': 'WebPlayer', accept: 'application/json' }; if (ctok) h['client-token'] = ctok;
    f('https://spclient.wg.spotify.com/color-lyrics/v2/track/1GjbTNFImFrjFsNdleDe78?format=json&vocalRemoval=false&market=from_token', { headers: h })
      .then(async r => send('lyrics', 'HTTP ' + r.status + ' ' + (await r.text()).slice(0, 220)), e => send('lyrics', 'error ' + e));
  }, 2000);
  const err = console.error.bind(console);
  console.error = (...a) => { send('console.error', a.join(' ')); err(...a); };
  window.addEventListener('error', e => send('error', e.message));
  window.addEventListener('unhandledrejection', e => send('rejection', e.reason));
})();
"#;

/// What the page shows right now, as one JSON line.
const POLL_JS: &str = r#"
(() => {
  const q = s => document.querySelector(s);
  const t = s => (q(s) || {}).textContent || '';
  const btn = q('[data-testid="control-button-playpause"]');
  const body = document.body ? document.body.innerText : '';
  const notice = body.match(/[^\n]*(protected content|not supported|Something went wrong)[^\n]*/i);
  return JSON.stringify({
    url: location.href.slice(0, 80),
    title: document.title,
    loggedOut: !!q('[data-testid="login-button"]'),
    user: !!q('[data-testid="user-widget-link"]'),
    button: btn ? btn.getAttribute('aria-label') : '',
    position: t('[data-testid="playback-position"]'),
    duration: t('[data-testid="playback-duration"]'),
    session: navigator.mediaSession ? navigator.mediaSession.playbackState : 'n/a',
    hidden: document.hidden,
    devId: (() => { try { return localStorage.getItem('_spharmony_device_id'); } catch (e) { return null; } })(),
    lsKeys: (() => { try { return Object.keys(localStorage).filter(k => /device|harmony/i.test(k)).join(','); } catch (e) { return ''; } })(),
    notice: notice ? notice[0].slice(0, 160) : ''
  });
})()
"#;

enum Cmd {
    Hide,
    LoadWebPlayer,
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Stylus's PKCE authorize URL (auth.rs `authorize_url`). The code is never exchanged here.
fn authorize_url() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    let verifier: String = (0..64).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={CLIENT_ID}&scope={}&redirect_uri={}&state=spike&code_challenge_method=S256&code_challenge={challenge}",
        urlencode(SCOPES),
        urlencode(REDIRECT_URI),
    )
}

fn main() -> wry::Result<()> {
    let oauth = std::env::var("SPIKE_MODE").as_deref() == Ok("oauth");
    let event_loop = EventLoopBuilder::<Cmd>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    // SPIKE_START_HIDDEN=1: the window is never shown (Stylus's Free-mode launch); SPIKE_SHOW_AT=<s>: show it then
    let start_hidden = std::env::var("SPIKE_START_HIDDEN").is_ok();
    let show_at: Option<u64> = std::env::var("SPIKE_SHOW_AT").ok().and_then(|v| v.parse().ok());
    let window = WindowBuilder::new()
        .with_title("Spotify web player spike")
        .with_inner_size(LogicalSize::new(1100.0, 760.0))
        .with_visible(!start_hidden)
        .build(&event_loop)
        .expect("window");
    let ua = std::env::var("SPIKE_UA").unwrap_or_else(|_| SAFARI_UA.to_string());
    let started = Instant::now();
    let stamp = move || format!("{:7.1}s", started.elapsed().as_secs_f32());
    let start_url = if oauth { authorize_url() } else { "https://open.spotify.com/".to_string() };
    println!(
        "mode: {}\nUA: {ua}",
        if oauth { "oauth (incognito store, Stylus's authorize URL, then open.spotify.com)" } else { "web player (persistent store), the window hides once playback runs" }
    );
    let nav_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_url(&start_url)
        .with_user_agent(&ua)
        .with_autoplay(true)
        .with_devtools(true)
        .with_incognito(oauth)
        .with_initialization_script(INIT_JS)
        // SPIKE_TOKEN_FILE=<path>: the page's auth headers go to that file (never to stdout), for a
        // check from outside the page (curl)
        .with_ipc_handler(move |req| {
            let body = req.body();
            if body.contains("\"kind\":\"tokdump\"") {
                if let Ok(path) = std::env::var("SPIKE_TOKEN_FILE") {
                    let data = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| v["data"].as_str().map(str::to_owned));
                    if let Some(d) = data { let _ = std::fs::write(&path, d); println!("{} host: token headers written to {path}", stamp()); }
                }
                return;
            }
            println!("{} page: {}", stamp(), body)
        })
        .with_navigation_handler(move |url| {
            if url.starts_with(REDIRECT_URI) {
                println!("{} OAUTH REDIRECT caught, code present: {}", stamp(), url.contains("code="));
                let _ = nav_proxy.send_event(Cmd::LoadWebPlayer);
                return false;
            }
            true
        })
        .build(&window)?;

    let playing_polls = Arc::new(AtomicUsize::new(0));
    let hidden = Arc::new(AtomicBool::new(false));
    let mut next_poll = Instant::now() + POLL;
    event_loop.run(move |event, _, control_flow| {
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                *control_flow = ControlFlow::Exit;
                return;
            }
            Event::UserEvent(Cmd::Hide) => {
                window.set_visible(false);
                println!("{} WINDOW HIDDEN", stamp());
            }
            Event::UserEvent(Cmd::LoadWebPlayer) => {
                println!("{} loading open.spotify.com in the same store", stamp());
                let _ = webview.load_url("https://open.spotify.com/");
            }
            _ => {}
        }
        if let Some(at) = show_at {
            if started.elapsed() >= Duration::from_secs(at) && !window.is_visible() {
                window.set_visible(true);
                println!("{} WINDOW SHOWN", stamp());
            }
        }
        if Instant::now() >= next_poll {
            next_poll += POLL;
            let (proxy, playing_polls, hidden) = (proxy.clone(), playing_polls.clone(), hidden.clone());
            let _ = webview.evaluate_script_with_callback(POLL_JS, move |s| {
                // the JS returns a JSON string; wry hands it back JSON-encoded once more
                let line = serde_json::from_str::<String>(&s).unwrap_or(s);
                println!("{} poll: {line}", stamp());
                let playing = line.contains("\"session\":\"playing\"");
                if !oauth && playing && playing_polls.fetch_add(1, Ordering::SeqCst) + 1 == HIDE_AFTER_POLLS && !hidden.swap(true, Ordering::SeqCst) {
                    let _ = proxy.send_event(Cmd::Hide);
                }
            });
        }
        *control_flow = ControlFlow::WaitUntil(next_poll);
    });
}
