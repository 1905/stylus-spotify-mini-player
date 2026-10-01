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
pub const REDIRECT_URI: &str = "http://127.0.0.1:1420/callback";
const CALLBACK_ADDR: &str = "127.0.0.1:1420";
/// Scopes requested at login. A stored grant missing any of them → "reconnect".
const REQUIRED_SCOPES: &[&str] = &[
    "user-read-private",
    "user-read-email",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-recently-played",
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
struct TokenResponse {
    access_token: String,
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
pub fn auth_status() -> &'static str {
    status_for(load_tokens().as_ref())
}

/// A refresh failure the user can only fix by logging in again.
fn is_terminal_refresh_failure(status: u16, body: &str) -> bool {
    status == 401 || (status == 400 && body.contains("invalid_grant"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

// ---- token persistence -----------------------------------------------------

fn token_path() -> std::path::PathBuf {
    let mut dir = dirs::config_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    dir.push("rust-spotify");
    let _ = std::fs::create_dir_all(&dir);
    dir.push("tokens.json");
    dir
}

pub fn load_tokens() -> Option<Tokens> {
    let data = std::fs::read_to_string(token_path()).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_tokens(t: &Tokens) {
    if let Ok(json) = serde_json::to_string_pretty(t) {
        let _ = std::fs::write(token_path(), json);
    }
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
/// exchanges the code for tokens, and persists them. Blocking.
pub async fn login() -> Result<(), String> {
    let verifier = gen_verifier();
    let state = gen_state();
    let auth_url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
        CLIENT_ID,
        urlencoding(&REQUIRED_SCOPES.join(" ")),
        urlencoding(REDIRECT_URI),
        state,
        challenge(&verifier),
    );

    // Start the loopback listener BEFORE opening the browser.
    let listener = TcpListener::bind(CALLBACK_ADDR)
        .map_err(|e| format!("cannot bind {CALLBACK_ADDR}: {e}. Is another instance running?"))?;

    // Open the browser (does not block).
    if let Err(e) = open_browser(&auth_url) {
        return Err(format!("could not open browser: {e}"));
    }

    // Block on the one incoming request. Spawn to a blocking thread so we
    // don't stall the async runtime.
    let expected_state = state.clone();
    let code = tokio::task::spawn_blocking(move || wait_for_code(listener, &expected_state))
        .await
        .map_err(|e| e.to_string())??;

    let tokens = exchange_code(&code, &verifier).await?;
    save_tokens(&tokens);
    Ok(())
}

/// Accepts one connection, parses the `code`/`state` from the GET line,
/// writes a friendly HTML page back, and returns the code.
fn wait_for_code(listener: TcpListener, expected_state: &str) -> Result<String, String> {
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
        let req = String::from_utf8_lossy(&buf[..n]);

        // First line: "GET /callback?code=...&state=... HTTP/1.1"
        let path = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("");
        if !path.starts_with("/callback") {
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
    Err("listener closed without a callback".into())
}

async fn exchange_code(code: &str, verifier: &str) -> Result<Tokens, String> {
    let client = http();
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ];
    let resp = client
        .post("https://accounts.spotify.com/api/token")
        .form(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("token exchange failed: {body}"));
    }
    let tr: TokenResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(Tokens {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token.unwrap_or_default(),
        expires_at: now() + tr.expires_in.saturating_sub(60),
        scope: tr.scope.unwrap_or_default(),
    })
}

/// Returns a valid access token, refreshing if expired. Errors if not logged in.
/// One HTTP client for every Spotify call. Finite deadlines: a stalled request
/// must fail, or it would hold REFRESH_LOCK and freeze every command behind it.
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

/// Serializes refreshes: Spotify rotates the refresh token, so two concurrent
/// refreshes with the same old token can get `invalid_grant` and log the user out.
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn valid_access_token() -> Result<String, String> {
    let _guard = REFRESH_LOCK.lock().await;
    let mut tokens = load_tokens().ok_or("AUTH_EXPIRED: not logged in")?;
    if now() < tokens.expires_at && !tokens.access_token.is_empty() {
        return Ok(tokens.access_token);
    }
    if tokens.refresh_token.is_empty() {
        return Err("AUTH_EXPIRED: no refresh token, log in again".into());
    }
    // Refresh.
    let client = http();
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", &tokens.refresh_token),
        ("client_id", CLIENT_ID),
    ];
    let resp = client
        .post("https://accounts.spotify.com/api/token")
        .form(&params)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        if is_terminal_refresh_failure(status.as_u16(), &body) {
            invalidate_tokens();
            return Err(format!("AUTH_EXPIRED: token refresh rejected: {body}"));
        }
        return Err(format!("token refresh failed ({}): {body}", status.as_u16()));
    }
    let tr: TokenResponse = resp.json().await.map_err(|e| e.to_string())?;
    tokens.access_token = tr.access_token;
    tokens.expires_at = now() + tr.expires_in.saturating_sub(60);
    // Keep the stored scope when the refresh response omits it.
    if let Some(scope) = tr.scope {
        tokens.scope = scope;
    }
    // Spotify may rotate the refresh token.
    if let Some(rt) = tr.refresh_token {
        tokens.refresh_token = rt;
    }
    save_tokens(&tokens);
    Ok(tokens.access_token)
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

fn urlencoding(s: &str) -> String {
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

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &str = "user-read-private user-read-email playlist-read-private playlist-read-collaborative user-read-playback-state user-modify-playback-state user-read-recently-played";

    #[test]
    fn scopes_all_present() {
        assert!(has_required_scopes(ALL));
    }

    #[test]
    fn scopes_missing_recently_played() {
        let s = ALL.replace(" user-read-recently-played", "");
        assert!(!has_required_scopes(&s));
    }

    #[test]
    fn scopes_empty() {
        assert!(!has_required_scopes(""));
    }

    #[test]
    fn scopes_extra_unknown() {
        assert!(has_required_scopes(&format!("streaming {ALL} something-new")));
    }

    #[test]
    fn status_for_cases() {
        let ok = Tokens { refresh_token: "r".into(), scope: ALL.into(), ..Default::default() };
        let no_rt = Tokens { scope: ALL.into(), ..Default::default() };
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
