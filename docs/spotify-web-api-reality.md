# Spotify Web API: what really works for this app

Probed 2026-10-02 against `https://api.spotify.com/v1` with this app's own user token.
Every row below shows the HTTP status we actually got.
The official reference and the February 2026 migration guide are wrong or stale in several places. This file wins over them.

## TL;DR

1. Library writes work only through `PUT /me/library?uris=…` and `DELETE /me/library?uris=…`. URIs go in the **query string**, comma-separated, max 40. All per-type endpoints (`/me/tracks`, `/me/albums`, `/me/shows`, `/me/episodes`, `/me/audiobooks`, `/me/following`) are 403 for writes and for `/contains`.
2. `/me/library` also follows/unfollows **artists, users and playlists** (`spotify:artist:…`, `spotify:user:…`, `spotify:playlist:…`). One call can mix types.
3. Saved-state checks: `GET /me/library/contains?uris=…` (max 40, mixed types, bare ids → 400).
4. Reading the library lists still works: `GET /me/tracks|albums|shows|episodes` and `GET /me/following?type=artist` (all max limit 50).
5. Playlist contents: only `/playlists/{id}/items` (rows use `item`, not `track`). `/playlists/{id}/tracks` is 403 for every method. Contents are readable only for playlists the user **owns** (other users' playlists → 403, Spotify-owned `37i9…` → 404).
6. Spotify-owned playlists (`37i9…`, mixes, editorial): `GET /playlists/{id}` 404, `/items` 404, but `/images` 200, follow/unfollow 200, and playback by `context_uri` works.
7. Every batch "several" endpoint is 403: `GET /tracks|albums|artists|shows|episodes|audiobooks|chapters?ids=`. Fetch one at a time.
8. Gone entirely: audio-features, audio-analysis (403), recommendations + genre seeds (404), related-artists, artist top-tracks, new-releases, featured playlists, categories, markets, `/users/{id}`, `/users/{id}/playlists` (all 403).
9. Search works but `limit` max is **10** (default 5); `limit + offset` ≤ 1000. Filters `tag:new`, `tag:hipster`, `year:`, `artist:`, `album:`, `isrc:`, `upc:`, `genre:` all return 200. Audiobooks return 0 results in every market tried.
10. Artist objects are stripped to `id, name, images, uri, href, external_urls, type` everywhere (no `genres`, `popularity`, `followers`). Tracks lose `popularity`, `available_markets`, `linked_from`; albums lose `label`, `popularity`, `available_markets`. `GET /artists/{id}/albums` max limit is **10**.

## How tested

- Date: 2026-10-02.
- App mode: development (Client ID in Development Mode, post-March-2026 rules).
- Account: `<user id>`, `product: premium`, `country: ID` (from `GET /me`).
- Granted scopes (all 19): `user-read-private user-read-email playlist-read-private playlist-read-collaborative playlist-modify-private playlist-modify-public user-read-playback-state user-modify-playback-state user-read-currently-playing user-read-recently-played user-read-playback-position user-library-read user-library-modify user-top-read user-follow-read user-follow-modify ugc-image-upload app-remote-control streaming`.
- Client: Python stdlib `urllib`, ≤4 req/s, ~340 requests, no 429 seen.
- Writes were limited to reversible actions on items the user had not saved/followed, plus one temporary playlist that was deleted. Final state checked: liked tracks 135, saved albums 12, shows 2, episodes 26, followed artists 11, playlists 5. All equal to the starting values.
- Player writes were not re-run here. Those rows are marked "verified 2026-10-02 by orchestrator".
- Library writes were verified by `GET /me/library/contains` plus the list `total` 4 s after the write.

Legend: ✓ works · ✗ blocked · ⚠ works with a quirk.

## Users / profile

| Method | Path | Scope | Status | Works | Notes / replacement |
|---|---|---|---|---|---|
| GET | `/me` | user-read-private, user-read-email | 200 | ⚠ | Still returns `country`, `email`, `product`, `explicit_content`, `followers`. The migration guide says these are removed; they are not (yet). Do not depend on them long-term. |
| GET | `/users/{id}` | – | 403 | ✗ | Also 403 for the user's own id and for `spotify`. No replacement. |
| GET | `/me/top/artists` | user-top-read | 200 | ✓ | limit max 50 (51 → 400). total=182. Artist objects stripped (no genres/popularity/followers). |
| GET | `/me/top/tracks` | user-top-read | 200 | ✓ | limit max 50. totals: short_term 532, medium_term 2010, long_term 6741. `offset=1950` works. Tracks have no `popularity`. |
| GET | `/me/following?type=artist` | user-follow-read | 200 | ✓ | limit max 50. Cursor paging with `after=<artist id>` works. |
| GET | `/me/following?type=user` | user-follow-read | 400 | ✗ | "Invalid type: user". Use `GET /me/library/contains?uris=spotify:user:…` to check one user. |
| GET | `/me/following/contains` | user-follow-read | 403 | ✗ | Both `type=artist` and `type=user`. Replacement: `GET /me/library/contains?uris=spotify:artist:…`. |
| PUT | `/me/following` | user-follow-modify | 403 | ✗ | artist and user. Replacement: `PUT /me/library?uris=spotify:artist:…` (200, verified). |
| DELETE | `/me/following` | user-follow-modify | 403 | ✗ | Replacement: `DELETE /me/library?uris=…` (200, verified). |
| PUT | `/playlists/{id}/followers` | playlist-modify-* | 200 | ⚠ | Still works although the migration guide lists it as removed. Tested on Spotify mix `37i9dQZF1E4yLltmVk3nyb`: contains → `[true]`. |
| DELETE | `/playlists/{id}/followers` | playlist-modify-* | 200 | ⚠ | Still works. This is also how you delete your own playlist (verified on the probe playlist). |
| GET | `/playlists/{id}/followers/contains` | playlist-read-private | 403 | ✗ | With or without `ids`. Replacement: `GET /me/library/contains?uris=spotify:playlist:…`. |

## Library (new unified endpoints)

| Method | Path | Scope | Status | Works | Notes |
|---|---|---|---|---|---|
| PUT | `/me/library?uris=a,b,…` | user-library-modify / user-follow-modify / playlist-modify-* | 200 | ✓ | Verified for `track`, `album`, `show`, `episode`, `artist`, `user`, `playlist` (contains → true, list totals +1). Mixed types in one call work. Max 40 URIs (41 → 400 "Too many uris requested"). Colons raw or `%3A`-encoded both work. Empty body, `Content-Type: application/json` with empty body, or body `{}` all work. Response body is empty. |
| PUT | `/me/library` with JSON body `{"uris":[…]}` and no query | – | 400 | ✗ | "Missing required field: uris". The body is ignored. |
| PUT | `/me/library?ids=…&type=track` | – | 400 | ✗ | "Missing required field: uris". Only `uris` is accepted. |
| PUT | `/me/library?uris=<bare id>` | – | 400 | ✗ | "Invalid Spotify URI". |
| PUT | `/me/library?uris=spotify:track:0000000000000000000000` | – | 200 | ⚠ | Unknown id is accepted silently; contains still `[false]`. A 200 does not prove the item exists. |
| DELETE | `/me/library?uris=a,b,…` | same as PUT | 200 | ✓ | Verified for all 7 types above and for a mixed 4-URI call. |
| GET | `/me/library/contains?uris=a,b,…` | user-library-read / user-follow-read / playlist-read-private | 200 | ✓ | Array of booleans in input order. Mixed types OK. Max 40 (41 → 400). Bare id → 400. `spotify:chapter:` → 400 "Invalid Spotify URI". `spotify:audiobook:<id not available in market>` → 500. |
| GET | `/me/library` | – | 405 | ✗ | No list endpoint. Use the per-type list endpoints below. |
| POST | `/me/library` | – | not reached | – | The first variant (PUT) already worked, so POST was not needed. |

## Tracks

| Method | Path | Scope | Status | Works | Notes / replacement |
|---|---|---|---|---|---|
| GET | `/tracks/{id}` | – | 200 | ⚠ | Keys: `album, artists, disc_number, duration_ms, explicit, external_ids, external_urls, href, id, is_local, is_playable, name, track_number, type, uri`. No `popularity`, `available_markets`, `linked_from`. `external_ids.isrc` is still present (guide says removed). `is_playable` present even without `market`. |
| GET | `/tracks?ids=` | – | 403 | ✗ | Fetch one by one with `/tracks/{id}`. |
| GET | `/me/tracks` | user-library-read | 200 | ✓ | limit max 50. Rows `{added_at, track}`. total=135. |
| PUT | `/me/tracks` | user-library-modify | 403 | ✗ | Query `ids`, JSON `ids`, and JSON `timestamped_ids` all 403. Replacement: `PUT /me/library?uris=spotify:track:…`. |
| DELETE | `/me/tracks` | user-library-modify | 403 | ✗ | Replacement: `DELETE /me/library?uris=…`. |
| GET | `/me/tracks/contains` | user-library-read | 403 | ✗ | Replacement: `GET /me/library/contains?uris=…`. |
| GET | `/audio-features/{id}` | – | 403 | ✗ | No replacement. |
| GET | `/audio-features?ids=` | – | 403 | ✗ | No replacement. |
| GET | `/audio-analysis/{id}` | – | 403 | ✗ | No replacement. |
| GET | `/recommendations` | – | 404 | ✗ | Endpoint is gone (404, not 403), with `seed_tracks` or `seed_artists`. |

## Albums

| Method | Path | Scope | Status | Works | Notes / replacement |
|---|---|---|---|---|---|
| GET | `/albums/{id}` | – | 200 | ⚠ | No `label`, `popularity`, `available_markets`. `genres` is `[]`. `external_ids.upc` present. `is_playable` (album and tracks) appears only when `market` is passed. |
| GET | `/albums?ids=` | – | 403 | ✗ | One by one. |
| GET | `/albums/{id}/tracks` | – | 200 | ✓ | limit max 50. Simplified tracks (no album). |
| GET | `/me/albums` | user-library-read | 200 | ✓ | limit max 50. Rows `{added_at, album}`; album includes `tracks`. |
| PUT / DELETE | `/me/albums` | user-library-modify | 403 / 403 | ✗ | Replacement: `/me/library?uris=spotify:album:…` (verified, total 12→13→12). |
| GET | `/me/albums/contains` | user-library-read | 403 | ✗ | Replacement: `/me/library/contains`. |
| GET | `/browse/new-releases` | – | 403 | ✗ | Partial replacement: search `q=tag:new&type=album` (200, total 100). |

## Artists

| Method | Path | Scope | Status | Works | Notes / replacement |
|---|---|---|---|---|---|
| GET | `/artists/{id}` | – | 200 | ⚠ | Only `external_urls, href, id, images, name, type, uri`. No `genres`, `popularity`, `followers`. Same stripping in search results and `/me/top/artists`. |
| GET | `/artists?ids=` | – | 403 | ✗ | One by one. |
| GET | `/artists/{id}/albums` | – | 200 | ⚠ | **limit max 10** (11, 20, 50 → 400 "Invalid limit"). Page with `offset`. `include_groups` works. |
| GET | `/artists/{id}/top-tracks` | – | 403 | ✗ | Partial replacement: search `q=artist:"<name>"&type=track` (ranking is search relevance, not plays). |
| GET | `/artists/{id}/related-artists` | – | 403 | ✗ | No replacement. |

## Shows / Episodes

| Method | Path | Scope | Status | Works | Notes / replacement |
|---|---|---|---|---|---|
| GET | `/shows/{id}` | – | 200 | ⚠ | No `publisher`, no `available_markets`. Includes `episodes` page, `total_episodes`. |
| GET | `/shows?ids=` | – | 403 | ✗ | One by one. |
| GET | `/shows/{id}/episodes` | – | 200 | ✓ | limit max 50. Items include `resume_point`. |
| GET | `/episodes/{id}` | user-read-playback-position for `resume_point` | 200 | ✓ | Includes `show` and `resume_point` (`resume_position_ms`, `fully_played`). |
| GET | `/episodes?ids=` | – | 403 | ✗ | One by one. |
| GET | `/me/shows`, `/me/episodes` | user-library-read | 200 | ✓ | limit max 50. |
| PUT / DELETE | `/me/shows`, `/me/episodes` | user-library-modify | 403 | ✗ | Replacement: `/me/library?uris=spotify:show:…` / `spotify:episode:…` (verified, totals 2→3→2 and 26→27→26). |
| GET | `/me/shows/contains`, `/me/episodes/contains` | user-library-read | 403 | ✗ | Replacement: `/me/library/contains`. |

## Audiobooks / Chapters

| Method | Path | Scope | Status | Works | Notes |
|---|---|---|---|---|---|
| GET | `/me/audiobooks` | user-library-read | 200 | ⚠ | `total: 3` but every item is `null`, with and without `market=US`. Audiobooks are not available in market `ID`, so ids cannot be learned. |
| GET | `/audiobooks/{id}` | – | 404 | untested | Only a guessed id was available; 404 "Resource not found" with and without `market=US`. No valid audiobook id was obtainable. |
| GET | `/audiobooks?ids=` | – | 403 | ✗ | Batch blocked (status independent of id validity). |
| GET | `/audiobooks/{id}/chapters` | – | 404 | untested | No valid id. |
| GET | `/chapters/{id}`, `/chapters?ids=` | – | – | untested | No valid chapter id. |
| PUT / DELETE | `/me/audiobooks` | user-library-modify | 403 / 403 | ✗ | Replacement should be `/me/library?uris=spotify:audiobook:…` per docs; untested (no valid id). |
| GET | `/me/audiobooks/contains` | user-library-read | 403 | ✗ | `/me/library/contains` with an invalid audiobook id → 500. |
| GET | `/search?type=audiobook` | – | 200 | ⚠ | 0 results for "dune" and "bonobo" with markets default, US, GB. |

## Playlists

| Method | Path | Scope | Status | Works | Notes / replacement |
|---|---|---|---|---|---|
| GET | `/me/playlists` | playlist-read-private | 200 | ✓ | limit max 50. Each item has `items: {href, total}` (renamed from `tracks`). Followed playlists owned by others also show `items.total`. |
| GET | `/users/{id}/playlists` | – | 403 | ✗ | Also 403 for the own user id. Use `/me/playlists`. |
| GET | `/playlists/{id}` (own) | playlist-read-private | 200 | ✓ | Includes `items` page (renamed from `tracks`) and `followers`. `fields=name,items.total` works. |
| GET | `/playlists/{id}` (other user's) | – | 200 | ⚠ | Metadata only: **no `items` key at all**. `followers` present. |
| GET | `/playlists/{id}` (Spotify-owned `37i9…`) | – | 404 | ✗ | Tested mix `37i9dQZF1E4yLltmVk3nyb`, `37i9dQZF1DXcBWIGoYBM5M`, `37i9dQZF1DX4sWSpwq3LiO`, `37i9dQZEVXcJZyENOWUFo7`. Also 404 with `market=ID`. |
| GET | `/playlists/{id}/items` (own) | playlist-read-private | 200 | ✓ | limit max 100. Rows: `added_at, added_by, is_local, item, primary_color, video_thumbnail`. `fields=total,items(item(name,uri))` works. Episodes come back with `item.type == "episode"`. |
| GET | `/playlists/{id}/items` (other user's) | – | 403 | ✗ | Tested `16rzOJ2faZsJeQb2Edk6SK` (followed by the user) and `0vvXsWCC9xrXsKd4FyS8kM` (chilledcow). No replacement. |
| GET | `/playlists/{id}/items` (Spotify-owned) | – | 404 | ✗ | No replacement. Playing via `context_uri` still works (see Player). |
| GET / POST / PUT / DELETE | `/playlists/{id}/tracks` | – | 403 | ✗ | Every method, own playlist included. Use `/items`. |
| GET | `/playlists/{id}/images` | – | 200 | ✓ | Works for own, other users' and Spotify-owned playlists (mix returned 4 images). |
| POST | `/me/playlists` | playlist-modify-private | 201 | ✓ | Body `{name, public, description}`. New endpoint name; works. |
| POST | `/users/{id}/playlists` | playlist-modify-* | – | untested | Not tried: `/me/playlists` worked and the probe was limited to one playlist. |
| PUT | `/playlists/{id}` | playlist-modify-* | 200 | ⚠ | `name` and `description` change. `public: false` did **not** stick: GET afterwards showed `public: true` (create response had `public: false`). `collaborative: true` was accepted and reads back true while `public` reads true. Do not trust `public`. |
| POST | `/playlists/{id}/items` | playlist-modify-* | 201 | ✓ | JSON `{"uris":[…]}` or query `uris=…&position=0`. Returns `{snapshot_id}`. Episodes accepted. Max 100 (101 → 400 "Too many ids requested"). Album URI → 400 "Invalid base62 id". |
| PUT | `/playlists/{id}/items` (reorder) | playlist-modify-* | 200 | ✓ | `{range_start, insert_before, range_length}`. |
| PUT | `/playlists/{id}/items` (replace) | playlist-modify-* | 200 | ✓ | JSON `{"uris":[…]}` or query `uris=a,b`. Replaces all items. |
| DELETE | `/playlists/{id}/items` | playlist-modify-* | 200 | ⚠ | Body key must be **`items`**: `{"items":[{"uri":…}]}`. The old key `tracks` → 400 "No uris provided". |
| PUT | `/playlists/{id}/images` | ugc-image-upload + playlist-modify-* | 202 | ✓ | Body = base64 JPEG, `Content-Type: image/jpeg`. 8×8 JPEG accepted; 5 s later `/images` returned 640/300/60 CDN URLs. |
| DELETE | `/playlists/{id}/followers` (own) | playlist-modify-* | 200 | ✓ | Deletes (unfollows) the own playlist. It vanished from `/me/playlists` and contains → false. `GET /playlists/{id}` still returns 200 afterwards (Spotify never hard-deletes). |
| GET | `/browse/featured-playlists` | – | 403 | ✗ | No replacement. |
| GET | `/browse/categories/{id}/playlists` | – | 403 | ✗ | No replacement. |

## Search

| Method | Path | Scope | Status | Works | Notes |
|---|---|---|---|---|---|
| GET | `/search` | – | 200 | ⚠ | `limit` max **10** (11 and 50 → 400), default 5. `limit + offset` > 1000 → 400 "Limit + Offset exceeds maximum of 1000" (offset 990 + limit 10 OK). |
| | `type` combined `artist,album,playlist,track,show,episode,audiobook` | | 200 | ✓ | One call returns all 7 groups. |
| | `type=playlist` | | 200 | ⚠ | Some items are `null` (2 of 3 for "bonobo", 1 of 5 for "lofi"). Filter nulls. |
| | `total` | | 200 | ⚠ | Unreliable: "bonobo" tracks gave total 12 at limit 5, 20 at limit 10, 528 at offset 990. Do not show it as a count. |
| | `tag:new` (albums) | | 200 | ✓ | total 100. Usable as a new-releases substitute. |
| | `tag:hipster` (albums) | | 200 | ✓ | total 100. |
| | `year:2024`, `year:2020-2022 artist:bonobo` | | 200 | ✓ | |
| | `genre:techno` (artist) | | 200 | ⚠ | 1 result only. Genre filter is nearly useless now that artists have no genres. `genre:"deep house"` (track) returned results. |
| | `artist:… album:…` | | 200 | ✓ | |
| | `isrc:QMBZ92037935` | | 200 | ⚠ | 2 items returned with `total: 0`. |
| | `upc:609008295069` | | 200 | ✓ | 1 album. |
| | `type=show`, `type=episode` | | 200 | ✓ | |
| | `type=audiobook` | | 200 | ⚠ | Always 0 results (markets default/US/GB). |
| | `market=US`, `from_token`, `XX` | | 200 | ⚠ | Same results as no market. Invalid `XX` is not rejected. |
| | `include_external=audio` | | 200 | ✓ | No visible effect. |

## Player

| Method | Path | Scope | Status | Works | Notes |
|---|---|---|---|---|---|
| GET | `/me/player` | user-read-playback-state | 204 | ✓ | 204 with no body when nothing is active (at probe time). `additional_types`, `market` accepted. |
| GET | `/me/player/devices` | user-read-playback-state | 200 | ✓ | Returned `MacBook Pro` (Computer) and `Marantz STEREO 70s` (AVR), both `supports_volume: true`. |
| GET | `/me/player/currently-playing` | user-read-currently-playing | 204 | ✓ | 204 when idle. |
| GET | `/me/player/queue` | user-read-playback-state | 200 | ⚠ | `{currently_playing: null, queue: []}` when idle. Orchestrator: reads `[]` while a Spotify mix plays even after a successful add. |
| GET | `/me/player/recently-played` | user-read-recently-played | 200 | ⚠ | limit max 50 (51 → 400). History is capped at the last 50 plays: `before=<cursors.before>` returns 0 items. `after=<cursors.after>` returns 0 (nothing newer). `after=<now − 7 days>` returns the oldest plays. Rows `{track, played_at, context}`; `context` was `null`. |
| PUT | `/me/player` (transfer, `{device_ids, play}`) | user-modify-playback-state | 204 | ✓ | verified 2026-10-02 by orchestrator. |
| PUT | `/me/player/play` (`{context_uri: spotify mix}`) | user-modify-playback-state | 2xx | ✓ | verified 2026-10-02 by orchestrator. Plays a `37i9…` mix even though the playlist itself is 404 to read. |
| PUT | `/me/player/volume` | user-modify-playback-state | 204 | ⚠ | verified 2026-10-02 by orchestrator. Ignored by the desktop client while paused. Read-back lags 1–3 s. |
| PUT | `/me/player/shuffle`, `/me/player/repeat` | user-modify-playback-state | 200 | ✓ | verified 2026-10-02 by orchestrator. |
| POST | `/me/player/queue` | user-modify-playback-state | 200 | ⚠ | verified 2026-10-02 by orchestrator. Queue read stays `[]` during a Spotify mix. |
| PUT/POST | pause, next, previous, seek | user-modify-playback-state | – | untested | Not run (they make sound on real speakers). Orchestrator: player writes now return 200 with a snapshot-id text body instead of 204. Treat any 2xx as success and do not parse the body as JSON. |

## Categories / Genres / Markets

| Method | Path | Status | Works | Notes |
|---|---|---|---|---|
| GET | `/browse/categories` | 403 | ✗ | |
| GET | `/browse/categories/{id}` | 403 | ✗ | `toplists` and `0JQ5DAqbMKFQ00XGBls6ym`. |
| GET | `/recommendations/available-genre-seeds` | 404 | ✗ | Endpoint gone. |
| GET | `/markets` | 403 | ✗ | Use `GET /me` → `country` (still present). |

## Quirks & replacements

- `/playlists/{id}/tracks` → `/playlists/{id}/items`. Row field `track` → `item`. Playlist object field `tracks` → `items`. `fields=` filters must use `items(item(...))`.
- `DELETE /playlists/{id}/items` needs body key `items`, not `tracks` (else 400 "No uris provided").
- `/me/{tracks,albums,shows,episodes,audiobooks}` writes and `/contains` → `/me/library` + `/me/library/contains` with full `spotify:<type>:<id>` URIs.
- `/me/following` writes and `/contains` → `/me/library` with `spotify:artist:…` / `spotify:user:…`. The doc page for `PUT /me/library` does not list `artist`, but it works (verified, followed artists 11→12→11).
- `/playlists/{id}/followers` PUT/DELETE still work (200), despite the migration guide. `/playlists/{id}/followers/contains` is 403 → use `/me/library/contains?uris=spotify:playlist:…`.
- Following a Spotify mix via `/me/library` makes contains `[true]` but `/me/playlists` total stayed 5 after 4 s. Do not expect followed mixes in `/me/playlists`.
- `/me/library` returns 200 for a nonexistent track id. Always confirm with `/me/library/contains`.
- `GET /me/library` → 405. There is no unified list.
- Batch `?ids=` reads are all 403 → loop over single-item GETs (mind rate limits).
- `new-releases` → search `tag:new&type=album`.
- `artist top-tracks` → search `artist:"<name>"&type=track` (relevance order, not play count).
- `/markets` → `GET /me` `country`.
- `/artists/{id}/albums` limit max 10 (not 50).
- `/search` limit max 10 (not 50); `total` is not a real count; playlist results contain `null`s.
- `/me/audiobooks` returns `null` items in market ID.
- Playlist `public` flag does not reliably reflect `PUT /playlists/{id}` `public:false`.
- Album `is_playable` appears only with `market`; track `is_playable` appears without it.
- Recently-played history stops at 50 plays.
- `GET /me` still has `country/email/product/explicit_content/followers` despite the guide. Guess: the field removal was not rolled out to this app, or to any app. Not verified beyond this account.
- `external_ids` (`isrc`, `upc`) is still present on tracks/albums despite the guide.

## Blocked for development-mode apps

All return 403 unless marked 404:

- `GET /tracks?ids=`, `/albums?ids=`, `/artists?ids=`, `/shows?ids=`, `/episodes?ids=`, `/audiobooks?ids=`
- `GET /audio-features/{id}`, `GET /audio-features?ids=`, `GET /audio-analysis/{id}`
- `GET /recommendations` (404), `GET /recommendations/available-genre-seeds` (404)
- `GET /artists/{id}/top-tracks`, `GET /artists/{id}/related-artists`
- `GET /browse/new-releases`, `/browse/featured-playlists`, `/browse/categories`, `/browse/categories/{id}`, `/browse/categories/{id}/playlists`
- `GET /markets`
- `GET /users/{id}`, `GET /users/{id}/playlists`
- `PUT`/`DELETE` `/me/tracks`, `/me/albums`, `/me/shows`, `/me/episodes`, `/me/audiobooks`, `/me/following`
- `GET /me/{tracks,albums,shows,episodes,audiobooks}/contains`, `/me/following/contains`, `/playlists/{id}/followers/contains`
- `GET/POST/PUT/DELETE /playlists/{id}/tracks`
- `GET /playlists/{id}/items` for playlists not owned by the user (403), and `GET /playlists/{id}` + `/items` for Spotify-owned `37i9…` playlists (404)

## How to save/unsave (the working recipe)

Verified 2026-10-02 on track `1J14CdDAvBTE1AJYUOwl6C`: liked total 135 → 136 → 135, contains `[false]` → `[true]` → `[false]`.

```
# save (like) — URIs in the QUERY string, comma-separated, max 40, any mix of types
PUT  https://api.spotify.com/v1/me/library?uris=spotify:track:1J14CdDAvBTE1AJYUOwl6C
Authorization: Bearer <token>
(no body needed; empty body, `{}` body, or Content-Type: application/json all work)
→ 200, empty body

# unsave
DELETE https://api.spotify.com/v1/me/library?uris=spotify:track:1J14CdDAvBTE1AJYUOwl6C
→ 200, empty body

# check
GET  https://api.spotify.com/v1/me/library/contains?uris=spotify:track:1J14CdDAvBTE1AJYUOwl6C,spotify:album:…
→ 200, [true, false]
```

- Same recipe, verified, for `spotify:album:`, `spotify:show:`, `spotify:episode:`, `spotify:artist:` (follow), `spotify:user:` (follow), `spotify:playlist:` (follow).
- `spotify:audiobook:` is untested (no valid id reachable in market ID).
- `:` may be raw or `%3A`-encoded. Both verified.
- What fails: `uris` in a JSON body (400 "Missing required field: uris"); `ids` + `type` (400, same message); bare ids (400 "Invalid Spotify URI"); more than 40 URIs (400 "Too many uris requested").
- Effect is visible in `contains` and list totals within 4 s (the only delay tested).
- The earlier "200 but nothing changed" report could not be reproduced. Every query-string form changed state. The "Missing required field: uris" message only appeared when `uris` was absent from the query string, and it came with status 400, not 200. Guess: that earlier request sent `uris` in the body or lost the query string.
- Current app code still calls the dead endpoints: `src-tauri/src/spotify.rs:274` (`/me/tracks/contains`), `:280` (`PUT /me/tracks`), `:285` (`DELETE /me/tracks`).
