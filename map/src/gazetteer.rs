//! Looking a place up by name, without a network and without a view.
//!
//! `find.rs` is the other half of this and answers a different question: it
//! searches *what is on screen*, which is right for a search box on a map you
//! are driving and useless for "where is Jaipur" asked from a chat page with no
//! map open. So this builds an index once, off the archive itself, by reading
//! every tile at one coarse zoom and keeping only the names and where they are.
//!
//! What it costs is bounded and small. The geometry is thrown away as each tile
//! is read -- an entry is a name, a point and a framing zoom -- and the tiles
//! never enter the renderer's cache, so building this does not evict the 96
//! tiles somebody is looking at.
//!
//! What it covers is whatever the basemap has at that zoom, which for the India
//! archive is every state and a few hundred towns. That is a real limit and it
//! is better to say so than to imply a geocoder: a name this cannot find gets a
//! "not found" rather than a plausible wrong point somewhere in Gujarat.

use crate::data::Layer;
use crate::pmtiles::TileId;

/// One place, and enough to fly to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    /// `state`, `place`, `water` -- the layer it came from, in a word.
    pub what: &'static str,
    /// World (Mercator) coordinates.
    pub world: [f64; 2],
    pub lonlat: (f64, f64),
    /// A zoom that frames it, from its own extent. A town is a point and gets a
    /// neighbourhood; a state gets the whole state.
    pub zoom: f64,
}

#[derive(Debug, Default)]
pub struct Gazetteer {
    entries: Vec<Entry>,
    /// The zooms the sweep read, for the log line. An empty index and an index
    /// of an archive that had nothing at those zooms are worth telling apart.
    pub swept: [u8; 5],
}

/// The zooms to read, and why there is more than one.
///
/// A vector tile carries a label at the zooms where that label is *useful*, not
/// at every zoom, so no single level is a gazetteer. Measured against this
/// archive: z6 alone has 280 names and no Kolkata or Bengaluru; z7 has 1433 and
/// no Chennai or Ahmedabad; z8 has 3124 and still no Chennai.
///
/// How deep to go was measured too, and the first answer was wrong. z6-8 is
/// 3187 names in 0.6 s -- and has **no Agra**, which is not an obscure town.
/// z6-9 is 4147 names in 1.2 s and finds it. z6-10 is 46582 names in 2.2 s and
/// also finds Sarnath. That is 15x the names for 3.5x the time, once, on a
/// background thread at boot, for about 4 MB -- so it is worth it, and "going
/// further up is not worth it" was a guess that a real search proved wrong.
///
/// z11 is where it stops being worth it: 43680 tiles, and the landmark layer
/// this would be reaching for still does not carry **Taj Mahal** at any depth
/// tried. Some things are simply not in this archive. Kochi is another -- not
/// under Kochi, Cochin or Ernakulam. That is why a miss is a miss: snapping to
/// the nearest name answered "where is Agra" with *Jagraon*, 600 km away in
/// Punjab, because one string contained the other.
pub const SWEEP_ZOOMS: [u8; 5] = [6, 7, 8, 9, 10];

/// Layers worth indexing, and what to call each one.
const WANTED: [(Layer, &str); 4] = [
    (Layer::Boundary, "state"),
    (Layer::Place, "place"),
    (Layer::Landmark, "landmark"),
    (Layer::Water, "water"),
];

impl Gazetteer {
    /// Read the archive once and keep the names.
    ///
    /// Takes the source rather than an archive because the always-resident
    /// overlays -- every Indian state, by name -- are held there and are not in
    /// the tile pyramid at all.
    pub fn build(src: &mut crate::tiles::Source) -> Gazetteer {
        let mut g = Gazetteer { entries: Vec::new(), swept: SWEEP_ZOOMS };

        // The overlays first, so a state found there wins over the same name in
        // a tile: the overlay carries the whole boundary and therefore a zoom
        // that frames the whole state.
        for tile in src.overlay_tiles() {
            g.take(&tile);
        }
        for z in SWEEP_ZOOMS {
            for id in sweep_ids(src.bounds(), z) {
                if let Some(tile) = src.read_uncached(id) {
                    g.take(&tile);
                }
            }
        }
        g.entries.sort_by(|a, b| a.name.cmp(&b.name));
        g.entries.dedup_by(|a, b| a.name.eq_ignore_ascii_case(&b.name));
        g
    }

    fn take(&mut self, tile: &crate::data::Tile) {
        for (layer, what) in WANTED {
            for &i in &tile.by_layer[layer.index()] {
                let f = &tile.features[i as usize];
                let Some(name) = f.name.as_deref() else { continue };
                if name.trim().is_empty() {
                    continue;
                }
                // The same width the panel is drawn at, so the framing zoom an
                // entry carries is the one that will actually be used. A zoom
                // fitted to a full screen shows a third of the state in a
                // thumbnail.
                let (world, zoom) = crate::find::frame(&f.pts, 46.0 * crate::canvas::SUB_X as f64);
                let (lon, lat) = crate::geo::world_to_lonlat(world[0], world[1]);
                self.entries.push(Entry {
                    name: name.to_string(),
                    what,
                    world,
                    lonlat: (lon, lat),
                    zoom,
                });
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The best match for a name, or nothing.
    ///
    /// Deliberately not fuzzy, for the reason `find.rs` gives: a fuzzy match
    /// over a few thousand names answers every query, and a lookup that always
    /// succeeds is one that will one day put Kolkata on screen because somebody
    /// asked about a colour. Exact, then prefix, then word-prefix.
    pub fn find(&self, q: &str) -> Option<&Entry> {
        let q = q.trim();
        if q.len() < 3 {
            return None;
        }
        self.entries
            .iter()
            // Exact, prefix, or the start of a word in the name -- and *not* a
            // bare substring, which `find::score` allows because a search box
            // on a map you are driving should be forgiving. A geocoder must not
            // be: `Agra` is inside `Jagraon`, and answering with a town in
            // Punjab is worse than answering with nothing.
            .filter_map(|e| crate::find::score(&e.name, q).filter(|r| *r <= 2).map(|r| (r, e)))
            // A state beats a town of the same name at the same score: asked
            // for "Goa", the state is what anybody means.
            .min_by_key(|(r, e)| (*r, if e.what == "state" { 0 } else { 1 }, e.name.len()))
            .map(|(_, e)| e)
    }

    /// Every name, for the tests and for a log line.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }
}

/// Every tile id at `z` covering `bounds`, in world coordinates.
///
/// Its own function rather than `tiles::tile_range` because that one is capped
/// for a frame -- it steps the zoom down until the count fits a viewport -- and
/// a sweep wants all of them exactly once.
fn sweep_ids(bounds: [f64; 4], z: u8) -> Vec<TileId> {
    let n = 1u32 << z;
    let at = |v: f64| ((v * n as f64).floor().max(0.0) as u32).min(n - 1);
    let (x0, x1) = (at(bounds[0]), at(bounds[2]));
    let (y0, y1) = (at(bounds[1]), at(bounds[3]));
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            out.push(TileId { z, x, y });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep covers the archive's own extent, once each.
    #[test]
    fn a_sweep_visits_every_tile_in_bounds_once() {
        let ids = sweep_ids([0.25, 0.25, 0.5, 0.5], 4);
        assert_eq!(ids.len(), 5 * 5, "not a full rectangle: {}", ids.len());
        let mut seen = ids.clone();
        seen.sort_by_key(|t| (t.z, t.x, t.y));
        seen.dedup();
        assert_eq!(seen.len(), ids.len(), "a tile was visited twice");
        assert!(ids.iter().all(|t| t.z == 4));
    }

    /// A whole-world sweep at the archive's zoom must not be unbounded.
    #[test]
    fn a_sweep_of_the_whole_world_is_still_a_number_we_can_read() {
        for z in SWEEP_ZOOMS {
            let all = sweep_ids([0.0, 0.0, 1.0, 1.0], z);
            assert_eq!(all.len(), (1usize << z) * (1usize << z));
        }
    }

    #[test]
    fn the_real_index_finds_places_across_india() {
        let mut src = crate::tiles::Source::open(None);
        if !src.has_basemap() {
            eprintln!("no basemap: skipping the index test");
            return;
        }
        let t0 = std::time::Instant::now();
        let g = Gazetteer::build(&mut src);
        let took = t0.elapsed();
        eprintln!("INDEX {} names in {:?} at z{:?}", g.len(), took, g.swept);

        // Spread deliberately: four corners of the country and two states, so a
        // sweep that quietly covered only Gujarat would fail this.
        for want in ["Jaipur", "Chennai", "Kolkata", "Ahmedabad", "Guwahati", "Kerala", "Punjab"] {
            let hit = g.find(want).unwrap_or_else(|| panic!("no `{want}` in {} names", g.len()));
            let (lon, lat) = hit.lonlat;
            assert!(
                (60.0..100.0).contains(&lon) && (5.0..40.0).contains(&lat),
                "`{want}` came back at {lon},{lat}, which is not in India"
            );
        }
        // Jaipur is a city in Rajasthan, and it had better not be in Gujarat.
        let j = g.find("Jaipur").unwrap();
        assert!((75.0..77.0).contains(&j.lonlat.0), "Jaipur at lon {}", j.lonlat.0);
        assert!((26.0..28.0).contains(&j.lonlat.1), "Jaipur at lat {}", j.lonlat.1);

        // A state frames wider than a town. If they came out the same the
        // framing zoom is not being taken from the feature's own extent.
        let state = g.find("Kerala").unwrap().zoom;
        assert!(state < j.zoom, "a whole state framed no wider than a city: {state} vs {}", j.zoom);

        // A name the archive does not have must come back empty rather than as
        // whatever sorted nearest. See the note on `SWEEP_ZOOMS`: this is not a
        // hypothetical, Kochi is a real hole in this archive.
        assert!(g.find("Lannisport").is_none(), "invented a place");
        assert!(g.find("Zzyzx").is_none(), "invented a place");
    }

    /// Too short a query finds nothing rather than the first thing.
    #[test]
    fn two_letters_is_not_a_lookup() {
        let g = Gazetteer {
            entries: vec![Entry {
                name: "Goa".into(),
                what: "state",
                world: [0.5, 0.5],
                lonlat: (0.0, 0.0),
                zoom: 8.0,
            }],
            swept: SWEEP_ZOOMS,
        };
        assert!(g.find("go").is_none(), "a two-letter query matched");
        assert!(g.find("Goa").is_some());
        assert!(g.find("  goa ").is_some(), "not trimmed or not case-folded");
        assert!(g.find("Jaipur").is_none(), "matched something it does not have");
    }
}
