//! The playing song's lyrics from Spotify's own endpoint, with the web player's token, the only
//! token it answers to: in Free mode the hidden page's (webplayer.rs reports it), in Premium mode
//! the token page's (webtoken.rs fetches it for a second).

use serde::Serialize;
use serde_json::Value;

use crate::player::Mode;

const ENDPOINT: &str = "https://spclient.wg.spotify.com/color-lyrics/v2/track";
const LOG: &str = "stylus::lyrics";

/// A song's lyrics as the UI shows them: `synced` when the lines carry start times
/// (`LINE_SYNCED`); the lines in time order.
#[derive(Debug, Serialize, PartialEq)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<Line>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Line {
    pub ms: u32,
    pub text: String,
}

/// The endpoint's body as `Lyrics`; Err for anything that isn't one (no `lyrics`, no `lines`, a
/// start time that isn't a number). An empty `words` is a gap the UI shows as ♪.
pub fn parse(body: &[u8]) -> Result<Lyrics, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("lyrics: parse: {e}"))?;
    let lyrics = v.get("lyrics").ok_or("lyrics: parse: no lyrics")?;
    let raw = lyrics.get("lines").and_then(Value::as_array).ok_or("lyrics: parse: no lines")?;
    let mut lines = Vec::with_capacity(raw.len());
    for l in raw {
        let ms = l["startTimeMs"].as_str().and_then(|s| s.parse().ok()).ok_or("lyrics: parse: bad startTimeMs")?;
        lines.push(Line { ms, text: l["words"].as_str().unwrap_or("").to_owned() });
    }
    lines.sort_by_key(|l| l.ms);
    Ok(Lyrics { synced: lyrics["syncType"] == "LINE_SYNCED", lines })
}

/// The lyrics of track `track_id` (a 22-character id) through the web player's token: None when
/// Spotify has none for it (any non-2xx answer, logged with its status); Err before the Free page
/// sent its token or while no token page may open (`LYRICS_NO_TOKEN`), while the account has no
/// web session (`LYRICS_NO_WEB_SESSION`), for anything but a track id (`LYRICS_BAD_ID`), and on a
/// transport or parse failure. A cached token the endpoint refuses (401/403) is dropped and fetched
/// once more; one fetched in this call is not. The token is never logged.
#[tauri::command]
pub async fn get_lyrics(track_id: String) -> Result<Option<Lyrics>, String> {
    if !crate::links::is_id(&track_id) {
        return Err("LYRICS_BAD_ID: not a track id".into());
    }
    // Premium asks the token page (STYLUS_LYRICS_TOKEN_PAGE=1 does so on a Free account too: the
    // way to run this path here); Free uses its page's token
    let page = std::env::var_os("STYLUS_LYRICS_TOKEN_PAGE").is_some() || crate::internal::engine().map_or(true, |e| e.mode() == Mode::Premium);
    let (token, fetched) = if page {
        crate::webtoken::token().await?
    } else {
        (crate::webplayer::report().token.ok_or("LYRICS_NO_TOKEN: the web player has not sent its token yet")?, false)
    };
    let mut resp = send(&track_id, &token).await?;
    let status = resp.status().as_u16();
    if page && matches!(status, 401 | 403) {
        crate::webtoken::reject(&token, status).await;
        if fetched {
            log::info!(target: LOG, "lyrics {track_id}: HTTP {status} with a token fetched just now");
            return Ok(None);
        }
        let (token, _) = crate::webtoken::token().await?;
        resp = send(&track_id, &token).await?;
    }
    let status = resp.status();
    if !status.is_success() {
        log::info!(target: LOG, "lyrics {track_id}: HTTP {}", status.as_u16());
        return Ok(None);
    }
    let body = resp.bytes().await.map_err(|e| format!("lyrics: transport: {e}"))?;
    parse(&body).map(Some)
}

async fn send(track_id: &str, token: &str) -> Result<reqwest::Response, String> {
    request(track_id, token).send().await.map_err(|e| format!("lyrics: transport: {e}"))
}

/// The endpoint call as the web player page makes it: its token, `app-platform: WebPlayer` (400
/// without), and a browser User-Agent (403 without, tested 2026-10-25).
fn request(track_id: &str, token: &str) -> reqwest::RequestBuilder {
    crate::paths::http()
        .get(format!("{ENDPOINT}/{track_id}?format=json&vocalRemoval=false&market=from_token"))
        .header("authorization", token)
        .header("app-platform", "WebPlayer")
        .header("accept", "application/json")
        .header("user-agent", crate::webplayer::SAFARI_UA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_asks_like_the_web_player_page() {
        let r = request("3gjHnylel3PTRpjS44ocqr", "Bearer abc").build().unwrap();
        assert_eq!(r.url().path(), "/color-lyrics/v2/track/3gjHnylel3PTRpjS44ocqr");
        assert_eq!(r.headers()["authorization"], "Bearer abc");
        assert_eq!(r.headers()["app-platform"], "WebPlayer");
        let ua = r.headers().get("user-agent").map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
        assert!(ua.starts_with("Mozilla/5.0 ") && ua.contains("Safari/"), "a browser user agent, got {ua:?}");
    }
    const SYNCED: &str = r#"{"lyrics":{"syncType":"LINE_SYNCED","language":"en","lines":[{"startTimeMs":"13780","words":"Don't ask me","syllables":[],"endTimeMs":"0"},{"startTimeMs":"16050","words":"","syllables":[],"endTimeMs":"0"},{"startTimeMs":"15000","words":"out of order","syllables":[],"endTimeMs":"0"}]}}"#;
    const UNSYNCED: &str = r#"{"lyrics":{"syncType":"UNSYNCED","lines":[{"startTimeMs":"0","words":"la"},{"startTimeMs":"0","words":"la la"}]}}"#;

    #[test]
    fn parse_maps_synced_lines_sorted_by_time() {
        let l = parse(SYNCED.as_bytes()).unwrap();
        assert!(l.synced);
        assert_eq!(l.lines, vec![Line { ms: 13780, text: "Don't ask me".into() }, Line { ms: 15000, text: "out of order".into() }, Line { ms: 16050, text: "".into() }]);
    }

    #[test]
    fn parse_keeps_unsynced_text_without_timings() {
        let l = parse(UNSYNCED.as_bytes()).unwrap();
        assert!(!l.synced);
        assert_eq!(l.lines.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["la", "la la"]);
    }

    #[test]
    fn parse_rejects_bodies_without_lyrics() {
        assert!(parse(b"not json").is_err());
        assert!(parse(br#"{"error":"nope"}"#).is_err());
        assert!(parse(br#"{"lyrics":{"syncType":"LINE_SYNCED","lines":[{"startTimeMs":"soon","words":"x"}]}}"#).is_err());
    }

    #[tokio::test]
    async fn get_lyrics_rejects_anything_but_a_track_id() {
        for id in ["", "1GjbTNFImFrjFsNdleDe7", "../connect-state/v1/x"] {
            assert!(get_lyrics(id.into()).await.unwrap_err().starts_with("LYRICS_BAD_ID"), "{id}");
        }
    }
}
