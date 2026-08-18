//! Drawing primitives that write into the subpixel canvas.

use crate::canvas::{Brush, Canvas};

/// 8x8 ordered dither. Fills use it to thin themselves out: the sea and parks
/// become a sparse stipple instead of a solid block, which reads as "behind
/// everything else" before the fog even gets a say.
///
/// Ordered dither alone is not enough here. An 8x8 block is exactly 4x2 braille
/// cells, so the pattern repeats at cell resolution and the eye reads the sea as
/// corduroy rather than as texture. `jitter` perturbs each threshold just enough
/// to break the tiling while keeping Bayer's even spacing.
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

pub struct Pen {
    pub width: f64,
    pub alpha: f32,
    pub depth: f32,
    pub tint: u8,
    /// MAT_DOT or MAT_SOLID -- which glyph family this feature draws with.
    pub mat: u8,
    pub pick: u32,
    /// Participate in hidden-surface removal (3D only).
    pub occlude: bool,
}

impl Pen {
    #[inline]
    fn brush(&self) -> Brush {
        Brush {
            depth: self.depth,
            tint: self.tint,
            mat: self.mat,
            pick: self.pick,
            occlude: self.occlude,
        }
    }
}

/// Deterministic per-subpixel noise in roughly -12..12, added to the Bayer
/// threshold. Deterministic matters: the stipple must not crawl while panning.
#[inline]
fn jitter(x: isize, y: isize) -> i32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2545_F491);
    h ^= h >> 13;
    (h % 25) as i32 - 12
}

/// Liang-Barsky clip against the canvas plus a small margin.
///
/// Without this, a coastline that runs a thousand screens off the edge still
/// costs a thousand screens' worth of stepping. Clipping first makes cost track
/// what is visible rather than what exists.
fn clip(c: &Canvas, mut a: [f64; 2], mut b: [f64; 2]) -> Option<([f64; 2], [f64; 2])> {
    const M: f64 = 4.0;
    let (xmin, ymin) = (-M, -M);
    let (xmax, ymax) = (c.sw as f64 + M, c.sh as f64 + M);

    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);

    for (p, q) in [
        (-dx, a[0] - xmin),
        (dx, xmax - a[0]),
        (-dy, a[1] - ymin),
        (dy, ymax - a[1]),
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
    let a0 = [a[0] + dx * t0, a[1] + dy * t0];
    let b0 = [a[0] + dx * t1, a[1] + dy * t1];
    a = a0;
    b = b0;
    Some((a, b))
}

pub fn line(c: &mut Canvas, a: [f64; 2], b: [f64; 2], pen: &Pen) {
    let Some((a, b)) = clip(c, a, b) else { return };

    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();

    if len < 1e-6 {
        c.splat(a[0], a[1], pen.alpha, &pen.brush());
        return;
    }

    // Sub-subpixel steps keep the trail continuous at any angle.
    let steps = (len * 2.5).ceil().max(1.0);
    let (ux, uy) = (dx / len, dy / len);
    let (px, py) = (-uy, ux);
    let half = (pen.width * 0.5 - 0.5).max(0.0);

    for s in 0..=(steps as usize) {
        let t = s as f64 / steps;
        let x = a[0] + dx * t;
        let y = a[1] + dy * t;

        // The centreline is written at full alpha to the subpixel that actually
        // contains it. Splatting alone spreads a diagonal thin enough to fall
        // under the dot threshold, which turns solid roads into dotted ones.
        c.plot(x.floor() as isize, y.floor() as isize, pen.alpha, &pen.brush());
        // A weaker splat on top gives the neighbours a soft shoulder, so the
        // brightness still varies with how the line falls across the grid. Kept
        // well under the quadrant threshold: a solid glyph is all-or-nothing
        // over four times the area, so a halo strong enough to trip it would
        // double the apparent width of every road.
        let halo = if pen.mat == crate::canvas::MAT_SOLID { 0.30 } else { 0.5 };
        c.splat(x, y, pen.alpha * halo, &pen.brush());

        // Widen along the perpendicular using coverage as a function of
        // distance from the centreline, rather than stamping fixed offsets.
        // Stepped offsets quantise width to whole subpixels, which made the
        // weight control jump between two or three usable values; with a
        // profile, in-between widths still change how bright the shoulder is,
        // and that reads as thickness.
        let mut o = 0.5;
        while o < half + 0.5 {
            let cov = ((half + 0.5 - o) as f32).clamp(0.0, 1.0);
            let a2 = pen.alpha * cov;
            c.splat(x + px * o, y + py * o, a2, &pen.brush());
            c.splat(x - px * o, y - py * o, a2, &pen.brush());
            o += 0.5;
        }
    }
}

/// Rasterise a segment into the per-cell box-drawing layer.
///
/// Rather than filling pixels, this records which edges of each cell the line
/// crosses; resolve then picks the glyph with exactly those arms. That is what
/// makes crossings render as real junctions instead of two lines overlapping.
pub fn cell_line(c: &mut Canvas, a: [f64; 2], b: [f64; 2], pen: &Pen, heavy: bool) {
    let Some((a, b)) = clip(c, a, b) else { return };
    let brush = pen.brush();

    let sx = crate::canvas::SUB_X as f64;
    let sy = crate::canvas::SUB_Y as f64;
    let (ax, ay) = (a[0] / sx, a[1] / sy);
    let (bx, by) = (b[0] / sx, b[1] / sy);

    let steps = (((bx - ax).abs() + (by - ay).abs()) * 4.0).ceil().max(1.0);
    let (mut cx, mut cy) = (ax.floor() as isize, ay.floor() as isize);
    // A segment shorter than a cell still marks its cell, so nothing vanishes.
    c.connect(cx, cy, 0, &brush, heavy);

    for i in 1..=(steps as usize) {
        let t = i as f64 / steps;
        let (nx, ny) = (
            (ax + (bx - ax) * t).floor() as isize,
            (ay + (by - ay) * t).floor() as isize,
        );
        while (cx, cy) != (nx, ny) {
            // Step one cell at a time; a diagonal jump becomes an L so the
            // chain of arms stays connected.
            let (dx, dy) = if cx != nx {
                ((nx - cx).signum(), 0)
            } else {
                (0, (ny - cy).signum())
            };
            let (out, back) = match (dx, dy) {
                (1, 0) => (2, 8),
                (-1, 0) => (8, 2),
                (0, 1) => (4, 1),
                _ => (1, 4),
            };
            c.connect(cx, cy, out, &brush, heavy);
            cx += dx;
            cy += dy;
            c.connect(cx, cy, back, &brush, heavy);
        }
    }
}

pub fn cell_polyline(c: &mut Canvas, pts: &[[f64; 2]], pen: &Pen, heavy: bool) {
    for w in pts.windows(2) {
        cell_line(c, w[0], w[1], pen, heavy);
    }
}

pub fn polyline(c: &mut Canvas, pts: &[[f64; 2]], pen: &Pen) {
    for w in pts.windows(2) {
        line(c, w[0], w[1], pen);
    }
}

/// Dashed polyline with the dash phase carried across segment joins, so the
/// pattern stays even through corners.
pub fn dashed_polyline(c: &mut Canvas, pts: &[[f64; 2]], pen: &Pen, on: f64, off: f64) {
    let period = on + off;
    let mut phase = 0.0f64;

    for w in pts.windows(2) {
        // Clip before walking: an off-screen segment would otherwise cost one
        // loop iteration per dash for its entire notional length.
        let Some((a, b)) = clip(c, w[0], w[1]) else { continue };
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }

        let mut t = 0.0;
        while t < len {
            let in_period = phase % period;
            let (seg, drawing) = if in_period < on {
                (on - in_period, true)
            } else {
                (period - in_period, false)
            };
            let end = (t + seg).min(len);
            if drawing {
                let p0 = [a[0] + dx * (t / len), a[1] + dy * (t / len)];
                let p1 = [a[0] + dx * (end / len), a[1] + dy * (end / len)];
                line(c, p0, p1, pen);
            }
            phase += end - t;
            t = end;
        }
    }
}

/// Walk the interior of a ring, calling `f(canvas, x, y)` per subpixel.
///
/// Shared by `fill` and `erase` so the two can never disagree about which
/// subpixels are inside -- a mismatch would leave a fringe of stray sea dots
/// along every shoreline.
fn scan<F>(c: &mut Canvas, ring: &[[f64; 2]], mut f: F)
where
    F: FnMut(&mut Canvas, isize, isize),
{
    if ring.len() < 3 {
        return;
    }

    let mut ymin = f64::MAX;
    let mut ymax = f64::MIN;
    for p in ring {
        ymin = ymin.min(p[1]);
        ymax = ymax.max(p[1]);
    }
    let y0 = (ymin.floor().max(0.0)) as isize;
    let y1 = (ymax.ceil().min(c.sh as f64)) as isize;
    if y1 <= y0 {
        return;
    }

    let mut xs: Vec<f64> = Vec::with_capacity(16);
    let n = ring.len();

    for y in y0..y1 {
        let yc = y as f64 + 0.5;
        xs.clear();
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            // Half-open test so a vertex exactly on the scanline counts once.
            if (a[1] <= yc) != (b[1] <= yc) {
                let t = (yc - a[1]) / (b[1] - a[1]);
                xs.push(a[0] + (b[0] - a[0]) * t);
            }
        }
        if xs.len() < 2 {
            continue;
        }
        xs.sort_by(|p, q| p.partial_cmp(q).unwrap_or(std::cmp::Ordering::Equal));

        for k in (0..xs.len() - 1).step_by(2) {
            let xa = xs[k].max(0.0).floor() as isize;
            let xb = xs[k + 1].min(c.sw as f64).ceil() as isize;
            for x in xa..xb {
                f(c, x, y);
            }
        }
    }
}

/// Blank everything inside a ring. Used to cut land out of the ocean wash.
pub fn erase(c: &mut Canvas, ring: &[[f64; 2]]) {
    scan(c, ring, |c, x, y| c.clear_at(x, y));
}

/// Even-odd scanline fill, thinned by the ordered dither.
///
/// `density` is 0..64: 64 fills solid, 6 leaves a fine stipple.
pub fn fill(c: &mut Canvas, ring: &[[f64; 2]], density: u8, pen: &Pen) {
    let alpha = pen.alpha;
    let brush = pen.brush();
    scan(c, ring, move |c, x, y| {
        let threshold =
            BAYER[(y.rem_euclid(8)) as usize][(x.rem_euclid(8)) as usize] as i32 + jitter(x, y);
        if threshold < density as i32 {
            c.plot(x, y, alpha, &brush);
        }
    });
}

/// Fill the whole canvas with the dither -- the ocean wash that land erases.
pub fn wash(c: &mut Canvas, density: u8, pen: &Pen) {
    let brush = pen.brush();
    let (w, h) = (c.sw as isize, c.sh as isize);
    for y in 0..h {
        for x in 0..w {
            let threshold =
                BAYER[(y.rem_euclid(8)) as usize][(x.rem_euclid(8)) as usize] as i32 + jitter(x, y);
            if threshold < density as i32 {
                c.plot(x, y, pen.alpha, &brush);
            }
        }
    }
}
