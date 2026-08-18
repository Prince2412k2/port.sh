#!/usr/bin/env python3
"""Probe a PMTiles v3 archive: resolve a z/x/y tile and dump its MVT schema.

Used to learn the class vocabulary of a basemap before mapping it onto termap's
layers. Deliberately dependency-free -- both the PMTiles directory format and
the slice of MVT we care about are small enough to parse by hand.
"""
import gzip
import json
import struct
import sys
from collections import Counter


def varint(b, p):
    r = s = 0
    while True:
        x = b[p]
        p += 1
        r |= (x & 0x7F) << s
        if not x & 0x80:
            return r, p
        s += 7


def zxy_to_tileid(z, x, y):
    """PMTiles orders tiles along a Hilbert curve, per zoom level."""
    acc = sum(1 << (2 * t) for t in range(z))
    n, d = 1 << z, 0
    s = n >> 1
    while s > 0:
        rx = 1 if x & s else 0
        ry = 1 if y & s else 0
        d += s * s * ((3 * rx) ^ ry)
        if ry == 0:
            if rx == 1:
                x, y = s - 1 - x, s - 1 - y
            x, y = y, x
        s >>= 1
    return acc + d


def deser_dir(buf):
    p = 0
    n, p = varint(buf, p)
    ids, last = [], 0
    for _ in range(n):
        v, p = varint(buf, p)
        last += v
        ids.append(last)
    runs = []
    for _ in range(n):
        v, p = varint(buf, p)
        runs.append(v)
    lens = []
    for _ in range(n):
        v, p = varint(buf, p)
        lens.append(v)
    offs = []
    for i in range(n):
        v, p = varint(buf, p)
        offs.append(offs[i - 1] + lens[i - 1] if v == 0 and i > 0 else v - 1)
    return list(zip(ids, runs, lens, offs))


class Archive:
    def __init__(self, path):
        self.f = open(path, "rb")
        h = self.f.read(127)
        (self.root_off, self.root_len, self.meta_off, self.meta_len,
         self.leaf_off, self.leaf_len, self.data_off, self.data_len,
         _, _, _) = struct.unpack_from("<11Q", h, 8)
        _, self.icomp, self.tcomp, _, self.minz, self.maxz = struct.unpack_from("<6B", h, 96)

    def _blob(self, off, ln, comp):
        self.f.seek(off)
        b = self.f.read(ln)
        return gzip.decompress(b) if comp == 2 else b

    def metadata(self):
        return json.loads(self._blob(self.meta_off, self.meta_len, self.icomp))

    def tile(self, z, x, y):
        want = zxy_to_tileid(z, x, y)
        off, ln = self.root_off, self.root_len
        for _ in range(4):  # root, then at most a few leaf levels
            entries = deser_dir(self._blob(off, ln, self.icomp))
            hit = None
            for tid, run, tlen, toff in entries:
                if tid <= want and (run == 0 or want < tid + run):
                    hit = (tid, run, tlen, toff)
            if not hit:
                return None
            tid, run, tlen, toff = hit
            if run == 0:  # pointer to a leaf directory
                off, ln = self.leaf_off + toff, tlen
                continue
            return self._blob(self.data_off + toff, tlen, self.tcomp)
        return None


def parse_mvt(buf):
    """-> {layer_name: (feature_count, Counter(class values))}"""
    out = {}
    p = 0
    while p < len(buf):
        key, p = varint(buf, p)
        if key >> 3 == 3 and key & 7 == 2:  # Tile.layers
            ln, p = varint(buf, p)
            name, feats, classes = _layer(buf[p:p + ln])
            out[name] = (feats, classes)
            p += ln
        else:
            p = _skip(buf, p, key & 7)
    return out


def _skip(b, p, wire):
    if wire == 0:
        _, p = varint(b, p)
    elif wire == 2:
        ln, p = varint(b, p)
        p += ln
    elif wire == 5:
        p += 4
    elif wire == 1:
        p += 8
    return p


def _layer(b):
    p, name, keys, vals, feats, tagsets = 0, "?", [], [], 0, []
    while p < len(b):
        key, p = varint(b, p)
        f, wire = key >> 3, key & 7
        if f == 1 and wire == 2:
            ln, p = varint(b, p)
            name = b[p:p + ln].decode("utf8", "replace")
            p += ln
        elif f == 3 and wire == 2:
            ln, p = varint(b, p)
            keys.append(b[p:p + ln].decode("utf8", "replace"))
            p += ln
        elif f == 4 and wire == 2:
            ln, p = varint(b, p)
            vals.append(_value(b[p:p + ln]))
            p += ln
        elif f == 2 and wire == 2:
            ln, p = varint(b, p)
            feats += 1
            tagsets.append(_feature_tags(b[p:p + ln]))
            p += ln
        else:
            p = _skip(b, p, wire)

    classes = Counter()
    for tags in tagsets:
        for i in range(0, len(tags) - 1, 2):
            k = keys[tags[i]] if tags[i] < len(keys) else "?"
            if k == "class":
                v = vals[tags[i + 1]] if tags[i + 1] < len(vals) else "?"
                classes[v] += 1
    return name, feats, classes


def _value(b):
    p = 0
    key, p = varint(b, p)
    f, wire = key >> 3, key & 7
    if f == 1 and wire == 2:
        ln, p = varint(b, p)
        return b[p:p + ln].decode("utf8", "replace")
    if wire == 0:
        v, _ = varint(b, p)
        return v
    return "?"


def _feature_tags(b):
    p, tags = 0, []
    while p < len(b):
        key, p = varint(b, p)
        f, wire = key >> 3, key & 7
        if f == 2 and wire == 2:
            ln, p = varint(b, p)
            end = p + ln
            while p < end:
                v, p = varint(b, p)
                tags.append(v)
        else:
            p = _skip(b, p, wire)
    return tags


def lonlat_to_tile(lon, lat, z):
    import math
    n = 1 << z
    x = int((lon + 180.0) / 360.0 * n)
    lat_r = math.radians(lat)
    y = int((1.0 - math.asinh(math.tan(lat_r)) / math.pi) / 2.0 * n)
    return x, y


if __name__ == "__main__":
    path = sys.argv[1]
    lon, lat, z = float(sys.argv[2]), float(sys.argv[3]), int(sys.argv[4])
    a = Archive(path)
    x, y = lonlat_to_tile(lon, lat, z)
    print(f"z{z}/{x}/{y}  (zoom range z{a.minz}-z{a.maxz})")
    t = a.tile(z, x, y)
    if not t:
        sys.exit("tile not found")
    print(f"tile bytes (decompressed): {len(t):,}\n")
    for lname, (n, classes) in sorted(parse_mvt(t).items()):
        print(f"  {lname:<12} {n:>6} features")
        for cv, cn in classes.most_common(14):
            print(f"       {cv:<22} {cn}")
