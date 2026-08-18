//! Map features and the `.tmap` reader.
//!
//! `.tmap` is a flat text format produced by `scripts/osm2tmap.py`. It exists so
//! the renderer has no JSON or vector-tile dependency; swapping in a real
//! pmtiles/MVT source later means replacing `MapData::load` and nothing else.
//!
//!     # termap 1
//!     F <layer> <rank> <closed> <npts> <name>
//!     <lon> <lat> <lon> <lat> ...

use crate::geo::lonlat_to_world;

pub const SAMPLE: &str = include_str!("../assets/mumbai-sample.tmap");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layer {
    Landuse,
    Water,
    Coast,
    Rail,
    RoadMinor,
    RoadMedium,
    RoadMajor,
    Place,
    Landmark,
    /// Not drawn. Closed land rings, used as a mask to cut the ocean wash --
    /// see `scene::draw`.
    Land,
    /// Administrative outlines. Not in the basemap -- planetiler's default
    /// profile drops them -- so they arrive as an overlay.
    Boundary,
    /// Footprints to extrude. `rank` carries the height in metres, so no extra
    /// field is needed on Feature.
    Building,
}

pub const LAYER_COUNT: usize = 12;

/// Back-to-front paint order. Whatever is listed later wins the depth buffer,
/// so this doubles as the z-ordering.
pub const DRAW_ORDER: [Layer; 8] = [
    Layer::Landuse,
    Layer::Water,
    Layer::RoadMinor,
    Layer::RoadMedium,
    Layer::Rail,
    Layer::Boundary,
    Layer::Coast,
    Layer::RoadMajor,
];

impl Layer {
    pub fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            0 => Layer::Landuse,
            1 => Layer::Water,
            2 => Layer::Coast,
            3 => Layer::Rail,
            4 => Layer::RoadMinor,
            5 => Layer::RoadMedium,
            6 => Layer::RoadMajor,
            7 => Layer::Place,
            8 => Layer::Landmark,
            9 => Layer::Land,
            10 => Layer::Boundary,
            11 => Layer::Building,
            _ => return None,
        })
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            Layer::Landuse => "landuse",
            Layer::Water => "water",
            Layer::Coast => "coastline",
            Layer::Rail => "railway",
            Layer::RoadMinor => "minor road",
            Layer::RoadMedium => "secondary",
            Layer::RoadMajor => "primary road",
            Layer::Place => "place",
            Layer::Landmark => "landmark",
            Layer::Land => "land",
            Layer::Boundary => "state border",
            Layer::Building => "building",
        }
    }

    pub fn is_point(self) -> bool {
        matches!(self, Layer::Place | Layer::Landmark)
    }
}

pub struct Feature {
    pub layer: Layer,
    pub rank: u16,
    pub closed: bool,
    pub name: Option<Box<str>>,
    /// World coords, projected once at load.
    pub pts: Vec<[f64; 2]>,
    /// minx, miny, maxx, maxy in world coords, for viewport culling.
    pub bbox: [f64; 4],
}

impl Feature {
    pub fn new(
        layer: Layer,
        rank: u16,
        closed: bool,
        name: Option<Box<str>>,
        pts: Vec<[f64; 2]>,
    ) -> Self {
        Feature { layer, rank, closed, name, bbox: Self::compute_bbox(&pts), pts }
    }

    fn compute_bbox(pts: &[[f64; 2]]) -> [f64; 4] {
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for p in pts {
            b[0] = b[0].min(p[0]);
            b[1] = b[1].min(p[1]);
            b[2] = b[2].max(p[0]);
            b[3] = b[3].max(p[1]);
        }
        b
    }

    #[inline]
    pub fn visible_in(&self, bounds: &[f64; 4]) -> bool {
        self.bbox[0] <= bounds[2]
            && self.bbox[2] >= bounds[0]
            && self.bbox[1] <= bounds[3]
            && self.bbox[3] >= bounds[1]
    }
}

/// A batch of features that are loaded and dropped together.
///
/// Both sources produce these: a `.tmap` file becomes one big tile covering
/// everything, a PMTiles archive produces one per z/x/y. Everything downstream
/// works the same way for either.
pub struct Tile {
    pub features: Vec<Feature>,
    /// Feature indices bucketed by layer, so a frame walks only what it needs.
    pub by_layer: [Vec<u32>; LAYER_COUNT],
}

impl Tile {
    pub fn new(features: Vec<Feature>) -> Self {
        let mut by_layer: [Vec<u32>; LAYER_COUNT] = Default::default();
        for (i, f) in features.iter().enumerate() {
            by_layer[f.layer.index()].push(i as u32);
        }
        // Higher rank draws last within a layer, so important roads sit on top
        // of the unimportant ones they overlap.
        for bucket in by_layer.iter_mut() {
            bucket.sort_by_key(|&i| features[i as usize].rank);
        }
        Tile { features, by_layer }
    }

}

pub struct MapData {
    pub tile: Tile,
    pub source: String,
}

impl MapData {
    /// Load `path` if it exists, otherwise fall back to the embedded sample.
    pub fn load(path: Option<&str>) -> Self {
        if let Some(p) = path {
            match std::fs::read_to_string(p) {
                Ok(text) => {
                    let mut d = Self::parse(&text);
                    d.source = p.to_string();
                    return d;
                }
                Err(e) => {
                    eprintln!("termap: could not read {p}: {e} -- using embedded sample");
                }
            }
        }
        for candidate in ["data/mumbai.tmap", "../data/mumbai.tmap"] {
            if let Ok(text) = std::fs::read_to_string(candidate) {
                let mut d = Self::parse(&text);
                d.source = candidate.to_string();
                return d;
            }
        }
        let mut d = Self::parse(SAMPLE);
        d.source = "embedded sample".to_string();
        d
    }

    pub fn parse(text: &str) -> Self {
        MapData { tile: Tile::new(parse_features(text)), source: String::new() }
    }
}

pub fn parse_features(text: &str) -> Vec<Feature> {
        let mut features = Vec::new();
        let mut lines = text.lines();

        while let Some(line) = lines.next() {
            let line = line.trim_end();
            if !line.starts_with("F ") {
                continue;
            }
            let Some(header) = parse_header(line) else { continue };
            let Some(coords) = lines.next() else { break };

            let mut pts = Vec::with_capacity(header.npts);
            let mut nums = coords.split_ascii_whitespace();
            while let (Some(a), Some(b)) = (nums.next(), nums.next()) {
                let (Ok(lon), Ok(lat)) = (a.parse::<f64>(), b.parse::<f64>()) else {
                    continue;
                };
                pts.push(lonlat_to_world(lon, lat));
            }
            if pts.is_empty() {
                continue;
            }

            features.push(Feature {
                layer: header.layer,
                rank: header.rank,
                closed: header.closed,
                name: header.name,
                bbox: Feature::compute_bbox(&pts),
                pts,
            });
        }

    features
}

impl MapData {
    /// Bounds worth opening on: the middle 80% of features by position.
    ///
    /// Full extent is the wrong answer. A city extract reaches whatever happens
    /// to clip the bbox -- one motorway running off to the next district drags
    /// the fit two zoom levels out and opens the map on mostly empty sea. Water
    /// and land rings are skipped outright, since both are drawn well past the
    /// shoreline by construction.
    pub fn bounds(&self) -> [f64; 4] {
        let mut xs: Vec<f64> = Vec::new();
        let mut ys: Vec<f64> = Vec::new();
        for f in &self.tile.features {
            if matches!(f.layer, Layer::Water | Layer::Land) {
                continue;
            }
            xs.push((f.bbox[0] + f.bbox[2]) * 0.5);
            ys.push((f.bbox[1] + f.bbox[3]) * 0.5);
        }
        if xs.is_empty() {
            return [0.0, 0.0, 1.0, 1.0];
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pick = |v: &[f64], q: f64| v[((v.len() - 1) as f64 * q) as usize];
        [
            pick(&xs, 0.10),
            pick(&ys, 0.10),
            pick(&xs, 0.90),
            pick(&ys, 0.90),
        ]
    }

}

struct Header {
    layer: Layer,
    rank: u16,
    closed: bool,
    npts: usize,
    name: Option<Box<str>>,
}

fn parse_header(line: &str) -> Option<Header> {
    // "F <layer> <rank> <closed> <npts> <name with spaces>"
    let mut it = line[2..].splitn(5, ' ');
    let layer = Layer::from_id(it.next()?.parse().ok()?)?;
    let rank = it.next()?.parse().ok()?;
    let closed = it.next()? == "1";
    let npts = it.next()?.parse().ok()?;
    let name = it.next().map(str::trim).filter(|s| !s.is_empty());
    Some(Header {
        layer,
        rank,
        closed,
        npts,
        name: name.map(Box::from),
    })
}
