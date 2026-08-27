//! Heightmap sampling, at the scale the screen can actually show.
//!
//! Reads the flat grid baked by `scripts/dem2hgt.py`. The file itself is
//! deliberately dumb: one header, one block of i16 metres. It is memory-mapped
//! rather than read into the heap, so resident cost is the working set instead
//! of the whole file, the page cache shares one copy across processes for free,
//! and a world heightmap at this fidelity stops being a memory problem before
//! it exists.
//!
//! On top of that sits a mip pyramid, and that is the part that matters.
//!
//! # Why the raw grid cannot be sampled directly
//!
//! Point-sampling a rough surface on a grid coarser than its own features is
//! aliasing, and that is what the first relief pass did: it walked a screen
//! grid three subpixels apart and asked the heightmap for one number at each
//! point. Measured over Kullu and Lahaul, this DEM has a ridge or a valley
//! every 2.7 km. At z7 three subpixels span 3.1 km, so each sample landed on a
//! different, unrelated ridge and the frame filled with speckle that crawled
//! when the camera moved -- the classic signature, and it reads as "the
//! terrain has too much texture" rather than as what it is.
//!
//! # Why interpolation is not the fix
//!
//! Worth saying because it is the obvious next guess. Bilinear interpolation
//! is only C0, so a hillshade taken from it should in principle show a quilt
//! of facet creases along the DEM cell boundaries. Rendered side by side
//! against Catmull-Rom over the same window, the two are indistinguishable:
//! the mat of small ridges is *real data*, not an interpolation artefact, and
//! no amount of smoothness between the samples removes it. Only removing the
//! features does.
//!
//! # What the fix is
//!
//! Low-pass the elevation before decimating it, which is what a mip pyramid
//! is. Each level is filtered with a binomial `[1,3,3,1]/8` kernel and then
//! halved. A plain 2x2 box is cheaper and leaves twice as much: against a
//! synthetic ripple at the grid's Nyquist, one level down, the box leaves
//! 72 m RMS where the binomial leaves 36. Two levels down both reach zero, so
//! this only buys anything at the first level -- which is the level in use at
//! the zooms where the terrain is sharpest, and so the one worth paying for.
//! Sampling picks the level whose spacing matches the ground each screen
//! sample stands for, and blends across two levels so zooming does not pop.
//!
//! The surface is smoothed and *then* exaggerated, never the other way round.
//! Exaggerating first multiplies every erosion channel by three and then asks
//! the filter to undo it.
//!
//! # How much smoothing
//!
//! More than "one DEM sample per screen sample", which is the intuitive answer
//! and is wrong. Hillshaded over a 111 km window of Kullu at 534 m per sample:
//! matching the screen picks level 0 and looks like noise, and the level above
//! it -- 1.8 km -- is the first that reads as ridges and valleys. That is a
//! kernel three to four times the screen sample spacing, and it works out at
//! roughly one terrain feature per ten samples. One per five is still speckle.
//!
//! The relief this costs is almost nothing, which is why it is safe to be so
//! aggressive: over Kullu the standard deviation of elevation falls from
//! 1520 m at level 0 to 1449 m at level 3, a 5% loss across a 16x change of
//! scale. Smoothing removes *features*, not the mountain.

use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use memmap2::{Mmap, MmapOptions};

const NODATA: i16 = -32768;
/// Magic, version, four f64 bounds, two u32 dimensions.
const HEADER_LEN: u64 = 48;

/// Metres per degree of latitude. Constant enough at this fidelity.
const M_PER_DEG_LAT: f64 = 110_540.0;

/// Coarsest level worth building. Level 8 of the shipped DEM is 236 km per
/// sample, wider than most of what is ever in frame; past that the pyramid is
/// answering questions nobody asks.
const MAX_LEVELS: usize = 9;

/// Smallest level worth building, in samples on a side.
const MIN_SIDE: usize = 8;

/// One level's samples, borrowed from wherever they live.
///
/// Level 0 stays in the mapping, so the 27 MB of it is never copied and never
/// counted against this process twice. Everything above it is built at runtime
/// and owned, which costs a third of level 0 again for the whole pyramid.
#[derive(Clone, Copy)]
enum Cells<'a> {
    /// Little-endian i16, straight off the file.
    Mapped(&'a [u8]),
    Owned(&'a [i16]),
}

#[derive(Clone, Copy)]
struct LevelRef<'a> {
    w: usize,
    h: usize,
    cells: Cells<'a>,
}

impl LevelRef<'_> {
    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        // Clamp rather than return zero. A zero off the edge is a cliff down
        // to the sea that the filter then smears back inland, so a coastal
        // peak grows a halo of false lowland; clamping continues the edge,
        // which is the conventional and less wrong answer.
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        let i = y * self.w + x;
        let v = match self.cells {
            Cells::Mapped(b) => i16::from_le_bytes([b[i * 2], b[i * 2 + 1]]),
            Cells::Owned(v) => v[i],
        };
        if v == NODATA { 0.0 } else { v as f32 }
    }

    /// Bilinear sample at a position in this level's own grid coordinates.
    fn bilinear(&self, fx: f64, fy: f64) -> f32 {
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let a = self.at(x0, y0);
        let top = a + (self.at(x0 + 1, y0) - a) * tx;
        let b = self.at(x0, y0 + 1);
        let bot = b + (self.at(x0 + 1, y0 + 1) - b) * tx;
        top + (bot - top) * ty
    }
}

/// A built level above zero.
struct Owned {
    w: usize,
    h: usize,
    cells: Vec<i16>,
}

/// Height and slope at a point, both taken off the same smoothed surface.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Probe {
    /// Metres above sea level.
    pub h: f32,
    /// Rise per unit run, eastward.
    pub dx: f32,
    /// Rise per unit run, northward.
    pub dy: f32,
}

pub struct Terrain {
    west: f64,
    south: f64,
    east: f64,
    north: f64,
    width: usize,
    height: usize,
    /// Ground metres per level-0 sample, north-south.
    m_per_px: f64,
    /// Row 0 is the north edge.
    base: Mmap,
    /// Levels 1 and up, built on first use rather than on open: a session that
    /// never asks for a smoothed height never pays for it, and the cost is a
    /// scan of the whole file.
    higher: OnceLock<Vec<Owned>>,
}

impl Terrain {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut f = std::fs::File::open(path)?;
        let mut head = [0u8; HEADER_LEN as usize];
        f.read_exact(&mut head)?;
        if &head[0..4] != b"TMHG" || head[4] != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not a v1 .tmhg heightmap",
            ));
        }
        let d = |o: usize| f64::from_le_bytes(head[o..o + 8].try_into().unwrap());
        let u = |o: usize| u32::from_le_bytes(head[o..o + 4].try_into().unwrap()) as usize;
        let (west, south, east, north) = (d(8), d(16), d(24), d(32));
        let (width, height) = (u(40), u(44));

        // SAFETY: the file is baked once by `scripts/dem2hgt.py` and never
        // rewritten while the map is running, so nothing can shorten or edit
        // the bytes underneath the mapping.
        let base = unsafe { MmapOptions::new().offset(HEADER_LEN).map(&f)? };
        if base.len() < width * height * 2 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "heightmap truncated",
            ));
        }

        Ok(Terrain {
            west,
            south,
            east,
            north,
            width,
            height,
            m_per_px: (north - south) / height as f64 * M_PER_DEG_LAT,
            base,
            higher: OnceLock::new(),
        })
    }

    /// Ground metres per sample at level 0: the sharpest this can ever be.
    pub fn resolution(&self) -> f64 {
        self.m_per_px
    }

    /// How many levels the pyramid has, level 0 included.
    pub fn levels(&self) -> usize {
        self.built().len() + 1
    }

    fn built(&self) -> &[Owned] {
        self.higher.get_or_init(|| {
            let mut out: Vec<Owned> = Vec::new();
            while out.len() + 1 < MAX_LEVELS {
                let prev = match out.last() {
                    Some(l) => LevelRef { w: l.w, h: l.h, cells: Cells::Owned(&l.cells) },
                    None => self.level0(),
                };
                if prev.w.min(prev.h) <= MIN_SIDE * 2 {
                    break;
                }
                out.push(halve(prev));
            }
            out
        })
    }

    #[inline]
    fn level0(&self) -> LevelRef<'_> {
        LevelRef { w: self.width, h: self.height, cells: Cells::Mapped(&self.base) }
    }

    fn level(&self, i: usize) -> LevelRef<'_> {
        match i.checked_sub(1).and_then(|j| self.built().get(j)) {
            Some(l) => LevelRef { w: l.w, h: l.h, cells: Cells::Owned(&l.cells) },
            None => self.level0(),
        }
    }

    /// Metres above sea level, low-passed to about `smooth_m` metres.
    ///
    /// `smooth_m` is read as the sample spacing wanted from the surface, so it
    /// selects the level whose own spacing is nearest and blends towards the
    /// next. Anything below the DEM's resolution costs nothing and gets
    /// nothing: level 0 is as sharp as the data goes.
    ///
    /// Outside the grid, and over ocean, this is zero -- the right answer for
    /// both.
    pub fn sample_smooth(&self, lon: f64, lat: f64, smooth_m: f64) -> f32 {
        if !self.inside(lon, lat) {
            return 0.0;
        }
        let (lo, t) = self.pick(smooth_m);
        let a = self.tap(self.level(lo), lon, lat);
        if t <= 0.0 {
            return a;
        }
        let b = self.tap(self.level(lo + 1), lon, lat);
        a + (b - a) * t
    }

    /// Metres above sea level, as sharp as the data goes.
    ///
    /// For draping a road or a building, where the question is "what is the
    /// ground under this one thing" and a smoothed answer would float it over
    /// a valley or bury it in a ridge.
    pub fn sample(&self, lon: f64, lat: f64) -> f32 {
        if !self.inside(lon, lat) {
            return 0.0;
        }
        self.tap(self.level0(), lon, lat)
    }

    /// Height and slope at a point, off one smoothed surface.
    ///
    /// The gradient is a central difference taken *at the smoothing scale*,
    /// not at the screen step. Differencing a field across less than the
    /// kernel that made it measures the interpolator rather than the mountain:
    /// the answer gets noisier the closer together the two taps are, which is
    /// the opposite of what a finer step is supposed to buy.
    pub fn probe(&self, lon: f64, lat: f64, smooth_m: f64) -> Probe {
        let run = smooth_m.max(self.m_per_px);
        let dlat = run / M_PER_DEG_LAT;
        let dlon = dlat / lat.to_radians().cos().max(0.2);
        let e = self.sample_smooth(lon + dlon * 0.5, lat, smooth_m);
        let w = self.sample_smooth(lon - dlon * 0.5, lat, smooth_m);
        let n = self.sample_smooth(lon, lat + dlat * 0.5, smooth_m);
        let s = self.sample_smooth(lon, lat - dlat * 0.5, smooth_m);
        Probe {
            h: self.sample_smooth(lon, lat, smooth_m),
            dx: (e - w) / run as f32,
            dy: (n - s) / run as f32,
        }
    }

    #[inline]
    fn inside(&self, lon: f64, lat: f64) -> bool {
        lon >= self.west && lon < self.east && lat > self.south && lat <= self.north
    }

    /// The level below a smoothing width, and how far towards the next one.
    fn pick(&self, smooth_m: f64) -> (usize, f32) {
        let top = self.levels() - 1;
        let lod = (smooth_m / self.m_per_px).max(1.0).log2();
        let lo = (lod.floor() as usize).min(top);
        (lo, if lo == top { 0.0 } else { (lod - lo as f64) as f32 })
    }

    /// Bilinear sample of one level, from lon/lat.
    fn tap(&self, l: LevelRef<'_>, lon: f64, lat: f64) -> f32 {
        let fx = (lon - self.west) / (self.east - self.west) * l.w as f64 - 0.5;
        let fy = (self.north - lat) / (self.north - self.south) * l.h as f64 - 0.5;
        l.bilinear(fx, fy)
    }
}

/// One level down: blur with `[1,3,3,1]/8` on both axes, then take every
/// second sample.
///
/// Separable, so it is two passes of four taps rather than one of sixteen. The
/// intermediate is f32 because rounding back to i16 once per axis per level
/// accumulates a bias all the way down the pyramid.
fn halve(src: LevelRef<'_>) -> Owned {
    const K: [f32; 4] = [0.125, 0.375, 0.375, 0.125];
    let (w, h) = (src.w / 2, src.h / 2);
    // Horizontal pass filters and decimates in one go, so the intermediate is
    // already half the samples wide.
    let mut mid = vec![0f32; w * src.h];
    for y in 0..src.h {
        for x in 0..w {
            let cx = (x * 2) as isize;
            let mut a = 0.0;
            for (i, k) in K.iter().enumerate() {
                a += k * src.at(cx + i as isize - 1, y as isize);
            }
            mid[y * w + x] = a;
        }
    }
    let mut cells = vec![0i16; w * h];
    for y in 0..h {
        let cy = (y * 2) as isize;
        for x in 0..w {
            let mut a = 0.0;
            for (i, k) in K.iter().enumerate() {
                let yy = (cy + i as isize - 1).clamp(0, src.h as isize - 1) as usize;
                a += k * mid[yy * w + x];
            }
            cells[y * w + x] = a.round().clamp(-32_767.0, 32_767.0) as i16;
        }
    }
    Owned { w, h, cells }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Two degrees square, so the arithmetic in the tests stays legible.
    const W: f64 = 10.0;
    const S: f64 = 20.0;
    const E: f64 = 12.0;
    const N: f64 = 22.0;

    fn bake(name: &str, side: usize, f: impl Fn(usize, usize) -> f32) -> Terrain {
        let p = std::env::temp_dir()
            .join(format!("termap-terrain-{}-{name}.tmhg", std::process::id()));
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TMHG");
        buf.push(1);
        buf.extend_from_slice(&[0u8; 3]);
        for v in [W, S, E, N] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        buf.extend_from_slice(&(side as u32).to_le_bytes());
        buf.extend_from_slice(&(side as u32).to_le_bytes());
        for y in 0..side {
            for x in 0..side {
                buf.extend_from_slice(&(f(x, y).round() as i16).to_le_bytes());
            }
        }
        std::fs::File::create(&p).unwrap().write_all(&buf).unwrap();
        Terrain::open(&p).unwrap()
    }

    /// A broad hill with a fine ripple riding on it: the shape of the actual
    /// complaint, in a form whose two parts can be measured apart.
    fn hill_and_ripple(x: usize, y: usize) -> f32 {
        let (fx, fy) = (x as f32 / 256.0 - 0.5, y as f32 / 256.0 - 0.5);
        let hill = 3000.0 * (-8.0 * (fx * fx + fy * fy)).exp();
        // Period four samples: right at the edge of what the grid can hold,
        // which is where the DEM's own worst texture lives.
        let ripple = 250.0 * (x as f32 * std::f32::consts::PI / 2.0).sin();
        hill + ripple
    }

    /// Height along one row, sampled at `n` points.
    fn row(t: &Terrain, smooth: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let lon = W + (E - W) * (0.25 + 0.5 * i as f64 / n as f64);
                t.sample_smooth(lon, (S + N) * 0.5, smooth)
            })
            .collect()
    }

    #[test]
    fn samples_come_off_the_map_little_endian_with_nodata_as_sea() {
        let t = bake("raw", 4, |x, y| if y == 1 && x == 1 { 500.0 } else { 0.0 });
        // Grid centres sit at 1/8, 3/8, 5/8, 7/8 of the span. Cell (1,1) is
        // 3/8 east of west and 3/8 south of north.
        let lon = W + (E - W) * 3.0 / 8.0;
        let lat = N - (N - S) * 3.0 / 8.0;
        assert!((t.sample(lon, lat) - 500.0).abs() < 1.0, "got {}", t.sample(lon, lat));
        assert_eq!(t.sample(W - 1.0, lat), 0.0, "west of the grid is sea");
    }

    /// The reason this module exists: asking for a coarser surface must
    /// actually remove the fine features, and must not flatten the mountain.
    ///
    /// Both halves are one measurement. The same hill is baked twice, once
    /// with the ripple on it and once without, and the test asks how far the
    /// smoothed noisy surface lands from the smoothed clean one. That is the
    /// claim in full -- a filter that flattened the hill as well would move
    /// away from the clean surface, not towards it -- and it avoids scoring
    /// the ripple's own amplitude as if it were relief, which is the trap the
    /// first version of this test fell into.
    #[test]
    fn smoothing_takes_the_ripple_and_leaves_the_hill() {
        let noisy = bake("ripple", 256, hill_and_ripple);
        let clean = bake("hill", 256, |x, y| {
            let (fx, fy) = (x as f32 / 256.0 - 0.5, y as f32 / 256.0 - 0.5);
            3000.0 * (-8.0 * (fx * fx + fy * fy)).exp()
        });
        let res = noisy.resolution();
        let rms = |a: &[f32], b: &[f32]| -> f32 {
            let n = a.len() as f32;
            (a.iter().zip(b).map(|(p, q)| (p - q) * (p - q)).sum::<f32>() / n).sqrt()
        };

        // Unsmoothed, the two surfaces are apart by the ripple: amplitude 250,
        // so about 177 m RMS. This is the control, and it is what the frame
        // was being asked to draw.
        let raw = rms(&row(&noisy, res, 240), &row(&clean, res, 240));
        assert!(raw > 120.0, "the ripple is not even there to remove: {raw:.0} m");

        // Smoothed, they are the same mountain.
        let got = rms(&row(&noisy, res * 8.0, 240), &row(&clean, res * 8.0, 240));
        assert!(got < raw / 20.0, "ripple survived: {got:.1} m against {raw:.0} m raw");

        // One level down is where the filter is actually chosen. Everything
        // annihilates a ripple given three levels; the binomial gets to a
        // quarter of the raw error in one, and a 2x2 box only gets to a half.
        // This is the assertion that holds the kernel in place.
        let one = rms(&row(&noisy, res * 2.0, 240), &row(&clean, res * 2.0, 240));
        assert!(one < raw * 0.3, "the first level barely filters: {one:.0} m of {raw:.0}");

        // And it is the mountain, not a plain. Against the clean surface's
        // own climb rather than a number: the row crosses the hill from its
        // flank to its summit, which is 1180 m of the 3000 m it was built
        // with, and quoting 3000 here would only be measuring where the row
        // happens to start.
        let climb = |v: &[f32]| v.iter().cloned().fold(f32::MIN, f32::max) - v[0];
        let (want, got) = (climb(&row(&clean, res, 240)), climb(&row(&noisy, res * 8.0, 240)));
        assert!(got > want * 0.9, "smoothed flat: {got:.0} m of climb against {want:.0}");
    }

    /// Zooming must not pop. Without the blend between levels the surface
    /// jumps every time the chosen level changes, and on a moving camera that
    /// is a visible shudder rather than a gradual softening.
    #[test]
    fn the_surface_slides_between_levels_instead_of_stepping() {
        let t = bake("blend", 256, hill_and_ripple);
        let res = t.resolution();
        // Off-centre, where the hill has slope and the levels disagree most.
        let (lon, lat) = (W + (E - W) * 0.42, S + (N - S) * 0.55);

        let mut worst = 0.0f32;
        let mut prev = t.sample_smooth(lon, lat, res);
        // A hundred steps across four octaves: several level boundaries are
        // crossed, and none of them may be findable in the output.
        for i in 1..=100 {
            let s = res * 16f64.powf(i as f64 / 100.0);
            let now = t.sample_smooth(lon, lat, s);
            worst = worst.max((now - prev).abs());
            prev = now;
        }
        assert!(worst < 12.0, "a level boundary shows: {worst:.1} m in one step");
    }

    /// Slope points uphill, and is measured in rise over run.
    #[test]
    fn the_gradient_faces_up_the_slope() {
        // 2000 m of rise across two degrees of longitude, eastward.
        let t = bake("ramp", 128, |x, _| x as f32 / 128.0 * 2000.0);
        let res = t.resolution();
        let p = t.probe(11.0, 21.0, res * 4.0);

        // A degree of longitude at 21 N is 111320*cos(21) metres.
        let want = 1000.0 / (111_320.0 * 21f64.to_radians().cos()) as f32;
        assert!((p.dx - want).abs() < want * 0.1, "east slope {} against {want}", p.dx);
        assert!(p.dy.abs() < want * 0.1, "a ramp with no north-south slope has none: {}", p.dy);
    }

    /// The pyramid stops, and stops somewhere sensible.
    #[test]
    fn the_pyramid_is_finite() {
        let t = bake("depth", 256, |_, _| 100.0);
        assert_eq!(t.levels(), 5, "256 halves to 16 before it hits the floor");
        // Past the top, the coarsest level answers rather than an index panic.
        assert!((t.sample_smooth(11.0, 21.0, 1e9) - 100.0).abs() < 1.0);
    }
}
