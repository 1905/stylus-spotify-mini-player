# Snappy Implementation Plan v1.1

**Date:** 2026-10-02
**Status:** done 2026-10-02
**Spec:** ./spec.md
**Goal:** Resume the last session paused on launch; local (no-cloud) controls and loads on "The Run"; parallel + cached lists; clear loaders.
**Architecture:**
- Rust gets 7 `local_*` commands that drive librespot's Spirc directly (`play`, `pause`, `set_position_ms`, `next`, `prev`, `set_volume`, `load`).
- The engine gets a persisted device id, and exposes it in its status.
- JS routes transport calls to the local commands when the selected device is the engine's own, else to the Web API as today.
- Resume runs in JS after the engine is ready, from Spotify's player state or a localStorage "last session".
- Lists come from a Rust disk cache first, keyed by `snapshot_id` where possible, and the Rust side fetches pages in parallel.
- Loaders are CSS skeletons plus a pending-play state machine in JS.

**Tech Stack:** Rust (tauri 2.12, librespot-connect 0.8.0 `LoadRequest`/`LoadRequestOptions`/`PlayingTrack`, tokio, `futures` 0.3), vanilla JS, vitest, playwright-cli.

> For agentic workers: use superpowers:subagent-driven-development to implement task-by-task. Checkbox syntax for tracking.

**Exec notes:**
- Branch `feat/snappy` in the main checkout (no worktrees).
- Implementers are Opus: no git, no live Spotify calls, and no running the app.
- The orchestrator commits each verified task and does all live checks.
- Review rule: fix crit/high only.

## File map

**Modify:**
- `src-tauri/src/player.rs`, `src-tauri/src/spotify.rs`, `src-tauri/src/lib.rs`, `src-tauri/Cargo.toml`
- `src/app.js`, `src/styles.css`, `src/index.html`, `dev/index.html`
- `dev/mock.js`

**Create:**
- `src-tauri/src/cache.rs`
- `src/lib/transport.js` additions or `src/lib/route.js`, `src/lib/session.js`, `src/lib/pending.js`, each with a vitest file

## Locked interfaces

### Rust commands (new or changed)

| command | args (JS) | returns | notes |
|---|---|---|---|
| `engine_status` (changed) | — | adds `device_id: string \| null` | null until `ready` |
| `local_play` / `local_pause` / `local_next` / `local_prev` | — | null | `Err("ENGINE_NOT_READY: …")` when no Spirc |
| `local_seek` | `{positionMs}` | null | u32 ms |
| `local_volume` | `{percent}` | null | 0–100 → `u16` = round(percent × 65535 / 100) |
| `local_load` | `{contextUri?, uris?, trackUri?, positionMs, play}` | null | **Sends `spirc.activate()` first, then `spirc.load(..)`** on the same ordered command channel: librespot-connect 0.8.0 ignores every command, Load included, while the device is inactive (`spirc.rs:660`). The returned Ok only means "queued"; the JS pending-play machine confirms the track really loaded (Astra #1). | Exactly one of `contextUri` / `uris` (≤ 200). `trackUri` → `PlayingTrack::Uri`, `positionMs` → `seek_to`, `play` → `start_playing`. Built with `LoadRequest::from_context_uri` / `from_tracks`. Err as above; Err `BAD_ARGS` if both or neither of `contextUri` and `uris` are given |
| `cache_get` | `{account, key}` | `Value \| null` | Disk cache read; never errors (corrupt → null) |
| `me_id` | — | `string` | `/me` id (account scoping, lastSession.accountId) |
| `get_playlist_tracks` (changed) | `{playlistId, snapshotId?}` | `[Track]` | With a `snapshotId` and a cache hit for `playlist:<id>:<snapshotId>`, it returns from cache with no request. Writes the cache on success |
| `get_album_tracks` (changed) | `{albumId}` | `[Track]` | Cached forever under `album:<id>` |
| `get_saved_tracks` / `get_saved_albums` / `get_followed_artists` / `get_top` / `get_playlists` (changed) | same | same | Always fetch, then write the cache under `liked`, `albums`, `following`, `top:<kind>:<range>:<limit>`, `playlists` |

### Engine behaviour

- **Device id:** read `player-device-id` from the app dir. If it's missing, generate a UUID and write it with `auth::write_private`. Pass it to `SessionConfig { device_id, .. }`. A stable id lets Spotify treat the app as the same device across launches.
- **Status:** `engine_status.device_id` is the stable id once `ready`, and null before.

### Parallel paging (spotify.rs)

- `async fn pages_parallel(first: Value, path_for_offset: impl Fn(usize) -> String, page_size, max_items, concurrency = 4) -> Result<Vec<Value>, String>`.
- It reads `total` from the first page, then fetches the remaining offsets with at most `concurrency` requests in flight, and returns the items in offset order.
- Any page error fails the whole call.
- `get_playlist_tracks` must add `total` to its `fields` projection (`total,next,items(...)`) (Astra med #5): without it the helper has no count. If `total` is missing anyway, fall back to the serial `next` walk. Test the real request path string.
- Used by `get_playlist_tracks` (page 50; limit 100 returns 403), `get_saved_tracks` (≤ 1000) and `get_saved_albums` (≤ 200).
- `get_followed_artists` stays cursor-paged and serial.

### Cache (cache.rs)

- **Account-scoped (Astra #4):** every key is prefixed with the verified account id, `<accountId>/…`.
  - JS learns the id once from a new `me_id` command (`/me` id, cached in memory for the session).
  - JS never reads the cache before that id is known. The id is captured when each request starts and passed as `account` to every cached command.
  - On logout/account change, memory caches are dropped; disk entries of another account are simply never read.
  - **Test:** switching accounts with a failed refresh shows no rows from the other account.

- **Location:** `<app_dir>/cache/lists/<sha1(key)>.json`, written with `write_private`. The body is `{key, saved_at, value}`.
- **Interface:**
  - `get(key) -> Option<Value>`: reads the file and checks that the stored key equals the requested key.
  - `put(key, &Value)`.
- **Eviction:** after each `put`, if the total size of the dir is over 50 MB, delete files oldest-mtime-first until the total is ≤ 40 MB.
- **Errors:** every I/O error is logged and ignored; the cache never fails a command.

### JS: routing (transport)

- `isLocal(deviceId)` is true when `engine.state === "ready"`, `deviceId === engine.device_id` **and the last poll's active device is that id**. Ready means connected, not active: Spirc ignores play/pause/seek/next/prev/volume while inactive (Astra #1). An inactive The Run goes through the Web API (transfer/play) or `local_load`, which activates.
- `deviceId` is the device the command was **captured for**, not `state.device` at send time. Volume passes its captured `volDevice` (Astra med #6, free with this signature).
- `togglePlay` / `skip` / `seekTo` / volume send:
  - When `isLocal()`, invoke `local_play`/`local_pause` / `local_next`/`local_prev` / `local_seek` / `local_volume`.
  - Otherwise use the existing Web API invoke.
  - This stays inside the existing `withDevice` / `changeTrack` / `sendIntent` queues, so ordering and stale-session guards still apply.
  - A local call that fails with `ENGINE_NOT_READY` retries once on the Web API path.
- **Volume:** with `isLocal()`, the debounce is 30 ms (still coalesces a drag) and the settle lag is 1000 ms. Remote keeps 200 ms and 2500 ms.
- **`playUris` / `playMix`:** with `isLocal()`, use `local_load({uris | contextUri, trackUri: first, positionMs: 0, play: true})` instead of `play_on_device` / `play_context`.

### JS: resume

- **localStorage `therun.lastSession`:** `{accountId, contextUri, origin, uris, trackUri, positionMs, savedAt}`.
  - **`origin`** (Astra #10, the user's "same playlist") is `{kind: "playlist"|"album", id}` when the user started playback from a detail view. `playFrom` receives it from `openDetail`'s `curDetail`. On resume it becomes `contextUri = spotify:<kind>:<id>` with `trackUri`, so playback continues through the whole playlist, not a fragment.
  - **`uris`:** kept only for sources without a playlist/album (search hits, top tracks), capped at 200.
  - **Provenance** (Astra med #7, cheap here): when the poll shows a track that isn't in the saved source, the source fields are replaced by the poll's `context_uri`, or by `{uris: [track]}` when it has none.
- **When it's written:**
  - when a play starts from the app (`playFrom` / `playMix` / `play_context`)
  - from the poll, at most every 10 s while a song is current: it updates `trackUri` and `positionMs`, and takes `contextUri` from the poll whenever it's non-null
  - on pause
  - on `beforeunload`
- **When it runs:** once per app launch, after `engine.state === "ready"` and the first poll has completed.
  - **Skip** if `playback_state` shows `is_playing` on any device.
  - **Choose `src`:** `resumeSource(playbackState, lastSession)` normalizes to exactly one source (Astra #2).
    - **Paused somewhere, with `context_uri`:** `{contextUri, trackUri, positionMs}`.
    - **Paused somewhere, without a context:** if `lastSession.origin` (see below) or `lastSession.uris` contains the track, use that. Else `{uris: [trackUri], trackUri, positionMs}`.
    - **Nothing on Spotify:** `lastSession`, as saved.
    - **Nothing at all:** null, so do nothing.
    - **Tests:** paused with a context; paused without a context, with and without a matching saved list; no state.
  - **Load:** `local_load({...src, play: false})`, then `pickDevice`-style UI selection of "The Run" without a transfer call. The load itself makes it active.
  - **Failure:** ignore it quietly, start idle.
- **Login/logout:** `showLogin` doesn't clear `lastSession` (same user). An `account_mismatch` or a different `/me` id clears it.

### JS: lists cached first

- **Detail open:** call `cache_get(key)`. On a hit, render rows at once and mark them stale (`aria-busy`). Then call the fetch command, replace the rows if the uris or order differ, and clear the stale mark.
- **Playlists:** pass the `snapshotId` from the playlists list (`p.snapshot_id`). When the backend returns from cache with no request, there's nothing to do.
- **Library groups** use the same pattern with the `liked`, `albums`, `following`, `top:…` and `playlists` keys.

### JS: loaders

- **Skeleton:** `skeletonRows(n)` makes markup of `.row.is-skeleton`, with grey blocks for art, title and sub, and a shimmer animation. Under `prefers-reduced-motion` there's no shimmer.
- **Where skeletons are used:**
  - every list area that is waiting with no cached rows: library groups, detail rows, search results, artist page
  - 8 rows for track lists, 4 tiles for shelves
- **Pending play** (`src/lib/pending.js`, pure state machine):
  - `start(kind, {trackUri?})` returns a token.
  - `onPoll({isPlaying, trackUri})` resolves the pending play when `isPlaying` is true and either `trackUri` matches or none was expected.
  - `timeout` fires at 8 s.
- **While a play is pending:**
  - `#playBtn` gets `.is-pending` (spinner ring over the icon, `aria-busy="true"`)
  - `#nowArtist` shows "Starting…"
  - the clicked row gets `.is-pending` (dimmed, with a small spinner)
  - on timeout: toast "Spotify is slow to respond" and clear the pending state
- **Device switch:** the chip gets `.is-pending` and the label "Moving to <name>…" until the transfer intent settles or fails.

### JS: play a nearby cover (spec #5)

- **Hover:** each visible `.cover[data-role="past"|"next"]` in `#run` gets a round `.cover-play` button, shown on hover/focus-within next to the existing caption (title + artist). The `now` cover gets none. There's no change to how many covers show (`maxPast` / `maxNext` stay) and no scrolling.
- **Keyboard:** covers become focusable through the button (`aria-label="Play <title>"`). Enter/Space press it.
- **Target** (pure `coverTarget(item, ctx)` in `src/lib/timeline.js`, tested):
  - **next** cover (Astra #3): **use a context only when membership is known.** The queue can hold tracks the user added from elsewhere ("+").
  - **In the current context:** if the track is in the loaded context's track list (`origin`'s cached rows, or `lastSession.uris`), use `{contextUri | uris, trackUri}`.
  - **Otherwise:** `{uris: [trackUri, ...the visible queue uris after it]}`.
  - **Tests:** a queued track from another album during playlist playback.
  - **past** cover: the history row's own `context_uri` (recently-played rows carry it) gives `{contextUri: row.context_uri, trackUri}`. Else, if the track is in the current `lastSession.uris`, use that list. Else use `{uris: [trackUri]}`.
  - Never send a `trackUri` with a context the track isn't known to be in: Spotify would start that context from the top.
- **Play:**
  - With `isLocal()`, `local_load({...target, positionMs: 0, play: true})`.
  - Otherwise the Web API: `play_context` gains an optional `trackUri` offset (`PUT /me/player/play {context_uri, offset:{uri}}`), or `play_on_device` with `uris` + `offset` handled by ordering (the target track first).
  - It goes through `changeTrack`, so seek guards apply. Pending-play loaders apply too.
  - The FLIP run animation already moves the covers when the poll sees the new track.
- **Backend:** `play_context` (Rust) gains an optional `track_uri`, sent as `offset: {uri}`. The mock honours it.

## Tasks

### T0: Baseline (orchestrator)
- [ ] On `feat/snappy`: `cargo test`, `cargo build` with no warnings, `npx vitest run`, all green.

### T1: Engine local commands + device id (heavy, Opus)
**Files:** `src-tauri/src/player.rs`. It does **not** touch `lib.rs`; the orchestrator registers the commands.
- [ ] Failing unit tests first:
  - device-id load/create round trip, in a temp dir via an injectable path
  - percent → u16 volume mapping (0 → 0, 100 → 65535, 50 → 32768)
  - `local_load` args validation: exactly one of `contextUri` / `uris`, `uris` ≤ 200
  - status serialization with `device_id`
- [ ] Implement the persisted device id, `device_id` in status, and the 7 `local_*` commands per the locked table. They read the current Spirc from the engine state and return `ENGINE_NOT_READY` when there's none.
- [ ] Verify: `cargo test` passes and `cargo build` has 0 warnings in our crate.

### T2: Parallel paging + disk cache (heavy, Opus), parallel with T1
**Files:** `src-tauri/src/spotify.rs`, `src-tauri/src/cache.rs` (new), `src-tauri/Cargo.toml` (`futures = "0.3"`, `sha1_smol` or similar for keys). It does **not** touch `lib.rs` beyond `mod cache;`, which the orchestrator adds.
- [ ] Failing tests first:
  - `pages_parallel` keeps offset order when pages complete out of order (inject a fake fetcher via a closure/trait)
  - offsets computed from `total`
  - cache get/put round trip
  - key mismatch returns None
  - corrupt file returns None
  - eviction to ≤ 40 MB oldest first, in a temp dir
- [ ] Implement per the locked interfaces, including the `snapshotId` arg on `get_playlist_tracks` and the `cache_get` command.
- [ ] Verify `cargo test` and `cargo build`.

### T3: JS routing, resume, cached-first lists, loaders (heavy, Opus), parallel with T1/T2
**Files:** `src/app.js`, `src/lib/*` (+ tests), `src/styles.css`, `src/index.html` = `dev/index.html` body, `dev/mock.js`.
- [ ] Pure helpers TDD:
  - routing choice `isLocal(engine, device)`
  - `sessionToSave(prev, poll, nowMs)` with a 10 s throttle
  - `resumeSource(playbackState, lastSession)` → src or null
  - pending-play state machine
  - `skeletonRows(n)`
- [ ] Wire everything per the locked JS sections.
- [ ] Mock:
  - the `local_*` commands act on the mock player instantly
  - `engine_status.device_id`
  - `cache_get` with an in-memory cache
  - `get_playlist_tracks` honours `snapshotId`
  - a `slow` scenario (all list commands +2 s, play +2 s) to see the loaders
  - a `resume` scenario (the engine is ready, nothing plays, and lastSession is set)
- [ ] Verify `npx vitest run` and `node --check`.

### T3b: Cover play button (heavy, Opus), after T3 (same files)
- [ ] `coverTarget` TDD covering every branch above. Add the `.cover-play` button markup and CSS (round, 44px, centered on the art, fade in on hover/focus-within; respects reduced motion).
- [ ] Click → target → local or remote play per above. Rust `play_context` gets the optional `track_uri` (orchestrator applies this small Rust change when registering in T4).
- [ ] Mock: `play_context` honours `trackUri`. A harness check: hover a next cover, click play, and that track becomes now.
- [ ] Verify vitest and `node --check`.

### T4: Register + gate (orchestrator)
- [ ] Add the new commands to `lib.rs` and `mod cache;`. Build, run all tests, and the v1/v2/standalone harness regression scripts.
- [ ] Sonnet QA (read-only, `/playwright-cli`): cover play buttons (hover shows them, click jumps, past and next, the run animates), `slow` (skeletons, play spinner, "Starting…", row pending, timeout toast), `resume`, cached reopen, local routing (`__mock` call log shows `local_*` when The Run is selected and Web API calls for the Marantz), no console errors, both viewports.
- [ ] Live, on the real release build, with the Spotify app quit:
  1. Play a playlist on The Run, pause mid-song, quit the app, relaunch. It shows the same song at the same second, paused, and the next track comes from the same playlist.
  2. Time play/pause and volume on The Run, from click to audible change (player event timestamps in the log). Target under 300 ms.
  3. Open the 759-song playlist cold, then again. Record both times.
  4. Play a new playlist on The Run (local load). Record click → sound.
- [ ] Fix loop.

### T5: Exit gate
- [ ] Astra until 0 crit/high, `/simplify`, then a final Astra.
- [ ] As-built notes, plan done, move to `plans/done/`, merge, release build (`opt-level = 3`), relaunch, notify, stop the watchdog.

## Type-consistency check
- Command names are the same in the locked table, T1/T2 (Rust), T3 (JS + mock) and T4 (lib.rs registration).
- Arg names are `positionMs`, `percent`, `contextUri`, `uris`, `trackUri`, `play`, `key`, `playlistId`, `snapshotId`, `albumId`. Rust uses snake_case: `position_ms`, `context_uri`, `track_uri`, `snapshot_id`.
- The `therun.lastSession` shape is the same in the writer, `resumeSource` and the mock scenario.

## Astra plan review (v1.0 → v1.1)

Rating 5/10.

**Applied:**
- 4 HIGH:
  - #1 Spirc ignores Load while inactive → activate first + JS verification
  - #2 contextless resume → normalized source
  - #3 queued covers outside the context → membership rule
  - #4 cache across accounts → account-scoped keys
- med #5 (`total` missing from the fields projection: parallel paging can't work without it)
- med #10 (restore the origin playlist/album: the user's explicit "same playlist")
- med #6 and #7 came for free with the #1/#2 changes

**Skipped (med, crit/high rule):**
- #8 id-based comparison for album/artist group refresh
- #9 stale lookup when a playlist's snapshot changes: the user sees skeletons on the first open after a change
