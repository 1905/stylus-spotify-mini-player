# rust-spotify — "The Run" redesign

**Date:** 2026-10-01
**Scope:** ~/dev/rust-spotify
**Status:** shipped 2026-10-02 (as-built notes at the end)

## TL;DR

**P1 — Backend fixes and real data.**
What: fix search (Spotify now rejects `limit=20` with 400), paginate albums, add real queue and real listening history, detect missing scopes and dead sessions.
Why: search is broken in the live app today, and history/queue are guesses.
You must: click **Reconnect Spotify** once (new scope `user-read-recently-played`).
Does NOT: change any UI.

**P2 — Dev harness and pure logic.**
What: a browser-only harness at `dev/` with real-album-art fixtures and named scenarios, plus tested pure modules (timeline, color, format).
Why: we design and QA in a normal browser with Playwright, as you asked, without your login.
You must: nothing.
Does NOT: ship anything to the app bundle (`dev/` sits outside `src/`).

**P3 — The stage.**
What: one screen. A horizontal run of covers — played (left, small, faded) → now (center, large) → next (right). Huge track title in Bricolage Grotesque. Background colour comes from the current cover. One animation: the run slides one step when the track changes.
Why: the old three-column Spotify clone was generic. History before/after becomes the interface itself, not a side panel.
You must: nothing.
Does NOT: play audio in-app (still Spotify Connect to your Mac / Marantz).

**P4 — Library and Search overlays.**
What: Library slides in from the left (playlists → tracks). Search is a simple overlay (songs + albums), and you click a result to play it. Only two keys: Space = play/pause, Esc = close.
Why: no permanent sidebars. The stage stays clean, and the tools appear only when called. User: "minimum features, keep it simple".
You must: nothing.
Does NOT: add likes, shuffle, repeat, lyrics, volume, playlist editing, extra shortcuts, or click-to-jump on the run.

**P5 — States, polish, QA.**
What: every empty, error and loading state designed. A Sonnet QA agent sweeps all scenarios with Playwright. I fix and re-shoot until it's clean.
Why: "don't stop until it's great".
You must: nothing until the final notify.
Does NOT: verify inside the Tauri WebKit window. You said web only for now. Noted as follow-up.

## Problem(s)

1. **The UI is generic and was called ugly twice.** The current layout is a fixed three-column grid — rail, main, side panel (`src/styles.css:90`) — with an accent tint. It reads as a Spotify clone. History and queue sit in a side list (`src/main.js:232`), even though "before/after" was a core ask.
2. **Search is broken in production.** `src-tauri/src/spotify.rs:155` sends `limit=20`; Spotify answers `400 Invalid limit` (verified live 2026-10-01; `limit=10` → 200).
3. **History and queue are guesses.** Queue = our own context slice, and history = tracks we observed while the app was open (`src/main.js:232`). Spotify exposes the real queue (`/me/player/queue`, verified 200, 20 items) and real history (`/me/player/recently-played`, currently 403 `Insufficient client scope`).
4. **No way back to login.** `has_tokens` (`src-tauri/src/auth.rs:60`) treats any stored refresh token as valid. A revoked token or missing scope traps the user in a broken app.
5. **Races and stale state** (Codex review, all confirmed by reading):
   - The detail view applies late responses from a previous playlist (`src/main.js:169`).
   - The Play button keeps the old playlist's handler (`src/main.js:208`).
   - The device ID is cached forever (`src/main.js:343`).
   - Polls overlap every second (`src/main.js:404`).
6. **Albums truncate.** `src-tauri/src/spotify.rs:195` reads only the first page of `tracks`.

## Goals

1. One-screen stage with a played → now → next run of covers (fixes problems 1, 3).
2. Search works and returns 10 songs + 10 albums (fixes 2).
3. Real queue and real history, with session-observed history as fallback (fixes 3).
4. Missing scope or dead session → login screen with the right copy (fixes 4).
5. No stale-response, stale-device, or overlapping-poll bugs (fixes 5).
6. Full album track lists (fixes 6).
7. A distinctive, minimal look with every state designed, verified in a browser at 1440×900 and 800×600 (fixes 1).

## Non-goals

- In-app audio (Web Playback SDK fails in WKWebView; settled).
- Shuffle, repeat, likes, lyrics, volume, playlist editing, devices menu beyond showing the active one.
- Keyboard shortcuts beyond Space and Esc; click-to-jump on the run.
- Profile info of any kind; playlist owner names.
- Tauri WebKit layout verification in this pass.
- A light theme.

## Design system

**Concept.** The run *is* the history. Size and opacity encode distance in time from now. Nothing else on screen competes with it.

**Colour.** No fixed accent. Everything derives from the current cover.

| token | value | role |
|---|---|---|
| `--ink` | art colour at L≈8%, S≤40% (fallback `#15131b`) | page background |
| `--wash` | art vivid colour at 22% alpha | radial glow behind the current cover |
| `--fg` | `#f6f4f8` | primary text, controls |
| `--fg-2` | `rgba(246,244,248,.62)` | secondary text |
| `--fg-3` | `rgba(246,244,248,.38)` | times, hints |
| `--hair` | `rgba(246,244,248,.12)` | track lines, dividers |
| `--live` | `#1ed760` | only the "device connected" dot |

Background = two stacked layers (`.bg-a`, `.bg-b`), each `radial-gradient(--wash) over --ink`, crossfaded with opacity (gradients can't transition). A 3% SVG-noise grain sits on top to stop gradient banding.

**Type.** One family: **Bricolage Grotesque** (variable `opsz 12–96`, `wdth 75–100`, `wght 200–800`), self-hosted woff2 in `src/fonts/` (no network at runtime). Fallback `system-ui`.

| role | size / weight / width | notes |
|---|---|---|
| now title | `clamp(40px, 6vw, 84px)` / 700 / wdth 80 | tracking −0.035em, max 2 lines, ellipsis |
| now artist | 20px / 500 | `--fg` |
| now album | 15px / 400 | `--fg-2`, own line (no middle-dot joins) |
| overlay heading | 28px / 700 / wdth 85 | |
| body | 14px / 450 | |
| small | 12.5px / 450 | tabular figures for times |

No ALL CAPS, no eyebrow labels, no `→` in buttons, no monospace.

**Layout (1440×900).**

```
┌──────────────────────────────────────────────────────────────────────┐
│ Library                      Search  ⌘K                  ● MacBook Pro│ 64px top bar, transparent
│                                                                      │
│        ▫   ▫   ▫      ┌──────────────┐     ▢    ▢    ▢    ▢    ▢     │
│      (played, faded)  │              │   (next, 0.85 → fade at edge) │
│                       │   NOW 380px  │                               │
│                       └──────────────┘                               │
│                                                                      │
│                       Together                                       │ title left-aligned to the cover
│                       The xx                                         │
│                       Coexist                                        │
│                                                                      │
│      0:44 ━━━━━━━━━━━━━━━━●──────────────────────────────── 3:12     │
│                       ⏮     ⏯     ⏭                                │
└──────────────────────────────────────────────────────────────────────┘
```

- The current cover is centred horizontally, and its centre sits at 40% of the viewport height.
- Played covers sit left of now: up to 4, sizes 150/124/104/88px, opacity .62/.46/.34/.24, saturation 60%. Nearest first.
- Next covers sit right: up to 8, 170px, opacity .9. A mask fades the right edge.
- Gap 28px. Vertical alignment: bottom edges aligned to the current cover's bottom.
- Under 900px width: now = 260px, 2 played, 4 next, title clamps to 40px.
- Every cover shows its caption (title / artist, 12.5px) on hover, below the cover. The run is display-only: no click actions (user: minimum features).

**Motion.** One moment only. On track change, a FLIP animation moves every cover from its old slot to its new slot (600ms, `cubic-bezier(.2,.7,.2,1)`), and the background layers crossfade (900ms). Hover: no scale, no lift — the caption only. `prefers-reduced-motion` → instant.

**Overlays.**
- **Library sheet:** slides from the left, 440px wide (full width under 700px), backdrop `rgba(0,0,0,.45)`, blur 8px.
  - Level 1: playlists list (56px cover, name, "N tracks").
  - Level 2: tracks list (back control, cover, name, Play button, rows: #, cover, title / artist, duration).
  - Clicking a row plays the playlist from that row.
- **Search overlay:** centred, 640px, top at 14vh.
  - Input on top; results grouped "Songs" (10 rows) then "Albums" (row of 10 covers, horizontal scroll).
  - Clicking a song plays it.
  - Clicking an album opens Library level 2 for that album.
  - Debounce 250ms, and the last request wins (generation token).

**Keyboard.** Two keys only:
- Space = play/pause (not while typing).
- Esc = close the open overlay.

Tab order works through the visible controls with a 2px `--fg` focus ring.

## Data flow

```
boot ─▶ auth_status ─┬─ "login"     → login screen ("Connect Spotify")
                     ├─ "reconnect" → login screen ("Reconnect Spotify to load your listening history")
                     └─ "ok"        → stage
stage ─▶ poll loop (sequential, setTimeout 1000ms after each await)
           playback_state ─▶ now, progress, is_playing, device
           on track change ─▶ get_queue, get_recently_played (+ FLIP)
           every 10th tick  ─▶ get_queue (queue changes behind our back)
command errors: "NO_ACTIVE_DEVICE" → drop cached device, rediscover once, retry once
                "AUTH_EXPIRED"     → stop polling, show login
```

`state` shape (frontend):

```json
{
  "auth": "ok",
  "device": { "id": "60f97…", "name": "MacBook Pro" },
  "now": { "uri": "spotify:track:…", "name": "Together", "artists": "The xx", "album": "Coexist", "cover": "https://i.scdn.co/…", "duration_ms": 192000 },
  "isPlaying": true,
  "progressMs": 44000,
  "queue": [ { "uri": "…", "name": "I Dare You", "artists": "The xx", "album": "I See You", "cover": "…", "duration_ms": 0 } ],
  "history": [ { "track": { "...": "same shape" }, "played_at": "2026-10-01T15:02:11Z" } ],
  "overlay": null,
  "gen": { "detail": 3, "search": 7 }
}
```

History = `recently-played` merged with tracks observed this session, deduped by `uri+played_at`, newest first. Consecutive duplicate tracks are collapsed.

## Backend API (Tauri commands)

| command | returns | notes |
|---|---|---|
| `auth_status` | `"ok" \| "login" \| "reconnect"` | reconnect = stored `scope` lacks a required scope |
| `login` | `()` | stores `scope` from the token response |
| `get_playlists` | `[playlist]` | unchanged (`items.total` normalisation stays) |
| `get_playlist_tracks(playlistId)` | `[track]` | unchanged |
| `get_album_tracks(albumId)` | `[track]` | follows `tracks.next` |
| `search(query)` | `{tracks[10], albums[10]}` | `limit=10` |
| `get_queue` | `[track]` | `/me/player/queue` → `queue[]` simplified |
| `get_recently_played` | `[{track, played_at}]` | `limit=30`; reads `track` or `item` per row |
| `playback_state`, `list_devices`, `play_on_device`, `resume`, `pause`, `next_track`, `previous_track`, `seek` | as today | errors carry codes `NO_ACTIVE_DEVICE` (404) and `AUTH_EXPIRED`; `set_volume` removed |

Terminal refresh failure (`400 invalid_grant` or `401`) → move `tokens.json` to `tokens.json.invalid` and return `AUTH_EXPIRED`. The tokens are not silently deleted.

## File-level changes

| file | change |
|---|---|
| `src-tauri/src/auth.rs` | Add the `user-read-recently-played` scope and a `REQUIRED_SCOPES` list. Store `scope` in `Tokens` (`#[serde(default)]`). New `auth_status()`. Terminal refresh failure → invalidate the file and return `AUTH_EXPIRED`. Unit tests for the scope check. |
| `src-tauri/src/spotify.rs` | Search `limit=10`. Album pagination. `get_queue`, `get_recently_played`. Error-code mapping (`NO_ACTIVE_DEVICE`, `AUTH_EXPIRED`). Pure parse helpers (`parse_queue`, `parse_recent`, `simplify_track` reading `item`/`track`) with unit tests. |
| `src-tauri/src/lib.rs` | Register `auth_status`, `get_queue`, `get_recently_played`. Drop `is_authenticated`, `get_profile`, `get_access_token` (unused). |
| `src-tauri/tauri.conf.json` | Title "Player". CSP: drop the SDK/media entries, keep `img-src` Spotify CDNs, `font-src 'self'`. |
| `src/index.html` | New markup: login, stage (top bar, run, now block, transport), library sheet, search palette, bg layers. `<script type="module" src="app.js">`. |
| `src/styles.css` | Full rewrite to the design system above. |
| `src/app.js` | New entry (replaces `main.js`): state, poll loop, render, overlays, keyboard, generation tokens. |
| `src/lib/format.js` | `fmtTime`, `esc`. |
| `src/lib/color.js` | `pickColors(rgbaArray) → {vivid, ink}` (pure) + `extractFromImage(url)` (DOM). |
| `src/lib/timeline.js` | `buildRun({history, now, queue}, {maxPast, maxNext}) → [{key, role, offset, track}]` (pure), plus FLIP helper. |
| `src/lib/*.test.js` | vitest unit tests for format, color, timeline. |
| `src/fonts/` | Bricolage Grotesque variable woff2 (latin). |
| `src/main.js`, `src/mock.js`, `src/dev.html` | Removed (moved to `/tmp/trash`). |
| `dev/index.html` | Copy of the app shell + `mock.js` before `app.js`. Served from the project root so `../src/` resolves. |
| `dev/mock.js` | `window.__TAURI__` stub driven by `?s=<scenario>`, backed by `fixture.json`. |
| `dev/fixture.json` | Real playlists / queue / recent / search captured once from the live API (covers = real i.scdn.co art). |
| `dev/capture.py` | Script that builds `fixture.json` from the live API (reads the local token). |
| `package.json` | Add `vitest` devDependency and `test` / `dev:web` scripts. |

## Tests

- **Rust unit** (`cargo test`):
  - `urlencode`
  - `join_artists`
  - `simplify_track` for `item` rows and `track` rows
  - `parse_queue` on a sample payload
  - `parse_recent` for both row shapes
  - `has_required_scopes`: missing → false, all → true, extra → true
- **JS unit** (`npx vitest run`):
  - `fmtTime`: 0, 59s, 61s, 1h.
  - `pickColors`: grey image → fallback; saturated pixel wins.
  - `buildRun`:
    - empty input
    - only `now`
    - history longer than `maxPast`
    - `now` also in history (dedupe)
    - queue longer than `maxNext`
    - stable keys across a track change
- **Browser QA** (Sonnet + playwright-cli, `dev/index.html?s=…` at 1440×900 and 800×600). Scenarios:
  - `playing`, `paused`, `nothing`, `nodevice`, `login`, `reconnect`
  - `library`, `library-detail`, `search`, `search-empty`
  - `long-titles`, `error`
- **Manual (user):** reconnect once; play from a playlist; skip; open search.

## Failure modes & decisions

| failure | behaviour |
|---|---|
| Nothing playing (204) | Stage shows "Nothing playing" (title slot), the run shows history only, and the Library button is primary. |
| No devices at all | Title slot: "Open Spotify on a device". Line: "Your Mac, phone, or speaker — then press play." |
| Device vanished mid-command (404) | Drop the cached device, rediscover once, retry the command once. If it fails again → toast "Couldn't reach MacBook Pro". |
| Missing scope | Login screen, copy "Reconnect Spotify to load your listening history", button "Reconnect Spotify". |
| Refresh token revoked | Tokens file moved to `.invalid`, polling stops, login screen "Your Spotify session ended", button "Connect Spotify". |
| Search 0 results | "No songs or albums for "<q>"." |
| Search request error | Inline line in the palette: "Search failed — <short reason>". |
| Playlist empty | Level 2 shows "This playlist is empty." Play is disabled. |
| Cover fails to load | Neutral tile in `--hair` with the first letter of the title. No broken-image icon. |
| Late detail/search response | Discarded by generation token. |
| Poll slower than 1s | Next poll starts only after the current one ends. |
| Art colour extraction fails (CORS) | Keep the previous colours, and use the fallback ink on first load. |
| Very long title | 2 lines, then ellipsis; full title in the `title` attribute. |

## Out of scope

- Tauri/WKWebView layout verification (follow-up after the web sign-off).
- Web Playback SDK / in-app audio.
- Git setup and commits (no repo; not agreed). Checkpoints are file backups in the scratchpad.
- Multi-device picker UI.
- Light mode, theming options.

## Rollout

- **P1** — backend fixes + new commands + Rust tests; `cargo build && cargo test` green.
- **P2** — dev harness, fixture capture, pure lib modules + vitest; `npx vitest run` green.
- **P3** — stage UI (run, now block, transport, colour, FLIP); screenshot check of `playing` / `nothing`.
- **P4** — library sheet, search palette, keyboard, login/reconnect screens.
- **P5** — all failure states, Sonnet QA sweep, fix loop until clean; then Codex review → fixes → `/simplify` → notify.

## As-built notes (2026-10-02)

What shipped differs from the text above in these places:

- **Now cover under 900px** is `min(260px, 36vh)`. At 800×600 it renders 216px, so the stage fits a 600px-tall window.
- **Past covers cut by the window edge** are hidden (`is-off`), so 1440 can show 3 instead of 4.
- **History rule.** A track that played for less than 30s of real listening time (or half its length, if shorter) is not recorded as played. Seeks and pauses don't count as listening. This matches Spotify's own play count. Past never repeats the current track. Queued repeats keep their earlier plays.
- **Ads and podcasts.** `playback_state` returns `track: null` for them. The stage shows "Playing on {device}" and "An ad or a podcast is on. Songs show up here." Play/pause still works; prev, next and scrub are off.
- **Keyboard.** Space presses the focused button. With nothing focused it toggles play. The scrub bar is focusable when a song plays: arrows seek ±5s, Home/End jump to the ends.
- **Polling.** It stops while the window is hidden and restarts with a fresh poll when it is visible again. With nothing playing, every 10th tick fetches the device list only.
- **Auth.** Tokens are cached in memory under one async mutex, which also serializes refreshes. Spotify rotates refresh tokens. Every HTTP call has a 5s connect and 15s total deadline. The login callback listener gives up after 3 minutes.
- **Stale responses.** Every `invoke` belongs to a login session. A response from before a logout never settles, so it can't log out the new session. Within a session, a poll epoch ignores polls from before a stop or restart. A restart always re-renders in full.
- **History merge** (`mergeHistory` in `lib/timeline.js`). Session plays match recently-played rows one-to-one, closest time first, within 2 min. `played_at` is the end of a play, which was measured on 11 real plays. Our observation is taken at the same moment. A replay stays until the API reports it.
- **Logout** cancels a pending search and pending loads, and a session-end signal arriving on the login screen is ignored. Logout also clears all account data: playlists cache, history, session plays, queue and the rendered run. The next login may be another account.
- **After a polling gap** (hidden window), the first poll never records the old track as played. A failed history fetch is retried every 10th tick until it succeeds.
- **Player commands** (play, pause, next, previous, seek, play from Library/Search) run in one ordered chain. Spotify doesn't promise order across player endpoints. Every click joins the chain in click order, so the last action wins. A command queued before a logout is dropped. Play/pause flips the UI at once and reverts only if the latest click fails. Login saves new tokens under the token lock.
- **Seek** is blocked from the moment a track change is asked for until a poll started after it lands. During that time the screen still shows the old track's position. Each seek carries the track generation of its click or keypress and is dropped if the track changed. Keyboard seek moves the bar at once and sends 1 seek after 250ms of quiet. A held arrow key doesn't queue dozens of seeks in front of Pause. A slow Play from Library or Search closes the overlay only if the user is still on that view.
- **Play state**: a poll doesn't overwrite a play/pause that is still queued, or one that landed less than 500ms before the poll started.
- **Local files** in playlists show as dimmed, disabled rows. They are left out of play requests, because Spotify rejects them.
- **Search albums** return `{id, name, artists, cover}` only.
- **Dev harness** has an extra `ad` scenario. `window.__mock.handlers` lets QA inject failures. `src/markup.test.js` fails if `dev/index.html` drifts from `src/index.html`.
- **Not checked** in the real Tauri WKWebView window. Web harness only, per the user.
