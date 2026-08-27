//! The taste section, as a room rather than a page.
//!
//! It used to be a shelf: twelve small plates two-up with a caption each, which
//! is a contact sheet — a format for *finding* something, and there is nothing
//! here to find. Eight things, and the point of each is the one line under it.
//!
//! So: one work at a time, as large as the terminal will carry, its quote set
//! underneath, and a field of contour lines behind in the work's own colour.
//! Left and right slide the wall.
//!
//! **What moves, and when.** The selected plate loops while it is fresh and
//! then holds its last frame; any navigation makes it fresh again. That is a
//! bandwidth decision and the arithmetic is not close: a 64x24 plate is 1536
//! cells, roughly half of them change between frames even quantised, and at 8
//! fps that is ~136 KB/s for as long as somebody leaves the tab open. Bounded
//! to the seconds after an arrival it is a few hundred KB and then silence.
//! The same rule governs the field behind it.
//!
//! **How big the picture is depends on the window.** Every plate is baked at
//! two sizes and `portraits::fit` picks the largest this screen can hold, so a
//! maximised terminal gets a picture two to three times the area an 80-column
//! one does. That is also why the loop's *duration* is derived from the plate
//! rather than fixed — see `paint::lively_for`. Otherwise turning up with a
//! big window would silently cost three times as much per arrival.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::paint::{self, wrap, Theme};
use crate::portraits::{self, Portrait};
use crate::taste::{Entry, Sheet};

/// Works per second while sliding. One beat, not a scroll.
const SLIDE: f64 = 3.2;

/// Room between the plate and the terminal's edges.
const PAD: u16 = 2;

/// Rows the room needs for everything that is not the picture: the mount above
/// and below it, the title, the blank line, a line or two of quote, and the
/// index along the bottom.
///
/// Used to decide which bake of a plate fits, which has to happen before the
/// quote is wrapped — the wrap measure comes from the plate's width, so asking
/// for the exact caption height first would be circular. An allowance it is.
const CHROME: u16 = 10;

pub struct Museum {
    works: Vec<Entry>,
    /// Which work is chosen. Always a valid index.
    pub sel: usize,
    /// The chosen work on an unbounded wall. Its modulo is `sel`; keeping the
    /// unbounded coordinate makes the last and first works true neighbours.
    target: i64,
    /// Where the wall actually is, in works.
    pos: f64,
    /// Seconds since the selection last changed.
    fresh: f64,
    t: f64,
}

impl Museum {
    pub fn new(s: &Sheet) -> Museum {
        // Figures and works hang on the same wall. The sheet keeps them apart
        // because it is a document; a room does not have two halves.
        let works: Vec<Entry> = s.figures.iter().chain(&s.works).cloned().collect();
        Museum {
            works,
            sel: 0,
            target: 0,
            pos: 0.0,
            fresh: 0.0,
            t: 0.0,
        }
    }

    pub fn len(&self) -> usize {
        self.works.len()
    }

    pub fn go(&mut self, i: usize) {
        if self.works.is_empty() {
            return;
        }
        let i = i.min(self.works.len() - 1);
        if i != self.sel {
            let n = self.works.len() as i64;
            let here = self.target.rem_euclid(n);
            let forward = (i as i64 - here).rem_euclid(n);
            let backward = forward - n;
            self.target += if forward <= -backward {
                forward
            } else {
                backward
            };
            self.sel = i;
            self.fresh = 0.0;
        }
    }

    /// Go straight to a work with no slide. For snapshots, which have to be a
    /// pure function of the flags that produced them -- `go` alone leaves the
    /// wall easing toward the selection, so a frame drawn immediately after it
    /// still shows whatever was on screen before.
    pub fn jump(&mut self, i: usize) {
        self.go(i);
        self.target = self.sel as i64;
        self.pos = self.target as f64;
    }

    pub fn next(&mut self) {
        if !self.works.is_empty() {
            self.target += 1;
            self.sel = self.target.rem_euclid(self.works.len() as i64) as usize;
            self.fresh = 0.0;
        }
    }

    pub fn prev(&mut self) {
        if !self.works.is_empty() {
            self.target -= 1;
            self.sel = self.target.rem_euclid(self.works.len() as i64) as usize;
            self.fresh = 0.0;
        }
    }

    /// True while anything on this screen is still moving.
    ///
    /// Takes the body rect because how long a plate loops for depends on which
    /// bake of it is on screen, and that depends on the size of the window.
    pub fn moving(&self, area: Rect) -> bool {
        self.sliding() || self.fresh < self.lively_for(area)
    }

    pub fn sliding(&self) -> bool {
        (self.pos - self.target as f64).abs() > 0.001
    }

    /// How long this work's plate should keep looping, at the size it is drawn.
    ///
    /// A still has nothing to loop, so it settles the moment it arrives and
    /// the screen goes quiet — no reason to keep repainting a photograph.
    fn lively_for(&self, area: Rect) -> f64 {
        self.plate_at(self.sel, area).map_or(0.0, paint::lively_for)
    }

    /// The plate for work `i`, ignoring what size it is drawn at.
    ///
    /// For the questions every bake of it answers the same way: is there a
    /// picture, what colour is it. Layout must use `plate_at` instead.
    fn plate(&self, i: usize) -> Option<&'static Portrait> {
        self.works.get(i).and_then(|e| portraits::find(&e.id))
    }

    /// The largest bake of work `i` that this screen can carry, if any.
    fn plate_at(&self, i: usize, area: Rect) -> Option<&'static Portrait> {
        let e = self.works.get(i)?;
        portraits::fit(
            &e.id,
            area.width.saturating_sub(PAD * 2 + 4),
            area.height.saturating_sub(CHROME),
        )
    }

    pub fn tick(&mut self, dt: f64) {
        self.t += dt;
        self.fresh += dt;
        let target = self.target as f64;
        let d = target - self.pos;
        if d.abs() <= 0.001 {
            self.pos = target;
        } else {
            // Eased by distance so it arrives rather than stopping dead, and
            // clamped so a stalled link slides late instead of teleporting.
            let step = (d.signum() * SLIDE * dt).clamp(-d.abs(), d.abs());
            self.pos += step;
        }
    }

    /// The colour of the wall right now, blended across a slide so the room
    /// changes with the work rather than after it.
    fn wall(&self) -> (u8, u8, u8) {
        let n = self.works.len() as i64;
        let floor = self.pos.floor() as i64;
        let lo = floor.rem_euclid(n) as usize;
        let hi = (floor + 1).rem_euclid(n) as usize;
        let k = (self.pos - floor as f64).clamp(0.0, 1.0) as f32;
        let a = self.plate(lo).map_or((128, 128, 128), |p| p.tint);
        let b = self.plate(hi).map_or(a, |p| p.tint);
        let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k) as u8;
        (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
    }

    pub fn pick_index(&mut self, area: Rect, column: u16, row: u16) -> bool {
        let n = self.works.len();
        let width = (n * 2) as u16;
        if width + 2 > area.width || row != area.bottom().saturating_sub(1) {
            return false;
        }
        let x = area.x + (area.width - width) / 2;
        if column < x || column >= x + width {
            return false;
        }
        self.go(((column - x) / 2) as usize);
        true
    }
}

/// Where one work's picture and caption sit, at rest.
///
/// Computed once and used twice, because the two uses must agree: `work` clears
/// this rectangle so the wall does not run through the photograph, and `walls`
/// fades its scene out of the same area so there is nothing at its edge to
/// clear. Two copies of this arithmetic would show up as a scene that stops a
/// column short of the mount, which is precisely the hard line this is all
/// arranged to avoid.
struct Bed {
    /// The plate that is going to be drawn, resolved once.
    plate: Option<&'static Portrait>,
    /// The quote, wrapped to the picture's width.
    lines: Vec<String>,
    /// Columns the caption was wrapped to.
    measure: u16,
    /// First row of the picture.
    top: u16,
    /// Rows from the top of the picture to the bottom of the caption.
    total: u16,
}

impl Museum {
    fn bed(&self, area: Rect, i: usize) -> Bed {
        let e = &self.works[i];
        let quote = format!("\u{201c}{}\u{201d}", e.quote);
        let plate = self.plate_at(i, area);
        // Measured against the plate rather than the terminal: a line the width
        // of a wide screen is one the eye cannot track back from, and the
        // caption reads as belonging to the picture when it shares its edges.
        let pw = plate.map_or(40, |p| p.cols);
        let measure = pw.max(40).min(area.width.saturating_sub(PAD * 2));
        let lines = wrap(&quote, measure as usize);
        let art_h = plate.map_or(0, |p| p.rows);
        // title, blank, quote, blank, index
        let total = art_h + 2 + lines.len() as u16 + 2;
        let top = area.y + (area.height.saturating_sub(total)) / 2;
        Bed { plate, lines, measure, top, total }
    }

    /// The same, as the rectangle to keep clear -- the mount, and the hole in
    /// the wall behind it.
    fn mount(&self, area: Rect, i: usize) -> Rect {
        let b = self.bed(area, i);
        let mw = b.measure.max(b.plate.map_or(0, |p| p.cols)) + 4;
        let centre = area.x as f64 + area.width as f64 / 2.0;
        let x = (centre - mw as f64 / 2.0).max(area.x as f64) as u16;
        let y = b.top.saturating_sub(1);
        Rect {
            x,
            y,
            width: mw.min((area.x + area.width).saturating_sub(x)),
            height: (b.total + 3).min((area.y + area.height).saturating_sub(y)),
        }
    }
}

pub fn render(f: &mut Frame, area: Rect, m: &Museum, th: Theme) {
    if m.works.is_empty() || area.width < 24 || area.height < 12 {
        return;
    }

    // The field goes down first and everything else sits on it.
    field(f, area, m, th);

    let stride = area.width as f64;
    // Only the works that can actually reach the screen. At a stride of one
    // full width that is the one either side, mid-slide.
    let first = m.pos.floor() as i64 - 1;
    for k in 0..3 {
        let virtual_index = first + k;
        let i = virtual_index.rem_euclid(m.works.len() as i64) as usize;
        let dx = (virtual_index as f64 - m.pos) * stride;
        if dx.abs() >= area.width as f64 {
            continue;
        }
        work(f, area, m, i, dx, th);
    }

    index(f, area, m, th);
}

/// One work, offset `dx` columns from centre.
fn work(f: &mut Frame, area: Rect, m: &Museum, i: usize, dx: f64, th: Theme) {
    let e = &m.works[i];

    // Resolved once, in one place, shared with the wall behind it. Every
    // measurement below is against the bake that is going to be drawn, and
    // asking twice would let the caption be laid out for one size and the
    // picture drawn at another.
    let Bed { plate, lines, measure, top, .. } = m.bed(area, i);

    let centre = area.x as f64 + area.width as f64 / 2.0;
    let put = |f: &mut Frame, y: u16, w: u16, spans: Vec<Span<'static>>| {
        let x = centre + dx - w as f64 / 2.0;
        // Off the side of the screen entirely, or clipped into nothing.
        if x + w as f64 <= area.x as f64 || x >= (area.x + area.width) as f64 {
            return;
        }
        let x0 = x.max(area.x as f64) as u16;
        let w = w.min((area.x + area.width).saturating_sub(x0));
        if w == 0 || y >= area.y + area.height {
            return;
        }
        f.render_widget(Paragraph::new(Line::from(spans)), Rect { x: x0, y, width: w, height: 1 });
    };

    // The bed the picture lies on. The wall behind is already faded out of this
    // area -- see `walls` -- so this is not hiding a hard edge, it is giving the
    // caption a surface: text and braille dots in the same cells read as a
    // rendering fault however faint the dots are.
    {
        let bed = m.mount(area, i);
        let x = bed.x as f64 + dx;
        if x >= area.x as f64
            && x + bed.width as f64 <= (area.x + area.width) as f64
            && bed.height > 0
        {
            let r = Rect { x: x as u16, ..bed };
            f.render_widget(ratatui::widgets::Clear, r);
            f.render_widget(Paragraph::new("").style(Style::default().bg(th.page())), r);
        }
    }

    // A work with no picture is hung as wall text rather than as a gap: the
    // quote is the exhibit either way, and an empty frame reads as a bug.
    // Two ways to end up here: a work whose image is not in assets/, and a
    // terminal too small for even the smallest bake of one that is.
    if plate.is_none() {
        let x = centre + dx - measure as f64 / 2.0;
        if x >= area.x as f64 {
            let rule = "\u{2500}".repeat(measure as usize / 3);
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(rule, Style::default().fg(th.ghost())))),
                Rect { x: x as u16, y: top + 1, width: measure, height: 1 },
            );
        }
    }

    if let Some(p) = plate {
        let x = centre + dx - p.cols as f64 / 2.0;
        // The blitter takes unsigned screen coordinates and draws a whole
        // plate, so a half-off-screen one is dropped rather than wrapped
        // around into a very large u16.
        if x >= area.x as f64 && x + p.cols as f64 <= (area.x + area.width) as f64 {
            let frame = if i == m.sel {
                paint::portrait_loop(p, m.fresh, m.fresh < paint::lively_for(p))
            } else {
                p.frames[0]
            };
            paint::portrait(f, area, x as u16, top, frame, p.cols);
        }
    }

    let mut y = top + plate.map_or(0, |p| p.rows) + 1;
    put(f, y, measure, vec![
        Span::styled(e.name.to_uppercase(), Style::default().fg(th.ink()).add_modifier(Modifier::BOLD)),
        Span::styled(format!("   {}", e.from), Style::default().fg(th.ghost())),
    ]);
    y += 2;
    for l in lines {
        put(f, y, measure, vec![Span::styled(
            l,
            Style::default().fg(th.faint()).add_modifier(Modifier::ITALIC),
        )]);
        y += 1;
    }
}

/// Where you are in the collection, as a row of marks.
fn index(f: &mut Frame, area: Rect, m: &Museum, th: Theme) {
    let n = m.works.len();
    let w = (n * 2) as u16;
    if w + 2 > area.width {
        return;
    }
    let x = area.x + (area.width - w) / 2;
    let y = area.y + area.height.saturating_sub(1);
    let spans: Vec<Span> = (0..n)
        .map(|i| {
            if i == m.sel {
                Span::styled("\u{25cf} ", Style::default().fg(th.amber()))
            } else {
                Span::styled("\u{00b7} ", Style::default().fg(th.ghost()))
            }
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { x, y, width: w, height: 1 });
}

/// The wall behind the work: a scene of its own, in the work's colour.
///
/// The scene is `walls`; this decides which one, where the hole in it goes, and
/// how far it has slid. Drawn on termap's subpixel canvas, the same braille the
/// map and the tide are made of, so the room belongs to the same object as the
/// rest of the app.
///
/// It holds still once the plate does -- a wall that keeps raining behind a
/// settled photograph is bandwidth spent on something nobody is looking at.
fn field(f: &mut Frame, area: Rect, m: &Museum, th: Theme) {
    let Some(e) = m.works.get(m.sel) else { return };
    // A name nothing answers to is the plain wall, not an error: this file is
    // content, edited without a rebuild, and a typo in it should cost the
    // scenery rather than the room.
    let wall = crate::walls::Wall::named(&e.wall).unwrap_or(crate::walls::Wall::Contours);

    // Frozen with the plate, so the whole screen goes quiet together.
    let t = if m.moving(area) { m.t } else { 0.0 };
    // How far the wall has slid from its resting place, in cells. The planes
    // divide it between them, so the horizon barely moves while the foreground
    // travels -- which is the only reason a flat terminal can feel deep.
    let drift = (m.sel as f64 - m.pos) * area.width as f64 * 0.5;
    // The same scatter every time for the same work, and a different one for
    // the next: stars that jump when a reader walks back along the wall are
    // worse than no stars.
    let seed = e.id.bytes().fold(0x2545_F491_4F6C_DD1Du64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x1000_0000_01B3)
    });

    if crate::walls::draw(f, area, wall, m.mount(area, m.sel), t, drift, seed) {
        // The canvas draws in greys; the wall's colour is applied over the top
        // so one scene serves every work rather than needing a palette slot
        // each.
        // Stronger than the old field's 0.26. That was a texture nobody was
        // meant to look at; this is a picture, and at a quarter strength every
        // scene read as smudges on an empty screen.
        paint::recolour(f, area, m.wall(), 0.55, th);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taste;

    fn sheet() -> Sheet {
        taste::parse(include_str!("../data/taste.txt"))
    }

    /// A maximised terminal on a normal monitor: room for the large bake of
    /// every plate.
    const BIG: Rect = Rect { x: 0, y: 0, width: 200, height: 60 };
    /// A default xterm. Too short for the small bake and its caption both.
    const SMALL: Rect = Rect { x: 0, y: 0, width: 80, height: 24 };
    /// Wide and shallow: the snapshot size, and the awkward case — plenty of
    /// columns for the large bake and nowhere near the rows for it.
    const WIDE: Rect = Rect { x: 0, y: 0, width: 180, height: 48 };

    #[test]
    fn the_wall_holds_every_entry_from_both_halves_of_the_sheet() {
        let s = sheet();
        let m = Museum::new(&s);
        assert_eq!(m.len(), s.figures.len() + s.works.len());
        assert!(m.len() >= 7, "{}", m.len());
    }

    /// Wrapping matters here: the collection is a loop and the index row shows
    /// it, so `prev` from the first must land on the last rather than clamp.
    #[test]
    fn navigation_wraps_and_marks_the_work_fresh() {
        let s = sheet();
        let mut m = Museum::new(&s);
        let last = m.len() - 1;

        m.prev();
        assert_eq!(m.sel, last);
        m.next();
        assert_eq!(m.sel, 0);

        // Freshness is what keeps a loop running; arriving must restart it.
        m.fresh = 99.0;
        m.next();
        assert_eq!(m.fresh, 0.0);
    }

    #[test]
    fn the_end_and_start_are_one_slide_apart() {
        let s = sheet();
        let mut m = Museum::new(&s);
        let last = m.len() - 1;
        m.jump(last);
        m.next();

        assert_eq!(m.sel, 0);
        assert_eq!(m.target, m.len() as i64);
        assert_eq!(m.target as f64 - m.pos, 1.0);

        for _ in 0..60 {
            m.tick(1.0 / 60.0);
        }
        assert_eq!(m.pos, m.len() as f64);
    }

    /// The slide has to actually finish. An eased approach that only ever
    /// halves the remaining distance would leave `moving()` true for ever and
    /// the section would never stop repainting.
    #[test]
    fn a_slide_settles_exactly_rather_than_approaching_for_ever() {
        let s = sheet();
        let mut m = Museum::new(&s);
        m.go(3);
        for _ in 0..600 {
            m.tick(1.0 / 60.0);
        }
        assert!(!m.sliding(), "still sliding at pos {}", m.pos);
        assert_eq!(m.pos, m.sel as f64);
    }

    /// A photograph has one frame, so there is nothing to loop and the screen
    /// should go quiet as soon as the slide ends.
    #[test]
    fn a_still_work_stops_the_screen_but_an_animated_one_keeps_it_alive() {
        let s = sheet();
        let mut m = Museum::new(&s);

        let animated = (0..m.len())
            .find(|&i| m.plate(i).is_some_and(|p| p.frames.len() > 1))
            .expect("no animated plate in the collection");
        let still = (0..m.len())
            .find(|&i| m.plate(i).is_some_and(|p| p.frames.len() == 1))
            .expect("no still plate in the collection");

        m.go(still);
        for _ in 0..600 {
            m.tick(1.0 / 60.0);
        }
        assert!(!m.moving(BIG), "a photograph is still asking for frames");

        m.go(animated);
        for _ in 0..30 {
            m.tick(1.0 / 60.0);
        }
        assert!(m.moving(BIG), "an animation settled immediately");
    }

    /// `--snapshot --scroll N` has to show work N, not whatever the wall was
    /// easing away from. This was wrong once: `go` alone left `pos` behind and
    /// every snapshot showed the first work whatever was asked for.
    #[test]
    fn a_jump_lands_immediately_so_a_snapshot_shows_what_was_asked_for() {
        let s = sheet();
        let mut m = Museum::new(&s);
        m.jump(5);
        assert_eq!(m.sel, 5);
        assert!(!m.sliding(), "still easing toward it at pos {}", m.pos);
    }

    /// And it must eventually stop, or a tab left open streams for ever.
    #[test]
    fn even_an_animation_settles_in_the_end() {
        let s = sheet();
        let mut m = Museum::new(&s);
        let animated = (0..m.len())
            .find(|&i| m.plate(i).is_some_and(|p| p.frames.len() > 1))
            .expect("no animated plate");
        m.go(animated);
        for _ in 0..((paint::LIVELY as usize + 2) * 60) {
            m.tick(1.0 / 60.0);
        }
        let ceiling = paint::LIVELY;
        assert!(!m.moving(BIG), "still looping after {ceiling}s");
    }

    /// The whole point of baking each plate twice. A wide window has to
    /// actually reach the large bake, or the second bake is dead weight in the
    /// binary.
    #[test]
    fn a_big_window_gets_a_bigger_plate_than_a_small_one() {
        let s = sheet();
        let m = Museum::new(&s);

        for i in 0..m.len() {
            let (Some(big), Some(small)) = (m.plate_at(i, BIG), m.plate_at(i, SMALL)) else {
                continue;
            };
            assert!(
                big.cols as u32 * big.rows as u32 > small.cols as u32 * small.rows as u32,
                "{} is the same size on a 200x60 terminal as on an 80x24 one",
                m.works[i].id,
            );
        }
    }

    /// Rows are the scarce dimension, and this is the case that catches a large
    /// bake being chosen on columns alone: 180x48 has columns to spare and four
    /// rows too few, so it must come back with the small one rather than a
    /// picture whose bottom is off the screen.
    #[test]
    fn a_wide_but_shallow_window_falls_back_rather_than_overflowing() {
        let s = sheet();
        let m = Museum::new(&s);
        for i in 0..m.len() {
            if let Some(p) = m.plate_at(i, WIDE) {
                assert!(
                    p.rows <= WIDE.height - CHROME,
                    "{} chose a {}-row plate for a {}-row window",
                    m.works[i].id,
                    p.rows,
                    WIDE.height,
                );
            }
        }
    }

    /// A windowed browser or a tmux pane has to keep its pictures.
    ///
    /// This is why there is a bake below the one that used to be the only one:
    /// at 36 rows the room has 26 left after the caption, the old plates were
    /// 32, and the four upright works quietly became wall text on a size of
    /// terminal plenty of people actually use.
    #[test]
    fn every_work_still_has_a_picture_on_a_merely_ordinary_terminal() {
        let s = sheet();
        let m = Museum::new(&s);
        let area = Rect { x: 0, y: 0, width: 130, height: 36 };
        for i in 0..m.len() {
            assert!(
                m.plate_at(i, area).is_some(),
                "{} has no picture at 130x36",
                m.works[i].id,
            );
        }
    }

    /// Past a point there is genuinely no room, and then the quote is the
    /// exhibit on its own. The room is built around the line under the picture,
    /// so dropping the line to make space for the picture has it backwards.
    #[test]
    fn a_tiny_window_keeps_the_quote_and_drops_the_picture() {
        let s = sheet();
        let m = Museum::new(&s);
        let area = Rect { x: 0, y: 0, width: 80, height: 20 };
        assert!(
            (0..m.len()).all(|i| m.plate_at(i, area).is_none()),
            "something still claimed to fit in 20 rows",
        );
    }

    /// Nothing is ever scaled, so a bake that does not fit is a bake drawn off
    /// the side of the screen. Whatever comes back has to fit what was asked
    /// for, at every size.
    #[test]
    fn a_chosen_bake_always_fits_what_was_measured_for_it() {
        let s = sheet();
        let m = Museum::new(&s);
        for height in 12..=72u16 {
            for width in [40, 60, 80, 100, 130, 176, 200, 240] {
                let area = Rect { x: 0, y: 0, width, height };
                for i in 0..m.len() {
                    let Some(p) = m.plate_at(i, area) else { continue };
                    assert!(
                        p.cols + PAD * 2 + 4 <= width && p.rows + CHROME <= height,
                        "{} chose {}x{} for a {width}x{height} room",
                        m.works[i].id,
                        p.cols,
                        p.rows,
                    );
                }
            }
        }
    }
}
