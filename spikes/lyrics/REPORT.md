# lyrics spike: which token reads a track's lyrics?

**Question.** The official Spotify apps show the lyrics of the playing song. Stylus has two tokens at
hand: the librespot session's login5 token (Premium and Free mode) and, in Free mode, whatever the
hidden web player page sends. Which of them does Spotify's lyrics endpoint
`GET {spclient}/color-lyrics/v2/track/{id}?format=json&vocalRemoval=false&market=from_token` accept,
and does that token work from outside the page?

**Setup.** Two probes on 2026-10-25, the user's Free account, macOS 15 on Apple Silicon.

- `spikes/lyrics` (`cargo run`): logs in with Stylus's stored player credentials
  (`~/Library/Application Support/stylus/player-credentials.json`), takes the session's login5 token,
  client-token and spclient base, and calls the endpoint in several header variants.
  `CLIENT_ID=<id>` asks login5 for a token of another client id.
- `spikes/web-player` (`SPIKE_START_HIDDEN=1 SPIKE_TOKEN_FILE=<path>`): the hidden web player page.
  Its init script captures the `authorization` and `client-token` headers the page sends to spclient
  and calls the endpoint from the page's own context; the host writes the headers to the file and a
  Python `urllib` call repeats the request from outside the page. The track was
  `1GjbTNFImFrjFsNdleDe78`.

| token | request | result |
|---|---|---|
| login5 token of librespot's client id (`65b7…`) | `gew4-spclient.spotify.com` and `spclient.wg.spotify.com`; `app-platform` OSX or WebPlayer; with and without client-token (8 variants) | 403 in every variant |
| login5 token for the web player's client id (`d8a5…`) with the stored credentials | – | login5 answers 400, no token |
| the page's own Bearer (419 characters) + its client-token, `app-platform: WebPlayer`, called from the page | `spclient.wg.spotify.com` | 200, `syncType: LINE_SYNCED`, `lines[]` with `startTimeMs`, `words`, `endTimeMs`, `syllables` |
| the same Bearer from a plain HTTP client, no client-token | `spclient.wg.spotify.com` | 200, same body |
| the same Bearer from a plain HTTP client **without a browser User-Agent** (none, or `Python-urllib`), with or without client-token | `spclient.wg.spotify.com` | 403 `Forbidden` (found 2026-10-25 in Stylus's first live run: reqwest sends no User-Agent by default; the page's Safari UA gives 200) |
| the same without `app-platform` | – | 400 |

Timing with a hidden page: the token was captured 1.2 s after the page started loading, the lyrics
answer came at 2.8 s.

## What this means for Stylus

- Lyrics are reachable only with the web player page's own token. The hidden page's init script
  already hooks `fetch` and `XMLHttpRequest` for the device id; the same hook can report the Bearer
  token to Rust, and Rust can call the endpoint itself: the token works from any HTTP client,
  `app-platform: WebPlayer` and a browser `User-Agent` are required, the client-token is not.
- Premium mode has no page after login (the `webplayer` window is destroyed when the engine reports
  Premium), so lyrics in Premium mode need a page kept or loaded for its token. Each page load also
  registers a "Web Player (Safari)" Connect device for as long as the page lives.
- Never log the token (it is the account's web session).

## Open, not covered here

- The token's lifetime and how the page refreshes it (the hook sees every new `authorization` header).
- The answer for a track without lyrics (presumably 404) and for an ad item.
- `syncType` values other than `LINE_SYNCED` (unsynced lyrics).
