#!/usr/bin/env python3
"""Convert an Overpass JSON dump into termap's flat .tmap format.

.tmap is deliberately dumb so the Rust side needs no JSON dependency:

    # termap 1
    F <layer> <rank> <closed> <npts> <name>
    <lon> <lat> <lon> <lat> ...

One F header line, one coordinate line. Layers are the numeric ids in LAYER.
"""
import json
import math
import os
import sys

LAYER = {
    "land": 9,
    "landuse": 0,
    "water": 1,
    "coast": 2,
    "rail": 3,
    "road_minor": 4,
    "road_medium": 5,
    "road_major": 6,
    "place": 7,
    "landmark": 8,
}

# Tiers chosen from the actual tag histogram of a Mumbai extract, not from the
# OSM wiki's ordering: primary alone is 16% of ways there, so putting it in the
# top tier collapses the whole network into one bright undifferentiated mesh.
# Rank varies inside a tier so primary still outweighs secondary.
ROAD_TIER = {
    "motorway": ("road_major", 220),
    "motorway_link": ("road_major", 205),
    "trunk": ("road_major", 215),
    "trunk_link": ("road_major", 200),
    "primary": ("road_medium", 190),
    "primary_link": ("road_medium", 180),
    "secondary": ("road_medium", 165),
    "secondary_link": ("road_medium", 155),
    "tertiary": ("road_minor", 130),
    "tertiary_link": ("road_minor", 120),
    "residential": ("road_minor", 95),
    "unclassified": ("road_minor", 85),
    "living_street": ("road_minor", 80),
}

PLACE_RANK = {"city": 240, "town": 195, "suburb": 168, "neighbourhood": 118}


def classify(tags):
    """-> (layer_name, rank) or None to drop the feature."""
    hw = tags.get("highway")
    if hw in ROAD_TIER:
        return ROAD_TIER[hw]

    if tags.get("railway") in ("rail", "light_rail", "subway"):
        return "rail", 120
    if tags.get("natural") == "coastline":
        return "coast", 180
    if tags.get("natural") == "water" or tags.get("waterway") == "riverbank":
        return "water", 60
    if tags.get("leisure") in ("park", "garden", "nature_reserve"):
        return "landuse", 40

    place = tags.get("place")
    if place in PLACE_RANK:
        bonus = 20 if ("wikidata" in tags or "wikipedia" in tags) else 0
        return "place", PLACE_RANK[place] + bonus

    # Landmark rank does real filtering work. Ranks come from what the tags
    # actually contain in a city extract, not from what they sound like:
    # `historic=monument` is mostly local statuary and `tourism=attraction` is a
    # junk drawer, while `railway=station` is both reliable and what you
    # actually navigate by. A wikidata/wikipedia link is the best available
    # proxy for "someone considers this notable", so it earns a boost.
    notable = 45 if ("wikidata" in tags or "wikipedia" in tags) else 0
    base = {
        ("railway", "station"): 170,
        ("tourism", "museum"): 160,
        ("amenity", "university"): 158,
        ("historic", "fort"): 150,
        ("historic", "ruins"): 145,
        ("historic", "monument"): 128,
        ("tourism", "attraction"): 128,
        ("amenity", "hospital"): 95,
    }
    for (k, v), r in base.items():
        if tags.get(k) == v:
            return "landmark", min(r + notable, 250)

    return None


def perp_dist(p, a, b):
    (px, py), (ax, ay), (bx, by) = p, a, b
    dx, dy = bx - ax, by - ay
    if dx == 0 and dy == 0:
        return math.hypot(px - ax, py - ay)
    t = ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)
    t = max(0.0, min(1.0, t))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def simplify(pts, eps):
    """Iterative Douglas-Peucker; recursion blows the stack on long coastlines."""
    if len(pts) < 3:
        return pts
    keep = [False] * len(pts)
    keep[0] = keep[-1] = True
    stack = [(0, len(pts) - 1)]
    while stack:
        lo, hi = stack.pop()
        if hi - lo < 2:
            continue
        worst, worst_i = 0.0, -1
        for i in range(lo + 1, hi):
            d = perp_dist(pts[i], pts[lo], pts[hi])
            if d > worst:
                worst, worst_i = d, i
        if worst > eps:
            keep[worst_i] = True
            stack.append((lo, worst_i))
            stack.append((worst_i, hi))
    return [p for p, k in zip(pts, keep) if k]


# Coarser layers tolerate far more simplification than roads do.
EPS = {
    "landuse": 0.00030,
    "water": 0.00025,
    "coast": 0.00008,
    "rail": 0.00012,
    "road_minor": 0.00020,
    "road_medium": 0.00012,
    "road_major": 0.00008,
}


def signed_area(ring):
    """Positive = counter-clockwise in a lon/lat frame with north up."""
    a = 0.0
    for i in range(len(ring)):
        x0, y0 = ring[i]
        x1, y1 = ring[(i + 1) % len(ring)]
        a += x0 * y1 - x1 * y0
    return a / 2.0


def stitch(ways, tol=1e-7):
    """Join coastline ways end-to-end into the longest chains they support."""
    chains = [list(w) for w in ways]
    key = lambda p: (round(p[0] / tol), round(p[1] / tol))

    merged = True
    while merged:
        merged = False
        heads = {}
        for i, ch in enumerate(chains):
            if ch is None:
                continue
            heads.setdefault(key(ch[0]), []).append(i)

        for i, ch in enumerate(chains):
            if ch is None or key(ch[0]) == key(ch[-1]):
                continue
            for j in heads.get(key(ch[-1]), []):
                if j != i and chains[j] is not None:
                    chains[i] = ch + chains[j][1:]
                    chains[j] = None
                    merged = True
                    break
    return [c for c in chains if c]


def perimeter_t(p, bbox):
    """Position of a boundary point along the bbox perimeter, walking CCW."""
    s, w, n, e = bbox
    width, height = e - w, n - s
    lon, lat = p
    # Snap to whichever edge is nearest; data gaps leave endpoints slightly off.
    d = {"s": abs(lat - s), "n": abs(lat - n), "w": abs(lon - w), "e": abs(lon - e)}
    edge = min(d, key=d.get)
    if edge == "s":
        return max(0.0, min(width, lon - w))
    if edge == "e":
        return width + max(0.0, min(height, lat - s))
    if edge == "n":
        return width + height + max(0.0, min(width, e - lon))
    return 2 * width + height + max(0.0, min(height, n - lat))


def close_on_bbox(chain, bbox):
    """Close an open coastline chain by walking the bbox edge back to its start.

    OSM draws coastline with land on the left, so continuing counter-clockwise
    around the bbox keeps land on the left too, and the ring encloses land.
    """
    s, w, n, e = bbox
    width, height = e - w, n - s
    perim = 2 * (width + height)
    corners = [(0.0, (w, s)), (width, (e, s)),
               (width + height, (e, n)), (2 * width + height, (w, n))]

    t_end = perimeter_t(chain[-1], bbox)
    t_start = perimeter_t(chain[0], bbox)

    ring = list(chain)
    t = t_end
    guard = 0
    while guard < 8:
        guard += 1
        # Next corner strictly ahead of t, wrapping.
        ahead = [(ct - t) % perim for ct, _ in corners]
        step = min(x for x in ahead if x > 1e-12)
        if (t_start - t) % perim <= step:
            break
        t = (t + step) % perim
        ring.append(corners[[round((ct - t) % perim, 9) for ct, _ in corners].index(0.0)][1])
    return ring


def build_land(coast_ways, bbox):
    """Coastline ways -> closed land rings, for use as an ocean mask."""
    rings = []
    for chain in stitch(coast_ways):
        if len(chain) < 4:
            continue
        if chain[0] != chain[-1]:
            chain = close_on_bbox(chain, bbox)
        ring = chain[:-1] if chain[0] == chain[-1] else chain
        if len(ring) < 3:
            continue
        # CCW encloses land; CW is a water body sitting inside land, and inland
        # water already comes from its own layer.
        if signed_area(ring) > 0:
            rings.append(ring)

    s, w, n, e = bbox
    box = abs((e - w) * (n - s))
    covered = sum(abs(signed_area(r)) for r in rings) / box if box else 0
    # A plausible extract is part land, part sea. Anything outside that band
    # means the stitching went wrong, and a wrong mask is worse than none.
    if not 0.02 < covered < 0.97:
        print(f">> land mask rejected (covers {covered:.0%} of bbox); skipping ocean wash")
        return []
    print(f">> land mask: {len(rings)} ring(s), {covered:.0%} of bbox")
    return rings


def convert(sources, dst, bbox=None):
    """Merge one or more Overpass JSON dumps into a single .tmap."""
    elements = []
    for src in sources:
        with open(src) as fh:
            elements.extend(json.load(fh)["elements"])

    out = ["# termap 1"]
    kept = dropped = 0
    seen = set()
    near = set()
    coast_ways = []

    for el in elements:
        # Chunks are fetched by tag and by tile, so the same way can arrive
        # twice; drawing it twice would just brighten it.
        key = (el["type"], el.get("id"))
        if key[1] is not None:
            if key in seen:
                dropped += 1
                continue
            seen.add(key)

        tags = el.get("tags") or {}
        hit = classify(tags)
        if not hit:
            dropped += 1
            continue
        layer, rank = hit
        name = (tags.get("name:en") or tags.get("name") or "").replace("\n", " ").strip()

        if el["type"] == "node":
            pts = [(el["lon"], el["lat"])]
            closed = 0
        else:
            geom = el.get("geometry")
            if not geom:
                dropped += 1
                continue
            pts = [(g["lon"], g["lat"]) for g in geom if g]
            if len(pts) < 2:
                dropped += 1
                continue
            closed = 1 if pts[0] == pts[-1] else 0
            if layer == "coast":
                coast_ways.append(pts)
            pts = simplify(pts, EPS.get(layer, 0.0002))

        # Unnamed points are noise -- they cannot be labelled and draw as specks.
        if layer in ("place", "landmark"):
            if not name:
                dropped += 1
                continue
            # OSM often carries the same feature as several nodes with names
            # differing only in case ("Dalvi nursing home" / "Dalvi Nursing
            # Home"). Collapse those within ~200 m of each other.
            cell = (round(pts[0][0] / 0.002), round(pts[0][1] / 0.002))
            tag = (name.casefold(), cell)
            if tag in near:
                dropped += 1
                continue
            near.add(tag)

        out.append(f"F {LAYER[layer]} {rank} {closed} {len(pts)} {name}")
        out.append(" ".join(f"{x:.6f} {y:.6f}" for x, y in pts))
        kept += 1

    if bbox and coast_ways:
        for ring in build_land(coast_ways, bbox):
            # Deliberately not simplified. Douglas-Peucker anchors on the first
            # and last point, which on a closed ring are neighbours -- every
            # vertex then measures as "close to the baseline" and the ring
            # collapses to a handful of points. Coastline is only a few thousand
            # vertices, so it costs nothing to keep it whole.
            out.append(f"F {LAYER['land']} 10 1 {len(ring)} ")
            out.append(" ".join(f"{x:.6f} {y:.6f}" for x, y in ring))
            kept += 1

    with open(dst, "w") as fh:
        fh.write("\n".join(out) + "\n")

    print(f">> kept {kept} features, dropped {dropped}")


if __name__ == "__main__":
    convert(sys.argv[1:-1], sys.argv[-1], os.environ.get("BBOX_TUPLE") and
            tuple(float(v) for v in os.environ["BBOX_TUPLE"].split(",")))
