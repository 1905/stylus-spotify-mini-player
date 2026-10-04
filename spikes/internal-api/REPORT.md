# Internal API spike — librespot 0.8.0 Session

**Date:** 2026-10-03 · **Run by:** spike agent (`src/main.rs`, `cargo run --release -- [probe…]`) · api.spotify.com never called.

Auth: the Session's own tokens only — login5 access token as `Authorization: Bearer` + `client-token` header. librespot logs in with the desktop client id (`65b708073fc0480ea92a077233ca87bd`), so the app's exhausted Web API quota doesn't apply. Pathfinder (`api-partner.spotify.com`) accepted both.

✓ = HTTP 200 in a real run.

| # | Need | Endpoint | Result | Format | Notes |
|---|---|---|---|---|---|
| 1 | Search | pathfinder `searchDesktop` | ✓ 331 tracks, 202 albums, 127 artists, 199 playlists | JSON | All 4 types in one call. **Use.** |
| 1 | Search, one type, paged | pathfinder `searchTracks` offset=10 | ✓ total 439 | JSON | Albums/artists/playlists variants have hashes, not run. |
| 1 | Search | `context-resolve/v1/spotify:search:…` | ✓ 20 track URIs | context JSON | Tracks only. Fallback. |
| 1 | Search | `/searchview/km/v4/search/{q}` | ✗ 400 | — | Dead. |
| 2 | My playlists | spclient `playlist/v2/user/{u}/rootlist` | ✓ 13, names + counts + owner | protobuf | Only 6/13 have a cover. |
| 2 | My playlists + covers | pathfinder `libraryV3` ["Playlists"] | ✓ 13, all with images | JSON | No track count. |
| 3 | Playlist tracks (own, other user's, 37i9 editorial, 37i9dQZF1E personal) | spclient `playlist/v2/playlist/{id}?from&length` | ✓ all 4 | protobuf | URIs only. |
| 3 | Playlist, ready for UI | pathfinder `fetchPlaylist` | ✓ 50 tracks with names | JSON | **Use.** |
| 4 | Liked Songs | spclient `POST /collection/v2/paging` | ✓ 2×50 pages | protobuf | Needs `Content-Type: application/vnd.collection-v2.spotify.proto`. Unsorted: sort by added_at. |
| 4 | Liked Songs, ready for UI | pathfinder `fetchLibraryTracks` | ✓ total 137, newest first | JSON | **Use.** |
| 5 | Saved albums | pathfinder `libraryV3` ["Albums"] | ✓ 12 | JSON | **Use.** collection paging set=album → ✗ 403. |
| 6 | Followed artists | pathfinder `libraryV3` ["Artists"] | ✓ 11 | JSON | **Use.** |
| 7 | Artist page (top tracks + albums) | pathfinder `queryArtistOverview` | ✓ top tracks + 15 albums | JSON | **Use.** Fallback: extended-metadata ARTIST_V4 ✓ (no image). |
| 8 | Recently played | spclient `recently-played/v3/user/{u}/recently-played?format=json` | ✓ 20 | JSON | Contexts, not tracks. Names via pathfinder `fetchEntitiesForRecentlyPlayed` ✓. |
| 9 | Album tracks | pathfinder `getAlbum` | ✓ 12 | JSON | Fallback: extended-metadata ALBUM_V4+TRACK_V4 ✓ (unordered, map by entity_uri). |
| 10 | Save/unsave | spclient `POST /collection/v2/write` | ✓ add + remove (reverted) | protobuf | Writes real data. |
| 10 | Is liked? | pathfinder `areEntitiesInLibrary` | ✓ | JSON | `/collection/v2/contains` guessed body → ✗ 400. |
| 11 | Devices / player state | connect-state | not probed | — | Spirc already has it; a probe would register a device. |

## Recommendation

Pathfinder for views (search, library lists, liked, artist, album, playlist); spclient for save/unsave and recently played; spclient protobuf fallbacks for playlist, liked, album, save so a stale pathfinder hash degrades instead of breaking.

## Gotchas

- Pathfinder doesn't go through `SpClient` (base URL option is private): reqwest `POST https://api-partner.spotify.com/pathfinder/v2/query` with `{variables, operationName, extensions.persistedQuery.sha256Hash}` and the two tokens.
- Persisted-query hashes come from the web player's JS bundles and rotate (`queryArtistOverview` had 2 hashes in one scrape). `tools/hashes.py` re-scrapes; `tools/hashes.json` has 185.
- `session.country()` is empty right after connect.
- librespot-protocol lacks collection v2 messages; the spike defines them with prost.

## Risk

Undocumented, can change without notice. Spotify's developer terms cover only the public Web API; using internal endpoints with the desktop client id is librespot's grey zone. Likely worst case a rate limit or account flag (guess). collection/v2/write changes real data.

## Not tested

searchAlbums/Artists/Playlists variants; pathfinder add/removeFromLibrary; whether `app-platform` / `spotify-app-version` headers are required; stale-hash behaviour; connect-state; long-run rate limits.
