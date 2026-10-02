# Standalone — this app plays audio itself (librespot)

**Date:** 2026-10-02
**Scope:** ~/dev/rust-spotify
**Status:** approved 2026-10-02 (user: "approved but do prototype first super minimal")

## TL;DR

**P0. Spike: can librespot play here? (half a day)** One test: our Rust backend logs in with librespot and plays one song out of the Mac speakers. *Why:* everything below depends on it. *You do:* one browser login for the player. It probably can't reuse the app's login (to be confirmed). *Does NOT:* change the app. If it fails, I stop and report, and nothing else gets built.

**P1. "This Mac" plays inside the app.** The app becomes its own Spotify speaker named "The Run". "This Mac" in the device menu plays through it, and "Opens Spotify" goes away. *Why:* you want to uninstall the 2 GB Spotify app. *You do:* uninstall Spotify when you're happy. *Does NOT:* work offline or without Premium, or download songs. The Marantz and phone work as before.

**P2. Media keys + Now Playing.** The keyboard play/pause/next keys and the macOS Now Playing widget control the app and show the current song. *Why:* the Spotify app did this, and losing it would feel broken. *You do:* nothing. *Does NOT:* add a menu-bar app or a mini player.

**Risk you accept:** librespot is unofficial and against Spotify's terms, and the risk is on your account. When Spotify changes its protocol, Mac playback stops until librespot ships a fix (days to weeks). Other speakers keep working.

## Problem(s)

1. **No audio without the Spotify app.** The Web API only sends commands to players (`docs/spotify-web-api-reality.md`, Player section). "This Mac" therefore starts the Spotify app in the background (`src-tauri/src/local.rs:9` `launch_local_spotify`, `src/app.js:1042` `playOnThisMac`). The user wants that app gone: "why its opens spotify wtf?"
2. **When the Spotify app quits, the Mac vanishes as a device.** That was the cause of the earlier "can't play" 403/404 (devices list showed only the Marantz). An in-app player lives exactly as long as the app.
3. **Media keys and Now Playing came from the Spotify app.** Without it, the keyboard media keys and the macOS Now Playing widget do nothing for this app.

## Goals

1. librespot runs inside the backend, appears as the Connect device "The Run", and plays audio through CoreAudio (P1; traces to problem 1).
2. "This Mac" in the picker selects "The Run" with no app launch, and "Opens Spotify" is removed (P1; traces to problems 1 and 2).
3. The player starts with the app, stops with it, and reconnects by itself after a network drop or sleep (P1; traces to problem 2).
4. Media keys and Now Playing reflect and control whatever device is playing (P2; traces to problem 3).
5. All existing features keep working unchanged on other devices (P1–P2).

## Non-goals

- Offline downloads. librespot streams only, and Spotify's offline files are locked to its apps.
- Our own reimplementation of Spotify's streaming or key protocol. librespot is the implementation.
- Spotify's web player or Widevine in the web view (DRM; unlikely to work in WKWebView).
- The internal-API extras (play counts, mixes, radio). Those live in `plans/2026-10-02-internal-api`.
- A redesign. That's a separate design pass later.

## Player architecture

```
┌──────────── Tauri app (one process) ─────────────┐
│ UI (unchanged)                                    │
│   │ invoke                                        │
│ Rust backend                                      │
│   ├─ spotify.rs  public Web API ── controls ──┐   │
│   └─ player.rs   librespot                    │   │
│        Session ── Spirc (Connect) ── Player ──┼── CoreAudio → speakers
│        device name "The Run", type Computer   │   │
└───────────────────────────────────────────────┼───┘
                                                ▼
                          Spotify cloud: "The Run" is a device like the Marantz
```

- **Control stays on the Web API.** The UI already transfers, plays, seeks and queues through `spotify.rs`. "The Run" is just another device id, so the UI needs almost no change.
- **librespot crates (0.8.0, MIT):** `librespot-core` (session), `-connect` (Spirc), `-playback` (rodio backend → CoreAudio), `-oauth` (login), `-discovery` off (no mDNS needed: we register through the cloud session).
- **Login:**
  - P0 checks whether our existing OAuth token (19 scopes, incl. `streaming`, `src-tauri/src/auth.rs:38`) is accepted by librespot's session.
  - If it isn't, a one-time librespot OAuth login runs in the browser, and librespot's reusable credentials are stored in the macOS Keychain.
- **Lifecycle:**
  - The player starts at app launch, after auth is "ok", and shuts down cleanly on quit.
  - On session loss (network, sleep), it reconnects with backoff.
  - Its state is exposed as an `engine_status` command (`starting | ready | reconnecting | failed: <reason>`).
- **Audio:** default output device, normal bitrate (160 kbps), no audio cache in v1. Volume goes through the Connect volume we already send (`set_volume`).

## Media keys + Now Playing (P2)

- macOS `MPRemoteCommandCenter` and `MPNowPlayingInfoCenter`, through a small Rust binding (objc2) or a Tauri plugin. Which one is decided in the plan after a check.
- **Now Playing:** title, artist, album, cover, duration, position and play state, from the poll we already run. It updates on track change and on play/pause.
- **Commands:** play/pause/next/previous/seek from the OS go to the same functions the buttons use (`togglePlay`, `skip`, `seekTo`), so the v1/v2 ordering and session guards apply.

## File-level changes

| file | change |
|---|---|
| `src-tauri/Cargo.toml` | Add the librespot crates (0.8.0) with the rodio backend and no discovery. Add the media-control binding (P2). |
| `src-tauri/src/player.rs` (new) | librespot session, Spirc and Player; the start/stop/reconnect loop; an `engine_status` command; credential storage in the Keychain. |
| `src-tauri/src/local.rs` | Removed in P1 (replaced by the in-app player). |
| `src-tauri/src/lib.rs` | Start the player in `setup`, stop it on exit, and register the commands. |
| `src-tauri/src/media.rs` (new, P2) | Now Playing info + remote commands, emitted to the UI as Tauri events. |
| `src/app.js` | "This Mac" row → transfer to "The Run" (no launch). Engine status in the device menu ("Starting…", "Reconnecting…"). Now Playing updates and handling of OS commands (P2). |
| `dev/mock.js` | "The Run" device, `engine_status` states, media command events. |
| `docs/spotify-web-api-reality.md` | Note that "The Run" is a librespot device and what the Web API sees of it. |

## Tests

- **P0 spike (manual, live, on the MacBook):**
  - login works
  - "The Run" appears in `/me/player/devices`
  - a transfer from the Web API plays audible sound
  - pause/seek/next work
  - the app's process memory and binary size are recorded
- **Rust unit:** engine status transitions, credential load/save (Keychain mocked behind a trait), device-name config.
- **Harness QA (Sonnet, `/playwright-cli`, read-only):**
  - "This Mac" picks "The Run" with no launch
  - engine states show correctly
  - media events drive play/pause/next
- **Live (orchestrator):**
  - play, pause, seek, next, volume on "The Run"
  - transfer between "The Run" and the Marantz, both ways
  - sleep/wake and Wi-Fi off/on: the player reconnects
  - media keys and Now Playing show the right song
- **Regression:** all v1/v2 harness scripts, the 24-shot sweep, cargo + vitest.
- **Exit:** Astra until 0 crit/high (crit/high-only rule), `/simplify`, final Astra.

## Failure modes & decisions

| failure | behaviour |
|---|---|
| P0: librespot can't log in or play | Stop. Report. Keep the current "Opens Spotify" path. |
| Our OAuth token rejected by librespot | One-time player login in the browser; credentials kept in the Keychain. |
| Session drops (network, sleep) | Reconnect with backoff. The device menu shows "This Mac — reconnecting…". Other devices unaffected. |
| Spotify breaks librespot's protocol | `engine_status: failed`, and the menu shows "This Mac isn't available right now". Other devices keep working. Fix = update librespot. |
| Account not Premium | `failed: Premium required`, shown once. |
| No audio output device | `failed: no audio output`. |
| Media framework call fails | Ignore and log; buttons still work. |

## Out of scope

- Offline mode and downloads
- Internal API extras (own plan), design pass (own plan)
- Equalizer, crossfade, gapless tuning, audio cache
- Windows/Linux builds

## Rollout

- **P0:** spike branch, librespot plays one song (gate: you hear it). Report before going on.
- **P1:** "The Run" device + "This Mac" through it; `local.rs` removed (gate: live tests + harness QA).
- **P2:** media keys + Now Playing (gate: live check with the keyboard and Control Center).
- **P3:** exit gate (Astra, `/simplify`, final Astra), merge, notify. Then you uninstall the Spotify app.

## P0 result (2026-10-02): PASS

Prototype: `spikes/librespot/` (librespot 0.8.0, rodio → CoreAudio). It played "Audiophile Mix" from the user's playlists out of the MacBook speakers, and the user confirmed "i hear".

- **Login:**
  - The app's own OAuth token logs librespot in, but every audio fetch then fails with `INVALID_CREDENTIALS`.
  - Audio needs librespot's own OAuth (Spotify's keymaster client id, PKCE, redirect `127.0.0.1:5588`).
  - With an existing browser session the login is automatic (no prompt).
  - librespot's reusable credentials are cached, so later runs need no browser.
- **Build:** pin `vergen = 9.0.6` (9.1 breaks `librespot-core`'s build script with `vergen-gitcl` 1.0.8).
- **Cost while playing:** 25 MB footprint (43 MB RSS), ~1.4% CPU; spike binary 10.6 MB (no size profile).
- **For P1:** store librespot's credentials in the Keychain (the spike used a file under `~/Library/Application Support/rust-spotify/librespot-spike/`). The app will need 2 logins on first run, Web API and player; ideally both happen in one browser visit.
