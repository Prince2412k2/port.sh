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

/// Which of the two the chat leans on, flipped by `/theme`.
///
/// Both colours already exist and are already used together; this only decides
/// which one leads. A whole second palette would be a different design, not a
/// setting -- and the rest of the app keeps its own colours either way, because
/// the map's water is not a matter of taste.
static WARM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Flip it, and report where it landed.
pub fn flip_theme() -> bool {
    !WARM.fetch_xor(true, std::sync::atomic::Ordering::Relaxed)
}

/// The colour the chat leads with.
pub fn lead() -> Color {
    if WARM.load(std::sync::atomic::Ordering::Relaxed) {
        ACCENT
    } else {
        CYAN
    }
}

/// How fast a baked animation plays, in frames a second.
///
/// Deliberately slow, and the rate is a bandwidth setting as much as a
/// timing one. Every step repaints about half the cells of a large plate --
/// ~11 KB for the home portrait -- so 8 fps and 6 fps differ by 25% of the
/// whole page's cost. The footage is a slow pan in both cases and does not
/// read as choppier for it.
pub const PORTRAIT_FPS: f64 = 6.0;

/// The longest a plate ever loops for after somebody arrives at it.
///
/// Bounded motion is the rule this project has broken and re-measured twice,
/// so the thing held constant is not the duration but what one arrival costs
/// to download. `lively_for` divides this budget by the size of the plate:
/// nine seconds is what the middle tier gets, and it is the ceiling rather
/// than the setting.
pub const LIVELY: f64 = 9.0;

/// Short of this a loop is over before it has registered as motion, so a plate
/// too large to afford its share of the budget gets this much anyway and costs
/// what it costs. At 6 fps it is eighteen frames: one whole pass.
const LIVELY_MIN: f64 = 3.0;

/// The plate the budget was set against, in cells: 64x24 for `LIVELY` seconds.
///
/// Measured over a real WebSocket at 176x44, that arrival is ~130 KB/s while
/// the loop runs and 1.2 MB by the time it settles. The same walk onto the
/// 104x40 bake at 200x56 runs at ~285 KB/s and stops after three seconds:
/// 1.0 MB, which is 0.84x the cost of the smaller picture for 2.7x the cells.
/// Both go quiet at 0.2 KB/s afterwards. That is the whole point of deriving
/// the duration rather than fixing it — turning up with a big window buys a
/// bigger picture and not a longer download.
const LIVELY_CELLS: f64 = 64.0 * 24.0;

/// How long this plate should keep looping after somebody arrives at it.
///
/// Zero for a photograph: there is nothing to loop, and repainting a still
/// image at six frames a second is bandwidth spent to show somebody exactly
/// what they are already looking at.
///
/// Otherwise the budget above, divided by how much of the screen the plate
/// covers. A cell costs roughly the same wherever it is, so a plate with three
/// times the cells costs three times as much a second and gets a third as long
/// — which keeps the megabyte-per-arrival figure roughly flat across the tiers
/// instead of letting it scale with whatever size terminal somebody turned up
/// with.
pub fn lively_for(p: &portraits::Portrait) -> f64 {
    if p.frames.len() <= 1 {
        return 0.0;
    }
    let cells = p.cols as f64 * p.rows as f64;
    (LIVELY * LIVELY_CELLS / cells.max(1.0)).clamp(LIVELY_MIN, LIVELY)
}

/// Which frame is showing at `t`, looping while `alive` and holding after.
///
/// The museum wants a loop for as long as somebody has just arrived at a work
/// and then silence, which is neither "play once" nor "loop for ever". Holding
/// the frame it happened to be on would stop mid-gesture, so it settles on the
/// last one — the same place `portrait_frame` ends up.
pub fn portrait_loop(p: &portraits::Portrait, t: f64, alive: bool) -> &'static [portraits::Cell] {
    let n = p.frames.len();
    if n <= 1 {
        return p.frames[0];
    }
    if !alive {
        return p.frames[n - 1];
    }
    p.frames[((t * PORTRAIT_FPS) as usize) % n]
}

/// Remap everything already drawn in a region onto one colour.
///
/// The subpixel canvas draws in greys because coverage is all it knows; this
/// turns that into a hue afterwards, so one field can serve eight works
/// instead of needing a palette slot each. Cells the region never drew into
/// are left alone — the test is the page's own ground, not brightness, or the
/// darkest parts of the field would be indistinguishable from empty.
pub fn recolour(f: &mut Frame, area: Rect, rgb: (u8, u8, u8), k: f32) {
    let Color::Rgb(kr, kg, kb) = BG else { return };
    let buf = f.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            let (fg, bg) = (cell.fg, cell.bg);
            let map = |c: Color| -> Color {
                let Some((r, g, b)) = termap::canvas::rgb_of(c) else { return c };
                if (r, g, b) == (kr, kg, kb) {
                    return c;
                }
                // Rec. 601 luma recovers "how much ink is in this cell"
                // before the colour is put back on top of it.
                let l = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0;
                let mix = |t: u8, base: u8| {
                    (base as f32 + (t as f32 * l - base as f32) * k).clamp(0.0, 255.0) as u8
                };
                termap::canvas::ink(mix(rgb.0, kr), mix(rgb.1, kg), mix(rgb.2, kb))
            };
            cell.set_fg(map(fg)).set_bg(map(bg));
        }
    }
}

/// Blit one baked plate at `x`,`y`, clipped to `area`.
///
/// Unlike the emblems these carry real colour and are drawn as they were
/// baked; there is no tint to apply. Cells chafa left as pure background are
/// skipped rather than painted, so a plate sits on the page instead of on a
/// rectangle of its own.
pub fn ink(i: portraits::Ink) -> Color {
    match i {
        portraits::Ink::I(n) => Color::Indexed(n),
        portraits::Ink::C(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Whether a cell is the page's own ground rather than part of the picture.
///
/// chafa spells "nothing here" as a space or as braille blank, depending on
/// which symbol class won the cell. Either one over our own background is a
/// cell to leave alone, so a plate sits on the page rather than on a rectangle
/// of its own.
fn is_ground(ch: char, bg: portraits::Ink) -> bool {
    if ch != ' ' && ch != '\u{2800}' {
        return false;
    }
    let Color::Rgb(kr, kg, kb) = BG else { return false };
    match bg {
        portraits::Ink::C(r, g, b) => (r, g, b) == (kr, kg, kb),
        // Quantised, so our ground has landed on whatever palette entry is
        // nearest it -- near-black, and near-black behind a blank glyph is
        // nothing worth painting either way.
        portraits::Ink::I(n) => n == 16 || n == 0 || n == 232 || n == 233,
    }
}

pub fn portrait(f: &mut Frame, area: Rect, x: u16, y: u16, cells: &[portraits::Cell], cols: u16) {
    for (i, &(ch, fg, bg)) in cells.iter().enumerate() {
        let (c, r) = (i as u16 % cols, i as u16 / cols);
        let (px, py) = (x + c, y + r);
        if px >= area.x + area.width || py >= area.y + area.height {
            continue;
        }
        if is_ground(ch, bg) {
            continue;
        }
        if let Some(cell) = f.buffer_mut().cell_mut((px, py)) {
            cell.set_char(ch).set_fg(ink(fg)).set_bg(ink(bg));
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

/// Dissolve a region into the page from the inside out.
///
/// The panel used to be a rectangle of map with a hard edge, which reads as a
/// window cut into the page -- a frame, with the picture behind it. What is
/// wanted is one surface: the map at full strength in the middle, thinning to
/// nothing before it reaches any edge, so there is no line anywhere for the eye
/// to catch on.
///
/// The boundary is deliberately not an ellipse. A perfect oval is as obviously
/// a shape as a rectangle is; two low harmonics of the angle push it in and out
/// by a few percent, which is enough to read as torn rather than cut. It is a
/// pure function of position -- no clock -- so it does not shimmer, and a
/// snapshot is the same picture every time.
///
/// `strength` scales the whole thing, so a panel arriving fades and feathers in
/// one pass rather than being dimmed twice.
pub fn feather(f: &mut Frame, area: Rect, strength: f32) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    // Where the falloff begins, as a fraction of the way out. Inside this the
    // map is untouched; the remainder is the dissolve.
    const CORE: f32 = 0.52;
    let (cx, cy) = (area.width as f32 / 2.0, area.height as f32 / 2.0);
    let buf = f.buffer_mut();
    for y in 0..area.height {
        for x in 0..area.width {
            // Half a cell in, so the middle of a cell is what is measured and
            // the shape is symmetric about the centre of the rect rather than a
            // corner of one.
            let u = (x as f32 + 0.5 - cx) / cx;
            let v = (y as f32 + 0.5 - cy) / cy;
            let r = u.hypot(v);
            let ang = v.atan2(u);
            let wobble = 0.07 * (ang * 3.0).sin() + 0.05 * (ang * 5.0 + 1.7).cos();
            let edge = 1.0 + wobble;
            let a = if r <= CORE * edge {
                1.0
            } else if r >= edge {
                0.0
            } else {
                let t = (r - CORE * edge) / (edge - CORE * edge);
                1.0 - t * t * (3.0 - 2.0 * t)
            };
            let k = (a * strength).clamp(0.0, 1.0);
            let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) else { continue };
            let (fg, bg) = (toward_bg(cell.fg, k), toward_bg(cell.bg, k));
            cell.set_fg(fg).set_bg(bg);
        }
    }
}

/// Dim a region by an amount that slides across it.
///
/// The reading column had a flat `veil` over it, and a flat dim has edges: a
/// rectangle of slightly darker map over a soft radial fade is a hard line
/// straight down the screen, which is exactly the seam this was supposed to
/// remove. Ramped, the knock-back arrives gradually and there is nowhere for the
/// eye to catch.
///
/// `from` applies at the left of `area` and `to` at the right, both as keep
/// factors like `veil` -- 1 leaves a cell alone.
pub fn veil_ramp(f: &mut Frame, area: Rect, from: f32, to: f32) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = f.buffer_mut();
    for x in 0..area.width {
        // Smoothstepped rather than linear so the two ends meet whatever is
        // beside them without a crease.
        let t = x as f32 / (area.width.max(2) - 1) as f32;
        let k = from + (to - from) * (t * t * (3.0 - 2.0 * t));
        if k >= 0.999 {
            continue;
        }
        for y in 0..area.height {
            let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) else { continue };
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


    /// The panel has no edge to catch the eye on.
    ///
    /// The failure this pins is a rectangle of map sitting on the page like a
    /// window cut into it. Filled solid and feathered, the middle must survive
    /// untouched and every corner must be gone -- and the boundary must not be
    /// the same distance out in every direction, or it is an ellipse, which is
    /// as obviously a shape as the rectangle was.
    #[test]
    fn the_map_panel_dissolves_into_the_page_rather_than_ending() {
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;
        use ratatui::Terminal;

        let (w, h) = (46u16, 20u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let area = Rect { x: 0, y: 0, width: w, height: h };
        let ink = Color::Rgb(220, 220, 220);
        term.draw(|f| {
            for y in 0..h {
                for x in 0..w {
                    if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                        c.set_char('#').set_fg(ink);
                    }
                }
            }
            feather(f, area, 1.0);
        })
        .unwrap();

        let buf = term.backend().buffer().clone();
        // Through the same converter the blend uses: a neutral grey comes back
        // as a palette index rather than a triple, and reading only `Rgb` made
        // the middle of a fully lit panel measure zero.
        let bright = |x: u16, y: u16| -> u16 {
            match buf.cell((x, y)).and_then(|c| termap::canvas::rgb_of(c.fg)) {
                Some((r, g, b)) => r as u16 + g as u16 + b as u16,
                None => 0,
            }
        };
        let full = bright(w / 2, h / 2);
        assert!(full > 600, "the middle was dimmed: {full}");
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            let corner = bright(x, y);
            assert!(corner < 60, "a corner survived at {x},{y}: {corner} against {full}");
        }

        // Not an ellipse: the distance at which it gives out differs by
        // direction. Measured along two rays from the centre.
        let reach = |dx: i32, dy: i32| -> u16 {
            let (mut n, mut x, mut y) = (0u16, w as i32 / 2, h as i32 / 2);
            while x >= 0 && y >= 0 && x < w as i32 && y < h as i32 {
                if bright(x as u16, y as u16) < 60 {
                    break;
                }
                n += 1;
                x += dx;
                y += dy;
            }
            n
        };
        let (right, up) = (reach(1, 0), reach(0, -1));
        assert!(right > 2 && up > 2, "it gave out immediately: {right}, {up}");

        // And a strength of zero takes the whole thing, so the arrival fade and
        // the falloff are one pass rather than two.
        term.draw(|f| {
            for y in 0..h {
                for x in 0..w {
                    if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                        c.set_char('#').set_fg(ink);
                    }
                }
            }
            feather(f, area, 0.0);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let mid = buf
            .cell((w / 2, h / 2))
            .and_then(|c| termap::canvas::rgb_of(c.fg))
            .map_or(0u16, |(r, g, b)| r as u16 + g as u16 + b as u16);
        assert!(mid < 60, "an invisible panel still drew: {mid}");
    }
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
                let f = portrait_loop(p, t, true);
                assert_eq!(f.len(), p.cols as usize * p.rows as usize, "{} at {t}s", p.id);
            }
        }
    }
}
