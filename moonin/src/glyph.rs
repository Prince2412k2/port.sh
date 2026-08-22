//! Picking a glyph for one cell's worth of coverage.
//!
//! Every family the terminal offers is in play — solid, eighths, halves,
//! quadrants, shades, braille — and they are chosen by *matching*, not by rules.
//! Each glyph gets a template of which of the cell's 64 samples it fills; the
//! winner is whichever template differs least from what the field actually
//! covered. Adding a glyph is adding a template, and there is no branchy
//! classifier to keep consistent with itself.
//!
//! The families are not interchangeable, and the asymmetry is worth knowing:
//! there are eight left-anchored eighths and eight bottom-anchored ones, but no
//! right or top equivalents outside the halves. So a left or bottom edge gets
//! eight levels of precision and a right or top edge gets two. Braille covers
//! the rest, which is the one thing it is genuinely good at.
//!
//! Shades are templates too, and that is what makes them fall out correctly: a
//! cell whose coverage is spread evenly rather than pressed against one side
//! matches a lattice better than any band, so diffuse edges become `░▒▓` on
//! their own without being special-cased.

/// Samples across and down one cell. Eight by eight, so the eighths families
/// land on exact boundaries.
pub const SX: usize = 8;
pub const SY: usize = 8;

/// One template: bit `y * SX + x` set means that sample is inside.
struct Tile {
    glyph: char,
    mask: u64,
}

/// Above this many mismatched samples out of 64, no solid glyph is describing
/// the cell well enough and braille takes over.
const GIVE_UP: u32 = 7;

/// Fewer lit samples than this and the cell is left empty.
///
/// Without a floor, a boundary that clips one corner of a cell finds a genuinely
/// good match in a one-eighth bar or a lone braille dot, and the outline grows a
/// fringe of `▏` and `⠁` that reads as glitching rather than as an edge. The
/// mismatch score cannot catch this on its own — those matches are *correct*,
/// they are just not worth drawing.
const MIN_INK: u32 = 6;

fn mask_of(f: impl Fn(usize, usize) -> bool) -> u64 {
    let mut bits = 0u64;
    for y in 0..SY {
        for x in 0..SX {
            if f(x, y) {
                bits |= 1 << (y * SX + x);
            }
        }
    }
    bits
}

fn atlas() -> Vec<Tile> {
    let mut tiles = vec![Tile { glyph: '\u{2588}', mask: !0 }];

    // Left eighths. ▌ is the four-eighths case and needs no separate entry.
    for (k, glyph) in "\u{258f}\u{258e}\u{258d}\u{258c}\u{258b}\u{258a}\u{2589}"
        .chars()
        .enumerate()
    {
        let k = k + 1;
        tiles.push(Tile { glyph, mask: mask_of(|x, _| x < k) });
    }
    // Bottom eighths. ▄ is the four-eighths case.
    for (k, glyph) in "\u{2581}\u{2582}\u{2583}\u{2584}\u{2585}\u{2586}\u{2587}"
        .chars()
        .enumerate()
    {
        let k = k + 1;
        tiles.push(Tile { glyph, mask: mask_of(|_, y| y >= SY - k) });
    }
    // The two halves with no eighths family behind them.
    tiles.push(Tile { glyph: '\u{2580}', mask: mask_of(|_, y| y < SY / 2) });
    tiles.push(Tile { glyph: '\u{2590}', mask: mask_of(|x, _| x >= SX / 2) });

    // Quadrants, named by which corners they fill.
    let half = (SX / 2, SY / 2);
    let ul = |x: usize, y: usize| x < half.0 && y < half.1;
    let ur = |x: usize, y: usize| x >= half.0 && y < half.1;
    let ll = |x: usize, y: usize| x < half.0 && y >= half.1;
    let lr = |x: usize, y: usize| x >= half.0 && y >= half.1;
    for (glyph, f) in [
        ('\u{2598}', Box::new(ul) as Box<dyn Fn(usize, usize) -> bool>),
        ('\u{259d}', Box::new(ur)),
        ('\u{2596}', Box::new(ll)),
        ('\u{2597}', Box::new(lr)),
        ('\u{259a}', Box::new(|x, y| ul(x, y) || lr(x, y))),
        ('\u{259e}', Box::new(|x, y| ur(x, y) || ll(x, y))),
        ('\u{2599}', Box::new(|x, y| !ur(x, y))),
        ('\u{259b}', Box::new(|x, y| !lr(x, y))),
        ('\u{259c}', Box::new(|x, y| !ll(x, y))),
        ('\u{259f}', Box::new(|x, y| !ul(x, y))),
    ] {
        tiles.push(Tile { glyph, mask: mask_of(|x, y| f(x, y)) });
    }

    // Shades, as the lattices they actually look like.
    tiles.push(Tile {
        glyph: '\u{2591}',
        mask: mask_of(|x, y| x % 2 == 0 && y % 2 == 0),
    });
    tiles.push(Tile {
        glyph: '\u{2592}',
        mask: mask_of(|x, y| (x + y) % 2 == 0),
    });
    tiles.push(Tile {
        glyph: '\u{2593}',
        mask: mask_of(|x, y| !(x % 2 == 0 && y % 2 == 0)),
    });

    tiles
}

pub struct Atlas {
    tiles: Vec<Tile>,
}

impl Atlas {
    pub fn new() -> Atlas {
        Atlas { tiles: atlas() }
    }

    /// The glyph that best describes this cell, or `None` for an empty one.
    pub fn pick(&self, cover: u64) -> Option<char> {
        if cover.count_ones() < MIN_INK {
            return None;
        }
        let mut best = ('\u{2588}', u32::MAX);
        for tile in &self.tiles {
            let miss = (tile.mask ^ cover).count_ones();
            if miss < best.1 {
                best = (tile.glyph, miss);
            }
        }
        if best.1 <= GIVE_UP {
            return Some(best.0);
        }
        Some(braille(cover))
    }
}

/// Falls back to braille, which can hold any two-by-four pattern and so can
/// describe shapes no solid glyph has. Each dot takes the majority of the four
/// by two samples beneath it.
fn braille(cover: u64) -> char {
    const DOTS: [[u8; 2]; 4] = [
        [0x01, 0x08],
        [0x02, 0x10],
        [0x04, 0x20],
        [0x40, 0x80],
    ];
    let mut bits = 0u8;
    for (r, row) in DOTS.iter().enumerate() {
        for (c, &dot) in row.iter().enumerate() {
            let mut lit = 0;
            for dy in 0..SY / 4 {
                for dx in 0..SX / 2 {
                    let (x, y) = (c * (SX / 2) + dx, r * (SY / 4) + dy);
                    if cover >> (y * SX + x) & 1 == 1 {
                        lit += 1;
                    }
                }
            }
            if lit * 2 >= (SX / 2) * (SY / 4) {
                bits |= dot;
            }
        }
    }
    char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
}
