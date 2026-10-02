# Snappy — idea

**Date:** 2026-10-02
**Status:** done

User, after the standalone build: "on start we should open same track it was before on the same place. no autoplay. and same playlist where we left of.. also fix volume its lagging a second after i move it. also add nice loaders to everything. currently playlist songs loading long time we need cache + nice loader. then when i press play also lag a few seconds (!) try to fix it faster. and make nice loader so we clearly tell 'wait'".

Measured 2026-10-02: Spotify keeps the session (track + position) server-side, but "The Run" gets a new device id each launch. Every control goes app → Web API → Spotify → dealer → librespot (~1 s+). The biggest playlist (759 tracks) loads in 16 sequential 50-item pages (~0.4 s each; limit 100 now returns 403).
