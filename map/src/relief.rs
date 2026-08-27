//! Terrain relief: the pass that makes a tilt read as three dimensions.
//!
//! A grid is laid out in *screen* space, unprojected onto the ground, sampled
//! from the heightmap, and drawn back as a field of vertical ribbons. Screen
//! space rather than world space because the grid then stays even on the
//! display however the camera is turned, and a ribbon rather than a dot
//! because a bare grid of dots is not a surface: nothing can hide behind it,
//! and terrain that does not occlude what is behind it does not read as solid.
//!
//! # The two things this pass has to get right
//!
//! **What it asks the heightmap for.** Not the height at a point -- the height
//! of the ground that one screen sample stands for. The difference is the
//! whole of `terrain.rs`, and without it this pass draws speckle. Each sample
//! measures how far apart it and its neighbour landed on the ground and asks
//! for a surface smoothed to a multiple of that, so the level of detail
//! follows the camera without anything having to be told the zoom.
//!
//! **The order it draws in.** Near to far, against the depth buffer. The
//! obvious alternative -- far to near, letting nearer ribbons paint over the
//! ones behind -- is only correct for an opaque renderer, and this one
//! composites alpha-over (`cov += a * (1 - cov)`), so a far ridge stays on
//! screen *underneath* the near one at reduced contrast and the frame turns to
//! mush. Near first, and the front ridge claims its subpixels before anything
//! behind arrives to ask for them.

use crate::canvas::{Behind, Brush, Canvas, Theme, MAT_DOT, TINT_GREEN};
use crate::geo::{meters_per_world_unit, world_to_lonlat, Viewport};
use crate::terrain::Terrain;

/// Subpixels between grid samples.
///
/// Two, where the first version of this used three. Three was a density
/// chosen to keep unsmoothed terrain from turning into a wash, which was
/// treating the symptom: the wash came from every sample landing on a
/// different ridge, not from there being too many samples. With the surface
/// low-passed to match, a denser grid is a smoother surface rather than a
/// noisier one, and the ribbons close into something continuous.
const STEP: f64 = 2.0;

/// How wide a kernel to ask for, as a multiple of the grid spacing.
///
/// Measured, not guessed. Hillshaded over a 111 km window of Kullu at 534 m
/// per sample, a kernel matching the sample spacing looks like noise and 1.8 km
/// -- three and a half times it -- is the first that reads as ridges and
/// valleys. Below about 2.5 the mat of small ridges comes back; much above 4
/// the mountains start losing the spurs that give them their shape.
const SMOOTH: f64 = 3.5;

/// The least local relief that counts as a mountain, metres.
///
/// About a ten-storey building over the width of the smoothing kernel. Ground
/// that stands over nothing is not a landform: over the plains the surface
/// moves a few metres across kilometres, and a mark for every sample of it
/// fills the frame with an even stipple that says only "there is ground here",
/// which the reader already knew.
///
/// Skipped whole -- ribbon and all, not just the ink -- so that flat ground is
/// properly absent rather than an invisible wall punching holes in the roads
/// behind it.
const MOUNTAIN: f32 = 30.0;

/// Vertical scale on the shading, as distinct from the one on the geometry.
///
/// A Lambert shade of true-scale terrain at these distances is nearly blank:
/// the steepest ground in the Himalaya is a slope of about 0.6, and against a
/// unit normal that is a few percent of tonal range. The convention on printed
/// relief maps is a z-factor, and this is it.
const SHADE_EXAG: f32 = 2.5;

/// Light direction in world terms: east, north, up. North-west and above,
/// which is where every printed relief map has put it since lithography, for
/// the good reason that lit-from-below terrain reads inside out.
const LIGHT: [f32; 3] = [-0.52, 0.52, 0.68];

/// Slope, in rise over run, below which the ground is not a landform.
///
/// Not a threshold on the ink but a gate on it: without one, flat ground takes
/// the shade a horizontal surface gets -- which in either theme is a solid
/// mid-tone -- and the plains render as a grey sheet with the mountains barely
/// separable from it.
const GATE_SLOPE: f32 = 0.20;

/// What a Lambert shade returns for level ground.
///
/// The up component of the light, and the number the whole shading is measured
/// against: ink is what a face does *differently* from flat, not how much light
/// it catches. Without subtracting it, level ground comes out at 0.68 of full
/// ink -- so the plains render as a grey sheet, the frame has no background
/// left, and the mountains have nothing to stand out of. Over Himachal that
/// was 94% of samples drawing at a mean alpha of 0.6, which is a wash and not
/// a map.
const FLAT_LIT: f32 = LIGHT[2];

/// The kernel the relief pass will be using near the middle of the frame.
///
/// Anything draped on the ground has to be draped on the *same* surface the
/// ground is drawn from. Drape a road on the raw heightmap while the surface
/// beside it is smoothed and the road runs through the hillside it is supposed
/// to sit on -- into every ridge the smoothing took off, and over every valley
/// it filled in.
pub fn drape_smoothing(vp: &Viewport) -> f64 {
    let m_per_sub = meters_per_world_unit(vp.center_lonlat().1) / vp.scale();
    m_per_sub * STEP * SMOOTH
}

/// What share of the frame's height the ground in view should fill.
///
/// The number that replaces a vertical-exaggeration ladder. A fixed factor
/// cannot work across this zoom range: fourteen times is about right over a
/// 180 km frame, where the Dhauladhar is otherwise eleven subpixels of a
/// two-hundred-row canvas, and at 45 km it throws the peaks five hundred
/// subpixels off the top of the screen. Zoom is a poor proxy for the question
/// anyway -- the same zoom over Zanskar and over Gujarat wants factors an
/// order of magnitude apart.
///
/// So the pass exposes for what is actually in frame, the way the contour
/// interval already does: whatever relief is in view is stretched to this much
/// of the canvas, and the exaggeration is read off from that. The number is
/// shared with `geo`, which runs the ground slab the same distance past the
/// bottom of the frame so that the lift has foreground to raise into.
const FILL: f64 = crate::geo::LIFT_HEADROOM;

/// Bounds on what the exposure may choose.
///
/// A backstop and not the policy. What stops a plain being stretched into a
/// range of fictional foothills is the relief floor in `expose` -- ground that
/// is not a landform is not exposed for -- and the ceiling only catches what
/// gets past it. Set from the widest view that has to work: 4000 m of relief
/// across a 100 km frame needs 28, and a ceiling that binds there quietly
/// flattens the Himalaya at exactly the zoom the whole pass is for.
const EXAG_MIN: f64 = 1.0;
const EXAG_MAX: f64 = 40.0;

/// How the height in the frame is being stretched to fit it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Lift {
    /// The elevation drawn at ground level, metres.
    ///
    /// Exaggerating above *sea* level slides the whole map up the screen
    /// wherever the land sits high: a valley floor at 1500 m lifts by more
    /// than the frame before its own mountains have been drawn at all. Only
    /// relief above the local ground is worth exaggerating.
    pub datum: f32,
    /// Vertical exaggeration applied above the datum.
    pub exag: f64,
    /// The relief the exposure was made for, metres: how far the highest
    /// ground in view stands above the lowest.
    pub relief: f32,
}

/// A slope measured along the camera's axes, turned into one along the
/// compass: east and north.
///
/// The grid is laid out on the screen, so what comes off it is a slope in the
/// camera's frame. The light is fixed to the compass, as every printed relief
/// map has it, and shading the camera's frame instead turns the light with the
/// map -- rotate the view and the sun goes round with it, which reads as the
/// mountains changing shape rather than the camera moving.
///
/// `across` runs along a grid row, `along` runs away from the camera. At a
/// bearing of zero those are already east and north.
#[inline]
fn to_compass(across: f32, along: f32, sin_b: f32, cos_b: f32) -> (f32, f32) {
    (across * cos_b + along * sin_b, along * cos_b - across * sin_b)
}

/// Grid rows nearest first. Screen y grows downwards and a tilted camera puts
/// the near ground at the bottom, so this is simply the reverse.
fn near_to_far(gh: usize) -> impl Iterator<Item = usize> {
    (1..gh.saturating_sub(2)).rev()
}

/// What the caller controls.
#[derive(Clone, Copy)]
pub struct Plot {
    /// 0 to 1, for fading the whole pass in and out across a zoom boundary.
    pub strength: f32,
    /// Which way the ink runs.
    ///
    /// The relief pass is the one place that genuinely needs to know. Ink is
    /// the mark, and for a shaded surface the mark is the *shadow* on paper
    /// and the *sunlight* on black -- the same information written in opposite
    /// directions. Everything else on the map is a line, and a line is a line
    /// in either theme.
    pub theme: Theme,
}

#[derive(Default)]
pub struct Relief {
    /// Smoothed height per sample, metres.
    h: Vec<f32>,
    /// Where each sample landed on the ground.
    world: Vec<[f64; 2]>,
    /// Ground metres between this sample and the next along the row.
    span: Vec<f32>,
    /// Ground metres between this sample and the one a row nearer.
    ///
    /// A separate number from `span`, and it has to be. Under a tilt the grid
    /// is even on screen and nothing like even on the ground: at 69 degrees a
    /// row step covers 2.8 times what a column step does, and it grows with
    /// distance on top of that. Dividing the north-south rise by the east-west
    /// run overstates the slope by exactly that factor, which tips every
    /// surface normal towards the camera and skews the lighting -- the more
    /// the camera leans, the more wrong it gets.
    down: Vec<f32>,
    /// How far this sample stands above the lowest ground near it, metres.
    stands: Vec<f32>,
    /// Scratch for the separable minimum behind `stands`.
    low: Vec<f32>,
    gw: usize,
    gh: usize,
    /// The exposure chosen for the last frame drawn. Anything draped on the
    /// ground has to be lifted by exactly this or it will not sit on it.
    pub lift: Lift,
}

impl Relief {
    /// Draw the terrain surface. Returns the number of samples plotted.
    pub fn draw(&mut self, t: &Terrain, canvas: &mut Canvas, vp: &Viewport, plot: Plot) -> usize {
        self.lay_out(canvas, vp);
        self.sample(t);
        self.measure_relief();
        self.lift = self.expose(vp, canvas.sh as f64);
        self.paint(canvas, vp, plot)
    }

    /// Unproject the screen grid onto the ground and note how far apart the
    /// samples landed.
    fn lay_out(&mut self, canvas: &Canvas, vp: &Viewport) {
        self.gw = (canvas.sw as f64 / STEP).ceil() as usize + 3;
        // Past the bottom of the frame, by the same headroom the exposure can
        // spend. Every sample is drawn above where it was taken -- that is
        // what the lift does -- so a grid that stops at the last row on screen
        // leaves the foreground empty by however much it lifted. Over Spiti at
        // 69 degrees the bottom 45% of the frame had nothing in it, and the
        // ground that belonged there had never been sampled.
        let reach = canvas.sh as f64 * (1.0 + crate::geo::LIFT_HEADROOM);
        self.gh = (reach / STEP).ceil() as usize + 3;
        let n = self.gw * self.gh;
        self.world.clear();
        self.world.resize(n, [0.0; 2]);
        self.span.clear();
        self.span.resize(n, 0.0);
        self.down.clear();
        self.down.resize(n, 0.0);

        for gy in 0..self.gh {
            for gx in 0..self.gw {
                // Odd rows offset by half a sample. A square grid turns every
                // steep slope into aligned vertical ribbons at this
                // resolution; a triangular one has the same sample count and
                // spacing and no screen-wide column alias.
                let stagger = if gy & 1 == 0 { 0.0 } else { STEP * 0.5 };
                let sx = (gx as f64 - 1.0) * STEP + stagger;
                let sy = (gy as f64 - 1.0) * STEP;
                self.world[gy * self.gw + gx] = vp.unproject([sx, sy]);
            }
        }

        // Spacing on the ground, which under a tilt is nothing like uniform
        // even though the grid on screen is: the far rows are foreshortened,
        // so each of them stands for far more ground, and each therefore wants
        // a coarser surface. This is where the level of detail comes from.
        for gy in 0..self.gh {
            for gx in 0..self.gw {
                let i = gy * self.gw + gx;
                let (_, lat) = world_to_lonlat(self.world[i][0], self.world[i][1]);
                let m = meters_per_world_unit(lat);
                let gap = |j: usize| {
                    let (a, b) = (self.world[i], self.world[j]);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    ((dx * dx + dy * dy).sqrt() * m) as f32
                };
                self.span[i] = gap(if gx + 1 < self.gw { i + 1 } else { i - 1 });
                // Two rows, halved -- not one row. Odd rows are staggered half
                // a sample sideways, so the step to the *next* row is diagonal
                // and comes out 12% long. Rows two apart share a stagger, the
                // sideways part cancels, and two apart is exactly what the
                // central difference below spans anyway.
                let two = self.gw * 2;
                self.down[i] = gap(if gy + 2 < self.gh { i + two } else { i - two }) * 0.5;
            }
        }
    }

    /// Ask the heightmap for a surface at each sample's own scale.
    fn sample(&mut self, t: &Terrain) {
        self.h.clear();
        self.h.resize(self.gw * self.gh, 0.0);
        for i in 0..self.h.len() {
            let (lon, lat) = world_to_lonlat(self.world[i][0], self.world[i][1]);
            // The two spacings in geometric mean, because the kernel is round
            // and the footprint under a tilt is not. Taking the column spacing
            // alone -- the smaller of the two -- under-filters along the rows
            // and lets the far field alias again.
            let foot = (self.span[i] as f64 * self.down[i].max(1.0) as f64).sqrt();
            self.h[i] = t.sample_smooth(lon, lat, foot * SMOOTH);
        }
    }

    /// How far each sample stands above the ground around it.
    ///
    /// Off the sampled grid rather than out of the heightmap: the neighbours
    /// are already in hand, and because the grid is screen-space the window is
    /// automatically the right size in ground terms at every zoom. The first
    /// version of this measured a fixed number of *grid steps*, which spans
    /// twelve kilometres at street zoom and a hundred and twenty at country
    /// zoom -- and over a hundred and twenty kilometres almost everywhere in
    /// India stands thirty metres above something, so it filtered nothing.
    fn measure_relief(&mut self) {
        /// Half-width of the window, in grid steps.
        const R: isize = 3;
        let (gw, gh) = (self.gw as isize, self.gh as isize);
        let n = self.gw * self.gh;
        self.stands.clear();
        self.stands.resize(n, 0.0);
        self.low.clear();
        self.low.resize(n, 0.0);

        // Separably: the lowest point in a square is the lowest of the row
        // minima, so this is two passes of seven taps rather than one of
        // forty-nine. Not a micro-optimisation -- the square version was
        // 5.2 ms of a 10 ms frame over Spiti, more than half of it, and the
        // grid it runs over got 75% bigger when the foreground was extended.
        for gy in 0..gh {
            for gx in 0..gw {
                let mut low = f32::MAX;
                for dx in -R..=R {
                    let x = (gx + dx).clamp(0, gw - 1);
                    low = low.min(self.h[(gy * gw + x) as usize]);
                }
                self.low[(gy * gw + gx) as usize] = low;
            }
        }
        for gy in 0..gh {
            for gx in 0..gw {
                let mut low = f32::MAX;
                for dy in -R..=R {
                    let y = (gy + dy).clamp(0, gh - 1);
                    low = low.min(self.low[(y * gw + gx) as usize]);
                }
                let i = (gy * gw + gx) as usize;
                self.stands[i] = self.h[i] - low;
            }
        }
    }


    /// Choose a datum and an exaggeration from the ground in view.
    ///
    /// Only ground that is actually on the slab counts. The screen grid runs
    /// to the edges of the canvas, and under a tilt its top rows unproject to
    /// points far past the horizon -- which are not in view, and one of which
    /// landing on a peak would set the exposure for the whole frame.
    fn expose(&self, vp: &Viewport, sh: f64) -> Lift {
        let bounded = vp.bounded();
        let plate = vp.plate();
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for (i, &h) in self.h.iter().enumerate() {
            if bounded {
                let m = vp.plane_of(self.world[i]);
                if m[0].abs() > plate[0] || m[1] < plate[1] || m[1] > plate[2] {
                    continue;
                }
            }
            lo = lo.min(h);
            hi = hi.max(h);
        }
        if lo > hi {
            return Lift::default();
        }
        let relief = (hi - lo) as f64;
        let flat = Lift { datum: lo, exag: EXAG_MIN, relief: relief as f32 };
        // Nothing that is not a landform gets exposed for. The same threshold
        // that decides whether a sample is drawn at all, so the two cannot
        // disagree: a frame with no mountain in it must not be given the
        // exaggeration a mountain would have got.
        if relief < MOUNTAIN as f64 {
            return flat;
        }
        let (_, clat) = world_to_lonlat(vp.center[0], vp.center[1]);
        // Screen rows the relief would occupy with no exaggeration at all.
        // Through the same terms `project3` uses for the height of a point --
        // a tilt of 50 degrees puts only 77% of a rise on the screen, and
        // exposing against the unforeshortened figure quietly aims low.
        let st = vp.tilt.sin();
        let flat_px =
            relief / meters_per_world_unit(clat) * vp.scale() * st * crate::geo::PIXEL_ASPECT;
        if flat_px < 1e-6 {
            return flat;
        }
        Lift { exag: (sh * FILL / flat_px).clamp(EXAG_MIN, EXAG_MAX), ..flat }
    }

    fn paint(&mut self, canvas: &mut Canvas, vp: &Viewport, plot: Plot) -> usize {
        let bounded = vp.bounded();
        let plate = vp.plate();
        let (_, clat) = world_to_lonlat(vp.center[0], vp.center[1]);
        let m_per_world = meters_per_world_unit(clat);
        let Lift { datum, exag, .. } = self.lift;
        let exag = if vp.is_flat() { 0.0 } else { exag };
        // Ground drawn at full strength is a surface and takes what is behind
        // it away. Ground still fading in is a hint, and a hint that deletes
        // the road behind it leaves a hole with nothing to explain the hole.
        let behind = if plot.strength >= 1.0 { Behind::Hide } else { Behind::Veil };
        let (sb64, cb64) = vp.bearing.sin_cos();
        let (bearing_sin, bearing_cos) = (sb64 as f32, cb64 as f32);
        let half = (STEP * 0.5).ceil() as isize;
        let mut plotted = 0usize;

        for gx in 1..self.gw - 1 {
            for gy in near_to_far(self.gh) {
                let i = gy * self.gw + gx;
                // Sea level is not terrain; the water layer already owns it.
                if self.h[i] < 1.0 || self.stands[i] < MOUNTAIN {
                    continue;
                }

                // Outside the slab there is no ground, so the plate keeps a
                // clean edge and the tilt gets the horizon that makes it
                // legible.
                let mut fade = 1.0f32;
                if bounded {
                    let m = vp.plane_of(self.world[i]);
                    if m[0].abs() > plate[0] || m[1] < plate[1] || m[1] > plate[2] {
                        continue;
                    }
                    fade = crate::scene::plate_fade(m, plate);
                    if fade <= 0.02 {
                        continue;
                    }
                }

                // Central differences off the grid, in rise over run. Safe to
                // take across two steps because the surface was low-passed to
                // three and a half of them: there is no energy left at this
                // scale for the difference to pick up as noise.
                //
                // Each direction over its own run. They are the same length on
                // screen and not on the ground.
                let across = (self.h[i + 1] - self.h[i - 1]) / (self.span[i].max(1.0) * 2.0);
                // Row 0 is the top of the screen, so a step down a row is a
                // step *towards* the camera -- away from the horizon. Hence
                // the sign: this is the rise going away from the camera.
                let along = (self.h[i - self.gw] - self.h[i + self.gw])
                    / (self.down[i].max(1.0) * 2.0);

                let (dzdx, dzdy) = to_compass(across, along, bearing_sin, bearing_cos);
                let (dzdx, dzdy) = (dzdx * SHADE_EXAG, dzdy * SHADE_EXAG);

                let (nx, ny, nz) = (-dzdx, -dzdy, 1.0f32);
                let len = (nx * nx + ny * ny + 1.0).sqrt();
                let lambert =
                    ((nx * LIGHT[0] + ny * LIGHT[1] + nz * LIGHT[2]) / len).clamp(0.0, 1.0);

                // Light carries the form, and slope only decides whether
                // there is a landform here to light.
                //
                // The first version had these the other way round -- ink
                // driven by slope, because slope means the same thing in both
                // themes and needs no inverting. It renders a uniform wash:
                // over Himachal the mean slope saturates the ramp almost
                // everywhere, so lit faces and shadowed ones come out at the
                // same weight and the mountains have no relief at all. Shape
                // comes from the difference between a face turned towards the
                // light and one turned away, and nothing else here can supply
                // it.
                //
                // Only half the ground is ever drawn, and that is the point:
                // on black the sunlit faces are the mark and the shadowed ones
                // are the page, on paper the other way about. Either way, the
                // faces turned the other way and everything level are the
                // background -- which is what gives the mountain an outside.
                let lit = match plot.theme {
                    Theme::Night => (lambert - FLAT_LIT) / (1.0 - FLAT_LIT),
                    Theme::Paper => (FLAT_LIT - lambert) / FLAT_LIT,
                }
                .clamp(0.0, 1.0);
                let slope = (dzdx * dzdx + dzdy * dzdy).sqrt();
                let gate = (slope / GATE_SLOPE).clamp(0.0, 1.0);
                // Weighted towards the light end. Lambert is linear in the
                // cosine, which spends most of its range on faces barely
                // turned from level -- so a linear map puts almost every lit
                // face at a quarter strength and the mountain comes out as a
                // scatter of specks instead of a face.
                let mark = gate * lit.powf(0.6);
                let alpha = mark * fade * plot.strength;

                let lift = |k: usize| (self.h[k] - datum) as f64 * exag / m_per_world;
                let (sp, depth) = vp.project3(self.world[i], lift(i));
                if !depth.is_finite() {
                    continue;
                }
                // The ribbon spans to the next row nearer, so the surface is
                // continuous instead of a field of specks -- and, being
                // continuous, it can occlude.
                let j = i + self.gw;
                let (near, dn) = vp.project3(self.world[j], lift(j));
                if !dn.is_finite() {
                    continue;
                }

                let brush = Brush { depth, tint: TINT_GREEN, mat: MAT_DOT, pick: u32::MAX, behind };

                // Between the two, in whichever order they came out. The first
                // version spanned downwards only, and a slope rising towards
                // the camera projects its nearer sample *higher* -- so every
                // such slope was a row of gaps.
                let (y0, y1) = (sp[1].min(near[1]), sp[1].max(near[1]));
                // A span longer than the screen is a sample thrown past the
                // horizon; drawing it paints a column across everything.
                // At least the grid step, so consecutive rows always touch:
                // the surface has to be continuous before it can occlude, and
                // a ribbon shorter than the spacing leaves a seam per row.
                let steps = ((y1 - y0).max(STEP).min(canvas.sh as f64)).ceil() as usize;
                for s in 0..steps {
                    let y = y0 + s as f64;
                    // Ink only where there is something to see -- but the
                    // ground is opaque either way. Skipping the whole sample
                    // when its face is dark is what left the mountains as
                    // strung-out dotted lines with the map showing through:
                    // over Himachal at 50 degrees that was seven thousand
                    // samples of solid rock that occluded nothing.
                    if alpha >= 0.02 {
                        canvas.splat(sp[0], y, alpha, &brush);
                    }
                    // Opaque from the gaps too, not only where a dot landed:
                    // the stipple is how the surface is *drawn*, not how much
                    // of the ground it covers.
                    let iy = y.round() as isize;
                    for dx in -half..=half {
                        canvas.occlude_at(sp[0].round() as isize + dx, iy, depth);
                    }
                }
                if alpha >= 0.02 {
                    plotted += 1;
                }
            }
        }
        plotted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::Terrain;
    use std::io::Write;

    /// A ridge running east-west across two degrees, `peak` metres above its
    /// valleys, on a heightmap centred where the tests put the camera.
    fn ridge(name: &str, peak: f32) -> Terrain {
        let side = 256usize;
        // Named per test: these run in parallel, and two of them baking the
        // same path had one reading the file while the other was truncating
        // it -- which surfaces as "memory map offset is larger than length"
        // and not as anything that points at the cause.
        let p = std::env::temp_dir()
            .join(format!("termap-relief-{}-{name}.tmhg", std::process::id()));
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TMHG");
        buf.push(1);
        buf.extend_from_slice(&[0u8; 3]);
        for v in [76.0f64, 31.0, 78.0, 33.0] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        buf.extend_from_slice(&(side as u32).to_le_bytes());
        buf.extend_from_slice(&(side as u32).to_le_bytes());
        for y in 0..side {
            // Nine cycles across the two degrees, so the wavelength is about
            // 24 km and every zoom in the tests below has whole ridges in
            // frame rather than one flank of one.
            let t = (y as f32 / side as f32) * std::f32::consts::PI * 18.0;
            let h = (peak * 0.5) * (1.0 + t.cos());
            for _ in 0..side {
                buf.extend_from_slice(&(h.round() as i16).to_le_bytes());
            }
        }
        std::fs::File::create(&p).unwrap().write_all(&buf).unwrap();
        Terrain::open(&p).unwrap()
    }

    fn camera(zoom: f64, tilt_deg: f64) -> Viewport {
        let mut vp = Viewport::new(crate::geo::lonlat_to_world(77.0, 32.0), zoom);
        vp.sw = 400.0;
        vp.sh = 200.0;
        vp.tilt = tilt_deg.to_radians();
        vp
    }

    /// Set up a pass far enough to have chosen an exposure.
    fn exposed(t: &Terrain, vp: &Viewport) -> Lift {
        let mut r = Relief::default();
        let canvas = Canvas::new(vp.sw as usize / SUB, vp.sh as usize / 4);
        r.lay_out(&canvas, vp);
        r.sample(t);
        r.expose(vp, vp.sh)
    }

    /// Cells per screen subpixel across, only so the test canvas comes out the
    /// size the viewport says.
    const SUB: usize = crate::canvas::SUB_X;

    /// The exposure has one job: put the relief in frame at a known share of
    /// the frame, whatever the zoom, the tilt or the mountain.
    ///
    /// It replaced a fixed ladder that was wrong at both ends -- fourteen
    /// times, which is right at 180 km and throws the peaks five hundred rows
    /// off a two-hundred-row canvas at 45 km. So what is checked is the thing
    /// the ladder could not do: that the answer lands in the same place from
    /// very different starting points.
    #[test]
    fn the_exposure_puts_the_mountain_in_the_frame() {
        let t = ridge("exposure", 4000.0);
        for (zoom, tilt) in [(8.0, 45.0), (9.5, 45.0), (11.0, 45.0), (9.5, 25.0), (9.5, 65.0)] {
            let vp = camera(zoom, tilt);
            let lift = exposed(&t, &vp);
            // Screen rows the exposed relief actually occupies, through the
            // same terms `project3` uses for the height of a point.
            let relief = lift.relief as f64 / crate::geo::meters_per_world_unit(32.0);
            let rows = relief * lift.exag * vp.scale() * vp.tilt.sin() * crate::geo::PIXEL_ASPECT;
            let want = vp.sh * FILL;
            assert!(
                (rows - want).abs() < want * 0.25,
                "z{zoom} tilt{tilt}: {rows:.0} rows against {want:.0} wanted \
                 (exag {:.1}, relief {:.0} m)",
                lift.exag,
                lift.relief
            );
        }
    }

    /// The sun does not go round with the camera.
    ///
    /// A hillside has one aspect. Turn the camera and the *screen* slope
    /// changes -- what ran across the frame now runs into it -- but the
    /// compass slope, and so the shade, must not. Without the rotation the
    /// light is nailed to the frame instead of to the map, and rotating the
    /// view repaints every mountain.
    #[test]
    fn turning_the_camera_does_not_move_the_sun() {
        // A slope falling to the south-east, in compass terms.
        let (east, north) = (0.4f32, -0.25f32);
        for deg in [0.0f32, 30.0, 90.0, 180.0, 270.0] {
            let (s, c) = deg.to_radians().sin_cos();
            // What the screen grid would measure at this bearing: the compass
            // slope projected onto the camera's own axes.
            let across = east * c - north * s;
            let along = east * s + north * c;
            let (gx, gy) = to_compass(across, along, s, c);
            assert!(
                (gx - east).abs() < 1e-5 && (gy - north).abs() < 1e-5,
                "bearing {deg}: ({gx:.4}, {gy:.4}) against ({east}, {north})"
            );
        }
    }

    /// A row step and a column step are the same on screen and not on the
    /// ground, and the shading has to divide each rise by its own run.
    ///
    /// Under a tilt the grid is foreshortened away from the camera, so a row
    /// step covers more ground than a column step by roughly `1/cos(tilt)`.
    /// Dividing the north-south rise by the east-west run overstates that
    /// slope by the same factor, tipping every normal towards the camera.
    #[test]
    fn a_row_step_covers_more_ground_than_a_column_step() {
        let t = ridge("spacing", 4000.0);
        for tilt in [0.0f64, 45.0, 69.0] {
            let vp = camera(9.5, tilt);
            let mut r = Relief::default();
            let canvas = Canvas::new(vp.sw as usize / SUB, vp.sh as usize / 4);
            r.lay_out(&canvas, &vp);
            r.sample(&t);
            // At the middle of the frame, where the foreshortening is the
            // plain `1/cos` with no perspective on top of it.
            let i = (r.gh / 2) * r.gw + r.gw / 2;
            let want = 1.0 / tilt.to_radians().cos();
            let got = (r.down[i] / r.span[i]) as f64;
            assert!(
                (got - want).abs() < want * 0.1,
                "tilt {tilt}: rows are {got:.2}x columns, expected {want:.2}x"
            );
        }
    }

    /// The separable minimum is the square one.
    ///
    /// It has to be exactly, not nearly: `stands` feeds the gate that decides
    /// whether a sample is a landform, so a disagreement shows up as terrain
    /// appearing and disappearing rather than as a slightly different tone.
    /// Clamping at the edges rather than skipping is safe for the same reason
    /// the split is -- a clamped index only repeats a value the window already
    /// holds, and a minimum does not count.
    #[test]
    fn the_split_window_finds_the_same_low_ground_as_a_square_one() {
        let t = ridge("window", 4000.0);
        let vp = camera(9.5, 45.0);
        let mut r = Relief::default();
        let canvas = Canvas::new(vp.sw as usize / SUB, vp.sh as usize / 4);
        r.lay_out(&canvas, &vp);
        r.sample(&t);
        r.measure_relief();

        let (gw, gh) = (r.gw as isize, r.gh as isize);
        for gy in (0..gh).step_by(7) {
            for gx in (0..gw).step_by(11) {
                let mut low = f32::MAX;
                for dy in -3..=3 {
                    for dx in -3..=3 {
                        let (y, x) = (gy + dy, gx + dx);
                        if y < 0 || y >= gh || x < 0 || x >= gw {
                            continue;
                        }
                        low = low.min(r.h[(y * gw + x) as usize]);
                    }
                }
                let i = (gy * gw + gx) as usize;
                let want = r.h[i] - low;
                assert!(
                    (r.stands[i] - want).abs() < 1e-3,
                    "({gx},{gy}): {} against {want}",
                    r.stands[i]
                );
            }
        }
    }

    /// Ground with no relief must not be inflated into fictional mountains.
    #[test]
    fn a_plain_is_left_a_plain() {
        let t = ridge("plain", 6.0);
        let lift = exposed(&t, &camera(9.5, 45.0));
        assert_eq!(
            lift.exag, EXAG_MIN,
            "{:.0} m of relief was stretched into a range",
            lift.relief
        );
    }

    /// The datum is the floor of what is in view, not sea level.
    ///
    /// Exaggerating above sea level slides the map up the screen wherever the
    /// land sits high: over a valley at 1500 m, fourteen times pushes the
    /// valley floor alone past the top of the frame before a mountain has been
    /// drawn at all.
    #[test]
    fn the_datum_sits_on_the_valley_floor_not_at_sea_level() {
        let t = ridge("datum", 4000.0);
        let lift = exposed(&t, &camera(9.5, 45.0));
        assert!(lift.datum > 1.0, "the datum went to sea: {}", lift.datum);
    }
}
