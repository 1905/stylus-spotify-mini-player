//! P0 spike (plans/2026-10-02-standalone): does librespot play songs from my playlists
//! out of this Mac's speakers, with no Spotify app?
//!
//!   cargo run --release            list your playlists
//!   cargo run --release -- <n>     play playlist n (first 5 songs), Ctrl-C to stop
//!
//! Login: librespot's own OAuth (opens the browser once); the reusable credentials it gets
//! back are cached, so later runs need no browser. The app's token logs in but can't fetch
//! audio (INVALID_CREDENTIALS); TRY_APP_TOKEN=1 retries that path.

use std::{path::PathBuf, time::Duration};

use librespot_core::{authentication::Credentials, cache::Cache, config::SessionConfig, session::Session, SpotifyUri};
use librespot_playback::{
    audio_backend,
    config::{AudioFormat, PlayerConfig},
    mixer::NoOpVolume,
    player::{Player, PlayerEvent},
};
use serde_json::Value;

const SONGS_PER_RUN: usize = 5;
const OAUTH_PORT_URI: &str = "http://127.0.0.1:5588/login";
const KEYMASTER_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd"; // librespot's default client id
const SCOPES: &[&str] = &["streaming", "user-read-private", "user-read-email"];

fn app_dir() -> PathBuf {
    dirs::config_dir().unwrap().join("rust-spotify")
}

/// The app's Web API access token (refreshed by the running app).
fn app_token() -> String {
    let raw = std::fs::read_to_string(app_dir().join("tokens.json")).expect("tokens.json: log in with the app first");
    serde_json::from_str::<Value>(&raw).unwrap()["access_token"].as_str().unwrap().to_string()
}

async fn web_get(token: &str, url: &str) -> Value {
    let full = if url.starts_with("http") { url.to_string() } else { format!("https://api.spotify.com/v1{url}") };
    let res = reqwest::Client::new().get(full).bearer_auth(token).send().await.unwrap();
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    assert!(status.is_success(), "Web API {status}: {body} (is the app running, so the token is fresh?)");
    body
}

/// Session connected with, in order: cached librespot credentials, the app's token, librespot OAuth.
async fn connect() -> Session {
    let cache = Cache::new(Some(app_dir().join("librespot-spike")), None, None, None).unwrap();
    let mut tries: Vec<(&str, Credentials)> = Vec::new();
    if let Some(c) = cache.credentials() {
        tries.push(("cached librespot credentials", c));
    }
    // Measured: the app's token logs in, but every audio fetch then fails with
    // INVALID_CREDENTIALS (audio needs a login from librespot's own client id).
    if std::env::var("TRY_APP_TOKEN").is_ok() {
        tries.push(("the app's OAuth token", Credentials::with_access_token(app_token())));
    }
    for (what, creds) in tries {
        let session = Session::new(SessionConfig::default(), Some(cache.clone()));
        match session.connect(creds, true).await {
            Ok(()) => {
                println!("✓ librespot logged in with {what} as {}", session.username());
                return session;
            }
            Err(e) => println!("✗ login with {what} refused: {e}"),
        }
    }
    println!("→ opening the browser for librespot's own Spotify login (one time)");
    let token = tokio::task::spawn_blocking(|| {
        librespot_oauth::OAuthClientBuilder::new(KEYMASTER_CLIENT_ID, OAUTH_PORT_URI, SCOPES.to_vec())
            .open_in_browser()
            .build()
            .and_then(|c| c.get_access_token())
            .expect("librespot OAuth failed")
    })
    .await
    .unwrap();
    let session = Session::new(SessionConfig::default(), Some(cache));
    session.connect(Credentials::with_access_token(token.access_token), true).await.expect("login with librespot OAuth refused");
    println!("✓ librespot logged in with its own OAuth as {}", session.username());
    session
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let token = app_token();
    let playlists = web_get(&token, "/me/playlists?limit=50").await;
    let items: Vec<&Value> = playlists["items"].as_array().unwrap().iter().filter(|p| !p.is_null()).collect();

    let Some(n) = std::env::args().nth(1).and_then(|a| a.parse::<usize>().ok()) else {
        for (i, p) in items.iter().enumerate() {
            println!("{i:>2}  {}", p["name"].as_str().unwrap_or("?"));
        }
        println!("\nplay one: cargo run --release -- <n>");
        return;
    };
    let pl = items.get(n).expect("no playlist with that number");
    println!("playlist: {}", pl["name"].as_str().unwrap_or("?"));
    let page = web_get(&token, &format!("/playlists/{}/items?limit=50", pl["id"].as_str().unwrap())).await;
    let tracks: Vec<(String, String)> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| {
            let t = if row["item"].is_null() { &row["track"] } else { &row["item"] };
            let uri = t["uri"].as_str()?;
            uri.starts_with("spotify:track:").then(|| (uri.to_string(), t["name"].as_str().unwrap_or("?").to_string()))
        })
        .take(SONGS_PER_RUN)
        .collect();

    let session = connect().await;
    let backend = audio_backend::find(None).expect("no audio backend");
    let player = Player::new(PlayerConfig::default(), session.clone(), Box::new(NoOpVolume), move || backend(None, AudioFormat::default()));
    let mut events = player.get_player_event_channel();

    for (uri, name) in tracks {
        println!("▶ {name}  ({uri})");
        player.load(SpotifyUri::from_uri(&uri).unwrap(), true, 0);
        // play 20s of each song, then move on (enough to hear it works); report unavailable ones
        let until = tokio::time::sleep(Duration::from_secs(20));
        tokio::pin!(until);
        loop {
            tokio::select! {
                _ = &mut until => break,
                ev = events.recv() => match ev {
                    Some(PlayerEvent::Playing { position_ms, .. }) => println!("  playing (at {position_ms} ms)"),
                    Some(PlayerEvent::Unavailable { .. }) => { println!("  ✗ unavailable"); break; }
                    Some(PlayerEvent::EndOfTrack { .. }) => break,
                    None => break,
                    _ => {}
                },
            }
        }
    }
    player.stop();
    println!("done");
}
