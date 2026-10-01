# The Run — Redesign Implementation Plan v1.0

**Date:** 2026-10-01
**Status:** in-progress, paused 2026-10-02 after T6 (user: "dont run anything"). Next: T7.

## Progress (as of 2026-10-02)

| task | state | evidence |
|---|---|---|
| T0 baseline | ✓ done | `cargo build` green, `node --check` clean |
| T1+T2 Rust backend | ✓ done | 16/16 `cargo test`, 0 build warnings. Search `limit=10`, album paging, `auth_status`, `get_queue`, `get_recently_played`, `AUTH_EXPIRED` / `NO_ACTIVE_DEVICE` codes |
| T3+T4 harness + lib | ✓ done | 21/21 vitest. `dev/fixture.json` holds real data: 5 playlists, 20 queue, 30 real recent plays. Font self-hosted (WOFF2) |
| T5 stage UI | ✓ done | commit `151c7d1`. History left, up next right, FLIP on track change verified, colour from cover |
| T6 library + search | ✓ done | commit `c458cbe`. Library sheet, search overlay, Esc, scrub aligned to cover, left-edge sliver removed |
| T7 Sonnet QA | ✗ not done | the agent was dispatched but stopped when the session exited, so there are no results. Re-dispatch from scratch |
| T8 fix loop | pending | known bug (orchestrator saw it): search album captions are clipped by the panel bottom (`p2/search-1440.png`) |
| T9 exit gate | pending | Astra loop until 0 crit/high, `/simplify`, reconcile spec, merge to `main`, notify |

**Repo:** branch `feat/the-run-redesign` is pushed to `github.com/1905/rust-spotify` (private). Last commit is `c458cbe`, and the tree was clean after it. `main` still holds only the bootstrap commit.

**To resume:**
1. Serve the repo root: `cd ~/dev/rust-spotify && python3 -m http.server 8799 --bind 127.0.0.1`.
2. Re-dispatch T7: Sonnet + `/playwright-cli`, read-only, all 12 scenarios at 1440×900 and 800×600.
3. Run T8 with the QA findings plus the album-caption bug.
4. Run T9.

**Side effects to know about:**
- The user's token was refreshed by a one-off reconnect script, so it now has the `user-read-recently-played` scope. The old token is backed up at `scratchpad/tokens.backup.json`.
- The Tauri dev watcher is stopped.
- Nothing has been checked in the real Tauri (WKWebView) window yet. Web only, per user.
**Spec:** ./spec.md
**Goal:** Replace the generic three-column UI with a minimal one-screen player built around a played → now → next run of covers, backed by the real Spotify queue and history, and fix the 7 review bugs.
**Architecture:** The Rust Tauri backend stays a thin Web API proxy, with new `auth_status`, `get_queue` and `get_recently_played` commands plus fixes. The frontend is plain ES modules: `app.js` (state, poll loop, DOM) and pure `lib/*.js` tested with vitest. A browser-only `dev/` harness stubs `window.__TAURI__` with real-art fixtures for Playwright QA.
**Tech Stack:** Rust (tauri 2, reqwest, serde_json), vanilla JS ES modules, vitest, Bricolage Grotesque (self-hosted), playwright-cli (QA).

> For agentic workers: use superpowers:subagent-driven-development to implement task-by-task. Checkbox syntax for tracking.

**Repo note (updated 2026-10-01, user: "commit and push to private gh. commit often"):** private repo `1905/rust-spotify`. Branch `feat/the-run-redesign` off `main`. The orchestrator commits after each verified task and pushes; implementers do no git. Merge to `main` when the exit gate is green. No CI workflows yet.

**Hard requirement (user, mid-plan):** "history and up next should work too". Both must show real data: up next = `/me/player/queue`, history = `/me/player/recently-played` + session-observed tracks. QA checks both.

## File map

**Modify:**
- `src-tauri/src/auth.rs`
- `src-tauri/src/spotify.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/tauri.conf.json`
- `src/index.html`
- `src/styles.css`
- `package.json`

**Create:**
- `src/app.js`
- `src/lib/format.js`, `src/lib/color.js`, `src/lib/timeline.js`
- `src/lib/format.test.js`, `src/lib/color.test.js`, `src/lib/timeline.test.js`
- `src/fonts/bricolage.woff2`
- `dev/index.html`, `dev/mock.js`, `dev/fixture.json`, `dev/capture.py`

**Move to /tmp/trash:**
- `src/main.js`, `src/mock.js`, `src/dev.html`

**Out of scope:** Tauri WKWebView check, git, in-app audio, volume, extra shortcuts.

## Locked interfaces

**Tauri commands** (camelCase args from JS):

| command | args | returns | error strings |
|---|---|---|---|
| `auth_status` | — | `"ok" \| "login" \| "reconnect"` | — |
| `login` | — | `null` | free text |
| `get_playlists` | — | `[{id, name, images, tracks:{total}}]` | `AUTH_EXPIRED`, free text |
| `get_playlist_tracks` | `{playlistId}` | `[Track]` | same |
| `get_album_tracks` | `{albumId}` | `[Track]` (all pages) | same |
| `search` | `{query}` | `{tracks:[Track]≤10, albums:[{id,uri,name,artists,cover,year,total_tracks}]≤10}` | same |
| `get_queue` | — | `[Track]` | same |
| `get_recently_played` | — | `[{track: Track, played_at}]` (≤30, newest first) | same |
| `playback_state` | — | `{active:false}` or `{active:true, is_playing, progress_ms, device_id, device_name, track: Track}` | same |
| `list_devices` | — | `[{id,name,type,is_active}]` | same |
| `play_on_device` | `{deviceId, uris}` | `null` | `NO_ACTIVE_DEVICE`, `AUTH_EXPIRED`, free text |
| `resume` | `{deviceId}` | `null` | same |
| `pause`, `next_track`, `previous_track` | — | `null` | same |
| `seek` | `{positionMs}` | `null` | same |

`Track` = `{id, uri, name, artists (string), album (string), cover (url|null), duration_ms}`.

Removed: `is_authenticated`, `get_profile`, `get_access_token`, `set_volume`.

**Error strings.** A Rust `Err(String)` that **starts with** `AUTH_EXPIRED` or `NO_ACTIVE_DEVICE` is a code. The frontend checks `String(e).startsWith(code)`.

**JS lib signatures:**
- `format.js`:
  - `fmtTime(ms) → "m:ss"`, or `"h:mm:ss"` when ≥ 1h; null → `"0:00"`
  - `esc(s) → string`
- `color.js`:
  - `FALLBACK = {vivid:[139,124,240], ink:[21,19,27]}`
  - `pickColors(rgba: ArrayLike<number>) → {vivid:[r,g,b], ink:[r,g,b]}`: vivid = most saturated mid-luminance pixel, or the fallback when max saturation < 0.15. Ink = vivid's hue at HSL lightness 8%, saturation min(s, 40%).
  - `extractColors(url) → Promise<{vivid,ink}|null>` (DOM canvas, 16×16, `crossOrigin='anonymous'`)
- `timeline.js`:
  - `buildRun({history, now, queue}, {maxPast=4, maxNext=8}) → [{key, role:'past'|'now'|'next', offset:int, track}]`, in display order left→right.
  - Past = history newest-first, then:
    1. drop rows whose track uri equals `now.uri` while they're at the head
    2. collapse consecutive duplicate uris
    3. take `maxPast`, reverse so the oldest is leftmost
  - Offsets: past −n…−1, now 0, next 1…n.
  - `key` = `uri + "~" + k`, where k = count of the same uri earlier in the display list. That keeps the key stable for a track moving next→now→past.
  - `now` null → no `now` item, and past/next are still returned.
  - `flip(container, prevRects: Map<key,DOMRect>)`: animates children by `data-key` (FLIP, 600ms). No-op under reduced motion.
  - `measure(container) → Map<key,DOMRect>`.

**DOM ids** (QA and tests rely on them):
- Screens: `#login`, `#loginTitle`, `#loginSub`, `#loginBtn`, `#stage`
- Top bar: `#libraryBtn`, `#searchBtn`, `#device` (dot + name)
- Stage content: `#run`, `#nowTitle`, `#nowArtist`, `#nowAlbum`, `#emptyState`
- Transport: `#curTime`, `#durTime`, `#scrub`, `#scrubFill`, `#prevBtn`, `#playBtn`, `#nextBtn`
- Library: `#library`, `#libList`, `#libDetail`
- Search: `#search`, `#searchInput`, `#searchResults`
- Feedback and background: `#toast`, `#bgA`, `#bgB`

**Dev scenarios** (`dev/index.html?s=`):
- `playing` (default), `paused`, `nothing`, `nodevice`, `login`, `reconnect`, `error`
- `library`, `library-detail`, `search`, `search-empty`, `ad` (active device, no track)
- `long-titles`

Viewport is set by QA; the page has no viewport param.

## Tasks

### T0 — Baseline sanity (gate)
- [ ] `cd ~/dev/rust-spotify/src-tauri && cargo build` → `Finished`
- [ ] `node --check ~/dev/rust-spotify/src/main.js` → no output

### T1 — Auth: scopes, status, invalidation (heavy)
**Files:** Modify `src-tauri/src/auth.rs`.
- [ ] Write failing unit tests in `#[cfg(test)] mod tests`:
  - `has_required_scopes("user-read-private … user-read-recently-played …")` → true
  - the same string minus `user-read-recently-played` → false
  - empty → false
  - extra unknown scope → true
- [ ] `cargo test auth` → fails (fn missing).
- [ ] Implement:
  - `const REQUIRED_SCOPES: &[&str]` (the 7 current scopes minus `streaming`, plus `user-read-recently-played`). Set `SCOPES` to the same list.
  - `Tokens.scope: String` with `#[serde(default)]`. Store `scope` from the token response.
  - `pub fn has_required_scopes(granted: &str) -> bool`
  - `pub fn auth_status() -> &'static str`: no file or empty refresh → `"login"`; scopes missing → `"reconnect"`; else `"ok"`.
  - In `valid_access_token` refresh: response 400 with `invalid_grant`, or 401 → rename `tokens.json` → `tokens.json.invalid` (overwrite allowed) and return `Err("AUTH_EXPIRED: …")`. Keep the stored `scope` when the refresh response omits it.
- [ ] `cargo test auth` → pass. `cargo build` → no warnings from auth.rs.

### T2 — Spotify API: fixes + queue + history (heavy)
**Files:** Modify `src-tauri/src/spotify.rs`, `src-tauri/src/lib.rs`.
- [ ] Failing tests first (pure fns on `serde_json::Value`):
  - `simplify_track(&row_or_track)`: plain track object → Track; must read `album.images[0].url` and join artists with ", ".
  - `track_of_row(&row)`: returns `row["item"]` if non-null else `row["track"]`.
  - `parse_queue(&v)`: `{currently_playing, queue:[…]}` → Vec<Track> from `queue`, skipping nulls.
  - `parse_recent(&v)`: `{items:[{track|item, played_at}]}` → `[{track, played_at}]`.
  - `urlencode("a b&c")` → `"a%20b%26c"`.
- [ ] Implement:
  - search `limit=10`
  - `get_album_tracks` follows `tracks.next` (absolute URL → strip API prefix)
  - `get_queue` → GET `/me/player/queue`, then `parse_queue`
  - `get_recently_played` → GET `/me/player/recently-played?limit=30`, then `parse_recent`
  - `get_playlist_tracks` rows go through `track_of_row` + `simplify_track` (remove the inline duplicate)
- [ ] Error mapping in `get`/`put`/`send_empty`:
  - HTTP 401 → `Err("AUTH_EXPIRED: …")`
  - HTTP 404 on `/me/player*` paths → `Err("NO_ACTIVE_DEVICE: …")`
  - `AUTH_EXPIRED` from `valid_access_token` passes through unchanged
- [ ] `lib.rs`: register `auth_status`, `get_queue`, `get_recently_played`. Remove `is_authenticated`, `get_profile`, `get_access_token`, `set_volume` and their spotify.rs fns.
- [ ] `cargo test` → all pass. `cargo build` → zero warnings.
- [ ] No live API tests in Rust. The orchestrator already verified `/me/player/queue` → 200 with curl.

### T3 — Dev harness, fixture, fonts, tooling (heavy)
**Files:**
- Create `dev/capture.py`, `dev/fixture.json`, `dev/mock.js`, `dev/index.html`, `src/fonts/bricolage.woff2`
- Modify `package.json`
- [ ] `dev/capture.py`: stdlib only (urllib). Reads the access token from `~/Library/Application Support/rust-spotify/tokens.json`. Writes `dev/fixture.json`:
  - `playlists`, with `items.total` normalised to `tracks.total`
  - `playlistTracks` for the first 2 playlists (rows via `item` or `track`)
  - `queue` (from `/me/player/queue`)
  - `now` (`currently_playing`)
  - `recent`: `/me/player/recently-played` if it returns 200; on 403, build `recent` from the next 8 queue tracks with fake `played_at`, and print `recent: synthesized (scope missing)`
  - `search` for "the xx", limit=10 tracks + albums
  - `albumTracks` for the first album result

  All in the Track shape. Search limit = 10.
- [ ] Run `python3 dev/capture.py` → prints counts. Check that `fixture.json` has ≥1 playlist, ≥1 queue item, and cover URLs on `i.scdn.co`.
- [ ] `dev/mock.js`:
  - defines `window.__TAURI__.core.invoke` from `fixture.json`, loaded with a synchronous XHR to `fixture.json` (relative to `dev/index.html`) at the top of `mock.js`
  - scenario from `?s=`, matching the Dev scenarios list
  - `next_track` advances a simulated now/queue/history, so the FLIP is testable
  - `error`: every data call rejects with `"network down"`
  - `long-titles`: overrides the now title with 120 chars
- [ ] `dev/index.html`: same markup as `src/index.html`, with paths `../src/styles.css` and `../src/app.js`. Loads `mock.js` before `app.js`. Header comment: "keep markup in sync with src/index.html".
- [ ] Fonts: fetch `https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wdth,wght@12..96,75..100,200..800&display=swap` with a Chrome UA. Download the latin woff2 to `src/fonts/bricolage.woff2`, then verify it with `file` → "Web Open Font Format (Version 2)".
- [ ] `package.json`:
  - add devDependency `vitest@^3`
  - scripts `"test": "vitest run"`, `"dev:web": "python3 -m http.server 8799"` (run from the project root)
  - `npm install`

### T4 — Pure lib modules (heavy)
**Files:** Create `src/lib/{format,color,timeline}.js` and their `.test.js`.
- [ ] Tests first:
  - `fmtTime`: `0→"0:00"`, `59000→"0:59"`, `61000→"1:01"`, `3600000→"1:00:00"`, `null→"0:00"`
  - `esc` escapes `<>&"'`
  - `pickColors`: an all-grey array → `FALLBACK.vivid`; one saturated red pixel among greys → vivid ≈ red; the ink lightness ≤ 10%
  - `buildRun`:
    - empty input → `[]`
    - only now → 1 item, offset 0
    - history of 6 → 4 past, offsets −4…−1, oldest leftmost
    - history head == now → dropped
    - consecutive dupes collapsed
    - queue of 12 → 8 next
    - keys: build A, then advance (now←queue[0], history←old now), and every shared track keeps its key
- [ ] `npx vitest run` → fails, then implement, then passes.

### T5 — Stage UI (heavy)
**Files:**
- Modify `src/index.html`, `src/styles.css`
- Create `src/app.js`
- Move `src/main.js`, `src/mock.js`, `src/dev.html` to `/tmp/trash/`
- [ ] `index.html` markup per the DOM ids:
  - login section, then stage (top bar / run / now block / transport), library aside, search dialog, toast
  - `#bgA`/`#bgB` layers plus a grain overlay
  - `<script type="module" src="app.js">`
- [ ] `styles.css`: the spec design system exactly — tokens, type table, run sizes/opacities, 900px breakpoint, focus ring, reduced motion, `@font-face` from `fonts/bricolage.woff2`. No ALL CAPS, no middle dots, no `→`.
- [ ] `app.js`:
  - boot → `auth_status` → login copy per state
  - sequential poll loop (`setTimeout` after await)
  - on track change: `get_queue` + `get_recently_played`, merge session history, `measure` → render run → `flip`, `extractColors` → crossfade bg layers via CSS vars on the inactive layer then opacity swap
  - every 10th poll: `get_queue`
  - transport: play/pause / prev / next / scrub-click seek
  - `NO_ACTIVE_DEVICE` → drop the device, call `list_devices` once, retry once, else toast
  - `AUTH_EXPIRED` → stop polling, login screen with "Your Spotify session ended"
  - nothing-playing and no-device empty states; cover image fallback tile (first letter)
- [ ] Verify:
  - `node --check src/app.js`
  - `npm test` green
  - `cargo build` green
  - the orchestrator screenshots `?s=playing` and `?s=nothing`

### T6 — Library + Search overlays (heavy)
**Files:** Modify `src/app.js`, `src/styles.css`, `src/index.html`, `dev/index.html`.
- [ ] Library sheet:
  - level 1 playlists; level 2 tracks (back, cover, name, Play)
  - row click → `play_on_device(rows from i)`
  - album detail reuses level 2
  - generation token `gen.detail`; Play disabled until the current gen's tracks arrive and are non-empty
  - empty copy "This playlist is empty."
- [ ] Search overlay:
  - input; debounce 250ms; `gen.search`
  - Songs (10 rows, click → play that uri), Albums (row of covers, click → library level 2)
  - empty "No songs or albums for "q"."
  - error "Search failed — <reason>"
- [ ] Keys: Space = play/pause unless focus is in an input; Esc closes the top overlay.
- [ ] Verify: `node --check`, `npm test`, screenshots of `?s=library-detail` and `?s=search`.

### T7 — QA sweep (light, Sonnet, read-only)
- [ ] Serve the project root: `python3 -m http.server 8799` (background).
- [ ] Sonnet agent, **/playwright-cli skill only**. Scope: "read-only QA, never edit files, never run app code changes, never call Spotify".
  - Load every scenario at 1440×900 and 800×600.
  - Screenshot each one and check the assertions below.
  - Report each failure with screenshot path + selector + what's wrong.
- [ ] Assertions:
  - `#run` shows ≥1 past and ≥1 next cover in `playing` (**history and up next visible**)
  - `next_track` click moves the old now cover into the past slot
  - `#nowTitle` ≤ 2 lines in `long-titles`
  - no horizontal page scroll
  - empty/error/login copy exact per spec
  - overlays close on Esc
  - no console errors except the expected `error` scenario rejections

### T8 — Fix loop (heavy)
- [ ] Orchestrator fixes each confirmed QA failure (or dispatches Opus), re-shoots, and repeats T7 on the failed scenarios until zero failures. Max 3 loops, then reassess the spec.

### T9 — Exit gate
- [ ] Astra loop (user, 2026-10-01: "review with astra until no crit or high. verify and fix all"):
  1. `/rival-codex review ~/dev/rust-spotify`
  2. verify every finding against the code
  3. fix ALL confirmed findings (any severity)
  4. re-run `cargo test` + `npm test` + `cargo build`
  5. repeat until a round reports 0 crit and 0 high (confirmed)
- [ ] `/simplify`.
- [ ] Reconcile `spec.md` (`## As-built notes`), plan Status → done, move the dir to `plans/done/`.
- [ ] `/notify`: ask the user to reconnect Spotify once and look at the app (path, open locally).

## Type-consistency check
- Command names are identical in the T2 table, the T5/T6 usage and the T3 mock: `auth_status`, `get_queue`, `get_recently_played`, `play_on_device`, `next_track`, `previous_track`, `seek`, `resume`, `pause`, `search`, `get_album_tracks`, `get_playlist_tracks`, `get_playlists`, `list_devices`, `playback_state`, `login`.
- JS arg names: `playlistId`, `albumId`, `deviceId`, `uris`, `positionMs`, `query`. Tauri maps them to snake_case Rust params `playlist_id`, `album_id`, `device_id`, `uris`, `position_ms`, `query`.
- Track shape is identical in Rust `simplify_track`, `fixture.json` and `buildRun` input.
