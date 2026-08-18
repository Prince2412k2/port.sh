#!/usr/bin/env python3
"""Fetch building footprints with heights from Overpass -> .tmap overlay.

The vector basemap has no buildings layer at all, and buildings are what make a
street-level view read as three-dimensional. Scoped to a small bbox on purpose:
footprints are dense, and they are only worth drawing above about z15.

    python3 scripts/fetch_buildings.py 72.81,18.96,72.87,19.04 data/buildings.tmap
"""
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

MIRRORS = [
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
]

L_BUILDING = 11

# Metres per storey where only building:levels is tagged. Indian residential
# floors run a little lower than the 3 m usually assumed.
STOREY_M = 3.1
DEFAULT_M = 9.0


def height_of(tags):
    """Best available height in metres, or a default."""
    for key in ("height", "building:height"):
        v = tags.get(key)
        if v:
            try:
                return max(2.0, float(str(v).split()[0].replace("m", "")))
            except ValueError:
                pass
    for key in ("building:levels", "levels"):
        v = tags.get(key)
        if v:
            try:
                return max(2.0, float(str(v).split(";")[0]) * STOREY_M)
            except ValueError:
                pass
    return DEFAULT_M


def fetch(bbox):
    query = (
        f"[out:json][timeout:240];\n"
        f'(way["building"]({bbox});\n'
        f' way["building:part"]({bbox});\n);\n'
        f"out geom qt;"
    )
    data = urllib.parse.urlencode({"data": query}).encode()
    for attempt in range(3):
        for url in MIRRORS:
            host = url.split("/")[2]
            try:
                req = urllib.request.Request(
                    url, data=data,
                    headers={"User-Agent": "termap/0.1 (building extrusion)"},
                )
                with urllib.request.urlopen(req, timeout=300) as r:
                    payload = r.read()
                if not payload.lstrip().startswith(b"{"):
                    raise ValueError("non-JSON response")
                print(f"  ok {len(payload) // 1024} KB via {host}")
                return json.loads(payload)
            except Exception as e:
                print(f"  {host} failed: {getattr(e, 'code', type(e).__name__)}")
        time.sleep(5 * (attempt + 1))
    sys.exit("all mirrors failed")


def main():
    bbox, dst = sys.argv[1], sys.argv[2]
    print(f">> buildings for {bbox}")
    els = fetch(bbox)["elements"]

    out = ["# termap 1"]
    kept = 0
    for el in els:
        geom = el.get("geometry")
        if not geom or len(geom) < 4:
            continue
        tags = el.get("tags") or {}
        pts = [(g["lon"], g["lat"]) for g in geom if g]
        if pts[0] != pts[-1]:
            pts.append(pts[0])
        # Footprints are already small on screen; simplifying them costs corners
        # and gains nothing.
        h = height_of(tags)
        name = (tags.get("name:en") or tags.get("name") or "").replace("\n", " ").strip()
        # Rank carries the height in metres, so the renderer needs no extra
        # field to extrude from.
        out.append(f"F {L_BUILDING} {int(min(h, 400))} 1 {len(pts)} {name}")
        out.append(" ".join(f"{x:.6f} {y:.6f}" for x, y in pts))
        kept += 1

    os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
    with open(dst, "w") as f:
        f.write("\n".join(out) + "\n")
    print(f">> wrote {dst}: {kept} buildings ({os.path.getsize(dst) / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
