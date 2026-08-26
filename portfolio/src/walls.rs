//! What hangs behind each thing on the taste wall.
//!
//! The room used to have one backdrop: seven sine curves, re-tinted per work.
//! It read as a texture rather than as a place, and with the picture floating in
//! the middle of a wide terminal the flanks were simply empty -- two thirds of
//! the screen doing nothing on a page whose whole subject is *things that meant
//! something*.
//!
//! So every work gets its own scene, and each one is about the thing it stands
//! behind: rain on a bridge for the one who walks off in the spring, a
//! departures graticule for the one who kept going, a cup and its rings for the
//! one who poured the tea. Not illustration -- the photograph is already the
//! illustration -- but the *place* the quotation was said in.
//!
//! **Depth is three planes and real fog, not three alphas.** `Fog` attenuates by
//! the depth written into each subpixel, so a far stroke is dimmer *because it is
//! far*, and the planes also drift at different rates when the wall slides: the
//! near plane travels about seven times as far as the horizon. That is the whole
//! trick of parallax and it costs one multiply.
//!
//! **The picture sits in a hole, and the hole has no edge.** The lesson the map
//! taught -- a rectangle of anything on top of a gradient is a hard line -- is
//! the one that matters most here, because the mount behind the plate *is* a
//! rectangle. So the scenes never draw into it: every stroke and every dot is
//! multiplied by a wobbled radial falloff, which thins the art as it approaches
//! the picture and leaves it with a soft ragged shore rather than a cut edge.
//! Composition, not a mask -- there is nothing to feather afterwards because
//! nothing was drawn there.
//!
//! **Nothing moves once the room is still.** Same bandwidth rule as the plates:
//! a wall that keeps raining behind a settled photograph is a few hundred KB a
//! minute spent on something nobody is looking at. `t` arrives as zero when the
//! room has settled, so each scene's frozen frame is the one that has to be
//! worth looking at -- which is a useful constraint on the drawing rather than a
//! limitation of it.

use ratatui::layout::Rect;
use ratatui::Frame;
use termap::canvas::{Brush, Canvas, Fog, Theme, MAT_DOT, MAT_SOLID, TINT_MONO};
use termap::raster::{self, Pen};

/// The three planes, as depths.
///
/// Not evenly spaced: the gap between mid and far is where the sense of distance
/// comes from, and the near plane wants to be nearly unfogged so its detail
/// survives at all.
const FAR: f32 = 0.94;
const MID: f32 = 0.52;
const NEAR: f32 = 0.08;

/// How much of the picture's own width the hole reaches beyond it.
///
/// Generous, and deliberately so: the quote under the plate has to be readable
/// against a background, and text competing with braille dots for the same cells
/// reads as a rendering fault. The fade is wide enough that the thinning is the
/// thing you notice rather than the emptiness.
const HOLE: f64 = 1.18;

/// Which scene stands behind a work.
///
/// Named in `data/taste.txt` beside the emblem, so somebody adding an entry
/// chooses its room without a rebuild -- and a name nothing answers to falls
/// back to the contours the wall had before any of this, which is a plain wall
/// rather than an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wall {
    /// Rain over a bridge, and a river under it.
    Rain,
    /// A graticule, flight arcs, and steam off something just out of frame.
    Flights,
    /// A cup's rim, rings spreading in it, steam turning as it rises.
    Ripples,
    /// A pitch in perspective under two floodlights.
    Pitch,
    /// Halftone dots, a web slung from the corner, and the print off-register.
    Web,
    /// Stars, a small planet with one volcano, a dune, and one flower.
    Stars,
    /// Long swells, foam on the crests, an island, two gulls.
    Sea,
    /// The plain wall: slow contours. What every work used to get.
    Contours,
}

impl Wall {
    /// Every scene there is, and the name `taste.txt` calls it by.
    ///
    /// One table rather than a list and a match that can disagree about which
    /// names exist -- the same rule the gates keep. Adding a scene means adding
    /// a row here, an arm in `draw`, and a function; forgetting any of the three
    /// is a compile error rather than a wall that is quietly missing.
    pub const ALL: &'static [(&'static str, Wall)] = &[
        ("rain", Wall::Rain),
        ("flights", Wall::Flights),
        ("ripples", Wall::Ripples),
        ("pitch", Wall::Pitch),
        ("web", Wall::Web),
        ("stars", Wall::Stars),
        ("sea", Wall::Sea),
        ("contours", Wall::Contours),
    ];

    /// The name as `taste.txt` spells it.
    pub fn named(s: &str) -> Option<Wall> {
        let s = s.trim();
        Wall::ALL.iter().find(|(name, _)| *name == s).map(|(_, w)| *w)
    }
}

/// Draw the wall for one work.
///
/// `hole` is where the picture and its quote are going, in cells. `t` is
/// seconds, and zero when the room has settled. `drift` is how far the wall has
/// slid from its resting place, in cells -- the planes divide it between them.
/// `seed` makes a scene's scattered things its own: the same work gets the same
/// stars every time, and no two works get the same ones.
///
/// Returns whether there was room to draw anything, which is what the tests ask.
pub fn draw(
    f: &mut Frame,
    area: Rect,
    wall: Wall,
    hole: Rect,
    t: f64,
    drift: f64,
    seed: u64,
) -> bool {
    let (cw, ch) = (area.width as usize, area.height as usize);
    if cw < 12 || ch < 8 {
        return false;
    }
    let mut canvas = Canvas::new(cw, ch);
    let mut hand = Hand::new(&mut canvas, area, hole, t, drift, seed);
    let ring = hand.veil.ring();
    match wall {
        Wall::Rain => rain(&mut hand),
        Wall::Flights => flights(&mut hand),
        Wall::Ripples => ripples(&mut hand),
        Wall::Pitch => pitch(&mut hand),
        Wall::Web => web(&mut hand),
        Wall::Stars => stars(&mut hand),
        Wall::Sea => sea(&mut hand),
        Wall::Contours => contours(&mut hand),
    }
    // And the hole, cut once for every scene rather than remembered by each.
    // Everything stroked has already faded to nothing here; this is for what was
    // filled, and for whatever gets drawn in here next year by somebody who has
    // not read the paragraph above.
    raster::erase(&mut canvas, &ring);
    canvas.resolve(
        f.buffer_mut(),
        area,
        // Tuned by rendering it and looking. At the map's `far: 0.22` the
        // horizon of every scene disappeared completely -- that value is for a
        // terrain with a thousand strokes in the distance, and these have
        // twenty. A third is the point where a far hill is still a hill.
        &Fog { near: 1.0, far: 0.36, gamma: 1.15 },
        true,
        Theme::Night,
    );
    true
}

/// The hole the picture sits in, as a factor to multiply paint by.
///
/// A plain ellipse would put a machine-drawn oval in the middle of a hand-drawn
/// scene, so the radius wobbles with the angle -- two harmonics, seeded, so
/// every work's hole is a slightly different shape. What the eye reads is the
/// *thinning*, and the wobble is what stops the thinning looking printed.
struct Veil {
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    wob: f64,
}

impl Veil {
    /// The hole's own outline, as a ring to cut out of anything filled.
    ///
    /// Strokes fade into the hole a segment at a time and need nothing else, but
    /// a *fill* is one shape with one factor -- a floodlight wedge crossing the
    /// frame is nowhere near the picture at its centre and straight over it at
    /// its foot. Rather than teach the scanline fill about the falloff, the
    /// shapes are cut afterwards along this ring, which is the same wobbled
    /// curve the fade uses. A stipple ends at it softly enough that the join
    /// does not read as an edge.
    fn ring(&self) -> Vec<[f64; 2]> {
        (0..72)
            .map(|i| {
                let ang = i as f64 / 72.0 * std::f64::consts::TAU;
                let wob = 1.0
                    + 0.11 * (ang * 3.0 + self.wob).sin()
                    + 0.06 * (ang * 5.0 - self.wob * 1.7).sin();
                // Just inside where the fade reaches nothing, so the cut lands
                // in cells that were already almost empty.
                let r = 1.06 * wob;
                [self.cx + ang.cos() * self.rx * r, self.cy + ang.sin() * self.ry * r]
            })
            .collect()
    }

    /// 0 inside the hole, 1 clear of it, smooth between.
    fn clear(&self, p: [f64; 2]) -> f32 {
        let dx = (p[0] - self.cx) / self.rx.max(1.0);
        let dy = (p[1] - self.cy) / self.ry.max(1.0);
        let r = (dx * dx + dy * dy).sqrt();
        if r > 1.6 {
            // Well clear, and this is the common case for a wide screen.
            return 1.0;
        }
        let ang = dy.atan2(dx);
        let wob = 1.0
            + 0.11 * (ang * 3.0 + self.wob).sin()
            + 0.06 * (ang * 5.0 - self.wob * 1.7).sin();
        let r = r / wob;
        // A long ramp. A short one is a hard edge with extra steps.
        smoothstep(0.86, 1.34, r)
    }
}

fn smoothstep(a: f64, b: f64, x: f64) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    (t * t * (3.0 - 2.0 * t)) as f32
}

/// A circle that has been sat on: where, how wide, how tall, how tilted.
#[derive(Clone, Copy)]
struct Oval {
    at: [f64; 2],
    rx: f64,
    ry: f64,
    turn: f64,
}

/// Everything a scene draws with.
///
/// Holds the canvas, the hole, the clock and the dice, so a scene reads as
/// drawing rather than as bookkeeping: `h.curve(...)`, `h.dot(...)`.
struct Hand<'a> {
    c: &'a mut Canvas,
    veil: Veil,
    /// Subpixels across and down.
    sw: f64,
    sh: f64,
    /// Seconds, or zero when the room has settled.
    t: f64,
    /// How far the wall has slid, in cells.
    drift: f64,
    rng: u64,
}

impl<'a> Hand<'a> {
    fn new(c: &'a mut Canvas, area: Rect, hole: Rect, t: f64, drift: f64, seed: u64) -> Hand<'a> {
        let (sw, sh) = (c.sw as f64, c.sh as f64);
        // The hole in subpixels, relative to the section.
        let sx = |x: u16| (x.saturating_sub(area.x)) as f64 * termap::canvas::SUB_X as f64;
        let sy = |y: u16| (y.saturating_sub(area.y)) as f64 * termap::canvas::SUB_Y as f64;
        let (hx, hy) = (sx(hole.x), sy(hole.y));
        let (hw, hh) = (
            hole.width as f64 * termap::canvas::SUB_X as f64,
            hole.height as f64 * termap::canvas::SUB_Y as f64,
        );
        Hand {
            veil: Veil {
                cx: hx + hw / 2.0,
                cy: hy + hh / 2.0,
                rx: (hw / 2.0) * HOLE,
                ry: (hh / 2.0) * HOLE,
                wob: (seed % 617) as f64 * 0.31,
            },
            sw,
            sh,
            t,
            drift,
            rng: seed | 1,
            c,
        }
    }

    /// A deterministic 0..1. Splitmix64, because there is no `rand` here and a
    /// scene wants its scatter to be the same scatter every frame.
    fn dice(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A 0..1 in a range.
    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.dice() * (hi - lo)
    }

    /// How far this plane has slid. Near things move most, which is the whole of
    /// parallax; the horizon barely moves at all.
    fn shift(&self, plane: f32) -> f64 {
        let near = 1.0 - plane as f64;
        self.drift * termap::canvas::SUB_X as f64 * (0.06 + near * 0.62)
    }

    fn pen(&self, plane: f32, alpha: f32, width: f64, solid: bool) -> Pen {
        Pen {
            width,
            alpha,
            depth: plane,
            tint: TINT_MONO,
            mat: if solid { MAT_SOLID } else { MAT_DOT },
            pick: u32::MAX,
            behind: termap::canvas::Behind::Ignore,
        }
    }

    /// One straight stroke, faded by the hole.
    fn seg(&mut self, a: [f64; 2], b: [f64; 2], plane: f32, alpha: f32, width: f64) {
        let dx = self.shift(plane);
        let (a, b) = ([a[0] + dx, a[1]], [b[0] + dx, b[1]]);
        let mid = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        let k = self.veil.clear(mid);
        if k <= 0.02 {
            return;
        }
        let pen = self.pen(plane, alpha * k, width, false);
        raster::line(self.c, a, b, &pen);
    }

    /// A sampled curve. Each segment is faded on its own, so a line running past
    /// the picture thins as it approaches and thickens as it leaves.
    fn curve(
        &mut self,
        steps: usize,
        plane: f32,
        alpha: f32,
        width: f64,
        at: impl Fn(f64) -> [f64; 2],
    ) {
        let mut prev: Option<[f64; 2]> = None;
        for s in 0..=steps {
            let p = at(s as f64 / steps as f64);
            if let Some(q) = prev {
                self.seg(q, p, plane, alpha, width);
            }
            prev = Some(p);
        }
    }

    /// A dot. Stars, foam, halftone.
    ///
    /// Boosted, because a dot is one subpixel where a stroke fills a run of
    /// them: at the same alpha a field of stars came out invisible next to
    /// lines that read fine. The factor is what makes one number mean the same
    /// weight whichever you draw with.
    fn dot(&mut self, p: [f64; 2], plane: f32, alpha: f32) {
        let alpha = alpha * 1.7;
        let x = p[0] + self.shift(plane);
        let k = self.veil.clear([x, p[1]]);
        if k <= 0.02 {
            return;
        }
        let brush = Brush {
            depth: plane,
            tint: TINT_MONO,
            mat: MAT_DOT,
            pick: u32::MAX,
            behind: termap::canvas::Behind::Ignore,
        };
        self.c.splat(x, p[1], alpha * k, &brush);
    }

    /// A closed shape, thinned by the dither. Hills, hulls, sand, floodlight.
    fn wash(&mut self, ring: &[[f64; 2]], density: u8, plane: f32, alpha: f32) {
        let dx = self.shift(plane);
        // One factor for the whole shape, from its centre: a silhouette is read
        // as one thing, and dithering it per-subpixel against the hole would
        // just make it grubby.
        let mut mid = [0.0, 0.0];
        for p in ring {
            mid[0] += p[0] / ring.len() as f64;
            mid[1] += p[1] / ring.len() as f64;
        }
        let k = self.veil.clear([mid[0] + dx, mid[1]]);
        if k <= 0.05 {
            return;
        }
        let shifted: Vec<[f64; 2]> = ring.iter().map(|p| [p[0] + dx, p[1]]).collect();
        let pen = self.pen(plane, alpha * k, 1.0, false);
        let density = ((density as f32) * k).round() as u8;
        if density == 0 {
            return;
        }
        raster::fill(self.c, &shifted, density, &pen);
    }

    /// An ellipse, as a curve: centre, its two radii, and how far it is tilted.
    ///
    /// The shape's four numbers travel together because they are one shape --
    /// eight loose arguments at a call site is where a radius ends up in the
    /// tilt.
    fn ellipse(&mut self, o: Oval, plane: f32, alpha: f32, width: f64) {
        let (st, ct) = o.turn.sin_cos();
        self.curve(48, plane, alpha, width, |u| {
            let a = u * std::f64::consts::TAU;
            let (x, y) = (o.rx * a.cos(), o.ry * a.sin());
            [o.at[0] + x * ct - y * st, o.at[1] + x * st + y * ct]
        });
    }
}

// ---------------------------------------------------------------------------
// The scenes.
//
// Each one is laid out in fractions of the canvas, so it composes the same way
// on an 80-column terminal and a maximised one. They share a habit: the far
// plane sets the horizon, the mid plane carries the subject, and the near plane
// puts something small and specific at the bottom of the frame, where a reader's
// eye lands last.
// ---------------------------------------------------------------------------

/// Snufkin: rain, a bridge, and the water going on without him.
fn rain(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);

    // Two hills, the far one flatter. Stroked rather than filled: a silhouette
    // this size reads as a wall, and the point is distance.
    for (i, (base, amp)) in [(0.40, 0.055), (0.47, 0.085)].iter().enumerate() {
        let plane = if i == 0 { FAR } else { FAR - 0.1 };
        let phase = 1.7 + i as f64 * 2.2;
        h.curve(64, plane, 0.5, 1.0, |u| {
            let y = base + amp * ((u * 2.4 + phase).sin() * 0.5 + 0.5) * (1.0 - u * 0.25);
            [u * sw, y * sh]
        });
    }

    // The bridge, off to the left, arching over the water. On the near plane and
    // low in the frame: it is the thing in front, and at mid depth it was lost
    // in its own rain -- a hundred and twenty streaks at the same brightness
    // will bury anything drawn at the same brightness.
    let (bx0, bx1, by) = (0.0 * sw, 0.36 * sw, 0.70 * sh);
    let rise = 0.075 * sh;
    // Twice, a subpixel apart: a plank bridge has a deck and a handrail, and
    // one stroke at this size reads as a wire.
    for (drop, alpha) in [(0.0, 1.0), (0.016, 0.75)] {
        h.curve(40, NEAR, alpha, 1.8, |u| {
            [bx0 + (bx1 - bx0) * u, by + drop * sh - rise * (u * std::f64::consts::PI).sin()]
        });
    }
    for k in 0..5 {
        let u = 0.08 + k as f64 * 0.21;
        let x = bx0 + (bx1 - bx0) * u;
        let top = by - rise * (u * std::f64::consts::PI).sin();
        h.seg([x, top], [x, by + 0.07 * sh], NEAR, 0.85, 1.3);
    }

    // Rain. Slanted, three lengths, and falling when the room is moving: the
    // whole field slides down and wraps, so nothing appears or vanishes.
    // Fewer and longer than the first attempt: a hundred and twenty short
    // streaks is a texture, and eighty long ones is weather.
    const DROPS: usize = 80;
    for _ in 0..DROPS {
        let x = h.between(-0.05, 1.05) * sw;
        let y0 = h.between(0.0, 1.0);
        let len = h.between(0.045, 0.11) * sh;
        // Far rain is shorter and fainter, which is the only way a flat sheet of
        // it has any depth at all.
        let far = h.dice() < 0.55;
        let (plane, len, alpha) = match far {
            true => (FAR, len * 0.55, 0.55),
            false => (MID, len, 0.4),
        };
        let fall = (y0 + h.t * 0.22).fract() * sh;
        h.seg([x, fall], [x - 0.35 * len, fall + len], plane, alpha, 1.0);
    }

    // The river: three ripples across the bottom, and the nearest one brightest.
    for (i, y) in [0.80, 0.87, 0.94].iter().enumerate() {
        let plane = [MID, NEAR, NEAR].get(i).copied().unwrap_or(NEAR);
        let alpha = 0.4 + i as f32 * 0.18;
        let k = 4.0 + i as f64 * 1.7;
        let t = h.t;
        h.curve(80, plane, alpha, 1.0, |u| {
            let wob = (u * k + t * 0.5).sin() * 0.008 + (u * k * 2.3 - t * 0.31).sin() * 0.004;
            [u * sw, (y + wob) * sh]
        });
    }
}

/// Bourdain: the shape of a departures board, and a bowl just out of frame.
fn flights(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);

    // The graticule, bulged like a globe: meridians bow out from the middle and
    // parallels sag toward the edges. Dashed, because a printed grid is dashed.
    for i in 1..7 {
        let u = i as f64 / 7.0;
        h.curve(40, FAR, 0.34, 1.0, |v| {
            let bow = (v * std::f64::consts::PI).sin() * 0.045 * (u - 0.5).signum();
            [(u + bow * (u - 0.5).abs() * 2.0) * sw, v * sh]
        });
    }
    for i in 1..5 {
        let v = i as f64 / 5.0;
        h.curve(48, FAR, 0.3, 1.0, |u| {
            let sag = (u * std::f64::consts::PI).sin();
            [u * sw, (v - 0.02 + 0.035 * (1.0 - sag)) * sh]
        });
    }

    // Four routes, each a great circle flattened onto the page: a quadratic
    // through a raised control point, with a mark at either end.
    // Four routes, laid out rather than scattered: one long one across the whole
    // frame and three shorter, so there is a composition instead of a handful
    // of arcs that happened to land somewhere. The jitter is still seeded, so
    // no two works fly the same routes.
    for (ax, bx, span) in [(0.02, 0.96, 0.26), (0.05, 0.55, 0.16), (0.45, 0.98, 0.19), (0.18, 0.78, 0.12)]
    {
        let (x0, y0) = ((ax + h.between(-0.02, 0.02)) * sw, h.between(0.24, 0.78) * sh);
        let (x1, y1) = ((bx + h.between(-0.02, 0.02)) * sw, h.between(0.20, 0.78) * sh);
        let lift = (span + h.between(-0.03, 0.03)) * sh;
        let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0 - lift);
        h.curve(56, MID, 0.7, 1.0, |u| {
            let m = 1.0 - u;
            [
                m * m * x0 + 2.0 * m * u * cx + u * u * x1,
                m * m * y0 + 2.0 * m * u * cy + u * u * y1,
            ]
        });
        for (x, y) in [(x0, y0), (x1, y1)] {
            let r = 0.006 * sw;
            h.wash(
                &[[x - r, y - r], [x + r, y - r], [x + r, y + r], [x - r, y + r]],
                52,
                MID,
                0.85,
            );
        }
    }

    // Steam off something hot that is not in the picture. Three ribbons from
    // below the bottom edge, wandering more the higher they get, fading out.
    for i in 0..3 {
        let x0 = (0.62 + i as f64 * 0.11) * sw;
        let sway = 1.6 + i as f64 * 0.7;
        let t = h.t;
        let ribbon = move |u: f64| {
            // Wandering more the higher it gets, which is what rising air does.
            let spread = u * u * 0.06;
            [
                x0 + (u * sway * 3.0 + t * 0.6 + i as f64).sin() * spread * sw,
                (1.04 - u * 0.38) * sh,
            ]
        };
        // In three stretches, fainter as it rises: steam does not stop, it stops
        // being visible. One `curve` takes one alpha, and this is cheaper than
        // teaching it to take a ramp for the one scene that wants one.
        for (lo, hi, alpha) in [(0.0, 0.4, 0.7), (0.4, 0.72, 0.45), (0.72, 1.0, 0.22)] {
            h.curve(18, NEAR, alpha, 1.0, |u| ribbon(lo + (hi - lo) * u));
        }
    }
    // The rim it is rising off: a shallow arc at the very bottom.
    h.curve(30, NEAR, 0.5, 1.2, |u| {
        [(0.58 + u * 0.26) * sw, (1.02 - (u * std::f64::consts::PI).sin() * 0.02) * sh]
    });
}

/// Iroh: a cup from above, rings spreading in it, steam turning as it rises.
fn ripples(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);
    let centre = [0.19 * sw, 0.66 * sh];

    // The cup and its saucer, mostly off the left edge: two ellipses seen at a
    // shallow angle. Cropping them is what makes the room feel bigger than the
    // frame.
    h.ellipse(Oval { at: centre, rx: 0.20 * sw, ry: 0.10 * sh, turn: -0.06 }, FAR, 0.6, 1.3);
    h.ellipse(Oval { at: centre, rx: 0.30 * sw, ry: 0.15 * sh, turn: -0.06 }, FAR, 0.34, 1.0);

    // Rings, spreading. Five of them, each further out and fainter, and the
    // whole set breathing outward with the clock.
    for i in 0..5 {
        let phase = (h.t * 0.18 + i as f64 / 5.0).fract();
        let r = 0.04 + phase * 0.17;
        let fade = (1.0 - phase) as f32 * 0.8;
        h.ellipse(Oval { at: centre, rx: r * sw, ry: r * sh * 0.5, turn: -0.06 }, MID, fade, 1.0);
    }

    // Steam: two spirals, rising and turning. A spiral rather than a wiggle
    // because heat off a cup *rotates*, and it is the one shape in here nobody
    // draws.
    for i in 0..2 {
        let dir = if i == 0 { 1.0 } else { -1.0 };
        let x0 = centre[0] + dir * 0.05 * sw;
        let t = h.t;
        h.curve(70, NEAR, 0.6, 1.0, |u| {
            let up = u * 0.5;
            let turns = 2.4;
            let a = u * turns * std::f64::consts::TAU * dir + t * 0.5;
            let r = (0.012 + u * 0.045) * sw;
            [x0 + a.cos() * r, centre[1] - up * sh - a.sin() * r * 0.35]
        });
    }
}

/// Ted Lasso: a pitch under floodlights, drawn from the touchline.
fn pitch(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);
    // Everything below the horizon, converging on a point above it.
    let horizon = 0.30 * sh;
    let vanish = [0.5 * sw, horizon - 0.10 * sh];

    // Two floodlights: wide faint wedges from the top corners. Filled at a very
    // low density so they read as light rather than as shapes.
    for side in [0.06, 0.94] {
        let top = [side * sw, 0.02 * sh];
        let spread = 0.30 * sw;
        h.wash(
            &[
                top,
                [side * sw - spread, sh],
                [side * sw + spread, sh],
            ],
            5,
            FAR,
            0.5,
        );
        // The pylon itself, just enough to say where the light is from.
        h.seg(top, [top[0], 0.16 * sh], FAR, 0.7, 1.2);
    }

    // The grass, in mown stripes: quads narrowing toward the vanishing point.
    for i in 0..7 {
        if i % 2 == 1 {
            continue;
        }
        let (u0, u1) = (i as f64 / 7.0, (i as f64 + 1.0) / 7.0);
        let near = |u: f64| -0.4 * sw + u * 1.8 * sw;
        let far = |u: f64| vanish[0] + (u - 0.5) * 0.5 * sw;
        h.wash(
            &[
                [near(u0), sh],
                [near(u1), sh],
                [far(u1), horizon],
                [far(u0), horizon],
            ],
            4,
            MID,
            0.6,
        );
    }

    // The markings. A halfway line, the centre circle as a flattened ellipse,
    // and the touchlines running away to the vanishing point.
    h.seg([-0.4 * sw, sh], vanish, MID, 0.55, 1.0);
    h.seg([1.4 * sw, sh], vanish, MID, 0.55, 1.0);
    let half = 0.58 * sh;
    h.seg([0.06 * sw, half], [0.94 * sw, half], MID, 0.7, 1.2);
    h.ellipse(Oval { at: [0.5 * sw, half], rx: 0.20 * sw, ry: 0.055 * sh, turn: 0.0 }, MID, 0.8, 1.2);
    h.dot([0.5 * sw, half], MID, 0.9);

    // The near penalty arc, cropped by the bottom of the frame: the detail that
    // says this is a pitch and not a road.
    h.curve(40, NEAR, 0.7, 1.2, |u| {
        let a = std::f64::consts::PI * (0.15 + u * 0.7);
        [0.5 * sw + a.cos() * 0.34 * sw, 1.04 * sh - a.sin() * 0.16 * sh]
    });
}

/// Miles: halftone, a web off the corner, and the plates a hair out of register.
fn web(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);

    // Ben-Day dots. Density falls across the diagonal, with two soft blooms in
    // it, so it reads as printed rather than as a grid.
    let step = (sw / 46.0).max(3.0);
    let blooms = [[0.18 * sw, 0.26 * sh], [0.84 * sw, 0.74 * sh]];
    let mut y = 0.0;
    while y < sh {
        let mut x = if ((y / step) as usize).is_multiple_of(2) { 0.0 } else { step / 2.0 };
        while x < sw {
            let d = 1.0 - (x / sw * 0.6 + y / sh * 0.4);
            let bloom: f64 = blooms
                .iter()
                .map(|b| {
                    let (dx, dy) = ((x - b[0]) / (0.22 * sw), (y - b[1]) / (0.22 * sh));
                    (1.0 - (dx * dx + dy * dy).sqrt()).max(0.0)
                })
                .sum();
            let a = (d * 0.5 + bloom * 0.5).clamp(0.0, 1.0);
            if a > 0.12 {
                h.dot([x, y], FAR, (a * 0.75) as f32);
            }
            x += step;
        }
        y += step;
    }

    // The web, slung from the top-right corner. Radials, then a sagging chord
    // between each neighbouring pair, twice out -- which is how a web is
    // actually built and why it looks right.
    let anchor = [1.02 * sw, -0.04 * sh];
    const RAYS: usize = 9;
    // Down and to the left, which is the only quadrant a web slung from the
    // top-right corner can occupy. Written as a quarter turn from horizontal to
    // vertical rather than as an angle on the unit circle: the first version
    // was the latter, got the sign of the sine wrong, and sent every ray out
    // through the top of the screen -- a wall with nothing on it and no error
    // anywhere.
    let ends: Vec<[f64; 2]> = (0..RAYS)
        .map(|i| {
            let a = std::f64::consts::FRAC_PI_2 * (0.08 + 0.88 * i as f64 / (RAYS - 1) as f64);
            [anchor[0] - a.cos() * 1.45 * sw, anchor[1] + a.sin() * 1.45 * sh]
        })
        .collect();
    for e in &ends {
        h.seg(anchor, *e, MID, 0.5, 1.0);
        // The offset plate: the same line a subpixel over, fainter. Print
        // misregistration, which is the look the films are quoting.
        h.seg([anchor[0] + 2.0, anchor[1] + 1.0], [e[0] + 2.0, e[1] + 1.0], NEAR, 0.16, 1.0);
    }
    for ring in [0.34, 0.58, 0.82] {
        for i in 0..RAYS - 1 {
            let (a, b) = (ends[i], ends[i + 1]);
            let p0 = [anchor[0] + (a[0] - anchor[0]) * ring, anchor[1] + (a[1] - anchor[1]) * ring];
            let p1 = [anchor[0] + (b[0] - anchor[0]) * ring, anchor[1] + (b[1] - anchor[1]) * ring];
            // Sag toward the anchor, more on the outer rings.
            let sag = 0.10 + ring * 0.12;
            let mid = [
                (p0[0] + p1[0]) / 2.0 + (anchor[0] - (p0[0] + p1[0]) / 2.0) * sag,
                (p0[1] + p1[1]) / 2.0 + (anchor[1] - (p0[1] + p1[1]) / 2.0) * sag,
            ];
            h.curve(14, MID, 0.62, 1.0, |u| {
                let m = 1.0 - u;
                [
                    m * m * p0[0] + 2.0 * m * u * mid[0] + u * u * p1[0],
                    m * m * p0[1] + 2.0 * m * u * mid[1] + u * u * p1[1],
                ]
            });
        }
    }
}

/// The Little Prince: a small planet, one volcano, a dune, one flower.
fn stars(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);

    // Stars, and a few brighter with a cross flare. Seeded, so they are this
    // work's sky and nobody else's.
    for _ in 0..170 {
        let p = [h.between(0.0, 1.0) * sw, h.between(0.0, 0.86) * sh];
        let a = h.between(0.2, 1.0);
        // Spread across the planes rather than all at the horizon: a sky with
        // every star at the same distance is a ceiling. The near ones are also
        // the bright ones, which is how the eye reads depth in a starfield.
        let plane = if a > 0.85 {
            NEAR
        } else if a > 0.55 {
            MID
        } else {
            FAR
        };
        h.dot(p, plane, (0.45 + a * 0.55) as f32);
        // A handful get a companion subpixel and a cross flare -- the ones a
        // reader's eye stops on.
        if a > 0.9 {
            let r = 0.009 * sw;
            h.dot([p[0] + 1.0, p[1]], plane, 0.7);
            h.seg([p[0] - r, p[1]], [p[0] + r, p[1]], plane, 0.55, 1.0);
            h.seg([p[0], p[1] - r * 0.6], [p[0], p[1] + r * 0.6], plane, 0.55, 1.0);
        }
    }

    // The planet, small and up to the left, with an orbit around it and three
    // volcanoes on its limb -- two active, one out, as the book has it.
    let planet = [0.17 * sw, 0.24 * sh];
    let r = 0.05 * sw;
    h.ellipse(Oval { at: planet, rx: r * 2.6, ry: r * 1.1, turn: 0.22 }, MID, 0.3, 1.0);
    h.ellipse(Oval { at: planet, rx: r, ry: r * 0.52, turn: 0.0 }, MID, 0.85, 1.2);
    h.wash(
        &(0..24)
            .map(|i| {
                let a = i as f64 / 24.0 * std::f64::consts::TAU;
                [planet[0] + a.cos() * r, planet[1] + a.sin() * r * 0.52]
            })
            .collect::<Vec<_>>(),
        7,
        MID,
        0.7,
    );
    for (i, u) in [0.18, 0.5, 0.82].iter().enumerate() {
        let a = std::f64::consts::PI * (1.0 + u);
        let base = [planet[0] + a.cos() * r * 0.9, planet[1] + a.sin() * r * 0.47];
        let up = if i == 1 { 0.030 } else { 0.020 };
        h.seg(base, [base[0], base[1] - up * sh], MID, 0.7, 1.0);
    }

    // The dune, and sand under it. One long curve: a desert is one line.
    let crest = |u: f64| {
        (0.80 + 0.055 * ((u * 1.9 + 0.6).sin() * 0.5 + 0.5) - 0.03 * u) * sh
    };
    h.curve(90, NEAR, 0.75, 1.2, move |u| [u * sw, crest(u)]);
    let mut ring: Vec<[f64; 2]> =
        (0..=40).map(|i| [i as f64 / 40.0 * sw, crest(i as f64 / 40.0)]).collect();
    ring.push([sw, sh]);
    ring.push([0.0, sh]);
    h.wash(&ring, 6, NEAR, 0.5);

    // And the flower, on the crest, off to one side. Three strokes: it should be
    // small enough to be missed and specific enough to matter when it is not.
    let fx = 0.72 * sw;
    let fy = crest(0.72);
    h.seg([fx, fy], [fx - 0.004 * sw, fy - 0.045 * sh], NEAR, 0.9, 1.0);
    h.ellipse(Oval { at: [fx - 0.004 * sw, fy - 0.052 * sh], rx: 0.010 * sw, ry: 0.010 * sh, turn: 0.0 }, NEAR, 0.95, 1.0);
    h.seg([fx - 0.002 * sw, fy - 0.022 * sh], [fx + 0.014 * sw, fy - 0.030 * sh], NEAR, 0.7, 1.0);
}

/// One Piece: long swells, foam, an island nobody has named, two gulls.
fn sea(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);
    let horizon = 0.34 * sh;

    h.curve(40, FAR, 0.5, 1.0, |u| [u * sw, horizon]);

    // An island on the right, low and soft. Somewhere to be going.
    let island: Vec<[f64; 2]> = (0..=30)
        .map(|i| {
            let u = i as f64 / 30.0;
            let hump = (u * std::f64::consts::PI).sin();
            [(0.66 + u * 0.22) * sw, horizon - hump * 0.055 * sh]
        })
        .chain([[0.88 * sw, horizon], [0.66 * sw, horizon]])
        .collect();
    h.wash(&island, 26, FAR, 0.7);

    // Two gulls, near the horizon. Four strokes in total.
    for (gx, gy, s) in [(0.40, 0.26, 1.0), (0.46, 0.22, 0.7)] {
        let (x, y, w) = (gx * sw, gy * sh, 0.012 * sw * s);
        h.seg([x - w, y], [x, y - w * 0.45], FAR, 0.8, 1.0);
        h.seg([x, y - w * 0.45], [x + w, y], FAR, 0.8, 1.0);
    }

    // Swells. Each crest is a sine sharpened at the top, so it breaks rather
    // than rolls, and each one nearer is bigger, brighter and slower.
    for (i, (base, amp, k, plane)) in [
        (0.46, 0.030, 3.2, FAR),
        (0.58, 0.045, 2.4, MID),
        (0.72, 0.062, 1.8, MID),
        (0.90, 0.085, 1.3, NEAR),
    ]
    .iter()
    .enumerate()
    {
        let (base, amp, k, plane) = (*base, *amp, *k, *plane);
        let t = h.t * (0.5 - i as f64 * 0.09);
        let phase = i as f64 * 1.9;
        let alpha = 0.45 + i as f32 * 0.14;
        let crest = move |u: f64| {
            let s = (u * k * std::f64::consts::TAU + t + phase).sin();
            // Sharpen: the peaks pinch, the troughs flatten.
            let s = s.abs().powf(0.65) * s.signum();
            [u * sw, (base - amp * s) * sh]
        };
        h.curve(110, plane, alpha, if i == 3 { 1.4 } else { 1.0 }, crest);

        // Foam, on the peaks only, scattered.
        for _ in 0..(14 + i * 10) {
            let u = h.between(0.0, 1.0);
            let p = crest(u);
            let s = (u * k * std::f64::consts::TAU + t + phase).sin();
            if s < 0.55 {
                continue;
            }
            let jx = h.between(-0.008, 0.008) * sw;
            let jy = h.between(-0.010, 0.004) * sh;
            h.dot([p[0] + jx, p[1] + jy], plane, alpha * 1.4);
        }
    }
}

/// The plain wall: slow contours, which is what every work used to get.
///
/// Kept as the fallback rather than deleted. An entry that names no scene, or
/// names one that does not exist, gets a wall that is quiet and finished rather
/// than an error or an empty room.
fn contours(h: &mut Hand) {
    let (sw, sh) = (h.sw, h.sh);
    const LINES: usize = 7;
    for i in 0..LINES {
        let fy = (i as f64 + 0.5) / LINES as f64;
        let plane = 0.35 + (fy - 0.5).abs() as f32 * 1.2;
        let phase = i as f64 * 2.3;
        let (k1, k2) = (3.0 + i as f64 * 0.4, 1.7 + i as f64 * 0.23);
        let t = h.t;
        h.curve(64, plane.min(FAR), 0.55, 1.0, |u| {
            let env = (u * std::f64::consts::PI).sin().powf(1.2);
            let a = (u * k1 + t * 0.35 + phase).sin() * 0.6
                + (u * k2 - t * 0.22 + phase * 1.4).sin() * 0.4;
            [u * sw, fy * sh + a * env * sh * 0.055]
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    const AREA: Rect = Rect { x: 0, y: 0, width: 180, height: 48 };
    /// Where a picture and its caption sit, roughly: the middle third.
    const HOLE: Rect = Rect { x: 58, y: 8, width: 64, height: 30 };

    /// Draw one wall and report how many cells it put something in, and how many
    /// of those were inside the hole.
    fn painted(wall: Wall, t: f64, drift: f64, seed: u64) -> (usize, usize) {
        let mut term = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
        term.draw(|f| {
            draw(f, AREA, wall, HOLE, t, drift, seed);
        })
        .unwrap();
        let buf = term.backend().buffer();
        let (mut all, mut inside) = (0, 0);
        for y in 0..AREA.height {
            for x in 0..AREA.width {
                let c = buf[(x, y)].symbol();
                if c != " " && !c.is_empty() {
                    all += 1;
                    // The core of the hole, well inside the ramp.
                    let (cx, cy) = (HOLE.x + HOLE.width / 2, HOLE.y + HOLE.height / 2);
                    let (dx, dy) = (
                        (x as f64 - cx as f64) / (HOLE.width as f64 / 2.0),
                        (y as f64 - cy as f64) / (HOLE.height as f64 / 2.0),
                    );
                    if (dx * dx + dy * dy).sqrt() < 0.55 {
                        inside += 1;
                    }
                }
            }
        }
        (all, inside)
    }

    fn every() -> Vec<(&'static str, Wall)> {
        Wall::ALL.to_vec()
    }

    /// Every scene draws something, and none of them draws into the picture.
    ///
    /// The second half is the one that matters: the hole is what keeps the
    /// quotation readable and the plate from looking like a rendering fault, and
    /// it is enforced by every scene multiplying its paint by the same falloff
    /// rather than by anybody remembering to.
    #[test]
    fn every_wall_fills_the_room_and_leaves_the_picture_alone() {
        for (name, wall) in every() {
            let (all, inside) = painted(wall, 0.0, 0.0, 7);
            assert!(all > 200, "`{name}` drew almost nothing: {all} cells");
            assert_eq!(inside, 0, "`{name}` drew {inside} cells inside the picture");
        }
    }

    /// The walls are actually different walls. This is the test that would have
    /// caught the thing being replaced: one field re-tinted seven times looks
    /// like seven walls in a screenshot and is one.
    #[test]
    fn no_two_scenes_draw_the_same_room() {
        let mut seen: Vec<(&str, Vec<bool>)> = Vec::new();
        for (name, wall) in every() {
            let mut term = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
            term.draw(|f| {
                draw(f, AREA, wall, HOLE, 0.0, 0.0, 7);
            })
            .unwrap();
            let buf = term.backend().buffer();
            let map: Vec<bool> = (0..AREA.height)
                .flat_map(|y| {
                    (0..AREA.width).map(move |x| (x, y)).collect::<Vec<_>>()
                })
                .map(|(x, y)| {
                    let s = buf[(x, y)].symbol();
                    s != " " && !s.is_empty()
                })
                .collect();
            for (other, theirs) in &seen {
                let same = map.iter().zip(theirs).filter(|(a, b)| a == b).count();
                let ratio = same as f64 / map.len() as f64;
                assert!(
                    ratio < 0.97,
                    "`{name}` and `{other}` are {:.1}% the same wall",
                    ratio * 100.0
                );
            }
            seen.push((name, map));
        }
    }

    /// A scene's scatter belongs to its work: the same seed twice is the same
    /// sky, and a different seed is a different one. Without this a reader who
    /// walks back along the wall sees the stars jump.
    #[test]
    fn a_seed_makes_a_scene_its_own_and_keeps_it() {
        let a = painted(Wall::Stars, 0.0, 0.0, 11);
        let again = painted(Wall::Stars, 0.0, 0.0, 11);
        let other = painted(Wall::Stars, 0.0, 0.0, 12);
        assert_eq!(a, again, "the same work drew a different sky");
        assert_ne!(a.0, other.0, "two works drew the same sky");
    }

    /// Nothing moves when the room is still, which is the bandwidth rule the
    /// plates already follow. A wall that animates behind a settled photograph
    /// is a few hundred KB a minute nobody asked for.
    #[test]
    fn a_settled_room_draws_the_same_frame_every_time() {
        for (name, wall) in every() {
            let a = painted(wall, 0.0, 0.0, 3);
            let b = painted(wall, 0.0, 0.0, 3);
            assert_eq!(a, b, "`{name}` is not a pure function of its arguments");
        }
    }

    /// Sliding the wall moves it. Cheap to assert and it pins the parallax
    /// wiring: a drift that reaches no plane is a wall that slides with the
    /// picture, which is exactly the flatness this replaced.
    #[test]
    fn sliding_the_wall_moves_what_is_on_it() {
        let still = painted(Wall::Sea, 0.0, 0.0, 5);
        let slid = painted(Wall::Sea, 0.0, 9.0, 5);
        assert_ne!(still.0, slid.0, "the wall did not move when the room slid");
    }

    /// A window too small for a room gets no room, rather than a panic or a
    /// smear. The section itself refuses under 24x12; this is the layer under
    /// that saying so for itself.
    #[test]
    fn a_window_with_no_room_in_it_is_left_alone() {
        for (w, h) in [(4, 4), (11, 20), (40, 6)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            let area = Rect { x: 0, y: 0, width: w, height: h };
            let mut drew = true;
            term.draw(|f| {
                drew = draw(f, area, Wall::Rain, HOLE, 0.0, 0.0, 1);
            })
            .unwrap();
            assert!(!drew, "{w}x{h} claimed to have room");
        }
    }

    /// Every name in the data resolves, and an unknown one is `None` rather
    /// than a guess -- `taste.rs` turns that into the plain wall.
    #[test]
    fn a_name_that_is_not_a_scene_is_not_guessed_at() {
        assert_eq!(Wall::named("sea"), Some(Wall::Sea));
        assert_eq!(Wall::named("  sea  "), Some(Wall::Sea));
        assert_eq!(Wall::named("ocean"), None);
        assert_eq!(Wall::named(""), None);
        for (name, wall) in Wall::ALL {
            assert_eq!(Wall::named(name), Some(*wall), "`{name}` is listed and unreachable");
        }
    }
}
