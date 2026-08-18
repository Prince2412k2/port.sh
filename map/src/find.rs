//! Looking a place up by name.
//!
//! There is no geocoder here and no network to reach one, so the gazetteer is
//! whatever the renderer already has in memory: the tour's own stops, the
//! always-resident overlays (every Indian state, by name), and the named
//! features of whatever tiles the current view has pulled in. That last source
//! is why a search finds far more at street zoom than at country zoom — it is
//! searching what has been loaded, not an index, and saying so is better than
//! pretending otherwise.

use std::rc::Rc;

use crate::data::{Layer, Tile};

pub struct Hit {
    pub name: String,
    pub what: &'static str,
    /// World coordinates to centre on.
    pub at: [f64; 2],
    /// A zoom that frames it, worked out from its own extent.
    pub zoom: f64,
    /// Lower sorts first.
    rank: u8,
}

/// Case-insensitive, prefix-first. Deliberately not fuzzy: a fuzzy match over a
/// few thousand names returns something for every query, and a search that
/// always succeeds is one you stop trusting.
fn score(name: &str, q: &str) -> Option<u8> {
    let n = name.to_lowercase();
    let q = q.to_lowercase();
    if n == q {
        Some(0)
    } else if n.starts_with(&q) {
        Some(1)
    } else if n.split_whitespace().any(|w| w.starts_with(&q)) {
        Some(2)
    } else if n.contains(&q) {
        Some(3)
    } else {
        None
    }
}

/// Centre and a zoom that frames the feature, from its own bounding box.
fn frame(pts: &[[f64; 2]], sw: f64) -> ([f64; 2], f64) {
    let (mut x0, mut y0) = (f64::MAX, f64::MAX);
    let (mut x1, mut y1) = (f64::MIN, f64::MIN);
    for p in pts {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let c = [(x0 + x1) * 0.5, (y0 + y1) * 0.5];
    let span = (x1 - x0).max(y1 - y0);
    let zoom = if span <= 0.0 {
        // A point has no extent, so there is nothing to fit to; this is close
        // enough to see a neighbourhood around it.
        13.5
    } else {
        // Fit the long side into about two thirds of the frame.
        (sw * 0.66 / (256.0 * span)).log2()
    };
    (c, zoom.clamp(crate::geo::MIN_ZOOM, 16.0))
}

pub fn search(
    q: &str,
    places: &[crate::place::Place],
    tiles: &[Rc<Tile>],
    sw: f64,
    limit: usize,
) -> Vec<Hit> {
    let q = q.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Hit> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for p in places {
        if let Some(r) = score(&p.name, q) {
            out.push(Hit {
                name: p.name.clone(),
                what: "tour",
                at: p.world,
                zoom: p.zoom,
                // Tour stops outrank the basemap: on this map they are the
                // subject and everything else is context.
                rank: r,
            });
            seen.push(p.name.to_lowercase());
        }
    }

    for tile in tiles {
        for f in &tile.features {
            let Some(name) = f.name.as_deref() else { continue };
            let Some(r) = score(name, q) else { continue };
            let key = name.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            let what = match f.layer {
                Layer::Boundary => "state",
                Layer::Place => "place",
                Layer::Landmark => "landmark",
                Layer::Water => "water",
                _ => continue,
            };
            let (at, zoom) = frame(&f.pts, sw);
            seen.push(key);
            out.push(Hit { name: name.to_string(), what, at, zoom, rank: r + 4 });
        }
    }

    out.sort_by(|a, b| a.rank.cmp(&b.rank).then(a.name.len().cmp(&b.name.len())));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Feature;

    fn tile(names: &[(&str, Layer)]) -> Rc<Tile> {
        let feats = names
            .iter()
            .enumerate()
            .map(|(i, (n, l))| {
                Feature::new(
                    *l,
                    0,
                    false,
                    Some((*n).into()),
                    vec![[0.5 + i as f64 * 0.01, 0.5], [0.52 + i as f64 * 0.01, 0.52]],
                )
            })
            .collect();
        Rc::new(Tile::new(feats))
    }

    #[test]
    fn a_prefix_beats_a_substring() {
        let t = tile(&[("Gujarat", Layer::Boundary), ("New Gujarat Road", Layer::Place)]);
        let hits = search("guj", &[], &[t], 360.0, 10);
        assert_eq!(hits[0].name, "Gujarat");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn the_tour_outranks_the_basemap() {
        let places = crate::place::parse(include_str!("../data/places.txt")).unwrap();
        let t = tile(&[("Silver Oak Road", Layer::Place)]);
        let hits = search("silver", &places, &[t], 360.0, 10);
        assert_eq!(hits[0].what, "tour");
    }

    /// A search that returns something for every query is one nobody trusts.
    #[test]
    fn nonsense_finds_nothing() {
        let t = tile(&[("Gujarat", Layer::Boundary)]);
        assert!(search("qzxwv", &[], std::slice::from_ref(&t), 360.0, 10).is_empty());
        assert!(search("   ", &[], &[t], 360.0, 10).is_empty());
    }

    #[test]
    fn the_same_name_is_not_listed_twice() {
        let t = tile(&[("Kapadvanj", Layer::Place), ("Kapadvanj", Layer::Place)]);
        assert_eq!(search("kapad", &[], &[t], 360.0, 10).len(), 1);
    }

    #[test]
    fn a_bigger_feature_gets_a_wider_zoom() {
        let big = tile(&[("Big", Layer::Boundary)]);
        let hits = search("big", &[], &[big], 360.0, 5);
        assert!(hits[0].zoom < 16.0 && hits[0].zoom > crate::geo::MIN_ZOOM);
    }
}
