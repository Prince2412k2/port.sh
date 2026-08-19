//! The shared palette, and compositing over a finished frame.
//!
//! Both embedded renderers already agree on these colours; naming them once
//! here is what stops the shell drifting away from the two things it frames.

use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Frame;

use crate::portraits;

pub const BG: Color = Color::Rgb(8, 9, 11);
pub const FG: Color = Color::Rgb(196, 200, 206);
pub const DIM: Color = Color::Rgb(96, 102, 112);
pub const FAINT: Color = Color::Rgb(58, 62, 70);
pub const ACCENT: Color = Color::Rgb(255, 176, 64);
pub const CYAN: Color = Color::Rgb(110, 224, 255);

/// How fast a baked animation plays, in frames a second.
///
/// Deliberately slow. These are a dozen frames sampled out of a loop, not a
/// smooth one, and every frame that changes is cells crossing a network to a
/// visitor who is reading rather than watching.
pub const PORTRAIT_FPS: f64 = 8.0;

/// How long a baked animation runs before it settles, in seconds.
pub fn portrait_secs(p: &portraits::Portrait) -> f64 {
    p.frames.len() as f64 / PORTRAIT_FPS
}

/// Which frame of a baked animation is showing at `t` seconds.
///
/// Plays once and then holds the last frame -- it does not loop. That is a
/// bandwidth decision, not a taste one, and the numbers are lopsided enough
/// to settle it: chafa picks glyphs and colours for each frame independently,
/// so two visually similar frames share almost no cells, and all 408 of the
/// home portrait's change on every step. Looped at 8 fps that is **126 KB/s**
/// on the landing page, for ever, measured over a real WebSocket -- against
/// the 0.3 KB/s an idle screen costs otherwise. Played once it is a quarter of
/// a megabyte on arrival and nothing after.
///
/// So the portrait moves while somebody is arriving and reading the first
/// line, and is a still by the time they are done.
pub fn portrait_frame(p: &portraits::Portrait, t: f64) -> &'static [portraits::Cell] {
    let n = p.frames.len();
    if n <= 1 {
        return p.frames[0];
    }
    p.frames[((t * PORTRAIT_FPS) as usize).min(n - 1)]
}

/// Blit one baked plate at `x`,`y`, clipped to `area`.
///
/// Unlike the emblems these carry real colour and are drawn as they were
/// baked; there is no tint to apply. Cells chafa left as pure background are
/// skipped rather than painted, so a plate sits on the page instead of on a
/// rectangle of its own.
pub fn portrait(f: &mut Frame, area: Rect, x: u16, y: u16, cells: &[portraits::Cell], cols: u16) {
    let Color::Rgb(kr, kg, kb) = BG else { return };
    for (i, &(ch, fr, fg, fb, br, bg, bb)) in cells.iter().enumerate() {
        let (c, r) = (i as u16 % cols, i as u16 / cols);
        let (px, py) = (x + c, y + r);
        if px >= area.x + area.width || py >= area.y + area.height {
            continue;
        }
        // chafa spells "nothing here" as a space or as braille blank,
        // depending on which symbol class won the cell. Either one over our
        // own ground is a cell to leave alone, so the plate sits on the page
        // rather than on a rectangle of its own.
        if (ch == ' ' || ch == '\u{2800}') && (br, bg, bb) == (kr, kg, kb) {
            continue;
        }
        if let Some(cell) = f.buffer_mut().cell_mut((px, py)) {
            cell.set_char(ch)
                .set_fg(Color::Rgb(fr, fg, fb))
                .set_bg(Color::Rgb(br, bg, bb));
        }
    }
}

/// Blend a colour toward the background. `k` of 1 leaves it alone, 0 removes it.
///
/// Goes through `termap::canvas::rgb_of` rather than matching `Color::Rgb`,
/// because the map emits palette indices for neutral cells and a match on the
/// truecolor variant alone silently skips most of the screen. That exact bug
/// cost real time once already.
pub fn toward_bg(c: Color, k: f32) -> Color {
    let Some((r, g, b)) = termap::canvas::rgb_of(c) else { return c };
    let Color::Rgb(br, bg, bb) = BG else { return c };
    let mix = |a: u8, t: u8| (t as f32 + (a as f32 - t as f32) * k).round().clamp(0.0, 255.0) as u8;
    termap::canvas::ink(mix(r, br), mix(g, bg), mix(b, bb))
}

/// Fade a colour to nothing. The inverse reading of `toward_bg`, named for the
/// way it is used: things arriving and leaving rather than being dimmed.
pub fn dim_to(c: Color, alpha: f32) -> Color {
    toward_bg(c, alpha.clamp(0.0, 1.0))
}

/// Dissolve a whole region toward the background.
///
/// Used for the section transition. Compositing the finished frame rather than
/// asking each section to render itself at an opacity means the shell can fade
/// anything it can draw, including two renderers that know nothing about it.
pub fn veil(f: &mut Frame, area: Rect, k: f32) {
    if k >= 0.999 {
        return;
    }
    let buf = f.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            let (fg, bg) = (toward_bg(cell.fg, k), toward_bg(cell.bg, k));
            cell.set_fg(fg).set_bg(bg);
        }
    }
}

/// Smootherstep, the easing used everywhere else in this project.
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Greedy word wrap, no mid-word breaks.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// portraits.rs is written by a Python script, so nothing but this checks
    /// that what it emitted is the shape the blitter indexes into. A frame
    /// short of `cols * rows` would draw a plate that slides diagonally.
    #[test]
    fn every_baked_frame_is_exactly_its_declared_grid() {
        for p in portraits::PORTRAITS.iter() {
            assert!(p.cols > 0 && p.rows > 0, "{} is empty", p.id);
            assert!(!p.frames.is_empty(), "{} has no frames", p.id);
            for (i, f) in p.frames.iter().enumerate() {
                assert_eq!(
                    f.len(),
                    p.cols as usize * p.rows as usize,
                    "{} frame {i} is {} cells, want {}x{}",
                    p.id, f.len(), p.cols, p.rows
                );
            }
        }
    }

    /// The frame index has to stay inside the array for any time at all,
    /// including the moment a session has been open for hours.
    #[test]
    fn the_animation_clock_wraps_instead_of_running_off_the_end() {
        for p in portraits::PORTRAITS.iter() {
            for t in [0.0, 0.5, 3.0, 1e4, 8.64e4] {
                let f = portrait_frame(p, t);
                assert_eq!(f.len(), p.cols as usize * p.rows as usize, "{} at {t}s", p.id);
            }
        }
    }
}
