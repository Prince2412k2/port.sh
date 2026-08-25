//! The landing section.
//!
//! Deliberately the quietest screen in the app. The other three are animated,
//! dense and doing something; if this one competed with them it would be noise
//! before anyone had read a word. It is a name, a paragraph, and a way in.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::about::About;
use crate::paint::{wrap, ACCENT, BG, CYAN, DIM, FAINT, FG};

/// Text is set against this rather than the terminal's full width. A pitch
/// measured across 200 columns is one line the eye cannot track back from.
const MEASURE: u16 = 62;
/// Between the portrait and the text.
const GAP: u16 = 5;

/// The badge's own colour, so the mark on this line and the plate behind
/// `/cert` are recognisably the same object.
fn cert_mark() -> ratatui::style::Color {
    let (r, g, b) = crate::cert::PLATE;
    ratatui::style::Color::Rgb(r, g, b)
}

/// `FOUNDATIONS` as `Foundations`. The badge sets it in capitals because it is
/// a badge; a line of prose is not, and a word in caps in the middle of one
/// reads as shouting.
fn titlecase(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// The contact row: whichever links exist, joined by a dot.
///
/// Built in one place because two things need it — the row itself, and the
/// layout that has to leave room for the row — and a layout that measured the
/// links separately from the way they are drawn would be one edit away from
/// disagreeing with itself.
fn contact_row(a: &About) -> Vec<Span<'static>> {
    let mut spans: Vec<Span> = Vec::new();
    for (i, s) in [&a.github, &a.email, &a.ssh].iter().filter(|s| !s.is_empty()).enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ·   ", Style::default().fg(FAINT)));
        }
        spans.push(Span::styled((*s).clone(), Style::default().fg(CYAN)));
    }
    spans
}

fn row_width(spans: &[Span]) -> u16 {
    spans.iter().map(|s| s.content.chars().count()).sum::<usize>() as u16
}

/// How wide the text beside the portrait needs to be.
///
/// The prose sets to `MEASURE`, but the contact row is one line that must not
/// wrap or clip, and three links do not fit a reading measure. So the block is
/// as wide as its widest line and the paragraph is simply narrower than the
/// block it sits in.
///
/// This exists because it went wrong: with the portrait at its old size the
/// contact row happened to fit in what was left over, and the moment the
/// picture got bigger the ssh line lost its last few characters off the right
/// of the frame.
fn text_width(a: &About) -> u16 {
    MEASURE.max(row_width(&contact_row(a)))
}

/// Which bake of the portrait this screen can hang beside the text, if any.
///
/// The text block and the margins come first: whatever is left over is what
/// the picture may have, and if that is not enough for even the smallest bake
/// there is no picture. Dropped rather than squeezed — half a drawing beside a
/// narrowed column is worse than no drawing.
///
/// Shared with `shell`, which needs the same answer to decide whether this
/// section is still animating. Two independent guesses would let the loop stop
/// while the plate was still asking for frames.
pub fn plate(area: Rect, a: &About) -> Option<&'static crate::portraits::Portrait> {
    crate::portraits::fit(
        "snufkin-home",
        area.width.saturating_sub(text_width(a) + GAP + 8),
        area.height.saturating_sub(2),
    )
}

/// Where the two columns of this screen sit.
///
/// Worked out in one place so the test below can check the result rather than
/// re-deriving it, which is the only way that check is worth anything.
struct Columns {
    /// The portrait's width, or zero when there is no room for one.
    art: u16,
    /// Left edge of the portrait, and of the whole block.
    art_x: u16,
    /// Left edge of the text.
    text_x: u16,
    /// What the prose wraps to.
    measure: u16,
    /// What the contact row has, which is the rest of the frame.
    contact: u16,
}

fn columns(area: Rect, a: &About) -> Columns {
    let art = plate(area, a).map_or(0, |p| p.cols);
    let gap = if art > 0 { GAP } else { 0 };
    let room = area.width.saturating_sub(8 + art + gap);
    let measure = MEASURE.min(room);
    // Centred on the block's widest line rather than on the paragraph, so the
    // contact row gets the columns it needs instead of whatever the prose left
    // behind.
    let block = art + gap + measure.max(text_width(a).min(room));
    let art_x = area.x + (area.width.saturating_sub(block)) / 2;
    let text_x = art_x + art + gap;
    Columns {
        art,
        art_x,
        text_x,
        measure,
        contact: (area.x + area.width).saturating_sub(text_x).saturating_sub(1),
    }
}

pub fn render(f: &mut Frame, area: Rect, a: &About, t: f64) {
    if area.width < 24 || area.height < 8 {
        return;
    }
    let face = plate(area, a);
    let c = columns(area, a);
    let (aw, x0, x, w) = (c.art, c.art_x, c.text_x, c.measure);

    let pitch = wrap(&a.pitch, w as usize);
    let now = wrap(&a.now, w as usize);
    // 3 for the name block, the prose, the NOW block if there is one, then the
    // credential line and its gap, the gap and five ways in, and the contact
    // row. Counted rather than guessed because it is what centres the column.
    let body = 3 + pitch.len() + if now.is_empty() { 0 } else { 2 + now.len() } + 2 + 2 + 6;
    let tall = body.max(face.map_or(0, |m| m.rows as usize) + 1);
    let mut y = area.y + ((area.height as usize).saturating_sub(tall) / 2).max(1) as u16;

    if aw > 0 {
        if let Some(m) = face {
            // Loops while somebody is arriving and then holds. See the note
            // in museum.rs: a looping plate is ~136 KB/s for as long as the
            // tab is open, and this is the first screen anybody sees. How long
            // it runs comes from the size of the bake, so a wide window buys a
            // bigger picture rather than a dearer one.
            let frame = crate::paint::portrait_loop(m, t, t < crate::paint::lively_for(m));
            crate::paint::portrait(f, area, x0, y, frame, m.cols);
        }
    }

    let put = |f: &mut Frame, y: u16, spans: Vec<Span<'static>>| {
        if y < area.y + area.height {
            f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y, width: w, height: 1 });
        }
    };

    put(f, y, vec![Span::styled(
        a.name.to_uppercase(),
        Style::default().fg(FG).add_modifier(Modifier::BOLD),
    )]);
    y += 1;
    put(f, y, vec![
        Span::styled(a.role.clone(), Style::default().fg(ACCENT)),
        Span::styled("   ", Style::default()),
        Span::styled(a.where_.clone(), Style::default().fg(DIM)),
    ]);
    y += 2;

    for l in &pitch {
        put(f, y, vec![Span::styled(l.clone(), Style::default().fg(FG))]);
        y += 1;
    }

    if !now.is_empty() {
        y += 1;
        put(f, y, vec![Span::styled("NOW", Style::default().fg(FAINT))]);
        y += 1;
        for l in &now {
            put(f, y, vec![Span::styled(l.clone(), Style::default().fg(DIM))]);
            y += 1;
        }
    }

    y += 1;
    // One line, and only the fact. The badge itself is a picture and lives
    // behind `/cert` in the chat -- a landing page that opens with somebody's
    // certificate is a landing page that is selling, and the rule here is that
    // the visitor asks first.
    put(f, y, vec![
        Span::styled("\u{25c6}  ", Style::default().fg(cert_mark())),
        Span::styled(crate::cert::NAME.to_string(), Style::default().fg(FG)),
        Span::styled(
            format!(" \u{b7} {}", titlecase(crate::cert::TIER)),
            Style::default().fg(FAINT),
        ),
    ]);
    y += 2;

    // The way in. On a server nobody has the keys memorised, and a portfolio
    // that has to be guessed at is one nobody sees past the first screen.
    for (key, label, blurb) in [
        ("1", "experience", "five places on a map you can drive"),
        ("2", "projects", "ten of them, and how they work"),
        ("3", "skills", "the tools"),
        ("4", "taste", "a room you can walk"),
        ("5", "ask", "put a question to the agent on this box"),
    ] {
        put(f, y, vec![
            Span::styled(format!("{key}  "), Style::default().fg(ACCENT)),
            Span::styled(format!("{label:<12}"), Style::default().fg(FG)),
            Span::styled(blurb.to_string(), Style::default().fg(FAINT)),
        ]);
        y += 1;
    }

    y += 1;
    let links = contact_row(a);
    // The contact row is the one line that must not wrap or clip, and three
    // links do not fit the prose measure. It gets the rest of the frame, and
    // `columns` is what guarantees the rest of the frame is enough.
    if y < area.y + area.height {
        f.render_widget(
            Paragraph::new(Line::from(links)),
            Rect { x, y, width: c.contact, height: 1 },
        );
    }
}

/// The practical key map, grouped by where each key works.
pub fn help(f: &mut Frame, area: Rect) {
    let rows: [(&str, &str); 19] = [
        ("navigation", ""),
        ("1 – 6", "open a section; 0 is home too"),
        ("click / esc", "open rail / local back, then home"),
        ("/", "this list outside Ask"),
        ("", ""),
        ("experience", "a map you can actually drive"),
        ("n b / ?", "previous-next place / find one"),
        ("drag / wheel", "pan / zoom; p opens layers"),
        ("projects", ""),
        ("← → / h l", "browse; ↑ ↓ / j k read"),
        ("space / m", "motion / monochrome"),
        ("skills", ""),
        ("drag / wheel", "move; hover inspects a tile"),
        ("", ""),
        ("taste", ""),
        ("← → / wheel", "browse the seamless loop"),
        ("ask", ""),
        ("enter / shift-enter", "send / new line"),
        ("tab / ctrl-alt-⌫", "complete / delete word / menu"),
    ];
    let w = 52.min(area.width.saturating_sub(4));
    let h = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new("").style(Style::default().bg(BG)),
        popup,
    );

    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| {
            if v.is_empty() && !k.is_empty() {
                Line::from(Span::styled(format!("  {k}"), Style::default().fg(ACCENT)))
            } else {
                Line::from(vec![
                    Span::styled(format!("  {k:<18}"), Style::default().fg(FG)),
                    Span::styled(v.to_string(), Style::default().fg(DIM)),
                ])
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::about;

    fn shipped() -> About {
        about::parse(include_str!("../data/about.txt"))
    }

    /// The contact row is the one line on this screen that must not clip, and
    /// it is the last thing to get columns — so it is what breaks when the
    /// portrait grows. It did break: raising the portrait to its larger bake
    /// took the closing bracket off `ssh -p 2222 <this-host>` and nothing
    /// failed. Whenever a portrait is drawn at all, the row it shares the frame
    /// with has to fit.
    #[test]
    fn a_portrait_is_never_wide_enough_to_clip_the_contact_row() {
        let a = shipped();
        // Measured off the row itself, not off the layout's opinion of it —
        // asking `text_width` how much room the row needs would make this test
        // agree with any answer that function gave, including a wrong one.
        let need = row_width(&contact_row(&a));
        assert!(need > MEASURE, "the shipped links now fit a reading measure, \
                                 so this test no longer proves anything");
        for width in 24..=320u16 {
            let c = columns(Rect { x: 0, y: 0, width, height: 50 }, &a);
            if c.art == 0 {
                continue; // no picture is the escape hatch, and it is allowed
            }
            assert!(
                c.contact >= need,
                "at {width} columns the portrait is {} wide and leaves the \
                 contact row {} for {need}",
                c.art,
                c.contact,
            );
        }
    }

    /// And the picture has to actually appear once there is room, or the second
    /// bake is dead weight in the binary.
    #[test]
    fn the_portrait_grows_with_the_window_and_leaves_when_it_cannot_fit() {
        let a = shipped();
        let wide = columns(Rect { x: 0, y: 0, width: 220, height: 50 }, &a);
        let narrow = columns(Rect { x: 0, y: 0, width: 96, height: 50 }, &a);
        assert!(wide.art > 0, "no portrait on a 220-column terminal");
        assert_eq!(narrow.art, 0, "a 96-column terminal still tried to hang one");

        // The prose keeps its measure whatever the picture does: it is a
        // reading column, not a leftover.
        assert_eq!(wide.measure, MEASURE);
    }

    /// A short window has no room for the portrait's rows however wide it is.
    #[test]
    fn a_shallow_window_drops_the_portrait_too() {
        let a = shipped();
        let c = columns(Rect { x: 0, y: 0, width: 240, height: 14 }, &a);
        assert_eq!(c.art, 0, "hung a portrait in 14 rows");
    }

    #[test]
    fn the_key_map_fits_a_standard_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                help(frame, area);
            })
            .unwrap();
        let plain = termap::snapshot::plain(terminal.backend().buffer());
        assert!(plain.contains("1 – 6"));
        assert!(plain.contains("complete / delete word / menu"));
    }
}
