//! The local MCP server: `http://127.0.0.1:5590/mcp` (rmcp streamable HTTP, stateless, JSON
//! answers) while the app runs and `mcp_enabled` is on. Tools: mcp_tools.rs, over the app's own
//! commands (mcp_app.rs). Every request must carry `Authorization: Bearer <mcp_key>`, a Host of
//! this loopback port, and no browser Origin: any program or web page on this Mac can reach a
//! localhost port, so the key and the Origin check keep them from driving the player.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::model::{CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::mcp_tools::{self, Backend};

pub const PORT: u16 = 5590;
const LOG: &str = "stylus::mcp";

pub fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

// ---- request checks -------------------------------------------------------------------------------

/// Why a request is refused: 401 (no or wrong key) or 403 (Host or Origin).
#[derive(Debug, PartialEq, Eq)]
pub struct Refused(pub StatusCode, pub &'static str);

/// Equal bytes, compared in constant time (the length isn't secret).
pub fn same_key(given: &str, key: &str) -> bool {
    use subtle::ConstantTimeEq;
    given.len() == key.len() && bool::from(given.as_bytes().ct_eq(key.as_bytes()))
}

/// The checks every request passes before any tool runs: Host is this loopback port (DNS
/// rebinding), no browser Origin ("null" counts as one too), then the bearer key.
pub fn check(headers: &HeaderMap, key: &str, port: u16) -> Result<(), Refused> {
    let host = headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or("");
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Err(Refused(StatusCode::FORBIDDEN, "bad Host"));
    }
    if headers.contains_key("origin") {
        return Err(Refused(StatusCode::FORBIDDEN, "browser Origin"));
    }
    let auth = headers.get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("");
    let given = auth.strip_prefix("Bearer ").unwrap_or("");
    if key.is_empty() || !same_key(given, key) {
        return Err(Refused(StatusCode::UNAUTHORIZED, "missing or wrong key"));
    }
    Ok(())
}

#[derive(Clone)]
struct Guard {
    key: Arc<RwLock<String>>,
    port: u16,
}

/// One refusal warning per reason per minute: a page hammering the port can't flood the log.
fn warn_refused(why: &str) {
    static GATE: Mutex<Option<crate::internal::WarnGate>> = Mutex::new(None);
    let now = Instant::now();
    let mut g = GATE.lock().unwrap_or_else(|e| e.into_inner());
    let gate = g.get_or_insert_with(Default::default);
    // WarnGate's window is 10 min; a minute is enough here, so the key carries the minute
    let minute = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 60).unwrap_or(0);
    if gate.allow(&format!("{why}/{minute}"), now) {
        log::warn!(target: LOG, "refused a request: {why}");
    }
}

async fn guard(State(g): State<Guard>, req: Request, next: Next) -> Response {
    let key = g.key.read().map(|k| k.clone()).unwrap_or_default();
    match check(req.headers(), &key, g.port) {
        Ok(()) => next.run(req).await,
        Err(Refused(status, why)) => {
            warn_refused(why);
            (status, why).into_response()
        }
    }
}

// ---- the MCP handler -----------------------------------------------------------------------------

#[derive(Clone)]
pub struct Stylus {
    backend: Arc<dyn Backend>,
}

const INSTRUCTIONS: &str = "Stylus is a Spotify player on this Mac. Search before you play; prefer the user's own playlists and mixes when they name one (play with `name`). This Mac is Stylus's own speaker. play reports status playing (with the track) or requested; after requested, check now_playing once.";

/// One line per call: tool, args (cut at 160 chars), ok or the error, ms.
fn log_call(tool: &str, args: &Value, result: &Result<Value, String>, ms: u128) {
    let a: String = args.to_string().chars().take(160).collect();
    match result {
        Ok(_) => log::info!(target: LOG, "{tool} {a} ok {ms} ms"),
        Err(e) => log::warn!(target: LOG, "{tool} {a} error {ms} ms: {e}"),
    }
}

/// Calls today: (day number, count).
static CALLS: Mutex<(u64, u64)> = Mutex::new((0, 0));

fn today() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0)
}

fn count_call() {
    let mut c = CALLS.lock().unwrap_or_else(|e| e.into_inner());
    let d = today();
    if c.0 != d {
        *c = (d, 0);
    }
    c.1 += 1;
}

pub fn calls_today() -> u64 {
    let c = CALLS.lock().unwrap_or_else(|e| e.into_inner());
    if c.0 == today() {
        c.1
    } else {
        0
    }
}

impl ServerHandler for Stylus {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("stylus", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(&self, _request: Option<PaginatedRequestParams>, _context: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        let tools = mcp_tools::tools()
            .into_iter()
            .map(|(name, description, schema)| Tool::new(name, description, Arc::new(schema.as_object().cloned().unwrap_or_default())))
            .collect();
        // protocol 2026-07-28 (what current clients negotiate) requires ttlMs and cacheScope on the
        // list; with_all_items leaves them empty and Claude Code then drops every tool
        let mut result = ListToolsResult::with_all_items(tools);
        result.ttl_ms = Some(60_000);
        result.cache_scope = Some(CacheScope::Private);
        Ok(result)
    }

    async fn call_tool(&self, request: CallToolRequestParams, _context: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        let started = Instant::now();
        count_call();
        let result = mcp_tools::call(self.backend.as_ref(), &request.name, &args).await;
        log_call(&request.name, &args, &result, started.elapsed().as_millis());
        Ok(match result {
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        }
        .into())
    }
}

// ---- the server -------------------------------------------------------------------------------------

/// Serves MCP on `listener` (bound to 127.0.0.1) until `stop` is cancelled. `key`: the bearer key,
/// read on every request (a reset applies at once).
pub async fn serve(listener: tokio::net::TcpListener, backend: Arc<dyn Backend>, key: Arc<RwLock<String>>, stop: CancellationToken) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_allowed_hosts([format!("127.0.0.1:{port}"), format!("localhost:{port}")])
        .with_cancellation_token(stop.child_token());
    let handler = Stylus { backend };
    let service: StreamableHttpService<Stylus, LocalSessionManager> = StreamableHttpService::new(move || Ok(handler.clone()), Default::default(), config);
    let router = axum::Router::new().nest_service("/mcp", service).layer(axum::middleware::from_fn_with_state(Guard { key, port }, guard));
    axum::serve(listener, router).with_graceful_shutdown(async move { stop.cancelled_owned().await }).await
}

struct Running {
    stop: CancellationToken,
}

#[derive(Default)]
struct Server {
    running: Option<Running>,
    error: Option<String>,
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);
static KEY: OnceLock<Arc<RwLock<String>>> = OnceLock::new();
use std::sync::OnceLock;

fn with_server<T>(f: impl FnOnce(&mut Server) -> T) -> T {
    let mut g = SERVER.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Server::default))
}

fn live_key() -> Arc<RwLock<String>> {
    KEY.get_or_init(|| Arc::new(RwLock::new(String::new()))).clone()
}

fn set_live_key(key: &str) {
    if let Ok(mut k) = live_key().write() {
        *k = key.to_string();
    }
}

/// Starts the server on PORT if it isn't running. A taken port is kept as the status error.
async fn start() {
    if with_server(|s| s.running.is_some()) {
        return;
    }
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await {
        Ok(l) => l,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::AddrInUse { format!("port {PORT} is in use") } else { format!("couldn't start: {e}") };
            log::warn!(target: LOG, "{msg}");
            with_server(|s| s.error = Some(msg));
            return;
        }
    };
    let stop = CancellationToken::new();
    with_server(|s| {
        s.running = Some(Running { stop: stop.clone() });
        s.error = None;
    });
    log::info!(target: LOG, "serving on {}", url(PORT));
    let backend: Arc<dyn Backend> = Arc::new(crate::mcp_app::App);
    tauri::async_runtime::spawn(async move {
        if let Err(e) = serve(listener, backend, live_key(), stop.clone()).await {
            log::warn!(target: LOG, "stopped: {e}");
            with_server(|s| s.error = Some(format!("stopped: {e}")));
        }
        with_server(|s| {
            if s.running.as_ref().is_some_and(|r| r.stop.is_cancelled() || r.stop == stop) {
                s.running = None;
            }
        });
    });
}

fn stop() {
    if let Some(r) = with_server(|s| s.running.take()) {
        r.stop.cancel();
        log::info!(target: LOG, "stopped");
    }
    with_server(|s| s.error = None);
}

/// At launch: start when the setting is on.
pub async fn start_if_enabled() {
    let s = tokio::task::spawn_blocking(crate::settings::load).await.unwrap_or_default();
    if !s.mcp_enabled {
        return;
    }
    // on, but the key was dropped (a hand-edited file): a new one, like the first enable
    let key = match s.mcp_key {
        Some(k) => k,
        None => match tokio::task::spawn_blocking(|| crate::settings::update(|s| s.mcp_key.get_or_insert_with(crate::settings::new_key).clone())).await {
            Ok(Ok((k, _))) => k,
            _ => return with_server(|s| s.error = Some("couldn't save a key".into())),
        },
    };
    set_live_key(&key);
    start().await;
}

fn status() -> Value {
    let enabled = crate::settings::load().mcp_enabled;
    let (running, error) = with_server(|s| (s.running.is_some(), s.error.clone()));
    json!({ "enabled": enabled, "running": running, "port": PORT, "error": error, "callsToday": calls_today() })
}

// ---- Tauri commands -------------------------------------------------------------------------------

/// `{enabled, running, port, error, callsToday}`; error: why it isn't running ("port 5590 is in use").
#[tauri::command]
pub fn mcp_status() -> Value {
    status()
}

/// Turns the server on (making the key on first use) or off, and saves the choice.
#[tauri::command]
pub async fn mcp_set_enabled(on: bool) -> Result<Value, String> {
    let (_, s) = tokio::task::spawn_blocking(move || {
        crate::settings::update(|s| {
            s.mcp_enabled = on;
            if on && s.mcp_key.is_none() {
                s.mcp_key = Some(crate::settings::new_key());
            }
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    log::info!(target: LOG, "MCP server {}", if on { "on" } else { "off" });
    if on {
        set_live_key(s.mcp_key.as_deref().unwrap_or(""));
        start().await;
    } else {
        stop();
    }
    Ok(status())
}

/// A new key: the old connect text stops working at once.
#[tauri::command]
pub async fn mcp_reset_key() -> Result<Value, String> {
    let (key, _) = tokio::task::spawn_blocking(|| {
        crate::settings::update(|s| {
            let k = crate::settings::new_key();
            s.mcp_key = Some(k.clone());
            k
        })
    })
    .await
    .map_err(|e| e.to_string())??;
    set_live_key(&key);
    log::info!(target: LOG, "key reset");
    Ok(status())
}

/// The text a client needs: `format` "json" (the mcpServers block most clients take) or
/// "claude" (the `claude mcp add` line). Makes the key if there is none yet.
#[tauri::command]
pub async fn mcp_connect_text(format: String) -> Result<String, String> {
    let (key, _) = tokio::task::spawn_blocking(|| {
        crate::settings::update(|s| s.mcp_key.get_or_insert_with(crate::settings::new_key).clone())
    })
    .await
    .map_err(|e| e.to_string())??;
    connect_text(&format, &key, PORT)
}

pub fn connect_text(format: &str, key: &str, port: u16) -> Result<String, String> {
    match format {
        "json" => {
            let v = json!({ "mcpServers": { "stylus": { "type": "http", "url": url(port), "headers": { "Authorization": format!("Bearer {key}") } } } });
            serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
        }
        "claude" => Ok(format!("claude mcp add --scope user --transport http stylus {} --header \"Authorization: Bearer {key}\"", url(port))),
        f => Err(format!("BAD_ARGS: unknown format {f}")),
    }
}

/// A SKILL.md that tells an agent how to use the tools well.
#[tauri::command]
pub fn mcp_skill_text() -> String {
    SKILL.to_string()
}

const SKILL: &str = "---
name: stylus
description: Control the Stylus Spotify player on this Mac - find and play music, control playback, browse the library. Use when the user asks to play, pause, skip, queue or find music, or asks what is playing.
---

# Stylus

Stylus's MCP server (`stylus`) controls Spotify through the Stylus app on this Mac. It works while Stylus is open.

## Rules

- Search before you play when the user names a song or album you don't have a uri for (`search`, then `play` with the `uri`).
- When the user names a playlist or mix (\"my Bonobo Radio\", \"Daily Mix 2\", \"Discover Weekly\"), play it with `play` `name`; `list_mixes` and `list_playlists` show what exists.
- A Spotify share link (open.spotify.com/...) goes to `open_link`; `save: true` keeps it in Stylus's library, `play: true` plays it.
- \"This Mac\" is Stylus's own speaker. It keeps working when Spotify rate-limits Stylus's Web API; other devices need the Web API.
- `play` on This Mac waits up to 5 s: `status: playing` names the track that started. `status: requested` means not confirmed yet: call `now_playing` once a few seconds later. After `transfer`, call `now_playing` once. Never poll it in a loop.
- Volume: `set_volume` (0-100), `volume_step` (+/-), `mute` / `unmute`.
- If a name matches several items, the error lists them with uris: pick one, or ask the user.
- Errors are plain sentences: tell the user what they say, don't retry the same call more than once.
";

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    const KEY: &str = "k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y";

    #[test]
    fn checks() {
        let ok = [("host", "127.0.0.1:5590"), ("authorization", "Bearer k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y-k3y")];
        assert_eq!(check(&headers(&ok), KEY, 5590), Ok(()));
        assert_eq!(check(&headers(&[("host", "localhost:5590"), ok[1]]), KEY, 5590), Ok(()));
        // the key
        assert_eq!(check(&headers(&[ok[0]]), KEY, 5590).unwrap_err().0, StatusCode::UNAUTHORIZED);
        assert_eq!(check(&headers(&[ok[0], ("authorization", "Bearer nope")]), KEY, 5590).unwrap_err().0, StatusCode::UNAUTHORIZED);
        assert_eq!(check(&headers(&[ok[0], ("authorization", KEY)]), KEY, 5590).unwrap_err().0, StatusCode::UNAUTHORIZED, "no Bearer prefix");
        assert_eq!(check(&headers(&[ok[0], ("authorization", "Bearer ")]), "", 5590).unwrap_err().0, StatusCode::UNAUTHORIZED, "no key yet: nothing passes");
        // Host: DNS rebinding names another host, or another port
        for host in ["evil.example:5590", "127.0.0.1:80", "127.0.0.1", "[::1]:5590", ""] {
            assert_eq!(check(&headers(&[("host", host), ok[1]]), KEY, 5590).unwrap_err().0, StatusCode::FORBIDDEN, "{host}");
        }
        // a browser's Origin, "null" too
        for origin in ["https://evil.example", "null", "http://127.0.0.1:5590"] {
            assert_eq!(check(&headers(&[ok[0], ok[1], ("origin", origin)]), KEY, 5590).unwrap_err().0, StatusCode::FORBIDDEN, "{origin}");
        }
    }

    #[test]
    fn constant_time_compare() {
        assert!(same_key(KEY, KEY));
        assert!(!same_key("", KEY));
        assert!(!same_key(&KEY[1..], KEY));
        assert!(!same_key(&format!("{}x", &KEY[..42]), KEY));
    }

    #[test]
    fn connect_texts() {
        let j: Value = serde_json::from_str(&connect_text("json", "abc", 5590).unwrap()).unwrap();
        assert_eq!(j, json!({"mcpServers": {"stylus": {"type": "http", "url": "http://127.0.0.1:5590/mcp", "headers": {"Authorization": "Bearer abc"}}}}));
        assert_eq!(connect_text("claude", "abc", 5590).unwrap(), "claude mcp add --scope user --transport http stylus http://127.0.0.1:5590/mcp --header \"Authorization: Bearer abc\"");
        assert!(connect_text("yaml", "abc", 5590).is_err());
        assert!(SKILL.starts_with("---\nname: stylus\n"));
    }

    #[test]
    fn counts_calls() {
        let before = calls_today();
        count_call();
        assert_eq!(calls_today(), before + 1);
    }
}
