# Standalone — idea

**Date:** 2026-10-02
**Status:** done

User, after "This Mac" turned out to launch the Spotify app: "why its opens spotify wtf? we cant uninstall it and just use api?", then "librespot ok! will we have FULL app with it? i want to remove 2gb original spotify".

The public Web API only controls players; it carries no audio. To uninstall the Spotify app, this app must be its own player. librespot (open-source Spotify Connect client, Rust, MIT) is that player.

First guess: embed librespot in the Tauri backend as a Connect device named "The Run", make "This Mac" use it, add macOS media keys / Now Playing. The internal-API extras stay in their own plan (`plans/2026-10-02-internal-api`), the design pass in a later one.
