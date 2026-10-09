#!/usr/bin/env python3
"""Find raw private values in an anonymized log.

Usage: python3 dev/leak_check.py RAW ANON

Takes candidate private values from RAW (the raw stylus.log) and looks for each one in
ANON (the anonymized file), case-insensitive. Prints counts and the anon line of each hit,
never a value. Exit code 1 when a candidate is found in ANON.
"""

import re
import sys
from collections import Counter
from urllib.parse import parse_qsl, urlsplit

SKIP = {"this mac", "here", "stylus"}

PATTERNS = [
    # (kind, regex, group, minimum length)
    ("quoted", re.compile(r'"((?:[^"\\\n]|\\.)+)"'), 1, 4),
    ("angled", re.compile(r"<([^<>\n]+)>"), 1, 4),
    ("spotify_id", re.compile(r"spotify:[A-Za-z_-]+:([^\s\"'<>()\[\],]+)"), 1, 1),
    ("uuid", re.compile(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b"), 0, 1),
    ("email", re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}"), 0, 1),
    ("ipv4", re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b"), 0, 1),
    ("user", re.compile(r"/Users/([^/\s\"'<>]+)"), 1, 1),
    ("user", re.compile(r"Authenticated as '([^'\n]+)'"), 1, 1),
    ("user", re.compile(r"hm://collection/collection/([^/\s\"'<>]+)"), 1, 1),
]
TOKEN = re.compile(r"[A-Za-z0-9+/=_-]{16,}")
URL = re.compile(r"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s\"'<>)\]]+")


def candidates(raw):
    """Gives a dict: lower-case value -> kind (first kind that found it)."""
    found = {}

    def add(kind, value, minimum):
        value = value.strip()
        if len(value) >= minimum and value.lower() not in SKIP:
            found.setdefault(value.lower(), kind)

    for kind, rx, group, minimum in PATTERNS:
        for m in rx.finditer(raw):
            add(kind, m.group(group), minimum)
    for m in TOKEN.finditer(raw):
        if any(c.isdigit() for c in m.group(0)):
            add("token", m.group(0), 16)
    for m in URL.finditer(raw):
        try:
            parts = urlsplit(m.group(0))
        except ValueError:
            continue
        for part in parts.path.split("/"):
            add("url_part", part, 6)
        for _, value in parse_qsl(parts.query, keep_blank_values=True):
            add("url_part", value, 6)
    return found


def main(argv):
    if len(argv) != 3:
        print("usage: python3 dev/leak_check.py RAW ANON", file=sys.stderr)
        return 2
    with open(argv[1], encoding="utf-8", errors="replace") as f:
        raw = f.read()
    with open(argv[2], encoding="utf-8", errors="replace") as f:
        anon_lines = [line.lower() for line in f.read().splitlines()]

    found = candidates(raw)
    kinds = Counter(found.values())
    summary = ", ".join(f"{k}: {n}" for k, n in sorted(kinds.items()))
    print(f"candidates: {len(found)} ({summary})")

    leaks = 0
    for value, kind in found.items():
        for number, line in enumerate(anon_lines, start=1):
            if value in line:
                print(f"leak: {kind} at anon line {number}")
                leaks += 1
                break
    print(f"leaks: {leaks}")
    return 1 if leaks else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
