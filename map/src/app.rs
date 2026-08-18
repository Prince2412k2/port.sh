//! Application state and input handling.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::canvas::{Canvas, Fog, RoadGlyph, SUB_X, SUB_Y};
use crate::data::{Layer, Tile, LAYER_COUNT};
use crate::tiles::Source;
use std::rc::Rc;
use crate::geo::Viewport;
use crate::scene::Stats;
use crate::style::{DepthField, FocusMode};
use crate::tour::Tour;

/// Layers exposed to the number keys, in panel order.
pub const TOGGLES: [Layer; 8] = [
    Layer::RoadMajor,
    Layer::RoadMedium,
    Layer::RoadMinor,
    Layer::Rail,
    Layer::Water,
    Layer::Landuse,
    Layer::Landmark,
    Layer::Boundary,
];

pub struct App {
    pub source: Source,
    /// Tiles covering the current viewport, refreshed each frame.
    pub tiles: Vec<Rc<Tile>>,
    pub vp: Viewport,
    pub canvas: Canvas,
    pub layers: [bool; LAYER_COUNT],
    pub focus: FocusMode,
    pub fog: Fog,

    /// Cursor in cells, relative to the map area.
    pub cursor: Option<(u16, u16)>,
    drag_from: Option<(u16, u16)>,
    pub hover: Option<u32>,
    pub pinned: Option<u32>,

    pub show_help: bool,
    pub show_panel: bool,
    pub show_labels: bool,
    /// Force the whole map to the paper-white tint.
    pub mono: bool,
    pub show_terrain: bool,
    pub relief: crate::relief::Relief,
    pub road_glyph: RoadGlyph,
    /// Multiplies every stroke width, so road weight can be dialled in at
    /// runtime instead of guessed at compile time.
    pub road_weight: f64,
    /// Tilt and perspective follow zoom until the user takes the camera.
    pub auto_view: bool,
    pub home: crate::home::Slot,
    /// The experience tour: the places, and the camera that flies between them.
    pub tour: Tour,
    /// Chrome from before the tour took over, restored when it lets go.
    panel_before_tour: bool,
    /// Stop the tour should open on, once the canvas has a size. The opening
    /// descent is framed against the whole basemap, and "the whole basemap"
    /// is not a thing that can be computed before the viewport knows how many
    /// subpixels wide it is.
    tour_pending: Option<usize>,
    pub quit: bool,

    pub map_area: Rect,
    /// Set until the first frame, when the canvas size is finally known and the
    /// view can be fitted to the data.
    fit_pending: bool,
    pub stats: Stats,
    pub frame_us: u128,
    pub toast: Option<String>,
}

impl App {
    pub fn new(source: Source) -> Self {
        let b = source.bounds();
        App {
            vp: Viewport::new([(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5], 11.5),
            canvas: Canvas::new(1, 1),
            source,
            tiles: Vec::new(),
            layers: [true; LAYER_COUNT],
            focus: FocusMode::Subtle,
            fog: Fog::default(),
            cursor: None,
            drag_from: None,
            hover: None,
            pinned: None,
            show_help: false,
            show_panel: true,
            show_labels: true,
            mono: true,
            show_terrain: true,
            relief: Default::default(),
            road_glyph: RoadGlyph::Dotted,
            road_weight: 1.0,
            auto_view: true,
            home: crate::home::spawn(),
            tour: Tour::new(crate::place::load()),
            panel_before_tour: true,
            tour_pending: None,
            quit: false,
            map_area: Rect::default(),
            fit_pending: true,
            stats: Stats::default(),
            frame_us: 0,
            toast: None,
        }
    }

    /// Render mode for the current zoom.
    pub fn mode(&self) -> crate::view::Mode {
        crate::view::Mode::of(self.vp.zoom)
    }

    /// Advance anything that moves on its own. `dt` is seconds since the last
    /// frame — passed in rather than measured here so the whole tour can be run
    /// deterministically in a test.
    pub fn tick(&mut self, dt: f64) {
        self.tour.tick(dt, &mut self.vp);
    }

    /// True while something is animating and the loop must keep drawing.
    pub fn animating(&self) -> bool {
        self.tour.moving()
    }

    /// Enter the tour, landing on `i`.
    pub fn start_tour(&mut self, i: usize) {
        if self.tour.places.is_empty() {
            self.toast = Some("no places: add data/places.txt".into());
            return;
        }
        if !self.tour.active {
            self.panel_before_tour = self.show_panel;
        }
        self.show_panel = false;
        // The tour owns the camera outright; auto-by-zoom would fight it for
        // the tilt on the very next frame.
        self.auto_view = false;
        self.fit_pending = false;
        self.tour.active = true;
        self.tour_pending = Some(i.min(self.tour.places.len() - 1));
    }

    /// Put the camera on the whole basemap and start the descent. Called once
    /// the canvas has a real size, for the reason `tour_pending` exists.
    pub fn open_tour_if_pending(&mut self) {
        let Some(i) = self.tour_pending.take() else { return };
        // Start from as far out as the data goes: the tour is about where in
        // the world these places are, and it cannot say that from street level.
        self.vp.fit(self.source.bounds());
        self.vp.tilt = 0.0;
        self.vp.bearing = 0.0;
        self.vp.persp = 0.0;
        let vp = self.vp;
        self.tour.open(&vp, i);
    }

    pub fn stop_tour(&mut self) {
        self.tour.active = false;
        self.tour_pending = None;
        self.show_panel = self.panel_before_tour;
        self.auto_view = true;
    }

    /// Drive the camera from zoom, unless the user has taken it over.
    pub fn sync_camera(&mut self) {
        if self.auto_view {
            self.vp.tilt = crate::view::auto_tilt(self.vp.zoom);
            self.vp.persp = crate::view::auto_persp(self.vp.zoom);
        }
    }

    /// Called once the canvas has a real size.
    pub fn fit_if_pending(&mut self) {
        if self.fit_pending {
            self.vp.fit(self.source.bounds());
            self.fit_pending = false;
        }
    }

    /// Pin the zoom explicitly, suppressing the initial fit.
    pub fn set_zoom(&mut self, z: f64) {
        self.vp.zoom = z;
        self.fit_pending = false;
    }

    pub fn depth_field(&self) -> DepthField {
        // With no cursor the focus sits at the middle of the map, which still
        // gives a centre-weighted falloff rather than snapping to flat.
        let focus = match self.cursor {
            Some((x, y)) => [
                (x as usize * SUB_X) as f64,
                (y as usize * SUB_Y) as f64,
            ],
            None => [self.canvas.sw as f64 * 0.5, self.canvas.sh as f64 * 0.5],
        };
        DepthField {
            mode: self.focus,
            focus,
            radius: (self.canvas.sw.max(self.canvas.sh) as f64) * 0.55,
        }
    }

    pub fn highlight(&self) -> Option<u32> {
        self.pinned.or(self.hover)
    }

    /// Resolve what is under the cursor using the previous frame's pick buffer.
    pub fn update_hover(&mut self) {
        self.hover = match self.cursor {
            Some((x, y)) => self.canvas.pick_near(x as usize, y as usize, 3),
            None => None,
        };
    }

    pub fn feature_info(&self, id: u32) -> Option<String> {
        let f = crate::scene::unpack_pick(&self.tiles, id)?;
        let name = f.name.as_deref().unwrap_or("unnamed");
        Some(match self.length_km(id) {
            Some(km) => format!("{name}  ·  {}  ·  {km:.2} km", f.layer.label()),
            None => format!("{name}  ·  {}", f.layer.label()),
        })
    }

    fn length_km(&self, id: u32) -> Option<f64> {
        let f = crate::scene::unpack_pick(&self.tiles, id)?;
        if f.layer.is_point() || f.closed || f.pts.len() < 2 {
            return None;
        }
        let (_, lat) = self.vp.center_lonlat();
        let m_per_world = crate::geo::meters_per_world_unit(lat);
        let total: f64 = f
            .pts
            .windows(2)
            .map(|w| {
                let dx = w[1][0] - w[0][0];
                let dy = w[1][1] - w[0][1];
                (dx * dx + dy * dy).sqrt()
            })
            .sum();
        Some(total * m_per_world / 1000.0)
    }

    /// Tilt is capped short of edge-on: at 90 degrees the ground plane is a
    /// line, unproject stops being invertible and drag-pan goes wild.
    fn set_tilt(&mut self, t: f64) {
        self.auto_view = false;
        self.vp.tilt = t.clamp(0.0, 1.20);
        self.toast = Some(format!(
            "tilt {:.0}°  bearing {:.0}°",
            self.vp.tilt.to_degrees(),
            self.vp.bearing.to_degrees().rem_euclid(360.0)
        ));
    }

    fn set_bearing(&mut self, b: f64) {
        self.auto_view = false;
        self.vp.bearing = b;
        self.toast = Some(format!(
            "tilt {:.0}°  bearing {:.0}°",
            self.vp.tilt.to_degrees(),
            self.vp.bearing.to_degrees().rem_euclid(360.0)
        ));
    }

    fn pan_fraction(&mut self, fx: f64, fy: f64) {
        let (dx, dy) = (self.vp.sw * fx, self.vp.sh * fy);
        self.vp.pan_subpixels(dx, dy);
    }

    fn zoom_centered(&mut self, dz: f64) {
        let anchor = [self.vp.sw * 0.5, self.vp.sh * 0.5];
        self.vp.zoom_at(dz, anchor);
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press {
            return;
        }
        self.toast = None;

        if self.show_help {
            // Any key dismisses help, so it never traps you.
            self.show_help = false;
            if matches!(k.code, KeyCode::Char('q')) {
                self.quit = true;
            }
            return;
        }

        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Esc => {
                if self.pinned.take().is_none() {
                    self.quit = true;
                }
            }

            KeyCode::Char('h') | KeyCode::Left => self.pan_fraction(-0.18, 0.0),
            KeyCode::Char('l') | KeyCode::Right => self.pan_fraction(0.18, 0.0),
            KeyCode::Char('k') | KeyCode::Up => self.pan_fraction(0.0, -0.18),
            KeyCode::Char('j') | KeyCode::Down => self.pan_fraction(0.0, 0.18),

            KeyCode::Char('+') | KeyCode::Char('=') => self.zoom_centered(0.35),
            KeyCode::Char('-') | KeyCode::Char('_') => self.zoom_centered(-0.35),

            KeyCode::Char('f') => {
                self.focus = self.focus.next();
                self.toast = Some(format!("depth focus: {}", self.focus.label()));
            }
            KeyCode::Char('t') => {
                self.show_labels = !self.show_labels;
            }
            KeyCode::Char('u') => self.set_tilt(self.vp.tilt + 0.08),
            KeyCode::Char('o') => self.set_tilt(self.vp.tilt - 0.08),
            KeyCode::Char(',') => self.set_bearing(self.vp.bearing - 0.10),
            KeyCode::Char('.') => self.set_bearing(self.vp.bearing + 0.10),
            KeyCode::Char('@') => {
                // Cloned out of the lock first: the arms mutate self, and the
                // guard would still be alive across them.
                let fix = self.home.lock().unwrap().clone();
                match fix {
                Some(f) => {
                    self.vp.center = f.world;
                    // Zoom to the uncertainty, not past it. Flying to street
                    // level on a fix that is only good to ten kilometres shows
                    // a confident dot on the wrong street.
                    let z = if f.accuracy_km <= 0.05 {
                        15.0
                    } else {
                        let m = crate::geo::meters_per_world_unit(f.lonlat.1);
                        // Sized so the accuracy circle spans about a third of
                        // the frame: visibly an area, not a point.
                        (m * self.vp.sw / (256.0 * 6.0 * f.accuracy_km * 1000.0))
                            .log2()
                            .clamp(crate::geo::MIN_ZOOM, 15.0)
                    };
                    self.set_zoom(z);
                    self.toast = Some(format!(
                        "{}  ({:.4}, {:.4})  ±{:.0} km via {}",
                        f.label, f.lonlat.1, f.lonlat.0, f.accuracy_km, f.source
                    ));
                }
                None => {
                    self.toast =
                        Some("locating… set TERMAP_HOME=lat,lon to pin it".into());
                }
                }
            }
            KeyCode::Char('9') => {
                self.show_terrain = !self.show_terrain;
                self.toast = Some(
                    if self.show_terrain { "terrain on" } else { "terrain off" }.into(),
                );
            }
            KeyCode::Char('m') => {
                self.auto_view = !self.auto_view;
                if !self.auto_view {
                    self.vp.tilt = 0.0;
                    self.vp.bearing = 0.0;
                    self.vp.persp = 0.0;
                }
                self.toast = Some(
                    if self.auto_view { "camera: auto by zoom" } else { "camera: manual, flat" }
                        .into(),
                );
            }
            KeyCode::Char('[') => {
                self.road_weight = (self.road_weight - 0.15).max(0.4);
                self.toast = Some(format!("road weight {:.2}", self.road_weight));
            }
            KeyCode::Char(']') => {
                self.road_weight = (self.road_weight + 0.15).min(2.5);
                self.toast = Some(format!("road weight {:.2}", self.road_weight));
            }
            KeyCode::Char('r') => {
                self.road_glyph = self.road_glyph.next();
                self.toast = Some(format!("roads: {}", self.road_glyph.label()));
            }
            KeyCode::Char('c') => {
                self.mono = !self.mono;
                self.toast = Some(
                    if self.mono { "colour: mono" } else { "colour: by kind" }.into(),
                );
            }
            KeyCode::Char('p') => {
                self.show_panel = !self.show_panel;
            }
            KeyCode::Char('g') => {
                self.vp.fit(self.source.bounds());
                self.toast = Some("fitted to data".into());
            }
            KeyCode::Char('?') => self.show_help = true,

            // The experience tour.
            KeyCode::Char('e') => {
                if self.tour.active {
                    self.stop_tour();
                    self.toast = Some("tour off".into());
                } else {
                    self.start_tour(0);
                }
            }
            KeyCode::Tab | KeyCode::Char('n') => {
                if self.tour.active {
                    self.tour.next(&self.vp);
                } else {
                    self.start_tour(0);
                }
            }
            KeyCode::BackTab | KeyCode::Char('b') => {
                if self.tour.active {
                    self.tour.prev(&self.vp);
                } else {
                    self.start_tour(self.tour.places.len().saturating_sub(1));
                }
            }
            KeyCode::Enter => {
                // Replay the arrival at the current stop, from wherever the
                // camera has wandered to since.
                if self.tour.active {
                    let at = self.tour.at;
                    self.tour.go(&self.vp, at);
                }
            }

            KeyCode::Char('0') => {
                self.layers = [true; LAYER_COUNT];
                self.toast = Some("all layers on".into());
            }
            KeyCode::Char(d @ '1'..='8') => {
                let i = d as usize - '1' as usize;
                let layer = TOGGLES[i];
                let on = &mut self.layers[layer.index()];
                *on = !*on;
                self.toast = Some(format!(
                    "{}: {}",
                    layer.label(),
                    if *on { "on" } else { "off" }
                ));
            }
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        // Everything is expressed relative to the map area; events elsewhere
        // only matter for ending a drag.
        let inside = self.map_area.width > 0
            && m.column >= self.map_area.x
            && m.row >= self.map_area.y
            && m.column < self.map_area.x + self.map_area.width
            && m.row < self.map_area.y + self.map_area.height;

        let local = (
            m.column.saturating_sub(self.map_area.x),
            m.row.saturating_sub(self.map_area.y),
        );

        match m.kind {
            MouseEventKind::Moved => {
                self.cursor = inside.then_some(local);
            }
            MouseEventKind::Down(MouseButton::Left) if inside => {
                self.drag_from = Some(local);
                self.cursor = Some(local);
                // Click pins whatever is under the pointer; click empty space to
                // clear. Pinning survives further mouse movement.
                self.pinned = self.canvas.pick_near(local.0 as usize, local.1 as usize, 3);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.drag_from {
                    let dx = from.0 as f64 - local.0 as f64;
                    let dy = from.1 as f64 - local.1 as f64;
                    self.vp
                        .pan_subpixels(dx * SUB_X as f64, dy * SUB_Y as f64);
                    self.drag_from = Some(local);
                }
                self.cursor = Some(local);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_from = None;
            }
            MouseEventKind::ScrollUp if inside => {
                let anchor = Self::to_sub(local);
                self.vp.zoom_at(0.30, anchor);
            }
            MouseEventKind::ScrollDown if inside => {
                let anchor = Self::to_sub(local);
                self.vp.zoom_at(-0.30, anchor);
            }
            _ => {}
        }
    }

    fn to_sub((x, y): (u16, u16)) -> [f64; 2] {
        [
            (x as usize * SUB_X + SUB_X / 2) as f64,
            (y as usize * SUB_Y + SUB_Y / 2) as f64,
        ]
    }
}
