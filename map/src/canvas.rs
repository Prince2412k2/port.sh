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

/// The quietest a cell with any ink in it can be.
///
/// Small on purpose. See the note where `tone` is computed: this was 0.40 and
/// it cost the bottom of the ramp, which is most of what made a frame of
/// terrain read as one flat texture.
const INK_FLOOR: f32 = 0.06;

/// How far the ground has to step, in normalised depth, before the near side
/// of the step is outlined. Roughly one `VEIL_STEP`: the same distance the
/// veil already treats as "a layer further back".
pub const RIM_STEP: f32 = 0.03;

/// Shapes coverage into tone. Below 1 it lifts faint marks, which is what
/// makes a low floor survivable.
const INK_GAMMA: f32 = 0.5;

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

/// Which way round the map is drawn.
///
/// Not a palette swap. On a dark terminal a mark is *added* to the ground --
/// more of it means brighter -- and on paper a mark is taken out of the page,
/// so more of it means darker. Every colour in the renderer is computed as a
/// strength between 0 and 1, and the two themes spend that strength in
/// opposite directions. Swapping only the numbers would give white ink on
/// white paper.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Theme {
    /// Light on black: the terminal's own ground.
    #[default]
    Night,
    /// Ink on paper.
    Paper,
}

/// The page: what a cell looks like where nothing was drawn.
///
/// The paper is warm on purpose and it is not free. `ink()` encodes neutral
/// greys as xterm ramp indices, twelve bytes, and anything with hue as
/// truecolor, twenty -- and a cream page puts hue into every cell of the map,
/// background included. A full repaint of Ahmedabad goes 48 KB to 71 KB.
///
/// Taken knowingly. A neutral page would ride the ramp and be cheap, but the
/// ramp *is* neutral, so it would render as flat light grey and the one thing
/// asked for here was that the background look like paper. Night is untouched
/// and stays the default, so nobody pays this who did not ask for it.
const PAGE: [(u8, u8, u8); 2] = [
    (8, 9, 11),      // night: near-black, a touch cool
    (238, 234, 224), // paper: warm cream, off-white so it is not a glare
];

impl Theme {
    pub fn next(self) -> Theme {
        match self {
            Theme::Night => Theme::Paper,
            Theme::Paper => Theme::Night,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::Night => "night",
            Theme::Paper => "paper",
        }
    }

    /// The colour of an untouched cell.
    pub fn page(self) -> Color {
        let (r, g, b) = PAGE[self as usize];
        ink(r, g, b)
    }

    /// The colour of a mark at full strength: chrome text, the strongest rule.
    pub fn ink(self) -> Color {
        self.grey(1.0)
    }

    /// A mark at the strength chrome uses for things it is not asking you to
    /// read yet -- key hints, inactive rows, the scalebar's rule.
    pub fn faint(self) -> Color {
        self.grey(0.42)
    }

    /// Quieter still: rules, separators, the things that are structure rather
    /// than content.
    pub fn ghost(self) -> Color {
        self.grey(0.26)
    }

    /// Neutral ink at an arbitrary strength.
    pub fn grey(self, strength: f32) -> Color {
        self.paint(TINT_MONO as usize, strength, false, false)
    }

    /// A tint at full strength, exempt from monochrome: the accents chrome
    /// uses to mean "this one" and "you are here".
    pub fn accent(self, tint: u8) -> Color {
        self.paint(tint as usize, 1.0, false, true)
    }

    /// The warm accent: headings, the thing being pointed at.
    pub fn amber(self) -> Color {
        self.accent(TINT_LANDMARK)
    }

    /// The cool accent: selection, the thing being edited.
    pub fn cyan(self) -> Color {
        self.accent(TINT_SELECT)
    }

    fn tints(self) -> &'static [(u8, u8, u8); 12] {
        match self {
            Theme::Night => &TINT_NIGHT,
            Theme::Paper => &TINT_PAPER,
        }
    }

    /// A tint at a strength, as the colour that actually goes on the cell.
    ///
    /// `lum` is how much of the mark is there: coverage times depth haze. On
    /// night that scales the tint up from black. On paper it pulls the page
    /// down toward the ink. Same number, opposite direction, and this is the
    /// only place in the renderer that needs to know which way round it is.
    ///
    /// `keep` exempts a tint from monochrome. Selection and position answer
    /// "which one" and "where am I", not "what kind of thing is this".
    pub fn paint(self, tint: usize, lum: f32, mono: bool, keep: bool) -> Color {
        let table = self.tints();
        let (r, g, b) = if mono && !keep { table[0] } else { table[tint] };
        let l = quantise_for(lum, r, g, b);
        match self {
            Theme::Night => ink((r as f32 * l) as u8, (g as f32 * l) as u8, (b as f32 * l) as u8),
            Theme::Paper => {
                let (pr, pg, pb) = PAGE[1];
                let mix = |page: u8, tint: u8| (page as f32 + (tint as f32 - page as f32) * l) as u8;
                ink(mix(pr, r), mix(pg, g), mix(pb, b))
            }
        }
    }
}

/// Base colour per tint, at full brightness, on black.
///
/// Every hue is heavily desaturated and they sit in a narrow luminance band, so
/// the map reads as a tinted technical drawing rather than a chart. Hue carries
/// *kind*; brightness still carries depth.
const TINT_NIGHT: [(u8, u8, u8); 12] = [
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

/// The same twelve on paper: what the mark is at full strength, which here
/// means the darkest the page gets rather than the brightest.
///
/// Not the night table inverted. Inverting hue gives the complementary colour,
/// so water comes out orange. These are the same hues taken down instead of
/// up, and the luminance order is deliberately *reversed*: the motorway is the
/// brightest thing on black and the darkest on paper, because on both grounds
/// the answer to "which is loudest" has to be the same road.
const TINT_PAPER: [(u8, u8, u8); 12] = [
    (34, 32, 30),    // mono: warm near-black, the pen
    (168, 96, 8),    // landmark amber
    (0, 104, 140),   // selection cyan
    (64, 108, 156),  // water
    (28, 26, 24),    // motorway / trunk: the darkest line on the page
    (92, 68, 36),    // primary / secondary: sepia
    (118, 116, 112), // tertiary / residential: plain grey, and it stays quiet
    (86, 60, 132),   // railway: violet
    (56, 100, 56),   // parks and green landuse
    (52, 92, 124),   // coastline: the structural line
    (140, 62, 92),   // administrative border: dusty rose
    (206, 36, 20),   // your position
];

/// How hard a tint argues for the cell's colour, against how much of it it
/// actually covers.
///
/// A cell carries one colour and the tints used to vote on it by coverage
/// alone. That is the wrong election over terrain. The braille glyph already
/// merges every dot in the cell whatever drew it, so a road crossing a hillside
/// *is* drawn -- its dots are in there -- but the stipple around it outvotes it
/// several times over and the whole cell comes out terrain green. The road is
/// on the screen and invisible, which took an embarrassingly long time to see
/// because every measurement said it was being drawn.
///
/// So coverage is weighted by what the thing is. A road is what you trace with
/// your eye and ground is what it is traced against; the canvas already says so
/// about which glyph family wins a cell, and this is the same sentence about
/// colour. Ground sits below 1.0 rather than roads far above it, so a cell with
/// nothing else in it still reads as exactly what it is.
const TINT_PULL: [f32; 12] = [
    1.0, // mono
    2.0, // landmark
    4.0, // selection: whatever you asked about wins outright
    1.0, // water
    3.0, // motorway / trunk
    2.6, // primary / secondary
    2.2, // tertiary / residential
    2.4, // railway
    0.7, // parks, landuse, terrain -- the page, not the drawing
    1.6, // coastline
    1.4, // administrative border
    4.0, // your position
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
/// Direction, not coverage.
///
/// A cell of this family reads the *shape* of what was drawn into it and picks
/// the line glyph that runs the same way. Braille can put a mark anywhere in a
/// cell but every mark looks the same; these four look like the thing they are
/// describing, which is worth more than sub-cell placement when what is being
/// said is "the ground falls this way".
pub const MAT_HATCH: u8 = 2;
/// Tone. Coverage becomes one of four densities rather than a dot pattern.
///
/// Braille at low coverage reads as speckle -- individual dots the eye counts.
/// A shade block at the same coverage reads as a surface, which is the right
/// answer when the thing being drawn is ground rather than a line on it.
pub const MAT_SHADE: u8 = 3;

/// Line glyphs by direction: horizontal, down-right, vertical, down-left.
const HATCH: [char; 4] = ['\u{2500}', '\u{2572}', '\u{2502}', '\u{2571}'];

/// The line glyph that runs closest to `theta`, an axis angle in radians.
///
/// Screen y runs downward, so a positive angle is a stroke falling to the
/// right. The axis is undirected -- a stroke and its reverse are the same
/// line -- so this only has to cover a half turn, and the four glyphs split it
/// into quarters centred on horizontal, the two diagonals, and vertical.
fn hatch_of(theta: f32) -> char {
    // Folded into a half turn first. The caller's `0.5 * atan2` already lands
    // in this range, but a function that quietly returns the wrong glyph for an
    // angle one turn away is a trap for the next caller, and the fix is a line.
    let mut theta = theta % std::f32::consts::PI;
    if theta > std::f32::consts::FRAC_PI_2 {
        theta -= std::f32::consts::PI;
    } else if theta < -std::f32::consts::FRAC_PI_2 {
        theta += std::f32::consts::PI;
    }
    let eighth = std::f32::consts::FRAC_PI_8;
    let k = if theta.abs() < eighth {
        0
    } else if theta >= eighth && theta < 3.0 * eighth {
        1
    } else if theta.abs() >= 3.0 * eighth {
        2
    } else {
        3
    };
    HATCH[k]
}

/// Coverage ramp, lightest first.
const SHADES: [char; 4] = ['\u{2591}', '\u{2592}', '\u{2593}', '\u{2588}'];

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

/// What a mark does when something is already in front of it.
///
/// Two of these are the old boolean. The third is the one that makes a tilted
/// view of mountains readable.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Behind {
    /// Draw anyway. Chrome, labels, anything that is not in the world.
    #[default]
    Ignore,
    /// Do not draw at all. Hidden-surface removal, for anything opaque.
    Hide,
    /// Draw, one step dimmer for each layer of ground in front of it.
    ///
    /// `Hide` is the right answer for a surface and the wrong one for the lines
    /// drawn on it: a contour that vanishes behind a ridge leaves a hole, and a
    /// contour that ignores the ridge entirely gives what the Himalayas at z10
    /// gave -- seven iso-lines, each of them thousands of segments long, all at
    /// full strength, stacked into a wall of ink with no front or back to it.
    ///
    /// Between the two: the further behind something is, the fainter it draws,
    /// in steps rather than smoothly. Steps because the eye reads three or four
    /// distinct planes far better than it reads a continuous ramp, and because
    /// "one layer back" is the thing being communicated.
    Veil,
}

/// Depth counted as one layer of ground.
const VEIL_STEP: f32 = 0.035;
/// What is left of a mark after one layer.
const VEIL_FADE: f32 = 0.55;
/// Layers after which there is nothing worth drawing.
const VEIL_LIMIT: f32 = 4.0;

/// Everything a draw call carries besides coverage.
#[derive(Clone, Copy)]
pub struct Brush {
    pub depth: f32,
    pub tint: u8,
    pub mat: u8,
    pub pick: u32,
    /// What to do when something nearer already occupies the subpixel.
    ///
    /// Off in 2D. There, depth is a styling device -- "importance as distance"
    /// -- and the paint order is not monotonic in it, so testing against it
    /// would drop minor roads wherever they cross water.
    pub behind: Behind,
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
///
/// The ramp is two-tier. Neutral tints must stay on 24, because xterm's grey
/// indices 232..255 *are* that ramp and the cheap encoding depends on landing
/// exactly on it. Anything with hue rides truecolor anyway, so it takes a finer
/// 48 — the extra steps cost nothing on the wire for cells that were never
/// going to share an index escape, and they are where gradients live (museum
/// fields, water, tinted terrain in colour mode).
const LEVELS: f32 = 24.0;
const LEVELS_HUED: f32 = 48.0;

#[inline]
fn quantise(l: f32) -> f32 {
    (l.clamp(0.0, 1.0) * LEVELS).round() / LEVELS
}

#[inline]
fn quantise_hued(l: f32) -> f32 {
    (l.clamp(0.0, 1.0) * LEVELS_HUED).round() / LEVELS_HUED
}

/// Same spread rule `ink()` applies to the resolved colour, decided here on the
/// unmultiplied tint instead -- so a tint either rides the grey index ramp end
/// to end or keeps truecolor the whole way, never switches mid-fade.
#[inline]
fn is_neutral(r: u8, g: u8, b: u8) -> bool {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    max - min <= 8
}

#[inline]
fn quantise_for(l: f32, r: u8, g: u8, b: u8) -> f32 {
    if is_neutral(r, g, b) { quantise(l) } else { quantise_hued(l) }
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
        // Tolerance, not equality: a road draped on terrain sits exactly on the
        // surface it is being tested against, and would otherwise z-fight its
        // way in and out of view along its length.
        let gap = b.depth - self.zbuf[i];
        let mut a = a;
        match b.behind {
            Behind::Ignore => {}
            Behind::Hide => {
                if gap > Z_BIAS {
                    return;
                }
                self.zbuf[i] = self.zbuf[i].min(b.depth);
            }
            Behind::Veil => {
                if gap > Z_BIAS {
                    let steps = (gap / VEIL_STEP).floor();
                    if steps >= VEIL_LIMIT {
                        return;
                    }
                    a *= VEIL_FADE.powf(steps);
                } else {
                    // In front: this is now what later marks are behind.
                    self.zbuf[i] = self.zbuf[i].min(b.depth);
                }
            }
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

    /// Draw the edge of the ground: everywhere it stops, or steps.
    ///
    /// The thing that makes a braille surface read as a *surface* rather than
    /// as texture. A field of dots has no boundary, so the eye has nothing to
    /// segment on and reads the whole frame as one material -- which is the
    /// entire complaint about terrain here, and it is not a complaint about
    /// braille. Give the same dots an outline and they become a mass with an
    /// inside and an outside.
    ///
    /// Two kinds of edge, and both come off the depth buffer rather than out
    /// of the heightmap, so this needs no geometry and no detection:
    ///
    /// * ground next to no ground -- the outer boundary,
    /// * ground next to ground a long way further off -- a step, which is one
    ///   landform standing in front of another. That is the outline that makes
    ///   a near ridge separate from the range behind it instead of merging
    ///   into it.
    ///
    /// The near side is the one that gets the mark. An outline belongs to the
    /// thing in front; drawn on the far side it reads as a shadow cast by
    /// nothing.
    ///
    /// Collected before anything is drawn, because plotting writes coverage
    /// and tint that the scan would otherwise pick up as it went and trail an
    /// edge along behind itself.
    pub fn rim(&mut self, jump: f32, alpha: f32) {
        let mut edge: Vec<(usize, f32, u8)> = Vec::new();
        let mark = |a: usize, b: usize, this: &Self, out: &mut Vec<(usize, f32, u8)>| {
            let (za, zb) = (this.zbuf[a], this.zbuf[b]);
            match (za.is_finite(), zb.is_finite()) {
                (true, false) => out.push((a, za, this.tint[a])),
                (false, true) => out.push((b, zb, this.tint[b])),
                (true, true) if (za - zb).abs() > jump => {
                    let near = if za < zb { a } else { b };
                    out.push((near, this.zbuf[near], this.tint[near]));
                }
                _ => {}
            }
        };
        for y in 0..self.sh {
            for x in 0..self.sw {
                let i = y * self.sw + x;
                if x + 1 < self.sw {
                    mark(i, i + 1, self, &mut edge);
                }
                if y + 1 < self.sh {
                    mark(i, i + self.sw, self, &mut edge);
                }
            }
        }
        for (i, depth, tint) in edge {
            let (x, y) = ((i % self.sw) as isize, (i / self.sw) as isize);
            self.plot(
                x,
                y,
                alpha,
                &Brush {
                    depth,
                    tint,
                    // Braille, deliberately. The ask was for the ground to
                    // have a border, not for it to stop being braille -- and
                    // a run of dots along an edge reads as a line for the same
                    // reason a dotted road does.
                    mat: MAT_DOT,
                    pick: u32::MAX,
                    // It is the boundary of the nearest thing in its column,
                    // so there is nothing it could be behind, and a z-fight
                    // with the surface it is the edge of would break the line
                    // exactly where it has to hold.
                    behind: Behind::Ignore,
                },
            );
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
        // Nothing non-finite gets into the buffer.
        //
        // This is a net rather than a diagnosis: the caller that hands over a
        // NaN has a bug of its own and should be fixed there. But a NaN here is
        // uniquely nasty. `NaN as isize` saturates to zero, so the write lands
        // on a real subpixel rather than being caught by a bounds check, and
        // the coverage it stores is NaN. The frame then draws fine and the
        // renderer panics later and elsewhere -- in the resolve pass, sorting
        // tint weights, with nothing left to say which draw call was at fault.
        if !x.is_finite() || !y.is_finite() || !a.is_finite() {
            return;
        }
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
    pub fn resolve(&self, buf: &mut Buffer, area: Rect, fog: &Fog, mono: bool, theme: Theme) {
        for cy in 0..self.ch.min(area.height as usize) {
            for cx in 0..self.cw.min(area.width as usize) {
                let (sx, sy) = (area.x + cx as u16, area.y + cy as u16);

                if let Some(o) = self.overlay[cy * self.cw + cx] {
                    // Selection and position keep their colour in monochrome:
                    // they are answers to "which one" and "where am I", not
                    // part of the map's palette.
                    let keep = o.tint == TINT_SELECT || o.tint == TINT_HOME;
                    // Labels are exempt from the haze and the outline on
                    // purpose: they are ink on the page, not things in it.
                    let mut style =
                        Style::default().fg(theme.paint(o.tint as usize, o.lum, mono, keep));
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
                    let fg = theme.paint(l.tint as usize, fog.factor(l.depth), mono, false);
                    if let Some(cell) = buf.cell_mut((sx, sy)) {
                        cell.set_char(table[(l.mask & 15) as usize]).set_fg(fg);
                    }
                    continue;
                }

                let mut bits = 0u8;
                let mut quad = [0.0f32; 4];
                let mut sum = 0.0f32;
                let mut lit = 0u32;
                let mut near = DEPTH_CLEAR;
                let mut weight = [0.0f32; TINT_NIGHT.len()];
                let mut solid_w = 0.0f32;
                let mut dot_w = 0.0f32;
                let mut hatch_w = 0.0f32;
                let mut shade_w = 0.0f32;
                // Coverage-weighted moments of the lit subpixels, for `MAT_HATCH`
                // to read a direction out of. A braille dot is square on the
                // usual cell, so column and row are already in the same units
                // and no aspect correction belongs here.
                let (mut mx, mut my) = (0.0f32, 0.0f32);
                let (mut mxx, mut myy, mut mxy) = (0.0f32, 0.0f32, 0.0f32);
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
                        match self.mat[i] {
                            MAT_SOLID => solid_w += a,
                            MAT_HATCH => hatch_w += a,
                            MAT_SHADE => shade_w += a,
                            _ => dot_w += a,
                        }
                        let (fx, fy) = (col as f32, row as f32);
                        mx += a * fx;
                        my += a * fy;
                        mxx += a * fx * fx;
                        myy += a * fy * fy;
                        mxy += a * fx * fy;
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
                // The tone this cell's ink is worth, over the whole range the
                // terminal has rather than the top three fifths of it.
                //
                // This used to be `0.40 + 0.60 * mean^0.55` -- a 40% floor, on
                // the reasoning that anything which tripped `DOT_ON` deserved
                // to be seen. The cost of that reasoning was the entire bottom
                // of the ramp: no cell could ever be quiet, so a hillside and
                // the one road crossing it arrived within a few steps of each
                // other and the frame had no depth in it. It is also why
                // dimming a mark did nothing measurable -- halving alpha moved
                // a Himalayan frame by 0.04%, because half of almost-full is
                // still almost-full once the floor is added back.
                //
                // The floor is now just enough that a lit dot is not literally
                // the background, and the gamma is what keeps faint marks
                // legible instead.
                let tone = (INK_FLOOR + (1.0 - INK_FLOOR) * mean.powf(INK_GAMMA))
                    * fog.factor(near);
                let tint = weight
                    .iter()
                    .enumerate()
                    .map(|(i, w)| (i, w * TINT_PULL[i]))
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(i, _)| i)
                    .unwrap_or(0);

                // A cell can only be one glyph, so the two families compete and
                // the heavier contribution wins the cell outright.
                // Four families now, and a cell can only be one glyph, so they
                // compete and the heaviest contribution takes it outright.
                let winner = [
                    (dot_w, MAT_DOT),
                    (solid_w, MAT_SOLID),
                    (hatch_w, MAT_HATCH),
                    (shade_w, MAT_SHADE),
                ]
                .into_iter()
                .fold((0.0f32, MAT_DOT), |a, b| if b.0 > a.0 { b } else { a })
                .1;

                if winner == MAT_HATCH {
                    // The direction of the ink, from the principal axis of the
                    // lit subpixels. Reading it back off the cell rather than
                    // carrying it alongside means any drawer gets this for free
                    // and the glyph can never disagree with the mark.
                    let inv = 1.0 / sum.max(1e-6);
                    let (cx, cy) = (mx * inv, my * inv);
                    let (vxx, vyy, vxy) =
                        (mxx * inv - cx * cx, myy * inv - cy * cy, mxy * inv - cx * cy);
                    // One dot, or a perfectly round blob, has no direction to
                    // report. Braille says "something is here" without claiming
                    // an orientation it cannot see.
                    let spread = vxx + vyy;
                    if spread > 0.05 {
                        // Screen y runs down, so a positive axis angle is a
                        // stroke falling to the right.
                        let theta = 0.5 * (2.0 * vxy).atan2(vxx - vyy);
                        if let Some(cell) = buf.cell_mut((sx, sy)) {
                            let lum = tone;
                            let keep =
                                tint == TINT_SELECT as usize || tint == TINT_HOME as usize;
                            cell.set_char(hatch_of(theta))
                                .set_fg(theme.paint(tint, lum, mono, keep));
                        }
                        continue;
                    }
                }

                if winner == MAT_SHADE {
                    // Fill fraction of the whole cell, not the mean over lit
                    // subpixels: a surface is described by how much of the cell
                    // it covers, which is what a shade block encodes.
                    let fill = sum / (SUB_X * SUB_Y) as f32;
                    let k = ((fill * 4.0) as usize).min(3);
                    if let Some(cell) = buf.cell_mut((sx, sy)) {
                        let lum = tone;
                        let keep = tint == TINT_SELECT as usize || tint == TINT_HOME as usize;
                        cell.set_char(SHADES[k]).set_fg(theme.paint(tint, lum, mono, keep));
                    }
                    continue;
                }

                let ch = if winner == MAT_SOLID {
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
                            .max_by(|a, b| a.1.total_cmp(b.1))
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
                let lum = tone;
                // Same exemption as labels: selection and position answer
                // "which one" and "where am I", so they keep their colour even
                // when the map is deliberately monochrome.
                let keep = tint == TINT_SELECT as usize || tint == TINT_HOME as usize;
                let color = theme.paint(tint, lum, mono, keep);

                if let Some(cell) = buf.cell_mut((sx, sy)) {
                    cell.set_char(ch).set_fg(color);
                }
            }
        }
    }
}

#[cfg(test)]
mod rim_tests {
    use super::*;

    /// Ground that stops gets an edge; ground that goes on does not.
    #[test]
    fn the_rim_marks_where_the_ground_ends() {
        let mut c = Canvas::new(8, 4);
        // A block of surface occupying the left half, at one depth.
        for y in 0..c.sh {
            for x in 0..c.sw / 2 {
                c.occlude_at(x as isize, y as isize, 0.5);
            }
        }
        c.rim(0.03, 1.0);
        // The last covered column is the boundary and gets marked; the middle
        // of the slab is interior and must stay clean, or the outline is not
        // an outline, it is a wash.
        let at = |x: usize, y: usize| c.cov[y * c.sw + x];
        assert!(at(c.sw / 2 - 1, 2) > 0.0, "the edge of the ground was not drawn");
        assert_eq!(at(1, 2), 0.0, "the inside of the ground was drawn as edge");
        assert_eq!(at(c.sw / 2, 2), 0.0, "the empty side was drawn as edge");
    }

    /// A step in the ground is outlined on the near side of the step.
    ///
    /// An outline belongs to the thing in front. Put it on the far side and it
    /// reads as a shadow cast by nothing.
    #[test]
    fn a_step_in_the_ground_is_outlined_on_the_near_side() {
        let mut c = Canvas::new(8, 4);
        for y in 0..c.sh {
            for x in 0..c.sw {
                // Near ground on the left, far ground on the right.
                let z = if x < c.sw / 2 { 0.2 } else { 0.8 };
                c.occlude_at(x as isize, y as isize, z);
            }
        }
        c.rim(0.03, 1.0);
        let at = |x: usize, y: usize| c.cov[y * c.sw + x];
        assert!(at(c.sw / 2 - 1, 2) > 0.0, "the near side of the step is bare");
        assert_eq!(at(c.sw / 2, 2), 0.0, "the far side of the step was outlined");
    }

}

#[cfg(test)]
mod theme_tests {
    use super::*;

    fn lum_of(c: Color) -> f32 {
        let (r, g, b) = rgb_of(c).expect("the renderer emitted a colour with no rgb");
        0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
    }

    /// A mark that fades out has to disappear into the ground it is on.
    ///
    /// The bug this guards is the whole reason `Theme` exists rather than a
    /// swapped palette: every colour here is a strength between 0 and 1, and
    /// the two themes spend it in opposite directions. Spend it the night way
    /// on paper and a mark at zero strength comes out black -- the faintest
    /// possible thing becoming the loudest.
    #[test]
    fn a_mark_at_no_strength_is_the_page_it_is_drawn_on() {
        for theme in [Theme::Night, Theme::Paper] {
            for tint in 0..TINT_NIGHT.len() {
                let gone = lum_of(theme.paint(tint, 0.0, false, false));
                let page = lum_of(theme.page());
                assert!(
                    (gone - page).abs() <= 12.0,
                    "{theme:?} tint {tint} at zero strength is {gone} against a page of {page}"
                );
            }
        }
    }

    /// Strength means "more of the mark" on both grounds, which is more ink on
    /// paper and more light on black -- opposite directions on the same axis.
    #[test]
    fn strength_moves_away_from_the_page_whichever_ground_it_is() {
        for theme in [Theme::Night, Theme::Paper] {
            let page = lum_of(theme.page());
            let mut last = 0.0f32;
            for step in 1..=5 {
                let away = (lum_of(theme.grey(step as f32 / 5.0)) - page).abs();
                assert!(
                    away >= last,
                    "{theme:?} at {step}/5 is {away} from the page, less than {last} before it"
                );
                last = away;
            }
            assert!(last > 60.0, "{theme:?} never gets far from its page: {last}");
        }
    }

    /// The loudest road is the same road on both grounds.
    ///
    /// The paper table is not the night table inverted -- inverting hue turns
    /// water orange -- so the ordering has to be asserted rather than assumed.
    /// A motorway is the brightest thing on black and has to be the darkest on
    /// paper, because on both the answer to "which line is shouting" is the
    /// same line.
    #[test]
    fn the_motorway_is_the_loudest_line_on_either_ground() {
        for theme in [Theme::Night, Theme::Paper] {
            let page = lum_of(theme.page());
            let contrast = |t: u8| (lum_of(theme.paint(t as usize, 1.0, false, false)) - page).abs();
            let road = contrast(TINT_MAJOR);
            for quieter in [TINT_MEDIUM, TINT_MINOR, TINT_GREEN, TINT_WATER] {
                assert!(
                    road > contrast(quieter),
                    "{theme:?}: tint {quieter} argues louder than the motorway"
                );
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_tints_keep_the_index_exact_ramp_and_hues_get_the_finer_one() {
        let l = 0.52;
        assert_eq!(quantise_for(l, 232, 232, 226), quantise(l));
        assert_eq!(quantise_for(l, 140, 186, 140), quantise_hued(l));
        assert_ne!(quantise(l), quantise_hued(l));
    }
}

#[cfg(test)]
mod brush_tests {
    use super::*;
    use std::f32::consts::PI;

    /// The hatch glyph has to look like the direction it is reporting.
    ///
    /// That is the whole reason this family exists: braille can put a mark
    /// anywhere in a cell but every mark looks the same, so a stroke saying
    /// "the ground falls this way" has to be inferred from where the dots sit.
    /// A line glyph says it outright.
    #[test]
    fn a_hatch_glyph_runs_the_way_the_ink_does() {
        // Screen y is down, so a positive angle falls to the right.
        assert_eq!(hatch_of(0.0), '\u{2500}');
        assert_eq!(hatch_of(PI / 4.0), '\u{2572}');
        assert_eq!(hatch_of(-PI / 4.0), '\u{2571}');
        assert_eq!(hatch_of(PI / 2.0), '\u{2502}');
        assert_eq!(hatch_of(-PI / 2.0), '\u{2502}');
    }

    /// An axis is undirected: a stroke and its reverse are the same line, so
    /// the two must never pick different glyphs.
    #[test]
    fn a_stroke_and_its_reverse_draw_the_same_glyph() {
        let mut t = -PI / 2.0;
        while t <= PI / 2.0 {
            let flipped = if t > 0.0 { t - PI } else { t + PI };
            assert_eq!(
                hatch_of(t),
                hatch_of(flipped),
                "{t} and {flipped} disagree"
            );
            t += 0.05;
        }
    }

    /// Every direction gets an answer, and all four glyphs are reachable.
    #[test]
    fn the_half_turn_is_covered_by_all_four() {
        let mut seen = std::collections::HashSet::new();
        let mut t = -PI / 2.0;
        while t <= PI / 2.0 {
            seen.insert(hatch_of(t));
            t += 0.01;
        }
        for g in HATCH {
            assert!(seen.contains(&g), "{g} is unreachable");
        }
    }
}

#[cfg(test)]
mod nan_tests {
    use super::*;

    fn brush() -> Brush {
        Brush { depth: 0.5, tint: TINT_GREEN, mat: MAT_DOT, pick: u32::MAX, behind: Behind::Ignore }
    }

    /// A NaN position must not reach the buffer.
    ///
    /// The reason this is worth a guard rather than a comment: `NaN as isize`
    /// saturates to zero, so the write lands on a real subpixel instead of
    /// being caught by the bounds check, and it stores NaN coverage. The frame
    /// still draws. The renderer then panics later and somewhere else -- in the
    /// resolve pass, comparing tint weights -- with nothing left to say which
    /// draw call put it there.
    #[test]
    fn a_mark_at_no_position_is_not_drawn_at_the_origin() {
        for (x, y, a) in [
            (f64::NAN, 4.0, 1.0f32),
            (4.0, f64::NAN, 1.0),
            (f64::INFINITY, 4.0, 1.0),
            (4.0, 4.0, f32::NAN),
        ] {
            let mut c = Canvas::new(8, 4);
            c.splat(x, y, a, &brush());
            assert!(
                c.cov.iter().all(|v| *v == 0.0),
                "({x}, {y}, {a}) put something in the buffer"
            );
        }
        // And an ordinary mark still lands, so the guard is not simply off.
        let mut c = Canvas::new(8, 4);
        c.splat(4.0, 4.0, 1.0, &brush());
        assert!(c.cov.iter().any(|v| *v > 0.0), "the guard rejected a real mark");
    }

    /// And if one ever does get in, the frame must still draw.
    ///
    /// `partial_cmp(..).unwrap()` on tint weights was the crash site. It is a
    /// total order now, so a bad value upstream costs one wrong-coloured cell
    /// instead of the whole program.
    #[test]
    fn a_nan_already_in_the_buffer_does_not_bring_the_frame_down() {
        let mut c = Canvas::new(8, 4);
        c.splat(4.0, 4.0, 1.0, &brush());
        // Straight past `splat`, the way a NaN would have arrived before it
        // was guarded.
        c.cov[2 * c.sw + 2] = f32::NAN;
        c.cov[2 * c.sw + 3] = 0.9;
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 4));
        let fog = Fog { near: 1.0, far: 0.3, gamma: 1.0 };
        c.resolve(&mut buf, Rect::new(0, 0, 8, 4), &fog, false, Theme::Night);
    }
}

#[cfg(test)]
mod veil_tests {
    use super::*;

    fn at(behind: Behind, depth: f32) -> Brush {
        Brush { depth, tint: TINT_GREEN, mat: MAT_DOT, pick: u32::MAX, behind }
    }

    fn ink(c: &Canvas, x: usize, y: usize) -> f32 {
        c.cov[y * c.sw + x]
    }

    /// One step of opacity per layer of ground in front, and gone after four.
    ///
    /// The middle ground between hiding a mark behind a ridge, which leaves a
    /// hole in a contour, and ignoring the ridge, which is what gave a wall of
    /// iso-lines with no front or back to it.
    #[test]
    fn each_layer_in_front_takes_a_step_of_opacity() {
        let near = 0.10;
        let mut seen = Vec::new();
        for layers in 0..6 {
            let mut c = Canvas::new(4, 2);
            // Something up front, but faint -- coverage is alpha-over, so an
            // occluder at full strength saturates the subpixel and there is no
            // headroom left to read the dimming in.
            c.plot(2, 2, 0.2, &at(Behind::Hide, near));
            // Then a mark that many layers behind it.
            c.plot(2, 2, 1.0, &at(Behind::Veil, near + VEIL_STEP * layers as f32 + 0.001));
            seen.push(ink(&c, 2, 2));
        }
        // The front layer is undimmed; each one behind is fainter than the last.
        for w in seen.windows(2) {
            assert!(w[1] <= w[0], "a layer further back came out brighter: {seen:?}");
        }
        assert!(seen[0] > seen[1], "the first layer back was not dimmed at all");
        assert_eq!(
            seen[VEIL_LIMIT as usize + 1],
            seen[VEIL_LIMIT as usize],
            "past the limit there should be nothing left to remove"
        );
    }

    /// In front of everything, a veiled mark is at full strength -- it is not a
    /// fade, it is an answer to "what is between me and this".
    #[test]
    fn a_mark_in_front_is_not_dimmed() {
        let mut c = Canvas::new(4, 2);
        c.plot(2, 2, 1.0, &at(Behind::Hide, 0.60));
        c.plot(2, 2, 1.0, &at(Behind::Veil, 0.10));
        let veiled = ink(&c, 2, 2);

        let mut clean = Canvas::new(4, 2);
        clean.plot(2, 2, 1.0, &at(Behind::Ignore, 0.10));
        assert!((veiled - ink(&clean, 2, 2)).abs() < 1e-6);
    }

    /// And the other two behaviours are unchanged, because everything else in
    /// the renderer still uses them.
    #[test]
    fn hide_still_hides_and_ignore_still_ignores() {
        let mut c = Canvas::new(4, 2);
        c.plot(2, 2, 1.0, &at(Behind::Hide, 0.10));
        let front_only = ink(&c, 2, 2);
        c.plot(2, 2, 1.0, &at(Behind::Hide, 0.90));
        assert_eq!(ink(&c, 2, 2), front_only, "something behind was allowed through");

        let mut c = Canvas::new(4, 2);
        c.plot(2, 2, 0.5, &at(Behind::Hide, 0.10));
        let before = ink(&c, 2, 2);
        c.plot(2, 2, 0.5, &at(Behind::Ignore, 0.90));
        assert!(ink(&c, 2, 2) > before, "Ignore should draw regardless of depth");
    }
}

#[cfg(test)]
mod tint_tests {
    use super::*;

    fn brush(tint: u8) -> Brush {
        Brush { depth: 0.5, tint, mat: MAT_DOT, pick: u32::MAX, behind: Behind::Ignore }
    }

    fn colour_of(cell_paint: impl Fn(&mut Canvas)) -> Color {
        let mut c = Canvas::new(2, 1);
        cell_paint(&mut c);
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        c.resolve(&mut buf, Rect::new(0, 0, 2, 1), &Fog { near: 1.0, far: 1.0, gamma: 1.0 }, false, Theme::Night);
        buf.cell((0, 0)).unwrap().fg
    }

    /// A road crossing a hillside must come out the colour of a road.
    ///
    /// The braille glyph already merges every dot in the cell whatever drew it,
    /// so the road's dots were always on the screen. What it lost was the
    /// colour: the tints voted by coverage alone and a hillside outvotes a road
    /// several times over, so the whole cell came out terrain green and the
    /// road was invisible while every count said it was drawn.
    #[test]
    fn a_road_over_a_hillside_is_still_coloured_a_road() {
        // Hue, not the exact colour: coverage changes the brightness whatever
        // wins the vote, so comparing colours outright passes for the wrong
        // reason. The first version of this test did exactly that and went on
        // passing with the weighting taken out.
        let hue = |c: Color| match c {
            Color::Rgb(r, g, b) => {
                let m = r.max(g).max(b).max(1) as f32;
                ((r as f32 / m * 8.0).round() as u8,
                 (g as f32 / m * 8.0).round() as u8,
                 (b as f32 / m * 8.0).round() as u8)
            }
            other => panic!("expected a hue, got {other:?}"),
        };
        let ground = hue(colour_of(|c| {
            for y in 0..4 {
                for x in 0..2 {
                    c.plot(x, y, 0.5, &brush(TINT_GREEN));
                }
            }
        }));
        let road = hue(colour_of(|c| {
            c.plot(0, 1, 0.9, &brush(TINT_MAJOR));
            c.plot(1, 1, 0.9, &brush(TINT_MAJOR));
        }));
        assert_ne!(ground, road, "the two tints are not distinguishable to begin with");

        // A hillside with a road through it: outnumbered three to one on
        // coverage, and it still has to be the road you see.
        let crossed = hue(colour_of(|c| {
            for y in 0..4 {
                for x in 0..2 {
                    c.plot(x, y, 0.5, &brush(TINT_GREEN));
                }
            }
            c.plot(0, 1, 0.9, &brush(TINT_MAJOR));
            c.plot(1, 1, 0.9, &brush(TINT_MAJOR));
        }));
        assert_eq!(crossed, road, "the road was drowned out by the hillside");
    }

    /// And ground still reads as ground when it is the only thing there, so the
    /// weighting is a tie-break and not a thumb on every scale.
    #[test]
    fn ground_on_its_own_is_still_ground() {
        let alone = colour_of(|c| {
            for y in 0..4 {
                for x in 0..2 {
                    c.plot(x, y, 0.6, &brush(TINT_GREEN));
                }
            }
        });
        let reference = colour_of(|c| c.plot(0, 0, 0.6, &brush(TINT_GREEN)));
        // Same hue either way; brightness differs with coverage, which is fine.
        let hue = |c: Color| match c {
            Color::Rgb(r, g, b) => {
                let m = r.max(g).max(b).max(1) as f32;
                Some(((r as f32 / m * 8.0) as u8, (g as f32 / m * 8.0) as u8, (b as f32 / m * 8.0) as u8))
            }
            _ => None,
        };
        assert_eq!(hue(alone), hue(reference), "ground changed colour on its own");
    }

    /// Every tint has a weight, or the lookup would panic on the one that does
    /// not -- and it would be whichever was added last.
    #[test]
    fn every_tint_has_a_pull() {
        assert_eq!(TINT_PULL.len(), TINT_NIGHT.len());
        assert_eq!(TINT_PAPER.len(), TINT_NIGHT.len());
        for (i, p) in TINT_PULL.iter().enumerate() {
            assert!(*p > 0.0, "tint {i} would never win a cell it was alone in");
        }
    }
}
