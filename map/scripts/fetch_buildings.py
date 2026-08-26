#!/usr/bin/env python3
"""Fetch building footprints with heights from Overpass -> .tmap overlay.

The vector basemap has no buildings layer at all -- probe it and you get
landcover, landuse, places, pois, roads, water, waterways and nothing else --
and buildings are what make a street-level view read as three-dimensional.

By default this reads the tour's own stops out of places.txt and fetches a box
around each, which is the only list that cannot drift: a stop added to the tour
gets buildings the next time this runs, and a bbox typed by hand does not.

    python3 map/scripts/fetch_buildings.py                 # every tour stop
    python3 map/scripts/fetch_buildings.py --radius-km 2.5 # wider boxes

A single box can still be asked for, and note the order -- south,west,north,east,
which is Overpass's, *not* the west,south,east,north that GIS tools use. The
example this file carried for a long time was in the other order and would have
been rejected outright as a latitude of 72.

    python3 map/scripts/fetch_buildings.py --bbox 18.96,72.81,19.04,72.87
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


def stops(path):
    """(id, lat, lon) for every tour stop, read out of places.txt."""
    out, cur = [], None
    for line in open(path):
        t = line.strip()
        if t.startswith("place "):
            cur = t.split(None, 1)[1].strip()
        elif t.startswith("at ") and cur:
            lat, lon = (float(v) for v in t[3:].split(","))
            out.append((cur, lat, lon))
            cur = None
    return out


def box(lat, lon, km):
    """south,west,north,east around a point -- Overpass's order, not GIS's."""
    import math
    dlat = km / 111.32
    dlon = km / (111.32 * math.cos(math.radians(lat)))
    return f"{lat - dlat:.5f},{lon - dlon:.5f},{lat + dlat:.5f},{lon + dlon:.5f}"


def convert(els, seen):
    """Overpass ways -> .tmap lines, skipping ids already taken.

    `seen` is shared across boxes because neighbouring stops overlap -- two of
    the Ahmedabad ones are three kilometres apart -- and the same building
    arriving twice would be drawn twice, extruded twice, and counted twice.
    """
    out, kept = [], 0
    for el in els:
        if el.get("id") in seen:
            continue
        geom = el.get("geometry")
        if not geom or len(geom) < 4:
            continue
        seen.add(el.get("id"))
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
    return out, kept


def main():
    args = sys.argv[1:]
    radius, one, dst = 1.5, None, None
    while args:
        a = args.pop(0)
        if a == "--radius-km":
            radius = float(args.pop(0))
        elif a == "--bbox":
            one = args.pop(0)
        elif a == "--out":
            dst = args.pop(0)
        else:
            sys.exit(f"unknown argument {a!r} -- see the docstring at the top")

    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    dst = dst or os.path.join(here, "data", "buildings.tmap")

    if one:
        boxes = [("bbox", one)]
    else:
        places = os.path.join(here, "data", "places.txt")
        boxes = [(pid, box(lat, lon, radius)) for pid, lat, lon in stops(places)]
        if not boxes:
            sys.exit(f"no stops with an `at` line in {places}")

    out, total, seen = ["# termap 1"], 0, set()
    for pid, bbox in boxes:
        print(f">> {pid}: {bbox}")
        lines, kept = convert(fetch(bbox)["elements"], seen)
        out += lines
        total += kept
        print(f"   {kept} buildings")

    os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
    with open(dst, "w") as f:
        f.write("\n".join(out) + "\n")
    print(f">> wrote {dst}: {total} buildings ({os.path.getsize(dst) / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
