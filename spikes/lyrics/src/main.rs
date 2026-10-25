//! Spike (throwaway): can Stylus's player session read Spotify's lyrics endpoint?
//!
//! Logs in with Stylus's stored player credentials, then GETs the lyrics endpoint the official
//! clients use (`color-lyrics/v2/track/<id>`) with the login5 Bearer token and client-token, the
//! way internal.rs calls Spotify's internal endpoints. Prints the HTTP status and the start of the
//! body for a few header variants and both spclient hosts. Usage: `cargo run -- [track id]`.
use librespot_core::{authentication::Credentials, Session, SessionConfig};

#[tokio::main]
async fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "1GjbTNFImFrjFsNdleDe78".into());
    let path = format!("{}/Library/Application Support/stylus/player-credentials.json", std::env::var("HOME").unwrap());
    let creds: Credentials = serde_json::from_str(&std::fs::read_to_string(&path).expect("credentials file")).expect("credentials json");
    // CLIENT_ID=<id>: ask login5 for a token of another client (e.g. the web player's) instead of librespot's
    let mut config = SessionConfig::default();
    if let Ok(id) = std::env::var("CLIENT_ID") {
        config.client_id = id;
    }
    println!("client id {}", config.client_id);
    let session = Session::new(config, None);
    session.connect(creds, false).await.expect("connect");
    println!("connected as {}", session.username());
    let token = session.login5().auth_token().await.expect("login5 token");
    let ct = session.spclient().client_token().await.ok();
    let base = session.spclient().base_url().await.expect("base url");
    println!("spclient base {base}; client-token {}", if ct.is_some() { "yes" } else { "no" });
    let client = reqwest::Client::new();
    for host in [base.as_str(), "https://spclient.wg.spotify.com"] {
        let url = format!("{host}/color-lyrics/v2/track/{id}?format=json&vocalRemoval=false&market=from_token");
        for (platform, with_ct) in [("OSX", true), ("OSX", false), ("WebPlayer", true), ("WebPlayer", false)] {
            let mut rb = client
                .get(&url)
                .header("authorization", format!("Bearer {}", token.access_token))
                .header("app-platform", platform)
                .header("accept", "application/json");
            if with_ct {
                if let Some(ct) = &ct {
                    rb = rb.header("client-token", ct);
                }
            }
            match rb.send().await {
                Ok(r) => {
                    let status = r.status();
                    let body = r.text().await.unwrap_or_default();
                    println!("\n[{host} | app-platform {platform} | client-token {with_ct}] HTTP {status}\n{}", body.chars().take(400).collect::<String>());
                }
                Err(e) => println!("\n[{host} | {platform} | client-token {with_ct}] error: {e}"),
            }
        }
    }
}
