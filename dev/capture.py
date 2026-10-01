#!/usr/bin/env python3
"""Build dev/fixture.json from the live Spotify Web API (GET only, read-only).

Reads the access token from the app's tokens.json. Never refreshes or writes it.
Run from anywhere: python3 dev/capture.py
"""
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone

API = "https://api.spotify.com/v1"
TOKENS = os.path.expanduser("~/Library/Application Support/rust-spotify/tokens.json")
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixture.json")
SEARCH_QUERY = "the xx"


class Forbidden(Exception):
    pass


def load_token():
    with open(TOKENS) as f:
        tok = json.load(f).get("access_token")
    if not tok:
        sys.exit("capture: no access_token in tokens.json")
    return tok


def get(token, path):
    url = path if path.startswith("http") else API + path
    req = urllib.request.Request(url, headers={"Authorization": "Bearer " + token}, method="GET")
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            body = r.read()
            return json.loads(body) if body else None
    except urllib.error.HTTPError as e:
        if e.code == 401:
            print("capture: 401 from Spotify -- the access token expired. "
                  "Open the app once (or re-login) to refresh tokens.json, then re-run.")
            sys.exit(1)
        if e.code == 403:
            raise Forbidden(path)
        sys.exit(f"capture: HTTP {e.code} on GET {path}: {e.read()[:300]!r}")


def first_image(images):
    return images[0]["url"] if images else None


def join_artists(artists):
    return ", ".join(a.get("name", "") for a in (artists or []))


def track(t, album=None):
    """Spotify track object -> fixture Track. `album` overrides for album-track endpoints."""
    alb = album or t.get("album") or {}
    return {
        "id": t.get("id"),
        "uri": t.get("uri"),
        "name": t.get("name", ""),
        "artists": join_artists(t.get("artists")),
        "album": alb.get("name", ""),
        "cover": first_image(alb.get("images")),
        "duration_ms": t.get("duration_ms", 0),
    }


def row_track(row):
    return row.get("item") or row.get("track")


def main():
    token = load_token()
    fx = {}

    pl = get(token, "/me/playlists?limit=50")
    playlists = []
    for p in pl.get("items", []):
        if not p:
            continue
        total = (p.get("items") or {}).get("total")
        if total is None:
            total = (p.get("tracks") or {}).get("total", 0)
        playlists.append({"id": p["id"], "name": p.get("name", ""),
                          "images": p.get("images") or [], "tracks": {"total": total}})
    fx["playlists"] = playlists

    fx["playlistTracks"] = {}
    for p in playlists[:2]:
        rows = get(token, f"/playlists/{p['id']}/items?limit=50").get("items", [])
        fx["playlistTracks"][p["id"]] = [track(t) for t in map(row_track, rows)
                                         if t and t.get("type", "track") == "track"]

    q = get(token, "/me/player/queue") or {}
    fx["queue"] = [track(t) for t in q.get("queue", []) if t and t.get("type", "track") == "track"]
    cur = q.get("currently_playing")
    fx["now"] = track(cur) if cur else None

    try:
        rp = get(token, "/me/player/recently-played?limit=30")
        fx["recent"] = [{"track": track(row_track(r)), "played_at": r["played_at"]}
                        for r in rp.get("items", []) if row_track(r)]
        recent_note = "real (recently-played 200)"
    except Forbidden:
        base = datetime.now(timezone.utc)
        fx["recent"] = [{"track": t, "played_at": (base - timedelta(minutes=4 * (i + 1)))
                         .strftime("%Y-%m-%dT%H:%M:%SZ")}
                        for i, t in enumerate(fx["queue"][:8])]
        recent_note = "synthesized (scope missing)"

    s = get(token, "/search?" + urllib.parse.urlencode(
        {"q": SEARCH_QUERY, "type": "track,album", "limit": 10}))
    albums = []
    for a in (s.get("albums") or {}).get("items", []):
        if not a:
            continue
        albums.append({"id": a["id"], "uri": a["uri"], "name": a.get("name", ""),
                       "artists": join_artists(a.get("artists")),
                       "cover": first_image(a.get("images")),
                       "year": (a.get("release_date") or "")[:4],
                       "total_tracks": a.get("total_tracks", 0)})
    fx["search"] = {"query": SEARCH_QUERY,
                    "tracks": [track(t) for t in (s.get("tracks") or {}).get("items", []) if t],
                    "albums": albums}

    fx["albumTracks"] = {}
    if albums:
        a = get(token, f"/albums/{albums[0]['id']}")
        alb = {"name": a.get("name", ""), "images": a.get("images") or []}
        items = list(a["tracks"]["items"])
        nxt = a["tracks"].get("next")
        while nxt:
            page = get(token, nxt)
            items += page.get("items", [])
            nxt = page.get("next")
        fx["albumTracks"][albums[0]["id"]] = [track(t, alb) for t in items if t]

    with open(OUT, "w") as f:
        json.dump(fx, f, indent=1, ensure_ascii=False)

    print(f"playlists: {len(playlists)}")
    for pid, rows in fx["playlistTracks"].items():
        print(f"playlistTracks[{pid}]: {len(rows)}")
    print(f"queue: {len(fx['queue'])}")
    print(f"now: {fx['now']['name'] if fx['now'] else None}")
    print(f"recent: {len(fx['recent'])} {recent_note}")
    print(f"search tracks: {len(fx['search']['tracks'])}, albums: {len(albums)}")
    for aid, rows in fx["albumTracks"].items():
        print(f"albumTracks[{aid}]: {len(rows)}")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
