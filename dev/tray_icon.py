"""Draws the menu-bar template icon: src-tauri/icons/tray-template.png (36x36, black + alpha).

A record with a tonearm, in the icons.js style (24-grid, ~1.75 strokes, round caps). macOS shows
the tray image 18 pt high and recolours a template image for light and dark menu bars, so only the
alpha matters. Stdlib only: python3 dev/tray_icon.py (from the repo root).
"""

import math
import struct
import sys
import zlib
from pathlib import Path

SIZE = 36  # px: 18 pt at 2x
GRID = 24.0  # the icons.js grid
SS = 6  # supersamples per axis

# the glyph on the 24-grid
RING = ((10.5, 13.5), 7.6, 1.9)  # centre, radius, stroke
LABEL = ((10.5, 13.5), 1.7)  # the record's centre, solid
PIVOT = ((19.6, 4.2), 1.75)  # the tonearm's pivot, solid
ARM = [(19.6, 4.2), (19.6, 11.2), (15.4, 15.6)]  # the tonearm, to the needle on the record
ARM_W = 1.75


def seg_dist(p, a, b):
    """Distance from p to the segment a-b."""
    ax, ay = a
    bx, by = b
    px, py = p
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def inside(p):
    """True when the point p (grid units) is ink."""
    (cx, cy), r, w = RING
    if abs(math.hypot(p[0] - cx, p[1] - cy) - r) <= w / 2:
        return True
    for (cx, cy), r in (LABEL, PIVOT):
        if math.hypot(p[0] - cx, p[1] - cy) <= r:
            return True
    return any(seg_dist(p, a, b) <= ARM_W / 2 for a, b in zip(ARM, ARM[1:]))


def render():
    """Alpha (0-255) rows, SIZE x SIZE."""
    scale = GRID / SIZE
    rows = []
    for y in range(SIZE):
        row = []
        for x in range(SIZE):
            hits = 0
            for sy in range(SS):
                for sx in range(SS):
                    p = ((x + (sx + 0.5) / SS) * scale, (y + (sy + 0.5) / SS) * scale)
                    hits += inside(p)
            row.append(round(255 * hits / (SS * SS)))
        rows.append(row)
    return rows


def png(rows):
    """RGBA PNG bytes: black with the given alpha."""
    raw = b"".join(b"\x00" + b"".join(bytes((0, 0, 0, a)) for a in row) for row in rows)

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    head = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", head) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("src-tauri/icons/tray-template.png")
    out.write_bytes(png(render()))
    print(f"wrote {out} ({SIZE}x{SIZE})")


if __name__ == "__main__":
    main()
