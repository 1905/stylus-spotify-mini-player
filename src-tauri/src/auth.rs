//! Spotify Authorization Code + PKCE flow for the player login, and the local
//! loopback callback server that catches the redirect.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use crate::paths::{http, urlencode};

#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
}

/// The login screen's question: `{"status": "ok" | "login" | "not_premium"}`, from the player
/// engine's state (`Engine::auth_status`).
#[tauri::command]
pub fn auth_status() -> serde_json::Value {
    let status = crate::internal::engine().map_or("login", |e| e.auth_status());
    serde_json::json!({ "status": status })
}

// ---- PKCE helpers ----------------------------------------------------------

fn gen_verifier() -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::thread_rng();
    (0..64).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

fn challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn gen_state() -> String {
    let mut rng = rand::thread_rng();
    (0..16).map(|_| format!("{:x}", rng.gen_range(0..16u8))).collect()
}

// ---- the flow --------------------------------------------------------------

/// The browser URL that starts a PKCE login.
fn authorize_url(client_id: &str, scopes: &[&str], redirect_uri: &str, state: &str, verifier: &str) -> String {
    format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
        client_id,
        urlencode(&scopes.join(" ")),
        urlencode(redirect_uri),
        state,
        challenge(verifier),
    )
}

/// One interactive Authorization Code + PKCE login for `client_id`: opens the browser,
/// waits up to `timeout` for the redirect to `http://127.0.0.1:{port}{redirect_path}`, and
/// exchanges the code. The player login (player.rs) uses it.
/// A port in use → Err naming the port. `cancelled()` true → Err(`LOGIN_CANCELLED`) and the
/// port is free again within about 100 ms.
pub(crate) async fn oauth_login(
    client_id: &str,
    port: u16,
    redirect_path: &str,
    scopes: &[&str],
    timeout: std::time::Duration,
    cancelled: impl Fn() -> bool + Send + 'static,
) -> Result<TokenResponse, String> {
    let redirect_uri = format!("http://127.0.0.1:{port}{redirect_path}");
    let verifier = gen_verifier();
    let state = gen_state();
    let auth_url = authorize_url(client_id, scopes, &redirect_uri, &state, &verifier);

    // Start the loopback listener BEFORE opening the browser.
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("cannot bind 127.0.0.1:{port}: {e}. Is another instance running?"))?;

    // Open the browser (does not block).
    tauri_plugin_opener::open_url(&auth_url, None::<&str>)
        .map_err(|e| format!("could not open browser: {e}"))?;

    // Block on the one incoming request. Spawn to a blocking thread so we
    // don't stall the async runtime.
    let path = redirect_path.to_string();
    let code = tokio::task::spawn_blocking(move || wait_for_code(listener, &path, &state, timeout, cancelled))
        .await
        .map_err(|e| e.to_string())??;

    token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &redirect_uri),
        ("client_id", client_id),
        ("code_verifier", &verifier),
    ])
    .await
    .map_err(|(_, body)| format!("token exchange failed: {body}"))
}

/// The error of a login that a logout cancelled while the browser was open.
pub(crate) const LOGIN_CANCELLED: &str = "LOGIN_CANCELLED: logged out while the browser login was open";
/// The longest time a connection can stay silent before the callback server drops it.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How often a blocked accept or read checks the deadline and `cancelled`.
const POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// The longest request line the callback server reads.
const MAX_REQUEST_LINE: usize = 8192;

/// Accepts connections until one hits `callback_path` with `expected_state`, writes a
/// friendly HTML page back, and returns its `code` (or Err for its `error`).
/// A request with a wrong or missing state gets "Waiting…" and the server keeps listening.
/// Gives up at `timeout`, also while a connection is open, so a closed browser tab doesn't
/// leave the app waiting forever with the port held.
/// `cancelled()` true → Err(`LOGIN_CANCELLED`) within about `POLL`, also while a connection is open.
fn wait_for_code(
    listener: TcpListener,
    callback_path: &str,
    expected_state: &str,
    timeout: std::time::Duration,
    cancelled: impl Fn() -> bool + Send + 'static,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + timeout;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    loop {
        if cancelled() {
            return Err(LOGIN_CANCELLED.into());
        }
        let Some(left) = deadline.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero()) else {
            return Err("no answer from Spotify in 3 minutes, try again".into());
        };
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(left.min(POLL));
                continue;
            }
            Err(_) => continue,
        };
        // the accepted socket may inherit non-blocking mode; reads need a bounded block
        let _ = stream.set_nonblocking(false);
        // a browser preconnect may never send a request: skip it, keep listening
        let Some(line) = read_request_line(&mut stream, deadline, &cancelled) else { continue };

        // "GET /callback?code=...&state=... HTTP/1.1"
        let path = line.split_whitespace().nth(1).unwrap_or("");
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        let mut code = None;
        let mut got_state = None;
        let mut error = None;
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("code", v)) => code = Some(url_decode(v)),
                Some(("state", v)) => got_state = Some(url_decode(v)),
                Some(("error", v)) => error = Some(url_decode(v)),
                _ => {}
            }
        }
        // favicon, another path, a wrong or missing state: not our redirect, keep listening
        if route != callback_path || got_state.as_deref() != Some(expected_state) {
            let _ = stream.write_all(http_page("Waiting…").as_bytes());
            continue;
        }
        if let Some(e) = error {
            let _ = stream.write_all(http_page(&format!("Login cancelled: {e}")).as_bytes());
            return Err(format!("authorization denied: {e}"));
        }
        match code {
            Some(c) => {
                let _ = stream.write_all(
                    http_page("✓ Logged in. You can close this tab and return to the app.")
                        .as_bytes(),
                );
                return Ok(c);
            }
            None => {
                let _ = stream.write_all(http_page("No code returned.").as_bytes());
                return Err("no authorization code in callback".into());
            }
        }
    }
}

/// Reads the request line (up to `\r\n`, max `MAX_REQUEST_LINE` bytes) before `deadline`.
/// None when the client sends nothing, closes early, is silent for `READ_TIMEOUT`, or
/// `cancelled()` turns true.
fn read_request_line(
    stream: &mut std::net::TcpStream,
    deadline: std::time::Instant,
    cancelled: &dyn Fn() -> bool,
) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let mut idle_until = std::time::Instant::now() + READ_TIMEOUT;
    loop {
        if let Some(end) = buf.windows(2).position(|w| w == b"\r\n") {
            return Some(String::from_utf8_lossy(&buf[..end]).into_owned());
        }
        if buf.len() >= MAX_REQUEST_LINE {
            return Some(String::from_utf8_lossy(&buf[..MAX_REQUEST_LINE]).into_owned());
        }
        if cancelled() {
            return None;
        }
        let left = deadline.min(idle_until).checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero())?;
        // short read slices so a cancel is seen without waiting for the idle limit
        stream.set_read_timeout(Some(left.min(POLL))).ok()?;
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => {
                let room = MAX_REQUEST_LINE - buf.len();
                buf.extend_from_slice(&chunk[..n.min(room)]);
                idle_until = std::time::Instant::now() + READ_TIMEOUT;
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return None,
        }
    }
}

/// POST to Spotify's token endpoint. Err carries (HTTP status, body); 0 = no response.
async fn token_request(params: &[(&str, &str)]) -> Result<TokenResponse, (u16, String)> {
    let resp = http()
        .post("https://accounts.spotify.com/api/token")
        .form(params)
        .send()
        .await
        .map_err(|e| (0, e.to_string()))?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err((status, resp.text().await.unwrap_or_default()));
    }
    resp.json().await.map_err(|e| (status, e.to_string()))
}

// ---- small utilities -------------------------------------------------------

/// `s` with the HTML special characters replaced by entities.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn http_page(msg: &str) -> String {
    let msg = html_escape(msg);
    let body = format!(
        "<!doctype html><html><head><meta charset=utf-8><title>Stylus</title>\
         <style>body{{background:#121212;color:#fff;font-family:system-ui;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0}}\
         div{{text-align:center}}h1{{color:#1db954}}</style></head>\
         <body><div><h1>Stylus</h1><p>{msg}</p></div></body></html>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}


fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}


#[cfg(test)]
mod tests {

    #[test]
    fn wait_for_code_times_out_without_a_callback() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let r = wait_for_code(l, "/callback", "s", std::time::Duration::from_millis(200), || false);
        assert!(r.unwrap_err().contains("no answer"));
    }

    #[test]
    fn wait_for_code_skips_a_silent_preconnect() {
        use std::io::{Read, Write};
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let silent = std::net::TcpStream::connect(addr).unwrap();
            drop(silent); // closes without a request: read returns 0 bytes
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            s.write_all(b"GET /callback?code=abc&state=s HTTP/1.1\r\n\r\n").unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
        });
        let r = wait_for_code(l, "/callback", "s", std::time::Duration::from_secs(5), || false);
        client.join().unwrap();
        assert_eq!(r.unwrap(), "abc");
    }

    #[test]
    fn wait_for_code_takes_only_its_own_path() {
        use std::io::{Read, Write};
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            // the app's path and a lookalike are not the player's callback
            for req in ["GET /callback?code=app&state=s", "GET /loginx?code=no&state=s", "GET /login?code=player&state=s"] {
                let mut s = std::net::TcpStream::connect(addr).unwrap();
                s.write_all(format!("{req} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
            }
        });
        let r = wait_for_code(l, "/login", "s", std::time::Duration::from_secs(5), || false);
        client.join().unwrap();
        assert_eq!(r.unwrap(), "player");
    }

    #[test]
    fn wait_for_code_ignores_a_request_with_a_wrong_state() {
        use std::io::{Read, Write};
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            // a forged error with a wrong state must not end the login
            for req in ["GET /login?error=x&state=bad", "GET /login?error=y", "GET /login?code=ok&state=s"] {
                let mut s = std::net::TcpStream::connect(addr).unwrap();
                s.write_all(format!("{req} HTTP/1.1\r\n\r\n").as_bytes()).unwrap();
                let mut out = String::new();
                let _ = s.read_to_string(&mut out);
            }
        });
        let r = wait_for_code(l, "/login", "s", std::time::Duration::from_secs(2), || false);
        client.join().unwrap();
        assert_eq!(r.unwrap(), "ok");
    }

    #[test]
    fn wait_for_code_reads_a_request_line_sent_in_two_parts() {
        use std::io::{Read, Write};
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            s.write_all(b"GET /login?code=ab").unwrap();
            s.flush().unwrap();
            // longer than the 100 ms accept poll, so the server reads the first part alone
            std::thread::sleep(std::time::Duration::from_millis(250));
            s.write_all(b"c&state=s HTTP/1.1\r\n\r\n").unwrap();
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
        });
        let r = wait_for_code(l, "/login", "s", std::time::Duration::from_secs(2), || false);
        client.join().unwrap();
        assert_eq!(r.unwrap(), "abc");
    }

    #[test]
    fn wait_for_code_keeps_its_deadline_with_a_silent_open_connection() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        // connects, sends nothing and stays open until the end of the test
        let _silent = std::net::TcpStream::connect(addr).unwrap();
        let start = std::time::Instant::now();
        let r = wait_for_code(l, "/login", "s", std::time::Duration::from_millis(300), || false);
        assert!(r.is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "took {:?}", start.elapsed());
    }

    #[test]
    fn wait_for_code_stops_when_cancelled() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let setter = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            setter.store(true, Ordering::SeqCst);
        });
        let start = std::time::Instant::now();
        let r = wait_for_code(l, "/login", "s", std::time::Duration::from_secs(60), move || flag.load(Ordering::SeqCst));
        assert!(r.unwrap_err().starts_with("LOGIN_CANCELLED"));
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "took {:?}", start.elapsed());
    }

    #[test]
    fn wait_for_code_stops_when_cancelled_during_a_silent_open_connection() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let _silent = std::net::TcpStream::connect(addr).unwrap();
        let start = std::time::Instant::now();
        let r = wait_for_code(l, "/login", "s", std::time::Duration::from_secs(60), move || {
            start.elapsed() > std::time::Duration::from_millis(200)
        });
        assert!(r.unwrap_err().starts_with("LOGIN_CANCELLED"));
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "took {:?}", start.elapsed());
    }

    #[test]
    fn http_page_escapes_its_text() {
        let p = http_page("<script>&\"'");
        assert!(p.contains("&lt;script&gt;&amp;&quot;&#39;"));
        assert!(!p.contains("<script>"));
    }

    #[test]
    fn authorize_url_carries_client_scopes_and_redirect() {
        let u = authorize_url("cid", &["streaming", "user-read-email"], "http://127.0.0.1:5588/login", "st", "v");
        assert!(u.contains("client_id=cid&"));
        assert!(u.contains("scope=streaming%20user-read-email&"));
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5588%2Flogin&"));
        assert!(u.contains(&format!("code_challenge={}", challenge("v"))));
    }
    use super::*;
}
