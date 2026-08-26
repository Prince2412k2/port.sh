//! Mapbox Vector Tile decoding, cut down to what the renderer consumes.
//!
//! A full protobuf dependency would buy very little here: the tile schema is
//! four nested messages and we only read three fields out of them. Geometry is
//! converted straight to world coordinates on the way out, so nothing
//! downstream needs to know a tile was involved.

use crate::data::{Feature, Layer};
use crate::pmtiles::TileId;

/// Planetiler's default schema. Values come from probing a real archive rather
/// than the docs -- see `scripts/probe_pmtiles.py`.
fn classify(layer: &str, class: &str, subclass: &str) -> Option<(Layer, u16)> {
    Some(match layer {
        "roads" => match class {
            "motorway" => (Layer::RoadMajor, 220),
            "trunk" => (Layer::RoadMajor, 215),
            "primary" => (Layer::RoadMedium, 190),
            "secondary" => (Layer::RoadMedium, 165),
            "tertiary" => (Layer::RoadMinor, 130),
            "unclassified" | "residential" => (Layer::RoadMinor, 95),
            "living_street" | "service" => (Layer::RoadMinor, 80),
            "rail" | "transit" => (Layer::Rail, 120),
            // Footpaths, steps and tracks are noise at every zoom this renders.
            _ => return None,
        },
        "water" => (Layer::Water, 60),
        "waterways" => match class {
            "river" | "canal" => (Layer::Water, 70),
            _ => return None,
        },
        "landcover" => match class {
            "forest" | "wood" | "grass" | "farmland" => (Layer::Landuse, 40),
            _ => return None,
        },
        "landuse" => (Layer::Landuse, 45),
        "places" => match class {
            "country" => (Layer::Place, 250),
            "state" => (Layer::Place, 215),
            "city" => (Layer::Place, 200),
            "town" => (Layer::Place, 170),
            "suburb" => (Layer::Place, 150),
            "neighbourhood" | "village" => (Layer::Place, 120),
            _ => (Layer::Place, 110),
        },
        // POI ranks come from probing the archive, not from the tag names.
        // Transit and culture are what you navigate by; restaurants, shops and
        // clinics are the bulk of the layer and are ranked below every zoom
        // floor so they surface only when you are right on top of them.
        "pois" => match subclass {
            "station" | "bus_station" | "airport" | "aerodrome" | "ferry_terminal" => {
                (Layer::Landmark, 175)
            }
            "museum" | "theatre" | "attraction" | "gallery" => (Layer::Landmark, 155),
            // The things a city is actually known by, and every one of them was
            // invisible until this line existed.
            "monument" | "memorial" | "fort" | "ruins" | "tower" | "viewpoint" | "artwork" => {
                (Layer::Landmark, 152)
            }
            "university" | "college" => (Layer::Landmark, 150),
            "place_of_worship" => (Layer::Landmark, 138),
            "park" | "stadium" | "zoo" | "garden" => (Layer::Landmark, 130),
            "cinema" | "library" | "marketplace" | "theme_park" => (Layer::Landmark, 122),
            "hospital" => (Layer::Landmark, 112),
            "police" | "post_office" | "fire_station" | "townhall" | "courthouse" => {
                (Layer::Landmark, 108)
            }
            _ => (Layer::Landmark, poi_by_category(class)),
        },
        _ => return None,
    })
}

/// What a POI is worth when its `subcategory` is not one this knows.
///
/// The archive carries a coarse `category` beside the fine `subcategory`, and
/// for a long time this threw it away: anything whose subcategory missed the
/// list above scored 70, and `scene::rank_floor` never drops below 105. Not
/// low-priority -- *unreachable*, at every zoom, for ever. Measured against a
/// real 25-tile patch of Mumbai that was 73% of the POI layer, including 28 of
/// the 41 features the archive itself files under `landmark`: monuments,
/// memorials, towers, galleries, forts, ruins.
///
/// The categories left at 70 are the ones that would drown the map -- shops,
/// food, lodging, clinics, schools -- and they are the bulk of the layer.
/// They are still searchable and still label on hover; they just do not
/// compete for space with a fort.
fn poi_by_category(category: &str) -> u16 {
    match category {
        "transit" => 168,
        "landmark" => 152,
        "culture" => 122,
        "recreation" => 118,
        "civic" => 108,
        _ => 70,
    }
}

fn varint(b: &[u8], p: &mut usize) -> u64 {
    let mut r = 0u64;
    let mut s = 0u32;
    while *p < b.len() {
        let x = b[*p];
        *p += 1;
        r |= ((x & 0x7F) as u64) << s;
        if x & 0x80 == 0 {
            break;
        }
        s += 7;
    }
    r
}

fn skip(b: &[u8], p: &mut usize, wire: u64) {
    match wire {
        0 => {
            varint(b, p);
        }
        1 => *p += 8,
        2 => {
            let n = varint(b, p) as usize;
            *p += n;
        }
        5 => *p += 4,
        _ => *p = b.len(),
    }
}

#[inline]
fn zigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Decode one tile's worth of features, already in world coordinates.
pub fn decode(buf: &[u8], tile: TileId) -> Vec<Feature> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p < buf.len() {
        let key = varint(buf, &mut p);
        if key >> 3 == 3 && key & 7 == 2 {
            let n = varint(buf, &mut p) as usize;
            let end = (p + n).min(buf.len());
            decode_layer(&buf[p..end], tile, &mut out);
            p = end;
        } else {
            skip(buf, &mut p, key & 7);
        }
    }
    out
}

fn decode_layer(b: &[u8], tile: TileId, out: &mut Vec<Feature>) {
    let mut name = String::new();
    let mut keys: Vec<String> = Vec::new();
    let mut vals: Vec<String> = Vec::new();
    let mut extent = 4096u32;
    let mut feats: Vec<&[u8]> = Vec::new();

    let mut p = 0usize;
    while p < b.len() {
        let key = varint(b, &mut p);
        let (field, wire) = (key >> 3, key & 7);
        match (field, wire) {
            (1, 2) => {
                let n = varint(b, &mut p) as usize;
                name = String::from_utf8_lossy(&b[p..p + n]).into_owned();
                p += n;
            }
            (3, 2) => {
                let n = varint(b, &mut p) as usize;
                keys.push(String::from_utf8_lossy(&b[p..p + n]).into_owned());
                p += n;
            }
            (4, 2) => {
                let n = varint(b, &mut p) as usize;
                vals.push(decode_value(&b[p..p + n]));
                p += n;
            }
            (2, 2) => {
                let n = varint(b, &mut p) as usize;
                feats.push(&b[p..p + n]);
                p += n;
            }
            (5, 0) => extent = varint(b, &mut p) as u32,
            _ => skip(b, &mut p, wire),
        }
    }

    // Nothing in this layer maps onto a renderable layer -- skip the geometry
    // work entirely rather than decode and discard.
    if classify(&name, "", "").is_none()
        && !matches!(name.as_str(), "roads" | "waterways" | "landcover" | "places")
    {
        return;
    }

    for f in feats {
        decode_feature(f, &name, &keys, &vals, extent, tile, out);
    }
}

fn decode_value(b: &[u8]) -> String {
    let mut p = 0usize;
    let key = varint(b, &mut p);
    let (field, wire) = (key >> 3, key & 7);
    match (field, wire) {
        (1, 2) => {
            let n = varint(b, &mut p) as usize;
            String::from_utf8_lossy(&b[p..p + n]).into_owned()
        }
        (_, 0) => varint(b, &mut p).to_string(),
        _ => String::new(),
    }
}

fn decode_feature(
    b: &[u8],
    layer_name: &str,
    keys: &[String],
    vals: &[String],
    extent: u32,
    tile: TileId,
    out: &mut Vec<Feature>,
) {
    let mut tags: Vec<u32> = Vec::new();
    let mut geom_type = 0u64;
    let mut geom: Vec<u32> = Vec::new();

    let mut p = 0usize;
    while p < b.len() {
        let key = varint(b, &mut p);
        let (field, wire) = (key >> 3, key & 7);
        match (field, wire) {
            (2, 2) => {
                let n = varint(b, &mut p) as usize;
                let end = p + n;
                while p < end {
                    tags.push(varint(b, &mut p) as u32);
                }
            }
            (3, 0) => geom_type = varint(b, &mut p),
            (4, 2) => {
                let n = varint(b, &mut p) as usize;
                let end = p + n;
                while p < end {
                    geom.push(varint(b, &mut p) as u32);
                }
            }
            _ => skip(b, &mut p, wire),
        }
    }

    let mut class = "";
    let mut subclass = "";
    let mut name: Option<&str> = None;
    for t in tags.chunks_exact(2) {
        let (k, v) = (t[0] as usize, t[1] as usize);
        if k >= keys.len() || v >= vals.len() {
            continue;
        }
        match keys[k].as_str() {
            "class" | "category" if class.is_empty() => class = &vals[v],
            "subclass" | "subcategory" if subclass.is_empty() => subclass = &vals[v],
            // Prefer the romanised name; a Devanagari label cannot be drawn in
            // a single terminal cell per character anyway.
            "name:en" => name = Some(&vals[v]),
            "name" if name.is_none() => name = Some(&vals[v]),
            _ => {}
        }
    }

    // POIs carry the useful distinction in subcategory ("station") and a coarse
    // one in category ("transit"). Both go in: the fine one decides when it is
    // recognised, the coarse one catches everything else.
    let Some((layer, rank)) = classify(layer_name, class, subclass) else { return };

    let scale = 1.0 / (extent as f64 * (1u64 << tile.z) as f64);
    let ox = tile.x as f64 / (1u64 << tile.z) as f64;
    let oy = tile.y as f64 / (1u64 << tile.z) as f64;
    let closed = geom_type == 3;

    // Command-encoded geometry: MoveTo starts a new part, LineTo extends it.
    let mut parts: Vec<Vec<[f64; 2]>> = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    let (mut cx, mut cy) = (0i64, 0i64);
    let mut i = 0usize;
    while i < geom.len() {
        let cmd = geom[i] & 0x7;
        let count = geom[i] >> 3;
        i += 1;
        match cmd {
            1 | 2 => {
                for _ in 0..count {
                    if i + 1 >= geom.len() {
                        break;
                    }
                    cx += zigzag(geom[i] as u64);
                    cy += zigzag(geom[i + 1] as u64);
                    i += 2;
                    if cmd == 1 && !cur.is_empty() {
                        parts.push(std::mem::take(&mut cur));
                    }
                    cur.push([ox + cx as f64 * scale, oy + cy as f64 * scale]);
                }
            }
            7 => {
                if !cur.is_empty() {
                    parts.push(std::mem::take(&mut cur));
                }
            }
            _ => break,
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }

    let name: Option<Box<str>> = name.filter(|s| !s.is_empty()).map(Box::from);

    for pts in parts {
        if pts.len() < 2 && !layer.is_point() {
            continue;
        }
        // MVT winds exterior rings clockwise in tile space. Tile space has y
        // increasing downward, so clockwise is *positive* shoelace area -- the
        // opposite of the usual convention, and easy to get backwards. Holes
        // are dropped: one ring per feature keeps the even-odd fill honest, and
        // an unfilled lake reads better than a filled one.
        if closed && signed_area(&pts) <= 0.0 {
            continue;
        }
        out.push(Feature::new(layer, rank, closed, name.clone(), pts));
    }
}

fn signed_area(r: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..r.len() {
        let p = r[i];
        let q = r[(i + 1) % r.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The archive's coarse `category` is the fallback, and it has to be one.
    ///
    /// These are real subcategory values, counted off a 25-tile patch of Mumbai
    /// in the shipped archive. Every one of them used to score 70 against a
    /// label floor that never drops below 105 -- not deprioritised, unreachable,
    /// at every zoom. Twenty-eight of the forty-one features the archive itself
    /// files under `landmark` were invisible for that reason.
    #[test]
    fn a_landmark_the_subcategory_list_has_never_heard_of_still_ranks() {
        for sub in ["monument", "memorial", "tower", "fort", "ruins", "gallery", "zoo"] {
            let (layer, rank) = classify("pois", "landmark", sub).expect("classified");
            assert_eq!(layer, Layer::Landmark);
            assert!(rank >= 105, "{sub} scored {rank}, under every label floor");
        }
        // And through the category alone, for a subcategory nobody has listed.
        let (_, rank) = classify("pois", "landmark", "obelisk").expect("classified");
        assert!(rank >= 105, "an unlisted landmark scored {rank}");
    }

    /// The other half of the bargain: the bulk of the layer stays down.
    ///
    /// Widening the net is only safe because these do not come with it. A map
    /// that names every restaurant is a directory.
    #[test]
    fn shops_and_restaurants_stay_under_the_floor() {
        for (cat, sub) in [
            ("shop", "clothes"),
            ("food", "restaurant"),
            ("food", "cafe"),
            ("lodging", "hotel"),
            ("health", "clinic"),
            ("education", "school"),
            ("service", "atm"),
        ] {
            let (_, rank) = classify("pois", cat, sub).expect("classified");
            assert!(rank < 105, "{cat}/{sub} scored {rank} and would crowd the map");
        }
    }

    /// A layer this does not render is skipped whole, and the archive has one:
    /// there is no `buildings` layer in it at all.
    #[test]
    fn an_unknown_layer_is_declined() {
        assert!(classify("buildings", "yes", "").is_none());
        assert!(classify("aeroway", "runway", "").is_none());
    }
}
