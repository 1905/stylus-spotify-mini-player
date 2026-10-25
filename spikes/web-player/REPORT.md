# web-player spike: the Spotify web player inside Tauri's WebKit layer, Free account

**Question.** Free accounts cannot play through librespot: Spotify's key server denies every audio key
(`error audio key 0 1`, tested 2026-10-24 at 320, 160 and 96 kbps). Can the Spotify web player
(open.spotify.com) play inside a WKWebView created by wry 0.57 / tao 0.37 (the versions Tauri 2 uses
in src-tauri/Cargo.lock) on macOS, with a Free account; can Stylus control it over Connect; does it
keep playing with its window hidden; does Stylus's own OAuth login inside the same WebView log
the web player in too; and how can Stylus tell which Connect device is its own web player?

**Setup.** `cargo run` in this folder: one window, Safari's user agent, `with_autoplay(true)`, no code
signing. Default mode: open.spotify.com in wry's default (persistent) data store; the window hides
itself after 7 "playing" polls. `SPIKE_MODE=oauth`: an incognito store starting at Stylus's PKCE
authorize URL (auth.rs `authorize_url`, same client id, scopes and redirect); the navigation handler
catches the redirect to 127.0.0.1:5588 and loads open.spotify.com in the same store. An init script
reports EME key-system probes, the page's `fetch`/XHR calls to connect-state, console errors and
page errors over IPC; the host polls the page every 3 s. Run on 2026-10-25, macOS 15 on Apple
Silicon, a Free account created in the "31…" username era. The Connect checks used a local Stylus
0.4.1 build with the Premium gate removed and its MCP server on.

| check | expected | seen | result |
|---|---|---|---|
| page loads as a supported browser | the web player, no "browser not supported" | web player UI; it probed `WebKitMediaKeys.isTypeSupported` for Widevine, PlayReady and `com.spotify.invalid` (all false) | PASS |
| login inside the WebView | a session that survives | Google sign-in opened; the passkey step failed ("Something went wrong"), the "get a code" path worked; back on open.spotify.com at 132 s | PASS (no passkeys) |
| DRM | a key system is granted | `WebKitMediaKeys.isTypeSupported com.apple.fps.1_0 audio/mp4 -> true` (FairPlay through WebKit's legacy prefixed EME); no standard `requestMediaKeySystemAccess` call | PASS |
| playback | position advances, sound audible | "I Touch Myself • Divinyls": button Pause, `mediaSession.playbackState` playing, position 0:00 → 0:05 → 1:28 without page errors; the user heard it; still playing at 7 min on another playlist | PASS |
| the web player is a Connect device for this account | it is in the cluster Stylus's session receives | Stylus log `first Connect cluster: 2 devices, active device 30b5bbe4…`; MCP `devices`: `This Mac` and `Web Player (Safari)` (active, type Computer, volume 100) | PASS |
| Stylus controls it | pause and resume reach the page | MCP `pause` → `{"path":"connect"}`, the page showed Play / paused at 0:57 within 6 s; `resume` → Pause / playing, 0:59 → 1:02 | PASS |
| login persists across launches | no login after a restart | the relaunched spike showed the web player with `user-widget-link` present at 3 s, no login button | PASS |
| Stylus starts playback remotely, no click in the page | a playlist plays on the web player | MCP `play {uri: playlist, device: "Web Player (Safari)"}` → the page went Play/paused → "Never Tear Us Apart • INXS", playing, position 0:01 → 0:16 | PASS |
| hidden window keeps playing | position advances after `set_visible(false)` | `WINDOW HIDDEN` at 33 s; `document.hidden` true; position 0:21 → 0:57 over the next 36 s; Stylus's `now_playing` said `is_playing: true` at 60.9 s; the user heard it. Note: `mediaSession.playbackState` read "paused" while hidden although playback went on | PASS |
| one login for both | after Stylus's OAuth login in the WebView, open.spotify.com is logged in | incognito store; Google by code; `OAUTH REDIRECT caught, code present: true` at 86.8 s; open.spotify.com loaded at 90 s with `user: true`, no login button, no consent page | PASS |
| the web player's own device id | something the page can tell the host | not in `localStorage` (no key matching device/harmony). The page registers itself with `PUT https://gew4-spclient.spotify.com/connect-state/v1/devices/hobs_<id>`, and `<id>` is the cluster device id (`0a19054b…` in both). The id changed on every page load (`30b5bbe4…`, `b360e966…`, `d5345fbf…`, `0a19054b…`), so the cluster name "Web Player (Safari)" alone cannot identify it | PASS (capture the PUT) |
| phone's device picker | shows the web player | not shown on the user's phone (not investigated; Stylus's own cluster view is what matters) | n/a |
| memory | – | spike main process 119 MB RSS while playing; WebKit's helper processes not counted | info |

## What this means for Stylus

- A Free account can play on this Mac through the web player in a hidden WKWebView, and Stylus already
  controls remote Connect devices through its session (`path: connect`). A "Free mode" is: a hidden
  webview window with open.spotify.com as the local speaker, Stylus's UI on top, commands over the
  existing remote-device path. librespot's player stays for Premium accounts that still get keys, and
  its session stays the control plane in Free mode (every remote command needs the engine `Ready`).
- One login: if Stylus's OAuth page opens in that same webview instead of the system browser, the
  web player inherits the session. Passkeys do not work inside WKWebView; password and Google-by-code do.
- The web player must tell Stylus its device id: an init script that sees the device PUT.
- The Premium gate in player.rs becomes a mode switch, not a dead end.

## Open questions, not covered here

- Ads: none were observed in ~15 minutes of Free playback; how the web player's ad breaks look to
  Stylus (track metadata, position, the "ad" context) is unknown.
- Login methods other than Google by code (email + password, Apple) were not tried in the WebView.
  The web player's session lifetime in a persistent data store is unknown.
- Now Playing: whether WebKit registers its own Now Playing entry next to Stylus's, media keys.
- Web player changes: Spotify can change the UI or its DRM path at any time; the spike reads only
  `data-testid` hooks and the device PUT for observation, control goes over Connect.
- The user agent is Safari's; the real one is a tracked lie. Not tested without it.
