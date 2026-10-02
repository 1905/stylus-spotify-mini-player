//! Spotify Authorization Code + PKCE flow, token storage, and the local
//! loopback callback server that catches the redirect.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{SystemTime, UNIX_EPOCH};

pub const CLIENT_ID: &str = "9b2bc32ee90c4ef6aa0a25ccc1b076c7";
/// The app login's redirect is `http://127.0.0.1:1420/callback`.
const CALLBACK_PORT: u16 = 1420;
const CALLBACK_PATH: &str = "/callback";
/// Scopes requested at login: every standard Spotify scope, so a new feature
/// never needs another login (user, 2026-10-02). Partner-only scopes are left
/// out: Spotify rejects the whole login if an app asks for one.
/// A stored grant missing any of them → "reconnect".
const REQUIRED_SCOPES: &[&str] = &[
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-read-playback-position",
    "user-library-read",
    "user-library-modify",
    "user-top-read",
    "user-follow-read",
    "user-follow-modify",
    "ugc-image-upload",
    "app-remote-control",
    "streaming",
];

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when access_token expires.
    pub expires_at: u64,
    /// Space-separated scopes granted by Spotify. Empty in pre-scope files.
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TokenResponse {
    pub(crate) access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
}

/// True when `granted` (space-separated) contains every required scope.
pub fn has_required_scopes(granted: &str) -> bool {
    let have: Vec<&str> = granted.split_whitespace().collect();
    REQUIRED_SCOPES.iter().all(|s| have.contains(s))
}

fn status_for(tokens: Option<&Tokens>) -> &'static str {
    match tokens {
        Some(t) if !t.refresh_token.is_empty() => {
            if has_required_scopes(&t.scope) {
                "ok"
            } else {
                "reconnect"
            }
        }
        _ => "login",
    }
}

/// "login" (no usable tokens), "reconnect" (scopes missing) or "ok".
#[tauri::command]
pub fn auth_status() -> &'static str {
    status_for(load_tokens().as_ref())
}

/// A refresh failure the user can only fix by logging in again.
fn is_terminal_refresh_failure(status: u16, body: &str) -> bool {
    status == 401 || (status == 400 && body.contains("invalid_grant"))
}

pub(crate) fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

// ---- token persistence -----------------------------------------------------

fn token_path() -> std::path::PathBuf {
    app_dir().join("tokens.json")
}

pub fn load_tokens() -> Option<Tokens> {
    let data = std::fs::read_to_string(token_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_tokens(t: &Tokens) -> Result<(), String> {
    let json = serde_json::to_string_pretty(t).map_err(|e| e.to_string())?;
    write_private(&token_path(), &json).map_err(|e| format!("could not save login to {}: {e}", token_path().display()))
}

/// The app's data folder (`~/Library/Application Support/rust-spotify`), created if missing.
pub(crate) fn app_dir() -> std::path::PathBuf {
    let mut dir = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.push("rust-spotify");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Writes a secret file readable by this user only (0600), replacing it atomically.
pub(crate) fn write_private(path: &std::path::Path, data: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(data.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Moves tokens.json aside to tokens.json.invalid (overwriting) so the next
/// auth_status() reports "login". The file is kept for inspection.
fn invalidate_tokens() {
    let path = token_path();
    let _ = std::fs::rename(&path, path.with_extension("json.invalid"));
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

/// Runs the full interactive login: opens the browser, waits for the callback,
/// exchanges the code for tokens, and persists them.
#[tauri::command]
pub async fn login() -> Result<(), String> {
    let tr = oauth_login(CLIENT_ID, CALLBACK_PORT, CALLBACK_PATH, REQUIRED_SCOPES, LOGIN_TIMEOUT).await?;
    let tokens = Tokens {
        expires_at: expires_at(&tr),
        access_token: tr.access_token,
        refresh_token: tr.refresh_token.unwrap_or_default(),
        scope: tr.scope.unwrap_or_default(),
    };
    // under the lock: a refresh still in flight must not overwrite or invalidate these
    let mut cached = TOKENS.lock().await;
    save_tokens(&tokens)?;
    *cached = Some(tokens);
    Ok(())
}

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
/// exchanges the code. The app login and the player login (player.rs) both use it.
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

const LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);
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

fn expires_at(tr: &TokenResponse) -> u64 {
    now() + tr.expires_in.saturating_sub(60)
}

/// One HTTP client for every Spotify call. Finite deadlines: a stalled request
/// must fail, or it would hold the TOKENS lock and freeze every command behind it.
pub fn http() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client")
        })
        .clone()
}

/// The tokens in memory, loaded from disk on first use. The mutex also
/// serializes refreshes: Spotify rotates the refresh token, so two concurrent
/// refreshes with the same old token can get `invalid_grant` and log the user out.
static TOKENS: tokio::sync::Mutex<Option<Tokens>> = tokio::sync::Mutex::const_new(None);

/// Returns a valid access token, refreshing if expired. Errors if not logged in.
pub async fn valid_access_token() -> Result<String, String> {
    let mut cached = TOKENS.lock().await;
    if cached.is_none() {
        *cached = load_tokens();
    }
    let tokens = cached.as_mut().ok_or("AUTH_EXPIRED: not logged in")?;
    if now() < tokens.expires_at && !tokens.access_token.is_empty() {
        return Ok(tokens.access_token.clone());
    }
    if tokens.refresh_token.is_empty() {
        return Err("AUTH_EXPIRED: no refresh token, log in again".into());
    }
    let refresh_token = tokens.refresh_token.clone();
    let tr = match token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", &refresh_token),
        ("client_id", CLIENT_ID),
    ])
    .await
    {
        Ok(tr) => tr,
        Err((status, body)) if is_terminal_refresh_failure(status, &body) => {
            invalidate_tokens();
            *cached = None;
            return Err(format!("AUTH_EXPIRED: token refresh rejected: {body}"));
        }
        Err((status, body)) => return Err(format!("token refresh failed ({status}): {body}")),
    };
    tokens.expires_at = expires_at(&tr);
    tokens.access_token = tr.access_token;
    // Keep the stored scope when the refresh response omits it.
    if let Some(scope) = tr.scope {
        tokens.scope = scope;
    }
    // Spotify may rotate the refresh token.
    if let Some(rt) = tr.refresh_token {
        tokens.refresh_token = rt;
    }
    // best effort: the fresh token is in memory, so this session keeps working if the disk write fails
    let _ = save_tokens(tokens);
    Ok(tokens.access_token.clone())
}

// ---- small utilities -------------------------------------------------------

fn http_page(msg: &str) -> String {
    let body = format!(
        "<!doctype html><html><head><meta charset=utf-8><title>rust-spotify</title>\
         <style>body{{background:#121212;color:#fff;font-family:system-ui;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0}}\
         div{{text-align:center}}h1{{color:#1db954}}</style></head>\
         <body><div><h1>rust-spotify</h1><p>{msg}</p></div></body></html>"
    );
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
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

    /// A grant holding exactly the scopes the app asks for.
    fn all() -> String {
        REQUIRED_SCOPES.join(" ")
    }

    #[test]
    fn scopes_cover_every_standard_scope() {
        assert_eq!(REQUIRED_SCOPES.len(), 19);
        for s in ["playlist-modify-private", "user-follow-modify", "user-read-currently-playing", "ugc-image-upload"] {
            assert!(REQUIRED_SCOPES.contains(&s), "missing {s}");
        }
    }

    #[test]
    fn scopes_missing_library_is_reconnect() {
        let s = all().replace(" user-library-read", "");
        assert!(!has_required_scopes(&s));
    }

    #[test]
    fn scopes_all_present() {
        assert!(has_required_scopes(&all()));
    }

    #[test]
    fn scopes_missing_recently_played() {
        let s = all().replace(" user-read-recently-played", "");
        assert!(!has_required_scopes(&s));
    }

    #[test]
    fn scopes_empty() {
        assert!(!has_required_scopes(""));
    }

    #[test]
    fn scopes_extra_unknown() {
        assert!(has_required_scopes(&format!("{} something-new", all())));
    }

    #[test]
    fn status_for_cases() {
        let ok = Tokens { refresh_token: "r".into(), scope: all(), ..Default::default() };
        let no_rt = Tokens { scope: all(), ..Default::default() };
        let old = Tokens { refresh_token: "r".into(), ..Default::default() };
        assert_eq!(status_for(None), "login");
        assert_eq!(status_for(Some(&no_rt)), "login");
        assert_eq!(status_for(Some(&old)), "reconnect");
        assert_eq!(status_for(Some(&ok)), "ok");
    }

    #[test]
    fn old_token_file_loads_without_scope() {
        let t: Tokens = serde_json::from_str(
            r#"{"access_token":"a","refresh_token":"r","expires_at":1}"#,
        )
        .unwrap();
        assert_eq!(t.scope, "");
    }

    #[test]
    fn terminal_refresh_failures() {
        assert!(is_terminal_refresh_failure(400, r#"{"error":"invalid_grant"}"#));
        assert!(is_terminal_refresh_failure(401, ""));
        assert!(!is_terminal_refresh_failure(400, r#"{"error":"invalid_request"}"#));
        assert!(!is_terminal_refresh_failure(500, "invalid_grant"));
        assert!(!is_terminal_refresh_failure(429, ""));
    }
}
