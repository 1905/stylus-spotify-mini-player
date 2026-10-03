<p align="center">
  <img src="media/og.png" alt="Needle — a small Spotify player for macOS" width="100%" />
</p>

# Needle

A small, fast Spotify player for macOS. Needle plays music on your Mac by itself, so you do not need the 2 GB Spotify desktop app.

<p align="center">
  <img src="media/screenshot.png" alt="Needle playing a track" width="720" />
</p>

<table>
  <tr>
    <td width="33%"><img src="media/focus.png" alt="Focus layout with the cover row turned off" /></td>
    <td width="33%"><img src="media/library.png" alt="The Library, Albums tab" /></td>
    <td width="33%"><img src="media/tint.png" alt="The window tinted to the album cover" /></td>
  </tr>
  <tr>
    <td align="center">Focus layout: one cover, centred</td>
    <td align="center">Library with tabs</td>
    <td align="center">Colours follow the album cover</td>
  </tr>
</table>

## Install

### TL;DR

```sh
brew install --cask 1905/tap/needle
```

Or download `Needle.dmg` from [Releases](https://github.com/1905/needle-spotify-mini-player/releases/latest) and drag Needle to Applications.

Needle is not signed with an Apple Developer ID. On first launch, macOS may refuse to open it. Run this once:

```sh
xattr -dr com.apple.quarantine /Applications/Needle.app
```

### From source — TL;DR

Requires macOS (Apple Silicon), [Rust](https://rustup.rs) 1.85 or later, and Node.js 20 or later.

```sh
git clone https://github.com/1905/needle-spotify-mini-player.git
cd needle-spotify-mini-player
npm install
make install   # builds Needle.app and copies it to /Applications
```

Other targets:

| Command | What it does |
|---|---|
| `make install` | Release build of `Needle.app` into `/Applications`, then opens it. |
| `make run` | Fresh release build, run as a bare binary from the terminal. It has no Dock icon, so you can tell it apart from the installed app. |
| `make stop` | Quits every running copy. |
| `npx vitest run` | Frontend unit tests. |
| `cargo test --manifest-path src-tauri/Cargo.toml` | Backend unit tests. |

## Requirements

- macOS 13 or later on Apple Silicon.
- A **Spotify Premium** account. Playback through Spotify Connect requires Premium.

### Spotify login

Needle signs in twice on first launch, both in your browser:

1. **Library access** — the Spotify Web API login (OAuth with PKCE, no client secret).
2. **The player** — the login for the built-in Spotify Connect speaker.

The included client ID belongs to a Spotify developer app in development mode. Spotify only lets accounts on that app's allowlist sign in. To use Needle with your own account, create an app at [developer.spotify.com](https://developer.spotify.com/dashboard), add the redirect URI `http://127.0.0.1:1420/callback`, and put its client ID in `CLIENT_ID` in `src-tauri/src/auth.rs` before you build.

## Features

- **Plays on your Mac.** Needle contains its own Spotify Connect speaker, shown as "Here" in the app and as "This Mac" in other Spotify apps. Phones and other devices can send music to it.
- **Controls other devices.** Pick any of your Spotify Connect devices from the device menu.
- **Resumes where you stopped.** The playlist, song, position, volume, shuffle and repeat are restored, paused, when you open the app again.
- **Library and search.** Playlists, Liked Songs, albums, artists with their popular tracks, your top tracks and artists, and search across all of them.
- **Playlist view.** One panel shows what played before, what plays now and what comes next.
- **Two layouts.** A row of covers that shows what played and what comes next, or a single centred cover (Settings → Show cover row).
- **Native feel.** Media keys and Now Playing, the current album cover as the Dock icon, a smooth volume ramp, and instant skip and pause.
- **Audio quality.** Choose 96, 160 or 320 kbps.
- **Low on network.** While Needle is the active speaker, it reads the player state from the speaker itself instead of polling Spotify.

## How it works

- **App shell:** [Tauri 2](https://tauri.app) with a plain JavaScript frontend.
- **Playback:** [librespot](https://github.com/librespot-org/librespot) 0.8, embedded as a Spotify Connect device, with a custom audio output that applies volume at playback time.
- **Library data:** Spotify's internal endpoints, the same ones the official apps use, through the player's own session. The public Web API is used as a fallback. When Spotify rate-limits the Web API, Needle respects `Retry-After` and keeps playing.
- **State on disk:** `~/Library/Application Support/needle/` holds the login tokens, the player credentials (`0600`), the session, settings, a list cache and the log file `logs/needle.log`.

## Disclaimer

Needle is an independent project. It is not affiliated with, endorsed by or connected to Spotify. Spotify is a trademark of Spotify AB.

Needle uses librespot and undocumented Spotify endpoints. Spotify can change or block them at any time. Spotify's terms may not permit this use. Use Needle at your own risk.

## License

[MIT](LICENSE)
