# The Run v2 — pick the speaker, more controls, fuller library, your taste

**Date:** 2026-10-02
**Scope:** ~/dev/rust-spotify
**Status:** shipped 2026-10-02 (as-built notes at the end)

## TL;DR

**1. Device picker.** The device name in the top bar becomes a button. Clicking it lists your Spotify devices (now: MacBook Pro, Marantz STEREO 70s), and picking one moves playback there. *Why:* the app followed whatever Spotify had active (the Marantz), and you couldn't switch. *You do:* nothing. *Does NOT:* make this app a speaker. It stays a remote. A device shows up only while Spotify runs on it.

**2. Volume, shuffle, repeat, heart.** The control row gets shuffle and repeat on either side of prev/play/next, a heart (save the playing song to Liked Songs) and a volume slider for the selected device. *Why:* "too minimalistic". *You do:* **reconnect Spotify once.** The heart, Liked Songs, Top and followed artists need 4 new permissions (`user-library-read`, `user-library-modify`, `user-top-read`, `user-follow-read`), and the app shows its Reconnect screen. *Does NOT:* change volume on a device that doesn't support remote volume. The slider hides there.

**3. Fuller Library.** The Library shows Liked Songs at the top, then your playlists, then saved albums. Each one opens to a track list with Play. *Why:* only your 5 playlists showed before. *You do:* nothing beyond the reconnect. *Does NOT:* show podcasts or audiobooks.

**4. Spotify mixes (best effort).** A "Spotify mixes" group lists the mixes you played recently (e.g. Bonobo Radio), with cover and name where Spotify allows, and plays them. *Why:* you asked for Daily Mix / Discover Weekly. *The hard limit, checked live:* Spotify blocks its own playlists for apps like this one. Search finds 0 of them, and their names and tracks return 404/403. Only mixes you have played (seen in history or playing now) can be found, and the app remembers them from then on. *You do:* nothing. I'll start one mix once on the **MacBook**, not the Marantz, to prove playback works. If it fails, the group stays hidden. *Does NOT:* list Daily Mix or Discover Weekly you haven't played, or show any mix's track list.

**5. Your Top.** A Library group "Your top" shows your top tracks and top artists, with tabs for 4 weeks, 6 months and all time. Tracks play; artists open their page. *Why:* it's the one personal-taste feed Spotify still opens to apps. *You do:* nothing beyond the reconnect. *Does NOT:* include recommendations or "related" (Spotify blocks both, checked live). It isn't confirmed open until after the reconnect; if it's blocked, the group hides.

**6. Artist pages + followed artists.** Click an artist name (now playing, any track row, Search, Your top) to open their page: photo, name, albums and singles, each playable. Library gets a "Following" group of the artists you follow. *Why:* artists are a dead end now. *You do:* nothing. *Does NOT:* show the artist's top tracks, related artists, genres or follower counts (all blocked or stripped by Spotify, checked live).

**7. Add to queue.** Every track row gets a "+" that puts the song into Up next without interrupting what's playing. *Why:* the only way to build what plays next. *You do:* nothing. *Does NOT:* reorder or remove queue items (no API for that).

## Problem(s)

1. **No way to choose the output.** The device chip is a plain `<div>`, display only (`src/index.html:46`). `renderChrome` only writes the device name into it (`src/app.js:450-452`). On first real run the app showed the Marantz with no way to switch to the MacBook.
2. **Transport is the bare minimum.** The control row is prev/play/next only (`src/index.html:66-74`). `playback_state` drops volume, shuffle, repeat and context (`src-tauri/src/spotify.rs:91-96`). The v1 spec listed volume as out of scope.
3. **Library shows 5 playlists and nothing else.** Only `get_playlists` feeds the sheet (`src/app.js:775-789`). The granted scopes have no library access (`src-tauri/src/auth.rs:16-24`): `/me/tracks` and `/me/albums` return 403 "Insufficient client scope" (checked live 2026-10-02).
4. **No taste, no artists, no queueing.** Artists are plain text in every row (`simplify_track` joins names into one string, `src-tauri/src/spotify.rs` `join_artists`). Top items and followed artists need scopes we don't hold (403 "Insufficient client scope", live 2026-10-02). Artist albums are open (200). Artist top tracks, related artists, new releases, recommendations and audio features are blocked (403/404). There is no add-to-queue command.
5. **Spotify's own mixes are invisible.** Daily Mix, Radio and Discover Weekly aren't in `/me/playlists`. Checked live 2026-10-02:
   - `/playlists/37i9dQZF1E…` returns 404, and `/items` returns 404.
   - `/tracks` returns 403, `/browse/*` returns 403, and `/users/spotify/playlists` returns 403.
   - Search returns 0 Spotify-owned playlists for 5 queries.
   - `/playlists/{id}/images` returned 200 for Bonobo Radio (its URL holds `radio/artist/<artistId>`) and 404 for another mix.
   - Recently-played rows carry the mix as `context.uri`. `parse_recent` throws that away (`src-tauri/src/spotify.rs` `parse_recent`).

## Goals

1. Pick any listed device and move playback to it in one click (P1).
2. Show and change volume, shuffle and repeat. Save or unsave the current song (P2).
3. Show Liked Songs, playlists and saved albums in Library, each playable from any row (P3).
4. Show every Spotify mix the app has seen, playable by URI, with the best name and cover Spotify allows (P4).
5. Show your top tracks and artists (3 ranges), artist pages with discography, followed artists, add-to-queue (P5–P6).
6. Keep v1's guarantees: ordered player commands, stale-session safety, seek guard (P1–P4).

## Non-goals

- Making the app a Spotify Connect speaker (Web Playback SDK). It needs Premium web-player licensing, and it isn't what was asked.
- Track lists for Spotify mixes. The API blocks them (problem 5).
- Podcasts and audiobooks, the DJ feature.
- Artist top tracks, related artists, recommendations, new releases (blocked by Spotify, checked live).
- A queue editor or drag-to-reorder.

## Device picker

```
top bar:                                     [● Marantz STEREO 70s ▾]
click →  ┌─────────────────────────────┐
         │ ● Marantz STEREO 70s   AVR  │  ← active, check mark
         │   MacBook Pro          Computer │
         │ ─────────────────────────── │
         │ Open Spotify on a phone or  │
         │ speaker to see it here.     │
         └─────────────────────────────┘
```

- `list_devices` returns `[{id, name, type, is_active, is_restricted, supports_volume, volume_percent}]`. Restricted devices are listed but disabled, with the tooltip "Spotify doesn't allow remote control of this device".
- Picking a device calls `transfer_playback(deviceId, play)`, where `play` = the current `isPlaying`. It runs through the player chain as a track-change-class command, so seeks are blocked until a poll after it lands (v1 seek guard).
- The popover opens on click or Enter/Space, refreshes `list_devices` on open, and closes on Esc, an outside click, or a pick. Arrow keys move between rows.

## Transport additions

```
   0:44 ━━━━━━━━━●──────────────────────── 3:12
   ⤮   ⏮   ⏯   ⏭   ↻          ♡        🔈 ───●────
```

- `playback_state` adds `shuffle: bool`, `repeat: "off"|"context"|"track"`, `volume_percent: int|null`, `supports_volume: bool`, `context_uri: string|null`, and `track.id` (already there).
- **Shuffle**: toggles `set_shuffle(state)`. **Repeat**: cycles off → context → track → off through `set_repeat(mode)`. Both are optimistic, with the same pending-intent guard as play/pause (v1 round 19).
- **Volume**: the slider is focusable, arrows step ±5. The UI moves at once, and 1 `set_volume(percent)` is sent after 200ms of quiet, through the player chain. Polls don't overwrite it while it's pending or for 500ms after. It's hidden when `supports_volume` is false.
- **Heart**: on each track change, `is_saved(trackId)` runs once. A click flips it at once, then calls `save_track` / `unsave_track`. On failure it reverts with a toast. It's hidden while the scopes are missing, and for ads, podcasts and local files.

## Library groups

```
Library
  ♥ Liked Songs              1 234 songs
  Playlists
    Mia                       16 tracks
    …
  Albums
    I See You — The xx        11 tracks
  Spotify mixes
    Bonobo Radio
    Spotify mix
```

- **Liked Songs**: `get_saved_tracks()` returns `[Track]`, following `next`, capped at the newest 1000. A cap note shows when more exist. Level 2 reuses the playlist detail.
- **Albums**: `get_saved_albums()` returns `[{id, name, artists, cover, total_tracks}]`. A row opens the existing album detail (`get_album_tracks`).
- **Spotify mixes**: the frontend keeps `knownMixes` (`[{id, firstSeen}]`, newest first, max 30) in localStorage. It's fed from:
  1. `get_recently_played` rows' `context_uri` (newly kept by `parse_recent`)
  2. `playback_state.context_uri`

  Only `spotify:playlist:` URIs that are not in `/me/playlists` count. `mix_info(id)` returns `{name, cover}`:
  - `cover` comes from `/playlists/{id}/images` (null when that 404s).
  - `name` is `"<Artist> Radio"` when the image URL contains `radio/artist/<id>` (artist name from `/artists/<id>`), else `"Spotify mix"`.

  Level 2 shows the cover, the name, Play, and the line "Spotify doesn't share the track list of its own mixes." Play calls `play_context(deviceId, contextUri)`.

## Your top, artists, queue

- `Track` gains `artist_list: [{id, name}]`. The old `artists` string stays for display. Each name in a row or the now block becomes a link.
- **Your top**: `get_top(kind: "tracks"|"artists", range: "short_term"|"medium_term"|"long_term")` returns ≤20 items. The Library group has a 3-tab switch (4 weeks / 6 months / all time). Tracks render as track rows; artists as round tiles that open the artist page.
- **Artist page** (Library level 2, kind `artist`): `get_artist(id)` returns `{id, name, image}` and `get_artist_albums(id)` returns `[{id, name, cover, year, kind: "album"|"single"}]` (both groups, newest first, ≤50). An album tile opens the album detail. Back returns to wherever you came from.
- **Following**: `get_followed_artists()` returns `[{id, name, image}]` (follows `next`, ≤200), shown as round tiles.
- **Add to queue**: `add_to_queue(uri)` → `POST /me/player/queue?uri=`. It runs through the player chain. Toast "Added to Up next". On the next queue refresh the run shows it. Hidden for local files.

## Backend API additions (Tauri commands)

| command | args | returns |
|---|---|---|
| `transfer_playback` | `{deviceId, play}` | `null` |
| `set_volume` | `{percent}` | `null` |
| `set_shuffle` | `{on}` | `null` |
| `set_repeat` | `{mode}` | `null` |
| `get_saved_tracks` | — | `{tracks: [Track] (newest ≤1000), total: int}` |
| `get_saved_albums` | — | `[{id,name,artists,cover,total_tracks}]` |
| `is_saved` | `{trackId}` | `bool` |
| `save_track` / `unsave_track` | `{trackId}` | `null` |
| `play_context` | `{deviceId, contextUri}` | `null` |
| `mix_info` | `{playlistId}` | `{name, cover}` |
| `get_top` | `{kind, range}` | `[Track]` or `[{id,name,image}]` |
| `get_artist` | `{artistId}` | `{id, name, image}` |
| `get_artist_albums` | `{artistId}` | `[{id,name,cover,year,kind}]` |
| `get_followed_artists` | — | `[{id,name,image}]` |
| `add_to_queue` | `{deviceId, uri}` | `null` |

Changed: `playback_state` (5 new fields), `list_devices` (keeps `is_restricted`, `supports_volume`, `volume_percent`, `type`), `get_recently_played` (adds `context_uri`). Scopes add `user-library-read`, `user-library-modify`, `user-top-read`, `user-follow-read`. `auth_status` then returns `"reconnect"` for the current token, which is expected.

Endpoint paths (`/me/player`, `/me/player/volume`, `/me/player/shuffle`, `/me/player/repeat`, `/me/tracks`, `/me/tracks/contains`, `/me/albums`) get verified with one live GET each before coding the write side. A rename since 2024, as happened with `/items`, gets handled the same way as v1.

## File-level changes

| file | change |
|---|---|
| `src-tauri/src/auth.rs` | Add the 4 scopes to `REQUIRED_SCOPES`. Scope test updated. |
| `src-tauri/src/spotify.rs` | 15 new commands (table above). `simplify_track` adds `artist_list`. Extend `playback_state`, `parse_recent` and `list_devices`. Pure parsers `parse_saved_albums`, `mix_name_from_image` with tests. |
| `src-tauri/src/lib.rs` | Register the new commands. |
| `src/index.html`, `dev/index.html` | Device chip becomes a `<button>` plus a popover. Shuffle/repeat/heart/volume in `.controls`. Library groups. Mix detail note. |
| `src/styles.css` | Popover, the 4 new controls, group headers, cap and mix notes. Phone width unchanged (800px breakpoint: volume collapses to an icon that toggles a vertical slider). |
| `src/app.js` | Artist links, artist page (level 2 kind `artist`), Your top tabs, Following tiles, "+" queue action. Device picker, shuffle/repeat/volume/heart state with intent guards, library groups, mix discovery and storage, reconnect copy generalized ("Spotify needs new permissions"). |
| `src/lib/mixes.js` (+ test) | Pure: `noteMixes(known, contextUris, ownPlaylistIds, now) → known'` (dedupe, newest first, cap 30). |
| `dev/mock.js`, `dev/fixture.json` | Handlers for all new commands. 2 devices (one restricted variant). Liked songs, saved albums, 2 mixes. New scenarios `devices`, `library-full`, `mix-detail`. |

## Tests

- **Rust unit**:
  - `parse_recent` keeps `context_uri`
  - `parse_saved_albums`
  - `mix_name_from_image` (radio URL → artist id, other → none)
  - `playback_state` field mapping via a pure `simplify_state`
  - scope list includes the 2 new scopes
- **JS unit** (`mixes.test.js`): dedupe, own-playlist exclusion, cap 30, newest first, non-playlist URIs ignored.
- **Harness QA** (Sonnet, `/playwright-cli`, read-only), 1440×900 and 800×600:
  - picker opens/closes/keyboard, transfer reorders the active device
  - volume slider plus arrows, hidden for no-volume devices
  - shuffle/repeat cycle
  - heart flips, and reverts on injected failure
  - Library groups render; Liked/album/mix detail; mix Play sends `play_context`
  - Your top tabs switch; artist links open the artist page; Back returns; Following tiles open it
  - "+" sends `add_to_queue` and shows the toast
  - no horizontal scroll, no console errors
- **Live, once** (orchestrator, with this spec's approval as consent):
  - GET checks of the new endpoints
  - `transfer_playback` to the MacBook
  - `play_context` of one known mix on the MacBook
  - `set_volume` on the MacBook
  - GET `get_top` and `get_followed_artists` after your reconnect (confirms they're open)
  - `add_to_queue` of one song on the MacBook
- **Regression**: v1's 21 interaction scripts and 24-shot sweep stay green.
- **Exit**: Astra loop until 0 crit/high, then `/simplify`, then a final Astra round.

## Failure modes & decisions

| failure | behaviour |
|---|---|
| Token lacks new scopes | Reconnect screen ("Spotify needs new permissions for your library"). Nothing else breaks. |
| Picked device vanished (404 on transfer) | Toast "<name> isn't available any more". Device list refreshes. |
| Restricted device | Listed, disabled, with tooltip. |
| `supports_volume` false | Slider hidden. |
| Volume/shuffle/repeat command fails | UI reverts. Toast "Spotify didn't respond: …". |
| `play_context` on a mix 403/404s | Toast "Spotify won't start this mix from here". Mix is dropped from `knownMixes`. |
| Mix images 404 | Letter tile, name "Spotify mix". |
| Liked Songs > 1000 | Newest 1000 shown, plus the note "Showing your newest 1000 of N". |
| localStorage unavailable | Mixes group shows only what this session saw. |
| Heart on local file / ad / podcast | Heart hidden. |
| `get_top` / followed artists 403 after reconnect | That group hides. No error toast. |
| Artist has no image | Round letter tile. |
| `add_to_queue` fails (no active device) | Standard NO_ACTIVE_DEVICE path: rediscover once, then toast. |

## Out of scope

- Spotify Web Playback SDK / this app as a speaker
- Mix track lists, Discover Weekly or Daily Mix not yet played
- Podcasts, DJ, queue reorder/remove
- Follow/unfollow artists (read-only Following)
- Volume on devices that don't support remote volume

## Rollout

- **P1**: backend commands + scopes + device picker (live check: transfer to MacBook).
- **P2**: shuffle/repeat/volume/heart (live check: volume).
- **P3**: Library groups: Liked Songs, albums.
- **P4**: Spotify mixes (live check: one `play_context` on MacBook).
- **P5**: artist links + artist pages + Following.
- **P6**: Your top + add to queue.
- **P7**: QA sweep, fixes, Astra loop, `/simplify`, final Astra, merge.

## As-built notes (2026-10-02)

What shipped differs from the text above in these places:

- **All 19 standard Spotify scopes at login** (user: "ask now for ALL POSSIBLE permission upfront"). This is not the 4 new ones the TL;DR named. Partner-only scopes are left out, because Spotify rejects the login if an app asks for one. No future feature needs another reconnect.
- **Heart uses `/me/library`**. `PUT` and `DELETE /me/library?uris=spotify:track:<id>` and `GET /me/library/contains?uris=` replace the 403'd `/me/tracks*` endpoints (Feb 2026 change). The URIs must be in the query string; a JSON body gives 400.
- **Artist pages show "Your favorites"**: this artist's songs among your top tracks (3 ranges, 50 each) and your Liked Songs. It replaces the planned "artist top tracks", which is 403. Spotify also exposes no play counts or popularity any more. Albums render first, and favorites fill in after.
- **Artist albums** are fetched 10 per page (Spotify's new max; 11+ returns 400), up to 50.
- **Volume** settles for 2.5s, not 500ms, because the read-back lags 1–3s (measured live). Holds are per device, and the command carries `device_id`.
- **The Liked Songs count** is one request (`liked_count`). The list (up to 1000) loads only when opened. A heart click patches the cached list in place.
- **Player 404s** become `NO_ACTIVE_DEVICE` only when Spotify's body is about the device. Any other player 404 (e.g. a refused mix) stays a plain error.
- **Release profile** is tuned for size (`opt-level="s"`, LTO, strip, `panic="abort"`): binary 15.2 → 5.4 MB. The app's memory footprint is about 160 MB on the stage (app 34 MB plus WebKit), flat over time.
- **`docs/spotify-web-api-reality.md`** records every endpoint as tested live on 2026-10-02. It lists what works, what's blocked, and where the official docs are wrong.
- **Review rule** (user): fix only crit/high review findings. Med/low are listed, not fixed.
- **Not done:** add to playlist, follow/save buttons on artist and album pages, podcasts. All of these are possible (see the API doc), but they weren't approved for this round.
