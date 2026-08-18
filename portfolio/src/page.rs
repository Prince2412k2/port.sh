//! Laying the taste essay out as a page, and scrolling it.
//!
//! Blocks are measured once against a width and given absolute y positions;
//! rendering then draws only the ones the viewport overlaps. That is the whole
//! trick, and it is what makes the scroll cost nothing regardless of how long
//! the essay gets — the alternative, rendering everything and clipping, walks
//! the entire document every frame.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::emblems::{self, Emblem};
use crate::paint::{wrap, ACCENT, DIM, FAINT, FG};
use crate::taste::{Entry, Sheet};

/// The text measure. Prose set across a whole wide terminal is a line the eye
/// cannot find its way back from.
const TEXT: u16 = 60;
const GAP: u16 = 4;

/// The plate column is as wide as the widest drawing, measured rather than
/// guessed. Guessed, it was 22 against art that runs to 34, and every emblem
/// printed straight through the paragraph beside it.
fn plate_w() -> u16 {
    emblems::EMBLEMS.iter().map(|e| e.art.cols).max().unwrap_or(0)
}

pub fn width() -> u16 {
    plate_w() + GAP + TEXT
}

enum Block {
    Title(String, String),
    /// A section heading with a rule running off to the right.
    Rule(&'static str),
    /// An emblem, its titling, and its paragraph. `right` puts the plate on the
    /// far side, which is what stops six of these reading as a list.
    Plate { e: Entry, right: bool },
    Thread(Entry),
    Para(String),
    Space(u16),
}

pub struct Page {
    blocks: Vec<(u16, Block)>,
    pub height: u16,
}

impl Page {
    pub fn build(s: &Sheet) -> Page {
        let mut blocks = Vec::new();
        let mut y = 0u16;
        let mut push = |y: &mut u16, b: Block| {
            let h = height_of(&b);
            blocks.push((*y, b));
            *y += h;
        };

        push(&mut y, Block::Title("TASTE".into(), "what the rest of this is for".into()));
        push(&mut y, Block::Space(1));
        push(&mut y, Block::Para(s.open.clone()));
        push(&mut y, Block::Space(2));

        push(&mut y, Block::Rule("THE PEOPLE"));
        for (i, e) in s.figures.iter().enumerate() {
            push(&mut y, Block::Plate { e: e.clone(), right: i % 2 == 1 });
            push(&mut y, Block::Space(2));
        }

        push(&mut y, Block::Rule("THE WORK"));
        for (i, e) in s.works.iter().enumerate() {
            push(&mut y, Block::Plate { e: e.clone(), right: i % 2 == 0 });
            push(&mut y, Block::Space(2));
        }

        push(&mut y, Block::Rule("WHAT IT ADDS UP TO"));
        for e in &s.threads {
            push(&mut y, Block::Thread(e.clone()));
            push(&mut y, Block::Space(1));
        }

        push(&mut y, Block::Space(2));
        push(&mut y, Block::Para(s.close.clone()));
        push(&mut y, Block::Space(2));

        Page { blocks, height: y }
    }

    /// Draw the page at `scroll` rows down. Blocks outside the viewport are
    /// skipped whole; the one straddling each edge is clipped per line.
    pub fn render(&self, f: &mut Frame, area: Rect, scroll: u16) {
        let w = width().min(area.width.saturating_sub(4));
        let x = area.x + (area.width.saturating_sub(w)) / 2;
        let top = scroll;
        let bottom = scroll + area.height;

        for (y, b) in &self.blocks {
            let h = height_of(b);
            if *y + h < top || *y > bottom {
                continue;
            }
            draw(f, area, x, w, *y as i32 - scroll as i32, b);
        }
    }
}

fn height_of(b: &Block) -> u16 {
    match b {
        Block::Title(..) => 3,
        Block::Rule(_) => 3,
        Block::Space(n) => *n,
        Block::Para(t) => wrap(t, TEXT as usize).len() as u16,
        Block::Thread(e) => 1 + wrap(&e.body, TEXT as usize).len() as u16,
        Block::Plate { e, .. } => {
            let art = emblems::find(&e.emblem).map_or(0, |m| m.art.rows);
            // Titling is three lines, then the paragraph.
            let text = 3 + wrap(&e.body, TEXT as usize).len() as u16;
            art.max(text)
        }
    }
}

/// One line of the page, drawn only if it lands inside the viewport.
fn put(f: &mut Frame, area: Rect, x: u16, w: u16, dy: i32, spans: Vec<Span<'static>>) {
    if dy < 0 || dy >= area.height as i32 || w == 0 {
        return;
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x, y: area.y + dy as u16, width: w, height: 1 },
    );
}

fn draw(f: &mut Frame, area: Rect, x: u16, w: u16, dy: i32, b: &Block) {
    match b {
        Block::Space(_) => {}
        Block::Title(t, sub) => {
            put(f, area, x, w, dy, vec![Span::styled(
                t.clone(),
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            )]);
            put(f, area, x, w, dy + 1, vec![Span::styled(
                sub.clone(),
                Style::default().fg(FAINT),
            )]);
        }
        Block::Rule(label) => {
            let used = label.chars().count() as u16 + 2;
            put(f, area, x, w, dy + 1, vec![
                Span::styled(format!("{label}  "), Style::default().fg(ACCENT)),
                Span::styled(
                    "─".repeat(w.saturating_sub(used) as usize),
                    Style::default().fg(FAINT),
                ),
            ]);
        }
        Block::Para(t) => {
            for (i, l) in wrap(t, TEXT as usize).into_iter().enumerate() {
                put(f, area, x, w, dy + i as i32, vec![Span::styled(
                    l,
                    Style::default().fg(DIM),
                )]);
            }
        }
        Block::Thread(e) => {
            put(f, area, x, w, dy, vec![Span::styled(
                e.name.to_uppercase(),
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            )]);
            for (i, l) in wrap(&e.body, TEXT as usize).into_iter().enumerate() {
                put(f, area, x, w, dy + 1 + i as i32, vec![Span::styled(
                    l,
                    Style::default().fg(DIM),
                )]);
            }
        }
        Block::Plate { e, right } => {
            let pw = plate_w();
            let (px, tx) = if *right {
                (x + w.saturating_sub(pw), x)
            } else {
                (x, x + pw + GAP)
            };
            let tw = w.saturating_sub(pw + GAP);

            if let Some(m) = emblems::find(&e.emblem) {
                // Centred in its column: the drawings are cropped to their ink
                // and so are all different widths, and left-aligning them makes
                // the page's edge look ragged for no reason.
                plate(f, area, px + (pw.saturating_sub(m.art.cols)) / 2, dy, m);
            }
            put(f, area, tx, tw, dy, vec![Span::styled(
                e.name.to_uppercase(),
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            )]);
            put(f, area, tx, tw, dy + 1, vec![Span::styled(
                e.from.clone(),
                Style::default().fg(FAINT),
            )]);
            put(f, area, tx, tw, dy + 2, vec![Span::styled(
                e.line.clone(),
                Style::default().fg(ACCENT).add_modifier(Modifier::ITALIC),
            )]);
            for (i, l) in wrap(&e.body, tw as usize).into_iter().enumerate() {
                put(f, area, tx, tw, dy + 4 + i as i32, vec![Span::styled(
                    l,
                    Style::default().fg(DIM),
                )]);
            }
        }
    }
}

/// Blit one emblem, a half-block row at a time.
///
/// The art stores alphas rather than colours so a plate can be dimmed or lit
/// without regenerating it; the tint is applied here against the page ground.
fn plate(f: &mut Frame, area: Rect, x: u16, dy: i32, m: &Emblem) {
    use crate::paint::BG;
    use ratatui::style::Color;

    let Color::Rgb(br, bg, bb) = BG else { return };
    let mix = |a: u8| {
        let a = a as f32 / 255.0;
        termap::canvas::ink(
            (br as f32 + (m.rgb.0 as f32 - br as f32) * a) as u8,
            (bg as f32 + (m.rgb.1 as f32 - bg as f32) * a) as u8,
            (bb as f32 + (m.rgb.2 as f32 - bb as f32) * a) as u8,
        )
    };

    for r in 0..m.art.rows {
        let y = dy + r as i32;
        if y < 0 || y >= area.height as i32 {
            continue;
        }
        let buf = f.buffer_mut();
        for c in 0..m.art.cols {
            let (ch, fa, ba) = m.art.cells[(r * m.art.cols + c) as usize];
            if fa == 0 && ba == 0 {
                continue;
            }
            let Some(cell) = buf.cell_mut((x + c, area.y + y as u16)) else { continue };
            cell.set_char(ch).set_fg(mix(fa)).set_bg(mix(ba));
        }
    }
}
