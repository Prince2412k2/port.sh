//! Subpixel canvas. Eight by eight samples per cell, collapsed to one glyph by
//! the atlas.
//!
//! Eight by eight because that is what the eighths families need to land on
//! exact boundaries. The samples are not square — a cell is twice as tall as it
//! is wide — which is why the caller supersamples vertically when filling them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::glyph::{Atlas, SX, SY};

pub const SUB_X: i32 = SX as i32;
pub const SUB_Y: i32 = SY as i32;

/// One drawing layer, at subpixel resolution.
pub struct Layer {
    w: i32,
    h: i32,
    on: Vec<bool>,
}

impl Layer {
    pub fn new(w: i32, h: i32) -> Layer {
        Layer { w, h, on: vec![false; (w * h).max(0) as usize] }
    }

    pub fn size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    pub fn clear(&mut self) {
        self.on.fill(false);
    }

    pub fn plot(&mut self, x: i32, y: i32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return;
        }
        self.on[(y * self.w + x) as usize] = true;
    }

    /// Collapse to glyphs. Empty cells are skipped, so this composites over a
    /// finished frame without erasing anything behind it.
    pub fn resolve(
        &self,
        buf: &mut Buffer,
        area: Rect,
        at: (i32, i32),
        atlas: &Atlas,
        colour: Color,
    ) {
        for row in 0..self.h / SUB_Y {
            for col in 0..self.w / SUB_X {
                let mut cover = 0u64;
                for y in 0..SY {
                    for x in 0..SX {
                        let (sx, sy) = (col * SUB_X + x as i32, row * SUB_Y + y as i32);
                        if self.on[(sy * self.w + sx) as usize] {
                            cover |= 1 << (y * SX + x);
                        }
                    }
                }
                let Some(glyph) = atlas.pick(cover) else { continue };
                let (cx, cy) = (at.0 + col, at.1 + row);
                if cx < area.x as i32
                    || cy < area.y as i32
                    || cx >= area.right() as i32
                    || cy >= area.bottom() as i32
                {
                    continue;
                }
                if let Some(cell) = buf.cell_mut((cx as u16, cy as u16)) {
                    cell.set_char(glyph).set_fg(colour);
                }
            }
        }
    }
}
