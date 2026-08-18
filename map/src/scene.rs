//! Turns map data + viewport into a drawn canvas.

use crate::canvas::{Canvas, Overlay, RoadGlyph, MAT_DOT, MAT_SOLID, TINT_SELECT, SUB_X, SUB_Y};
use crate::data::{Feature, Layer, Tile, DRAW_ORDER};
use std::rc::Rc;
use crate::labels::{self, Candidate, Occupancy};
use crate::raster::{self, Pen};
use crate::style::{self, DepthField, FocusMode};
use crate::geo::Viewport;

pub struct SceneOpts<'a> {
    pub vp: &'a Viewport,
    pub layers: [bool; crate::data::LAYER_COUNT],
    pub depth: &'a DepthField,
    pub highlight: Option<u32>,
    pub show_labels: bool,
    pub road_glyph: RoadGlyph,
    /// Elevation used to drape geometry in 3D. Without it, roads project at sea
    /// level while the ground rises above them and nothing lines up.
    pub terrain: Option<&'a crate::terrain::Terrain>,
    pub exag: f64,
    /// Elevation treated as "ground level", metres.
    ///
    /// Exaggerating height above *sea* level slides the whole map up the screen
    /// wherever the land happens to sit high -- a town on a 300 m plateau lifts
    /// by more than half the frame at 14x, emptying the near half of the view.
    /// Only relief relative to the local ground is worth exaggerating.
    pub datum: f32,
    /// Your position, once known.
    pub home: Option<crate::home::Fix>,
    pub road_weight: f64,
    pub mode: crate::view::Mode,
    /// Map-local cell rect that labels must keep clear (the scalebar).
    pub reserved: Option<ratatui::layout::Rect>,
    /// Tour stops, drawn as markers when the experience tour is running.
    pub places: &'a [crate::place::Place],
    /// Which of them the camera is on.
    pub place_at: usize,
}

/// Pack (tile slot, feature index) into the u32 the pick buffer carries.
/// 12 bits of slot is far more tiles than a viewport ever holds, and 20 bits of
/// index covers the densest tile by a wide margin.
#[inline]
pub fn pack_pick(slot: usize, idx: u32) -> u32 {
    ((slot as u32) << 20) | (idx & 0xF_FFFF)
}

pub fn unpack_pick(tiles: &[Rc<Tile>], pick: u32) -> Option<&Feature> {
    let slot = (pick >> 20) as usize;
    let idx = (pick & 0xF_FFFF) as usize;
    tiles.get(slot)?.features.get(idx)
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub features: usize,
    pub labels: usize,
    pub segments: usize,
    pub relief: usize,
    pub buildings: usize,
}

pub fn draw(tiles: &[Rc<Tile>], canvas: &mut Canvas, o: &SceneOpts) -> Stats {
    let mut stats = Stats::default();
    let bounds = o.vp.world_bounds(64.0);
    let mut proj: Vec<[f64; 2]> = Vec::with_capacity(512);
    // Per-vertex camera depth, only populated when the camera is tilted.
    let mut zs: Vec<f32> = Vec::with_capacity(512);
    let tilted = !o.vp.is_flat();
    let drape = if tilted { o.terrain } else { None };
    // The ground slab: features stop where it stops, or roads run out into the
    // black past the edge of the plate.
    let plate = o.vp.plate();
    let m_per_world = crate::geo::meters_per_world_unit(o.vp.center_lonlat().1);

    ocean_wash(tiles, canvas, o, &bounds, &mut proj, &mut stats);

    for layer in DRAW_ORDER {
        if !o.layers[layer.index()] {
            continue;
        }
        let st = style::style(layer);
        if o.vp.zoom < st.min_zoom {
            continue;
        }
        let floor = crate::view::rank_floor(layer, o.vp.zoom);
        // Ground fills are texture; at region scale texture reads as static.
        if st.density > 0 && !crate::view::draws_fills(o.mode) {
            continue;
        }

        for (slot, tile) in tiles.iter().enumerate() {
        for &idx in &tile.by_layer[layer.index()] {
            let f = &tile.features[idx as usize];
            if f.rank < floor || !f.visible_in(&bounds) {
                continue;
            }

            proj.clear();
            zs.clear();
            if tilted {
                for &p in &f.pts {
                    let h = match drape {
                        Some(t) => {
                            let (lon, lat) = crate::geo::world_to_lonlat(p[0], p[1]);
                            (t.sample(lon, lat) - o.datum) as f64 * o.exag / m_per_world
                        }
                        None => 0.0,
                    };
                    let m = o.vp.plane_of(p);
                        let outside =
                        m[0].abs() > plate[0] || m[1] < plate[1] || m[1] > plate[2];
                    let (sp, z) = o.vp.project3(p, h);
                    proj.push(sp);
                    zs.push(if outside { f32::INFINITY } else { z });
                }
            } else {
                proj.extend(f.pts.iter().map(|&p| o.vp.project(p)));
            }
            if proj.len() < 2 {
                continue;
            }
            // Fills and dashes draw from `proj` wholesale rather than segment
            // by segment, so a ring with even one vertex outside the slab gets
            // scan-filled across everything between them. Lines are clipped per
            // segment and only need *some* vertex to survive; areas need all of
            // them.
            // Areas are scan-filled from the whole ring, so one stray vertex
            // drags the fill across everything between them -- they must lie
            // entirely inside the slab. Lines are clipped segment by segment
            // further down and need no gate at all; gating them here is what
            // made roads vanish at street zoom, where the basemap's vertices
            // are further apart than the plate is wide.
            let is_area = (st.density > 0 && f.closed) || st.dash.is_some();
            if tilted && is_area && zs.iter().any(|z| !z.is_finite()) {
                continue;
            }
            stats.features += 1;
            stats.segments += proj.len() - 1;

            let pick = pack_pick(slot, idx);
            let hot = o.highlight == Some(pick);
            let w = style::rank_weight(f.rank);
            // In line mode the road classes drop their block material; only the
            // ground layers keep drawing into the subpixel buffer.
            let mat = match (o.road_glyph, st.mat) {
                (RoadGlyph::Dotted, MAT_SOLID) => MAT_DOT,
                (_, m) => m,
            };
            // Only strokes scale; a dithered fill has no width to speak of.
            let base = if st.mat == MAT_SOLID {
                st.width * o.road_weight
            } else {
                st.width
            };
            let mut pen = Pen {
                width: if hot { base + 1.0 } else { base },
                alpha: if hot { 1.0 } else { st.alpha * w },
                depth: st.depth,
                tint: if hot { TINT_SELECT } else { st.tint },
                mat,
                pick,
                occlude: tilted,
            };

            if o.road_glyph == RoadGlyph::Line && st.mat == MAT_SOLID {
                pen.depth = o.depth.at(st.depth, centroid(&proj));
                raster::cell_polyline(canvas, &proj, &pen, layer == Layer::RoadMajor);
                continue;
            }

            if st.density > 0 && f.closed {
                if tilted {
                    if let Some(i) = zs.iter().position(|z| z.is_finite()) {
                        pen.depth = zs[i];
                    }
                } else {
                    pen.depth = o.depth.at(st.depth, centroid(&proj));
                }
                raster::fill(canvas, &proj, st.density, &pen);
                continue;
            }

            if let Some((on, off)) = st.dash {
                if tilted {
                    if let Some(i) = zs.iter().position(|z| z.is_finite()) {
                        pen.depth = zs[i];
                    }
                } else {
                    pen.depth = o.depth.at(st.depth, centroid(&proj));
                }
                raster::dashed_polyline(canvas, &proj, &pen, on, off);
                continue;
            }

            if tilted {
                // Real camera depth: the buffer that carried stylistic depth in
                // 2D becomes an actual z-buffer, so occlusion falls out for free.
                for seg in f.pts.windows(2) {
                    let m0 = o.vp.plane_of(seg[0]);
                    let m1 = o.vp.plane_of(seg[1]);
                    let Some((c0, c1)) = clip_to_plate(m0, m1, plate) else { continue };
                    let w0 = o.vp.world_of_plane(c0);
                    let w1 = o.vp.world_of_plane(c1);
                    let lift = |w: [f64; 2]| match drape {
                        Some(t) => {
                            let (lon, lat) = crate::geo::world_to_lonlat(w[0], w[1]);
                            (t.sample(lon, lat) - o.datum) as f64 * o.exag / m_per_world
                        }
                        None => 0.0,
                    };
                    let (p0, z0) = o.vp.project3(w0, lift(w0));
                    let (p1, z1) = o.vp.project3(w1, lift(w1));
                    if !z0.is_finite() || !z1.is_finite() {
                        continue;
                    }
                    let fade = plate_fade(c0, plate).min(plate_fade(c1, plate));
                    if fade <= 0.02 {
                        continue;
                    }
                    pen.depth = (z0 + z1) * 0.5;
                    let faded = Pen { alpha: pen.alpha * fade, ..pen };
                    raster::line(canvas, p0, p1, &faded);
                }
            } else if o.depth.mode == FocusMode::Off {
                raster::polyline(canvas, &proj, &pen);
            } else {
                // Per-segment depth: a long road can be near at one end and far
                // at the other, and that gradient is most of the effect.
                for w in proj.windows(2) {
                    let mid = [(w[0][0] + w[1][0]) * 0.5, (w[0][1] + w[1][1]) * 0.5];
                    pen.depth = o.depth.at(st.depth, mid);
                    raster::line(canvas, w[0], w[1], &pen);
                }
            }
        }
        }
    }

    if o.mode.buildings() {
        stats.buildings = draw_buildings(tiles, canvas, o, &bounds);
    }
    if o.show_labels {
        stats.labels = draw_labels(tiles, canvas, o, &bounds);
    }
    draw_home(canvas, o);
    draw_places(canvas, o);

    stats
}

/// Dither the whole viewport as ocean, then cut the land back out of it.
///
/// OSM stores no ocean geometry -- the sea is implied by which side of a
/// coastline you are on. Reconstructing sea polygons from that is a whole
/// pipeline (osmcoastline exists for a reason), so this inverts the problem:
/// wash everything, then erase the land rings the converter did manage to
/// close. Datasets with explicit sea polygons (the embedded sample) carry no
/// land rings, so they skip this entirely and fill normally.
fn ocean_wash(
    tiles: &[Rc<Tile>],
    canvas: &mut Canvas,
    o: &SceneOpts,
    bounds: &[f64; 4],
    proj: &mut Vec<[f64; 2]>,
    stats: &mut Stats,
) {
    // Only the hand-built .tmap datasets carry land rings; a real basemap has
    // genuine ocean polygons in its water layer and needs no mask at all.
    let has_land = tiles.iter().any(|t| !t.by_layer[Layer::Land.index()].is_empty());
    if !has_land || !o.layers[Layer::Water.index()] {
        return;
    }

    let st = style::style(Layer::Water);
    raster::wash(
        canvas,
        st.density,
        &Pen {
            width: 1.0,
            alpha: st.alpha,
            depth: st.depth,
            tint: st.tint,
            mat: st.mat,
            pick: u32::MAX,
            occlude: false,
        },
    );

    for tile in tiles {
        for &idx in &tile.by_layer[Layer::Land.index()] {
            let f = &tile.features[idx as usize];
            if !f.visible_in(bounds) {
                continue;
            }
            proj.clear();
            proj.extend(f.pts.iter().map(|&p| o.vp.project(p)));
            raster::erase(canvas, proj);
            stats.features += 1;
        }
    }
}

/// How much of the slab's edge to dissolve over, as a share of its extent.
const FADE: f64 = 0.22;

/// Fades to zero at the slab boundary.
///
/// A hard clip leaves a straight edge, and four straight edges read as a frame
/// around the map rather than as ground running out. Dissolving over the last
/// stretch bounds the plane just as firmly without ever drawing a box.
pub fn plate_fade(m: [f64; 2], plate: [f64; 3]) -> f32 {
    let (hw, far, near) = (plate[0], plate[1], plate[2]);
    let ramp = |d: f64, span: f64| {
        let t = (d / (span * FADE).max(1e-6)).clamp(0.0, 1.0);
        // Smoothstep: a linear ramp still shows a visible seam where it starts.
        t * t * (3.0 - 2.0 * t)
    };
    let fx = ramp(hw - m[0].abs(), hw);
    let depth_span = (near - far) * 0.5;
    let fy = ramp((m[1] - far).min(near - m[1]), depth_span);
    (fx.min(fy)) as f32
}

/// The tour stops, as markers on the ground.
///
/// Drawn for every stop, not just the current one, which is the reason the
/// flight climbs: at the top of the arc the places you have been and the place
/// you are going are on screen together, and the marker trail is what makes
/// that legible rather than merely wide.
fn draw_places(canvas: &mut Canvas, o: &SceneOpts) {
    use crate::canvas::{Overlay, TINT_HOME, TINT_SELECT};

    for (i, p) in o.places.iter().enumerate() {
        let h = match o.terrain {
            Some(t) => (t.sample(p.lonlat.0, p.lonlat.1) - o.datum) as f64 * o.exag
                / crate::geo::meters_per_world_unit(o.vp.center_lonlat().1),
            None => 0.0,
        };
        let (sp, depth) = o.vp.project3(p.world, h);
        if !depth.is_finite() {
            continue;
        }
        let (cx, cy) = (sp[0] / SUB_X as f64, sp[1] / SUB_Y as f64);
        if cx < 0.0 || cy < 0.0 || cx >= canvas.cw as f64 || cy >= canvas.ch as f64 {
            continue;
        }
        let here = i == o.place_at;
        canvas.set_overlay(
            cx as usize,
            cy as usize,
            Overlay {
                ch: if here { '\u{25c8}' } else { '\u{25c7}' },
                tint: if here { TINT_SELECT } else { TINT_HOME },
                // Visited stops sit back from the current one without
                // disappearing: the trail is context, not competition.
                lum: if here { 1.0 } else { 0.42 },
                bold: here,
            },
        );
    }
}

/// Your position: a marker and an accuracy ring.
///
/// The ring is not decoration. An IP fix is a city centroid, off by kilometres,
/// and a bare dot claims a precision the source does not have.
fn draw_home(canvas: &mut Canvas, o: &SceneOpts) {
    use crate::canvas::{Overlay, MAT_DOT, TINT_HOME};

    let Some(f) = &o.home else { return };
    let h = match o.terrain {
        Some(t) => (t.sample(f.lonlat.0, f.lonlat.1) - o.datum) as f64 * o.exag
            / crate::geo::meters_per_world_unit(o.vp.center_lonlat().1),
        None => 0.0,
    };
    let (p, depth) = o.vp.project3(f.world, h);
    if !depth.is_finite() {
        return;
    }

    // Accuracy in screen terms. Drawn only when it is big enough to mean
    // something and small enough to be a ring rather than a wash.
    let m_per_sub = o.vp.meters_per_subpixel().max(1e-6);
    let r = (f.accuracy_km * 1000.0) / m_per_sub;
    if (3.0..600.0).contains(&r) {
        let pen = Pen {
            width: 1.0,
            alpha: 0.5,
            depth,
            tint: TINT_HOME,
            mat: MAT_DOT,
            pick: u32::MAX,
            occlude: false,
        };
        const N: usize = 72;
        let mut prev: Option<[f64; 2]> = None;
        for i in 0..=N {
            let a = i as f64 / N as f64 * std::f64::consts::TAU;
            // Circular on the ground, so it foreshortens with the camera the
            // way the ground it describes does.
            let m = o.vp.plane_of(f.world);
            let w = o.vp.world_of_plane([m[0] + r * a.cos(), m[1] + r * a.sin()]);
            let (q, z) = o.vp.project3(w, h);
            if !z.is_finite() {
                prev = None;
                continue;
            }
            if let Some(pp) = prev {
                raster::line(canvas, pp, q, &pen);
            }
            prev = Some(q);
        }
    }

    let (cx, cy) = (
        (p[0] / SUB_X as f64) as usize,
        (p[1] / SUB_Y as f64) as usize,
    );
    canvas.set_overlay(
        cx,
        cy,
        Overlay { ch: '◉', tint: TINT_HOME, lum: 1.0, bold: true },
    );
}

/// Liang-Barsky clip of a segment against the ground slab, in plane coords.
///
/// Dropping segments with an endpoint outside the slab looks fine until the
/// geometry is coarse relative to the view: at street zoom the basemap's
/// vertices are hundreds of metres apart, so both ends of a road sit outside a
/// small plate while the middle crosses it, and the road disappears entirely.
fn clip_to_plate(mut a: [f64; 2], mut b: [f64; 2], plate: [f64; 3]) -> Option<([f64; 2], [f64; 2])> {
    let (hw, far, near) = (plate[0], plate[1], plate[2]);
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);

    for (p, q) in [
        (-dx, a[0] + hw),
        (dx, hw - a[0]),
        (-dy, a[1] - far),
        (dy, near - a[1]),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            if r > t1 {
                return None;
            }
            t0 = t0.max(r);
        } else {
            if r < t0 {
                return None;
            }
            t1 = t1.min(r);
        }
    }
    if t0 > t1 {
        return None;
    }
    let a2 = [a[0] + dx * t0, a[1] + dy * t0];
    let b2 = [a[0] + dx * t1, a[1] + dy * t1];
    a = a2;
    b = b2;
    Some((a, b))
}

/// Extrude footprints into masses.
///
/// Buildings are what make a street read as three-dimensional -- terrain is flat
/// at that scale, and a tilted flat map is just a skewed flat map. Each wall is
/// a quad from the footprint edge up to the roof, shaded by which way it faces,
/// with the roof outline brightest.
///
/// Back faces are not culled. They do not need to be: the z-buffer already
/// rejects anything a nearer wall has claimed, so the far side of a building is
/// hidden by its own near side for free.
fn draw_buildings(
    tiles: &[Rc<Tile>],
    canvas: &mut Canvas,
    o: &SceneOpts,
    bounds: &[f64; 4],
) -> usize {
    use crate::canvas::{MAT_DOT, TINT_MONO};

    let m_per_world = crate::geo::meters_per_world_unit(o.vp.center_lonlat().1);
    let plate = o.vp.plate();
    let tilted = !o.vp.is_flat();

    // Far to near, so the painter's order agrees with the depth buffer and
    // roofs never punch through the buildings in front of them.
    let mut order: Vec<(f64, usize, u32)> = Vec::new();
    for (slot, tile) in tiles.iter().enumerate() {
        for &idx in &tile.by_layer[Layer::Building.index()] {
            let f = &tile.features[idx as usize];
            if !f.visible_in(bounds) {
                continue;
            }
            let c = [(f.bbox[0] + f.bbox[2]) * 0.5, (f.bbox[1] + f.bbox[3]) * 0.5];
            if tilted {
                let m = o.vp.plane_of(c);
                if m[0].abs() > plate[0] || m[1] < plate[1] || m[1] > plate[2] {
                    continue;
                }
            }
            order.push((o.vp.plane_of(c)[1], slot, idx));
        }
    }
    order.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut base: Vec<[f64; 2]> = Vec::new();
    let mut top: Vec<[f64; 2]> = Vec::new();
    let mut drawn = 0usize;

    for (_, slot, idx) in order {
        let f = &tiles[slot].features[idx as usize];
        // rank is the height in metres, straight off the OSM tags.
        let h_m = f.rank.max(2) as f64;

        base.clear();
        top.clear();
        let mut depth_sum = 0.0f32;
        for &p in &f.pts {
            let ground = match o.terrain {
                Some(t) => {
                    let (lon, lat) = crate::geo::world_to_lonlat(p[0], p[1]);
                    (t.sample(lon, lat) - o.datum) as f64 * o.exag / m_per_world
                }
                None => 0.0,
            };
            // Building height is not exaggerated the way terrain is: at street
            // zoom a real tower is already tall on screen, and doubling it just
            // shears the skyline.
            let roof = ground + h_m / m_per_world;
            let (b, z) = o.vp.project3(p, ground);
            let (t2, z2) = o.vp.project3(p, roof);
            if !z.is_finite() || !z2.is_finite() {
                base.clear();
                break;
            }
            base.push(b);
            top.push(t2);
            depth_sum += z;
        }
        // A mass with any corner past the eye is dropped whole rather than
        // drawn with a torn face.
        if base.len() < 3 {
            continue;
        }
        let depth = depth_sum / base.len() as f32;

        let fade = plate_fade(o.vp.plane_of([
            (f.bbox[0] + f.bbox[2]) * 0.5,
            (f.bbox[1] + f.bbox[3]) * 0.5,
        ]), plate);
        if fade <= 0.02 {
            continue;
        }
        let wall = Pen {
            width: 1.0,
            alpha: 0.55 * fade,
            depth,
            tint: TINT_MONO,
            mat: MAT_DOT,
            pick: pack_pick(slot, idx),
            occlude: tilted,
        };
        // Walls are stippled rather than solid: a terminal has no fill shades to
        // spare, and a dithered face reads as a surface while still letting the
        // roof edge stay the brightest thing on the mass.
        let mut quad = [[0.0f64; 2]; 4];
        for i in 0..base.len() - 1 {
            quad[0] = base[i];
            quad[1] = base[i + 1];
            quad[2] = top[i + 1];
            quad[3] = top[i];
            raster::fill(canvas, &quad, 22, &wall);
        }

        // Vertical corner posts and the roof outline: the edges are what give
        // the mass its shape once the faces are only half-toned.
        let edge = Pen { alpha: 0.85 * fade, ..wall };
        for i in 0..base.len() {
            raster::line(canvas, base[i], top[i], &edge);
        }
        let roof = Pen { alpha: fade, width: 1.2, ..wall };
        raster::polyline(canvas, &top, &roof);

        drawn += 1;
    }
    drawn
}

fn centroid(pts: &[[f64; 2]]) -> [f64; 2] {
    let n = pts.len() as f64;
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
    [sx / n, sy / n]
}

/// Minimum rank a name needs to earn screen space at this zoom.
///
/// Without this the whole gazetteer shows up at once and the map turns into a
/// word cloud: the names crowd out the geometry they are supposed to annotate.
fn rank_floor(zoom: f64) -> u16 {
    match zoom {
        z if z < 10.0 => 168,
        z if z < 11.5 => 155,
        z if z < 13.0 => 140,
        // Never zero: a basemap's POI layer is mostly shops and clinics.
        _ => 105,
    }
}

/// Ceilings on labels per frame. Landmarks and places get separate budgets so a
/// dense cluster of amber cannot crowd the geography off the map -- with one
/// shared budget, an OSM extract's landmark ranks simply win every slot.
const MAX_LANDMARK_LABELS: usize = 9;
const MAX_PLACE_LABELS: usize = 20;

/// Longest label drawn. Some OSM names run to 40+ characters and a single one
/// can span a third of the viewport.
const MAX_LABEL_CHARS: usize = 26;

fn shorten(s: &str) -> String {
    if s.chars().count() <= MAX_LABEL_CHARS {
        return s.to_string();
    }
    s.chars().take(MAX_LABEL_CHARS - 1).collect::<String>() + "…"
}

fn draw_labels(tiles: &[Rc<Tile>], canvas: &mut Canvas, o: &SceneOpts, bounds: &[f64; 4]) -> usize {
    let floor = rank_floor(o.vp.zoom);
    let mut cands: Vec<Candidate> = Vec::new();
    // Tiles are generated with a buffer, so a place near an edge is present in
    // every tile that overlaps it. Without this the map reads "Kurla East Kurla
    // East".
    let mut seen: std::collections::HashSet<(u64, u64, &str)> = std::collections::HashSet::new();

    for layer in [Layer::Landmark, Layer::Place] {
        if !o.layers[layer.index()] {
            continue;
        }
        let budget = if layer == Layer::Landmark {
            MAX_LANDMARK_LABELS
        } else {
            MAX_PLACE_LABELS
        };
        let start = cands.len();
        let st = style::style(layer);
        if o.vp.zoom < st.min_zoom {
            continue;
        }

        for (slot, tile) in tiles.iter().enumerate() {
        for &idx in &tile.by_layer[layer.index()] {
            let f = &tile.features[idx as usize];
            if !f.visible_in(bounds) {
                continue;
            }
            let Some(name) = &f.name else { continue };
            let pick = pack_pick(slot, idx);
            let hot_feature = o.highlight == Some(pick);
            if f.rank < floor && !hot_feature {
                continue;
            }
            let p = o.vp.project(f.pts[0]);
            if p[0] < 0.0 || p[1] < 0.0 || p[0] >= canvas.sw as f64 || p[1] >= canvas.sh as f64 {
                continue;
            }
            let key = (
                (f.pts[0][0] * 4.0e6) as u64,
                (f.pts[0][1] * 4.0e6) as u64,
                name.as_ref(),
            );
            if !seen.insert(key) {
                continue;
            }

            cands.push(Candidate {
                anchor: p,
                text: shorten(name),
                // A hovered label always wins its slot.
                rank: if hot_feature { u16::MAX } else { f.rank },
                tint: if hot_feature { TINT_SELECT } else { st.tint },
                depth: o.depth.at(st.depth, p),
                marker: (layer == Layer::Landmark).then_some('◦'),
                feature: pick,
            });
        }
        }
        // Keep only this layer's best few before they compete for space.
        cands[start..].sort_by(|a, b| b.rank.cmp(&a.rank));
        cands.truncate(start + budget);
    }

    let mut occ = Occupancy::new(canvas.cw, canvas.ch);
    if let Some(r) = o.reserved {
        occ.block(r.x as usize, r.y as usize, r.width as usize, r.height as usize);
    }
    let placed = labels::place(cands, &mut occ, SUB_X, SUB_Y);
    let n = placed.len();

    for p in &placed {
        if let Some((a, b)) = p.leader {
            raster::line(
                canvas,
                a,
                b,
                &Pen {
                    width: 1.0,
                    alpha: 0.55,
                    depth: p.depth,
                    tint: p.tint,
                    mat: crate::canvas::MAT_DOT,
                    pick: p.feature,
                    occlude: false,
                },
            );
        }
    }

    // Markers and text go in last so nothing draws over them.
    for p in &placed {
        // Plain place names sit back a little; the amber landmarks are the ones
        // meant to catch the eye.
        let ceiling = if p.tint == crate::canvas::TINT_MONO { 0.78 } else { 1.0 };
        let lum = (0.40 + 0.60 * (1.0 - p.depth).clamp(0.0, 1.0)) * ceiling;

        if let Some((mx, my, ch)) = p.marker {
            canvas.set_overlay(mx, my, Overlay { ch, tint: p.tint, lum, bold: false });
        }
        for (i, ch) in p.text.chars().enumerate() {
            canvas.set_overlay(
                p.cell.0 + i,
                p.cell.1,
                Overlay { ch, tint: p.tint, lum, bold: p.bold },
            );
        }
    }

    n
}
