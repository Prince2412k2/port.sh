#![allow(dead_code)]
//! Subpixel canvas: the piece that makes a terminal behave like a framebuffer.
//!
//! Carried over from termap and kept whole rather than trimmed to today's
//! callers: the sheet uses the splat path, the projects tab will want the
//! overlay and fade paths, and a module that keeps arriving back in pieces is
//! worse than a few unused constants.
//!
//! This is termap's `canvas.rs` with the road machinery removed — a sky has no
//! junctions to resolve, so the box-drawing and quadrant families go with it.
//! Everything that remains is byte-compatible in behaviour, so when the two
//! apps are combined this file is a strict subset of that one and disappears.
//!
//! Each terminal cell holds a 2x4 braille grid, so a WxH terminal is really a
//! 2Wx4H bitmap. Rather than store a bare on/off bit per dot we keep a float
//! coverage value, a depth and a tint. The resolve pass turns each 2x4 block
//! into one glyph plus one colour, and that is where depth becomes visible:
//! coverage picks *which dots* light up, depth picks *how bright* they are.
//!
//! For a star field that split is the whole illusion. A star is faint because
//! it is small, or faint because it is far, and the eye reads those as
//! different facts about the same dot.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

pub const SUB_X: usize = 2;
pub const SUB_Y: usize = 4;

/// Depth of "nothing here", far beyond the 0..1 range real features use.
const DEPTH_CLEAR: f32 = 8.0;

/// Coverage a dot needs before it is switched on.
const DOT_ON: f32 = 0.34;

/// Dot bit for [row][col] within a braille cell. The ordering is historical:
/// dots 1-6 fill column-major, then 7 and 8 were bolted on underneath.
const BRAILLE: [[u8; SUB_X]; SUB_Y] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

pub const TINT_MONO: u8 = 0;
pub const TINT_SELECT: u8 = 1;
pub const TINT_DIM: u8 = 2;
pub const TINT_FAINT: u8 = 3;
/// First of the nine per-constellation hues. `TINT_CON + i` is constellation i.
pub const TINT_CON: u8 = 4;
pub const CON_TINTS: usize = 9;

/// Base colour per tint, at full brightness.
///
/// Every hue is heavily desaturated and they sit in a narrow luminance band,
/// same as the map's: hue carries *which constellation*, brightness carries
/// depth. A sky drawn in saturated primaries reads as a chart of skills. This
/// one is meant to read as a sky that happens to be labelled.
const TINT_RGB: [(u8, u8, u8); 4 + CON_TINTS] = [
    (232, 232, 226), // mono: paper white, a hair warm so it doesn't glare
    (110, 224, 255), // selection cyan — the one answer to "which one"
    (108, 110, 118), // dimmed: a constellation that is not the focus
    (118, 124, 140), // faint: background stars and the milky way
    // the nine constellations, in file order
    (196, 220, 236), // termap          cool blue-white
    (140, 186, 140), // stylized-maps   green
    (186, 164, 216), // watch-party     violet
    (236, 214, 168), // logify          warm cream
    (214, 158, 178), // Noter           dusty rose
    (176, 174, 168), // vcs             grey
    (222, 196, 150), // gitswitch       tan
    (146, 206, 226), // clip            pale cyan
    (230, 172, 110), // netjail         amber
];

#[derive(Clone, Copy)]
pub struct Brush {
    pub depth: f32,
    pub tint: u8,
    pub pick: u32,
}

impl Brush {
    pub fn new(depth: f32, tint: u8) -> Self {
        Brush { depth, tint, pick: u32::MAX }
    }
}

#[derive(Clone, Copy)]
pub struct Overlay {
    pub ch: char,
    pub tint: u8,
    pub lum: f32,
    pub bold: bool,
}

/// How depth is turned into brightness.
#[derive(Clone, Copy)]
pub struct Fog {
    /// Brightness multiplier at depth 0.
    pub near: f32,
    /// Brightness multiplier at depth 1.
    pub far: f32,
    /// Shapes the falloff. >1 keeps the near band bright longer.
    pub gamma: f32,
}

impl Default for Fog {
    fn default() -> Self {
        // Flatter than it looks like it should be. Depth here spans a real
        // range — a first-magnitude skill against the dust behind it — but
        // almost every star sits in the middle of that range, so a steep curve
        // sends the whole sky to near-black and leaves the labels floating on
        // nothing. The floor is what the dimmest thing is allowed to be, and
        // on a terminal that is not much above the background.
        Fog { near: 1.0, far: 0.30, gamma: 1.1 }
    }
}

impl Fog {
    #[inline]
    pub fn factor(&self, depth: f32) -> f32 {
        let d = depth.clamp(0.0, 1.0);
        self.far + (self.near - self.far) * (1.0 - d).powf(self.gamma)
    }
}

pub struct Canvas {
    pub cw: usize,
    pub ch: usize,
    pub sw: usize,
    pub sh: usize,
    cov: Vec<f32>,
    depth: Vec<f32>,
    tint: Vec<u8>,
    /// Star index owning each subpixel, for mouse hit-testing. u32::MAX = none.
    pick: Vec<u32>,
    overlay: Vec<Option<Overlay>>,
}

impl Canvas {
    pub fn new(cw: usize, ch: usize) -> Self {
        let (sw, sh) = (cw * SUB_X, ch * SUB_Y);
        Canvas {
            cw,
            ch,
            sw,
            sh,
            cov: vec![0.0; sw * sh],
            depth: vec![DEPTH_CLEAR; sw * sh],
            tint: vec![TINT_MONO; sw * sh],
            pick: vec![u32::MAX; sw * sh],
            overlay: vec![None; cw * ch],
        }
    }

    pub fn clear(&mut self) {
        self.cov.fill(0.0);
        self.depth.fill(DEPTH_CLEAR);
        self.tint.fill(TINT_MONO);
        self.pick.fill(u32::MAX);
        self.overlay.fill(None);
    }

    /// Composite one subpixel. Callers draw back to front, so a later write is
    /// nearer and takes ownership of the depth/tint/pick attribution.
    #[inline]
    pub fn plot(&mut self, x: isize, y: isize, a: f32, b: &Brush) {
        if a <= 0.001 || x < 0 || y < 0 || x >= self.sw as isize || y >= self.sh as isize {
            return;
        }
        let i = y as usize * self.sw + x as usize;
        // Alpha-over: overlapping glow stamps converge to solid instead of
        // clipping, so two near stars brighten each other rather than banding.
        self.cov[i] += a * (1.0 - self.cov[i]);
        if a >= 0.25 || self.depth[i] >= DEPTH_CLEAR {
            self.depth[i] = b.depth;
            self.tint[i] = b.tint;
            self.pick[i] = b.pick;
        }
    }

    /// Bilinear splat of a sample at fractional coords. This is the
    /// antialiasing: it spreads one sample across the four dots it straddles,
    /// so a star can sit *between* dots and still look like it has a position.
    #[inline]
    pub fn splat(&mut self, x: f64, y: f64, a: f32, b: &Brush) {
        let fx = x.floor();
        let fy = y.floor();
        let tx = (x - fx) as f32;
        let ty = (y - fy) as f32;
        let (ix, iy) = (fx as isize, fy as isize);
        self.plot(ix, iy, a * (1.0 - tx) * (1.0 - ty), b);
        self.plot(ix + 1, iy, a * tx * (1.0 - ty), b);
        self.plot(ix, iy + 1, a * (1.0 - tx) * ty, b);
        self.plot(ix + 1, iy + 1, a * tx * ty, b);
    }

    /// Scale down whatever coverage a cell already holds.
    ///
    /// Used to open a clearing around the text: a paragraph dropped straight
    /// onto a star field has its serifs competing with the dust for the same
    /// cells, and the eye has to work at separating them. Fading a ring of
    /// cells around the block is the terminal's version of a drop shadow.
    pub fn fade_cell(&mut self, cx: usize, cy: usize, k: f32) {
        if cx >= self.cw || cy >= self.ch {
            return;
        }
        for row in 0..SUB_Y {
            for col in 0..SUB_X {
                let i = (cy * SUB_Y + row) * self.sw + cx * SUB_X + col;
                self.cov[i] *= k;
            }
        }
    }

    pub fn set_overlay(&mut self, cx: usize, cy: usize, o: Overlay) {
        if cx < self.cw && cy < self.ch {
            self.overlay[cy * self.cw + cx] = Some(o);
        }
    }

    /// Nearest owning star within `radius` subpixels of a cell. Used for hover:
    /// the cursor is a whole cell wide, so an exact hit is far too strict.
    pub fn pick_near(&self, cx: usize, cy: usize, radius: isize) -> Option<u32> {
        let (px, py) = ((cx * SUB_X + 1) as isize, (cy * SUB_Y + 2) as isize);
        let mut best: Option<(f32, u32)> = None;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let (x, y) = (px + dx, py + dy);
                if x < 0 || y < 0 || x >= self.sw as isize || y >= self.sh as isize {
                    continue;
                }
                let i = y as usize * self.sw + x as usize;
                let id = self.pick[i];
                if id == u32::MAX || self.cov[i] < DOT_ON {
                    continue;
                }
                // Prefer the nearest star under the cursor, then the closest.
                let score = self.depth[i] * 100.0 + (dx * dx + dy * dy) as f32;
                if best.is_none_or(|(b, _)| score < b) {
                    best = Some((score, id));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    pub fn rgb(tint: u8, lum: f32, mono: bool) -> Color {
        // Selection keeps its colour in monochrome: it answers "which one",
        // which is not part of the palette's job.
        let (r, g, b) = if mono && tint != TINT_SELECT {
            TINT_RGB[0]
        } else {
            TINT_RGB[(tint as usize).min(TINT_RGB.len() - 1)]
        };
        let l = lum.clamp(0.0, 1.0);
        Color::Rgb(
            (r as f32 * l) as u8,
            (g as f32 * l) as u8,
            (b as f32 * l) as u8,
        )
    }

    /// Collapse the subpixel buffer into glyphs and colours.
    pub fn resolve(&self, buf: &mut Buffer, area: Rect, fog: &Fog, mono: bool) {
        for cy in 0..self.ch.min(area.height as usize) {
            for cx in 0..self.cw.min(area.width as usize) {
                let (sx, sy) = (area.x + cx as u16, area.y + cy as u16);

                if let Some(o) = self.overlay[cy * self.cw + cx] {
                    let mut style = Style::default().fg(Self::rgb(o.tint, o.lum, mono));
                    if o.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if let Some(cell) = buf.cell_mut((sx, sy)) {
                        cell.set_char(o.ch).set_style(style);
                    }
                    continue;
                }

                let mut bits = 0u8;
                let mut sum = 0.0f32;
                let mut lit = 0u32;
                let mut near = DEPTH_CLEAR;
                let mut weight = [0.0f32; TINT_RGB.len()];
                // Fallback for anything too faint to trip DOT_ON anywhere in
                // the cell — without it the whole background sky vanishes.
                let mut strongest = (0.0f32, 0u8);

                for (row, dots) in BRAILLE.iter().enumerate() {
                    for (col, &dot) in dots.iter().enumerate() {
                        let i = (cy * SUB_Y + row) * self.sw + cx * SUB_X + col;
                        let a = self.cov[i];
                        if a <= 0.0 {
                            continue;
                        }
                        sum += a;
                        lit += 1;
                        weight[self.tint[i] as usize] += a;
                        near = near.min(self.depth[i]);
                        if a >= DOT_ON {
                            bits |= dot;
                        }
                        if a > strongest.0 {
                            strongest = (a, dot);
                        }
                    }
                }

                if lit == 0 {
                    continue;
                }

                let mean = sum / lit as f32;
                let tint = weight
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .map(|(i, _)| i as u8)
                    .unwrap_or(0);

                if bits == 0 {
                    if strongest.0 < 0.055 {
                        continue;
                    }
                    bits = strongest.1;
                }

                // Opacity axis (mean coverage) times depth axis (fog). The
                // floor keeps a lit dot from ever being invisible — if it made
                // it through DOT_ON it should be seen.
                let lum = (0.34 + 0.66 * mean.powf(0.55)) * fog.factor(near);

                if let Some(cell) = buf.cell_mut((sx, sy)) {
                    cell.set_char(char::from_u32(0x2800 + bits as u32).unwrap_or(' '))
                        .set_fg(Self::rgb(tint, lum, mono));
                }
            }
        }
    }
}
