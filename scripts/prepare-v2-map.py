#!/usr/bin/env python3
"""Prepare the migration adapter's real map inputs, without copying the archive.

Requires python3-numpy and python3-pil. Downloads are cached and their digests
recorded. The TMHG adapter is transitional; it is not the PRD tiled terrain ABI.
The supplied RDR imagery must never be interpreted as encoded elevations.
"""
import argparse
import concurrent.futures
import hashlib
import io
import json
import math
from pathlib import Path
import struct
import sys
import urllib.request
import zipfile
import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "map/scripts"))
import ne2tmap


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("assets", type=Path)
    parser.add_argument("--output", type=Path, default=ROOT / "map-data")
    parser.add_argument("--zoom", type=int, default=7, choices=range(5, 8))
    args = parser.parse_args()
    output = args.output
    output.mkdir(parents=True, exist_ok=True)
    cache = output / "source-cache"
    cache.mkdir(exist_ok=True)
    manifest = json.loads((args.assets / "maps/current.json").read_text())
    vector = args.assets / manifest["products"]["coreVector"].lstrip("/")
    link = output / "vector.pmtiles"
    if not link.exists():
        link.symlink_to(vector.resolve())
    elif link.resolve() != vector.resolve():
        raise SystemExit(f"Refusing to replace {link}")
    buildings=ROOT/'map/data/buildings.tmap'
    if buildings.is_file() and not (output/'buildings.tmap').exists():
        (output/'buildings.tmap').symlink_to(buildings.resolve())

    sources = {}
    def download(url, name, limit):
        path = cache / name
        if not path.exists():
            with urllib.request.urlopen(url, timeout=45) as response:
                data = response.read(limit + 1)
            if len(data) > limit:
                raise ValueError(f"download too large: {url}")
            temporary = path.with_suffix(".part")
            temporary.write_bytes(data)
            temporary.replace(path)
        data = path.read_bytes()
        sources[name] = {"url": url, "sha256": hashlib.sha256(data).hexdigest()}
        return data

    # Natural Earth is also the V1 administrative-boundary source.
    zipbytes = download("https://naturalearth.s3.amazonaws.com/10m_cultural/ne_10m_admin_1_states_provinces.zip", "admin1.zip", 32 * 1024 * 1024)
    with zipfile.ZipFile(io.BytesIO(zipbytes)) as archive:
        for name in ["ne_10m_admin_1_states_provinces.shp", "ne_10m_admin_1_states_provinces.dbf"]:
            (cache / name).write_bytes(archive.read(name))
    old_args = sys.argv
    sys.argv = ["ne2tmap", str(cache / "ne_10m_admin_1_states_provinces"), "IN", str(output / "states.tmap")]
    ne2tmap.main()
    sys.argv = old_args

    west, south, east, north = manifest["region"]["bounds"]
    z, n = args.zoom, 1 << args.zoom
    def merc(lat):
        return (1 - math.asinh(math.tan(math.radians(lat))) / math.pi) / 2
    x0, x1 = math.floor((west + 180) / 360 * n), math.floor((east + 180) / 360 * n)
    y0, y1 = math.floor(merc(north) * n), math.floor(merc(south) * n)
    mosaic = np.zeros(((y1-y0+1)*256, (x1-x0+1)*256), dtype=np.float32)
    def tile(key):
        x, y = key
        url = f"https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png"
        data = download(url, f"dem-{z}-{x}-{y}.png", 1024 * 1024)
        rgb = np.asarray(Image.open(io.BytesIO(data)).convert("RGB"), dtype=np.float32)
        if rgb.shape != (256,256,3):
            raise ValueError("unsupported elevation tile dimensions")
        return x, y, rgb[:,:,0]*256 + rgb[:,:,1] + rgb[:,:,2]/256 - 32768
    keys = [(x,y) for y in range(y0,y1+1) for x in range(x0,x1+1)]
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
        for count, (x,y,heights) in enumerate(executor.map(tile, keys), 1):
            mosaic[(y-y0)*256:(y-y0+1)*256, (x-x0)*256:(x-x0+1)*256] = heights
            if count % 20 == 0: print(f"elevation {count}/{len(keys)}", flush=True)
    # TMHG is a regular lon/lat grid, not Web Mercator. Resample rows correctly.
    width, height = mosaic.shape[1], mosaic.shape[0]
    lon = np.linspace(west,east,width)
    lat = np.linspace(north,south,height)
    xs = np.clip(((lon+180)/360*n-x0)*256,0,width-1)
    ys = np.clip(np.array([(merc(float(v))*n-y0)*256 for v in lat]),0,height-1)
    xa = np.floor(xs).astype(int); xb=np.minimum(xa+1,width-1); tx=xs-xa
    result = np.empty((height,width),dtype="<i2")
    for row, y in enumerate(ys):
        ya=int(y); yb=min(ya+1,height-1); ty=y-ya
        top=mosaic[ya,xa]*(1-tx)+mosaic[ya,xb]*tx
        bottom=mosaic[yb,xa]*(1-tx)+mosaic[yb,xb]*tx
        result[row]=np.clip(np.rint(top*(1-ty)+bottom*ty),-32767,32767).astype("<i2")
    header=b"TMHG\x01\x00\x00\x00"+struct.pack("<4d2I",west,south,east,north,width,height)
    (output / "terrain.tmhg").write_bytes(header+result.tobytes())
    (output / "sources.json").write_text(json.dumps({"vectorRelease":manifest["releaseId"],"elevationZoom":z,"sources":sources},indent=2))
    print(f"Prepared {output}: vector symlink, boundaries, {width}x{height} real elevation grid")


if __name__ == "__main__":
    main()
