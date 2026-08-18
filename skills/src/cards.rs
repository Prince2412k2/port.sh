//! The projects tab.
//!
//! A box in the top-left corner holds the project's mark, drifting up and down
//! so the card reads as an object rather than a picture, with a row of pips
//! under it saying which of the nine you are on. The tools it was built with
//! loop past underneath. Everything else on screen — which is most of it — is
//! given to a working diagram of how the project actually functions.
//!
//! The diagram is the point. An essay in a terminal is an essay you could have
//! read anywhere; a mechanism you can watch run is the thing this medium is
//! actually good at. Projects that do not have one yet fall back to their
//! prose rather than leaving the space empty, and that fallback is meant to be
//! temporary.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::data::Project;
use crate::logos;
use crate::marks;
use crate::scene;
use crate::tile;

/// Cells the left column takes when there is room for it.
const RAIL: u16 = 52;

/// The composition never grows past this, however wide the terminal is.
///
/// A card is a page, and a page has a measure. Left to fill a 230-column
/// terminal the corner sticks to one edge, the diagram floats somewhere near
/// the middle, and the two stop looking like parts of the same thing — the eye
/// has to travel too far between them to hold both. Capping and centring costs
/// nothing on a normal terminal, where the cap never binds, and is the whole
/// difference on a large one.
const MAX_W: u16 = 150;
const MAX_H: u16 = 34;
/// Rows of looping tool strip under the mark.
const STRIP_H: u16 = 5;
/// How far the mark drifts, in rows, and how long a round trip takes.
const BOB: f64 = 1.4;
const BOB_SECS: f64 = 5.5;

const FG: Color = Color::Rgb(198, 202, 208);
const BODY: Color = Color::Rgb(152, 158, 170);
const FAINT: Color = Color::Rgb(74, 80, 92);

fn mix(c: (u8, u8, u8), a: f32) -> Color {
    let bg = tile::BG;
    let a = a.clamp(0.0, 1.0);
    Color::Rgb(
        (bg.0 as f32 + (c.0 as f32 - bg.0 as f32) * a) as u8,
        (bg.1 as f32 + (c.1 as f32 - bg.1 as f32) * a) as u8,
        (bg.2 as f32 + (c.2 as f32 - bg.2 as f32) * a) as u8,
    )
}

pub struct View<'a> {
    pub projects: &'a [Project],
    pub at: usize,
    pub scroll: u16,
    pub t: f64,
}

/// Where the pips landed, so one can be clicked.
#[derive(Default, Clone, Copy)]
pub struct Hit {
    pub pips: Rect,
}

pub fn render(f: &mut Frame, full: Rect, v: &View) -> Hit {
    let p = &v.projects[v.at];
    let accent = marks::find(&p.mark).map_or((190, 195, 205), |m| m.rgb);

    // The page, centred in whatever it was given.
    let w = full.width.min(MAX_W);
    let h = full.height.min(MAX_H);
    let area = Rect {
        x: full.x + (full.width - w) / 2,
        y: full.y + (full.height - h) / 2,
        width: w,
        height: h,
    };

    // Whether both fit is a question for the diagram, not a guess. Asking it
    // for its footprint is what stops a mechanism being cropped at the edge and
    // looking like a rendering fault. If it will not fit, the diagram goes
    // rather than the words: a mechanism drawn at forty columns is not a
    // mechanism, it is a smudge.
    let (sw, sh) = scene::footprint(&p.id);
    let rail = RAIL.min(area.width / 3).max(38);
    let wide = area.width >= rail + sw + 4 && area.height >= sh;
    let rail = if wide { rail } else { area.width };
    let left = Rect { width: rail, ..area };

    let hit = corner(f, left, v, accent);

    if wide {
        let stage = Rect {
            x: area.x + rail,
            width: area.width - rail,
            ..area
        };
        if !scene::draw(f.buffer_mut(), inset(stage, 2, 1), p, v.t) {
            prose(f, inset(stage, 2, 1), v, accent);
        }
    } else {
        // Narrow: the words take everything under the corner.
        let used = mark_h(v) + STRIP_H + 4;
        if area.height > used {
            prose(
                f,
                Rect { y: area.y + used, height: area.height - used, ..area },
                v,
                accent,
            );
        }
    }
    hit
}

fn inset(r: Rect, x: u16, y: u16) -> Rect {
    Rect {
        x: r.x + x,
        y: r.y + y,
        width: r.width.saturating_sub(x * 2),
        height: r.height.saturating_sub(y * 2),
    }
}

/// The box is sized to the largest mark, not to the current one.
///
/// Marks differ by a row or two — watch-party's reel is square where the drawn
/// emblems are wider than tall — and sizing the box to whichever is showing
/// makes the strip and the caption under it jump every time you page. A fixed
/// frame with the mark centred in it costs two rows of air and buys a layout
/// that holds still.
fn box_dims() -> (u16, u16) {
    marks::MARKS.iter().fold((0, 0), |(w, h), m| {
        (w.max(m.art.cols), h.max(m.art.rows))
    })
}

fn mark_h(_v: &View) -> u16 {
    box_dims().1 + 4
}

/// The mark, its box, the pips, and the loop of tools.
fn corner(f: &mut Frame, area: Rect, v: &View, accent: (u8, u8, u8)) -> Hit {
    let p = &v.projects[v.at];
    let Some(m) = marks::find(&p.mark) else { return Hit::default() };

    let (mw, mh) = box_dims();
    // The box takes the column rather than shrink-wrapping the mark. Sized to
    // the art it leaves a strip of dead ground between itself and the diagram,
    // and the two stop looking like one layout.
    let bw = area.width.saturating_sub(4).max(mw + 6);
    let bh = mh + 4;
    let bx = area.x + 2;
    let by = area.y + 1;
    let bounds = Rect { x: bx, y: by, width: bw, height: bh };

    scene::boxed(f.buffer_mut(), area, bounds, "", (58, 64, 78), false);

    // A slow float. Rows are the smallest step a terminal has, so the motion is
    // deliberately small and slow -- a fast bob at this resolution is a twitch,
    // and it would redraw the mark every frame for nothing.
    let bob = ((v.t / BOB_SECS) * std::f64::consts::TAU).sin() * BOB;
    let mx = bx as i32 + (bw as i32 - m.art.cols as i32) / 2;
    let my = by as i32 + 2 + (mh as i32 - m.art.rows as i32) / 2 + bob.round() as i32;
    draw_mark(f.buffer_mut(), inset(bounds, 1, 1), mx, my, m, 1.0);

    // Pips: which of the nine, and how many there are, in one row.
    let py = by + bh - 2;
    let n = v.projects.len() as u16;
    let pw = n * 2 - 1;
    let px = bx + bw.saturating_sub(pw) / 2;
    for i in 0..v.projects.len() {
        let on = i == v.at;
        scene::text(
            f.buffer_mut(),
            area,
            px as i32 + i as i32 * 2,
            py as i32,
            if on { "●" } else { "·" },
            if on { accent } else { (72, 78, 92) },
            on,
        );
    }

    let sy = by + bh + 1;
    if sy + STRIP_H <= area.y + area.height {
        // Wider than the box: the strip is a loop of everything the project was
        // built with, and cropping it to the mark's width makes it look like a
        // caption for the mark instead.
        strip(
            f,
            Rect {
                x: bx,
                y: sy,
                width: area.width.saturating_sub(4),
                height: STRIP_H,
            },
            v,
        );
    }

    // What it is, under the tools. The mark says which project; this says what
    // the project is, and without it the corner is a picture with no caption.
    let ty = sy + STRIP_H + 1;
    if ty < area.y + area.height {
        let w = area.width.saturating_sub(4) as usize;
        let mut lines = vec![Line::from(Span::styled(
            p.name.clone(),
            Style::default().fg(mix(accent, 1.0)).add_modifier(Modifier::BOLD),
        ))];
        for l in wrap(&p.tag, w) {
            lines.push(Line::from(Span::styled(l, Style::default().fg(FG))));
        }
        lines.push(Line::default());
        for l in wrap(&p.stats, w) {
            lines.push(Line::from(Span::styled(l, Style::default().fg(FAINT))));
        }
        if p.repo != "local" && p.repo != "private" {
            lines.push(Line::from(Span::styled(
                p.repo.strip_prefix("github.com/").unwrap_or(&p.repo).to_string(),
                Style::default().fg(FAINT),
            )));
        }
        if p.draft {
            lines.push(Line::from(Span::styled(
                "· from a summary, not the source",
                Style::default().fg(FAINT).add_modifier(Modifier::ITALIC),
            )));
        }
        f.render_widget(
            Paragraph::new(lines),
            Rect {
                x: bx,
                y: ty,
                width: area.width.saturating_sub(4),
                height: (area.y + area.height).saturating_sub(ty),
            },
        );
    }

    Hit { pips: Rect { x: px, y: py, width: pw, height: 1 } }
}

fn draw_mark(buf: &mut Buffer, clip: Rect, x: i32, y: i32, m: &marks::Mark, light: f32) {
    let a = &m.art;
    for r in 0..a.rows as i32 {
        let sy = y + r;
        if sy < clip.y as i32 || sy >= (clip.y + clip.height) as i32 {
            continue;
        }
        for c in 0..a.cols as i32 {
            let sx = x + c;
            if sx < clip.x as i32 || sx >= (clip.x + clip.width) as i32 {
                continue;
            }
            let (ch, fa, ba) = a.cells[(r * a.cols as i32 + c) as usize];
            if fa == 0 && ba == 0 {
                continue;
            }
            if let Some(cell) = buf.cell_mut((sx as u16, sy as u16)) {
                cell.set_char(ch)
                    .set_fg(mix(m.rgb, fa as f32 / 255.0 * light))
                    .set_bg(mix(m.rgb, ba as f32 / 255.0 * light));
            }
        }
    }
}

/// The tools, looping past. Always moving, because a strip that only scrolls
/// when it overflows changes character between projects for no reason the
/// reader can see.
fn strip(f: &mut Frame, area: Rect, v: &View) {
    let p = &v.projects[v.at];
    let arts: Vec<_> = p.tools.iter().filter_map(|t| logos::find(t)).collect();
    if arts.is_empty() || area.height == 0 {
        return;
    }
    const PAD: u16 = 3;
    let total: i32 = arts.iter().map(|l| (l.sm.cols + PAD) as i32).sum();
    let offset = ((v.t * 4.0) as i64).rem_euclid(total as i64) as i32;

    let buf = f.buffer_mut();
    // Drawn twice, a full loop apart, so a mark leaving on the left is already
    // arriving on the right. Otherwise the strip has a seam.
    for pass in 0..2 {
        let mut x = -offset + pass * total;
        for l in &arts {
            tile::draw(buf, area, area.x as i32 + x, area.y as i32, l, true, 0.8);
            x += (l.sm.cols + PAD) as i32;
        }
    }
}

/// The fallback for projects with no diagram yet, and the home of the prose.
fn prose(f: &mut Frame, area: Rect, v: &View, accent: (u8, u8, u8)) {
    let p = &v.projects[v.at];
    let w = area.width.max(20) as usize;
    let acc = mix(accent, 1.0);

    let mut lines: Vec<Line> = Vec::new();
    for b in &p.beats {
        lines.push(Line::default());
        for l in wrap(&b.head, w) {
            lines.push(Line::from(Span::styled(
                l,
                Style::default().fg(acc).add_modifier(Modifier::BOLD),
            )));
        }
        for l in wrap(&b.body, w) {
            lines.push(Line::from(Span::styled(l, Style::default().fg(BODY))));
        }
    }

    let total = lines.len() as u16;
    let top = v.scroll.min(total.saturating_sub(1));
    let more = total > area.height + top;
    let body_h = area.height.saturating_sub(more as u16);
    f.render_widget(
        Paragraph::new(lines).scroll((top, 0)),
        Rect { height: body_h, ..area },
    );
    if more {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "▾  j to read on",
                Style::default().fg(FAINT),
            ))),
            Rect { y: area.y + body_h, height: 1, width: 18.min(area.width), ..area },
        );
    }
}

/// Greedy word wrap. A word longer than the column overflows rather than being
/// hyphenated: the long ones here are paths and identifiers.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let n = word.chars().count();
        if !line.is_empty() && line.chars().count() + 1 + n > width {
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

    #[test]
    fn wrap_respects_the_column() {
        let t = "the quick brown fox jumps over the lazy dog";
        for w in [10, 24, 60] {
            for l in wrap(t, w) {
                assert!(l.chars().count() <= w, "{l:?} exceeds {w}");
            }
        }
    }

    #[test]
    fn the_page_never_outgrows_its_measure() {
        // Whatever the terminal, the composition stays a page: capped, and
        // centred rather than stretched to the edges.
        for (w, h) in [(120u16, 40u16), (200, 60), (400, 120)] {
            let used_w = w.min(MAX_W);
            let used_h = h.min(MAX_H);
            assert!(used_w <= MAX_W && used_h <= MAX_H);
            // and it is not jammed against an edge when there is room to spare
            if w > MAX_W {
                assert!((w - used_w) / 2 > 0, "no left margin at {w}");
            }
        }
    }

    #[test]
    fn every_mark_fits_the_corner() {
        let (w, h) = box_dims();
        assert!(w + 6 <= RAIL, "the widest mark is {w}");
        for p in crate::data::parse(include_str!("../data/projects.txt")).unwrap() {
            let m = marks::find(&p.mark).unwrap();
            assert!(m.art.cols <= w && m.art.rows <= h, "{} escapes the box", p.id);
        }
    }

    #[test]
    fn the_box_does_not_resize_between_projects() {
        // Whatever is showing, the frame is the same frame — otherwise the tool
        // strip and the caption below it move every time you page.
        let ps = crate::data::parse(include_str!("../data/projects.txt")).unwrap();
        // Ask for each project in turn. The previous version of this asked for
        // project 0 every time and added a term multiplied by zero, so it
        // compared a value with itself and would have passed whatever the
        // layout did.
        let heights: Vec<u16> = (0..ps.len())
            .map(|at| mark_h(&View { projects: &ps, at, scroll: 0, t: 0.0 }))
            .collect();
        assert!(heights.windows(2).all(|w| w[0] == w[1]), "{heights:?}");
    }
}
