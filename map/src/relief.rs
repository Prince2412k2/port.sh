//! Terrain relief: the pass that makes a tilt read as 3D rather than as skew.
//!
//! A world-aligned grid is sampled from the heightmap, hillshaded from finite
//! differences, then projected with elevation. Drawing it as dots rather than a
//! surface is deliberate -- the stipple already reads as "ground" against the
//! solid strokes used for roads, and a dot grid needs no triangulation, no
//! backface culling and no seams.

use crate::canvas::{Brush, Canvas, MAT_DOT, SUB_X, SUB_Y, TINT_GREEN};
use crate::geo::{meters_per_world_unit, world_to_lonlat, Viewport};
use crate::terrain::Terrain;
use crate::view::Ground;

/// Subpixels between grid samples. Denser than this and the relief turns into a
/// solid wash that competes with the roads it is supposed to sit behind.
const STEP: f64 = 3.0;

/// Half the ground each sample is answerable for, in subpixels, rounded up.
const HALF_STEP: isize = (STEP as isize + 1) / 2;

/// Grid rows in the order they must be drawn: nearest first.
///
/// This is not a detail. The canvas composites alpha-over -- `cov += a * (1 -
/// cov)` -- so painting a near mark on top of a far one *adds* to the cell
/// instead of replacing it. Drawing far-to-near and trusting nearer ribbons to
/// "paint over" the ones behind, which is what this did, is only true of an
/// opaque renderer, and this one is not: the range behind a ridge stayed on
/// screen underneath it at reduced contrast.
///
/// Near first, and the depth buffer does the work instead. The front ridge
/// claims its subpixels, and everything behind arrives afterwards to find them
/// taken. Over Zanskar at 55 degrees that is 1874 cells of ink down to 1293.
fn near_to_far(gh: usize) -> impl Iterator<Item = usize> {
    (1..gh.saturating_sub(2)).rev()
}

/// Contour intervals to choose between, metres. Round numbers only: a map
/// whose lines are 37 m apart is a map nobody can count in their head.
const STEPS: [f32; 10] = [5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 250.0, 500.0, 1000.0];

/// Roughly how many lines should cross the visible range of elevation.
const WANT_LINES: f32 = 9.0;

/// Closest two contours are allowed to appear, in terminal rows.
///
/// The interval used to be chosen from the elevation range alone, which fixes
/// how many lines cross the frame but says nothing about where they land. On a
/// steep face the same interval bunches them into adjacent rows and the lines
/// stop being readable as lines -- that is the mush: a hundred iso-lines a
/// row apart is a texture, not a contour map.
///
/// So the question is asked in the units it is actually answered in. How many
/// rows apart will these appear? If the answer is under this, the interval is
/// too fine for the screen whatever the elevation range says.
const MIN_ROWS: f32 = 5.0;

/// The interval for a given spread of elevation.
///
/// Chosen from the ground actually in frame rather than from the zoom, because
/// the same zoom over the Ghats and over Ahmedabad wants intervals an order of
/// magnitude apart -- 754 m of range against 21 m.
fn interval(range: f32) -> f32 {
    let want = (range / WANT_LINES).max(1.0);
    *STEPS.iter().find(|s| **s >= want).unwrap_or(&2000.0)
}

/// The contour interval, from the elevation range *and* the screen.
///
/// `interval` alone fixes how many lines cross the frame and says nothing
/// about where they land. On a steep face the same interval bunches them into
/// adjacent rows, and a hundred iso-lines a row apart is a texture rather than
/// a contour map -- which is most of what made these frames mush.
///
/// So the question gets asked in the units it is answered in: how many rows
/// apart will these actually appear? `steep_fall` is metres per grid step in
/// the vertical, taken at the steep end of the frame rather than the middle,
/// because bunching is a problem where the ground is steep and a median would
/// size the interval for the gentle majority and leave the faces packed.
fn screen_interval(range: f32, steep_fall: f32) -> f32 {
    // One grid step is `STEP / SUB_Y` of a terminal row.
    let m_per_row = steep_fall * SUB_Y as f32 / STEP as f32;
    let need = m_per_row * MIN_ROWS;
    interval(range).max(*STEPS.iter().find(|s| **s >= need).unwrap_or(&2000.0))
}

/// The least relief that counts as terrain worth drawing, metres.
///
/// About a ten-storey building, and the number came from the brief: show
/// something that would qualify as a mountain, not every undulation the
/// heightmap happens to record. It is the single biggest thing standing
/// between this renderer and a legible frame at region scale -- over Gujarat
/// and Rajasthan the ground moves by a few metres over kilometres, and drawing
/// a mark for every sample of it filled half of India with an even stipple
/// that said nothing except "there is ground here", which the reader already
/// knew.
///
/// Measured as how far a sample stands above the lowest ground within
/// `STANDS_R` grid steps, not as slope and not as absolute height. Slope would
/// keep a gently tilted plain and drop a cliff face seen edge-on; absolute
/// height would draw the whole Deccan plateau as though it were a mountain,
/// which is precisely the mistake -- a plateau is high ground, not a mountain,
/// and it looks like neither when it is drawn as texture.
const MOUNTAIN: f32 = 30.0;

/// Eight compass points, for the low-ground lookup.
const RING: [(f64, f64); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (0.7, 0.7),
    (0.7, -0.7),
    (-0.7, 0.7),
    (-0.7, -0.7),
];

/// How far away the local low point is looked for, metres.
///
/// A world distance and not a number of grid steps, which is the whole point
/// and was the bug in the first cut. The grid is screen-space, so a window of
/// four steps spans twelve kilometres at street zoom and a hundred and twenty
/// at country zoom -- and over a hundred and twenty kilometres almost
/// everywhere in India stands thirty metres above something. The test passed
/// everywhere and filtered nothing, which is exactly what the country-wide
/// frame looked like.
///
/// Three kilometres. Far enough that a real landform clears it, near enough
/// that a plain does not: measured off the shipped heightmap, ground near
/// Mahesana stands about 8 m over 3 km and the Aravalli edge about 180 m.
const STANDS_M: f64 = 3000.0;

/// Highest elevation used to normalise shading, metres.
const MAX_ELEV: f32 = 6000.0;

/// The light: north-west and 20 degrees up.
///
/// North-west because that is where every printed relief map has put the sun
/// for two hundred years -- lit from the other side the eye reads the ranges
/// inside out, valleys for ridges.
///
/// In *grid* space, and that is the same choice the hillshade already makes:
/// the light belongs to the page, not to the ground. Turn the map and the sun
/// stays over your left shoulder. A world-locked sun would swing round as you
/// rotate and invert the relief halfway through the turn.
///
/// Twenty degrees and not the forty-five a renderer would reach for first. A
/// shadow needs ground steeper than the sun is high, and the shipped heightmap
/// is 30 arcsec -- one sample every 853 m, which averages a cliff into a
/// slope. Measured over Zanskar, a 45 degree sun moved the frame by 0.4% and a
/// 20 degree sun by 8%: at forty-five there was nothing in the data steep
/// enough to block it. Low light is also what relief cartographers use, for
/// the same reason.
const SUN_RISE: f32 = 0.36;

/// Ordered dither for spending the light as stipple density.
///
/// Shadow has to be paid for in *dots*, not in brightness. The canvas lights a
/// braille dot at a fixed coverage threshold and then floors the cell at 40%
/// of full brightness, so dimming a mark to a quarter still draws it at over
/// half strength -- measured on the veil pass, halving alpha moved a Himalayan
/// frame by 0.04%. Removing the mark is the only thing the medium reads
/// reliably, and it is also how an engraver has always drawn shade: the same
/// line, further apart.
///
/// Ordered rather than random so the pattern holds still under redraw, and
/// indexed by grid position rather than screen position so it stays welded to
/// the ground as the camera moves.
const DITHER: [[f32; 4]; 4] = [
    [0.031, 0.531, 0.156, 0.656],
    [0.781, 0.281, 0.906, 0.406],
    [0.219, 0.719, 0.094, 0.594],
    [0.969, 0.469, 0.844, 0.344],
];

/// Steps the hillshade is quantised into, the first of which is blank page.
///
/// Five, because the terminal has no more than that to give: a shade block
/// ladder is four glyphs plus the empty cell. Fewer, larger steps also read
/// better than more, smaller ones -- a big jump in tone explains a change of
/// slope, and a small one is indistinguishable from the cell next to it.
const SHADE_STEPS: f32 = 5.0;

/// Coverage per step, which is what picks the glyph off the shade ladder.
/// Index 0 is never drawn: the darkest fifth of the ground is negative space.
const SHADE_ALPHA: [f32; 5] = [0.0, 0.18, 0.38, 0.62, 0.88];

/// How hard the light is stretched before it is stepped.
///
/// Lambert over an 850 m heightmap lives in a narrow band around `FLAT_LIGHT`,
/// so stepping it raw puts every level on the same step and the frame comes
/// out one flat tone. Exaggerated lighting is the standard answer in relief
/// cartography and it is more necessary here, not less.
const LIGHT_GAIN: f32 = 3.4;

/// Share of the frame's samples that are allowed a mass block.
///
/// A budget, not a threshold, and it is the mechanism that produces negative
/// space rather than hoping for it. Only the steepest third of the ground in
/// frame gets tone; a plain is page, and so is the gentle half of a range.
///
/// Absolute illumination cannot do this job. Flat ground faces the sun at
/// Lambert 0.7 -- brighter than most of a mountain -- so a hillshade keyed to
/// light alone paints a plain solid and a valley floor empty, which is exactly
/// backwards. What is being drawn here is *relief*: light chooses the tone,
/// slope chooses whether there is anything to tone at all.
const MASS_SHARE: f32 = 0.30;

/// Extra smoothing passes behind the mass layer, on top of the one every
/// derivative here gets. Enough that a hillside comes out as one region.
const MASS_BLUR: usize = 6;

/// Lambert of ground that is not sloped at all, which is where the ladder is
/// centred so that a face turned to the sun climbs and one turned away falls.
const FLAT_LIGHT: f32 = 0.70;

/// Share of samples that become ridge strokes, and valley strokes.
///
/// A rank rather than a curvature threshold, so the frame gets about the same
/// number of strokes whether it is over the Himalaya or the Deccan. Any fixed
/// cut-off draws nothing on one and a hedge on the other.
const RIDGE_SHARE: f32 = 0.07;
const VALLEY_SHARE: f32 = 0.05;

/// How far the skyline may jump between one grid column and the next before
/// the gap is read as the edge of the slab rather than as a cliff, in
/// subpixels.
const SKY_BREAK: f64 = 24.0;
const SKY_ALPHA: f32 = 0.98;

/// How far above the horizon ground has to stand to count as a peak rather
/// than as the cut edge of the sampled slab, in subpixels.
const SKY_RISE: f64 = 3.0;

const RIDGE_ALPHA: f32 = 0.92;
const VALLEY_ALPHA: f32 = 0.30;

/// What is left of the light inside a full shadow.
///
/// Not zero. A shadowed hillside on a clear day is lit by the sky, and a
/// terminal has so little tonal room that taking a region to nothing loses the
/// shape of the ground rather than describing it.
const AMBIENT: f32 = 0.28;


/// What a relief pass needs to know besides the ground itself: where zero is,
/// how far the height is stretched, which way of drawing to use, and how
/// firmly to draw at all.
///
/// A struct rather than four more parameters because the contour and hachure
/// passes want the same four, and threading them one by one down two levels
/// was how the signatures got to eight arguments.
#[derive(Clone, Copy)]
pub struct Plot {
    /// Elevation treated as ground level, metres.
    pub datum: f32,
    /// Vertical exaggeration.
    pub exag: f64,
    /// How the surface is drawn.
    pub ground: Ground,
    /// 0 to 1: see `view::ground_strength`.
    pub strength: f32,
}

/// The same, once the pass has resolved what the camera does to it.
#[derive(Clone, Copy)]
struct Lift {
    datum: f32,
    exag: f64,
    m_per_world: f64,
    strength: f32,
}

#[derive(Default)]
pub struct Relief {
    /// Sample heights, reused across frames to avoid reallocating.
    heights: Vec<f32>,
    /// How much of the light each sample keeps: 1 lit, `AMBIENT` in shadow.
    light: Vec<f32>,
    /// Local relief per sample, metres: how far this ground stands above the
    /// lowest ground near it. Below `MOUNTAIN` it is not drawn at all.
    stands: Vec<f32>,
    /// Curvature per sample: positive on a crest, negative in a hollow.
    lap: Vec<f32>,
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
        plot: Plot,
    ) -> usize {
        let Plot { datum, ground, strength, .. } = plot;
        // Sampling in screen space rather than world space keeps the grid
        // uniform on the display no matter how the camera is turned.
        self.gw = (canvas.sw as f64 / STEP).ceil() as usize + 3;
        self.gh = (canvas.sh as f64 / STEP).ceil() as usize + 3;
        self.heights.clear();
        self.heights.resize(self.gw * self.gh, 0.0);
        self.stands.clear();
        self.stands.resize(self.gw * self.gh, 0.0);

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
                let h = t.sample(lon, lat);
                self.heights[i] = h;
                // The ground this sample stands on, looked up straight out of
                // the heightmap at a fixed distance rather than off the
                // sampling grid, so the question stays the same question at
                // every zoom.
                let (dlon, dlat) = (
                    STANDS_M / (111_320.0 * lat.to_radians().cos().abs().max(0.05)),
                    STANDS_M / 110_540.0,
                );
                let mut floor = h;
                for (ox, oy) in RING {
                    floor = floor.min(t.sample(lon + ox * dlon, lat + oy * dlat));
                }
                self.stands[i] = h - floor;
            }
        }

        let (_, clat) = vp.center_lonlat();
        let m_per_world = meters_per_world_unit(clat);
        // Metres covered by one step of the sampling grid, which is what turns
        // the ray's rise into the same units the heightmap is in.
        let m_per_grid = STEP * m_per_world / vp.scale();
        self.cast_shadows(m_per_grid);
        // Flat views have no vertical axis to displace along, and `Shade` is the
        // mode that chooses not to use the one it has.
        let exag = if vp.is_flat() || !ground.displaces() { 0.0 } else { plot.exag };
        let lift = Lift { datum, exag, m_per_world, strength };
        // Fully drawn ground is a surface; ground fading in is not yet one.
        let solid = strength >= 1.0;
        let mut plotted = 0usize;

        // Column-major, marching far to near. That ordering is the whole trick:
        // a bare grid of dots has no surface to hide anything behind, so each
        // sample is drawn as a vertical ribbon down to its nearer neighbour,
        // and nearer ribbons paint over farther ones. Occlusion by a ridge then
        // falls out of the draw order without a visibility test.
        for gx in 1..self.gw - 1 {
            for gy in near_to_far(self.gh) {
                let i = gy * self.gw + gx;
                let h = self.heights[i];
                // Sea level is not terrain; the water layer already owns it.
                if h < 1.0 {
                    continue;
                }
                // Ground that stands over nothing is not a landform. It is not
                // occluded either -- skipping the depth ribbon as well as the
                // stipple, so that flat ground is properly absent rather than
                // an invisible wall that punches holes in the roads behind it.
                if self.stands[i] < MOUNTAIN {
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
                // Ground a ridge is standing in front of. Ink is light on this
                // map, so a shadow takes ink away -- the stipple thins out and
                // the shadow reads as the dark shape it is.
                // Shadow is spent as density first and brightness second: the
                // stipple opens up where a ridge stands in the way, and the
                // dots that survive are dimmer too.
                let lit = self.light[i];
                let shaded = lit < 1.0 && DITHER[gy & 3][gx & 3] >= lit;
                let alpha = (0.10 + 0.80 * relief) * (0.55 + 0.45 * band) * lit * fade * strength;

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
                    // Ground drawn at full strength is opaque and takes what
                    // is behind it away. Ground still coming up is a hint, and
                    // a hint that deletes the road behind it leaves a hole
                    // with nothing visible to explain the hole.
                    behind: if solid {
                        crate::canvas::Behind::Hide
                    } else {
                        crate::canvas::Behind::Veil
                    },
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
                while ground.paints_surface() && !shaded && py <= paint_to {
                    let t = (py - y0) / span;
                    let x = sp[0] + (sp_near[0] - sp[0]) * t;
                    canvas.splat(x, py, alpha, &brush);
                    py += 1.0;
                }
                let mut y = y0;
                while solid && y <= y1 {
                    // Join the projected samples, not merely their y values.
                    // With the stagger above, fixing x here would recreate the
                    // vertical ribbons the sampling pattern exists to remove.
                    let t = (y - y0) / span;
                    let x = sp[0] + (sp_near[0] - sp[0]) * t;
                    // The ribbon is opaque ground whether or not the stipple
                    // happened to paint here, so it claims the whole width it
                    // is responsible for. Two columns was not that: samples sit
                    // `STEP` subpixels apart and each was claiming two, so a
                    // third of every row had no ground in the depth buffer at
                    // all and the range behind showed through the gaps.
                    let xi = x as isize;
                    for dx in -HALF_STEP..=HALF_STEP {
                        canvas.occlude_at(xi + dx, y as isize, depth);
                    }
                    y += 1.0;
                }
                plotted += 1;
            }
        }

        if ground == Ground::Massif {
            plotted = self.massif(canvas, vp, &world, &lift);
        }
        if ground == Ground::Contour {
            self.contours(canvas, vp, &world, &lift);
        }
        if ground == Ground::Hachure {
            plotted = self.hachures(canvas, vp, &world, &lift);
        }
        plotted
    }

    /// Terrain as a picture of a mountain: masses, ridges, a few contours.
    ///
    /// The reasoning, because it is a deliberate loss of information and the
    /// next person to read this will want to put it back.
    ///
    /// A city renders well as one mark per feature because roads and buildings
    /// are *sparse*: the geometry the marks make is the information. Ground has
    /// no such gaps. Every sample has an elevation, so one-mark-per-sample
    /// fills the frame with marks of equal weight, and a frame of equal marks
    /// has no shape in it -- the eye reads texture and stops looking. The
    /// ribbon mode drew 5186 cells of a 6000-cell frame over Zanskar. That is
    /// not a map of a mountain range, it is a grey rectangle with a coastline.
    ///
    /// So: three layers, in the order the eye wants them, and nothing else.
    ///
    /// 1. Light and shade as *blocks*, quantised to five steps. Tone, not
    ///    stipple. A cell of `▒` reads as a surface at a brightness; eight
    ///    scattered braille dots read as eight things. Five steps and not
    ///    two hundred because the limitation is the point -- a big step in
    ///    brightness explains geometry, and a small one is noise the terminal
    ///    cannot resolve anyway.
    /// 2. Ridges as continuous strokes at the highest contrast on the frame.
    ///    This is the silhouette, and it is the thing that was missing: the
    ///    old frames had thousands of marks and no line your eye could follow
    ///    along a range.
    /// 3. Contours, thinned in *screen* space, as occasional reference.
    ///
    /// The brightest step of the shading is drawn as nothing at all. Empty
    /// cells are part of the vocabulary here, not a failure to draw.
    fn massif(
        &mut self,
        canvas: &mut Canvas,
        vp: &Viewport,
        world: &[[f64; 2]],
        lift: &Lift,
    ) -> usize {
        let &Lift { datum, exag, m_per_world, strength } = lift;

        // Derivatives off a smoothed copy. Curvature is the second difference
        // and the second difference of a bilinear interpolation of 850 m
        // samples is mostly the seams between heightmap cells; unsmoothed,
        // ridge detection finds the grid rather than the mountain.
        let blur = |src: &Vec<f32>, gw: usize, gh: usize| {
            let mut out = src.clone();
            for gy in 1..gh - 1 {
                for gx in 1..gw - 1 {
                    let i = gy * gw + gx;
                    out[i] = 0.5 * src[i]
                        + 0.125 * (src[i - 1] + src[i + 1] + src[i - gw] + src[i + gw]);
                }
            }
            out
        };
        let sm = blur(&self.heights, self.gw, self.gh);
        // A second, much broader field for the masses.
        //
        // One pass is right for curvature -- a ridge is a mid-frequency thing
        // and over-smoothing walks the crest off the crest. It is wrong for
        // tone. Light computed at grid resolution flips between neighbouring
        // cells, so "the steepest third" comes out as a checkerboard of lit
        // and unlit squares: the frame gets its negative space and spends it
        // on noise. Masses are the *low-frequency* layer -- that is what makes
        // them masses -- so they get a field smoothed until the regions it
        // picks out are contiguous enough to read as one hillside.
        let mut broad = sm.clone();
        for _ in 0..MASS_BLUR {
            broad = blur(&broad, self.gw, self.gh);
        }

        // The ridge threshold is a *rank*, not a number of metres. Curvature
        // in the Himalaya and curvature over the Deccan differ by an order of
        // magnitude, and any fixed cut-off draws either nothing on one and a
        // hedge on the other. Taking the top slice means the frame gets about
        // the same number of strokes wherever it is pointed, which is the
        // whole idea of budgeting marks by what the screen can hold.
        let mut curve: Vec<f32> = Vec::with_capacity(self.gw * self.gh);
        let mut steep: Vec<f32> = Vec::with_capacity(self.gw * self.gh);
        self.lap.clear();
        self.lap.resize(self.gw * self.gh, 0.0);
        for gy in 1..self.gh - 1 {
            for gx in 1..self.gw - 1 {
                let i = gy * self.gw + gx;
                if self.heights[i] < 1.0 || self.stands[i] < MOUNTAIN {
                    continue;
                }
                let l = 4.0 * sm[i]
                    - (sm[i - 1] + sm[i + 1] + sm[i - self.gw] + sm[i + self.gw]);
                self.lap[i] = l;
                curve.push(l);
                let (dx, dy) =
                    (broad[i + 1] - broad[i - 1], broad[i + self.gw] - broad[i - self.gw]);
                steep.push((dx * dx + dy * dy).sqrt());
            }
        }
        if curve.is_empty() {
            return 0;
        }
        curve.sort_by(f32::total_cmp);
        steep.sort_by(f32::total_cmp);
        let pick = |v: &Vec<f32>, q: f32| v[((v.len() - 1) as f32 * q) as usize];
        let ridge_at = pick(&curve, 1.0 - RIDGE_SHARE).max(1e-3);
        let valley_at = pick(&curve, VALLEY_SHARE).min(-1e-3);
        // The steepness a sample has to beat to be worth any tone at all.
        let mass_at = pick(&steep, 1.0 - MASS_SHARE).max(1e-3);

        let (mut masses, mut strokes) = (0usize, 0usize);
        // One shade block per cell. Without this a cell that two grid samples
        // land in gets painted twice, and alpha-over pushes it up a step --
        // the quantisation would be undone by the sampling.
        let mut taken = vec![false; canvas.cw * canvas.ch];

        for gy in 1..self.gh - 1 {
            for gx in 1..self.gw - 1 {
                let i = gy * self.gw + gx;
                let h = self.heights[i];
                if h < 1.0 || self.stands[i] < MOUNTAIN {
                    continue;
                }
                let hw = (h - datum) as f64 * exag / m_per_world;
                let (sp, depth) = vp.project3(world[i], hw);
                if !depth.is_finite() {
                    continue;
                }

                let dzdx = broad[i + 1] - broad[i - 1];
                let dzdy = broad[i + self.gw] - broad[i - self.gw];
                let nz = 60.0f32;
                let len = (dzdx * dzdx + dzdy * dzdy + nz * nz).sqrt().max(1e-6);
                let lambert = ((dzdx * 0.5 + dzdy * 0.5 + nz * 0.7) / len).clamp(0.0, 1.0);
                // Stretched before it is stepped. Real Lambert over ground this
                // coarse lives in a narrow band around the middle, and stepping
                // that band gives four levels that are all the same level.
                let lit =
                    (((lambert - FLAT_LIGHT) * LIGHT_GAIN + 0.5) * self.light[i]).clamp(0.0, 1.0);

                // Five steps, the first of which is the page -- and only the
                // steepest `MASS_SHARE` of the frame is in the running at all.
                let grade = (dzdx * dzdx + dzdy * dzdy).sqrt();
                let step = if grade < mass_at {
                    0
                } else {
                    (lit * SHADE_STEPS).floor().min(SHADE_STEPS - 1.0) as usize
                };
                if step > 0 {
                    let (cx, cy) = (sp[0] as isize / SUB_X as isize, sp[1] as isize / SUB_Y as isize);
                    if cx >= 0 && cy >= 0 && (cx as usize) < canvas.cw && (cy as usize) < canvas.ch {
                        let cell = cy as usize * canvas.cw + cx as usize;
                        if !taken[cell] {
                            taken[cell] = true;
                            let brush = Brush {
                                depth,
                                tint: TINT_GREEN,
                                mat: crate::canvas::MAT_SHADE,
                                pick: u32::MAX,
                                behind: crate::canvas::Behind::Hide,
                            };
                            // Fill the whole cell: the shade glyph is chosen
                            // from how much of the cell is covered, so a block
                            // level is only reachable by covering the block.
                            for sy in 0..SUB_Y {
                                for sx in 0..SUB_X {
                                    canvas.splat(
                                        (cx as usize * SUB_X + sx) as f64,
                                        (cy as usize * SUB_Y + sy) as f64,
                                        SHADE_ALPHA[step] * strength,
                                        &brush,
                                    );
                                }
                            }
                            masses += 1;
                        }
                    }
                }

                // Ridge and valley strokes, along the line of the landform
                // rather than across it: perpendicular to the gradient is the
                // direction a crest actually runs, so consecutive samples on
                // the same crest lay strokes end to end and the eye gets a
                // line to follow instead of a row of ticks.
                let l = self.lap[i];
                let (rdx, rdy) = (sm[i + 1] - sm[i - 1], sm[i + self.gw] - sm[i - self.gw]);
                // Non-maximum suppression across the landform, the same trick
                // an edge detector uses. Curvature above a threshold is a
                // *band* a few samples wide, and drawing all of it gives a
                // field of ticks with no line in it -- 744 of them on the
                // frame that prompted this. Keeping only the sample that is
                // the local maximum along the direction the ground falls
                // thins that band to one sample, and consecutive maxima along
                // the same crest then sit end to end and read as a line.
                if !self.is_crest(i, l) {
                    continue;
                }
                // Hatch, not block. A quadrant claims the whole cell as a
                // solid lump, and seven hundred lumps is a checkerboard however
                // well the ridges were detected. The hatch family picks its
                // glyph from the direction of the ink in the cell -- `╱ ╲ ─ │`
                // -- so a crest running north-east *looks* like it runs
                // north-east, and consecutive strokes along one read as a
                // continuous line rather than as a row of squares. Direction
                // is the thing a ridge has to communicate; mass is not.
                let (share, alpha, mat) = if l >= ridge_at {
                    (l / ridge_at, RIDGE_ALPHA, crate::canvas::MAT_HATCH)
                } else if l <= valley_at {
                    (l / valley_at, VALLEY_ALPHA, MAT_DOT)
                } else {
                    continue;
                };
                let rgrade = (rdx * rdx + rdy * rdy).sqrt();
                if rgrade < 1e-3 {
                    continue;
                }
                // Along the contour: turn the downhill vector a quarter turn.
                let (ax, ay) = (-rdy as f64 / rgrade as f64, rdx as f64 / rgrade as f64);
                let ex = [
                    (world[i + 1][0] - world[i - 1][0]) * 0.5,
                    (world[i + 1][1] - world[i - 1][1]) * 0.5,
                ];
                let ey = [
                    (world[i + self.gw][0] - world[i - self.gw][0]) * 0.5,
                    (world[i + self.gw][1] - world[i - self.gw][1]) * 0.5,
                ];
                let reach = 1.2;
                let ends = [-reach, reach].map(|r| {
                    [
                        world[i][0] + r * (ax * ex[0] + ay * ey[0]),
                        world[i][1] + r * (ax * ex[1] + ay * ey[1]),
                    ]
                });
                let (pa, da) = vp.project3(ends[0], hw);
                let (pb, db) = vp.project3(ends[1], hw);
                if !da.is_finite() || !db.is_finite() {
                    continue;
                }
                crate::raster::line(
                    canvas,
                    pa,
                    pb,
                    &crate::raster::Pen {
                        width: 1.0,
                        alpha: (alpha * share.clamp(1.0, 1.6)).clamp(0.0, 0.98) * strength,
                        depth: da.min(db),
                        tint: TINT_GREEN,
                        mat,
                        pick: u32::MAX,
                        behind: crate::canvas::Behind::Veil,
                    },
                );
                strokes += 1;
            }
        }

        strokes += self.skyline(canvas, vp, world, lift);
        self.contours(canvas, vp, world, lift);
        masses + strokes
    }

    /// The edge where the ground stops and the sky starts.
    ///
    /// The one cue the reference pictures lean on hardest and the one thing
    /// this renderer had no way to draw. Every mark in a frame of terrain says
    /// "there is ground here"; not one of them says "and none above this
    /// line", which is what actually makes a photograph of a mountain read as
    /// a mountain in one glance.
    ///
    /// It needs no detection and no threshold, which is the good part. Once
    /// the camera is tilted the skyline is just the upper envelope of the
    /// projected surface: walk each column of the sampling grid, keep the
    /// topmost point, join them up. Exact, and one pass over samples that have
    /// already been projected.
    ///
    /// Nothing is drawn in plan view, and that is not a limitation to fix
    /// later. Looking straight down there is no "behind the mountain" for a
    /// silhouette to be an edge against -- the frame is ground everywhere by
    /// definition. A skyline is a thing an oblique view has and a map does not.
    fn skyline(
        &self,
        canvas: &mut Canvas,
        vp: &Viewport,
        world: &[[f64; 2]],
        lift: &Lift,
    ) -> usize {
        if vp.is_flat() {
            return 0;
        }
        let &Lift { datum, exag, m_per_world, strength } = lift;
        let plate = vp.plate();

        // Topmost projected point per grid column: (screen point, depth).
        let mut sky: Vec<Option<([f64; 2], f32)>> = vec![None; self.gw];
        for (gx, top) in sky.iter_mut().enumerate() {
            for gy in 0..self.gh {
                let i = gy * self.gw + gx;
                let h = self.heights[i];
                if h < 1.0 || self.stands[i] < MOUNTAIN {
                    continue;
                }
                let m = vp.plane_of(world[i]);
                if m[0].abs() > plate[0] || m[1] < plate[1] || m[1] > plate[2] {
                    continue;
                }
                let hw = (h - datum) as f64 * exag / m_per_world;
                let (sp, depth) = vp.project3(world[i], hw);
                if !depth.is_finite() || !sp[1].is_finite() {
                    continue;
                }
                match *top {
                    Some((p, _)) if p[1] <= sp[1] => {}
                    _ => *top = Some((sp, depth)),
                }
            }
        }

        // Where the ground merely *stops* rather than rises.
        //
        // The slab is a rectangular clip in plane coords, so its far edge
        // projects to a straight line and the raw envelope is that line for
        // most of its length -- a ruled bar across the frame, which is what
        // the first version drew. That edge is a fact about the clip, not
        // about the mountain. The silhouette worth drawing is only the part
        // that stands above it, so the flat majority sets the datum and
        // anything that clears it by a margin is a peak.
        let mut level: Vec<f64> = sky.iter().flatten().map(|(p, _)| p[1]).collect();
        if level.is_empty() {
            return 0;
        }
        level.sort_by(f64::total_cmp);
        let horizon = level[level.len() / 2];

        let mut drawn = 0usize;
        for pair in sky.windows(2) {
            let [Some((a, da)), Some((b, db))] = pair else { continue };
            let (a, b, da, db) = (*a, *b, *da, *db);
            // Screen y grows downward, so above the horizon is a smaller y.
            if a[1] > horizon - SKY_RISE && b[1] > horizon - SKY_RISE {
                continue;
            }
            // A step taller than this is the edge of the sampled slab rather
            // than a cliff, and joining across it draws a wall out of nothing.
            if (b[1] - a[1]).abs() > SKY_BREAK {
                continue;
            }
            crate::raster::line(
                canvas,
                a,
                b,
                &crate::raster::Pen {
                    width: 1.0,
                    alpha: SKY_ALPHA * strength,
                    depth: da.min(db),
                    tint: TINT_GREEN,
                    // Solid, and the only place in the ground pass that asks
                    // for it. The skyline is the top of the hierarchy: if one
                    // mark on the frame is going to be a whole cell of ink,
                    // it is this one.
                    mat: crate::canvas::MAT_SOLID,
                    pick: u32::MAX,
                    // Nothing can be in front of the topmost thing in its
                    // column, so there is nothing to test against -- and a
                    // z-fight with the ground it is the edge of would break
                    // the line exactly where it matters.
                    behind: crate::canvas::Behind::Ignore,
                },
            );
            drawn += 1;
        }
        drawn
    }

    /// Is this the top of its ridge, measured across the ridge?
    ///
    /// Non-maximum suppression, the same step an edge detector uses: keep the
    /// sample only if nothing either side of it, *across* the landform, is more
    /// curved. Curvature above a threshold is a band several samples wide, and
    /// drawing the whole band gives a field of ticks with no line in it.
    ///
    /// "Across" comes from the Hessian, not from the gradient, and that is the
    /// whole difficulty. The obvious reading of "across the ridge" is "along
    /// the fall line", but the fall line is exactly what vanishes at a crest --
    /// the ground is level along the top of a ridge, so the gradient there is
    /// zero and has no direction to offer. Using it suppressed the one sample
    /// that mattered and kept its flanks; a test built from a plain roof
    /// caught it, because the apex is where the gradient is smallest.
    ///
    /// The principal axis of curvature has no such hole. It points across the
    /// landform whether or not the ground is falling.
    fn is_crest(&self, i: usize, l: f32) -> bool {
        let gw = self.gw;
        let h = &self.heights;
        let hxx = h[i + 1] - 2.0 * h[i] + h[i - 1];
        let hyy = h[i + gw] - 2.0 * h[i] + h[i - gw];
        let hxy = (h[i + gw + 1] - h[i + gw - 1] - h[i - gw + 1] + h[i - gw - 1]) * 0.25;
        if hxx.abs() + hyy.abs() + hxy.abs() < 1e-6 {
            return false;
        }
        let theta = 0.5 * (2.0 * hxy).atan2(hxx - hyy);
        // Rounded to a grid neighbour: at one sample of reach there is nothing
        // between the eight of them to interpolate towards.
        let (sx, sy) = (theta.cos().round() as isize, theta.sin().round() as isize);
        if sx == 0 && sy == 0 {
            return false;
        }
        let step = sy * gw as isize + sx;
        let (a, b) = (i as isize - step, i as isize + step);
        if a < 0 || b < 0 || a as usize >= self.lap.len() || b as usize >= self.lap.len() {
            return false;
        }
        let (la, lb) = (self.lap[a as usize], self.lap[b as usize]);
        if l >= 0.0 {
            l >= la && l >= lb
        } else {
            l <= la && l <= lb
        }
    }

    /// Which ground a ridge is standing in front of.
    ///
    /// Hillshade answers "which way is this bit of ground facing", which is a
    /// question about one sample on its own. It cannot say that a peak stands
    /// between the valley and the sun, because that is a question about every
    /// sample along a line -- so a hillshaded range comes out as a field of
    /// lit and unlit faces with no sense of one landform being in front of
    /// another. The cast shadow is what welds them together.
    ///
    /// A horizon sweep, not a ray march. The light runs exactly down the grid
    /// diagonal, so every sample that could shadow another lies on the same
    /// diagonal line, and one walk down each line settles the whole grid:
    /// carry the highest sun ray seen so far, drop it by `SUN_RISE` per step
    /// as it travels away from the light, and raise it wherever the ground
    /// pokes through. Ground below the carried ray is in shadow, by however
    /// far below. That is O(one visit per sample) against the march this
    /// replaced, which cost 56 lookups each and took the frame from 2.0 ms to
    /// 5.3 ms.
    ///
    /// The depth is graded rather than in-or-out. A hard test gives a shadow
    /// edge one sample wide, and at this resolution a one-sample edge crawls a
    /// whole cell at a time as the camera moves, which reads as tearing.
    fn cast_shadows(&mut self, m_per_grid: f64) {
        self.light.clear();
        self.light.resize(self.gw * self.gh, 1.0);
        if self.gw < 2 || self.gh < 2 {
            return;
        }
        // The diagonal is longer than the step, and the ray falls by distance.
        let drop = (m_per_grid * std::f64::consts::SQRT_2) as f32 * SUN_RISE;
        // How far under the ray the ground has to sit for the shadow to be
        // full. Tied to the sample spacing so the penumbra stays a fixed width
        // on screen instead of vanishing as you zoom in.
        let soft = (drop * 1.5).max(1.0);

        // Every diagonal running away from the light, which is up and to the
        // left: the starts are the top row and the left column.
        let starts = (0..self.gw).map(|x| (x, 0)).chain((1..self.gh).map(|y| (0, y)));
        for (sx, sy) in starts {
            let (mut x, mut y) = (sx, sy);
            let mut ray = f32::MIN;
            while x < self.gw && y < self.gh {
                let i = y * self.gw + x;
                let h = self.heights[i];
                ray -= drop;
                if h >= ray {
                    // This ground catches the light and becomes the new ray.
                    ray = h;
                } else {
                    let deep = ((ray - h) / soft).clamp(0.0, 1.0);
                    self.light[i] = 1.0 - (1.0 - AMBIENT) * deep;
                }
                x += 1;
                y += 1;
            }
        }
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
        lift: &Lift,
    ) -> usize {
        let &Lift { datum, exag, m_per_world, strength } = lift;
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
                if h < 1.0 || self.stands[i] < MOUNTAIN {
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
                        .clamp(0.08, 0.95)
                            * strength,
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
                        behind: crate::canvas::Behind::Veil,
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
        lift: &Lift,
    ) {
        let &Lift { datum, exag, m_per_world, strength } = lift;
        let (lo, hi) = self
            .heights
            .iter()
            .filter(|h| **h >= 1.0)
            .fold((f32::MAX, f32::MIN), |(a, b), h| (a.min(*h), b.max(*h)));
        if !(lo.is_finite() && hi.is_finite()) || hi - lo < 2.0 {
            return;
        }
        // Screen spacing, not just elevation spacing. `dh` per grid step in the
        // vertical taken at the steep end of the frame rather than the middle:
        // bunching is a problem *where the ground is steep*, and a median would
        // size the interval for the gentle majority and leave the faces packed.
        let mut fall: Vec<f32> = Vec::new();
        for gy in 1..self.gh - 1 {
            for gx in 0..self.gw {
                let i = gy * self.gw + gx;
                if self.heights[i] >= 1.0 {
                    fall.push((self.heights[i + self.gw] - self.heights[i - self.gw]).abs() * 0.5);
                }
            }
        }
        fall.sort_by(f32::total_cmp);
        let step = screen_interval(hi - lo, fall[(fall.len() - 1) * 3 / 4]);

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
                                alpha: if index { 0.55 } else { 0.28 } * strength,
                                depth: da.min(db),
                                tint: TINT_GREEN,
                                mat: MAT_DOT,
                                pick: u32::MAX,
                                behind: crate::canvas::Behind::Veil,
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
        // A number is either legible or it is litter, so heights wait until the
        // lines they belong to are properly drawn rather than fading up with
        // them.
        for &(_, _, level, cx, cy) in labelled.iter().filter(|_| strength >= 1.0) {
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

    /// A ridge puts the ground behind it in shadow, and the ground in front
    /// of it keeps the light.
    ///
    /// Built as a wall across the light's path with flat ground either side.
    /// The light runs down the grid diagonal from the upper left, so the far
    /// side of the wall -- down and to the right -- is the side that goes
    /// dark, and the near side must not.
    #[test]
    fn a_ridge_darkens_the_ground_behind_it_and_not_in_front() {
        let mut r = Relief { gw: 32, gh: 32, ..Default::default() };
        r.heights = vec![100.0; r.gw * r.gh];
        // A wall on the anti-diagonal, which is *across* the light rather than
        // along it. The first version of this test laid the wall down the same
        // diagonal the light runs on, so every sample it could have shadowed
        // was the wall itself, and nothing went dark.
        for x in 0..=8 {
            r.heights[(8 - x) * r.gw + x] = 900.0;
        }
        // 100 m a grid step, so the ray falls 100 * sqrt2 * 0.36 = 51 m a step
        // and the wall's 800 m of freeboard reaches about 16 steps.
        r.cast_shadows(100.0);

        let at = |x: usize, y: usize| r.light[y * r.gw + x];
        // Twelve steps down-light of the wall, still well inside its reach.
        assert!(at(16, 16) < 0.99, "behind the wall is lit at {}", at(16, 16));
        // Up-light of it: the wall is not between this ground and the sun.
        assert_eq!(at(2, 2), 1.0, "ground in front of the wall lost its light");
        // Twenty-four steps out, past where the ray has fallen back to ground.
        assert_eq!(at(28, 28), 1.0, "the shadow never ended");
    }

    /// The nearest row is drawn first, so the depth buffer can reject.
    ///
    /// The canvas is alpha-over, not opaque, so "nearer ribbons paint over
    /// farther ones" was never true -- a near mark laid on a far one adds to
    /// the cell. Hidden-surface removal here depends entirely on the near
    /// ground reaching the depth buffer before the far ground asks to draw.
    #[test]
    fn the_ground_nearest_the_camera_is_drawn_first() {
        let rows: Vec<usize> = near_to_far(12).collect();
        assert_eq!(rows.first(), Some(&9), "the first row drawn is not the nearest");
        assert_eq!(rows.last(), Some(&1), "the last row drawn is not the farthest");
        assert!(rows.windows(2).all(|w| w[0] > w[1]), "the order is not monotonic");
        // Degenerate grids must not wrap into an enormous range.
        assert_eq!(near_to_far(2).count(), 0);
    }

    /// The depth ribbon has to cover the ground between samples.
    ///
    /// Samples sit `STEP` subpixels apart and each one used to claim two
    /// columns, so a third of every row held no ground in the depth buffer and
    /// the range behind showed through the gap -- the stipple hid it well
    /// enough to look like dither rather than like a bug.
    #[test]
    fn the_depth_ribbon_leaves_no_column_unclaimed() {
        let claimed = 2 * HALF_STEP + 1;
        assert!(
            claimed >= STEP.ceil() as isize,
            "a sample claims {claimed} columns of the {STEP} it is answerable for"
        );
    }

    /// The sun has to be low, and the reason is the data, not the taste.
    ///
    /// A cast shadow needs ground steeper than the sun is high. The shipped
    /// heightmap is 30 arcsec -- 819 x 921 m a sample -- which averages a
    /// cliff into a slope, so a conventional 45 degree sun finds almost
    /// nothing steep enough to hide behind. Measured over Zanskar it moved the
    /// frame by 0.4%; at 20 degrees, by 8%.
    ///
    /// This guards the constant against being "corrected" back to 45.
    #[test]
    fn the_sun_is_lower_than_the_ground_the_heightmap_can_describe() {
        // The steepest the shipped grid can report between neighbours: a
        // Himalayan wall is about 2000 m over 5 km once sampled at 850 m.
        let steepest_the_data_holds = 2000.0f32 / 5000.0;
        assert!(
            SUN_RISE < steepest_the_data_holds,
            "a sun at {SUN_RISE} is steeper than any slope this heightmap has, \
             so nothing would ever cast a shadow"
        );
    }

    /// A steep frame gets a coarser interval than its elevation range asks
    /// for, because the range does not know how far apart the lines will land.
    #[test]
    fn the_interval_answers_in_rows_not_only_in_metres() {
        // The same 2000 m of range, over gentle ground and over a wall.
        let gentle = screen_interval(2000.0, 4.0);
        let steep = screen_interval(2000.0, 90.0);
        assert!(steep > gentle, "{steep} is not coarser than {gentle}");
        // And the coarse one actually delivers the spacing it was asked for.
        for fall in [4.0f32, 20.0, 90.0, 300.0] {
            let step = screen_interval(2000.0, fall);
            let rows = step / (fall * SUB_Y as f32 / STEP as f32);
            assert!(
                rows >= MIN_ROWS - 0.001,
                "a fall of {fall} m a step put contours {rows} rows apart"
            );
        }
    }

    /// Only the top of a ridge is the ridge.
    ///
    /// Curvature above a threshold is a band several samples wide. Drawing all
    /// of it gives a field of ticks with no line in it; keeping the local
    /// maximum across the fall thins it to something the eye can follow.
    #[test]
    fn only_the_crest_of_a_ridge_survives_the_thinning() {
        let mut r = Relief {
            gw: 9,
            gh: 9,
            // A roof: rises to the middle column and falls away again, so the
            // curvature band spans the whole width and the crest is column 4.
            heights: vec![0.0; 81],
            lap: vec![0.0; 81],
            ..Default::default()
        };
        for y in 0..9 {
            for x in 0..9 {
                let h = 100.0 - (x as f32 - 4.0).abs() * 20.0;
                r.heights[y * 9 + x] = h;
            }
        }
        for y in 1..8 {
            for x in 1..8 {
                let i = y * 9 + x;
                r.lap[i] = 4.0 * r.heights[i]
                    - (r.heights[i - 1] + r.heights[i + 1] + r.heights[i - 9] + r.heights[i + 9]);
            }
        }
        let crest = |x: usize| r.is_crest(4 * 9 + x, r.lap[4 * 9 + x]);
        assert!(crest(4), "the top of the roof was not taken as a crest");
        for x in [2, 3, 5, 6] {
            assert!(!crest(x), "the flank at {x} was taken as a crest");
        }
    }

    /// A tilted view of a range gets a skyline; a map looking straight down
    /// does not, and that is geometry rather than a gap.
    ///
    /// Counted off the resolved buffer rather than from the pass's return
    /// value, so what is asserted is that the silhouette reached the screen.
    /// Quadrant blocks are the tell: the skyline is the only thing the ground
    /// pass draws in `MAT_SOLID`, and there is no basemap in this test to
    /// contribute any.
    ///
    /// Run against the shipped heightmap over Zanskar, because the question is
    /// whether real ground produces an edge -- a synthetic hill would only
    /// prove the arithmetic, and the arithmetic was never the part in doubt.
    /// Skipped, not failed, where the data is absent.
    #[test]
    fn a_tilted_range_has_a_skyline_and_a_plan_view_has_none() {
        let Some(p) = crate::paths::data_file("india.tmhg") else { return };
        let t = crate::terrain::Terrain::open(&p).unwrap();

        let solids = |tilt: f64| {
            let mut vp = Viewport::new(crate::geo::lonlat_to_world(76.89, 33.47), 11.0);
            vp.sw = 300.0;
            vp.sh = 152.0;
            vp.tilt = tilt;
            let mut canvas = Canvas::new(150, 38);
            Relief::default().draw(
                &t,
                &mut canvas,
                &vp,
                Plot {
                    datum: 3500.0,
                    exag: crate::view::exaggeration(11.0),
                    ground: Ground::Massif,
                    strength: 1.0,
                },
            );
            let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(
                0, 0, 150, 38,
            ));
            canvas.resolve(
                &mut buf,
                ratatui::layout::Rect::new(0, 0, 150, 38),
                &crate::canvas::Fog::default(),
                true,
                crate::canvas::Theme::Night,
            );
            // Block elements, minus the shade ladder the mass layer draws
            // with. `░▒▓█` live in the same Unicode block as the quadrants,
            // so counting the block wholesale counted the masses too and the
            // plan view came back with 466 "silhouette" cells.
            buf.content()
                .iter()
                .filter(|c| {
                    c.symbol().chars().next().is_some_and(|ch| {
                        ('\u{2580}'..='\u{259F}').contains(&ch) && !"░▒▓█".contains(ch)
                    })
                })
                .count()
        };
        assert_eq!(solids(0.0), 0, "a plan view drew a silhouette against nothing");
        assert!(solids(30.0_f64.to_radians()) > 8, "a tilted range drew no skyline");
    }

    /// A plain is not terrain, and a mountain is.
    ///
    /// The threshold that decides it is measured against the shipped
    /// heightmap, because the whole question is whether real ground separates
    /// cleanly at thirty metres -- a synthetic hill would only restate the
    /// constant. Skipped, not failed, where the data is absent.
    #[test]
    fn a_plain_does_not_qualify_as_terrain_and_a_range_does() {
        let Some(p) = crate::paths::data_file("india.tmhg") else { return };
        let t = crate::terrain::Terrain::open(&p).unwrap();

        // How far ground stands over the low point 3 km around it -- the same
        // measure the renderer gates on.
        let stands = |lon: f64, lat: f64| {
            let h = t.sample(lon, lat);
            let dlon = STANDS_M / (111_320.0 * lat.to_radians().cos());
            let dlat = STANDS_M / 110_540.0;
            let mut floor = h;
            for (ox, oy) in RING {
                floor = floor.min(t.sample(lon + ox * dlon, lat + oy * dlat));
            }
            h - floor
        };
        // Mahesana and Radhanpur, on the north Gujarat plain -- the frame that
        // came back as an even stipple over everything.
        for (lon, lat, what) in [(72.40, 23.60, "Mahesana"), (71.60, 23.83, "Radhanpur")] {
            let s = stands(lon, lat);
            assert!(s < MOUNTAIN, "{what} stands {s} m and would be drawn as terrain");
        }
        // And the ground that should survive.
        for (lon, lat, what) in [(76.89, 33.47, "Zanskar"), (73.66, 19.94, "the Ghats")] {
            let s = stands(lon, lat);
            assert!(s >= MOUNTAIN, "{what} stands only {s} m and would be dropped");
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

