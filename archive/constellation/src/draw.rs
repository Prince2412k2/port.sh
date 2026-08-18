//! One frame of sky.
//!
//! Painted back to front — dust, background stars, figures, then the skills
//! themselves — because the canvas resolves a later write as the nearer one.
//!
//! Three passes are doing the work that makes it read as a sky rather than as a
//! scatter plot:
//!
//! * **The dust is dithered, not shaded.** A terminal has no fill levels, so a
//!   band drawn at uniform low coverage lights one dot in every cell and comes
//!   out as flat grey wash. Run through an ordered matrix it becomes a stipple,
//!   and a stipple reads as *behind* before brightness has any say.
//! * **Stars have a profile, not a position.** A single dot is a dot. A
//!   gaussian core two or three subpixels across, with diffraction spikes on the
//!   brightest, is what the eye has been trained by every photograph of the sky
//!   to read as a star.
//! * **Dimming is depth, not colour.** Focusing a constellation pushes
//!   everything else *back* rather than greying it, so the sky keeps its
//!   structure while one figure comes forward.

use crate::canvas::{Brush, Canvas, SUB_X, SUB_Y, TINT_CON, TINT_DIM, TINT_FAINT, TINT_SELECT};
use crate::data::Sky;
use crate::labels::Candidate;
use crate::layout::Layout;
use crate::sky::View;

/// 8x8 ordered dither, the same matrix termap fills with.
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// Subpixels between background-star candidate cells, held roughly constant as
/// the view zooms.
///
/// Fixing the grid in *sky* units instead looks right at one zoom and nowhere
/// else: pulled back to the whole sheet it turns into a solid wash that buries
/// the constellations, and pushed in to one project it empties out entirely.
/// The field is texture, not data, so it is allowed to be re-diced per zoom —
/// what it is not allowed to do is crawl while panning, which is why the grid
/// snaps to powers of two and the level salts the hash.
const DUST_STEP: f64 = 13.0;

/// How dim a star gets when something else has the focus.
const DIMMED: f32 = 0.20;

pub struct Scene<'a> {
    pub sky: &'a Sky,
    pub lay: &'a Layout,
    pub view: &'a View,
    /// Constellation with the focus. Everything else recedes.
    pub focus: Option<usize>,
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    /// Search results. Overrides `focus` while a query is active.
    pub matches: Option<&'a [usize]>,
    pub dust: bool,
    pub figures: bool,
}

impl Scene<'_> {
    /// How much of its brightness a star keeps, 0..1.
    fn emphasis(&self, star: usize) -> f32 {
        if let Some(m) = self.matches {
            return if m.contains(&star) { 1.0 } else { DIMMED };
        }
        match self.focus {
            Some(f) if !self.sky.stars[star].members.contains(&f) => DIMMED,
            _ => 1.0,
        }
    }

    fn con_emphasis(&self, con: usize) -> f32 {
        if self.matches.is_some() {
            return DIMMED;
        }
        match self.focus {
            Some(f) if f != con => DIMMED,
            _ => 1.0,
        }
    }
}

/// A project's name, already positioned. Constellation names do not go through
/// the collision placer: there are nine of them, they are the most important
/// text on the screen, and they belong dead centre of their own ring — which is
/// the one place a greedy placer will not put them, because its first choice is
/// always two cells east of the anchor.
pub struct Title {
    pub cell: (usize, usize),
    pub text: String,
    pub tint: u8,
    pub lum: f32,
    pub con: usize,
}

/// What one frame produced besides pixels.
pub struct Painted {
    /// Project names, already placed.
    pub titles: Vec<Title>,
    /// Star names wanting a slot, highest rank first choice.
    pub names: Vec<Candidate>,
    /// Cells a star is sitting in. Labels are kept off them — a name written
    /// over the star it names is the one collision the placer cannot see,
    /// because stars live in the coverage buffer and labels in the overlay.
    pub occupied: Vec<(usize, usize)>,
}

/// Paint the frame and hand back the names that want placing.
pub fn frame(c: &mut Canvas, s: &Scene) -> Painted {
    if s.dust {
        // Mode follows zoom, the same argument the map makes about terrain.
        // Pulled back to the whole sheet the dust is what gives the frame
        // depth; pushed in to read one project it is nine hundred dots of
        // noise between the reader and six words of prose. The band goes
        // first and hardest — you are *inside* it at that scale, so it stops
        // being a band and becomes a fog — and the field only thins.
        let z = s.view.zoom;
        // Opening a project takes another bite out of both. The background is
        // there to make the whole sky feel like a place; once one project is
        // the subject it is competing with the thing being read.
        let open = if s.focus.is_some() || s.matches.is_some() { 0.55 } else { 1.0 };
        dust_band(c, s.view, (((0.2 - z) / 1.6) as f32).clamp(0.0, 1.0) * open);
        field(c, s.view, (((1.6 - z) / 2.6) as f32).clamp(0.40, 1.0) * open);
    }
    if s.figures {
        figures(c, s);
    }
    stars(c, s)
}

// ── the milky way ────────────────────────────────────────────────────────────

/// A band of dust across the sky, dithered down to a stipple.
///
/// The intensity field is in *sky* coordinates so the band pans with the stars,
/// but the dither threshold is per *subpixel*, so the stipple stays put while
/// you drag. Both fixed to sky space would shimmer; both fixed to the screen
/// would leave the band painted onto the glass.
fn dust_band(c: &mut Canvas, v: &View, fade: f32) {
    if fade <= 0.01 {
        return;
    }
    // Axis of the band, and a point it passes through.
    let (sin_t, cos_t) = (0.40f64).sin_cos();
    let origin = [-6.0, -8.0];
    let half_width = 46.0;

    let brush = Brush::new(0.90, TINT_FAINT);

    for sy in 0..c.sh {
        for sx in 0..c.sw {
            let p = v.unproject([sx as f64 + 0.5, sy as f64 + 0.5]);
            // Signed distance to the band's centre line.
            let d = -(p[0] - origin[0]) * sin_t + (p[1] - origin[1]) * cos_t;
            let t = d / half_width;
            if t.abs() > 1.6 {
                continue;
            }
            let profile = (-t * t * 1.9).exp();

            // Three octaves: clumps, filaments, grain.
            let turb = 0.52 * noise(p[0] / 26.0, p[1] / 26.0)
                + 0.31 * noise(p[0] / 9.5, p[1] / 9.5)
                + 0.17 * noise(p[0] / 3.4, p[1] / 3.4);

            // The dark rift. Without it the band is a smooth smear and reads as
            // a gradient someone applied rather than as anything in the sky.
            let rift = (-((d - 10.0) / 18.0).powi(2)).exp() * 0.60;

            // Capped well below solid: this is texture, and a dither that ever
            // reaches full coverage stops being one.
            let density = ((profile * (0.30 + 1.05 * turb) - rift).max(0.0) * 30.0 * fade as f64) as i32;
            if density <= 0 {
                continue;
            }
            let (x, y) = (sx as isize, sy as isize);
            let threshold = BAYER[sy % 8][sx % 8] as i32 + jitter(x, y);
            if threshold < density {
                c.plot(x, y, 0.32, &brush);
            }
        }
    }
}

// ── background stars ─────────────────────────────────────────────────────────

/// The stars that are not skills.
///
/// Generated from a grid hashed in sky space rather than stored, so the field
/// is unbounded, costs only what is on screen, and is identical every run. The
/// brightness curve is cubed: most are barely there and a handful are not,
/// which is what keeps it from looking like evenly-sprinkled salt.
fn field(c: &mut Canvas, v: &View, fade: f32) {
    let level = (DUST_STEP / v.scale()).log2().floor();
    let grid = 2f64.powf(level);
    // Each zoom level gets its own field rather than a subset of one field:
    // subsetting makes stars vanish on zoom-in, which the eye reads as a bug.
    let salt = level as i64;

    let a = v.unproject([0.0, 0.0]);
    let b = v.unproject([c.sw as f64, c.sh as f64]);
    let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
    let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));

    let gx0 = (x0 / grid).floor() as i64;
    let gx1 = (x1 / grid).ceil() as i64;
    let gy0 = (y0 / grid).floor() as i64;
    let gy1 = (y1 / grid).ceil() as i64;

    // A frame zoomed all the way out still only asks for a few thousand cells,
    // but the guard keeps a pathological viewport from stalling the render.
    if (gx1 - gx0).saturating_mul(gy1 - gy0) > 400_000 {
        return;
    }

    for gy in gy0..=gy1 {
        for gx in gx0..=gx1 {
            let (hx, hy) = (gx + salt * 7919, gy - salt * 104_729);
            let h = hash2(hx, hy);
            // Most cells are empty — sparser than it wants to be on its own,
            // because the band has to out-read the field it sits inside. A
            // background as dense as the dust leaves the band invisible and
            // the whole sky an even wash.
            if h > 0.28 {
                continue;
            }
            let ox = hash2(hx * 7 + 1, hy * 13 - 3);
            let oy = hash2(hx * 17 - 5, hy * 23 + 11);
            let p = [(gx as f64 + ox) * grid, (gy as f64 + oy) * grid];
            let s = v.project(p);
            if s[0] < -3.0 || s[1] < -3.0 || s[0] > c.sw as f64 + 3.0 || s[1] > c.sh as f64 + 3.0 {
                continue;
            }
            let bright = (h / 0.28).powi(3) as f32;
            let brush = Brush::new(0.80 + 0.13 * (1.0 - bright), TINT_FAINT);
            c.splat(s[0], s[1], (0.10 + 0.46 * bright) * fade, &brush);
        }
    }
}

// ── constellation figures ────────────────────────────────────────────────────

fn figures(c: &mut Canvas, s: &Scene) {
    for con in 0..s.sky.cons.len() {
        let emph = s.con_emphasis(con);
        let tint = if emph < 0.5 { TINT_DIM } else { TINT_CON + con as u8 };
        // Figures sit behind their own stars but in front of the dust.
        let brush = Brush::new(0.55 + 0.40 * (1.0 - emph), tint);
        for &(a, b) in &s.lay.edges[con] {
            // A skill shared with a distant project is pulled a long way out of
            // its own constellation, and joining it with a line at full weight
            // rules a stripe across the whole sky. Attenuating by length keeps
            // the link legible as a direction — this figure continues that way
            // — without the line becoming the loudest thing in the frame.
            let d = (s.lay.pos[a][0] - s.lay.pos[b][0]).hypot(s.lay.pos[a][1] - s.lay.pos[b][1]);
            let reach = (1.0 / (1.0 + (d / 42.0).powi(2))) as f32;
            dashed(
                c,
                s.view.project(s.lay.pos[a]),
                s.view.project(s.lay.pos[b]),
                0.60 * emph * (0.22 + 0.78 * reach),
                &brush,
            );
        }
    }
}

/// A dashed segment. Dashes rather than a solid rule because a constellation
/// line is a convention drawn over the sky, not a thing in it, and a solid line
/// at this weight starts to look like structure the data does not have.
fn dashed(c: &mut Canvas, a: [f64; 2], b: [f64; 2], alpha: f32, brush: &Brush) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        return;
    }
    // Cheap reject: both ends far off the same edge.
    let m = 4.0;
    let (w, h) = (c.sw as f64, c.sh as f64);
    if (a[0] < -m && b[0] < -m)
        || (a[1] < -m && b[1] < -m)
        || (a[0] > w + m && b[0] > w + m)
        || (a[1] > h + m && b[1] > h + m)
    {
        return;
    }

    // 3.1 on, 2.3 off — but only where there is room for the pattern to read
    // as one. Pulled back to the whole sky a figure's edges are six or eight
    // subpixels long, and two dashes of a dotted line are indistinguishable
    // from two more stars. Below three periods the segment draws solid, which
    // is what makes a constellation legible as a shape rather than a cluster.
    let dashed = len > 16.0;
    let steps = len.ceil() as usize;
    for i in 0..=steps {
        let t = i as f64 / steps as f64;
        if dashed && (t * len + 1.4) % 5.4 > 3.1 {
            continue;
        }
        c.splat(a[0] + dx * t, a[1] + dy * t, alpha, brush);
    }
}

// ── the skills ───────────────────────────────────────────────────────────────

fn stars(c: &mut Canvas, s: &Scene) -> Painted {
    let mut names = Vec::new();
    let mut titles = Vec::new();
    let mut occupied = Vec::new();

    for (i, con) in s.sky.cons.iter().enumerate() {
        // The open project's name is set large in the middle of its own ring.
        // Drawing it again out on the figure is the same word twice, and the
        // label placer has no way to know they are the same word.
        if s.focus == Some(i) {
            continue;
        }
        // At the anchor, which is the middle of the ring — the same hole the
        // project's description drops into when it is opened. Anchoring to the
        // centroid of its stars instead drags the name toward whatever it
        // shares with distant projects, and nine names end up huddled in the
        // middle of the sky pointing at nothing.
        let p = s.view.project(con.at);
        let text = spaced(&con.name);
        let len = text.chars().count();
        let cx = (p[0] / SUB_X as f64) - len as f64 * 0.5;
        let cy = p[1] / SUB_Y as f64;
        if cx < 0.0 || cy < 0.0 || cx + len as f64 > c.cw as f64 || cy >= c.ch as f64 {
            continue;
        }
        let emph = s.con_emphasis(i);
        titles.push(Title {
            cell: (cx as usize, cy as usize),
            text,
            tint: if emph < 0.5 { TINT_DIM } else { TINT_CON + i as u8 },
            lum: if emph < 0.5 { 0.45 } else { 1.0 },
            con: i,
        });
    }

    for (i, star) in s.sky.stars.iter().enumerate() {
        let p = s.view.project(s.lay.pos[i]);
        let mag = star.magnitude();
        let emph = s.emphasis(i);
        let picked = s.selected == Some(i);
        let hovered = s.hover == Some(i);

        // Depth carries two things at once: how load-bearing the skill is, and
        // whether it is currently in focus. They compose, so a dimmed bright
        // star still outranks a dimmed faint one.
        let depth = ((0.85 - 0.80 * mag) + (1.0 - emph) * 0.85).min(1.0);
        let depth = if picked { 0.0 } else { depth };

        let tint = if picked {
            TINT_SELECT
        } else if emph < 0.5 {
            TINT_DIM
        } else {
            TINT_CON + star.home() as u8
        };

        let boost = if picked {
            1.0
        } else if hovered {
            0.92
        } else {
            0.30 + 0.70 * emph
        };

        let margin = 14.0;
        let on_screen = p[0] > -margin
            && p[1] > -margin
            && p[0] < c.sw as f64 + margin
            && p[1] < c.sh as f64 + margin;
        if !on_screen {
            continue;
        }

        glyph(c, p, mag, s.view.scale(), tint, depth, i as u32, boost, picked);

        let cell = (
            (p[0] / SUB_X as f64) as isize,
            (p[1] / SUB_Y as f64) as isize,
        );
        if cell.0 >= 0 && cell.1 >= 0 {
            occupied.push((cell.0 as usize, cell.1 as usize));
        }

        // Which names get drawn is a function of zoom, focus and magnitude,
        // and the rest are dropped rather than crowded in. Being generous here
        // is safe: the placer drops whatever will not fit, so the failure mode
        // is a missing label rather than an unreadable frame.
        //
        // Focus overrides the zoom rule outright. Asking to see one project and
        // getting its skills as anonymous dots is the one thing this screen
        // must not do, and the zoom that frames a project is nowhere near the
        // zoom at which a first-magnitude star earns its name on its own.
        let in_focus = s.focus.is_some_and(|f| s.sky.stars[i].members.contains(&f));
        let wants_name = picked
            || hovered
            || in_focus
            || s.matches.is_some_and(|m| m.contains(&i))
            || (emph > 0.5 && s.view.zoom + mag as f64 * 2.2 > 1.2);
        if !wants_name {
            continue;
        }

        names.push(Candidate {
            anchor: p,
            text: star.name.clone(),
            rank: if picked {
                240
            } else if hovered {
                235
            } else {
                (60.0 + 140.0 * mag) as u16
            },
            tint,
            depth: if picked { 0.0 } else { 0.25 + 0.55 * (1.0 - emph) },
            marker: None,
            feature: i as u32,
        });
    }

    Painted { titles, names, occupied }
}

/// One star: a gaussian core, and spikes on the bright ones.
///
/// The spikes are not decoration. At this resolution a bright star and a dim
/// one differ by about one subpixel of radius, which is nothing; the spikes are
/// what make magnitude legible at a glance, and they are the reason a
/// load-bearing skill can be picked out of the frame without reading a word.
#[allow(clippy::too_many_arguments)]
fn glyph(
    c: &mut Canvas,
    p: [f64; 2],
    mag: f32,
    scale: f64,
    tint: u8,
    depth: f32,
    pick: u32,
    boost: f32,
    selected: bool,
) {
    let brush = Brush { depth, tint, pick };
    // A star has a size in the sky as well as a floor in pixels, and takes
    // whichever is larger. Pulled back, everything is a point and magnitude
    // reads through the spikes; pushed in, the brighter ones open up and the
    // figure gains structure instead of just getting further apart. The cap is
    // what stops a first-magnitude skill becoming a moon at street level.
    //
    // The floor is set where it is because of the background field: those are
    // one dot each, and a skill that also resolves to one dot is invisible in
    // the middle of them. Faintest here is five — a centre and its four
    // neighbours — which is the smallest thing that still reads as a star and
    // not as dust.
    let r = (0.55 + 1.25 * mag as f64)
        .max((0.25 + 0.70 * mag as f64) * scale)
        .min(3.4);
    let reach = (r * 2.2).ceil() as isize;
    let (px, py) = (p[0], p[1]);
    let (ix, iy) = (px.round() as isize, py.round() as isize);

    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let (x, y) = (ix + dx, iy + dy);
            let d2 = (x as f64 - px).powi(2) + (y as f64 - py).powi(2);
            let a = boost * (-(d2 / (r * r)) * 1.15).exp() as f32;
            c.plot(x, y, a, &brush);
        }
    }
    // Guarantee a solid centre. Without it a star sitting between four dots
    // spreads its whole budget across them and comes out dimmer than a fainter
    // star that happened to land on one.
    c.splat(px, py, boost, &brush);

    if mag > 0.42 || selected {
        let len = r * (1.6 + 3.4 * mag as f64) + if selected { 2.5 } else { 0.0 };
        let n = len.ceil() as isize;
        for t in 1..=n {
            let f = t as f64 / len;
            if f > 1.0 {
                break;
            }
            let a = boost * 0.55 * (1.0 - f).powf(1.4) as f32;
            c.splat(px + t as f64, py, a, &brush);
            c.splat(px - t as f64, py, a, &brush);
            c.splat(px, py + t as f64, a, &brush);
            c.splat(px, py - t as f64, a, &brush);
        }
    }
}

/// `netjail` -> `N E T J A I L`. Letterspacing is the only typographic move a
/// terminal has, and it is enough to say "this is a place, not a thing".
pub fn spaced(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for (i, ch) in s.to_uppercase().chars().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

// ── deterministic noise ──────────────────────────────────────────────────────

/// Per-subpixel noise in roughly -12..12, added to the Bayer threshold.
///
/// An 8x8 block is exactly 4x2 braille cells, so the bare matrix repeats at cell
/// resolution and the eye reads the dust as corduroy rather than as texture.
#[inline]
fn jitter(x: isize, y: isize) -> i32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    (h % 25) as i32 - 12
}

#[inline]
fn hash2(x: i64, y: i64) -> f64 {
    let mut h = (x as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Value noise with a smoothstep, 0..1. Cheap, and the artefacts a gradient
/// noise would avoid are invisible under a dither this coarse.
fn noise(x: f64, y: f64) -> f64 {
    let (fx, fy) = (x.floor(), y.floor());
    let (ix, iy) = (fx as i64, fy as i64);
    let (tx, ty) = (x - fx, y - fy);
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let a = hash2(ix, iy);
    let b = hash2(ix + 1, iy);
    let c = hash2(ix, iy + 1);
    let d = hash2(ix + 1, iy + 1);
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    top + (bot - top) * sy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_stays_in_range() {
        for i in 0..2000 {
            let x = i as f64 * 0.37 - 300.0;
            let y = i as f64 * -0.11 + 40.0;
            let n = noise(x, y);
            assert!((0.0..=1.0).contains(&n), "{n} at {x},{y}");
        }
    }

    #[test]
    fn noise_is_continuous_across_cell_edges() {
        // The seam is where a bilinear value noise looks worst, so it is the
        // one place worth asserting.
        let left = noise(3.0 - 1e-7, 1.25);
        let right = noise(3.0 + 1e-7, 1.25);
        assert!((left - right).abs() < 1e-5, "{left} vs {right}");
    }

    #[test]
    fn spaced_letters_out() {
        assert_eq!(spaced("netjail"), "N E T J A I L");
        assert_eq!(spaced("a"), "A");
        assert_eq!(spaced(""), "");
    }

    #[test]
    fn jitter_is_bounded_and_stable() {
        for x in -50..50 {
            for y in -50..50 {
                let j = jitter(x, y);
                assert!((-12..=12).contains(&j));
                assert_eq!(j, jitter(x, y));
            }
        }
    }
}
