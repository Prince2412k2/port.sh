//! The reading surface: a block of text in the top-left corner of the frame.
//!
//! There is no side panel and no box. Opening a project flies the camera into
//! its constellation and *offsets* it, so the ring of skills sits in the open
//! space to the right while the words go in the corner. The two are not in
//! separate regions of the interface; they are in the same picture.
//!
//! Left-aligned and ragged-right, because it is prose. Centring a paragraph
//! costs the reader the return sweep — the left edge stops being a place the
//! eye can fall back to — and a description five lines long is exactly long
//! enough for that to be felt.
//!
//! Everything here writes through the canvas overlay layer rather than over the
//! top of the finished frame, so the text takes part in the same resolve as the
//! stars and can be faded, tinted and blocked out of the label placer by the
//! same machinery.

use crate::app::App;
use crate::canvas::{Canvas, Overlay, TINT_CON, TINT_DIM, TINT_MONO, TINT_SELECT};

/// Bounds on the text column, in cells. The lower bound is where prose stops
/// being worth setting at all; the upper is where a line gets long enough that
/// the eye starts losing its place on the way back.
const MIN_W: usize = 34;
const MAX_W: usize = 58;

/// Share of the frame the text may take, so the constellation keeps the rest.
const SHARE: f64 = 0.42;

/// Where the block sits, in cells from the top-left corner.
pub const AT: (usize, usize) = (3, 1);

/// Cells of clear space kept around the block on every side.
const HALO: usize = 4;

/// How much of the sky survives at each cell of distance out from the text.
///
/// A hard-edged hole reads as a box drawn on the sky, which is the one thing
/// this composition is trying not to be — there is no panel, so there must be
/// no panel *shape*. Four steps is enough for the ramp to read as the star
/// field thinning out on approach rather than as an edge with a gradient
/// painted on it.
const CLEARING: [f32; 5] = [0.0, 0.10, 0.30, 0.58, 0.82];

pub struct Row {
    pub text: String,
    pub tint: u8,
    pub lum: f32,
    pub bold: bool,
}

impl Row {
    fn new(text: impl Into<String>, tint: u8, lum: f32) -> Self {
        Row { text: text.into(), tint, lum, bold: false }
    }
}

/// Accumulates rows already broken to the column.
///
/// Every row goes through here rather than being pushed directly, because the
/// card's width is not a suggestion: the camera is placed from it, and one row
/// that overruns puts a word on top of a star the framing thought it had
/// cleared.
struct Build {
    rows: Vec<Row>,
    w: usize,
}

impl Build {
    fn new(w: usize) -> Self {
        Build { rows: Vec::new(), w }
    }

    fn text(&mut self, s: &str, tint: u8, lum: f32) -> &mut Self {
        for l in wrap(s, self.w) {
            self.rows.push(Row::new(l, tint, lum));
        }
        self
    }

    /// A single line that must not be broken.
    fn line(&mut self, s: &str, tint: u8, lum: f32) -> &mut Self {
        self.rows.push(Row::new(elide(s, self.w), tint, lum));
        self
    }

    fn bold(&mut self) -> &mut Self {
        if let Some(r) = self.rows.last_mut() {
            r.bold = true;
        }
        self
    }

    fn gap(&mut self) -> &mut Self {
        self.rows.push(Row::new("", TINT_MONO, 0.0));
        self
    }
}

/// Cut to width, marking that something was cut. Never silently truncates: a
/// name that merely stops looks like the name.
fn elide(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return String::new();
    }
    s.chars().take(width - 1).collect::<String>() + "…"
}

pub struct Card {
    pub rows: Vec<Row>,
    /// Column width the rows were wrapped to.
    pub w: usize,
}

impl Card {
    pub fn h(&self) -> usize {
        self.rows.len()
    }

    /// The cell rectangle the block occupies, halo included. This is what the
    /// camera is told to stay out of.
    pub fn rect(&self) -> (usize, usize, usize, usize) {
        (
            AT.0.saturating_sub(HALO),
            AT.1.saturating_sub(HALO),
            self.w + HALO * 2,
            self.h() + HALO * 2,
        )
    }
}

/// What belongs in the middle of the frame right now, if anything.
///
/// The order is the order of the reader's attention: a skill they opened, else
/// the project they opened, else the search they are running. The whole sky
/// with nothing chosen gets nothing — an empty sky is the one state that is
/// supposed to look empty.
pub fn build(app: &App, avail_w: usize, avail_h: usize) -> Option<Card> {
    let w = ((avail_w as f64 * SHARE) as usize)
        .clamp(MIN_W, MAX_W)
        .min(avail_w.saturating_sub(AT.0 + 2))
        .max(16);

    if let Some(s) = app.selected {
        return Some(skill(app, s, w));
    }
    if let Some(c) = app.focus {
        return Some(project(app, c, w, avail_h));
    }
    if !app.query.is_empty() {
        return Some(results(app, w));
    }
    None
}

/// Is there room for the long version?
///
/// On a short terminal the full description is most of the screen, and a
/// constellation squeezed into what is left is not a constellation. Dropping
/// the paragraph is the right trade: the one-line version and the numbers still
/// say what the project is, and the sky gets to be a sky.
fn roomy(rows: usize, avail_h: usize) -> bool {
    avail_h >= 30 && rows + AT.1 + HALO * 2 <= avail_h / 2 + 6
}

fn project(app: &App, con: usize, w: usize, avail_h: usize) -> Card {
    let c = &app.sky.cons[con];
    let tint = TINT_CON + con as u8;
    let repo = c.repo.strip_prefix("github.com/").unwrap_or(&c.repo);
    let n = app.sky.members_of(con).len();

    let mut b = Build::new(w);
    b.line(&c.name, tint, 1.0).bold();
    b.gap();
    b.text(&c.blurb, TINT_MONO, 0.95);
    b.gap();
    let long = wrap(&c.about, w);
    if roomy(long.len() + 9, avail_h) {
        b.text(&c.about, TINT_MONO, 0.62);
        b.gap();
    }
    b.text(&c.stats, TINT_DIM, 0.85);
    b.text(&format!("{}  ·  {repo}", c.year), TINT_DIM, 0.72);
    b.gap();
    b.text(
        &format!("{n} skills around it  ·  n and p to walk them"),
        TINT_DIM,
        0.55,
    );

    Card { rows: b.rows, w }
}

fn skill(app: &App, star: usize, w: usize) -> Card {
    let s = &app.sky.stars[star];

    // Where it was learned, then everywhere else it turned up — in that order,
    // because the story below is told about the first one.
    let mut from = String::new();
    for (n, (&m, &load)) in s.members.iter().zip(&s.load).enumerate() {
        if n > 0 {
            from.push_str("  ·  ");
        }
        from.push_str(&app.sky.cons[m].name);
        if load {
            from.push('*');
        }
    }

    let claims = s.members.len();
    let bearing = s.load.iter().filter(|&&b| b).count();
    let pips = (s.magnitude() * 5.0).round() as usize;

    let mut b = Build::new(w);
    b.line(&s.name, TINT_SELECT, 1.0).bold();
    b.text(&from, TINT_CON + s.home() as u8, 0.70);
    b.gap();
    // Never shortened, unlike a project's description. The story *is* the skill
    // as far as this program is concerned; there is nothing else to fall back
    // on. If it does not fit the terminal it overflows and scrolls, and the
    // renderer marks the bottom edge to say so.
    b.text(&s.story, TINT_MONO, 0.90);
    b.gap();
    b.text(
        &format!(
            "{}{}   {claims} project{}, {bearing} lean{} on it",
            "\u{25cf}".repeat(pips.min(5)),
            "\u{25cb}".repeat(5 - pips.min(5)),
            if claims == 1 { "" } else { "s" },
            if bearing == 1 { "s" } else { "" },
        ),
        TINT_DIM,
        0.60,
    );

    Card { rows: b.rows, w }
}

fn results(app: &App, w: usize) -> Card {
    let mut b = Build::new(w);
    b.line(&format!("/{}", app.query), TINT_SELECT, 0.95).bold();
    b.text(
        match app.matches.len() {
            0 => "nothing".to_string(),
            1 => "1 skill".to_string(),
            n => format!("{n} skills"),
        }
        .as_str(),
        TINT_DIM,
        0.65,
    );
    b.gap();

    const SHOWN: usize = 12;
    for &m in app.matches.iter().take(SHOWN) {
        let star = &app.sky.stars[m];
        let name = elide(&star.name, w.saturating_sub(10));
        let home = &app.sky.cons[star.home()].name;
        // Right-aligned project against left-aligned skill, so the two read as
        // columns without a rule between them.
        let used = name.chars().count() + home.chars().count();
        let pad = w.saturating_sub(used).max(2);
        b.rows.push(Row::new(
            format!("{name}{}{home}", " ".repeat(pad)),
            TINT_MONO,
            0.85,
        ));
    }
    // Say what was cut, rather than ending on a list that looks complete.
    if app.matches.len() > SHOWN {
        b.text(
            &format!("\u{2026} and {} more", app.matches.len() - SHOWN),
            TINT_DIM,
            0.55,
        );
    }
    if !app.matches.is_empty() {
        b.gap();
        b.text("enter opens the first", TINT_DIM, 0.55);
    }

    Card { rows: b.rows, w }
}

/// Paint the card into the middle of the canvas. Returns the cell rectangle it
/// claimed, halo included, so the label placer can be told to keep out.
///
/// `scroll` drops rows off the top, for the rare card taller than the terminal.
pub fn draw(c: &mut Canvas, card: &Card, scroll: usize) -> (usize, usize, usize, usize) {
    let w = card.w.min(c.cw);
    let rows: &[Row] = {
        let top = scroll.min(card.rows.len().saturating_sub(1));
        &card.rows[top..]
    };
    let h = rows.len().min(c.ch.saturating_sub(AT.1));
    let (x0, y0) = AT;

    // Clear a well, then let it come back gradually. A hard-edged hole reads as
    // a box drawn on the sky; a ramp reads as the sky thinning out.
    let (bx, by) = (x0.saturating_sub(HALO), y0.saturating_sub(HALO));
    let (bw, bh) = (w + HALO * 2, h + HALO * 2);
    for cy in by..(by + bh).min(c.ch) {
        for cx in bx..(bx + bw).min(c.cw) {
            let inside_x = cx >= x0 && cx < x0 + w;
            let inside_y = cy >= y0 && cy < y0 + h;
            let ring = if inside_x && inside_y {
                0
            } else {
                let dx = if cx < x0 { x0 - cx } else { cx.saturating_sub(x0 + w - 1) };
                let dy = if cy < y0 { y0 - cy } else { cy.saturating_sub(y0 + h - 1) };
                dx.max(dy)
            };
            c.fade_cell(cx, cy, CLEARING[ring.min(CLEARING.len() - 1)]);
        }
    }

    // Say when there is more below, rather than ending mid-paragraph on
    // something that looks like the end.
    let clipped = rows.len() > h;
    for (i, row) in rows.iter().enumerate() {
        if i >= h {
            break;
        }
        if clipped && i + 1 == h {
            for (j, ch) in "▾ pgdn".chars().enumerate() {
                c.set_overlay(x0 + j, y0 + i, Overlay { ch, tint: TINT_DIM, lum: 0.75, bold: false });
            }
            break;
        }
        if row.text.is_empty() {
            continue;
        }
        for (j, ch) in row.text.chars().enumerate() {
            if x0 + j >= c.cw {
                break;
            }
            c.set_overlay(
                x0 + j,
                y0 + i,
                Overlay { ch, tint: row.tint, lum: row.lum, bold: row.bold },
            );
        }
    }

    (bx, by, bw, bh)
}

/// Greedy word wrap. A word longer than the column overflows rather than being
/// hyphenated: the only things that long in here are URLs, and a broken URL is
/// worse than a ragged edge.
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
    fn elide_marks_what_it_cut() {
        assert_eq!(elide("Dart · TypeScript", 8), "Dart · …");
        assert_eq!(elide("Go", 8), "Go");
        assert_eq!(elide("Go", 1), "");
    }

    #[test]
    fn wrap_respects_the_column() {
        let t = "the quick brown fox jumps over the lazy dog";
        for w in [8, 12, 20, 40] {
            for l in wrap(t, w) {
                assert!(l.chars().count() <= w, "{l:?} exceeds {w}");
            }
        }
    }

    #[test]
    fn wrap_keeps_every_word_once() {
        let t = "alpha beta gamma delta epsilon";
        assert_eq!(wrap(t, 11).join(" "), t);
    }

    #[test]
    fn wrap_lets_a_long_word_overflow_rather_than_breaking_it() {
        let out = wrap("github.com/Prince2412k2/netjail x", 10);
        assert_eq!(out[0], "github.com/Prince2412k2/netjail");
        assert_eq!(out[1], "x");
    }

    #[test]
    fn every_card_fits_the_column_it_was_given() {
        let mut a = crate::app::App::new(include_str!("../data/skills.sky")).unwrap();
        for w in [40usize, 70, 120] {
            for c in 0..a.sky.cons.len() {
                a.focus = Some(c);
                a.selected = None;
                let card = build(&a, w, 40).unwrap();
                for r in &card.rows {
                    assert!(r.text.chars().count() <= card.w, "{:?} in {}", r.text, w);
                }
            }
            for s in 0..a.sky.stars.len() {
                a.selected = Some(s);
                let card = build(&a, w, 40).unwrap();
                for r in &card.rows {
                    assert!(r.text.chars().count() <= card.w, "{:?} in {}", r.text, w);
                }
            }
        }
    }

    #[test]
    fn a_short_terminal_gets_the_short_version() {
        let mut a = crate::app::App::new(include_str!("../data/skills.sky")).unwrap();
        a.focus = a.sky.con_by_id("netjail");
        let tall = build(&a, 120, 44).unwrap();
        let short = build(&a, 120, 24).unwrap();
        assert!(short.h() < tall.h(), "{} vs {}", short.h(), tall.h());
        // Whatever else goes, the name and the one-liner stay.
        assert_eq!(short.rows[0].text, "netjail");
        assert!(short.h() * 2 < 24, "still {} rows of 24", short.h());
    }

    #[test]
    fn an_untouched_sky_gets_no_card() {
        let a = crate::app::App::new(include_str!("../data/skills.sky")).unwrap();
        assert!(build(&a, 100, 40).is_none());
    }
}
