# Internal API integration — spec

Status: DRAFT, awaiting approval.

## TL;DR

Add Spotify's **internal API** (api-partner GraphQL + spclient) to rust-spotify
alongside the existing public-API client. Unlocks what the public API killed:
track play counts, artist top songs, Spotify-owned playlists (Daily Mix, radio),
playlist follower counts. Auth = steal the web-player session: one-time login in
a Tauri webview, then a hidden webview mints bearer + client-token on a schedule
(option A, proven in spotify-api-parser). Pure metadata reads, no playback, no
writes. Does NOT touch: player control, library writes, existing OAuth flow —
those stay on the public API.

## Facts this builds on (verified 2026-10-02, see spotify-api-parser/docs)

- Bearer: web player fetches it from `GET open.spotify.com/api/token?…&totp=…`
  (TOTP computed by Spotify's own JS). Lifetime ~54 min.
- client-token: `POST clienttoken.spotify.com/v1/clienttoken`. Long-lived.
- Both interceptable from a live webview boot — `session.py` does it in 15 s.
- GraphQL is persisted-query-only: `{operationName, variables, extensions.persistedQuery.sha256Hash}`.
  Hashes rotate with `spotify-app-version`. Full table: `spotify-api-parser/docs/graphql-ops.md`.
- Verified recipes: artist top+playcount (`queryArtistOverview`), track playcount
  (`getTrack`), 37i9 playlists (`fetchPlaylistContents`), radio resolve
  (`inspiredby-mix/v2/seed_to_playlist`, artist+track seeds), search ops.
- Required headers: full web-player set (authorization, client-token,
  app-platform, spotify-app-version, UA, referer) — partial sets are a bot signal.

## Non-goals

- Playback control, library writes, realtime (dealer WS) — later or never.
- Reimplementing the TOTP in Rust (option B) — only if the webview boot annoys us.
- Decrypting the desktop app's credential blob — dead end, documented already.
