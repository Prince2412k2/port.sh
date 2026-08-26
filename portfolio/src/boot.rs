//! The first two seconds.
//!
//! There is real work behind this: a 1.6 GB tile archive gets opened, a
//! heightmap mapped, the sheets parsed. Over SSH there is also a connection
//! settling down. Something has to be on screen for that, and a spinner would
//! be the wrong something — this is the one moment the whole thing is a blank
//! terminal, which is the most interesting canvas it will have all session.
//!
//! So: a horizon. Braille contour lines that arrive rough and settle flat,
//! with the name coming up out of them. Same canvas as the map, same tide as
//! the chat's wait, which is the point — by the time it clears you have
//! already been shown what the rest of this is made of.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::paint::{dim_to, ease, ACCENT, FAINT, FG};

/// How long the whole thing lasts.
pub const SECS: f64 = 2.4;

/// Where the name has fully arrived and the sea has gone flat.
const SETTLED: f64 = 1.5;

pub fn render(f: &mut Frame, area: Rect, t: f64) {
    if area.width < 20 || area.height < 8 {
        return;
    }
    // 1 at the start, 0 once settled: how far from flat the water still is.
    let swell = (1.0 - (t / SETTLED).clamp(0.0, 1.0)).powf(1.6);
    // The whole frame fades away over the last stretch.
    let out = 1.0 - ((t - SETTLED) / (SECS - SETTLED)).clamp(0.0, 1.0);
    let out = ease(out) as f32;

    sea(f, area, t, swell, out);

    let name = "PRINCE PATEL";
    let sub = "a terminal portfolio";
    let appear = ease((t / SETTLED).clamp(0.0, 1.0)) as f32 * out;
    let y = area.y + area.height / 2;
    let put = |f: &mut Frame, y: u16, s: &str, style: Style| {
        let w = s.chars().count() as u16;
        if w > area.width {
            return;
        }
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(s.to_string(), style))),
            Rect { x: area.x + (area.width - w) / 2, y, width: w, height: 1 },
        );
    };
    put(
        f,
        y,
        name,
        Style::default().fg(dim_to(FG, appear)).add_modifier(Modifier::BOLD),
    );
    // The subtitle comes in behind the name rather than with it, so the two
    // arrive as one thing settling instead of two things switching on.
    let late = ease(((t - 0.6) / (SETTLED - 0.4)).clamp(0.0, 1.0)) as f32 * out;
    put(f, y + 1, sub, Style::default().fg(dim_to(FAINT, late)));

    if t > SETTLED * 0.8 {
        let k = ease(((t - SETTLED * 0.8) / 0.5).clamp(0.0, 1.0)) as f32 * out;
        put(
            f,
            area.y + area.height.saturating_sub(3),
            "any key",
            Style::default().fg(dim_to(ACCENT, k * 0.7)),
        );
    }
}

/// The horizon: contour lines that start choppy and lie down.
fn sea(f: &mut Frame, area: Rect, t: f64, swell: f64, alpha: f32) {
    use termap::canvas::{Canvas, Fog, MAT_DOT, TINT_MONO};
    use termap::raster::{self, Pen};

    if alpha <= 0.01 {
        return;
    }
    let mut canvas = Canvas::new(area.width as usize, area.height as usize);
    let (sw, sh) = (canvas.sw as f64, canvas.sh as f64);

    const LINES: usize = 11;
    for i in 0..LINES {
        let fy = (i as f64 + 0.5) / LINES as f64;
        // Fade out from the middle, so the name has somewhere quiet to land.
        let from_mid = (fy - 0.5).abs() * 2.0;
        let pen = Pen {
            width: 1.0,
            alpha: (0.30 + 0.5 * from_mid) as f32 * alpha,
            depth: (0.1 + (1.0 - from_mid) * 0.75) as f32,
            tint: TINT_MONO,
            mat: MAT_DOT,
            pick: u32::MAX,
            behind: termap::canvas::Behind::Ignore,
        };
        let phase = i as f64 * 1.7;
        let (k1, k2) = (4.0 + i as f64 * 0.6, 2.1 + i as f64 * 0.27);
        let mut prev: Option<[f64; 2]> = None;
        let steps = (sw as usize / 2).max(8);
        for s in 0..=steps {
            let u = s as f64 / steps as f64;
            let env = (u * std::f64::consts::PI).sin().powf(1.3);
            let a = (u * k1 + t * 1.1 + phase).sin() * 0.6
                + (u * k2 - t * 0.7 + phase * 1.6).sin() * 0.4;
            let y = fy * sh + a * env * sh * 0.16 * swell;
            let p = [u * sw, y];
            if let Some(q) = prev {
                raster::line(&mut canvas, q, p, &pen);
            }
            prev = Some(p);
        }
    }
    canvas.resolve(f.buffer_mut(), area, &Fog::default(), true);
}
