# Stylus MCP server

Stylus can serve the Model Context Protocol (MCP) on this Mac, so AI tools such as Claude Code, Claude Desktop or Cursor can search, play and control music in Stylus.

- Address: `http://127.0.0.1:5590/mcp` (streamable HTTP, stateless, JSON answers).
- Runs only while Stylus is open and the setting is on. Off by default.
- Listens on the loopback address only. Nothing outside this Mac can reach it.

## Turn it on

1. Open Stylus, click the cog (Settings).
2. Turn on **MCP server**. The line under it says `Running on 127.0.0.1:5590 · N calls today`, or why it is not running (for example `port 5590 is in use`).
3. Click **JSON config** or **Claude Code** to copy the connect text. Paste it into your client.
4. Optional: click **Copy skill** and save the text as `~/.claude/skills/stylus/SKILL.md`. It tells an agent how to use the tools well.

## Connect

Claude Code (the copied line; `--scope user` makes it available in every project):

```
claude mcp add --scope user --transport http stylus http://127.0.0.1:5590/mcp --header "Authorization: Bearer <key>"
```

Most other clients take this JSON block (Claude Desktop, Cursor and others: `mcpServers` in their config file):

```json
{
  "mcpServers": {
    "stylus": {
      "type": "http",
      "url": "http://127.0.0.1:5590/mcp",
      "headers": { "Authorization": "Bearer <key>" }
    }
  }
}
```

`<key>` is filled in by the copy buttons.

## Tools

All tools return short JSON. Errors are plain sentences. A track is `{uri, name, artists, album, duration_ms}`. Names of playlists, mixes, albums, artists and devices are matched without regard to case: an exact name first, then a unique part of a name. When a name matches several items, the error lists them with their uris.

| Tool | Arguments | What it does |
|---|---|---|
| `now_playing` | – | Track, position, playing or paused, device, shuffle, repeat, volume, and the playlist or album it plays from. |
| `search` | `query`, `type` (track, album, artist, playlist; default track), `limit` (max 10) | Search results with uris. |
| `play` | one of `uri` (track, playlist, album, artist, or a share link), `name` (a playlist, mix, album or artist in your library), `query` (plays the top song hit), `uris` (track list), `context_uri` + optional `track_uri`; optional `device` | Starts playback. |
| `pause`, `resume`, `next`, `previous` | – | Transport. |
| `seek` | `position_ms` or `seconds` | Jumps in the current track. |
| `set_volume` | `percent` (0–100), optional `device` | Sets the volume. |
| `volume_step` | `delta` (for example 10 or -10), optional `device` | Volume up or down. |
| `mute`, `unmute` | optional `device` | Volume 0, and back to the level from before. |
| `set_shuffle` | `on` | Shuffle on or off. |
| `set_repeat` | `mode`: off, context, track | Repeat mode. |
| `queue_add` | `uri` or `query` | Adds a song to the queue. |
| `get_queue` | – | The next songs. |
| `list_playlists` | – | Your playlists, plus playlists added in Stylus by link (`saved_in_app`). |
| `list_mixes` | – | The Mixes tab: Made For You mixes from your Spotify home (Daily Mixes, Discover Weekly, Release Radar, artist radios and mixes), mixes added by link, and mixes you played. |
| `playlist_tracks` | `playlist` (uri, id, link or name), `limit`, `offset` | The songs of a playlist or mix. |
| `list_albums` | – | Saved albums, plus albums added by link. |
| `album_tracks` | `album` (uri, id, link or name) | The songs of an album. |
| `list_artists` | – | Followed artists, plus artists added by link. |
| `liked_songs` | `limit`, `offset` | Liked Songs, newest first. |
| `recently_played` | `limit` | Recent plays (one per playlist or album). |
| `top` | `kind` (tracks, artists), `range` (short, medium, long) | Your top tracks or artists. |
| `artist` | `artist` (uri, id, link or name) | Popular tracks, albums, and your liked songs by the artist. |
| `devices` | – | Spotify Connect devices. The active one and This Mac are marked. |
| `transfer` | `device` (name or id), `play` (default true) | Moves playback to a device. |
| `like`, `unlike` | optional `uri` (default: the current song) | Liked Songs. |
| `open_link` | `link`, optional `save`, `play`, `device` | Looks up a Spotify share link or uri. `save: true` adds it to Stylus's library; `play: true` plays it. |

### Devices

"This Mac" is Stylus's own speaker. Commands for it go straight to the player in Stylus. Stylus lists your other Spotify Connect devices, and `transfer` can move playback from them to This Mac. A command that Stylus cannot send to another device fails with a sentence that starts with "Not available for other devices". Library lists, search and the mixes come from Spotify's internal API, through the player's own session.

## Security

- The server listens on `127.0.0.1:5590` only.
- Every request needs `Authorization: Bearer <key>`. A missing or wrong key gets HTTP 401. The key is 32 random bytes (base64url), made the first time you turn the server on and kept in Stylus's `settings.json` (file mode 0600). The compare runs in constant time.
- The `Host` header must be `127.0.0.1:5590` or `localhost:5590`, else HTTP 403. This blocks DNS rebinding from web pages.
- A request with an `Origin` header (browsers send one, MCP clients do not; `null` counts too) gets HTTP 403. A web page open in your browser cannot drive the player.
- Refused requests are logged once a minute per reason. Every tool call is logged as one line in `stylus.log` (tool, arguments cut at 160 characters, ok or the error, time in ms).
- **Reset key** in Settings makes a new key at once. Copied connect texts with the old key stop working; copy them again.
- Any program that runs as your user can read `settings.json` and so the key. The key keeps out web pages and other users, not programs you run yourself.
