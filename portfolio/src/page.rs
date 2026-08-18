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

/// A drawing reduced for the gallery: columns, rows, and the cells.
type Small = (u16, u16, Vec<(char, u8, u8)>);

/// Room for the titling beside a plate.
const TEXT: u16 = 40;
/// Space between the two columns of the gallery.
const COLGAP: u16 = 6;
const GAP: u16 = 4;

/// The plate column is as wide as the widest drawing, measured rather than
/// guessed. Guessed, it was 22 against art that runs to 34, and every emblem
/// printed straight through the paragraph beside it.
fn plate_w() -> u16 {
    emblems::EMBLEMS.iter().map(|e| e.art.cols.div_ceil(2)).max().unwrap_or(0)
}

/// One cell of the gallery: a half-size drawing and its three lines.
fn cell_w() -> u16 {
    plate_w() + GAP + TEXT
}

pub fn width() -> u16 {
    cell_w() * 2 + COLGAP
}

/// A half-size copy of a drawing.
///
/// The emblems are authored at one pixel per half-block, which is the right
/// size to look at on its own and twice the size this gallery wants. Rather
/// than redraw twelve of them, unpack each back into pixels, average 2x2, and
/// re-pair into half-blocks. Averaging in coverage rather than in colour is
/// what keeps the tint exact.
fn halved(m: &Emblem) -> Small {
    let (w, rows) = (m.art.cols as usize, m.art.rows as usize);
    let h = rows * 2;
    let mut px = vec![0f32; w * h];
    for r in 0..rows {
        for c in 0..w {
            let (ch, fa, ba) = m.art.cells[r * w + c];
            if ch == ' ' {
                continue;
            }
            px[(r * 2) * w + c] = fa as f32;
            px[(r * 2 + 1) * w + c] = ba as f32;
        }
    }

    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let mut small = vec![0f32; hw * hh];
    for y in 0..hh {
        for x in 0..hw {
            let mut sum = 0.0;
            let mut n = 0.0;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (sy, sx) = (y * 2 + dy, x * 2 + dx);
                    if sy < h && sx < w {
                        sum += px[sy * w + sx];
                        n += 1.0;
                    }
                }
            }
            small[y * hw + x] = if n > 0.0 { sum / n } else { 0.0 };
        }
    }

    let out_rows = hh.div_ceil(2);
    let mut cells = Vec::with_capacity(hw * out_rows);
    for r in 0..out_rows {
        for c in 0..hw {
            let t = small[(r * 2) * hw + c];
            let b = if r * 2 + 1 < hh { small[(r * 2 + 1) * hw + c] } else { 0.0 };
            cells.push(if t < 1.0 && b < 1.0 {
                (' ', 0, 0)
            } else {
                ('\u{2580}', t.round() as u8, b.round() as u8)
            });
        }
    }
    (hw as u16, out_rows as u16, cells)
}

enum Block {
    Title(String, String),
    /// A section heading with a rule running off to the right.
    Rule(&'static str),
    /// Two entries side by side. The gallery is a shelf, not a list, and one
    /// per row turned twelve short captions into a very long scroll.
    Row(Vec<(Entry, Small)>),
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
        // A free function rather than a closure: the shelf below needs the
        // same two lines and two closures cannot both hold `blocks`.
        fn push(blocks: &mut Vec<(u16, Block)>, y: &mut u16, b: Block) {
            let h = height_of(&b);
            blocks.push((*y, b));
            *y += h;
        }

        push(&mut blocks, &mut y, Block::Title("TASTE".into(), s.open.clone()));
        push(&mut blocks, &mut y, Block::Space(2));

        let shelf = |blocks: &mut Vec<(u16, Block)>, y: &mut u16, title, list: &[Entry]| {
            push(blocks, y, Block::Rule(title));
            for pair in list.chunks(2) {
                let row: Vec<_> = pair
                    .iter()
                    .map(|e| {
                        let art = emblems::find(&e.emblem).map(halved).unwrap_or((0, 0, vec![]));
                        (e.clone(), art)
                    })
                    .collect();
                push(blocks, y, Block::Row(row));
                *y += 2;
            }
        };
        shelf(&mut blocks, &mut y, "PEOPLE", &s.figures);
        y += 1;
        shelf(&mut blocks, &mut y, "WATCHING", &s.works);

        push(&mut blocks, &mut y, Block::Space(2));
        push(&mut blocks, &mut y, Block::Para(s.close.clone()));
        push(&mut blocks, &mut y, Block::Space(2));

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
        Block::Row(cells) => cells.iter().map(|(_, a)| a.1.max(3)).max().unwrap_or(3),
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
        Block::Row(cells) => {
            let pw = plate_w();
            let cw = cell_w();
            for (i, (e, art)) in cells.iter().enumerate() {
                let cx = x + i as u16 * (cw + COLGAP);
                if cx + 8 > x + w {
                    continue;
                }
                // Centred in its column: the drawings are cropped to their ink
                // and so are all different widths, and left-aligning them makes
                // the shelf's edge look ragged for no reason.
                plate(f, area, cx + pw.saturating_sub(art.0) / 2, dy, art, e);
                let tx = cx + pw + GAP;
                let tw = cw.saturating_sub(pw + GAP);
                // Set against the middle of the drawing, so a short caption
                // does not float at the top of a tall plate.
                let top = dy + (art.1 as i32 - 3) / 2;
                put(f, area, tx, tw, top, vec![Span::styled(
                    e.name.to_uppercase(),
                    Style::default().fg(FG).add_modifier(Modifier::BOLD),
                )]);
                put(f, area, tx, tw, top + 1, vec![Span::styled(
                    e.from.clone(),
                    Style::default().fg(FAINT),
                )]);
                for (j, l) in wrap(&e.line, tw as usize).into_iter().enumerate() {
                    put(f, area, tx, tw, top + 2 + j as i32, vec![Span::styled(
                        l,
                        Style::default().fg(DIM),
                    )]);
                }
            }
        }
    }
}

/// Blit one emblem, a half-block row at a time.
///
/// The art stores alphas rather than colours so a plate can be dimmed or lit
/// without regenerating it; the tint is applied here against the page ground.
fn plate(
    f: &mut Frame,
    area: Rect,
    x: u16,
    dy: i32,
    art: &Small,
    e: &Entry,
) {
    use crate::paint::BG;
    use ratatui::style::Color;

    let Some(m) = emblems::find(&e.emblem) else { return };
    let Color::Rgb(br, bg, bb) = BG else { return };
    let mix = |a: u8| {
        let a = a as f32 / 255.0;
        termap::canvas::ink(
            (br as f32 + (m.rgb.0 as f32 - br as f32) * a) as u8,
            (bg as f32 + (m.rgb.1 as f32 - bg as f32) * a) as u8,
            (bb as f32 + (m.rgb.2 as f32 - bb as f32) * a) as u8,
        )
    };

    let (cols, rows, cells) = art;
    for r in 0..*rows {
        let y = dy + r as i32;
        if y < 0 || y >= area.height as i32 {
            continue;
        }
        let buf = f.buffer_mut();
        for c in 0..*cols {
            let (ch, fa, ba) = cells[(r * cols + c) as usize];
            if fa == 0 && ba == 0 {
                continue;
            }
            let Some(cell) = buf.cell_mut((x + c, area.y + y as u16)) else { continue };
            cell.set_char(ch).set_fg(mix(fa)).set_bg(mix(ba));
        }
    }
}
