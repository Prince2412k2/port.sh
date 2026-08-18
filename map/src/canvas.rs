//! Subpixel canvas: the piece that makes a terminal behave like a framebuffer.
//!
//! Each terminal cell holds a 2x4 braille grid, so a WxH terminal is really a
//! 2Wx4H bitmap. Rather than store a bare on/off bit per dot we keep a float
//! coverage value, a depth, and a tint. The resolve pass then turns each 2x4
//! block into one glyph plus one colour, and that is where depth becomes
//! visible: coverage picks *which dots* light up (opacity), depth picks *how
//! bright* they are (fog). Two independent axes out of a monochrome cell.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

pub const SUB_X: usize = 2;
pub const SUB_Y: usize = 4;

/// Depth of "nothing here", far beyond the 0..1 range real features use.
const DEPTH_CLEAR: f32 = 8.0;

/// How far behind a solid surface something may be and still draw. Absorbs the
/// rounding between a draped feature and the terrain sample under it.
const Z_BIAS: f32 = 0.004;

/// Coverage a dot needs before it is switched on.
const DOT_ON: f32 = 0.34;

/// Same, for quadrants. Higher because a quadrant is four times the area: at the
/// braille threshold a one-subpixel-wide road would bloom into a solid block.
const QUAD_ON: f32 = 0.52;

/// Dot bit for [row][col] within a braille cell. The ordering is historical:
/// dots 1-6 fill column-major, then 7 and 8 were bolted on underneath.
const BRAILLE: [[u8; SUB_X]; SUB_Y] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

/// Quadrant blocks, indexed by (TL=1, TR=2, BL=4, BR=8).
///
/// Braille is dotted by construction: its eight dots never touch, so a braille
/// road always reads as a dotted trail rather than a line. Quadrants are solid,
/// so a run of them is a continuous stroke. The cost is vertical resolution --
/// 2x2 per cell against braille's 2x4.
///
/// Unicode 16 octants (U+1CD00) would give solid strokes at the full 2x4, which
/// is exactly what this wants, but no font on a normal system has them yet.
const QUADRANT: [char; 16] = [
    ' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█',
];

pub const TINT_MONO: u8 = 0;
pub const TINT_LANDMARK: u8 = 1;
pub const TINT_SELECT: u8 = 2;
pub const TINT_WATER: u8 = 3;
pub const TINT_MAJOR: u8 = 4;
pub const TINT_MEDIUM: u8 = 5;
pub const TINT_MINOR: u8 = 6;
pub const TINT_RAIL: u8 = 7;
pub const TINT_GREEN: u8 = 8;
pub const TINT_COAST: u8 = 9;
pub const TINT_BORDER: u8 = 10;
pub const TINT_HOME: u8 = 11;

/// Base colour per tint, at full brightness.
///
/// Every hue is heavily desaturated and they sit in a narrow luminance band, so
/// the map reads as a tinted technical drawing rather than a chart. Hue carries
/// *kind*; brightness still carries depth.
const TINT_RGB: [(u8, u8, u8); 12] = [
    (232, 232, 226), // mono: paper white, a hair warm so it doesn't glare
    (255, 176, 64),  // landmark amber
    (110, 224, 255), // selection cyan
    (120, 168, 214), // water
    (255, 226, 176), // motorway / trunk: warm, the brightest thing on the map
    (226, 198, 152), // primary / secondary: tan
    (168, 166, 162), // tertiary / residential: plain grey
    (186, 164, 216), // railway: violet, distinct from every road class
    (140, 186, 140), // parks and green landuse
    (196, 220, 236), // coastline: cool and bright, the structural line
    (214, 158, 178), // administrative border: dusty rose, not a road, not water
    (255, 108, 92),  // your position: the one thing that is never terrain
];

/// Box-drawing glyphs indexed by connected edges (N=1, E=2, S=4, W=8).
///
/// The thinnest stroke a terminal can draw: a hairline through the cell centre,
/// rather than a filled block. The trade is that position and angle quantise to
/// whole cells -- a road can only leave a cell through one of four edges.
const LINE_LIGHT: [char; 16] = [
    ' ', '╵', '╶', '└', '╷', '│', '┌', '├', '╴', '┘', '─', '┴', '┐', '┤', '┬', '┼',
];

/// Heavy variants, so motorways still outrank side streets by weight.
const LINE_HEAVY: [char; 16] = [
    ' ', '╹', '╺', '┗', '╻', '┃', '┏', '┣', '╸', '┛', '━', '┻', '┓', '┫', '┳', '╋',
];

/// Which glyph family a subpixel belongs to. One glyph per cell means a cell
/// has to pick a side; roads win because they are what you trace with your eye.
pub const MAT_DOT: u8 = 0;
pub const MAT_SOLID: u8 = 1;

/// How roads are drawn. Cycled at runtime because the trade-off is genuinely a
/// matter of taste and of what the terminal's font renders well.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoadGlyph {
    /// Braille, 2x4 -- finest positioning, but dotted by construction.
    Dotted,
    /// Quadrant blocks, 2x2 -- continuous, but half a cell is the thinnest
    /// stroke available.
    Block,
    /// Box-drawing -- true hairlines with proper junctions, quantised to cells.
    Line,
}

impl RoadGlyph {
    pub fn next(self) -> Self {
        match self {
            RoadGlyph::Dotted => RoadGlyph::Block,
            RoadGlyph::Block => RoadGlyph::Line,
            RoadGlyph::Line => RoadGlyph::Dotted,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RoadGlyph::Dotted => "braille",
            RoadGlyph::Block => "blocks",
            RoadGlyph::Line => "lines",
        }
    }
}

/// One cell of the box-drawing road layer.
#[derive(Clone, Copy)]
struct LineCell {
    mask: u8,
    heavy: bool,
    tint: u8,
    depth: f32,
    pick: u32,
}

/// Everything a draw call carries besides coverage.
#[derive(Clone, Copy)]
pub struct Brush {
    pub depth: f32,
    pub tint: u8,
    pub mat: u8,
    pub pick: u32,
    /// Take part in hidden-surface removal: reject this write if something
    /// nearer already occupies the subpixel, and claim the subpixel if not.
    ///
    /// Off in 2D. There, depth is a styling device -- "importance as distance"
    /// -- and the paint order is not monotonic in it, so testing against it
    /// would drop minor roads wherever they cross water.
    pub occlude: bool,
}

#[derive(Clone, Copy)]
pub struct Overlay {
    pub ch: char,
    pub tint: u8,
    pub lum: f32,
    pub bold: bool,
}

/// Steps in the brightness ramp.
///
/// Luminance is computed as a float and would otherwise give almost every cell
/// its own slightly-different RGB value. That costs nothing on a local
/// terminal and a great deal over a network: adjacent cells never share a
/// style, so every single one needs its own SGR escape, and a full-screen
/// repaint runs to ~150 KB. Snapping to a ramp makes neighbours share a colour,
/// runs collapse to one escape, and the frame shrinks by roughly an order of
/// magnitude — for a difference that is not visible, because the ramp is finer
/// than the eye resolves on a dark background.
///
/// This is a real trade and the number is the knob: lower for cheaper frames,
/// higher for smoother gradients. Below about 16 the terrain stipple starts to
/// show banding.
const LEVELS: f32 = 24.0;

#[inline]
fn quantise(l: f32) -> f32 {
    (l.clamp(0.0, 1.0) * LEVELS).round() / LEVELS
}

/// Turn a resolved colour into the cheapest escape that still draws it.
///
/// A truecolor escape is `ESC[38;2;R;G;Bm` — up to nineteen bytes, every cell,
/// every frame. A palette index is `ESC[38;5;Nm`, about half that. On the
/// monochrome map (the default) every cell is a grey, and xterm's indices
/// 232..255 *are* a 24-step grey ramp — the same 24 steps `LEVELS` snaps to. So
/// the expensive encoding is buying nothing there and the index is exact.
///
/// Anything with actual hue keeps truecolor: the 6x6x6 cube would shift it, and
/// the map's palette is deliberately narrow-band.
///
/// The trade is that indices depend on the terminal's palette. In practice
/// themes remap 0..15 and leave the cube and ramp alone, which is why only the
/// ramp is used here and never the cube.
#[inline]
pub fn ink(r: u8, g: u8, b: u8) -> Color {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    // Neutral enough that the grey ramp is not a visible change. The map's
    // paper white is (232, 232, 226), a spread of six.
    if max - min <= 8 {
        let v = (max as u16 + min as u16) / 2;
        if v < 4 {
            return Color::Indexed(16); // the ramp starts at 8; below it is black
        }
        let i = (((v as f32) - 8.0) / 10.0).round().clamp(0.0, 23.0) as u8;
        return Color::Indexed(232 + i);
    }
    Color::Rgb(r, g, b)
}

/// The inverse of `ink` for the colours this renderer actually emits.
///
/// Anything compositing over a finished frame — the tour's fade band, say —
/// has to be able to read a cell's colour back. `ink` means that colour is
/// sometimes a palette index, and code that only understands `Color::Rgb` will
/// silently skip those cells rather than fail loudly. Returns `None` for
/// colours this module never produces, which the caller should leave alone.
pub fn rgb_of(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(16) => Some((0, 0, 0)),
        Color::Indexed(i @ 232..=255) => {
            let v = 8 + 10 * (i - 232);
            Some((v, v, v))
        }
        _ => None,
    }
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
        Fog { near: 1.0, far: 0.22, gamma: 1.0 }
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
    mat: Vec<u8>,
    /// Nearest *solid* surface per subpixel. Distinct from `depth`, which only
    /// records what was painted: terrain is a continuous surface even where its
    /// stipple leaves gaps, so it must occlude from the gaps too.
    zbuf: Vec<f32>,
    /// Feature index owning each subpixel, for mouse hit-testing. u32::MAX = none.
    pick: Vec<u32>,
    /// Per-cell box-drawing layer, used only in RoadGlyph::Line mode.
    lines: Vec<Option<LineCell>>,
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
            mat: vec![MAT_DOT; sw * sh],
            zbuf: vec![f32::INFINITY; sw * sh],
            pick: vec![u32::MAX; sw * sh],
            lines: vec![None; cw * ch],
            overlay: vec![None; cw * ch],
        }
    }

    pub fn clear(&mut self) {
        self.cov.fill(0.0);
        self.depth.fill(DEPTH_CLEAR);
        self.tint.fill(TINT_MONO);
        self.mat.fill(MAT_DOT);
        self.zbuf.fill(f32::INFINITY);
        self.pick.fill(u32::MAX);
        self.lines.fill(None);
        self.overlay.fill(None);
    }

    /// Composite one subpixel. Callers draw back-to-front, so a later write is
    /// nearer and takes ownership of the depth/tint/pick attribution.
    #[inline]
    pub fn plot(&mut self, x: isize, y: isize, a: f32, b: &Brush) {
        if a <= 0.001 || x < 0 || y < 0 || x >= self.sw as isize || y >= self.sh as isize {
            return;
        }
        let i = y as usize * self.sw + x as usize;
        if b.occlude {
            // Tolerance, not equality: a road draped on terrain sits exactly on
            // the surface it is being tested against, and would otherwise
            // z-fight its way in and out of view along its length.
            if b.depth > self.zbuf[i] + Z_BIAS {
                return;
            }
            self.zbuf[i] = self.zbuf[i].min(b.depth);
        }
        // Alpha-over: repeated stamps along a line converge to solid instead of
        // clipping, which keeps thick roads even where segments overlap.
        self.cov[i] += a * (1.0 - self.cov[i]);
        if a >= 0.25 || self.depth[i] >= DEPTH_CLEAR {
            self.depth[i] = b.depth;
            self.tint[i] = b.tint;
            self.mat[i] = b.mat;
            self.pick[i] = b.pick;
        }
    }

    /// Claim a subpixel as solid without painting it.
    ///
    /// Terrain is drawn as a stipple but is geometrically opaque; without this
    /// a road behind a ridge shows through the gaps between the dots.
    #[inline]
    pub fn occlude_at(&mut self, x: isize, y: isize, z: f32) {
        if x < 0 || y < 0 || x >= self.sw as isize || y >= self.sh as isize {
            return;
        }
        let i = y as usize * self.sw + x as usize;
        if z < self.zbuf[i] {
            self.zbuf[i] = z;
        }
    }

    /// Bilinear splat of a sample at fractional coords. This is the antialiasing
    /// -- it spreads one sample across the four dots it straddles, so a diagonal
    /// road reads as a smooth run of varying brightness rather than a staircase.
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

    /// Clear one subpixel back to empty.
    ///
    /// OSM has no ocean polygon -- the sea is implied by coastline direction,
    /// not stored as an area. So instead of trying to reconstruct sea polygons,
    /// the whole viewport is dithered as ocean and land rings erase it. This is
    /// the erase.
    #[inline]
    pub fn clear_at(&mut self, x: isize, y: isize) {
        if x < 0 || y < 0 || x >= self.sw as isize || y >= self.sh as isize {
            return;
        }
        let i = y as usize * self.sw + x as usize;
        self.cov[i] = 0.0;
        self.depth[i] = DEPTH_CLEAR;
        self.tint[i] = TINT_MONO;
        self.mat[i] = MAT_DOT;
        self.pick[i] = u32::MAX;
    }

    /// Connect a cell to its neighbours along `mask` (N=1, E=2, S=4, W=8).
    /// Masks accumulate, which is what turns crossing roads into junctions.
    pub fn connect(&mut self, cx: isize, cy: isize, mask: u8, b: &Brush, heavy: bool) {
        if cx < 0 || cy < 0 || cx >= self.cw as isize || cy >= self.ch as isize {
            return;
        }
        let i = cy as usize * self.cw + cx as usize;
        match &mut self.lines[i] {
            Some(c) => {
                c.mask |= mask;
                // Nearest feature owns the cell's colour and its identity.
                if b.depth < c.depth {
                    c.depth = b.depth;
                    c.tint = b.tint;
                    c.pick = b.pick;
                }
                c.heavy |= heavy;
            }
            slot => {
                *slot = Some(LineCell {
                    mask,
                    heavy,
                    tint: b.tint,
                    depth: b.depth,
                    pick: b.pick,
                })
            }
        }
    }

    pub fn set_overlay(&mut self, cx: usize, cy: usize, o: Overlay) {
        if cx < self.cw && cy < self.ch {
            self.overlay[cy * self.cw + cx] = Some(o);
        }
    }


    /// Nearest owning feature within `radius` subpixels of a cell. Used for
    /// hover: the cursor is a whole cell wide, so an exact hit is too strict.
    pub fn pick_near(&self, cx: usize, cy: usize, radius: isize) -> Option<u32> {
        // Box-drawn roads live in the cell layer, not the subpixel buffer.
        if let Some(c) = self.lines.get(cy * self.cw + cx).copied().flatten() {
            if c.pick != u32::MAX {
                return Some(c.pick);
            }
        }
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
                // Prefer the nearest thing under the cursor, then the closest.
                let score = self.depth[i] * 100.0 + (dx * dx + dy * dy) as f32;
                if best.is_none_or(|(b, _)| score < b) {
                    best = Some((score, id));
                }
            }
        }
        best.map(|(_, id)| id)
    }

    /// Collapse the subpixel buffer into glyphs and colours.
    pub fn resolve(&self, buf: &mut Buffer, area: Rect, fog: &Fog, mono: bool) {
        for cy in 0..self.ch.min(area.height as usize) {
            for cx in 0..self.cw.min(area.width as usize) {
                let (sx, sy) = (area.x + cx as u16, area.y + cy as u16);

                if let Some(o) = self.overlay[cy * self.cw + cx] {
                    // Selection and position keep their colour in monochrome:
                    // they are answers to "which one" and "where am I", not
                    // part of the map's palette.
                    let (r, g, b) = if mono && o.tint != TINT_SELECT && o.tint != TINT_HOME {
                        TINT_RGB[0]
                    } else {
                        TINT_RGB[o.tint as usize]
                    };
                    let l = quantise(o.lum);
                    let mut style = Style::default().fg(ink(
                        (r as f32 * l) as u8,
                        (g as f32 * l) as u8,
                        (b as f32 * l) as u8,
                    ));
                    if o.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if let Some(cell) = buf.cell_mut((sx, sy)) {
                        cell.set_char(o.ch).set_style(style);
                    }
                    continue;
                }

                if let Some(l) = self.lines[cy * self.cw + cx] {
                    let table = if l.heavy { &LINE_HEAVY } else { &LINE_LIGHT };
                    let lum = quantise(fog.factor(l.depth));
                    let (r, g, b) = if mono { TINT_RGB[0] } else { TINT_RGB[l.tint as usize] };
                    if let Some(cell) = buf.cell_mut((sx, sy)) {
                        cell.set_char(table[(l.mask & 15) as usize]).set_fg(ink(
                            (r as f32 * lum) as u8,
                            (g as f32 * lum) as u8,
                            (b as f32 * lum) as u8,
                        ));
                    }
                    continue;
                }

                let mut bits = 0u8;
                let mut quad = [0.0f32; 4];
                let mut sum = 0.0f32;
                let mut lit = 0u32;
                let mut near = DEPTH_CLEAR;
                let mut weight = [0.0f32; TINT_RGB.len()];
                let mut solid_w = 0.0f32;
                let mut dot_w = 0.0f32;
                // Fallback for features too faint to trip DOT_ON anywhere in the
                // cell -- without this, thin distant roads vanish entirely.
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
                        if self.mat[i] == MAT_SOLID {
                            solid_w += a;
                        } else {
                            dot_w += a;
                        }
                        // Two braille rows collapse into one quadrant row.
                        let q = (row / 2) * 2 + col;
                        quad[q] = quad[q].max(a);
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
                    .map(|(i, _)| i)
                    .unwrap_or(0);

                // A cell can only be one glyph, so the two families compete and
                // the heavier contribution wins the cell outright.
                let ch = if solid_w > dot_w {
                    // Quadrants cover four times the area of a braille dot, so
                    // they need a higher bar before lighting or thin roads bloom
                    // into blocks.
                    let mut q = 0usize;
                    for (k, &c) in quad.iter().enumerate() {
                        if c >= QUAD_ON {
                            q |= 1 << k;
                        }
                    }
                    if q == 0 {
                        let best = quad
                            .iter()
                            .enumerate()
                            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        if quad[best] < 0.12 {
                            continue;
                        }
                        q = 1 << best;
                    }
                    QUADRANT[q]
                } else {
                    if bits == 0 {
                        if strongest.0 < 0.10 {
                            continue;
                        }
                        bits = strongest.1;
                    }
                    char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
                };

                // Opacity axis (mean coverage) times depth axis (fog). The
                // floor keeps a lit dot from ever being invisible -- if it made
                // it through DOT_ON it should be seen.
                let lum = (0.40 + 0.60 * mean.powf(0.55)) * fog.factor(near);
                // Same exemption as labels: selection and position answer
                // "which one" and "where am I", so they keep their colour even
                // when the map is deliberately monochrome.
                let keep = tint == TINT_SELECT as usize || tint == TINT_HOME as usize;
                let (r, g, b) = if mono && !keep { TINT_RGB[0] } else { TINT_RGB[tint] };
                let l = quantise(lum);
                let color = ink(
                    (r as f32 * l) as u8,
                    (g as f32 * l) as u8,
                    (b as f32 * l) as u8,
                );

                if let Some(cell) = buf.cell_mut((sx, sy)) {
                    cell.set_char(ch).set_fg(color);
                }
            }
        }
    }
}
