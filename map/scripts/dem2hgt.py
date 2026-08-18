#!/usr/bin/env python3
"""Copernicus DSM GeoTIFFs -> a single downsampled heightmap grid.

The source is ~400 one-degree COG tiles at 1 arcsec (~30 m), 14 GB in total.
Nothing in a terminal needs 30 m, and a renderer should not be decompressing
GeoTIFF at all, so this bakes the lot down to one flat little-endian grid the
Rust side can read with a single `read_exact`.

    .tmhg layout
    ------------
    magic  "TMHG"            4 bytes
    version u8 = 1           1
    _pad                     3
    west,south,east,north    4 x f64   lon/lat degrees
    width,height             2 x u32   samples
    data                     width*height i16, metres, row 0 = north

    python3 scripts/dem2hgt.py <dem_dir> <out.tmhg> [--arcsec 30] [--bbox W,S,E,N]
"""
import glob
import os
import re
import struct
import sys

import numpy as np
from PIL import Image

Image.MAX_IMAGE_PIXELS = None

NODATA = -32768
TILE_NAME = re.compile(r"_([NS])(\d+)_00_([EW])(\d+)_00_DEM\.tif$")


def tile_origin(path):
    """-> (west_lon, south_lat) of a Copernicus tile, from its filename."""
    m = TILE_NAME.search(path)
    if not m:
        return None
    ns, lat, ew, lon = m.groups()
    lat = int(lat) * (1 if ns == "N" else -1)
    lon = int(lon) * (1 if ew == "E" else -1)
    return lon, lat


def main():
    dem_dir, out_path = sys.argv[1], sys.argv[2]
    arcsec = 30
    bbox = None
    args = sys.argv[3:]
    for i, a in enumerate(args):
        if a == "--arcsec":
            arcsec = int(args[i + 1])
        elif a == "--bbox":
            bbox = [float(v) for v in args[i + 1].split(",")]

    files = sorted(glob.glob(os.path.join(dem_dir, "*.tif")))
    origins = {f: tile_origin(f) for f in files}
    origins = {f: o for f, o in origins.items() if o}
    if not origins:
        sys.exit("no Copernicus tiles found")

    lons = [o[0] for o in origins.values()]
    lats = [o[1] for o in origins.values()]
    if bbox is None:
        bbox = [min(lons), min(lats), max(lons) + 1, max(lats) + 1]
    west, south, east, north = bbox

    # Samples per degree. 30 arcsec is ~900 m, which is plenty of relief for a
    # terminal and keeps the whole country under 30 MB.
    per_deg = 3600 // arcsec
    width = int(round((east - west) * per_deg))
    height = int(round((north - south) * per_deg))
    print(f"grid {width} x {height} @ {arcsec}\" (~{arcsec * 30} m)  "
          f"= {width * height * 2 / 1e6:.0f} MB")

    grid = np.full((height, width), NODATA, dtype=np.int16)
    done = 0

    for path, (tlon, tlat) in sorted(origins.items(), key=lambda kv: kv[1]):
        if tlon + 1 <= west or tlon >= east or tlat + 1 <= south or tlat >= north:
            continue
        try:
            im = Image.open(path)
            # BOX averaging rather than nearest: a single 30 m sample per 900 m
            # cell picks up spikes and makes the relief look like noise.
            small = im.resize((per_deg, per_deg), Image.BOX)
            a = np.asarray(small, dtype=np.float32)
        except Exception as e:
            print(f"  skip {os.path.basename(path)}: {e}")
            continue

        a = np.nan_to_num(a, nan=0.0, posinf=0.0, neginf=0.0)
        a = np.clip(a, -400, 9000).astype(np.int16)

        # Grid row 0 is the north edge, so a tile's north edge maps to the
        # smaller row index.
        x0 = int(round((tlon - west) * per_deg))
        y0 = int(round((north - (tlat + 1)) * per_deg))
        x1, y1 = x0 + per_deg, y0 + per_deg
        sx0, sy0 = max(0, -x0), max(0, -y0)
        x0c, y0c = max(0, x0), max(0, y0)
        x1c, y1c = min(width, x1), min(height, y1)
        if x1c <= x0c or y1c <= y0c:
            continue
        grid[y0c:y1c, x0c:x1c] = a[sy0:sy0 + (y1c - y0c), sx0:sx0 + (x1c - x0c)]

        done += 1
        if done % 25 == 0:
            print(f"  {done} tiles", flush=True)

    filled = int((grid != NODATA).sum())
    print(f"tiles used: {done}   filled: {100 * filled / grid.size:.0f}%")

    with open(out_path, "wb") as f:
        f.write(b"TMHG" + bytes([1, 0, 0, 0]))
        f.write(struct.pack("<4d", west, south, east, north))
        f.write(struct.pack("<2I", width, height))
        f.write(grid.astype("<i2").tobytes())
    print(f"wrote {out_path} ({os.path.getsize(out_path) / 1e6:.0f} MB)")


if __name__ == "__main__":
    main()
