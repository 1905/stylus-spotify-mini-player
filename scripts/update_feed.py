#!/usr/bin/env python3
"""Write latest.json, Tauri's static updater feed, for one Stylus release (Apple Silicon only).

    python3 scripts/update_feed.py 0.4.0 --notes "Stylus 0.4.0" \\
        --sig src-tauri/target/release/bundle/macos/Stylus.app.tar.gz.sig \\
        --url https://github.com/1905/stylus-spotify-mini-player/releases/download/v0.4.0/Stylus.app.tar.gz \\
        --out /tmp/latest.json

The installed app reads it from releases/latest/download/latest.json (tauri.conf.json plugins.updater).
Tests: python3 -m unittest discover -s scripts
"""

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlparse

TARGET = "darwin-aarch64"


def feed(version, notes, signature, url, now=None):
    """The feed as a dict. signature = the .sig file's text (minisign, base64)."""
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError(f"not a version (X.Y.Z): {version!r}")
    signature = signature.strip()
    if not signature:
        raise ValueError("the signature is empty")
    u = urlparse(url)
    if u.scheme not in ("https", "http") or not u.netloc or not u.path.endswith(".app.tar.gz"):
        raise ValueError(f"not an http(s) url of a .app.tar.gz: {url!r}")
    now = now or datetime.now(timezone.utc)
    return {
        "version": version,
        "notes": notes.strip(),
        "pub_date": now.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": {TARGET: {"signature": signature, "url": url}},
    }


def render(f):
    """The feed as the latest.json text."""
    return json.dumps(f, indent=2) + "\n"


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("version", help="X.Y.Z")
    notes = p.add_mutually_exclusive_group(required=True)
    notes.add_argument("--notes", help="the update notes, as text")
    notes.add_argument("--notes-file", help="a file with the update notes")
    p.add_argument("--sig", required=True, help="the Stylus.app.tar.gz.sig made by the build")
    p.add_argument("--url", required=True, help="where the Stylus.app.tar.gz downloads from")
    p.add_argument("--out", help="the file to write (default: stdout)")
    a = p.parse_args(argv)
    text = a.notes if a.notes is not None else Path(a.notes_file).read_text()
    try:
        out = render(feed(a.version, text, Path(a.sig).read_text(), a.url))
    except ValueError as e:
        sys.exit(f"✗ {e}")
    if a.out:
        Path(a.out).write_text(out)
    else:
        sys.stdout.write(out)


if __name__ == "__main__":
    main()
