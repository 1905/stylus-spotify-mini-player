# The Run v2 Implementation Plan v1.0

**Date:** 2026-10-02
**Status:** done 2026-10-02. T0-T11 complete; Astra v2 rounds 1-5 + final + verification (0 crit/high), /simplify, spec as-built notes, merged to main.
**Spec:** ./spec.md
**Goal:** Ship the 7 TL;DR blocks of the spec: device picker, volume/shuffle/repeat/heart, fuller Library, Spotify mixes, Your top, artist pages + Following, add to queue.
**Architecture:** The Rust backend gains 15 thin Web API commands and richer `playback_state` / `parse_recent` / `list_devices` / `simplify_track`. The frontend extends `app.js`. New UI goes through the v1 player chain (`withDevice` / `changeTrack`) and its guards (auth session, track generation, pending intent). One new pure module, `src/lib/mixes.js`. The dev harness mocks every new command.
**Tech Stack:** Rust (tauri 2, reqwest), vanilla JS ES modules, vitest, playwright-cli (QA).

> For agentic workers: use superpowers:subagent-driven-development to implement task-by-task. Checkbox syntax for tracking.

**Exec notes:**
- Branch `feat/the-run-v2` in the main checkout. User rule: no worktrees unless asked.
- Implementers: Opus only. They do no git and no live Spotify calls.
- The orchestrator commits after each verified phase, and runs every live call (consented in the spec).
- Live checks of `/me/tracks`, `/me/albums`, `/me/top`, `/me/following` need the user's reconnect, so they are deferred to the final report.

## File map

**Modify:**
- `src-tauri/src/auth.rs`, `src-tauri/src/spotify.rs`, `src-tauri/src/lib.rs`
- `src/index.html`, `dev/index.html`, `src/styles.css`, `src/app.js`
- `dev/mock.js`, `dev/fixture.json`

**Create:**
- `src/lib/mixes.js`, `src/lib/mixes.test.js`

**Out of scope:** Web Playback SDK, podcasts, queue editing, follow/unfollow.

## Locked interfaces

**Track (changed):** `{id, uri, name, artists, artist_list:[{id,name}], album, cover, duration_ms}`

**playback_state (active):** adds these keys to the v1 object:
- `shuffle: bool`
- `repeat: "off"|"context"|"track"`
- `volume_percent: int|null`
- `supports_volume: bool`
- `context_uri: string|null`

**get_recently_played rows:** `{track, played_at, context_uri: string|null}`

**list_devices rows:** `{id, name, type, is_active, is_restricted, supports_volume, volume_percent}`

**New commands** (JS camelCase args → Rust snake_case):

| command | JS args | returns | Web API |
|---|---|---|---|
| `transfer_playback` | `{deviceId, play}` | null | PUT /me/player `{device_ids:[id], play}` |
| `set_volume` | `{percent}` | null | PUT /me/player/volume?volume_percent= |
| `set_shuffle` | `{on}` | null | PUT /me/player/shuffle?state= |
| `set_repeat` | `{mode}` | null | PUT /me/player/repeat?state= |
| `get_saved_tracks` | — | `{tracks:[Track], total}` | GET /me/tracks?limit=50, follows `next`, ≤1000 |
| `get_saved_albums` | — | `[{id,name,artists,cover,total_tracks}]` | GET /me/albums?limit=50, follows `next`, ≤200 |
| `is_saved` | `{trackId}` | bool | GET /me/tracks/contains?ids= |
| `save_track` / `unsave_track` | `{trackId}` | null | PUT / DELETE /me/tracks?ids= |
| `play_context` | `{deviceId, contextUri}` | null | PUT /me/player/play?device_id= `{context_uri}` |
| `mix_info` | `{playlistId}` | `{name, cover}` | GET /playlists/{id}/images (+ /artists/{id}) |
| `get_top` | `{kind, range}` | `[Track]` or `[{id,name,image}]` | GET /me/top/{kind}?time_range=&limit=20 |
| `get_artist` | `{artistId}` | `{id,name,image}` | GET /artists/{id} |
| `get_artist_albums` | `{artistId}` | `[{id,name,cover,year,kind}]` | GET /artists/{id}/albums?include_groups=album,single&limit=50 |
| `get_followed_artists` | — | `[{id,name,image}]` | GET /me/following?type=artist&limit=50, cursor `after`, ≤200 |
| `add_to_queue` | `{deviceId, uri}` | null | POST /me/player/queue?uri=&device_id= |

- Errors use the v1 codes: `AUTH_EXPIRED`, `NO_ACTIVE_DEVICE`, and free text otherwise.
- Every player write (`transfer_playback`, volume, shuffle, repeat, `play_context`, `add_to_queue`) goes through `withDevice`.
- `transfer_playback` and `play_context` go through `changeTrack`.

**Pure Rust fns (tested):**
- `simplify_track` (adds `artist_list`)
- `simplify_state(&Value) -> Value`, the playback_state mapping
- `parse_recent` (adds `context_uri`)
- `parse_saved_albums(&Value) -> Vec<Value>`
- `simplify_artist(&Value) -> Value`
- `parse_artist_albums(&Value) -> Vec<Value>`
- `mix_artist_id(image_url: &str) -> Option<String>`: the segment after `radio/artist/`

**JS lib:** `mixes.js` exports `noteMixes(known, contextUris, ownPlaylistIds, nowIso) → known'`:
- keeps only `spotify:playlist:` URIs not in own playlists
- moves an already-known one to the front with a new `seen` time
- newest first, capped at 30
- item shape: `{id, seen}`

**localStorage key:** `therun.knownMixes`. Wrap reads and writes in try/catch.

**DOM ids (new):**
- device: `#deviceBtn` (replaces `#device`, keeps `.device-name`), `#devicePop`, `#deviceList`
- transport: `#shuffleBtn`, `#repeatBtn`, `#heartBtn`, `#volume` (wraps), `#volBtn`, `#volSlider` (role=slider), `#volFill`
- Library groups: `#libLiked`, `#libTop`, `#topTabs`, `#topTracks`, `#topArtists`, `#libAlbums`, `#libFollowing`, `#libMixes`
- detail: `#detailNote`
- artist links: `.artist-link` with `data-artist`

**Dev scenarios (new):**
- `devices`: 2 devices + 1 restricted, picker open
- `library-full`: all Library groups
- `artist`: an artist page open
- `mix-detail`
- `no-volume`: the active device doesn't support volume

## Tasks

### T0: Baseline + live GET checks (gate, orchestrator)
- [ ] `cargo test` 18/18, `npm test` 28/28, `cargo build` with no warnings.
- [ ] Live GET `/me/player` and `/me/player/devices`. Record the real field names for volume, shuffle, repeat, context and supports_volume, then fix the locked shapes if they differ.

### T1: Backend (heavy, Opus)
**Files:** `src-tauri/src/{auth,spotify,lib}.rs`
- [ ] Failing tests first:
  - scope list contains the 4 new scopes
  - `simplify_track` gives `artist_list`
  - `simplify_state` maps the 5 new fields, including `repeat_state` → `repeat`
  - `parse_recent` gives `context_uri`
  - `parse_saved_albums`
  - `simplify_artist` (image = first, else null)
  - `parse_artist_albums` (`kind` from `album_group` or `album_type`, `year` = first 4 chars)
  - `mix_artist_id` (radio URL → Some, other → None)
- [ ] Implement the 15 commands with the existing `request` / `get` / `command` / `all_items` helpers. `get_followed_artists` uses cursor paging (`artists.cursors.after`).
- [ ] Register them in `lib.rs`.
- [ ] Verify: `cargo test` all pass, `cargo build` with 0 warnings.

### T2: Mock + fixture (light→heavy, Opus)
**Files:** `dev/mock.js`, `dev/fixture.json`
- [ ] Handlers for all 15 commands. They keep state, so set → playback_state reflects it.
- [ ] Fixture data:
  - 2 devices: MacBook Pro (Computer, supports volume) and Marantz (AVR, active, supports volume)
  - liked: 60 tracks from existing fixture tracks, total 1234
  - saved albums: 3
  - top tracks and artists: built from fixture data
  - 1 artist with 6 albums
  - mixes: 2 context URIs in recent rows. mix_info gives Bonobo Radio with a cover, plus 1 with no cover
- [ ] Add the 5 new scenarios. `node --check dev/mock.js`.

### T3: Device picker (heavy, Opus)
**Files:** `src/index.html`, `dev/index.html`, `src/styles.css`, `src/app.js`
- [ ] The chip becomes `<button id="deviceBtn" aria-haspopup="listbox" aria-expanded>`. `#devicePop` holds `#deviceList`.
- [ ] On open, refresh `list_devices`. Rows show a dot for active, the name and a type label. Restricted rows are disabled, with a tooltip.
- [ ] Picking a row runs `changeTrack(() => invoke("transfer_playback", {deviceId, play: state.isPlaying}))`, sets `state.device` optimistically, then kicks.
- [ ] If transfer fails with 404/NO_ACTIVE_DEVICE, show the toast "<name> isn't available any more", refresh the list, and put `state.device` back.
- [ ] Keys: Esc and outside click close it; ArrowUp/ArrowDown move; Enter picks.
- [ ] Empty list: "Open Spotify on a phone, computer or speaker to see it here."
- [ ] The picker is not an overlay and doesn't make the stage inert. It closes when any overlay opens.

### T4: Shuffle, repeat, volume, heart (heavy, Opus)
**Files:** same as T3
- [ ] State: `shuffle`, `repeat`, `volume`, `supportsVolume`, `saved` (bool|null).
- [ ] Polls apply each field unless an intent is pending for it. Generalise v1's `playPending` / `playSettleAfter` into a small `intent(key)` helper used by play, shuffle, repeat and volume. Same semantics: pending count, plus a 500ms settle after the last one lands.
- [ ] Shuffle toggles. Repeat cycles off → context → track → off. Both are optimistic and revert on failure.
- [ ] Volume: slider click/drag/arrows (±5), Home/End. The UI moves at once, and `set_volume` is sent after 200ms of quiet. Hidden when `!supportsVolume`. At 800px it collapses to `#volBtn`, which toggles a popover slider.
- [ ] Heart: on track change, `is_saved` once, guarded by track generation. A click flips it optimistically and calls save/unsave. Failure reverts with a toast.
- [ ] Hidden when mode ≠ track, for local files, and when `is_saved` returned 403 (scopes missing).

### T5: Library groups, Liked, Albums (heavy, Opus)
**Files:** same + level-2 kinds
- [ ] Level 1 order: Liked Songs row, Your top, Playlists, Albums, Following, Spotify mixes. Each group loads independently. A 403 hides that group.
- [ ] Detail kinds:
  - `liked`: tracks + the cap note "Showing your newest 1000 of N"
  - `album`: existing
  - `mix`: Play only, plus `#detailNote` "Spotify doesn't share the track list of its own mixes."
  - `artist`: see T6
- [ ] Play: `playUris` (liked/playlist/album) or `changeTrack(play_context)` (mix).

### T6: Artist links, artist page, Following, Your top, add to queue (heavy, Opus)
**Files:** same
- [ ] `artist_list` renders as `.artist-link` buttons in the now block, track rows and search rows. A click opens `openDetail({kind:"artist", id})`. Clicks on a link inside a track row don't play the row.
- [ ] The artist page shows a round image, the name, and album/single tiles (kind badge). A tile opens the album detail. Back goes to the previous level (a stack, max 5).
- [ ] Your top: tabs 4 weeks / 6 months / all time (`short_term` / `medium_term` / `long_term`). Each tab's results are cached per session. Track rows plus round artist tiles.
- [ ] Following: round tiles.
- [ ] "+" on every playable track row: `withDevice(add_to_queue)`, then the toast "Added to Up next", then a queue refresh on the next tick. Hidden for local files.

### T7: Spotify mixes (heavy, Opus)
**Files:** `src/lib/mixes.js` + test, `src/app.js`
- [ ] TDD for `noteMixes`: dedupe, own-playlist exclusion, cap 30, newest first, non-playlist ignored.
- [ ] Feed it from history rows and the poll's `context_uri`. Persist to `therun.knownMixes`.
- [ ] The group renders tiles with `mix_info` covers and names, cached per session.
- [ ] Play via `play_context`. On 403/404: toast "Spotify won't start this mix from here", and drop the mix from the known list.

### T8: Reconnect copy (light, orchestrator)
- [ ] The `reconnect` LOGIN_COPY title becomes "Reconnect Spotify for your library", and the sub becomes "Spotify needs a few new permissions. It takes a few seconds."

### T9: Gate (orchestrator)
- [ ] `cargo test`, `cargo build` (0 warnings), `npm test`, `node --check`.
- [ ] Re-run all 21 v1 harness scripts plus the 24-shot sweep. They may need selector updates for `#deviceBtn`.
- [ ] Sonnet QA (read-only, `/playwright-cli`) on the new scenarios at 1440×900 and 800×600, against the spec's QA list.
- [ ] Fix loop: Opus for big fixes, the orchestrator for small ones.

### T10: Live checks (orchestrator, consented)
- [ ] Note the current device, track and play state first.
- [ ] Transfer to the MacBook. Run `set_volume` (restore the old value after). Add one song to the queue. Run `play_context` of a known mix.
- [ ] Transfer back to the original device, with the original play state.
- [ ] Report anything that fails.

### T11: Exit gate
- [ ] Astra loop until 0 crit/high (verify + fix every confirmed finding). Then `/simplify`, then a final Astra round.
- [ ] Reconcile the spec (As-built notes), set plan status to done, move the dir to `plans/done/`, merge to `main`.
- [ ] Final report + `/notify` (the user must reconnect Spotify), then stop the watchdog cron.

## Type-consistency check
- Command names in the locked table = T1 = T2 mock = T3–T7 usage.
- Arg names: `deviceId`, `play`, `percent`, `on`, `mode`, `trackId`, `contextUri`, `playlistId`, `kind`, `range`, `artistId`, `uri`.
- Rust params: `device_id`, `play`, `percent`, `on`, `mode`, `track_id`, `context_uri`, `playlist_id`, `kind`, `range`, `artist_id`, `uri`.
- `artist_list` is the same name in Rust `simplify_track`, the fixture, and the T6 renderer.
