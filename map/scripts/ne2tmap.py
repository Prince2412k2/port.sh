#!/usr/bin/env python3
"""Natural Earth admin-1 shapefile -> termap .tmap overlay.

Emits state outlines as a boundary layer plus one label point per state. The
basemap this renders over has neither: planetiler's default profile drops
admin boundaries, and its `places` layer stops at city level.

Shapefile parsing is done by hand -- .shp is a documented binary format and
.dbf is dBase III, which together are far less trouble than a GDAL dependency.

    python3 scripts/ne2tmap.py data/ne/ne_10m_admin_1_states_provinces IN data/states.tmap
"""
import struct
import sys

L_BOUNDARY = 10
L_PLACE = 7

# Shapefile geometry types we handle.
POLYGON, POLYLINE = 5, 3


def read_dbf(path):
    with open(path, "rb") as f:
        nrec, hlen, rlen = struct.unpack("<IHH", f.read(32)[4:12])
        fields = []
        for _ in range((hlen - 33) // 32):
            fd = f.read(32)
            fields.append((fd[:11].split(b"\0")[0].decode("latin1"), fd[16]))
        f.seek(hlen)
        rows = []
        for _ in range(nrec):
            rec = f.read(rlen)
            if len(rec) < rlen or rec[:1] == b"\x1a":
                break
            off, vals = 1, {}
            for name, ln in fields:
                # dBase pads with NUL, not spaces.
                raw = rec[off:off + ln].decode("latin1", "replace")
                vals[name] = raw.replace("\x00", "").strip()
                off += ln
            rows.append(vals)
    return rows


def read_shp(path):
    """-> list of records, each a list of rings (lists of (lon, lat))."""
    with open(path, "rb") as f:
        data = f.read()
    out = []
    p = 100  # skip the 100-byte file header
    while p + 8 <= len(data):
        _, clen = struct.unpack(">II", data[p:p + 8])
        rec = data[p + 8:p + 8 + clen * 2]
        p += 8 + clen * 2
        if len(rec) < 4:
            out.append([])
            continue
        shape = struct.unpack("<I", rec[:4])[0]
        if shape not in (POLYGON, POLYLINE):
            out.append([])
            continue
        nparts, npoints = struct.unpack("<II", rec[36:44])
        parts = list(struct.unpack(f"<{nparts}I", rec[44:44 + 4 * nparts]))
        pbase = 44 + 4 * nparts
        pts = struct.unpack(f"<{2 * npoints}d", rec[pbase:pbase + 16 * npoints])
        rings = []
        for i, start in enumerate(parts):
            end = parts[i + 1] if i + 1 < nparts else npoints
            rings.append([(pts[2 * j], pts[2 * j + 1]) for j in range(start, end)])
        out.append(rings)
    return out


def simplify(pts, eps):
    """Iterative Douglas-Peucker."""
    if len(pts) < 3:
        return pts
    import math

    def perp(p, a, b):
        dx, dy = b[0] - a[0], b[1] - a[1]
        if dx == 0 and dy == 0:
            return math.hypot(p[0] - a[0], p[1] - a[1])
        t = max(0.0, min(1.0, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)))
        return math.hypot(p[0] - (a[0] + t * dx), p[1] - (a[1] + t * dy))

    keep = [False] * len(pts)
    keep[0] = keep[-1] = True
    stack = [(0, len(pts) - 1)]
    while stack:
        lo, hi = stack.pop()
        if hi - lo < 2:
            continue
        worst, wi = 0.0, -1
        for i in range(lo + 1, hi):
            d = perp(pts[i], pts[lo], pts[hi])
            if d > worst:
                worst, wi = d, i
        if worst > eps:
            keep[wi] = True
            stack.append((lo, wi))
            stack.append((wi, hi))
    return [p for p, k in zip(pts, keep) if k]


def main():
    base, iso, dst = sys.argv[1], sys.argv[2], sys.argv[3]
    attrs = read_dbf(base + ".dbf")
    shapes = read_shp(base + ".shp")
    print(f"shapefile: {len(shapes)} shapes, {len(attrs)} records")

    out = ["# termap 1"]
    kept = 0
    for rec, rings in zip(attrs, shapes):
        if rec.get("iso_a2") != iso and rec.get("adm0_a3") not in (iso, "IND"):
            continue
        name = rec.get("name_en") or rec.get("name") or ""
        if not rings:
            continue

        # Outlines are drawn, not filled, so each ring is an open polyline.
        biggest, biggest_area = None, 0.0
        for ring in rings:
            ring = simplify(ring, 0.004)
            if len(ring) < 2:
                continue
            out.append(f"F {L_BOUNDARY} 200 0 {len(ring)} {name}")
            out.append(" ".join(f"{x:.5f} {y:.5f}" for x, y in ring))
            kept += 1
            a = abs(sum(ring[i][0] * ring[(i + 1) % len(ring)][1]
                        - ring[(i + 1) % len(ring)][0] * ring[i][1]
                        for i in range(len(ring))))
            if a > biggest_area:
                biggest_area, biggest = a, ring

        # One label per state, at the centroid of its largest part -- otherwise
        # an archipelago gets its name printed once per island.
        if name and biggest:
            cx = sum(p[0] for p in biggest) / len(biggest)
            cy = sum(p[1] for p in biggest) / len(biggest)
            out.append(f"F {L_PLACE} 215 0 1 {name}")
            out.append(f"{cx:.5f} {cy:.5f}")
            kept += 1

    with open(dst, "w") as f:
        f.write("\n".join(out) + "\n")
    print(f"wrote {dst}: {kept} features")


if __name__ == "__main__":
    main()
