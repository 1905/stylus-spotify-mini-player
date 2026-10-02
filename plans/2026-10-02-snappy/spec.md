# Snappy — resume where you left off, instant local controls, fast lists, clear loaders

**Date:** 2026-10-02
**Scope:** ~/dev/rust-spotify
**Status:** approved 2026-10-02 (user: "plan it well. then review 1 time with astra then implement")

## TL;DR

**1. Resume where you left off.** On start, the app loads the last song, at the same second, from the same playlist or album, **paused**. Nothing plays until you press play. *Why:* every launch began empty. *You do:* nothing. *Does NOT:* continue a song that is playing on another device; that one stays where it is.

**2. Instant controls on this Mac.** When "The Run" is the device, play/pause, volume, seek, next and previous go straight to the player inside the app instead of through Spotify's servers. *Why:* about 1 s+ lag per click today. *You do:* nothing. *Does NOT:* change anything for the Marantz or your phone; they still go through Spotify (their lag is Spotify's).

**3. Fast playlists.** Pages load in parallel (about 4× faster on big playlists), and every list is cached on disk. A playlist you opened before shows instantly, then quietly refreshes if it changed on Spotify. *Why:* your 759-song playlist takes about 6.5 s today. *You do:* nothing. *Does NOT:* work offline (playing still needs the internet).

**4. Clear "wait" states everywhere.** Skeleton rows while lists load. The play button turns into a spinner from click until sound starts, with "Starting…" under the title. Loaders also show on device switches, artist pages and search. *Why:* today a slow action looks like a dead click. *You do:* nothing. *Does NOT:* add progress percentages or a splash screen.

**5. Play a nearby cover.** On the main screen, hovering a played or upcoming cover shows its title **and a play button**. Clicking it jumps to that song in the same playlist or album, and the covers slide back or forward with the existing animation. *Why:* you asked; today the covers are display-only. *You do:* nothing. *Does NOT:* scroll the run or show more covers than today. It's only the ones you can see. If an old song isn't in the current playlist, it plays alone (or in the playlist it was played from, when Spotify recorded that).

## Problem(s)

1. **No resume.** `player.rs` builds the session from `SessionConfig::default()`, so the device id is a new UUID each launch. Spotify keeps the old session on a device id that no longer exists, and the app starts idle.
2. **Every control takes the round trip.** `withDevice` → `invoke("resume"|"pause"|"set_volume"|…)` → Web API → Spotify cloud → dealer → librespot (`src/app.js` transport, `src-tauri/src/spotify.rs`), plus the 200 ms volume debounce. The user feels about 1 s on volume and "a few seconds" on play.
3. **Slow playlists.** `get_playlist_tracks` (`spotify.rs:592`) follows `next` serially at limit 50: 759 tracks = 16 × about 0.4 s. Nothing is cached across opens or launches.
4. **Silent waiting.** A clicked row or play button gives no feedback until the data or sound arrives. Lists show a bare "Loading…" line.

## Goals

1. A stable device id, persisted. On start, if no other device is playing, the app moves the last session to "The Run" paused, at the same track and position, keeping the same context (P1).
2. A local fast path for play/pause/seek/next/prev/volume when the active device is "The Run", via Spirc; the Web API path stays for other devices (P2).
3. Parallel playlist paging. A disk cache for playlist/album/liked/top lists, keyed by `snapshot_id` where Spotify gives one; stale-while-revalidate (P3).
4. Skeleton and spinner states for every async view and for the play button, with a "Starting…" label (P4).

## Non-goals

- Offline playback or downloads.
- Changing how other devices are controlled.
- Preloading the next track's audio (librespot already preloads near track end).

## Resume (P1)

```
launch → engine ready ("The Run", stable device id)
       → GET /me/player
           ├─ another device is_playing → leave it (UI shows that device)
           ├─ a session exists, paused or on a vanished device
           │      → PUT /me/player {device_ids:[run], play:false}   (transfer, no sound)
           └─ 204 / nothing
                  → local last-state file {context_uri, track_uri, position_ms}
                    → Spirc load(context, track, position, start_playing:false)
```

- **Device id:** generated once and stored in `player-device-id` (0600) next to the credentials.
- **Last state:** the poll writes `{context_uri, track_uri, position_ms}` to `last-session.json`, throttled to once every 10 s and on pause/quit.
- **"Same playlist":** Spotify's context when the play has one. When the app played a list by URIs (no context), the app remembers which playlist/album the user clicked and restores it as the context.
- **Unverified:** whether a transfer with `play:false` restores the context and position. P1 starts with one live check. If it doesn't, the local fallback branch is used.

## Local fast path (P2)

New Rust commands for when the active device id equals the engine's: `local_play`, `local_pause`, `local_seek {ms}`, `local_next`, `local_prev`, `local_volume {percent}`.
- They call Spirc directly. Spirc reports the new state to Spotify itself, so other clients stay in sync.
- The JS transport picks the local command when `state.device.id === engine.deviceId` (`engine_status` gains `device_id`).
- Same queue (`withDevice`) and intents as today. The volume debounce drops to 0 ms locally (SoftMixer is instant).
- Playing a new list/track on "The Run" (`play_on_device` / `play_context`) stays on the Web API in v1. Spirc `load` needs a LoadRequest built from our URIs; that's a P2 stretch after the basics land.

## Fast lists + cache (P3)

- **Parallel paging:** `get_playlist_tracks` reads `total` from page 1, then fetches the remaining offsets 4 at a time, keeping order. Same for liked songs and saved albums.
- **Disk cache:** `cache/lists/<key>.json` in the app dir.
  - Playlists are keyed by id plus `snapshot_id` from `/me/playlists`. Same snapshot means a cache hit with no request.
  - Albums are immutable and cached forever.
  - Liked/top lists are shown from cache, then revalidated in the background.
  - Total cache capped at 50 MB, least recently used evicted first.
- **UI:** the detail view renders cached rows at once, refreshes in place if the data changed, and never blanks the list.

## Loaders (P4)

- **Lists** (library groups, detail, search, artist): 6–10 skeleton rows with a soft shimmer. Reduced motion means static grey.
- **Play button:** spinner from click until `playback_state` reports playing (or 8 s, then the toast "Spotify is slow to respond").
  - The title line shows "Starting…".
  - Row clicks dim the clicked row and show a small spinner in it.
- **Device switch:** the chip shows a spinner and "Moving to <name>…" until the transfer lands.

## File-level changes

| file | change |
|---|---|
| `src-tauri/src/player.rs` | Persisted device id; `device_id` in status; the 6 `local_*` commands; a resume step after `ready`. |
| `src-tauri/src/spotify.rs` | Parallel paging helper; `get_playlist_tracks` / liked / albums use it. |
| `src-tauri/src/cache.rs` (new) | Disk list cache (get/put/evict, snapshot-keyed). |
| `src-tauri/src/lib.rs` | Register the new commands. |
| `src/app.js` | Local fast path in the transport; resume handling; last-session writes; cached-first lists; loaders. |
| `src/lib/*` + tests | Pure helpers: choose local vs remote, last-session throttle, skeleton markup. |
| `src/styles.css`, `src/index.html`, `dev/index.html` | Skeleton/shimmer, play-button spinner, chip spinner. |
| `dev/mock.js` | Local commands, cache simulation, a slow-network scenario for loaders. |

## Tests

- **Rust unit:** paging order with out-of-order completion; cache hit/miss/evict; device-id persistence; local-vs-remote choice.
- **JS unit:** last-session throttle; transport routing; loader state transitions.
- **Harness QA (Sonnet):** skeletons on slow lists; play spinner until playing; "Starting…"; the device-switch loader; cached detail opens instantly.
- **Live:**
  - Quit while paused mid-song → relaunch → same song, same second, same playlist, paused.
  - Volume and play/pause on "The Run" feel instant: measure the click → audio change with timestamps from the player events.
  - Open the 759-song playlist: first open (was ~6.5 s), second open (instant).
- **Exit:** Astra until 0 crit/high, `/simplify`, final Astra.

## Failure modes & decisions

| failure | behaviour |
|---|---|
| Another device is playing at launch | Don't touch it; the UI shows it. |
| Transfer `play:false` doesn't restore the context | Use the local `last-session.json` via Spirc load (paused). |
| Last track unavailable | Start idle, no error. |
| Local command fails | Fall back to the Web API path once. |
| Cache corrupt / unreadable | Ignore it, fetch fresh. |
| A list changed on Spotify since cache | Show cached rows, replace in place when fresh rows arrive. |
| Play takes > 8 s | Stop the spinner; toast "Spotify is slow to respond". |

## Out of scope

- Spirc `load` for new lists, unless the P2 stretch lands
- Offline mode, audio cache
- Loaders for the cover run on the stage (it already animates)

## Rollout

- **P1:** resume (live check first).
- **P2:** local fast path.
- **P3:** parallel paging + cache.
- **P4:** loaders.
- **P4b:** play a nearby cover (hover play button, jump within the context).
- **P5:** QA, Astra loop, `/simplify`, merge, notify.
