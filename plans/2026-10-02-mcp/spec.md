# MCP — control Needle from any MCP client

**Date:** 2026-10-03
**Scope:** ~/dev/rust-spotify (Needle)
**Status:** pending review

## TL;DR

**1. An MCP server inside Needle, local only.** When you turn it on in Settings, Needle serves MCP at `http://127.0.0.1:5590/mcp` while the app is open. Any MCP client on this Mac (Claude Code, Claude Desktop, Cursor, others) connects to it and gets tools to search, play, pause, skip, change volume, list your playlists/albums/liked songs, queue songs, switch devices and like songs. Changes show in the open app within a second. *Why:* you want to say "find X and play it" in your AI tools. *You do:* nothing. *Does NOT:* listen on any network address but this Mac's loopback, and doesn't work while Needle is closed.

**2. Settings: on/off + two copy buttons.** In the cog menu: "MCP server" toggle, off by default. Under it: "Copy connect" with a choice of format — the generic JSON block most MCP clients take (`{"mcpServers":{"needle":{"type":"http","url":…,"headers":{"Authorization":"Bearer <key>"}}}}`), or the `claude mcp add …` line for Claude Code — and "Copy skill" (a short SKILL.md / instructions text that tells an agent how to use the tools well). *Why:* one paste connects any client. *You do:* paste once into your client; optionally save the skill. *Does NOT:* edit any client's config by itself.

**3. A key on every request.** The server only answers requests that carry a random key (made once, kept in `settings.json`, 0600). "Reset key" makes a new one. *Why:* any program or web page on this Mac could otherwise reach a localhost port and control your Spotify. *You do:* nothing, the key is inside the copied command. *Does NOT:* use OAuth or any account.

## Problem(s)

1. **No outside control surface.** Needle's only entry points are Tauri commands for its own webview (`src-tauri/src/lib.rs`, the `invoke_handler` list). Claude Code speaks MCP and can call none of them.
2. **Control logic is split.** Local commands to the in-app speaker go through Spirc in Rust (`player.rs` `local_*`), remote devices through the Web API (`spotify.rs`), but the choice between them is made in JS (`src/lib/route.js` `isLocal`, `src/app.js` `routed`/`playSource`). An MCP server in Rust needs that choice in Rust too.
3. **A localhost port is reachable by any local process and by web pages** (DNS rebinding, `fetch` to 127.0.0.1). Without a key and an Origin check, a web page could drive the player.

## Goals

1. MCP server (streamable HTTP) in Needle on `127.0.0.1:5590`, started and stopped by a setting, only while the app runs. (P1)
2. Tools covering search, playback control, library listing, queue, devices, like/unlike, now playing. (P1)
3. Device routing in Rust: the in-app speaker gets Spirc commands, other devices get Web API calls, same rule as the UI. (P2)
4. Bearer key + Origin/Host checks on every request. (P1)
5. Settings UI: toggle, status line, copy connect command, copy skill, reset key. (P3)
6. Every tool call logged to `needle.log` (tool name, args summary, result or error). (P1)

## Non-goals

- Remote clients (ChatGPT web, phones), tunnels, public URLs, Cloudflare, OAuth. Local clients only.
- A server that runs while Needle is closed.
- Writing Claude Code's config or skills folder automatically.
- New Spotify features the app doesn't have (no recommendations: that API is gone, see `docs/spotify-web-api-reality.md`).

## Server

```
MCP client ──HTTP POST /mcp (Authorization: Bearer <key>)──▶ 127.0.0.1:5590 (Needle, rmcp streamable-http)
                                                               │
                                     ┌─────────────────────────┴──────────────────────────┐
                               spotify.rs (Web API: search, library, remote devices)   player.rs (Spirc: This Mac)
                                                               │
                                              the open app sees the change on its next poll (≤1 s)
```

- Crate: `rmcp` 3.5.0 (`server`, `transport-streamable-http-server`, `macros`), axum router on tokio, bound to `127.0.0.1` only. Port 5590 fixed so the copied command stays valid; if it's taken, the setting shows "port 5590 is in use" and the server stays off.
- Stateless mode (no MCP session needed) unless rmcp requires sessions for Claude Code; decided in the plan after a test.
- Checks before any tool runs: `Authorization: Bearer <key>` equals the stored key (constant-time compare) → else 401. `Host` must be `127.0.0.1:5590` or `localhost:5590` → else 403. A request with an `Origin` header that isn't absent/`null` → 403 (browsers send Origin; MCP clients don't — if a client turns out to send one, its value is allowed explicitly).
- Key: 32 random bytes, base64url, in `settings.json` as `mcp_key`; `mcp_enabled: bool` (default false).

## Tools

All return short JSON. Track = `{uri, name, artists, album, duration_ms}`. Errors are plain sentences ("Nothing is playing", "No device to play on: open Needle or Spotify somewhere").

| Tool | Args | Does |
|---|---|---|
| `now_playing` | – | track, position, is_playing, device, shuffle, repeat, volume, context (playlist/album name + uri) |
| `search` | `query`, `type` (track/album/artist/playlist, default track), `limit` (≤10, API max) | results with uris |
| `play` | one of: `uri` (track/album/playlist/artist), `query` (plays the top track hit), `uris[]`; optional `context_uri` + `track_uri` to start inside a playlist/album; optional `device` (name or id) | starts playback; on This Mac via Spirc load, else Web API |
| `pause`, `resume`, `next`, `previous` | – | transport |
| `seek` | `position_ms` or `seconds` | seek |
| `set_volume` | `percent` 0–100 | volume (ramped on This Mac) |
| `set_shuffle` / `set_repeat` | `on` / `mode` off\|context\|track | modes |
| `queue_add` | `uri` or `query` | adds a track to the queue |
| `get_queue` | – | next tracks |
| `list_playlists` | – | your playlists (cached list) |
| `playlist_tracks` | `playlist` (uri/id or name) | tracks (cached by snapshot) |
| `list_albums` | – | saved albums |
| `album_tracks` | `album` (uri/id) | tracks |
| `liked_songs` | `limit`, `offset` | liked tracks |
| `recently_played` | `limit` | history |
| `top` | `kind` tracks\|artists, `range` short\|medium\|long | your top |
| `artist` | `artist` (uri/id or name) | artist + albums (+ your favourites by them) |
| `devices` | – | devices, the active one, "This Mac" marked |
| `transfer` | `device` (name or id), `play` bool | moves playback |
| `like` / `unlike` | `uri` (default: current track) | Liked Songs |

Names are resolved case-insensitively against the user's own lists first (playlist/device names), then search.

## Settings UI

```
⚙  Settings
   Album art as app icon        [on]
   Show cover row               [on]
   Audio quality   Normal | High | Very high
   ─────────────
   MCP server                   [off]
   Running on 127.0.0.1:5590 · 3 calls today       (or: Off / port 5590 is in use)
   Copy connect: [JSON config] [Claude Code]   [Copy skill]   Reset key
```

Copied connect text — generic JSON (most clients), and the Claude Code line (flags verified against `claude mcp add --help` during the plan):

```json
{"mcpServers": {"needle": {"type": "http", "url": "http://127.0.0.1:5590/mcp", "headers": {"Authorization": "Bearer <key>"}}}}
```


```
claude mcp add --transport http needle http://127.0.0.1:5590/mcp --header "Authorization: Bearer <key>"
```

Copied skill: a SKILL.md with frontmatter (`name: needle`, description "Control the Needle Spotify player: find and play music, control playback, browse the library") and short rules: search before play, prefer the user's playlists for names, confirm what started from `now_playing`, never loop calls.

## File-level changes

| File | Change |
|---|---|
| `src-tauri/Cargo.toml` | Add `rmcp` 3.5.0 (server features) and `axum` (version rmcp uses). |
| `src-tauri/src/mcp.rs` (new) | Server start/stop, auth/Host/Origin middleware, tool definitions calling `spotify.rs` / `player.rs`, logging. |
| `src-tauri/src/control.rs` (new) | Rust device routing: `is_local(device)` from engine status + last known active device; play/pause/next/… that pick Spirc or Web API. Used by MCP (the UI keeps its JS routing for now). |
| `src-tauri/src/settings.rs` | `mcp_enabled`, `mcp_key` fields; key generation. |
| `src-tauri/src/lib.rs` | Start the server at launch if enabled; commands `mcp_status`, `mcp_set_enabled`, `mcp_reset_key`, `mcp_connect_command`, `mcp_skill_text`. |
| `src/app.js`, `src/index.html`, `src/styles.css` | Settings rows: toggle, status, two copy buttons (clipboard via `navigator.clipboard` with a toast "Copied"), reset key. |
| `dev/mock.js` | Mock the new commands. |
| `docs/mcp.md` (new) | Tool list and how to connect. |

## Tests

- Rust unit: auth check (missing/wrong/right key, constant-time path), Host/Origin rules, name resolution (exact, case-insensitive, ambiguous → error listing matches), routing decision (`is_local`) as a pure function, settings round-trip with the new fields.
- Rust integration: start the server on a random port in a test, `initialize` + `tools/list` + one read tool against a stubbed Spotify client; 401 without key.
- JS (vitest): settings rows render from `mcp_status`; copy builds the right text.
- Manual: `claude mcp add …` from the copied command, then in Claude Code "play <song>" → it plays in Needle; toggle off → Claude Code shows the server as failed.

## Failure modes & decisions

| Failure | Behaviour |
|---|---|
| Port 5590 taken | Server off, status line says so; toggle stays on and retries at next launch. |
| Needle closed | Claude Code reports the server unreachable. |
| No active device | Play tools start on This Mac (the in-app speaker) when it's ready, else return "No device to play on". |
| Spotify 429 / errors | Returned as the tool error text; logged. |
| Wrong key / browser Origin | 401 / 403, logged once per minute (no log spam). |
| Ambiguous name ("Mix") | Error listing the matches with uris; Claude picks one. |
| Key reset | Old command stops working; copy the new one. |

## Out of scope

- ChatGPT or any remote client.
- Moving the UI's own routing to Rust (only MCP uses `control.rs` now; the UI can switch later).
- Podcasts, playlist editing (add/remove tracks, create playlists).

## Rollout

- P1: settings fields + `mcp.rs` server with auth and the read tools (now_playing, search, lists, devices, queue) + logging. Gate: integration test + manual `claude mcp add` and a read call.
- P2: `control.rs` + control tools (play/pause/next/prev/seek/volume/shuffle/repeat/queue_add/transfer/like). Gate: manual "find and play X" from Claude Code.
- P3: Settings UI (toggle, status, copy buttons, reset key), skill text, `docs/mcp.md`. Gate: harness QA + Astra review of the branch.
