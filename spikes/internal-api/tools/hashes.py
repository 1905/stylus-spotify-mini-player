"""Find current pathfinder persisted-query hashes in the public open.spotify.com web-player JS.
No auth; only static assets. Writes hashes.json next to this file."""
import json, re, sys, urllib.request, pathlib

UA = {"User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130 Safari/537.36"}
WANT = ["searchDesktop", "searchTracks", "searchAlbums", "searchArtists", "searchPlaylists", "queryArtistOverview",
        "queryArtistDiscographyAll", "fetchPlaylist", "fetchPlaylistContents", "fetchLibraryTracks", "getAlbum",
        "libraryV3", "queryAlbumTracks", "fetchEntitiesForRecentlyPlayed", "addToLibrary", "removeFromLibrary",
        "areEntitiesInLibrary", "home", "profileAttributes", "fetchExtractedColors"]

def get(url):
    return urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=30).read().decode("utf-8", "replace")

html = get("https://open.spotify.com/")
scripts = set(re.findall(r'src="(https://open\.spotifycdn\.com/cdn/build/web-player/[^"]+\.js)"', html))
print("entry scripts:", len(scripts))
found = {}
seen = set()
queue = list(scripts)
while queue and len(seen) < 400:
    u = queue.pop()
    if u in seen:
        continue
    seen.add(u)
    try:
        js = get(u)
    except Exception as e:
        print("fail", u, e)
        continue
    # pattern: new X.l("searchDesktop","query","<hash>",null)
    for name, kind, h in re.findall(r'"([A-Za-z0-9]+)","(query|mutation)","([0-9a-f]{64})"', js):
        found[name] = {"kind": kind, "hash": h}
    # lazy chunks: u.u=e=>""+(({id:"name",...})[e]||e)+"."+({id:"hash",...})[e]+".js"
    i = js.find("u.u=e=>")
    if i >= 0:
        expr = js[i:js.find('+".js"', i)]
        maps = re.findall(r'\(\{([^{}]*)\}\)\[e\]', expr)
        if len(maps) >= 2:
            names = dict(re.findall(r'(\d+):"([^"]+)"', maps[0]))
            hm = dict(re.findall(r'(\d+):"([^"]+)"', maps[-1]))
            base = u.rsplit("/", 1)[0] + "/"
            for k, h in hm.items():
                queue.append(f"{base}{names.get(k, k)}.{h}.js")
print("scripts scanned:", len(seen), "operations found:", len(found))
for w in WANT:
    print(f"  {w}: {found.get(w, {}).get('hash', '-')}")
pathlib.Path(__file__).with_name("hashes.json").write_text(json.dumps(found, indent=1, sort_keys=True))
