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
/// A port in use → Err naming the port.
pub(crate) async fn oauth_login(
    client_id: &str,
    port: u16,
    redirect_path: &str,
    scopes: &[&str],
    timeout: std::time::Duration,
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
    let code = tokio::task::spawn_blocking(move || wait_for_code(listener, &path, &state, timeout))
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

const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Accepts connections until one hits `callback_path`, parses its `code`/`state`,
/// writes a friendly HTML page back, and returns the code.
/// Gives up after `timeout`, so a closed browser tab doesn't leave the app
/// waiting forever with the port held.
fn wait_for_code(
    listener: TcpListener,
    callback_path: &str,
    expected_state: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + timeout;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    loop {
        let mut stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("no answer from Spotify in 3 minutes, try again".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            Err(_) => continue,
        };
        // the accepted socket may inherit non-blocking mode; reads need a bounded block
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
        let mut buf = [0u8; 2048];
        // a browser preconnect may never send a request: skip it, keep listening
        let Ok(n) = stream.read(&mut buf) else { continue };
        let req = String::from_utf8_lossy(&buf[..n]);

        // First line: "GET /callback?code=...&state=... HTTP/1.1"
        let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
        if path.split('?').next() != Some(callback_path) {
            // Ignore favicon etc., keep listening.
            let _ = stream.write_all(http_page("Waiting…").as_bytes());
            continue;
        }

        let query = path.splitn(2, '?').nth(1).unwrap_or("");
        let mut code = None;
        let mut got_state = None;
        let mut error = None;
        for pair in query.split('&') {
            let mut it = pair.splitn(2, '=');
            match (it.next(), it.next()) {
                (Some("code"), Some(v)) => code = Some(url_decode(v)),
                (Some("state"), Some(v)) => got_state = Some(url_decode(v)),
                (Some("error"), Some(v)) => error = Some(url_decode(v)),
                _ => {}
            }
        }

        if let Some(e) = error {
            let _ = stream.write_all(http_page(&format!("Login cancelled: {e}")).as_bytes());
            return Err(format!("authorization denied: {e}"));
        }
        if got_state.as_deref() != Some(expected_state) {
            let _ = stream.write_all(http_page("State mismatch — aborted.").as_bytes());
            return Err("state mismatch (possible CSRF) — try again".into());
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

fn http_page(msg: &str) -> String {
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
        let r = wait_for_code(l, "/callback", "s", std::time::Duration::from_millis(200));
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
        let r = wait_for_code(l, "/callback", "s", std::time::Duration::from_secs(5));
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
        let r = wait_for_code(l, "/login", "s", std::time::Duration::from_secs(5));
        client.join().unwrap();
        assert_eq!(r.unwrap(), "player");
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
