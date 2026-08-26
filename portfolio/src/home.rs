//! The landing section.
//!
//! Deliberately the quietest screen in the app. The other three are animated,
//! dense and doing something; if this one competed with them it would be noise
//! before anyone had read a word. It is a name, a paragraph, and a way in.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use crate::about::About;
use crate::paint::{wrap, Theme};

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

/// The contact block: where to find him, then how to get in.
///
/// Two rows rather than one, and the split is not cosmetic. Every one of these
/// is a line that must not wrap or clip, so `text_width` reserves the widest of
/// them out of the frame and the portrait gets whatever is left. On one row the
/// four links come to about a hundred columns, which on any ordinary terminal
/// left no room for a picture at all -- the drawing quietly disappeared to make
/// space for a line of text. Split, the widest row is nearer fifty, the reading
/// measure wins, and the portrait comes back.
///
/// They also read better apart. An address and a repository are where somebody
/// finds him; `ssh` and `mosh` are the two doors into this very program, and
/// putting them on their own line says so without a word of explanation.
///
/// Built in one place because two things need it -- the rows themselves, and
/// the layout that has to leave room for them -- and a layout that measured the
/// links separately from the way they are drawn would be one edit away from
/// disagreeing with itself.
fn contact_rows(a: &About, th: Theme) -> Vec<Vec<Span<'static>>> {
    let joined = |parts: [&String; 2]| -> Vec<Span<'static>> {
        let mut spans: Vec<Span> = Vec::new();
        for s in parts.iter().filter(|s| !s.is_empty()) {
            if !spans.is_empty() {
                spans.push(Span::styled("   ·   ", Style::default().fg(th.ghost())));
            }
            spans.push(Span::styled((*s).clone(), Style::default().fg(th.cyan())));
        }
        spans
    };
    [joined([&a.github, &a.email]), joined([&a.ssh, &a.mosh])]
        .into_iter()
        .filter(|r| !r.is_empty())
        .collect()
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
fn text_width(a: &About, th: Theme) -> u16 {
    MEASURE.max(contact_rows(a, th).iter().map(|r| row_width(r)).max().unwrap_or(0))
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
pub fn plate(area: Rect, a: &About, th: Theme) -> Option<&'static crate::portraits::Portrait> {
    crate::portraits::fit(
        "snufkin-home",
        area.width.saturating_sub(text_width(a, th) + GAP + 8),
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

fn columns(area: Rect, a: &About, th: Theme) -> Columns {
    let art = plate(area, a, th).map_or(0, |p| p.cols);
    let gap = if art > 0 { GAP } else { 0 };
    let room = area.width.saturating_sub(8 + art + gap);
    let measure = MEASURE.min(room);
    // Centred on the block's widest line rather than on the paragraph, so the
    // contact row gets the columns it needs instead of whatever the prose left
    // behind.
    let block = art + gap + measure.max(text_width(a, th).min(room));
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

pub fn render(f: &mut Frame, area: Rect, a: &About, t: f64, th: Theme) {
    if area.width < 24 || area.height < 8 {
        return;
    }
    let face = plate(area, a, th);
    let c = columns(area, a, th);
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
            crate::paint::portrait(f, area, x0, y, frame, m.cols, th);
        }
    }

    let put = |f: &mut Frame, y: u16, spans: Vec<Span<'static>>| {
        if y < area.y + area.height {
            f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y, width: w, height: 1 });
        }
    };

    put(f, y, vec![Span::styled(
        a.name.to_uppercase(),
        Style::default().fg(th.ink()).add_modifier(Modifier::BOLD),
    )]);
    y += 1;
    put(f, y, vec![
        Span::styled(a.role.clone(), Style::default().fg(th.amber())),
        Span::styled("   ", Style::default()),
        Span::styled(a.where_.clone(), Style::default().fg(th.faint())),
    ]);
    y += 2;

    for l in &pitch {
        put(f, y, vec![Span::styled(l.clone(), Style::default().fg(th.ink()))]);
        y += 1;
    }

    if !now.is_empty() {
        y += 1;
        put(f, y, vec![Span::styled("NOW", Style::default().fg(th.ghost()))]);
        y += 1;
        for l in &now {
            put(f, y, vec![Span::styled(l.clone(), Style::default().fg(th.faint()))]);
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
        Span::styled(crate::cert::NAME.to_string(), Style::default().fg(th.ink())),
        Span::styled(
            format!(" \u{b7} {}", titlecase(crate::cert::TIER)),
            Style::default().fg(th.ghost()),
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
            Span::styled(format!("{key}  "), Style::default().fg(th.amber())),
            Span::styled(format!("{label:<12}"), Style::default().fg(th.ink())),
            Span::styled(blurb.to_string(), Style::default().fg(th.ghost())),
        ]);
        y += 1;
    }

    y += 1;
    // None of these may wrap or clip. They get the rest of the frame, and
    // `columns` is what guarantees the rest of the frame is enough.
    for row in contact_rows(a, th) {
        if y >= area.y + area.height {
            break;
        }
        f.render_widget(
            Paragraph::new(Line::from(row)),
            Rect { x, y, width: c.contact, height: 1 },
        );
        y += 1;
    }
}

/// The key map, grouped by where each key works.
///
/// Grouped rather than listed, because the question somebody opens this with is
/// "what can I do *here*" and not "what keys exist". The first group is the one
/// that works everywhere; the rest are named after the section they belong to.
type Group = (&'static str, &'static [(&'static str, &'static str)]);

const KEYS: [Group; 6] = [
    (
        "anywhere",
        &[
            ("1 – 6", "open a section"),
            ("0", "home"),
            ("esc", "back, then home"),
            ("click", "the rail up top"),
            ("/", "this list"),
            ("q", "quit"),
        ],
    ),
    (
        "experience",
        &[
            ("drag / wheel", "pan / zoom"),
            ("n / b", "next place / back"),
            ("?", "find a place"),
            ("p", "layers"),
        ],
    ),
    (
        "projects",
        &[
            ("← → / h l", "browse"),
            ("↑ ↓ / j k", "read"),
            ("space / m", "motion / monochrome"),
        ],
    ),
    (
        "skills",
        &[("drag / wheel", "move"), ("hover", "inspects a tile")],
    ),
    ("taste", &[("← → / wheel", "browse the loop")]),
    (
        "ask",
        &[
            ("enter", "send"),
            ("shift-enter", "a new line"),
            ("tab", "complete a command"),
            ("shift ← →", "walk the route"),
        ],
    ),
];

/// One group after another, and how wide that came out.
///
/// The key column is measured per column rather than across the whole panel:
/// `shift-enter` is eleven characters and `/` is one, and one width for both
/// pushes every short key in the list a finger's width away from what it does.
fn column(groups: &[Group], th: Theme) -> (Vec<Line<'static>>, u16) {
    let keys = groups
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    let mut width = 0;
    for (i, (head, rows)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        width = width.max(head.chars().count());
        lines.push(Line::from(Span::styled(
            *head,
            Style::default().fg(th.amber()).add_modifier(Modifier::BOLD),
        )));
        for (k, v) in rows.iter() {
            width = width.max(2 + keys + 2 + v.chars().count());
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<keys$}  "), Style::default().fg(th.ink())),
                Span::styled(*v, Style::default().fg(th.faint())),
            ]));
        }
    }
    (lines, width as u16)
}

pub fn help(f: &mut Frame, area: Rect, th: Theme) {
    // Two columns where there is room, because one column is thirty-one rows
    // and the terminal this has to fit is twenty-four. Where to split is a
    // search over five places rather than a number written down here, which is
    // the only version of it that survives someone adding a key.
    //
    // Both constraints, not just the width. Checking the width alone is what it
    // did first, and the split that fit sideways was four groups against two --
    // so the panel was taller than the screen and quietly lost the last three
    // rows off the bottom, which is the failure the whole thing exists to fix.
    let gutter = 3u16;
    let room = area.width.saturating_sub(6);
    let tallest = area.height.saturating_sub(4);
    let mut fits: Option<(usize, u16)> = None;
    let mut nearest: Option<(usize, u16)> = None;
    for at in 1..KEYS.len() {
        let (left, lw) = column(&KEYS[..at], th);
        let (right, rw) = column(&KEYS[at..], th);
        if lw + gutter + rw > room {
            continue;
        }
        let tall = left.len().max(right.len()) as u16;
        if nearest.is_none_or(|(_, t)| tall < t) {
            nearest = Some((at, tall));
        }
        if tall <= tallest && fits.is_none_or(|(_, t)| tall < t) {
            fits = Some((at, tall));
        }
    }

    // Nothing that fits both ways gets the one that clips least, which is still
    // better than a single column and much better than the widest one.
    let (columns, inner_w, inner_h) = match fits.or(nearest) {
        Some((at, tall)) => {
            let (left, lw) = column(&KEYS[..at], th);
            let (right, rw) = column(&KEYS[at..], th);
            (vec![(left, lw), (right, rw)], lw + gutter + rw, tall)
        }
        None => {
            let (only, w) = column(&KEYS, th);
            let h = only.len() as u16;
            (vec![(only, w)], w, h)
        }
    };

    // The frame is two rows and two columns, and there is a row of air inside
    // it: a list that starts on the border reads as a list that has been cut
    // off at the top.
    let w = (inner_w + 4).min(area.width);
    let h = (inner_h + 4).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, popup);

    let frame = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.ghost()))
        .style(Style::default().bg(th.page()))
        .title(Span::styled(
            " keys ",
            Style::default().fg(th.amber()).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(" esc closes ", Style::default().fg(th.faint()))).right_aligned(),
        );
    let inside = frame.inner(popup);
    f.render_widget(frame, popup);

    let mut x = inside.x + 1;
    for (lines, width) in columns {
        let at = Rect {
            x,
            y: inside.y + 1,
            width: width.min(inside.right().saturating_sub(x)),
            height: inside.height.saturating_sub(1),
        };
        f.render_widget(Paragraph::new(lines), at);
        x += width + gutter;
    }
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
        let need = contact_rows(&a, Theme::default()).iter().map(|r| row_width(r)).max().expect("rows");
        for width in 24..=320u16 {
            let c = columns(Rect { x: 0, y: 0, width, height: 50 }, &a, Theme::default());
            if c.art == 0 {
                continue; // no picture is the escape hatch, and it is allowed
            }
            assert!(
                c.contact >= need,
                "at {width} columns the portrait is {} wide and leaves the \
                 contact rows {} for {need}",
                c.art,
                c.contact,
            );
        }
        // And the reason they were split: on one line these came to about a
        // hundred columns, which reserved the whole frame and left the
        // portrait nothing. Apart, they fit the reading measure and the
        // picture is free to use what is left.
        let together: u16 = contact_rows(&a, Theme::default()).iter().map(|r| row_width(r)).sum::<u16>() + 7;
        assert!(
            together > MEASURE && need <= MEASURE,
            "split rows are {need} wide and would be {together} on one line, \
             against a reading measure of {MEASURE}"
        );
    }

    /// And the picture has to actually appear once there is room, or the second
    /// bake is dead weight in the binary.
    #[test]
    fn the_portrait_grows_with_the_window_and_leaves_when_it_cannot_fit() {
        let a = shipped();
        let wide = columns(Rect { x: 0, y: 0, width: 220, height: 50 }, &a, Theme::default());
        let narrow = columns(Rect { x: 0, y: 0, width: 96, height: 50 }, &a, Theme::default());
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
        let c = columns(Rect { x: 0, y: 0, width: 240, height: 14 }, &a, Theme::default());
        assert_eq!(c.art, 0, "hung a portrait in 14 rows");
    }

    /// Every key in the list is on the screen, at the size that has the least
    /// screen to give.
    ///
    /// The panel lays itself out -- it picks a split, and the split decides how
    /// tall it is -- so the way it fails is not an error but a missing row. It
    /// chose a four-against-two split once because that was the only one narrow
    /// enough, went five rows past the bottom of an eighty-by-twenty-four
    /// terminal, and lost `hover`, `taste` and half of `ask` with nothing to say
    /// so. Reading the keys back off the buffer is the only check that notices.
    #[test]
    fn the_key_map_fits_a_standard_terminal() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        for (w, h) in [(80, 24), (100, 30), (120, 40), (200, 56)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    help(frame, area, Theme::default());
                })
                .unwrap();
            let plain = termap::snapshot::plain(terminal.backend().buffer());

            for (head, rows) in KEYS {
                assert!(plain.contains(head), "{w}x{h}: `{head}` is missing:\n{plain}");
                for (key, what) in rows {
                    assert!(plain.contains(key), "{w}x{h}: `{key}` is missing:\n{plain}");
                    assert!(
                        plain.contains(what),
                        "{w}x{h}: `{key}` has lost `{what}`:\n{plain}"
                    );
                }
            }

            // And the frame closed, which is the other half of "it fits": a
            // panel taller than the screen loses its bottom border first.
            assert!(
                plain.contains('╰') && plain.contains('╯'),
                "{w}x{h}: the panel has no bottom:\n{plain}"
            );
        }
    }
}
