#!/usr/bin/env python3
"""Fetch an OSM extract via Overpass and convert it to termap's .tmap format.

Overpass will happily 504 on a single query that asks for every road in a city,
so the work is split into chunks, each cached under data/raw/. Re-running skips
whatever already succeeded, which means a timeout costs you one chunk rather
than the whole fetch. Mirrors are tried in turn, with backoff on the rate limit.

    python3 scripts/fetch_osm.py
    BBOX="18.88,72.77,19.28,73.02" python3 scripts/fetch_osm.py
    python3 scripts/fetch_osm.py --force        # ignore the cache
"""
import json
import os
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

MIRRORS = [
    "https://overpass-api.de/api/interpreter",
    "https://overpass.kumi.systems/api/interpreter",
    "https://overpass.private.coffee/api/interpreter",
    "https://maps.mail.ru/osm/tools/overpass/api/interpreter",
]

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
RAW = os.path.join(ROOT, "data", "raw")
TMAP = os.path.join(ROOT, "data", "mumbai.tmap")

BBOX = os.environ.get("BBOX", "18.88,72.77,19.28,73.02")  # S,W,N,E
TIMEOUT = 180


def split_bbox(bbox, n):
    """Cut S,W,N,E into an n x n grid of bbox strings."""
    s, w, north, e = (float(v) for v in bbox.split(","))
    dy = (north - s) / n
    dx = (e - w) / n
    out = []
    for i in range(n):
        for j in range(n):
            out.append(f"{s + i * dy:.5f},{w + j * dx:.5f},"
                       f"{s + (i + 1) * dy:.5f},{w + (j + 1) * dx:.5f}")
    return out


def chunks():
    """-> list of (name, query body). Bulky layers get spatially tiled."""
    out = [
        ("major", f'way["highway"~"^(motorway|motorway_link|trunk|trunk_link|'
                  f'primary|primary_link)$"]({BBOX});'),
        ("secondary", f'way["highway"~"^(secondary|secondary_link|tertiary|'
                      f'tertiary_link)$"]({BBOX});'),
        ("rail", f'way["railway"~"^(rail|light_rail|subway)$"]({BBOX});'),
        ("coast", f'way["natural"="coastline"]({BBOX});'),
        ("water", f'way["natural"="water"]({BBOX});'
                  f'way["waterway"="riverbank"]({BBOX});'),
        ("landuse", f'way["leisure"~"^(park|garden|nature_reserve)$"]({BBOX});'),
        ("points", f'node["place"~"^(city|town|suburb|neighbourhood)$"]({BBOX});'
                   f'node["railway"="station"]({BBOX});'
                   f'node["tourism"~"^(attraction|museum)$"]({BBOX});'
                   f'node["historic"="monument"]({BBOX});'
                   f'node["amenity"~"^(university|hospital)$"]({BBOX});'),
    ]
    # Residential roads are the heavy one -- one tile per request.
    for i, tile in enumerate(split_bbox(BBOX, 3)):
        out.append((
            f"minor{i}",
            f'way["highway"~"^(residential|unclassified|living_street)$"]({tile});',
        ))
    return out


def fetch(name, body, force=False):
    dest = os.path.join(RAW, f"{name}.json")
    if os.path.exists(dest) and os.path.getsize(dest) > 0 and not force:
        print(f"  {name:<10} cached ({os.path.getsize(dest) // 1024} KB)")
        return dest

    query = f"[out:json][timeout:{TIMEOUT}];\n({body}\n);\nout geom qt;"
    data = urllib.parse.urlencode({"data": query}).encode()
    ctx = ssl.create_default_context()

    for attempt in range(1, 4):
        for url in MIRRORS:
            host = url.split("/")[2]
            try:
                req = urllib.request.Request(
                    url, data=data,
                    headers={"User-Agent": "termap/0.1 (OSM extract for a TUI map)"},
                )
                with urllib.request.urlopen(req, timeout=TIMEOUT + 60, context=ctx) as r:
                    payload = r.read()
                # Overpass reports some failures as a 200 with an HTML body.
                if not payload.lstrip().startswith(b"{"):
                    raise ValueError("non-JSON response")
                with open(dest, "wb") as fh:
                    fh.write(payload)
                print(f"  {name:<10} ok  {len(payload) // 1024:>6} KB  via {host}")
                return dest
            except (urllib.error.HTTPError, urllib.error.URLError,
                    ValueError, TimeoutError, OSError) as e:
                code = getattr(e, "code", "")
                print(f"  {name:<10} .. {host} failed ({code or type(e).__name__})")
        wait = 5 * attempt
        print(f"  {name:<10} all mirrors failed, retrying in {wait}s")
        time.sleep(wait)

    print(f"  {name:<10} GIVING UP -- rerun to retry just this chunk")
    return None


def main():
    force = "--force" in sys.argv
    os.makedirs(RAW, exist_ok=True)
    print(f">> bbox {BBOX}")

    paths, missing = [], []
    for name, body in chunks():
        p = fetch(name, body, force)
        if p:
            paths.append(p)
        else:
            missing.append(name)
        time.sleep(1)  # be polite between requests

    if not paths:
        sys.exit("no chunks fetched; nothing to convert")
    if missing:
        print(f">> WARNING: {len(missing)} chunk(s) missing: {', '.join(missing)}")
        print(">> converting what we have; rerun to fill the gaps")

    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import osm2tmap
    bbox = tuple(float(v) for v in BBOX.split(','))
    osm2tmap.convert(paths, TMAP, bbox)
    print(f">> wrote {TMAP} ({os.path.getsize(TMAP) // 1024} KB)")


if __name__ == "__main__":
    main()
