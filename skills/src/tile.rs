//! Drawing one tool mark into the terminal buffer.
//!
//! The marks arrive from `logos.rs` as a glyph plus two *coverages* rather than
//! two colours — see `scripts/logos.py`. That indirection is what this module
//! exists to spend: a tile can be dimmed because its project does not use it,
//! or lit because it is rising under the cursor, by scaling those coverages on
//! the way to a colour. Nothing is regenerated and nothing is blended twice.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::logos::Logo;

/// The ground the marks are composited against.
pub const BG: (u8, u8, u8) = (6, 7, 10);

#[inline]
fn mix(c: (u8, u8, u8), a: f32) -> Color {
    let a = a.clamp(0.0, 1.0);
    Color::Rgb(
        (BG.0 as f32 + (c.0 as f32 - BG.0 as f32) * a) as u8,
        (BG.1 as f32 + (c.1 as f32 - BG.1 as f32) * a) as u8,
        (BG.2 as f32 + (c.2 as f32 - BG.2 as f32) * a) as u8,
    )
}

/// Paint a mark with its top-left at `(x, y)` in buffer coordinates.
///
/// `light` scales every coverage in the mark, so 0 leaves the ground untouched
/// and 1 is the mark at full strength. Cells that carry no ink at all are
/// skipped rather than painted with the background, which is what lets a tile
/// overlap whatever is behind it without stamping a rectangle over it.
pub fn draw(
    buf: &mut Buffer,
    clip: Rect,
    x: i32,
    y: i32,
    logo: &Logo,
    small: bool,
    light: f32,
) {
    if light <= 0.01 {
        return;
    }
    let art = logo.art(small);
    for r in 0..art.rows as i32 {
        let sy = y + r;
        if sy < clip.y as i32 || sy >= (clip.y + clip.height) as i32 {
            continue;
        }
        for c in 0..art.cols as i32 {
            let sx = x + c;
            if sx < clip.x as i32 || sx >= (clip.x + clip.width) as i32 {
                continue;
            }
            let (ch, f, b) = art.cells[(r * art.cols as i32 + c) as usize];
            if f == 0 && b == 0 {
                continue;
            }
            let (fa, ba) = (f as f32 / 255.0 * light, b as f32 / 255.0 * light);
            if let Some(cell) = buf.cell_mut((sx as u16, sy as u16)) {
                cell.set_char(ch)
                    .set_fg(mix(logo.rgb, fa))
                    .set_bg(mix(logo.rgb, ba));
            }
        }
    }
}

/// A mark's footprint in cells.
pub fn size(logo: &Logo, small: bool) -> (u16, u16) {
    let a = logo.art(small);
    (a.cols, a.rows)
}

/// Centred text under a tile, in the mark's own colour.
pub fn caption(buf: &mut Buffer, clip: Rect, cx: i32, y: i32, text: &str, c: (u8, u8, u8), light: f32) {
    if light <= 0.01 || y < clip.y as i32 || y >= (clip.y + clip.height) as i32 {
        return;
    }
    let n = text.chars().count() as i32;
    let x0 = cx - n / 2;
    let style = Style::default()
        .fg(mix(c, light))
        .add_modifier(if light > 0.85 { Modifier::BOLD } else { Modifier::empty() });
    for (i, ch) in text.chars().enumerate() {
        let sx = x0 + i as i32;
        if sx < clip.x as i32 || sx >= (clip.x + clip.width) as i32 {
            continue;
        }
        if let Some(cell) = buf.cell_mut((sx as u16, y as u16)) {
            cell.set_char(ch).set_style(style);
        }
    }
}
