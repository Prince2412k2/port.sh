//! Where features come from.
//!
//! Two backends behind one interface. A `.tmap` file is loaded once and handed
//! back as a single tile; a PMTiles archive is read on demand, tile by tile, so
//! a country-sized basemap never has to fit in memory.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use crate::data::{MapData, Tile};
use crate::geo::Viewport;
use crate::mvt;
use crate::pmtiles::{Archive, TileId};

/// Tiles held after a frame. Enough for a wide view plus room to pan before
/// anything is re-read; a z14 tile of dense city is only a few hundred KB.
const CACHE_TILES: usize = 96;

/// Above this many tiles for one viewport, step down a zoom level instead.
/// Guards against a wide view at a high tile zoom asking for thousands.
const MAX_TILES_PER_VIEW: usize = 40;

pub struct Source {
    backend: Backend,
    /// Elevation, if a heightmap was found. Shared by every frame.
    pub terrain: Option<crate::terrain::Terrain>,
    /// Always-resident features drawn over whatever the backend returns.
    /// Administrative boundaries live here: the basemap has none, and they are
    /// small enough that streaming them would be pure overhead.
    overlays: Vec<Rc<Tile>>,
}

enum Backend {
    /// Everything resident, as one tile.
    Static { tile: Rc<Tile>, label: String, bounds: [f64; 4] },
    Tiled(Tiled),
}

impl Source {
    pub fn open(path: Option<&str>) -> Self {
        let mut src = Source {
            backend: Backend::open(path),
            terrain: None,
            overlays: Vec::new(),
        };
        for c in ["india.tmhg", "terrain.tmhg"].iter().filter_map(|n| crate::paths::data_file(n)) {
            {
                match crate::terrain::Terrain::open(Path::new(&c)) {
                    Ok(t) => {
                        src.terrain = Some(t);
                        break;
                    }
                    Err(e) => eprintln!("termap: {}: {e}", c.display()),
                }
            }
        }
        for c in ["states.tmap", "buildings.tmap"].iter().filter_map(|n| crate::paths::data_file(n)) {
            if let Ok(text) = std::fs::read_to_string(&c) {
                let feats = crate::data::parse_features(&text);
                if !feats.is_empty() {
                    src.overlays.push(Rc::new(Tile::new(feats)));
                }
            }
        }
        src
    }

    /// Whether a real tiled basemap was found, as opposed to the sample built
    /// into the binary.
    ///
    /// Worth asking out loud: the fallback is a small extract of Mumbai, and a
    /// tour of Gujarat drawn on top of Mumbai is not a degraded map, it is a
    /// wrong one. Better to say the archive is missing than to render a
    /// confident picture of somewhere else.
    pub fn has_basemap(&self) -> bool {
        matches!(self.backend, Backend::Tiled(_))
    }

    pub fn label(&self) -> &str {
        match &self.backend {
            Backend::Static { label, .. } => label,
            Backend::Tiled(t) => &t.label,
        }
    }

    /// Opening view.
    pub fn bounds(&self) -> [f64; 4] {
        match &self.backend {
            Backend::Static { bounds, .. } => *bounds,
            Backend::Tiled(t) => t.world_bounds,
        }
    }

    /// The always-resident overlays -- every Indian state, by name. Not in the
    /// tile pyramid, so a sweep of the pyramid alone would miss all of them.
    pub fn overlay_tiles(&self) -> Vec<Rc<Tile>> {
        self.overlays.clone()
    }

    /// One tile, straight off the archive and *not* into the cache.
    ///
    /// For the gazetteer's one-time sweep. Going through `tiles()` would work
    /// and would evict the ninety-six tiles somebody is currently looking at,
    /// to index names that are then thrown away with the geometry.
    pub fn read_uncached(&mut self, id: crate::pmtiles::TileId) -> Option<Tile> {
        let Backend::Tiled(t) = &mut self.backend else { return None };
        match t.archive.tile(id) {
            Ok(Some(bytes)) => Some(Tile::new(mvt::decode(&bytes, id))),
            _ => None,
        }
    }

    /// Tiles covering the viewport, loading whatever is missing.
    pub fn tiles(&mut self, vp: &Viewport) -> Vec<Rc<Tile>> {
        let mut out = match &mut self.backend {
            Backend::Static { tile, .. } => vec![tile.clone()],
            Backend::Tiled(t) => t.cover(vp),
        };
        out.extend(self.overlays.iter().cloned());
        out
    }

    /// Feature count currently resident, for the status line.
    pub fn resident(&self) -> usize {
        let base = match &self.backend {
            Backend::Static { tile, .. } => tile.features.len(),
            Backend::Tiled(t) => t.cache.values().map(|t| t.features.len()).sum(),
        };
        base + self.overlays.iter().map(|t| t.features.len()).sum::<usize>()
    }
}

impl Backend {
    fn open(path: Option<&str>) -> Self {
        if let Some(p) = path {
            if p.ends_with(".pmtiles") {
                match Tiled::open(Path::new(p)) {
                    Ok(t) => return Backend::Tiled(t),
                    Err(e) => eprintln!("termap: {p}: {e} -- falling back to .tmap"),
                }
            }
        }
        // With no explicit path, look for a basemap before falling back to a
        // .tmap. TERMAP_BASEMAP lets the archive live outside the project.
        if path.is_none() {
            let env: Option<std::path::PathBuf> =
                std::env::var_os("TERMAP_BASEMAP").map(Into::into);
            let found = ["india.pmtiles", "basemap.pmtiles"]
                .iter()
                .filter_map(|n| crate::paths::data_file(n));
            for c in env.into_iter().filter(|p| p.exists()).chain(found) {
                match Tiled::open(&c) {
                    Ok(t) => return Backend::Tiled(t),
                    Err(e) => eprintln!("termap: {}: {e}", c.display()),
                }
            }
        }
        let data = MapData::load(path);
        let bounds = data.bounds();
        Backend::Static {
            tile: Rc::new(data.tile),
            label: data.source,
            bounds,
        }
    }
}

pub struct Tiled {
    archive: Archive,
    cache: HashMap<TileId, Rc<Tile>>,
    /// Least-recently-used ordering; front is oldest.
    order: Vec<TileId>,
    pub label: String,
    pub world_bounds: [f64; 4],
}

impl Tiled {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let archive = Archive::open(path)?;
        let b = archive.bounds;
        let lo = crate::geo::lonlat_to_world(b[0], b[3]);
        let hi = crate::geo::lonlat_to_world(b[2], b[1]);
        Ok(Tiled {
            label: path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "pmtiles".into()),
            world_bounds: [lo[0], lo[1], hi[0], hi[1]],
            archive,
            cache: HashMap::new(),
            order: Vec::new(),
        })
    }

    fn cover(&mut self, vp: &Viewport) -> Vec<Rc<Tile>> {
        let b = vp.world_bounds(0.0);
        // One subpixel is one slippy pixel and a tile is 256 of them, so the
        // viewport zoom maps straight onto a tile zoom.
        let base = (vp.zoom.round() as i32)
            .clamp(self.archive.min_zoom as i32, self.archive.max_zoom as i32)
            as u8;

        // An archive can advertise a minimum zoom it does not actually
        // populate -- the India basemap claims z4 and has no z4 tiles. Rather
        // than render an empty map, step in until something comes back. Bounded,
        // because over open ocean every zoom is legitimately empty.
        for step in 0..=2u8 {
            let mut z = (base + step).min(self.archive.max_zoom);
            let mut ids = tile_range(&b, z);
            while ids.len() > MAX_TILES_PER_VIEW && z > self.archive.min_zoom {
                z -= 1;
                ids = tile_range(&b, z);
            }
            let out = self.load(ids);
            let empty = out.iter().all(|t| t.features.is_empty());
            if !empty || step == 2 || z >= self.archive.max_zoom {
                return out;
            }
        }
        Vec::new()
    }

    fn load(&mut self, ids: Vec<TileId>) -> Vec<Rc<Tile>> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = self.cache.get(&id) {
                let t = t.clone();
                self.touch(id);
                out.push(t);
                continue;
            }
            let tile = match self.archive.tile(id) {
                Ok(Some(bytes)) => Rc::new(Tile::new(mvt::decode(&bytes, id))),
                // Absent tiles are normal: the archive is sparse over ocean.
                _ => Rc::new(Tile::new(Vec::new())),
            };
            self.insert(id, tile.clone());
            out.push(tile);
        }
        out
    }

    fn touch(&mut self, id: TileId) {
        if let Some(i) = self.order.iter().position(|&o| o == id) {
            let v = self.order.remove(i);
            self.order.push(v);
        }
    }

    fn insert(&mut self, id: TileId, tile: Rc<Tile>) {
        self.cache.insert(id, tile);
        self.order.push(id);
        while self.order.len() > CACHE_TILES {
            let old = self.order.remove(0);
            self.cache.remove(&old);
        }
    }
}

/// Tile ids covering a world-coordinate bbox at zoom `z`.
fn tile_range(b: &[f64; 4], z: u8) -> Vec<TileId> {
    let n = (1u64 << z) as f64;
    let clamp = |v: f64| (v * n).floor().clamp(0.0, n - 1.0) as u32;
    let (x0, x1) = (clamp(b[0]), clamp(b[2]));
    let (y0, y1) = (clamp(b[1]), clamp(b[3]));
    let mut out = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            out.push(TileId { z, x, y });
        }
    }
    out
}
