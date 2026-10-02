# Standalone Implementation Plan v1.1

**Date:** 2026-10-02
**Status:** approved (full auto; v1.1 = v1.0 + the 4 confirmed HIGH findings from the Astra plan review, med/low skipped by user rule)
**Spec:** ./spec.md (P0 passed; this plan is P1 + P2 + exit)
**Goal:** The app is its own Spotify Connect speaker "The Run" on this Mac (librespot), "This Mac" plays through it with no Spotify app, and media keys + Now Playing work.
**Architecture:** A long-lived librespot engine (Session + Player + SoftMixer + Spirc) runs inside the Tauri backend and registers through the cloud as a Connect device. The UI keeps controlling everything through the public Web API, so "The Run" is just another device id. OS media controls (souvlaki → MPRemoteCommandCenter / MPNowPlayingInfoCenter) are fed from the existing poll, and route OS commands to the existing JS transport functions.
**Tech Stack:** Rust (tauri 2.12, librespot 0.8.0 core/playback/connect/oauth, keyring, souvlaki), vanilla JS, vitest, playwright-cli.

> For agentic workers: use superpowers:subagent-driven-development to implement task-by-task. Checkbox syntax for tracking.

**Exec notes:**
- Branch `feat/standalone` in the main checkout (no worktrees, user rule).
- Implementers: Opus. They do no git and no live Spotify calls. The orchestrator commits each verified task and runs all live checks.
- Review rule: fix only crit/high.

## File map

**Modify:**
- `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` (pin `vergen` 9.0.6)
- `src-tauri/src/lib.rs`
- `src/app.js`, `src/index.html`, `dev/index.html`, `src/styles.css`
- `dev/mock.js`

**Create:**
- `src-tauri/src/player.rs`, `src-tauri/src/media.rs`

**Move to /tmp/trash:**
- `src-tauri/src/local.rs`. The "This Mac" launch path is replaced.

## Locked interfaces

**Engine (Rust → JS commands):**

| command | args | returns |
|---|---|---|
| `engine_status` | — | `{state: "needs_login"\|"starting"\|"ready"\|"reconnecting"\|"failed", name: "The Run", reason?: string}` |
| `engine_login` | — | `null` once logged in and `ready`; Err on timeout/cancel/mismatch. Opens the browser for the player's OAuth (client id `65b708073fc0480ea92a077233ca87bd`, PKCE, redirect `http://127.0.0.1:5588/login`, scopes as librespot's binary uses) and stores the reusable credentials in the Keychain. **Does not use `librespot-oauth`'s listener** (blocking `accept` + `read_line`, no deadline): it reuses our own `auth.rs` PKCE flow + `wait_for_code` (3-minute deadline, read timeout, skips silent preconnects), generalized to take a client id and port. A second call while one is running returns `Err("LOGIN_IN_PROGRESS")`. Port in use → `Err` naming the port. |

**Engine (Rust → JS event):** `engine-status` with the same payload as `engine_status`, emitted on every state change.

**Same account (Astra #2):** on `ready` the engine compares the librespot session's username with the Web API `/me` id (fetched with the app token). If they differ: state `account_mismatch` (reason names both accounts), the engine stops, and the stored player credentials are replaced only through `engine_login`. `engine_status.state` adds `"account_mismatch"`. When the app logs out (`showLogin`) or logs in again, the UI calls a new `engine_restart` command (`null`): the engine re-checks the account on its next `ready`.

**Media (JS → Rust command):**
- `media_update`, args `{title, artist, album, cover, durationMs, positionMs, playing}` (any may be null), returns `null`.
- `media_clear`, no args, returns `null`.

**Media (Rust → JS event):** `media-command`, payload `{action: "play"|"pause"|"toggle"|"next"|"previous"|"seek", positionMs?}`.

**Engine behaviour:**
- **Start:** on app launch in `setup`, if Keychain credentials exist: `starting` → `ready`. Otherwise `needs_login`; `engine_login` is triggered by the UI.
- **Device:** ConnectConfig name "The Run", `device_type: Computer`, initial volume 50%, volume through SoftMixer (remote volume works).
- **Reconnect:** on Spirc task end or session loss → `reconnecting` → backoff 1s, 2s, 4s… capped at 60s → `ready`. A `BadCredentials`-type failure → `needs_login`; other permanent failures → `failed` with the reason.
- **Quit:** `spirc.shutdown()` on app exit.
- **Keychain:** service `rust-spotify`, account `librespot-credentials`, value = librespot's reusable `Credentials` serialized as JSON. Never logged. Behind a `CredStore` trait, so unit tests use an in-memory store.

**UI:**
- **"This Mac" row:**
  - Shown when no device named "The Run" is in the list and the engine isn't `ready`. Otherwise "The Run" is a normal row.
  - On click: if `needs_login` or `account_mismatch`, call `engine_login`. A resolved `engine_login` **is** readiness: no waiting for an event that may already have fired (Astra #4). Otherwise (`starting`/`reconnecting`), subscribe to `engine-status` first, then read `engine_status`; continue when `ready`, give up after 20s with a toast. Then poll `list_devices` until "The Run" appears (≤20s, toast on timeout) and `pickDevice` it.
- **Engine states in the device menu:**
  - "This Mac — Connecting…" (starting/reconnecting)
  - "This Mac — Log in to play here" (needs_login)
  - "This Mac — Not available right now" (failed, `title` = reason)
- **Media:** after each `renderChrome` that changes track or play state, call `media_update` (throttled to state changes, not every frame). In idle mode, call `media_clear`. Listen to `media-command` and route it to `togglePlay` / `skip("next_track")` / `skip("previous_track")` / `seekTo(ms)`, so existing guards apply.
- **Hidden window keeps polling while something plays (Astra #1):** the v2 rule "stop polling when the window is hidden" now applies only when nothing plays (mode idle). While a song plays, polling continues at 3s instead of 1s when hidden, so Now Playing and media commands stay current. Back to 1s when visible.

**Dev scenarios (new):** `engine-login` (needs_login → ready after a fake login), `engine-down` (failed).

## Tasks

### T0: Baseline (gate, orchestrator)
- [ ] On `feat/standalone`: `cargo test`, `cargo build` with no warnings, `npm test`.

### T1: Engine backend (heavy, Opus)
- [ ] Generalize `auth.rs` PKCE + `wait_for_code` into a reusable `oauth_login(client_id, port, scopes, timeout)`; the app login and the player login both use it (tests: existing `wait_for_code` tests still pass).
- [ ] `engine_restart` command; account check on `ready`; `account_mismatch` state (unit test on the pure state machine).
**Files:** `src-tauri/Cargo.toml`, `Cargo.lock`, `src/player.rs`, `src/lib.rs`, remove `src/local.rs`
- [ ] Add librespot-core/playback(rodio-backend)/connect 0.8.0 (not librespot-oauth: see engine_login) and `keyring = "4"` (default features = macOS Keychain). `cargo update -p vergen --precise 9.0.6`.
- [ ] Failing unit tests first:
  - state machine transitions (pure fn `next_state(state, event) -> state`)
  - `CredStore` memory impl round trip
  - status payload serialization
- [ ] Implement `player.rs`: Engine struct held in Tauri state, a tokio task running the connect loop, events emitted via `AppHandle::emit`, and the two commands.
- [ ] `lib.rs`: manage the Engine, start it in `setup`, shut it down on `RunEvent::Exit`, register the commands, remove `launch_local_spotify`.
- [ ] Verify:
  - `cargo test` passes
  - `cargo build` with 0 warnings
  - the release binary size is recorded

### T2: UI + mock (heavy, Opus), after T1
**Files:** `src/app.js`, `src/index.html`, `dev/index.html`, `src/styles.css`, `dev/mock.js`
- [ ] The "This Mac" flow and engine states exactly per the locked UI. Remove `playOnThisMac`'s launch path.
- [ ] Media wiring per the locked interface (`media_update` / `media_clear` / `media-command`).
- [ ] Mock:
  - `engine_status`, `engine_login`, "The Run" appears in devices after login
  - `media_update` records calls
  - a `__mock.emit(event, payload)` helper to fire `media-command` / `engine-status`
- [ ] New scenarios added.
- [ ] Verify: `npx vitest run`, `node --check`.

### T3: Media controls backend (heavy, Opus), parallel with T2
**Files:** `src-tauri/src/media.rs`, `src-tauri/Cargo.toml`, `src-tauri/src/lib.rs`
- [ ] souvlaki `MediaControls` created on the main thread in `setup` (macOS needs the app run loop).
- [ ] `media_update` / `media_clear` set metadata + playback state.
- [ ] Attach a handler that emits `media-command`.
- [ ] Failure to create controls → log and continue (non-fatal).
- [ ] Verify: `cargo build` with 0 warnings, `cargo test`.

### T4: Gate (orchestrator)
- [ ] All tests, the release build, the harness regression (v1/v2 scripts + sweep).
- [ ] Sonnet QA on the new scenarios (read-only, `/playwright-cli`).
- [ ] **Live:**
  - launch the release app with Spotify.app quit
  - "This Mac" → login (automatic) → "The Run" plays a playlist
  - pause/seek/next/volume from the app
  - transfer The Run ↔ Marantz
  - media keys + Control Center Now Playing
  - Wi-Fi off/on: reconnect
  - memory + binary size
- [ ] Live, from the Astra plan review:
  - minimize the app, let a track change and pause from the phone: Now Playing follows, and media keys act on the current song
  - close the player-login browser tab without logging in: `engine_login` fails within its deadline and a retry works
  - log in to the app as a second account (if available) or fake a mismatch in the mock: the menu shows the mismatch and offers the player login
- [ ] Fix loop (Opus for big, orchestrator for small).

### T5: Exit gate
- [ ] Astra until 0 crit/high, then `/simplify`, then a final Astra.
- [ ] Spec as-built notes, plan done, move to `plans/done/`.
- [ ] Merge to `main`, release build, relaunch the app, notify, stop the watchdog.

## Type-consistency check
- Command names `engine_status`, `engine_login`, `media_update`, `media_clear` are the same in T1/T3 (Rust), T2 (JS + mock) and the locked table.
- Event names `engine-status`, `media-command` are the same in Rust emit and JS listen.
- Device name "The Run" is the same in ConnectConfig, the UI match, and the mock.

## Astra plan review (v1.0 → v1.1)

Rating 5/10. 4 HIGH applied above (#1 hidden-window polling, #2 same account, #3 bounded player login, #4 missed ready event). Skipped by the user's crit/high-only rule: #5 play/pause mapped to toggle (med), #6 Now Playing position after seek (med), #7 placeholder while The Run isn't listed yet (med). #8 keyring backend (med) is a false positive for keyring 4.x, whose default `v1` feature includes the Apple keychain store; T1 pins `keyring = "4"` with defaults.
