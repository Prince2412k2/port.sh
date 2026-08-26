//! Terrain relief: the pass that makes a tilt read as 3D rather than as skew.
//!
//! A world-aligned grid is sampled from the heightmap, hillshaded from finite
//! differences, then projected with elevation. Drawing it as dots rather than a
//! surface is deliberate -- the stipple already reads as "ground" against the
//! solid strokes used for roads, and a dot grid needs no triangulation, no
//! backface culling and no seams.

use crate::canvas::{Brush, Canvas, MAT_DOT, TINT_GREEN};
use crate::geo::{meters_per_world_unit, world_to_lonlat, Viewport};
use crate::terrain::Terrain;
use crate::view::Ground;

/// Subpixels between grid samples. Denser than this and the relief turns into a
/// solid wash that competes with the roads it is supposed to sit behind.
const STEP: f64 = 3.0;

/// Contour intervals to choose between, metres. Round numbers only: a map
/// whose lines are 37 m apart is a map nobody can count in their head.
const STEPS: [f32; 10] = [5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0];

/// Roughly how many lines should cross the visible range of elevation.
const WANT_LINES: f32 = 9.0;

/// The interval for a given spread of elevation.
///
/// Chosen from the ground actually in frame rather than from the zoom, because
/// the same zoom over the Ghats and over Ahmedabad wants intervals an order of
/// magnitude apart -- 754 m of range against 21 m.
fn interval(range: f32) -> f32 {
    let want = (range / WANT_LINES).max(1.0);
    *STEPS.iter().find(|s| **s >= want).unwrap_or(&2000.0)
}

/// Highest elevation used to normalise shading, metres.
const MAX_ELEV: f32 = 6000.0;


#[derive(Default)]
pub struct Relief {
    /// Sample heights, reused across frames to avoid reallocating.
    heights: Vec<f32>,
    gw: usize,
    gh: usize,
}

impl Relief {
    /// Draw the terrain surface. Returns the number of samples plotted.
    pub fn draw(
        &mut self,
        t: &Terrain,
        canvas: &mut Canvas,
        vp: &Viewport,
        datum: f32,
        exag: f64,
        ground: Ground,
    ) -> usize {
        // Sampling in screen space rather than world space keeps the grid
        // uniform on the display no matter how the camera is turned.
        self.gw = (canvas.sw as f64 / STEP).ceil() as usize + 3;
        self.gh = (canvas.sh as f64 / STEP).ceil() as usize + 3;
        self.heights.clear();
        self.heights.resize(self.gw * self.gh, 0.0);

        // Fraction of the screen left as sky when tilted.
        //
        // A parallel projection has no vanishing point, so the ground plane
        // covers the whole frame and terrain runs edge to edge with nothing to
        // read a silhouette against. Clipping the far distance manufactures a
        // horizon: ground stops, peaks rise past it into black, and the eye
        // finally gets the cue it was missing.
        let over = 1.0;
        let plate = vp.plate();
        let bounded = !vp.is_flat();

        let mut world = vec![[0.0f64; 2]; self.gw * self.gh];
        for gy in 0..self.gh {
            for gx in 0..self.gw {
                // Alternate rows by half a sample. A square grid turns every
                // steep slope into aligned vertical ribbons at terminal
                // resolution; this triangular grid has the same sample count
                // and spacing but no screen-wide column alias.
                let stagger = if gy & 1 == 0 { 0.0 } else { STEP * 0.5 };
                let sx = (gx as f64 - 1.0) * STEP + stagger;
                let sy = (gy as f64 - 1.0) * STEP * over - canvas.sh as f64 * (over - 1.0);
                let w = vp.unproject([sx, sy]);
                let (lon, lat) = world_to_lonlat(w[0], w[1]);
                let i = gy * self.gw + gx;
                world[i] = w;
                self.heights[i] = t.sample(lon, lat);
            }
        }

        let (_, clat) = vp.center_lonlat();
        let m_per_world = meters_per_world_unit(clat);
        // Flat views have no vertical axis to displace along, and `Shade` is the
        // mode that chooses not to use the one it has.
        let exag = if vp.is_flat() || !ground.displaces() { 0.0 } else { exag };
        let mut plotted = 0usize;

        // Column-major, marching far to near. That ordering is the whole trick:
        // a bare grid of dots has no surface to hide anything behind, so each
        // sample is drawn as a vertical ribbon down to its nearer neighbour,
        // and nearer ribbons paint over farther ones. Occlusion by a ridge then
        // falls out of the draw order without a visibility test.
        for gx in 1..self.gw - 1 {
            for gy in 1..self.gh - 2 {
                let i = gy * self.gw + gx;
                let h = self.heights[i];
                // Sea level is not terrain; the water layer already owns it.
                if h < 1.0 {
                    continue;
                }
                // Outside the slab there is no ground, so nothing is drawn and
                // the plate keeps a clean edge.
                let mut fade = 1.0f32;
                if bounded {
                    let m = vp.plane_of(world[i]);
                    if m[0].abs() > plate[0] || m[1] < plate[1] || m[1] > plate[2] {
                        continue;
                    }
                    fade = crate::scene::plate_fade(m, plate);
                    if fade <= 0.02 {
                        continue;
                    }
                }

                // Finite differences off the sampled grid rather than extra
                // heightmap lookups -- the neighbours are already in hand.
                let dzdx = self.heights[i + 1] - self.heights[i - 1];
                let dzdy = self.heights[i + self.gw] - self.heights[i - self.gw];

                // Lambert shading against a fixed north-west light, the
                // convention every printed relief map uses.
                let nx = -dzdx;
                let ny = -dzdy;
                let nz = 60.0;
                let len = (nx * nx + ny * ny + nz * nz).sqrt().max(1e-6);
                let lambert = ((nx * -0.5 + ny * -0.5 + nz * 0.7) / len).clamp(0.0, 1.0);

                // Weighted towards slope rather than height. Terrain drawn by
                // elevation alone fills a whole city with texture and buries
                // the map; drawn by slope, flat ground stays empty and only
                // real relief shows, which is what the eye wants from it.
                let slope = ((dzdx * dzdx + dzdy * dzdy).sqrt() / 260.0).clamp(0.0, 1.0);
                let band = (h / MAX_ELEV).clamp(0.0, 1.0).powf(0.6);
                let relief = 0.72 * slope + 0.28 * (1.0 - lambert);
                if relief < 0.06 && band < 0.10 {
                    continue;
                }
                let alpha = (0.10 + 0.80 * relief) * (0.55 + 0.45 * band) * fade;

                let hw = (h - datum) as f64 * exag / m_per_world;
                let (sp, depth) = vp.project3(world[i], hw);
                if !depth.is_finite() {
                    continue;
                }

                let brush = Brush {
                    depth,
                    tint: TINT_GREEN,
                    // `Shade` is describing a surface rather than marking
                    // points on one, and a surface wants tone. Braille at low
                    // coverage reads as speckle the eye counts; the same
                    // coverage as a shade block reads as ground.
                    mat: if ground == Ground::Shade {
                        crate::canvas::MAT_SHADE
                    } else {
                        MAT_DOT
                    },
                    pick: u32::MAX,
                    occlude: true,
                };

                // The next row nearer: the ribbon spans the gap to it, so the
                // surface is continuous instead of a field of specks.
                let j = i + self.gw;
                let hn = self.heights[j];
                let hnw = (hn - datum) as f64 * exag / m_per_world;
                let (sp_near, dn) = vp.project3(world[j], hnw);
                if !dn.is_finite() {
                    continue;
                }

                let y0 = sp[1];
                let y1 = sp_near[1].max(y0);
                let span = (y1 - y0).max(1.0);
                // Paint only a short hatch towards the nearer sample. Filling
                // the entire occlusion ribbon produced screen-high columns on
                // steep ground: correct geometry, but a curtain rather than a
                // hill. A capped stroke keeps slope direction without the
                // alias; the z-buffer still receives the complete ribbon below.
                let paint_to = y0 + (y1 - y0).min(2.5);
                let mut py = y0;
                // `Hachure` writes the depth buffer below but lays no stipple:
                // it describes the surface instead of painting it, and the
                // quiet is what the strokes are drawn against.
                while ground.paints_surface() && py <= paint_to {
                    let t = (py - y0) / span;
                    let x = sp[0] + (sp_near[0] - sp[0]) * t;
                    canvas.splat(x, py, alpha, &brush);
                    py += 1.0;
                }
                let mut y = y0;
                while y <= y1 {
                    // Join the projected samples, not merely their y values.
                    // With the stagger above, fixing x here would recreate the
                    // vertical ribbons the sampling pattern exists to remove.
                    let t = (y - y0) / span;
                    let x = sp[0] + (sp_near[0] - sp[0]) * t;
                    // The ribbon is opaque ground whether or not the stipple
                    // happened to paint here, so claim both subpixel columns
                    // it spans. Otherwise roads behind a ridge leak through the
                    // gaps between dots.
                    let xi = x as isize;
                    canvas.occlude_at(xi, y as isize, depth);
                    canvas.occlude_at(xi + 1, y as isize, depth);
                    y += 1.0;
                }
                plotted += 1;
            }
        }

        if ground == Ground::Contour {
            self.contours(canvas, vp, &world, datum, exag, m_per_world);
        }
        if ground == Ground::Hachure {
            plotted = self.hachures(canvas, vp, &world, datum, exag, m_per_world);
        }
        plotted
    }

    /// Strokes down the line of steepest descent.
    ///
    /// One mark per sample, pointing the way water would run, as long and as
    /// dark as the ground is steep. Flat ground gets nothing, which is most of
    /// the reason to draw this way: the marks only ever appear where there is
    /// something to say, so the eye reads relief instead of reading texture.
    ///
    /// Three things here are deliberate and worth not undoing.
    ///
    /// The direction is computed in *world* space, from the two world vectors
    /// the sampling grid already spans, not from the gradient in screen space.
    /// That is what keeps a stroke attached to the hillside it belongs to: turn
    /// the camera and the mark turns with the slope rather than swimming across
    /// it. Screen-space marks shimmer, and a shimmering hillside reads as noise
    /// however pretty the single frame is.
    ///
    /// The glyph family changes with distance rather than only the brightness.
    /// Near strokes are laid in block, which has mass; far ones in braille,
    /// which is the finest mark available. Depth stops being a fade and becomes
    /// a change of language, which is a much stronger cue at this resolution.
    ///
    /// And a crest -- a sample higher than the ground either side of it along
    /// the fall line -- is drawn heavier. Silhouette carries more of a mountain
    /// than its interior does, so it gets the weight.
    fn hachures(
        &self,
        canvas: &mut Canvas,
        vp: &Viewport,
        world: &[[f64; 2]],
        datum: f32,
        exag: f64,
        m_per_world: f64,
    ) -> usize {
        let mut drawn = 0usize;
        // Every other sample, both ways. At full density the strokes touch and
        // the whole hillside greys over into the wash this mode exists to avoid.
        // Three, not two, and the reason is the glyph. A hatch mark claims a
        // whole cell where a braille dot claims an eighth of one, so the same
        // spacing that left air between dots leaves none between lines.
        const STRIDE: usize = 3;
        /// How much of the real fall the stroke is allowed to show.
        ///
        /// A hachure is read in plan -- the strokes radiate away from a summit
        /// and that fan is the whole shape. Given the full exaggerated drop the
        /// far end lands so far down the screen that every stroke stands up
        /// vertical and the fan disappears into a picket fence. Taking a third
        /// of it keeps the sense of falling and keeps the direction legible,
        /// which is the trade the medium asks for.
        const FALL: f64 = 0.32;
        for gy in (1..self.gh.saturating_sub(2)).step_by(STRIDE) {
            for gx in (1..self.gw.saturating_sub(1)).step_by(STRIDE) {
                let i = gy * self.gw + gx;
                let h = self.heights[i];
                if h < 1.0 {
                    continue;
                }
                let dzdx = self.heights[i + 1] - self.heights[i - 1];
                let dzdy = self.heights[i + self.gw] - self.heights[i - self.gw];
                let grade = (dzdx * dzdx + dzdy * dzdy).sqrt();
                // The quiet. Below this the ground is flat enough that a mark
                // would be describing rounding error in the heightmap.
                if grade < 6.0 {
                    continue;
                }

                // Downhill, in world units, and the units are the whole care
                // here. `ex`/`ey` are the world vectors that *one* grid step
                // spans -- the neighbours are two steps apart, hence the half.
                // Going through them is how a screen-space grid yields a
                // world-space direction without unprojecting anything, and it
                // is what keeps the stroke welded to its hillside when the
                // camera turns.
                let ex = [
                    (world[i + 1][0] - world[i - 1][0]) * 0.5,
                    (world[i + 1][1] - world[i - 1][1]) * 0.5,
                ];
                let ey = [
                    (world[i + self.gw][0] - world[i - self.gw][0]) * 0.5,
                    (world[i + self.gw][1] - world[i - self.gw][1]) * 0.5,
                ];
                // Unit downhill in grid space.
                let (gx, gy) = (-dzdx as f64 / grade as f64, -dzdy as f64 / grade as f64);

                // Steeper ground gets a longer stroke, measured in grid steps
                // and capped under the stride so neighbours never touch. Too
                // short is a speck with no direction; too long and the hillside
                // braids into curtains, which is what the first cut of this did.
                let steep = ((grade / 90.0) as f64).clamp(0.0, 1.0);
                let reach = (0.85 + 1.15 * steep).min(STRIDE as f64 * 0.8);
                let span = [
                    world[i][0] + reach * (gx * ex[0] + gy * ey[0]),
                    world[i][1] + reach * (gx * ex[1] + gy * ey[1]),
                ];

                // A crest: higher than the ground both up and down the fall
                // line. Silhouette over interior, so it gets the weight.
                let up = self.heights[i - self.gw].max(self.heights[i - 1]);
                let down = self.heights[i + self.gw].max(self.heights[i + 1]);
                let crest = h > up && h > down;

                let top = (h - datum) as f64 * exag / m_per_world;
                // The far end sits lower by however much the ground actually
                // falls over the stroke, so the mark lies on the surface rather
                // than floating off it. `grade` is a difference across two grid
                // steps, so the fall per step is half of it.
                let drop = (grade as f64 * 0.5 * reach) * exag * FALL / m_per_world;
                let (pa, da) = vp.project3(world[i], top);
                let (pb, db) = vp.project3(span, top - drop);
                if !da.is_finite() || !db.is_finite() {
                    continue;
                }

                let far = da.max(db).clamp(0.0, 1.0);
                crate::raster::line(
                    canvas,
                    pa,
                    pb,
                    &crate::raster::Pen {
                        width: 1.0,
                        // Steepness carries most of it, distance takes some
                        // back, and a crest is never faint.
                        alpha: (((0.30 + 0.55 * steep) * (1.0 - 0.5 * far as f64)) as f32
                            + if crest { 0.20 } else { 0.0 })
                        .clamp(0.08, 0.95),
                        depth: da.min(db),
                        tint: TINT_GREEN,
                        // Three families in one mode, and distance chooses
                        // between them. A near crest gets block, which has
                        // mass. The body of the slope gets hatch, whose glyph
                        // runs the way the stroke does -- the mark then *looks*
                        // like the direction it is reporting instead of being a
                        // dot that happens to sit along it. Far ground falls
                        // back to braille, the finest and quietest mark there
                        // is, because at that distance a line glyph is a whole
                        // cell of ink spent on something the eye is not reading
                        // in detail anyway.
                        mat: if crest && far < 0.35 {
                            crate::canvas::MAT_SOLID
                        } else if far < 0.72 {
                            crate::canvas::MAT_HATCH
                        } else {
                            MAT_DOT
                        },
                        pick: u32::MAX,
                        occlude: false,
                    },
                );
                drawn += 1;
            }
        }
        drawn
    }

    /// Iso-elevation lines over the sampled grid, by marching squares.
    ///
    /// Contours are the one way of drawing terrain where slope is read
    /// directly rather than inferred: the lines bunch where the ground is
    /// steep because that is what a constant height step means on a steep
    /// face. They are also the honest option when the heightmap is coarse --
    /// at 853 x 928 m per sample there is no fine detail to invent, and an
    /// interval wide enough to be supportable says so on the page.
    ///
    /// Drawn against the grid `draw` has already sampled, so this costs no
    /// heightmap lookups, and projected with the same exaggeration as the
    /// surface so the lines lie *on* the ground rather than through it.
    fn contours(
        &self,
        canvas: &mut Canvas,
        vp: &Viewport,
        world: &[[f64; 2]],
        datum: f32,
        exag: f64,
        m_per_world: f64,
    ) {
        let (lo, hi) = self
            .heights
            .iter()
            .filter(|h| **h >= 1.0)
            .fold((f32::MAX, f32::MIN), |(a, b), h| (a.min(*h), b.max(*h)));
        if !(lo.is_finite() && hi.is_finite()) || hi - lo < 2.0 {
            return;
        }
        let step = interval(hi - lo);

        // Where a level crosses the segment between two corners, as a world
        // point lifted to that level. Both ends are already projected samples,
        // so this is a straight interpolation in world space.
        let cross = |ia: usize, ib: usize, level: f32| -> Option<([f64; 2], f64)> {
            let (ha, hb) = (self.heights[ia], self.heights[ib]);
            if (ha < level) == (hb < level) || (hb - ha).abs() < 1e-6 {
                return None;
            }
            let f = ((level - ha) / (hb - ha)) as f64;
            let (a, b) = (world[ia], world[ib]);
            Some(([a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f], level as f64))
        };

        // Where each index contour's height gets written, and whether it has
        // been written yet. A contour map without numbers on it is a map of
        // shape, not of height: you can see that the ground rises without ever
        // learning what to. One label per index line is the printed-map
        // convention and it is also all there is room for.
        // (level index, distance from mid-screen, height, cell x, cell y)
        let mut labelled: Vec<(i32, f64, f32, f64, f64)> = Vec::new();

        let first = (lo / step).ceil() as i32;
        let last = (hi / step).floor() as i32;
        for k in first..=last {
            let level = k as f32 * step;
            if level < 1.0 {
                continue;
            }
            // Every fifth line is an index contour, the heavier one a reader
            // counts from. Straight off paper cartography.
            let index = k % 5 == 0;
            for gy in 0..self.gh - 1 {
                for gx in 0..self.gw - 1 {
                    let i = gy * self.gw + gx;
                    let (a, b, c, d) = (i, i + 1, i + self.gw, i + self.gw + 1);
                    // Sea is not terrain, and a coastline drawn as a contour
                    // fights the water layer that already owns it.
                    if self.heights[a] < 1.0 && self.heights[d] < 1.0 {
                        continue;
                    }
                    let mut ends: Vec<([f64; 2], f64)> = Vec::new();
                    for (p, q) in [(a, b), (b, d), (c, d), (a, c)] {
                        if let Some(hit) = cross(p, q, level) {
                            ends.push(hit);
                        }
                    }
                    // Two crossings is a segment. Four is a saddle, and joining
                    // the wrong pair draws an X through the col; taking them in
                    // edge order pairs each with its neighbour instead.
                    for pair in ends.chunks(2) {
                        let [(wa, la), (wb, lb)] = pair else { continue };
                        let ea = (*la as f32 - datum) as f64 * exag / m_per_world;
                        let eb = (*lb as f32 - datum) as f64 * exag / m_per_world;
                        let (pa, da) = vp.project3(*wa, ea);
                        let (pb, db) = vp.project3(*wb, eb);
                        if !da.is_finite() || !db.is_finite() {
                            continue;
                        }
                        crate::raster::line(
                            canvas,
                            pa,
                            pb,
                            &crate::raster::Pen {
                                width: 1.0,
                                alpha: if index { 0.55 } else { 0.28 },
                                depth: da.min(db),
                                tint: TINT_GREEN,
                                mat: MAT_DOT,
                                pick: u32::MAX,
                                occlude: false,
                            },
                        );


                        // The height itself, once per index contour.
                        //
                        // Placed on a near-horizontal run of the line: numbers
                        // set across a steep segment sit at an angle to it and
                        // read as though they belong to something else, and a
                        // contour is nearly always horizontal *somewhere* on
                        // screen. Nothing is drawn if there is no such run, and
                        // a level with no room simply goes unlabelled -- an
                        // unlabelled line is a small loss, a number in the wrong
                        // place is a wrong answer.
                        // A candidate spot for this level's number. Collected
                        // rather than drawn, because the scan runs top-down and
                        // taking the first hit put every label in the top row
                        // of the frame -- six heights in a line along the sky,
                        // none of them near the contour it belonged to.
                        //
                        // Only near-horizontal runs qualify: a number set
                        // across a steep segment sits at an angle to it and
                        // reads as belonging to something else.
                        if (pb[1] - pa[1]).abs() < 2.0 {
                            let cw = crate::canvas::SUB_X as f64;
                            let chh = crate::canvas::SUB_Y as f64;
                            let (cx, cy) = ((pa[0] + pb[0]) * 0.5 / cw, pa[1] / chh);
                            if cx >= 0.0 && cy >= 0.0 {
                                let (mid_x, mid_y) =
                                    (canvas.cw as f64 * 0.5, canvas.ch as f64 * 0.5);
                                let d = (cx - mid_x).hypot(cy - mid_y);
                                match labelled.iter().position(|c| c.0 == k) {
                                    Some(at) if labelled[at].1 <= d => {}
                                    Some(at) => labelled[at] = (k, d, level, cx, cy),
                                    None => labelled.push((k, d, level, cx, cy)),
                                }
                            }
                        }
                    }
                }
            }
        }

        // The numbers, once every level is known and the best spot for each has
        // been found. Nearest the middle first, so that when two collide the one
        // that keeps its place is the one the eye was going to look at anyway.
        labelled.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut taken: Vec<(usize, usize, usize)> = Vec::new();
        for &(_, _, level, cx, cy) in &labelled {
            let text = format!("{}", level as i32);
            let (cx, cy) = (cx as usize, cy as usize);
            if cx + text.len() + 1 >= canvas.cw || cy >= canvas.ch {
                continue;
            }
            // One clear cell either side, so a height never abuts another and
            // reads as a longer number than it is.
            if taken.iter().any(|&(tx, ty, tw)| {
                ty == cy && cx <= tx + tw + 1 && tx <= cx + text.len() + 1
            }) {
                continue;
            }
            for (n, c) in text.chars().enumerate() {
                canvas.set_overlay(
                    cx + n,
                    cy,
                    crate::canvas::Overlay { ch: c, tint: TINT_GREEN, lum: 0.85, bold: false },
                );
            }
            taken.push((cx, cy, text.len()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interval comes off the ground in frame, not off the zoom.
    ///
    /// The same zoom over the Ghats and over Ahmedabad wants intervals an order
    /// of magnitude apart -- 754 m of range against 21 m -- and a fixed ladder
    /// keyed to zoom would draw one line over the Ghats and five hundred over
    /// the plain.
    #[test]
    fn the_contour_interval_follows_the_ground_not_the_zoom() {
        // Real spreads, sampled from the shipped heightmap.
        let ghats = interval(754.0 - 34.0);
        let ahmedabad = interval(60.0 - 39.0);
        assert!(ghats > ahmedabad, "{ghats} vs {ahmedabad}");
        for range in [21.0f32, 68.0, 104.0, 720.0, 3000.0] {
            let step = interval(range);
            let lines = range / step;
            assert!(
                (1.0..=WANT_LINES + 1.0).contains(&lines),
                "{range} m of range gave {lines} lines at a {step} m interval"
            );
        }
    }

    /// Round numbers only. A map whose lines are 37 m apart cannot be counted.
    #[test]
    fn every_interval_is_one_a_reader_can_count_in() {
        for range in [5.0f32, 50.0, 500.0, 5000.0, 12345.0] {
            assert!(STEPS.contains(&interval(range)) || interval(range) == 2000.0);
        }
    }
}
