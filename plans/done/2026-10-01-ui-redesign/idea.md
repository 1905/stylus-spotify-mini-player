# Idea — rust-spotify redesign

**Date:** 2026-10-01
**Status:** done

User asked for a portfolio-grade, minimal Spotify player: playlists, search (songs + albums, cover art), a player, and history of what played and what plays next. No profile info, no "by owner" lines. Two redesigns were rejected as "ugly" — the second was a clean but generic three-column Spotify clone with tinted accents.

Why now: the app works functionally (Connect playback verified by the user, "sound works") but the UI fails the brief, and a Codex review found 7 real bugs (search returns 400, stale-request races, stuck device cache, no re-login path).

First-guess scope: throw away the three-column layout. One stage built around a horizontal timeline of covers (played → now → next), with Library and Search as overlays. Use the real Spotify queue and recently-played endpoints instead of guessing. Fix all 7 review findings. Web-first via a browser dev harness; Tauri WebKit check deferred (user: "debug only web part for now").
