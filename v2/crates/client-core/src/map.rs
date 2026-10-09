use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use portfolio_v2_scene::{ArtCell, CellArt, Detail, DetailClass, Primitive, Rgba8, VisualScene};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use termap::{
    canvas::{
        Canvas, CellDetail, Fog, Ground, RoadGlyph, Theme, DETAIL_GEOMETRY_PICK, DETAIL_MAP_LABEL,
        DETAIL_MAP_MARKER,
    },
    data::Tile,
    geo::{lonlat_to_world, Viewport as MapViewport},
    relief::{Plot, Relief},
    scene::{PlaceMarker, SceneOpts},
    style::{DepthField, FocusMode},
    terrain::Terrain,
};

use crate::{MapDemand, Viewport};

const PLACES: [Place; 5] = [
    Place {
        slug: "knowledge-high-school",
        name: "Knowledge High School",
        kind: "school",
        where_: "Kapadwanj, Gujarat",
        years: "— 2019",
        role: "Where it started",
        note: "A small town two hours out of Ahmedabad, and where I started taking things apart to see how they worked. Computers, machines, anything with a cover that came off. None of it was coursework.",
        lon: 73.070,
        lat: 23.020,
        zoom: 11.6,
        tilt: 46.0,
        bearing: 0.0,
    },
    Place {
        slug: "manjushree-research-institute",
        name: "Manjushree Research Institute",
        kind: "the wrong turn",
        where_: "Gandhinagar, Gujarat",
        years: "2020 — 2021",
        role: "Ayurvedic medicine",
        note: "I started college on a completely different path. It took a year to admit that the thing I wanted to do was the thing I had been doing for fun the whole time, and to start again. It is on the map because the route was not straight.",
        lon: 72.683_749,
        lat: 23.326_135,
        zoom: 11.8,
        tilt: 44.0,
        bearing: 0.0,
    },
    Place {
        slug: "silver-oak-university",
        name: "Silver Oak University",
        kind: "university",
        where_: "Ahmedabad, Gujarat",
        years: "2021 — 2025",
        role: "B.Tech, AI & Machine Learning",
        note: "Where I fell for Linux and the shell. The degree said AI and machine learning. What I actually learned was Python, the terminal, and how the machine underneath works. Graduated May 2025 with a 9.1.",
        lon: 72.534_691,
        lat: 23.090_453,
        zoom: 12.0,
        tilt: 50.0,
        bearing: 0.0,
    },
    Place {
        slug: "innoventa-technologies",
        name: "Innoventa Technologies",
        kind: "internship",
        where_: "Ahmedabad, Gujarat",
        years: "2024",
        role: "ML / AI Intern",
        note: "Built a GenAI book-reading application end to end. Content in one side, illustrated picture books with generated audio narration out the other. The first generative pipeline I had built that had to hold together for somebody other than me.",
        lon: 72.546,
        lat: 23.023,
        zoom: 12.0,
        tilt: 52.0,
        bearing: 0.0,
    },
    Place {
        slug: "gateway-corp",
        name: "Gateway Corp",
        kind: "work",
        where_: "Ahmedabad, Gujarat",
        years: "2025 — present",
        role: "SDE 1",
        note: "The first full-time engineering role. Production systems, real constraints, and the point where software stopped being coursework and became responsibility.",
        lon: 72.512_934,
        lat: 23.038_583,
        zoom: 12.0,
        tilt: 54.0,
        bearing: 0.0,
    },
];

#[derive(Clone, Copy)]
struct Place {
    slug: &'static str,
    name: &'static str,
    kind: &'static str,
    where_: &'static str,
    years: &'static str,
    role: &'static str,
    note: &'static str,
    lon: f64,
    lat: f64,
    zoom: f64,
    tilt: f64,
    bearing: f64,
}

impl Place {
    fn world(self) -> [f64; 2] {
        lonlat_to_world(self.lon, self.lat)
    }
}
#[derive(Clone, Copy, Debug)]
struct CameraStop {
    lon: f64,
    lat: f64,
    zoom: f64,
    tilt: f64,
    bearing: f64,
}
impl CameraStop {
    fn world(self) -> [f64; 2] {
        lonlat_to_world(self.lon, self.lat)
    }
}
impl From<Place> for CameraStop {
    fn from(p: Place) -> Self {
        Self {
            lon: p.lon,
            lat: p.lat,
            zoom: p.zoom,
            tilt: p.tilt,
            bearing: p.bearing,
        }
    }
}

const RHO: f64 = 1.42;
const MIN_TILE_ZOOM: u8 = 5;
const MAX_TILE_ZOOM: u8 = 14;
const SPEED: f64 = 2.6;
const MIN_FLIGHT: f64 = 1.5;
const MAX_FLIGHT: f64 = 4.5;
const SETTLE: f64 = 1.15;
const OPEN_STRETCH: f64 = 1.7;
const OPEN_MAX: f64 = 6.0;
const LEVEL_BY: f64 = 0.35;

#[derive(Clone, Copy)]
struct Flight {
    start: [f64; 2],
    delta: [f64; 2],
    width_start: f64,
    width_end: f64,
    distance: f64,
    r0: f64,
    length: f64,
}

impl Flight {
    fn new(start: [f64; 2], width_start: f64, end: [f64; 2], width_end: f64) -> Self {
        let delta = [end[0] - start[0], end[1] - start[1]];
        let distance = (delta[0] * delta[0] + delta[1] * delta[1]).sqrt();
        if distance < width_start * 1e-3 {
            return Self {
                start,
                delta,
                width_start,
                width_end,
                distance: 0.0,
                r0: 0.0,
                length: (width_end / width_start).ln().abs() / RHO,
            };
        }
        let rho2 = RHO * RHO;
        let rho4 = rho2 * rho2;
        let width_delta = width_end * width_end - width_start * width_start;
        let b0 = (width_delta + rho4 * distance * distance) / (2.0 * width_start * rho2 * distance);
        let b1 = (width_delta - rho4 * distance * distance) / (2.0 * width_end * rho2 * distance);
        let r0 = ((b0 * b0 + 1.0).sqrt() - b0).ln();
        let r1 = ((b1 * b1 + 1.0).sqrt() - b1).ln();
        Self {
            start,
            delta,
            width_start,
            width_end,
            distance,
            r0,
            length: (r1 - r0) / RHO,
        }
    }

    fn duration(self) -> f64 {
        (self.length / SPEED).clamp(MIN_FLIGHT, MAX_FLIGHT)
    }

    fn at(self, progress: f64) -> ([f64; 2], f64) {
        let progress = progress.clamp(0.0, 1.0);
        if self.distance == 0.0 {
            let width = self.width_start * (self.width_end / self.width_start).powf(progress);
            return (
                [
                    self.start[0] + self.delta[0] * progress,
                    self.start[1] + self.delta[1] * progress,
                ],
                width,
            );
        }
        let distance = progress * self.length;
        let cosh_r0 = self.r0.cosh();
        let x = RHO * distance + self.r0;
        let travelled =
            self.width_start / (RHO * RHO * self.distance) * (cosh_r0 * x.tanh() - self.r0.sinh());
        let width = self.width_start * cosh_r0 / x.cosh();
        (
            [
                self.start[0] + self.delta[0] * travelled,
                self.start[1] + self.delta[1] * travelled,
            ],
            width,
        )
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Rest,
    Flying,
    Settling,
}

#[derive(Clone)]
struct Tour {
    stops: Vec<CameraStop>,
    at: usize,
    shown: usize,
    phase: Phase,
    elapsed: f64,
    duration: f64,
    flight: Option<Flight>,
    from_tilt: f64,
    from_bearing: f64,
}
impl Default for Tour {
    fn default() -> Self {
        Self {
            stops: PLACES.into_iter().map(CameraStop::from).collect(),
            at: 0,
            shown: 0,
            phase: Phase::Rest,
            flight: None,
            elapsed: 0.0,
            duration: 0.0,
            from_tilt: 0.0,
            from_bearing: 0.0,
        }
    }
}

impl Tour {
    fn moving(&self) -> bool {
        self.phase != Phase::Rest
    }

    fn go(&mut self, viewport: &MapViewport, at: usize) {
        let Some(place) = self.stops.get(at).copied() else {
            return;
        };
        let target_width = viewport.sw / (256.0 * 2f64.powf(place.zoom));
        let flight = Flight::new(
            viewport.center,
            viewport.sw / viewport.scale(),
            place.world(),
            target_width,
        );
        self.shown = self.at;
        self.at = at;
        self.flight = Some(flight);
        self.duration = flight.duration();
        self.elapsed = 0.0;
        self.phase = Phase::Flying;
        self.from_tilt = viewport.tilt;
        self.from_bearing = viewport.bearing;
    }

    fn open(&mut self, viewport: &MapViewport, at: usize) {
        self.go(viewport, at);
        self.duration = (self.duration * OPEN_STRETCH).min(OPEN_MAX);
        self.shown = usize::MAX;
    }

    fn next(&mut self, viewport: &MapViewport) {
        self.go(viewport, (self.at + 1) % self.stops.len());
    }

    fn previous(&mut self, viewport: &MapViewport) {
        self.go(
            viewport,
            (self.at + self.stops.len() - 1) % self.stops.len(),
        );
    }

    fn tick(&mut self, seconds: f64, viewport: &mut MapViewport) {
        if self.phase == Phase::Rest {
            return;
        }
        self.elapsed += seconds;
        let place = self.stops[self.at];
        let progress = (self.elapsed / self.duration.max(1e-6)).clamp(0.0, 1.0);
        match self.phase {
            Phase::Flying => {
                let (center, width) = self.flight.expect("flight state").at(ease(progress));
                viewport.center = center;
                viewport.zoom = (viewport.sw / (256.0 * width.max(1e-12)))
                    .log2()
                    .clamp(termap::geo::MIN_ZOOM, termap::geo::MAX_ZOOM);
                let level = 1.0 - ease((progress / LEVEL_BY).min(1.0));
                viewport.tilt = self.from_tilt * level;
                viewport.bearing = angle_lerp(
                    self.from_bearing,
                    place.bearing.to_radians(),
                    ease(progress),
                );
                viewport.persp = 0.0;
                if progress >= 1.0 {
                    self.phase = Phase::Settling;
                    self.elapsed = 0.0;
                    self.duration = SETTLE;
                    self.from_tilt = viewport.tilt;
                    self.from_bearing = viewport.bearing;
                }
            }
            Phase::Settling => {
                let eased = ease(progress);
                let target_tilt = place.tilt.to_radians();
                viewport.tilt = self.from_tilt + (target_tilt - self.from_tilt) * eased;
                viewport.bearing = angle_lerp(self.from_bearing, place.bearing.to_radians(), eased);
                let lean = if target_tilt > 1e-6 {
                    (viewport.tilt / target_tilt).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                viewport.persp = termap::view::auto_persp(viewport.zoom) * lean;
                if progress >= 1.0 {
                    self.phase = Phase::Rest;
                    self.shown = self.at;
                }
            }
            Phase::Rest => {}
        }
    }

    fn card(&self) -> Option<(usize, f32)> {
        match self.phase {
            Phase::Rest => Some((self.at, 1.0)),
            Phase::Settling => Some((self.at, ease(self.elapsed / self.duration) as f32)),
            Phase::Flying => {
                let progress = (self.elapsed / self.duration.max(1e-6)).clamp(0.0, 1.0);
                if progress < 0.20 {
                    (self.shown < self.stops.len())
                        .then_some((self.shown, (1.0 - ease(progress / 0.20)) as f32))
                } else if progress > 0.62 {
                    Some((self.at, ease((progress - 0.62) / (1.0 - 0.62)) as f32))
                } else {
                    None
                }
            }
        }
    }
}

fn ease(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    value * value * value * (value * (value * 6.0 - 15.0) + 10.0)
}

fn angle_lerp(from: f64, to: f64, progress: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    let delta = (to - from).rem_euclid(TAU);
    let delta = if delta > PI { delta - TAU } else { delta };
    from + delta * progress
}

#[derive(Clone)]
pub struct MapState {
    camera: MapViewport,
    tour: Tour,
    opened: bool,
    tiles: Vec<(u8, u32, u32, std::sync::Arc<Tile>)>,
    pub overlay: Option<Tile>,
    terrain: Option<Rc<Terrain>>,
    buildings: Option<Tile>,
    relief: Rc<RefCell<Relief>>,
    show_terrain: bool,
    show_labels: bool,
    mono: bool,
    focus: FocusMode,
    road_glyph: RoadGlyph,
    road_weight: f64,
    auto_view: bool,
    search: Option<String>,
    results: Vec<portfolio_v2_protocol::map::SearchResult>,
    search_at: usize,
    hover: Option<String>,
    custom_title: Option<String>,
    places: Vec<portfolio_v2_protocol::Place>,
}

impl std::fmt::Debug for MapState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MapState")
            .field("at", &self.tour.at)
            .field("zoom", &self.camera.zoom)
            .field("tiles", &self.tiles.len())
            .field("overlay", &self.overlay.is_some())
            .field("terrain", &self.terrain.is_some())
            .finish()
    }
}

impl Default for MapState {
    fn default() -> Self {
        let place = PLACES[0];
        let mut camera = MapViewport::new(place.world(), place.zoom);
        camera.full_view = true;
        camera.tilt = place.tilt.to_radians();
        camera.bearing = place.bearing.to_radians();
        camera.persp = termap::view::auto_persp(camera.zoom);
        Self {
            camera,
            tour: Tour::default(),
            opened: false,
            tiles: Vec::new(),
            overlay: None,
            terrain: None,
            buildings: None,
            relief: Rc::new(RefCell::new(Relief::default())),
            show_terrain: true,
            show_labels: true,
            mono: false,
            focus: FocusMode::Subtle,
            road_glyph: RoadGlyph::Dotted,
            road_weight: 1.0,
            auto_view: false,
            search: None,
            results: Vec::new(),
            search_at: 0,
            hover: None,
            custom_title: None,
            places: PLACES
                .iter()
                .map(|p| portfolio_v2_protocol::Place {
                    id: p.slug.into(),
                    name: p.name.into(),
                    kind: p.kind.into(),
                    where_: p.where_.into(),
                    years: p.years.into(),
                    role: p.role.into(),
                    lonlat: (p.lon, p.lat),
                    world: p.world(),
                    zoom: p.zoom,
                    tilt: p.tilt.to_radians(),
                    bearing: p.bearing.to_radians(),
                    note: p.note.into(),
                })
                .collect(),
        }
    }
}

impl MapState {
    pub fn camera_snapshot(&self) -> [f64; 6] {
        let (lon, lat) = termap::geo::world_to_lonlat(self.camera.center[0], self.camera.center[1]);
        [
            lon,
            lat,
            self.camera.zoom,
            self.camera.tilt,
            self.camera.bearing,
            self.camera.persp,
        ]
    }
    pub fn navigation_snapshot(&self) -> (usize, Option<String>) {
        (self.tour.at, self.custom_title.clone())
    }
    pub fn restore_navigation(&mut self, index: usize, title: Option<String>) {
        if index < self.places.len() {
            self.tour.at = index;
            self.tour.shown = index;
        }
        self.custom_title = title.map(|s| s.chars().take(128).collect());
    }
    pub fn restore_camera(&mut self, camera: [f64; 6]) {
        if camera.iter().all(|v| v.is_finite())
            && (-180.0..=180.0).contains(&camera[0])
            && (-85.0..=85.0).contains(&camera[1])
        {
            self.camera.center = lonlat_to_world(camera[0], camera[1]);
            self.camera.zoom = camera[2].clamp(2.0, 20.0);
            self.camera.tilt = camera[3].clamp(0.0, 1.2);
            self.camera.bearing = camera[4].rem_euclid(std::f64::consts::TAU);
            self.camera.persp = camera[5].clamp(0.0, 1.0);
            self.tour.phase = Phase::Rest;
            self.opened = true;
        }
    }
    pub fn set_places(&mut self, places: &[portfolio_v2_protocol::Place]) {
        if places.is_empty() {
            return;
        }
        self.places = places.to_vec();
        self.tour.stops = places
            .iter()
            .map(|p| CameraStop {
                lon: p.lonlat.0,
                lat: p.lonlat.1,
                zoom: p.zoom,
                tilt: p.tilt.to_degrees(),
                bearing: p.bearing.to_degrees(),
            })
            .collect();
        self.tour.at = self.tour.at.min(self.places.len() - 1);
        self.tour.shown = self.tour.at;
        self.tour.phase = Phase::Rest;
        self.opened = false;
    }
    pub fn set_buildings(&mut self, tile: Tile) {
        self.buildings = Some(tile);
    }
    pub fn present(&mut self, title: &str, lon: f64, lat: f64, zoom: f64) {
        self.custom_title = Some(title.into());
        self.camera.center = lonlat_to_world(lon, lat);
        self.camera.zoom = zoom;
        self.camera.tilt = 0.0;
        self.camera.persp = 0.0;
        self.opened = true;
        self.tour.phase = Phase::Rest;
    }
    pub fn search_query(&self) -> Option<String> {
        self.search.clone()
    }
    pub fn search_results(
        &mut self,
        query: &str,
        results: Vec<portfolio_v2_protocol::map::SearchResult>,
    ) {
        if self.search.as_deref() == Some(query) {
            self.results = results.into_iter().filter(|r| r.valid()).take(10).collect();
            self.search_at = 0;
        }
    }
    pub fn search_key(&mut self, key: &str) -> bool {
        if key == "?" && self.search.is_none() {
            self.search = Some(String::new());
            self.results = self
                .places
                .iter()
                .map(|p| portfolio_v2_protocol::map::SearchResult {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    lon: p.lonlat.0,
                    lat: p.lonlat.1,
                })
                .collect();
            return true;
        }
        let Some(query) = &mut self.search else {
            return false;
        };
        match key {
            "Escape" => {
                self.search = None;
            }
            "ArrowDown" => {
                self.search_at = (self.search_at + 1).min(self.results.len().saturating_sub(1))
            }
            "ArrowUp" => self.search_at = self.search_at.saturating_sub(1),
            "Enter" => {
                if let Some(result) = self.results.get(self.search_at) {
                    self.custom_title = Some(result.name.clone());
                    self.camera.center = lonlat_to_world(result.lon, result.lat);
                    self.camera.zoom = 14.0;
                    self.camera.tilt = 0.0;
                    self.camera.persp = 0.0;
                    self.tour.phase = Phase::Rest;
                    self.search = None;
                }
            }
            "Backspace" => {
                query.pop();
                self.search_at = 0;
                self.results.clear();
            }
            _ if key.chars().count() == 1
                && !key.chars().any(char::is_control)
                && query.len() + key.len() <= 128 =>
            {
                query.push_str(key);
                self.search_at = 0;
                self.results.clear();
            }
            _ => (),
        }
        true
    }
    pub fn local_search(&self, query: &str) -> Vec<portfolio_v2_protocol::map::SearchResult> {
        let query = query.to_lowercase();
        let mut results = Vec::new();
        let mut seen = BTreeSet::new();
        for place in &self.places {
            if place.name.to_lowercase().contains(&query) {
                results.push(portfolio_v2_protocol::map::SearchResult {
                    id: place.id.clone(),
                    name: place.name.clone(),
                    lon: place.lonlat.0,
                    lat: place.lonlat.1,
                });
            }
        }
        for (_, _, _, tile) in &self.tiles {
            for feature in &tile.features {
                let Some(name) = &feature.name else { continue };
                if !name.to_lowercase().contains(&query) || !seen.insert(name.to_string()) {
                    continue;
                }
                let point = [
                    (feature.bbox[0] + feature.bbox[2]) * 0.5,
                    (feature.bbox[1] + feature.bbox[3]) * 0.5,
                ];
                let (lon, lat) = termap::geo::world_to_lonlat(point[0], point[1]);
                results.push(portfolio_v2_protocol::map::SearchResult {
                    id: format!("{:016x}", feature.stable_id),
                    name: name.to_string(),
                    lon,
                    lat,
                });
                if results.len() >= 10 {
                    return results;
                }
            }
        }
        results
    }
    pub fn hover(&mut self, point: [f64; 2]) -> bool {
        let mut nearest = 8.0f64;
        let mut found = None;
        for (_, _, _, tile) in &self.tiles {
            for feature in &tile.features {
                let Some(name) = &feature.name else { continue };
                for vertex in feature.pts.iter().step_by((feature.pts.len() / 64).max(1)) {
                    let p = self.camera.project(*vertex);
                    let distance = (point[0] - p[0]).hypot(point[1] - p[1]);
                    if distance < nearest {
                        nearest = distance;
                        found = Some(name.to_string());
                    }
                }
            }
        }
        if self.hover == found {
            false
        } else {
            self.hover = found;
            true
        }
    }
    pub fn description(&self) -> String {
        if let Some(title) = &self.custom_title {
            return title.clone();
        }
        let Some(place) = self.places.get(self.tour.shown) else {
            return "Experience tour: five places in India, from school in Kapadwanj to engineering work in Ahmedabad.".into();
        };
        format!(
            "{} — {}, {}. {}. {}",
            place.name, place.where_, place.years, place.role, place.note
        )
    }
    pub fn pixel_masks(&self, viewport: Viewport) -> Vec<[u16; 4]> {
        let gutter = if viewport.cols >= 90 { 7 } else { 0 };
        let width = viewport.cols.saturating_sub(gutter);
        let height = viewport.rows.saturating_sub(3);
        let mut masks = Vec::new();
        if let Some((at, _)) = self.tour.card().filter(|_| self.custom_title.is_none()) {
            if width >= 34 && height >= 12 {
                let lines = wrap(
                    &self.places[at].note,
                    width.saturating_sub(6).min(74) as usize,
                )
                .len();
                let lines = lines.min(((height / 3).saturating_sub(4)).clamp(1, 5) as usize);
                masks.push([
                    gutter + 2,
                    1,
                    width.saturating_sub(4).min(76),
                    (7 + lines as u16).min(height),
                ]);
            }
        }
        if let Some(bar) = scalebar(&self.camera, width, height) {
            masks.push([
                gutter + bar.rect.x,
                1 + bar.rect.y,
                bar.rect.width,
                bar.rect.height,
            ]);
        }
        masks
    }
    /// Experimental scene-native mesh. World geometry and the same elevation
    /// sampler feed real triangles; no terminal glyphs are used as terrain.
    pub fn pixel_mesh(&self, viewport: Viewport) -> Vec<f32> {
        use termap::geo::{meters_per_world_unit, world_to_lonlat};
        let vp = self.camera;
        let bounds = vp.world_bounds(16.0);
        let gutter = if viewport.cols >= 90 { 7.0 } else { 0.0 };
        let meters = meters_per_world_unit(world_to_lonlat(vp.center[0], vp.center[1]).1);
        let sample = |point: [f64; 2]| {
            if !self.show_terrain {
                return 0.0;
            }
            let (lon, lat) = world_to_lonlat(point[0], point[1]);
            self.terrain.as_ref().map_or(0.0, |t| {
                t.sample_smooth(lon, lat, (meters / vp.scale()).max(20.0))
            }) as f64
        };
        let datum = sample(vp.center);
        let height = |p: [f64; 2]| (sample(p) - datum) / meters;
        let mut out = Vec::new();
        let mut emit =
            |point: [f64; 2], altitude: f64, normal: [f32; 3], color: [f32; 3], bias: f32| {
                // Triangle-aligned ~7 MiB ceiling for the experimental transfer.
                if out.len() >= 131_070 * 12 {
                    return false;
                }
                let (screen, depth) = vp.project3(point, altitude);
                if !depth.is_finite() || !screen[0].is_finite() || !screen[1].is_finite() {
                    return false;
                }
                let x = (gutter + screen[0] / 2.0) / viewport.cols as f64 * 2.0 - 1.0;
                let y = 1.0 - (1.0 + screen[1] / 4.0) / viewport.rows as f64 * 2.0;
                out.extend_from_slice(&[
                    x as f32,
                    y as f32,
                    (depth + bias).clamp(0.001, 0.999),
                    normal[0],
                    normal[1],
                    normal[2],
                    color[0],
                    color[1],
                    color[2],
                    ((point[0] - bounds[0]) / (bounds[2] - bounds[0]).max(1e-12)) as f32,
                    ((point[1] - bounds[1]) / (bounds[3] - bounds[1]).max(1e-12)) as f32,
                    (altitude / (bounds[2] - bounds[0]).max(1e-12)) as f32,
                ]);
                true
            };
        const GRID: usize = 64;
        let sx = (bounds[2] - bounds[0]) / GRID as f64;
        let sy = (bounds[3] - bounds[1]) / GRID as f64;
        let mut heights = vec![0.0; (GRID + 1) * (GRID + 1)];
        for y in 0..=GRID {
            for x in 0..=GRID {
                heights[y * (GRID + 1) + x] =
                    height([bounds[0] + x as f64 * sx, bounds[1] + y as f64 * sy]);
            }
        }
        for y in 0..GRID {
            for x in 0..GRID {
                let corners = [[x, y], [x + 1, y], [x, y + 1], [x + 1, y + 1]];
                let points = corners.map(|[x, y]| {
                    (
                        [bounds[0] + x as f64 * sx, bounds[1] + y as f64 * sy],
                        heights[y * (GRID + 1) + x],
                    )
                });
                for triangle in [[0, 1, 2], [2, 1, 3]] {
                    let [a, b, c] = triangle.map(|i| points[i]);
                    let u = [b.0[0] - a.0[0], b.0[1] - a.0[1], b.1 - a.1];
                    let v = [c.0[0] - a.0[0], c.0[1] - a.0[1], c.1 - a.1];
                    let nz = (u[0] * v[1] - u[1] * v[0]).max(1e-24);
                    let normal = [
                        ((u[1] * v[2] - u[2] * v[1]) / nz) as f32,
                        ((u[2] * v[0] - u[0] * v[2]) / nz) as f32,
                        1.0,
                    ];
                    let color = if a.1 * meters + datum > 1500.0 {
                        [0.49, 0.48, 0.38]
                    } else {
                        [0.33, 0.44, 0.29]
                    };
                    if [a, b, c]
                        .iter()
                        .all(|(point, h)| vp.project3(*point, *h).1.is_finite())
                    {
                        for (point, h) in [a, b, c] {
                            emit(point, h, normal, color, 0.0);
                        }
                    }
                }
            }
        }
        // Roads and water use the archive's real paths, draped on that terrain.
        for tile in self
            .tiles
            .iter()
            .map(|(_, _, _, tile)| tile.as_ref())
            .chain(self.buildings.iter())
            .chain(self.overlay.iter())
        {
            for feature in &tile.features {
                if !feature.visible_in(&bounds) {
                    continue;
                }
                if matches!(
                    feature.layer,
                    termap::data::Layer::Building | termap::data::Layer::Water
                ) && feature.closed
                    && feature.pts.len() >= 3
                {
                    let vertices = feature
                        .pts
                        .iter()
                        .flat_map(|p| p.iter().copied())
                        .collect::<Vec<_>>();
                    let indices = earcutr::earcut(&vertices, &[], 2).unwrap_or_default();
                    let building = feature.layer == termap::data::Layer::Building;
                    let rise = if building {
                        feature.rank.clamp(3, 120) as f64 / meters
                    } else {
                        0.0
                    };
                    let base = height([
                        (feature.bbox[0] + feature.bbox[2]) * 0.5,
                        (feature.bbox[1] + feature.bbox[3]) * 0.5,
                    ]);
                    for triangle in indices.chunks_exact(3) {
                        if triangle
                            .iter()
                            .all(|&i| vp.project3(feature.pts[i], base + rise).1.is_finite())
                        {
                            for &i in triangle {
                                emit(
                                    feature.pts[i],
                                    base + rise,
                                    [0.0, 0.0, 1.0],
                                    if building {
                                        [0.65, 0.61, 0.49]
                                    } else {
                                        [0.15, 0.37, 0.53]
                                    },
                                    if building { -0.006 } else { -0.002 },
                                );
                            }
                        }
                    }
                    if building {
                        for edge in feature.pts.windows(2) {
                            let a = edge[0];
                            let b = edge[1];
                            let dx = b[0] - a[0];
                            let dy = b[1] - a[1];
                            let length = dx.hypot(dy).max(1e-12);
                            let normal = [(-dy / length) as f32, (dx / length) as f32, 0.0];
                            let face = [(a, base), (b, base), (a, base + rise), (b, base + rise)];
                            if face.iter().all(|(p, h)| vp.project3(*p, *h).1.is_finite()) {
                                for index in [0, 1, 2, 2, 1, 3] {
                                    emit(
                                        face[index].0,
                                        face[index].1,
                                        normal,
                                        [0.49, 0.45, 0.35],
                                        -0.004,
                                    );
                                }
                            }
                        }
                        continue;
                    }
                }
                let (color, pixels) = match feature.layer {
                    termap::data::Layer::RoadMajor => ([0.72, 0.64, 0.42], 1.4),
                    termap::data::Layer::RoadMedium => ([0.64, 0.60, 0.46], 1.0),
                    termap::data::Layer::RoadMinor => ([0.48, 0.48, 0.39], 0.6),
                    termap::data::Layer::Rail => ([0.29, 0.31, 0.30], 0.6),
                    termap::data::Layer::Water => ([0.15, 0.37, 0.53], 1.6),
                    termap::data::Layer::Boundary => ([0.61, 0.57, 0.38], 0.8),
                    termap::data::Layer::Coast => ([0.43, 0.54, 0.45], 0.8),
                    _ => continue,
                };
                for segment in feature.pts.windows(2) {
                    let [a, b] = [segment[0], segment[1]];
                    let dx = b[0] - a[0];
                    let dy = b[1] - a[1];
                    let length = dx.hypot(dy);
                    if length < 1e-12 {
                        continue;
                    }
                    let weight = pixels * self.road_weight / vp.scale();
                    let side = [-dy / length * weight, dx / length * weight];
                    let quad = [
                        [a[0] + side[0], a[1] + side[1]],
                        [a[0] - side[0], a[1] - side[1]],
                        [b[0] + side[0], b[1] + side[1]],
                        [b[0] - side[0], b[1] - side[1]],
                    ];
                    let ha = height(a);
                    let hb = height(b);
                    if !quad
                        .iter()
                        .enumerate()
                        .all(|(i, p)| vp.project3(*p, if i < 2 { ha } else { hb }).1.is_finite())
                    {
                        continue;
                    }
                    for i in [0, 1, 2, 2, 1, 3] {
                        let p = quad[i];
                        emit(
                            p,
                            if i < 2 { ha } else { hb },
                            [0.0, 0.0, 1.0],
                            color,
                            -0.002,
                        );
                    }
                }
            }
        }
        out
    }
    pub fn pixel_heightfield(&self) -> Vec<f32> {
        use termap::geo::{meters_per_world_unit, world_to_lonlat};
        let vp = self.camera;
        let bounds = vp.world_bounds(16.0);
        let sx = (bounds[2] - bounds[0]).max(1e-12);
        let sy = (bounds[3] - bounds[1]).max(1e-12);
        let meters = meters_per_world_unit(world_to_lonlat(vp.center[0], vp.center[1]).1);
        let sample = |point: [f64; 2]| {
            if !self.show_terrain {
                return 0.0;
            }
            let (lon, lat) = world_to_lonlat(point[0], point[1]);
            self.terrain.as_ref().map_or(0.0, |t| {
                t.sample_smooth(lon, lat, (meters / vp.scale()).max(20.0))
            }) as f64
        };
        let datum = sample(vp.center);
        let mut out = Vec::with_capacity(65 * 65 + 1);
        out.push((sx / sy) as f32);
        for y in 0..65 {
            for x in 0..65 {
                let point = [
                    bounds[0] + x as f64 / 64.0 * sx,
                    bounds[1] + y as f64 / 64.0 * sy,
                ];
                out.push(((sample(point) - datum) / meters / sx) as f32);
            }
        }
        for tile in self
            .tiles
            .iter()
            .map(|(_, _, _, tile)| tile.as_ref())
            .chain(self.buildings.iter())
        {
            for feature in &tile.features {
                if feature.layer != termap::data::Layer::Building
                    || !feature.closed
                    || !feature.visible_in(&bounds)
                {
                    continue;
                }
                let x0 = ((feature.bbox[0] - bounds[0]) / sx * 64.0)
                    .floor()
                    .clamp(0.0, 64.0) as usize;
                let x1 = ((feature.bbox[2] - bounds[0]) / sx * 64.0)
                    .ceil()
                    .clamp(0.0, 64.0) as usize;
                let y0 = ((feature.bbox[1] - bounds[1]) / sy * 64.0)
                    .floor()
                    .clamp(0.0, 64.0) as usize;
                let y1 = ((feature.bbox[3] - bounds[1]) / sy * 64.0)
                    .ceil()
                    .clamp(0.0, 64.0) as usize;
                let center = [
                    (feature.bbox[0] + feature.bbox[2]) * 0.5,
                    (feature.bbox[1] + feature.bbox[3]) * 0.5,
                ];
                let roof = ((sample(center) - datum) / meters / sx
                    + feature.rank.clamp(3, 120) as f64 / meters / sx)
                    as f32;
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let p = [
                            bounds[0] + x as f64 / 64.0 * sx,
                            bounds[1] + y as f64 / 64.0 * sy,
                        ];
                        let mut inside = false;
                        if feature.pts.len() < 3 {
                            continue;
                        }
                        let mut previous = feature.pts[feature.pts.len() - 1];
                        for &point in &feature.pts {
                            if (point[1] > p[1]) != (previous[1] > p[1])
                                && p[0]
                                    < (previous[0] - point[0]) * (p[1] - point[1])
                                        / (previous[1] - point[1])
                                        + point[0]
                            {
                                inside = !inside;
                            }
                            previous = point;
                        }
                        if inside {
                            let at = 1 + y * 65 + x;
                            out[at] = out[at].max(roof);
                        }
                    }
                }
            }
        }
        out
    }

    pub fn resize(&mut self, viewport: Viewport) {
        let gutter = if viewport.cols >= 90 { 7 } else { 0 };
        self.camera.sw = viewport.cols.saturating_sub(gutter) as f64 * 2.0;
        self.camera.sh = viewport.rows.saturating_sub(3) as f64 * 4.0;
    }

    pub fn open(&mut self, viewport: Viewport) {
        self.resize(viewport);
        if self.opened {
            return;
        }
        let lo = lonlat_to_world(67.979_642_6, 36.486_596);
        let hi = lonlat_to_world(97.706_966_8, 5.637_858_1);
        self.camera.fit([lo[0], lo[1], hi[0], hi[1]]);
        self.camera.tilt = 0.0;
        self.camera.bearing = 0.0;
        self.camera.persp = 0.0;
        self.tour.open(&self.camera, 0);
        self.opened = true;
    }

    pub fn tick(&mut self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        let mut remaining = seconds.clamp(0.0, 10.0);
        // Dropping presentation frames must not put the camera in slow motion.
        while remaining > 0.0 {
            let step = remaining.min(0.1);
            self.tour.tick(step, &mut self.camera);
            remaining -= step;
        }
    }

    pub fn finish_animation(&mut self) {
        if self.tour.moving() {
            self.tour.tick(OPEN_MAX + MAX_FLIGHT, &mut self.camera);
            self.tour.tick(SETTLE, &mut self.camera);
        }
    }

    pub fn animating(&self) -> bool {
        self.tour.moving()
    }

    pub fn command(&mut self, command: crate::MapCommand) {
        if matches!(
            command,
            crate::MapCommand::Next | crate::MapCommand::Previous | crate::MapCommand::Replay
        ) {
            self.custom_title = None;
        }
        // Direct manipulation takes ownership from the authored camera flight.
        if matches!(
            command,
            crate::MapCommand::Pan(..)
                | crate::MapCommand::Drag(..)
                | crate::MapCommand::Zoom(..)
                | crate::MapCommand::ZoomAt(..)
                | crate::MapCommand::Tilt(..)
                | crate::MapCommand::Bearing(..)
        ) {
            self.tour.phase = Phase::Rest;
        }
        match command {
            crate::MapCommand::Next => self.tour.next(&self.camera),
            crate::MapCommand::Previous => self.tour.previous(&self.camera),
            crate::MapCommand::Replay => self.tour.go(&self.camera, self.tour.at),
            crate::MapCommand::Pan(x, y) => {
                let from = [self.camera.sw * 0.5, self.camera.sh * 0.5];
                let to = [from[0] - self.camera.sw * x, from[1] - self.camera.sh * y];
                self.camera.pan_screen(from, to);
            }
            crate::MapCommand::Drag(from, to) => self.camera.pan_screen(from, to),
            crate::MapCommand::Zoom(delta) => {
                self.zoom_at(delta, [self.camera.sw * 0.5, self.camera.sh * 0.5]);
            }
            crate::MapCommand::ZoomAt(delta, anchor) => {
                self.zoom_at(delta, anchor);
            }
            crate::MapCommand::Tilt(delta) => {
                self.auto_view = false;
                self.camera.tilt = (self.camera.tilt + delta).clamp(0.0, 1.20);
                self.camera.persp = if self.camera.tilt > 0.01 {
                    termap::view::auto_persp(self.camera.zoom)
                } else {
                    0.0
                };
            }
            crate::MapCommand::Bearing(delta) => {
                self.auto_view = false;
                self.camera.bearing += delta;
            }
            crate::MapCommand::ToggleCamera => {
                self.auto_view = !self.auto_view;
                if self.auto_view {
                    self.sync_camera();
                } else {
                    self.camera.tilt = 0.0;
                    self.camera.bearing = 0.0;
                    self.camera.persp = 0.0;
                }
            }
            crate::MapCommand::ToggleTerrain => self.show_terrain = !self.show_terrain,
            crate::MapCommand::ToggleLabels => self.show_labels = !self.show_labels,
            crate::MapCommand::ToggleColor => self.mono = !self.mono,
            crate::MapCommand::CycleFocus => {
                self.focus = match self.focus {
                    FocusMode::Off => FocusMode::Subtle,
                    FocusMode::Subtle => FocusMode::Strong,
                    FocusMode::Strong => FocusMode::Off,
                }
            }
            crate::MapCommand::CycleRoads => {
                self.road_glyph = match self.road_glyph {
                    RoadGlyph::Dotted => RoadGlyph::Block,
                    RoadGlyph::Block => RoadGlyph::Line,
                    RoadGlyph::Line => RoadGlyph::Dotted,
                }
            }
            crate::MapCommand::RoadWeight(delta) => {
                self.road_weight = (self.road_weight + delta).clamp(0.4, 2.5)
            }
        }
    }

    pub fn insert_tile(&mut self, z: u8, x: u32, y: u32, tile: Tile) {
        if let Some(existing) = self
            .tiles
            .iter_mut()
            .find(|(tz, tx, ty, _)| (*tz, *tx, *ty) == (z, x, y))
        {
            existing.3 = std::sync::Arc::new(tile);
        } else {
            self.tiles.push((z, x, y, std::sync::Arc::new(tile)));
            if self.tiles.len() > 256 {
                self.tiles.remove(0);
            }
        }
    }

    pub fn replace_tiles(&mut self, tiles: Vec<(u8, u32, u32, std::sync::Arc<Tile>)>) {
        self.tiles = tiles;
    }

    pub fn set_terrain(&mut self, terrain: Terrain) {
        self.terrain = Some(Rc::new(terrain));
    }

    pub fn needs_terrain(&self) -> bool {
        self.terrain.is_none()
    }

    pub fn zoom(&self) -> f64 {
        self.camera.zoom
    }

    pub fn set_mono(&mut self, mono: bool) {
        self.mono = mono;
    }

    pub fn tilt_degrees(&self) -> f64 {
        self.camera.tilt.to_degrees()
    }

    pub fn mode_label(&self) -> &'static str {
        termap::view::Mode::of(self.camera.zoom).label()
    }

    fn sync_camera(&mut self) {
        if self.auto_view {
            self.camera.tilt = termap::view::auto_tilt(self.camera.zoom);
        }
        self.camera.persp = if self.camera.tilt > 0.01 {
            termap::view::auto_persp(self.camera.zoom)
        } else {
            0.0
        };
    }

    fn zoom_at(&mut self, delta: f64, anchor: [f64; 2]) {
        let before = self.camera.unproject(anchor);
        self.camera.zoom_at(delta, anchor);
        self.sync_camera();
        let after = self.camera.unproject(anchor);
        self.camera.center[0] = (self.camera.center[0] + before[0] - after[0]).clamp(0.0, 1.0);
        self.camera.center[1] = (self.camera.center[1] + before[1] - after[1]).clamp(0.0, 1.0);
    }

    pub fn demand(&self, viewport: Viewport) -> MapDemand {
        demand_for(self.camera, viewport)
    }

    pub fn prefetch_demand(&self, viewport: Viewport) -> MapDemand {
        const LIMIT: usize = 256;
        const FLIGHT_SAMPLES: usize = 8;
        let mut seen = BTreeSet::new();
        let mut tiles = Vec::new();
        let mut add = |camera: MapViewport| {
            for tile in demand_for(camera, viewport).tiles {
                if seen.insert(tile) && tiles.len() < LIMIT {
                    tiles.push(tile);
                }
            }
        };

        // Stops are the priority: every place should be complete on arrival.
        for place in &self.tour.stops {
            let mut camera = MapViewport::new(place.world(), place.zoom);
            camera.tilt = place.tilt.to_radians();
            camera.bearing = place.bearing.to_radians();
            camera.persp = termap::view::auto_persp(camera.zoom);
            add(camera);
        }

        // The opening descent crosses every map scale and is where a cold tile
        // generation is most visible, so warm it more densely than city hops.
        let mut opening = MapViewport::new(self.tour.stops[0].world(), self.tour.stops[0].zoom);
        opening.resize_for(viewport);
        let lo = lonlat_to_world(67.979_642_6, 36.486_596);
        let hi = lonlat_to_world(97.706_966_8, 5.637_858_1);
        opening.fit([lo[0], lo[1], hi[0], hi[1]]);
        let target_width = opening.sw / (256.0 * 2f64.powf(self.tour.stops[0].zoom));
        let flight = Flight::new(
            opening.center,
            opening.sw / opening.scale(),
            self.tour.stops[0].world(),
            target_width,
        );
        for step in 0..=24 {
            let progress = ease(step as f64 / 24.0);
            let (center, width) = flight.at(progress);
            add(MapViewport::new(
                center,
                (opening.sw / (256.0 * width.max(1e-12))).log2(),
            ));
        }

        // Then warm representative views along every leg of the authored route.
        for pair in self.tour.stops.windows(2) {
            let from = pair[0];
            let to = pair[1];
            let mut start = MapViewport::new(from.world(), from.zoom);
            start.resize_for(viewport);
            let target_width = start.sw / (256.0 * 2f64.powf(to.zoom));
            let flight = Flight::new(
                start.center,
                start.sw / start.scale(),
                to.world(),
                target_width,
            );
            for step in 1..FLIGHT_SAMPLES {
                let progress = ease(step as f64 / FLIGHT_SAMPLES as f64);
                let (center, width) = flight.at(progress);
                let mut camera =
                    MapViewport::new(center, (start.sw / (256.0 * width.max(1e-12))).log2());
                camera.bearing =
                    angle_lerp(from.bearing.to_radians(), to.bearing.to_radians(), progress);
                add(camera);
            }
        }
        MapDemand { tiles }
    }
}

trait ResizeMapViewport {
    fn resize_for(&mut self, viewport: Viewport);
}

impl ResizeMapViewport for MapViewport {
    fn resize_for(&mut self, viewport: Viewport) {
        self.full_view = true;
        let gutter = if viewport.cols >= 90 { 7 } else { 0 };
        self.sw = viewport.cols.saturating_sub(gutter) as f64 * 2.0;
        self.sh = viewport.rows.saturating_sub(3) as f64 * 4.0;
    }
}

fn demand_for(mut camera: MapViewport, viewport: Viewport) -> MapDemand {
    camera.resize_for(viewport);
    let mut z = tile_zoom(camera.zoom);
    let bounds = camera.world_bounds(0.0);
    let tile_count = |z: u8| {
        let n = 1u32 << z;
        let x0 = (bounds[0] * n as f64).floor() as i64;
        let y0 = (bounds[1] * n as f64).floor() as i64;
        let x1 = (bounds[2] * n as f64).floor() as i64;
        let y1 = (bounds[3] * n as f64).floor() as i64;
        (x1 - x0 + 1).max(0) as usize * (y1 - y0 + 1).max(0) as usize
    };
    while tile_count(z) > 40 && z > MIN_TILE_ZOOM {
        z -= 1;
    }
    let n = 1u32 << z;
    let x0 = (bounds[0] * n as f64).floor() as i64;
    let y0 = (bounds[1] * n as f64).floor() as i64;
    let x1 = (bounds[2] * n as f64).floor() as i64;
    let y1 = (bounds[3] * n as f64).floor() as i64;
    let mut tiles = Vec::new();
    for y in y0..=y1 {
        if !(0..n as i64).contains(&y) {
            continue;
        }
        for x in x0..=x1 {
            tiles.push((z, x.rem_euclid(n as i64) as u32, y as u32));
        }
    }
    MapDemand { tiles }
}

fn tile_zoom(camera_zoom: f64) -> u8 {
    camera_zoom
        .floor()
        .clamp(MIN_TILE_ZOOM as f64, MAX_TILE_ZOOM as f64) as u8
}

fn active_tile_zoom(
    tiles: &[(u8, u32, u32, std::sync::Arc<Tile>)],
    bounds: [f64; 4],
    camera_zoom: f64,
) -> Option<u8> {
    let target = tile_zoom(camera_zoom);
    tiles
        .iter()
        .filter(|(z, x, y, _)| tile_intersects(*z, *x, *y, bounds))
        .filter(|(_, _, _, tile)| !tile.features.is_empty())
        .map(|(z, _, _, _)| *z)
        .min_by_key(|z| (z.abs_diff(target), *z > target))
}

pub fn render(scene: &mut VisualScene, state: &MapState) {
    render_inner(scene, state, false);
}
pub fn render_annotations(scene: &mut VisualScene, state: &MapState) {
    render_inner(scene, state, true);
}
fn render_inner(scene: &mut VisualScene, state: &MapState, annotations_only: bool) {
    let x = if scene.viewport.cols >= 90 { 7 } else { 0 };
    let area = Rect::new(
        x,
        1,
        scene.viewport.cols.saturating_sub(x),
        scene.viewport.rows.saturating_sub(3),
    );
    if area.width == 0 || area.height == 0 {
        return;
    }
    let local = Rect::new(0, 0, area.width, area.height);
    let theme = match scene.theme {
        portfolio_v2_scene::Theme::Dark => Theme::System(Ground {
            rgb: (8, 9, 11),
            dark: true,
        }),
        portfolio_v2_scene::Theme::Light => Theme::Paper,
    };
    let mut buffer = Buffer::empty(local);
    buffer.set_style(local, Style::default().bg(theme.page()));
    let mut vp = state.camera;
    vp.sw = area.width as f64 * 2.0;
    vp.sh = area.height as f64 * 4.0;
    let mut canvas = Canvas::new(area.width as usize, area.height as usize);
    let scalebar = scalebar(&vp, area.width, area.height);
    let ground = if state.show_terrain && !annotations_only {
        termap::view::ground_strength(vp.zoom)
    } else {
        0.0
    };
    let mut lift = termap::relief::Lift::default();
    let terrain = state.terrain.as_deref().and_then(|terrain| {
        if ground <= 0.0 {
            return None;
        }
        let mut relief = state.relief.borrow_mut();
        relief.draw_lowland(
            terrain,
            &mut canvas,
            &vp,
            Plot {
                strength: ground,
                theme,
            },
        );
        lift = relief.lift;
        Some(terrain as &dyn termap::scene::Elevation)
    });
    let visible_bounds = vp.world_bounds(32.0);
    let active_zoom = active_tile_zoom(&state.tiles, visible_bounds, vp.zoom);
    let mut tiles: Vec<&Tile> = state
        .tiles
        .iter()
        .filter(|(z, x, y, _)| {
            Some(*z) == active_zoom && tile_intersects(*z, *x, *y, visible_bounds)
        })
        .map(|(_, _, _, tile)| tile.as_ref())
        .collect();
    if let Some(overlay) = &state.overlay {
        tiles.push(overlay);
    }
    if let Some(buildings) = &state.buildings {
        tiles.push(buildings);
    }
    let markers: Vec<_> = state
        .places
        .iter()
        .map(|place| PlaceMarker {
            world: lonlat_to_world(place.lonlat.0, place.lonlat.1),
            detail: termap::data::stable_detail_id("authored-place", &place.id),
        })
        .collect();
    let depth = DepthField {
        mode: state.focus,
        focus: [canvas.sw as f64 * 0.5, canvas.sh as f64 * 0.5],
        radius: canvas.sw.max(canvas.sh) as f64 * 0.55,
    };
    let mut attribution = vec![CellDetail::default(); area.width as usize * area.height as usize];
    if !tiles.is_empty() {
        let options = SceneOpts {
            vp: &vp,
            layers: [true; termap::data::LAYER_COUNT],
            depth: &depth,
            highlight: None,
            show_labels: state.show_labels,
            road_glyph: state.road_glyph,
            terrain,
            exag: lift.exag,
            datum: lift.datum,
            home: None,
            road_weight: state.road_weight,
            mode: termap::view::Mode::of(vp.zoom),
            reserved: scalebar.as_ref().map(|bar| bar.rect),
            places: &markers,
            place_at: state.tour.at,
        };
        if annotations_only {
            termap::scene::draw_annotations(&tiles, &mut canvas, &options);
        } else {
            termap::scene::draw(&tiles, &mut canvas, &options);
        }
        canvas.resolve_attributed(
            &mut buffer,
            local,
            &Fog {
                near: 1.0,
                far: 0.72,
                gamma: 0.85,
            },
            state.mono,
            theme,
            &mut attribution,
        );
    }
    if let Some(bar) = scalebar {
        draw_scalebar(&mut buffer, &bar, theme);
        clear_attribution(&mut attribution, area.width, bar.rect);
    }
    if let Some((at, alpha)) = state.tour.card().filter(|_| state.custom_title.is_none()) {
        if let Some(rect) = draw_card(&mut buffer, local, at, alpha, theme, &state.places) {
            clear_attribution(&mut attribution, area.width, rect);
        }
    }
    let mut cells = Vec::with_capacity(buffer.content.len());
    let mut detail_cache = BTreeMap::new();
    let mut detail_ids = BTreeMap::new();
    for (index, cell) in buffer.content.iter().enumerate() {
        let glyph = cell.symbol().chars().next().unwrap_or(' ');
        let detail = detail_ref(
            scene,
            attribution[index],
            glyph,
            &tiles,
            &mut detail_cache,
            &mut detail_ids,
        );
        cells.push(ArtCell {
            glyph,
            foreground: rgba(cell.fg, theme),
            background: Some(rgba(cell.bg, theme)),
            bold: cell.modifier.contains(Modifier::BOLD),
            detail,
        });
    }
    scene.primitives.push(Primitive::CellArt(CellArt {
        x: area.x,
        y: area.y,
        cols: area.width,
        rows: area.height,
        cells,
    }));
    if let Some(title) = &state.custom_title {
        crate::put(
            scene,
            area.x + 3,
            area.y + 2,
            title,
            crate::PaletteRole::Amber,
            true,
        );
    }
    if let Some(query) = &state.search {
        let width = area.width.saturating_sub(6).min(70);
        for row in 0..(state.results.len() + 3).min(area.height as usize) {
            crate::put(
                scene,
                area.x + 3,
                area.y + row as u16 + 1,
                &" ".repeat(width as usize),
                crate::PaletteRole::Ink,
                false,
            );
        }
        crate::put(
            scene,
            area.x + 3,
            area.y + 1,
            &format!("find › {query}"),
            crate::PaletteRole::Amber,
            true,
        );
        for (index, result) in state.results.iter().enumerate() {
            if index + 3 >= area.height as usize {
                break;
            }
            let text = format!(
                "{} {}",
                if index == state.search_at { "›" } else { " " },
                result.name
            );
            crate::put(
                scene,
                area.x + 3,
                area.y + 3 + index as u16,
                &text.chars().take(width as usize).collect::<String>(),
                if index == state.search_at {
                    crate::PaletteRole::Ink
                } else {
                    crate::PaletteRole::Faint
                },
                false,
            );
        }
    } else if let Some(hover) = &state.hover {
        crate::put(
            scene,
            area.x + 3,
            area.y + area.height.saturating_sub(1),
            hover,
            crate::PaletteRole::Ink,
            false,
        );
    }
}

fn detail_ref(
    scene: &mut VisualScene,
    source: CellDetail,
    glyph: char,
    tiles: &[&Tile],
    cache: &mut BTreeMap<(u8, u64), u16>,
    ids: &mut BTreeMap<String, u16>,
) -> u16 {
    if glyph == ' ' {
        return 0;
    }
    if let Some(index) = cache.get(&(source.kind, source.value)) {
        return *index;
    }
    let resolved = match source.kind {
        DETAIL_GEOMETRY_PICK => {
            let pick = source.value as u32;
            let slot = (pick >> 20) as usize;
            let index = (pick & 0xF_FFFF) as usize;
            tiles
                .get(slot)
                .and_then(|tile| tile.features.get(index))
                .map(|feature| {
                    (
                        DetailClass::MapGeometry,
                        format!("map-geometry:{:016x}", feature.stable_id),
                    )
                })
        }
        DETAIL_MAP_LABEL => Some((
            DetailClass::MapLabel,
            format!("map-label:{:016x}", source.value),
        )),
        DETAIL_MAP_MARKER => {
            let slug = PLACES.iter().find(|place| {
                termap::data::stable_detail_id("authored-place", place.slug) == source.value
            });
            Some((
                DetailClass::MapMarker,
                slug.map_or_else(
                    || format!("map-marker:{:016x}", source.value),
                    |place| format!("map-marker:{}", place.slug),
                ),
            ))
        }
        255 => return 0,
        _ => return 0,
    };
    let Some((class, id)) = resolved else {
        return 0;
    };
    let index = if let Some(index) = ids.get(&id) {
        *index
    } else {
        let index = (scene.details.len() + 1) as u16;
        ids.insert(id.clone(), index);
        scene.details.push(Detail { id, class });
        index
    };
    cache.insert((source.kind, source.value), index);
    index
}

fn clear_attribution(details: &mut [CellDetail], width: u16, rect: Rect) {
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width).min(width) {
            if let Some(detail) = details.get_mut(y as usize * width as usize + x as usize) {
                *detail = CellDetail {
                    kind: 255,
                    value: 0,
                };
            }
        }
    }
}

fn tile_intersects(z: u8, x: u32, y: u32, bounds: [f64; 4]) -> bool {
    let size = (1u64 << z) as f64;
    let left = x as f64 / size;
    let top = y as f64 / size;
    let right = (x + 1) as f64 / size;
    let bottom = (y + 1) as f64 / size;
    right >= bounds[0] && left <= bounds[2] && bottom >= bounds[1] && top <= bounds[3]
}

struct ScaleBar {
    rect: Rect,
    nums: String,
    bar: String,
}

fn scalebar(vp: &MapViewport, width: u16, height: u16) -> Option<ScaleBar> {
    if height < 4 || width < 24 {
        return None;
    }
    let m_per_cell = vp.meters_per_subpixel() * 2.0;
    let target_cells = (width as f64 * 0.22).clamp(10.0, 30.0);
    let raw = m_per_cell * target_cells;
    let pow = 10f64.powf(raw.log10().floor());
    let nice = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|multiple| multiple * pow)
        .find(|value| *value >= raw * 0.6)
        .unwrap_or(pow);
    let cells = (nice / m_per_cell).round() as u16;
    if cells < 6 || cells + 2 >= width {
        return None;
    }
    let (full, half) = if nice >= 1000.0 {
        (
            format!("{:.0} km", nice / 1000.0),
            format!("{:.1}", nice / 2000.0),
        )
    } else {
        (format!("{nice:.0} m"), format!("{:.0}", nice / 2.0))
    };
    let bar = (0..=cells)
        .map(|i| match i {
            0 => '├',
            i if i == cells => '┤',
            i if i == cells / 2 => '┼',
            _ => '─',
        })
        .collect();
    let measured = cells as usize + full.chars().count() + 2;
    let mut nums = vec![' '; measured];
    let mut put = |at: usize, value: &str| {
        for (offset, glyph) in value.chars().enumerate() {
            if at + offset < nums.len() {
                nums[at + offset] = glyph;
            }
        }
    };
    put(0, "0");
    put(
        (cells as usize / 2).saturating_sub(half.chars().count() / 2),
        &half,
    );
    put(cells as usize + 2, &full);
    Some(ScaleBar {
        rect: Rect::new(2, height - 3, (measured as u16).min(width - 2), 2),
        nums: nums.into_iter().collect(),
        bar,
    })
}

fn draw_scalebar(buffer: &mut Buffer, bar: &ScaleBar, theme: Theme) {
    let style = Style::default().fg(theme.faint()).bg(theme.page());
    Paragraph::new(Span::styled(bar.nums.clone(), style)).render(bar.rect, buffer);
    Paragraph::new(Span::styled(bar.bar.clone(), style)).render(
        Rect {
            y: bar.rect.y + 1,
            ..bar.rect
        },
        buffer,
    );
}

fn draw_card(
    buffer: &mut Buffer,
    area: Rect,
    at: usize,
    alpha: f32,
    theme: Theme,
    places: &[portfolio_v2_protocol::Place],
) -> Option<Rect> {
    if area.width < 34 || area.height < 12 {
        return None;
    }
    let place = &places[at];
    let width = area.width.saturating_sub(6).min(74);
    let note = wrap(&place.note, width as usize);
    let max_note = ((area.height / 3).saturating_sub(4)).clamp(1, 5) as usize;
    let note = &note[..note.len().min(max_note)];
    fade_band(buffer, area, 6 + note.len() as u16, 4, alpha, theme);
    let x = area.x + 3;
    let mut y = area.y + 1;
    let row = Rect::new(x, y, width, 1);
    let pips = places
        .iter()
        .enumerate()
        .flat_map(|(index, _)| {
            [
                Span::styled(
                    if index == at { "●" } else { "·" },
                    Style::default().fg(if index == at {
                        dim(theme.amber(), alpha, theme)
                    } else {
                        dim(theme.ghost(), alpha, theme)
                    }),
                ),
                Span::raw(" "),
            ]
        })
        .collect::<Vec<_>>();
    Paragraph::new(Line::from(pips)).render(row, buffer);
    Paragraph::new(Span::styled(
        place.kind.to_uppercase(),
        Style::default().fg(dim(theme.ghost(), alpha, theme)),
    ))
    .right_aligned()
    .render(row, buffer);
    y += 1;
    let row = Rect::new(x, y, width, 1);
    Paragraph::new(Span::styled(
        place.name.to_uppercase(),
        Style::default()
            .fg(dim(theme.ink(), alpha, theme))
            .add_modifier(Modifier::BOLD),
    ))
    .render(row, buffer);
    Paragraph::new(Span::styled(
        place.years.as_str(),
        Style::default().fg(dim(theme.amber(), alpha, theme)),
    ))
    .right_aligned()
    .render(row, buffer);
    y += 1;
    let row = Rect::new(x, y, width, 1);
    Paragraph::new(Span::styled(
        format!("{}  ·  {}", place.role, place.where_),
        Style::default().fg(dim(theme.faint(), alpha, theme)),
    ))
    .render(row, buffer);
    Paragraph::new(Span::styled(
        format!("{:.3}°N {:.3}°E", place.lonlat.1, place.lonlat.0),
        Style::default().fg(dim(theme.ghost(), alpha, theme)),
    ))
    .right_aligned()
    .render(row, buffer);
    y += 2;
    for line in note {
        Paragraph::new(Span::styled(
            line.as_str(),
            Style::default().fg(dim(theme.faint(), alpha, theme)),
        ))
        .render(Rect::new(x, y, width, 1), buffer);
        y += 1;
    }
    Some(Rect::new(
        area.x,
        area.y,
        area.width,
        (10 + note.len() as u16).min(area.height),
    ))
}

fn fade_band(buffer: &mut Buffer, area: Rect, solid: u16, ramp: u16, alpha: f32, theme: Theme) {
    for dy in 0..(solid + ramp).min(area.height) {
        let keep = if dy < solid {
            0.0
        } else {
            let t = (dy - solid + 1) as f32 / (ramp + 1) as f32;
            t * t * (3.0 - 2.0 * t)
        };
        let keep = 1.0 - alpha * (1.0 - keep);
        for dx in 0..area.width {
            let cell = &mut buffer[(area.x + dx, area.y + dy)];
            cell.set_style(
                Style::default()
                    .fg(toward_ground(cell.fg, keep, theme))
                    .bg(toward_ground(cell.bg, keep, theme)),
            );
        }
    }
}

fn dim(color: Color, alpha: f32, theme: Theme) -> Color {
    toward_ground(color, alpha.clamp(0.0, 1.0), theme)
}

fn toward_ground(color: Color, keep: f32, theme: Theme) -> Color {
    let Some((r, g, b)) = termap::canvas::rgb_of(color) else {
        return color;
    };
    let (br, bg, bb) = theme.ground();
    let mix = |value: u8, base: u8| {
        (base as f32 + (value as f32 - base as f32) * keep)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    termap::canvas::ink(mix(r, br), mix(g, bg), mix(b, bb))
}

fn wrap(value: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in value.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

pub(super) fn rgba(color: Color, theme: Theme) -> Rgba8 {
    let (r, g, b) = termap::canvas::rgb_of(color).unwrap_or_else(|| theme.ground());
    Rgba8(r, g, b, 255)
}

#[cfg(test)]
mod tests {
    #[test]
    fn opening_flight_has_a_semantic_description_before_a_card_is_shown() {
        let mut map = super::MapState::default();
        map.open(crate::Viewport::default());
        assert!(map.description().contains("India"));
        map.tick(0.1);
        assert!(!map.description().is_empty());
    }
    #[test]
    fn camera_catches_up_when_presentation_frames_are_dropped() {
        let mut slow = super::MapState::default();
        slow.open(crate::Viewport::default());
        let mut fast = slow.clone();
        slow.tick(1.0);
        for _ in 0..10 {
            fast.tick(0.1);
        }
        assert!((slow.camera.zoom - fast.camera.zoom).abs() < 1e-9);
        assert!((slow.camera.center[0] - fast.camera.center[0]).abs() < 1e-9);
        assert!((slow.camera.center[1] - fast.camera.center[1]).abs() < 1e-9);
    }
    use super::*;

    fn assert_point_close(actual: [f64; 2], expected: [f64; 2]) {
        for axis in 0..2 {
            assert!(
                (actual[axis] - expected[axis]).abs() < 1e-9,
                "axis {axis}: {actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn drag_keeps_the_grabbed_ground_under_the_pointer() {
        let mut map = MapState::default();
        map.resize(Viewport::default());
        let from = [120.0, 80.0];
        let to = [146.0, 91.0];
        let grabbed = map.camera.unproject(from);
        map.command(crate::MapCommand::Drag(from, to));
        assert_point_close(map.camera.unproject(to), grabbed);
    }

    #[test]
    fn anchored_zoom_keeps_the_ground_under_the_pointer() {
        let mut map = MapState::default();
        map.resize(Viewport::default());
        let anchor = [183.0, 62.0];
        let grabbed = map.camera.unproject(anchor);
        map.command(crate::MapCommand::ZoomAt(0.30, anchor));
        assert_point_close(map.camera.unproject(anchor), grabbed);
    }

    #[test]
    fn manual_camera_toggle_flattens_and_restores_auto_tilt() {
        let mut map = MapState::default();
        map.command(crate::MapCommand::ToggleCamera);
        assert!(map.auto_view);
        assert!((map.camera.tilt - termap::view::auto_tilt(map.camera.zoom)).abs() < 1e-12);
        map.command(crate::MapCommand::ToggleCamera);
        assert!(!map.auto_view);
        assert_eq!(map.camera.tilt, 0.0);
        assert_eq!(map.camera.bearing, 0.0);
        assert_eq!(map.camera.persp, 0.0);
    }

    #[test]
    fn manual_tilt_keeps_perspective_in_sync_with_zoom() {
        let mut map = MapState::default();
        map.auto_view = false;
        map.camera.tilt = 0.7;
        map.camera.zoom = 17.0;
        map.sync_camera();
        assert_eq!(map.camera.tilt, 0.7);
        assert!((map.camera.persp - termap::view::auto_persp(17.0)).abs() < 1e-12);
    }

    #[test]
    fn archive_zoom_uses_floor_and_stays_at_z14_past_the_archive_ceiling() {
        assert_eq!(tile_zoom(13.9), 13);
        for zoom in [16.5, 17.0, 18.0] {
            assert_eq!(tile_zoom(zoom), 14);
        }
    }

    #[test]
    fn z14_tiles_remain_active_and_renderable_at_high_camera_zoom() {
        let feature = termap::data::Feature::new(
            termap::data::Layer::RoadMajor,
            255,
            false,
            None,
            vec![[0.5, 0.5], [0.500_01, 0.500_01]],
        );
        let tiles = vec![(
            14,
            8192,
            8192,
            std::sync::Arc::new(Tile::new(vec![feature])),
        )];
        let bounds = [0.5, 0.5, 0.500_02, 0.500_02];
        for zoom in [16.5, 17.0, 18.0] {
            assert_eq!(active_tile_zoom(&tiles, bounds, zoom), Some(14));
        }
    }

    #[test]
    fn prefetch_includes_every_authored_stop_within_its_cap() {
        let map = MapState::default();
        let viewport = Viewport::default();
        let prefetch = map.prefetch_demand(viewport);
        assert!(prefetch.tiles.len() <= 256);
        let available = prefetch.tiles.iter().copied().collect::<BTreeSet<_>>();
        for place in PLACES {
            let mut camera = MapViewport::new(place.world(), place.zoom);
            camera.tilt = place.tilt.to_radians();
            camera.bearing = place.bearing.to_radians();
            camera.persp = termap::view::auto_persp(camera.zoom);
            for tile in demand_for(camera, viewport).tiles {
                assert!(
                    available.contains(&tile),
                    "missing stop tile {tile:?} for {}",
                    place.name
                );
            }
        }
    }

    #[test]
    fn authored_tour_cameras_are_north_up_at_terrain_scale() {
        for place in PLACES {
            assert_eq!(place.bearing, 0.0, "{} is not north-up", place.name);
            assert!(
                (11.0..=12.0).contains(&place.zoom),
                "{} is too close for the shipped DEM",
                place.name
            );
        }
    }

    #[test]
    fn distant_tiles_are_excluded_from_a_view() {
        let z = 14;
        let near = (11_515, 7_143);
        let bounds = [
            near.0 as f64 / (1u64 << z) as f64,
            near.1 as f64 / (1u64 << z) as f64,
            (near.0 + 1) as f64 / (1u64 << z) as f64,
            (near.1 + 1) as f64 / (1u64 << z) as f64,
        ];
        assert!(tile_intersects(z, near.0, near.1, bounds));
        assert!(!tile_intersects(z, near.0 + 20, near.1, bounds));
    }
}
