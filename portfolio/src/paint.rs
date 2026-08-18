//! The shared palette, and compositing over a finished frame.
//!
//! Both embedded renderers already agree on these colours; naming them once
//! here is what stops the shell drifting away from the two things it frames.

use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Frame;

pub const BG: Color = Color::Rgb(8, 9, 11);
pub const FG: Color = Color::Rgb(196, 200, 206);
pub const DIM: Color = Color::Rgb(96, 102, 112);
pub const FAINT: Color = Color::Rgb(58, 62, 70);
pub const ACCENT: Color = Color::Rgb(255, 176, 64);
pub const CYAN: Color = Color::Rgb(110, 224, 255);

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
