//! The MCP server end to end: a real HTTP server on a random loopback port, a stubbed data
//! source, and the requests an MCP client sends (initialize, tools/list, tools/call), plus the
//! 401/403 refusals.

use std::sync::{Arc, RwLock};

use needle_lib::mcp;
use needle_lib::mcp_tools::{Backend, Fut};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

const KEY: &str = "test-key-test-key-test-key-test-key-test-k";

struct Stub;

impl Backend for Stub {
    fn now_playing(&self) -> Fut<'_> {
        Box::pin(async {
            Ok(json!({
                "active": true, "is_playing": true, "progress_ms": 61_000, "device_id": "mac", "device_name": "This Mac",
                "track": {"uri": "spotify:track:7c378mlmubSu7NGkLFa4sN", "name": "Airbag", "artists": "Radiohead", "album": "OK Computer", "duration_ms": 284_000},
                "shuffle": false, "repeat": "off", "volume_percent": 40, "context_uri": "spotify:playlist:37i9dQZF1E4yLltmVk3nyb",
            }))
        })
    }
    fn mixes(&self) -> Fut<'_> {
        Box::pin(async { Ok(json!([{"id": "37i9dQZF1E4yLltmVk3nyb", "uri": "spotify:playlist:37i9dQZF1E4yLltmVk3nyb", "name": "Bonobo Radio", "source": "made_for_you"}])) })
    }
    fn playlists(&self) -> Fut<'_> {
        Box::pin(async { Ok(json!([])) })
    }
    fn links(&self) -> Fut<'_> {
        Box::pin(async { Ok(json!([])) })
    }
}

async fn server() -> (String, u16, CancellationToken) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = CancellationToken::new();
    let key = Arc::new(RwLock::new(KEY.to_string()));
    tokio::spawn(mcp::serve(listener, Arc::new(Stub), key, stop.clone()));
    (mcp::url(port), port, stop)
}

async fn post(url: &str, auth: Option<&str>, extra: &[(&str, &str)], body: Value) -> reqwest::Response {
    let mut rb = reqwest::Client::new()
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(k) = auth {
        rb = rb.header("authorization", format!("Bearer {k}"));
    }
    for (k, v) in extra {
        rb = rb.header(*k, *v);
    }
    rb.body(body.to_string()).send().await.unwrap()
}

fn rpc(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

#[tokio::test]
async fn initialize_list_and_call() {
    let (url, _, stop) = server().await;
    let init = rpc(1, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}));
    let r = post(&url, Some(KEY), &[], init).await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["result"]["serverInfo"]["name"], "needle");
    assert!(v["result"]["capabilities"]["tools"].is_object());

    let r = post(&url, Some(KEY), &[("mcp-protocol-version", "2025-06-18")], json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await;
    assert!(r.status().is_success(), "{}", r.status());

    let r = post(&url, Some(KEY), &[("mcp-protocol-version", "2025-06-18")], rpc(2, "tools/list", json!({}))).await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    let names: Vec<&str> = v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.len() >= 30, "{names:?}");
    for want in ["now_playing", "play", "list_mixes", "open_link", "volume_step", "mute"] {
        assert!(names.contains(&want), "{want}");
    }

    let r = post(&url, Some(KEY), &[("mcp-protocol-version", "2025-06-18")], rpc(3, "tools/call", json!({"name": "now_playing", "arguments": {}}))).await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["result"]["isError"], false, "{v}");
    let out: Value = serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(out["track"]["name"], "Airbag");
    assert_eq!(out["context"]["name"], "Bonobo Radio", "the mix's name from the Mixes tab");
    assert_eq!(out["device"]["name"], "This Mac");

    // a tool error is a result the agent reads, not a protocol error
    let r = post(&url, Some(KEY), &[("mcp-protocol-version", "2025-06-18")], rpc(4, "tools/call", json!({"name": "pause", "arguments": {}}))).await;
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["result"]["isError"], true, "{v}");
    stop.cancel();
}

#[tokio::test]
async fn refuses_without_key_bad_host_or_browser_origin() {
    let (url, port, stop) = server().await;
    let body = rpc(1, "tools/list", json!({}));
    assert_eq!(post(&url, None, &[], body.clone()).await.status(), 401);
    assert_eq!(post(&url, Some("wrong"), &[], body.clone()).await.status(), 401);
    assert_eq!(post(&url, Some(KEY), &[("origin", "https://evil.example")], body.clone()).await.status(), 403);
    assert_eq!(post(&url, Some(KEY), &[("origin", "null")], body.clone()).await.status(), 403);
    // DNS rebinding: the page's own host name reaches 127.0.0.1
    assert_eq!(post(&url, Some(KEY), &[("host", &format!("evil.example:{port}"))], body.clone()).await.status(), 403);
    assert_eq!(post(&url.replace("127.0.0.1", "localhost"), Some(KEY), &[], rpc(1, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}))).await.status(), 200);
    stop.cancel();
}
