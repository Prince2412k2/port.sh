//! State and input.

use crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::canvas::{Canvas, SUB_X, SUB_Y};
use crate::data::{parse, Sky};
use crate::layout::{solve, Layout};
use crate::sky::View;

/// Subpixels of margin left around the sheet when fitting the whole sky.
///
/// Subpixels, not cells — 26 of these is three rows of a forty-row terminal at
/// the top and three at the bottom, and a fit is height-constrained far more
/// often than it is width-constrained, so a generous margin here is paid for
/// entirely out of the zoom.
const FIT_PAD: f64 = 14.0;

/// Subpixels of air between the text block and the constellation, and around
/// the frame's edge.
const GUTTER: f64 = 10.0;

/// How much wider than its ring a constellation actually is, once the stars it
/// shares with other projects have pulled outward. Framing to the ring alone
/// puts every one of them off the edge at once.
const SPREAD: f64 = 1.32;

/// Subpixels a star's name is likely to need beyond the star itself.
const LABEL_ROOM: f64 = 22.0;

/// What a placed label points at.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Hit {
    Star(usize),
    Con(usize),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Browse,
    /// Typing a query. The sky filters live, so this is not a modal dialog so
    /// much as a lens being adjusted.
    Search,
    Help,
}

pub struct App {
    pub sky: Sky,
    pub lay: Layout,
    pub view: View,

    pub mode: Mode,
    pub selected: Option<usize>,
    pub hover: Option<usize>,
    /// A constellation's own name, under the cursor. Names are the only thing
    /// big enough to aim at when the whole sky is in frame.
    pub hover_con: Option<usize>,
    pub focus: Option<usize>,
    pub query: String,
    pub matches: Vec<usize>,
    /// Stars in walking order: by constellation, then brightest first.
    order: Vec<usize>,

    pub mono: bool,
    pub dust: bool,
    pub figures: bool,
    pub story_scroll: u16,

    /// Kept across frames because hover is resolved against the pick buffer
    /// the *previous* frame wrote — the cursor moves between draws, and
    /// re-rasterising the sky just to answer "what is under here" would cost
    /// a frame to save a lookup.
    pub canvas: Canvas,

    /// Cell runs of text placed last frame, as (x, y, len, feature), so a name
    /// can be clicked. A star at wide zoom is two subpixels across and its
    /// label is fifteen cells; asking the reader to hit the star is asking
    /// them to hit the wrong target.
    pub label_hits: Vec<(u16, u16, u16, u32)>,

    /// Where the sky is drawn, so mouse cells can be turned into subpixels.
    pub sky_area: Rect,
    pub cursor: Option<(u16, u16)>,
    drag: Option<((u16, u16), bool)>,

    /// A project to fly into, once it is known how much room its description
    /// needs. The camera is placed by the typography, not the other way round,
    /// so this cannot be resolved until the card has been laid out.
    pending_focus: Option<usize>,

    /// A star that must be on screen once the camera has settled.
    pending_reveal: Option<usize>,

    /// A framing that cannot be computed yet, because it depends on how large
    /// the sky area turns out to be. Resolved on the next draw.
    ///
    /// Fitting eagerly is the bug it exists to prevent: at the moment a key is
    /// pressed the viewport still holds the *previous* frame's size, and on the
    /// very first frame it holds no size at all.
    pending_fit: Option<([f64; 2], [f64; 2], f64)>,

    pub quit: bool,
    /// Set by anything that changes what a frame would look like. The sky does
    /// not animate, so redrawing on a timer would burn a core per SSH session
    /// to send zero bytes.
    pub dirty: bool,
}

impl App {
    pub fn new(sheet: &str) -> Result<Self, String> {
        let sky = parse(sheet)?;
        let lay = solve(&sky);

        let mut order: Vec<usize> = (0..sky.stars.len()).collect();
        order.sort_by(|&a, &b| {
            let sa = &sky.stars[a];
            let sb = &sky.stars[b];
            sa.home().cmp(&sb.home()).then_with(|| {
                sb.magnitude()
                    .partial_cmp(&sa.magnitude())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        Ok(App {
            sky,
            lay,
            view: View::new(),
            mode: Mode::Browse,
            selected: None,
            hover: None,
            hover_con: None,
            focus: None,
            query: String::new(),
            matches: Vec::new(),
            order,
            mono: false,
            dust: true,
            figures: true,
            story_scroll: 0,
            canvas: Canvas::new(1, 1),
            label_hits: Vec::new(),
            sky_area: Rect::default(),
            cursor: None,
            drag: None,
            pending_focus: None,
            pending_reveal: None,
            pending_fit: None,
            quit: false,
            dirty: true,
        }
        .opened())
    }

    fn opened(mut self) -> Self {
        self.fit_all();
        self
    }

    /// Resolve what the cursor is over, against the last frame that was drawn.
    ///
    /// Text first, then the pick buffer. A label is a far larger target than
    /// the star it names, and at wide zoom a project's name is the only thing
    /// on screen big enough to aim at at all.
    pub fn update_hover(&mut self) {
        let was = (self.hover, self.hover_con);
        self.hover = None;
        self.hover_con = None;

        if let Some((cx, cy)) = self.cursor {
            for &(x, y, len, feat) in &self.label_hits {
                if cy == y && cx >= x && cx < x + len {
                    match self.decode(feat) {
                        Some(Hit::Star(i)) => self.hover = Some(i),
                        Some(Hit::Con(i)) => self.hover_con = Some(i),
                        None => {}
                    }
                    break;
                }
            }
            if self.hover.is_none() && self.hover_con.is_none() {
                self.hover = self
                    .canvas
                    .pick_near(cx as usize, cy as usize, 3)
                    .map(|id| id as usize)
                    .filter(|&i| i < self.sky.stars.len());
            }
        }

        if was != (self.hover, self.hover_con) {
            self.dirty = true;
        }
    }

    /// Labels carry a star index, or a constellation counted down from the top
    /// of the range so the two can never be confused for one another.
    fn decode(&self, feature: u32) -> Option<Hit> {
        if feature == u32::MAX {
            return None;
        }
        let con_floor = u32::MAX - 1 - self.sky.cons.len() as u32;
        if feature > con_floor {
            return Some(Hit::Con((u32::MAX - 1 - feature) as usize));
        }
        ((feature as usize) < self.sky.stars.len()).then_some(Hit::Star(feature as usize))
    }

    pub fn active_matches(&self) -> Option<&[usize]> {
        (!self.query.is_empty()).then_some(self.matches.as_slice())
    }

    pub fn fit_all(&mut self) {
        self.pending_fit = Some((self.lay.min, self.lay.max, FIT_PAD));
        self.dirty = true;
    }

    /// Resolve a deferred framing, now that the sky area is known.
    pub fn apply_pending(&mut self) {
        if let Some((min, max, pad)) = self.pending_fit.take() {
            self.view.fit(min, max, pad);
        }
    }

    /// Anything that moves the camera by hand outranks a framing that has not
    /// been applied yet.
    pub fn take_the_wheel(&mut self) {
        self.pending_fit = None;
        self.pending_focus = None;
        self.pending_reveal = None;
    }

    /// Place the camera so a project's constellation sits in the space its own
    /// description left over.
    ///
    /// The typography decides the framing, not the other way round. The ring
    /// has a known size in sky units — it is what the layout springs rest at —
    /// so once the text block has claimed its corner, there is one scale and
    /// one offset that put the figure in what remains, centred in that space
    /// rather than in the frame.
    ///
    /// Two compositions, chosen by how much room is left beside the text. On a
    /// wide terminal the constellation goes to its right; on a narrow one it
    /// goes underneath, because a column of sky forty cells wide is not a
    /// constellation, it is a stripe.
    ///
    /// Stars further out than the ring — the ones shared with distant projects
    /// — are allowed off the edge. Their figure lines still point at them, and
    /// that is the more honest picture: they genuinely are somewhere else too.
    pub fn frame_project(&mut self, con: usize, reserved: (usize, usize, usize, usize)) {
        let (rx, ry, rw, rh) = reserved;
        let (sw, sh) = (self.view.sw, self.view.sh);
        let right = ((rx + rw) * SUB_X) as f64;
        let below = ((ry + rh) * SUB_Y) as f64;

        let beside = sw - right - GUTTER;
        let under = sh - below - GUTTER;
        // Which arm of the L is bigger. A short block — a skill's story, or a
        // project on a wide terminal — leaves most of the frame below it, and
        // squeezing the constellation into the column beside it would waste
        // two-thirds of the sky. A tall one leaves a column and nothing else.
        let (cx, cy, aw, ah) = if under >= sh * 0.55 {
            (sw * 0.5, below + under * 0.5, sw - GUTTER * 2.0, under)
        } else if beside >= sw * 0.50 {
            // Slightly left of centre and slightly low: even here the free
            // space is an L rather than a column, and the figure belongs in
            // the corner of the L rather than the middle of its long arm.
            (
                right + beside * 0.46,
                sh * 0.56,
                beside,
                sh - GUTTER * 2.0,
            )
        } else {
            // Neither: the terminal is small enough that the text and the sky
            // have to share the same ground. Pushed down and right so the
            // overlap lands where the text is thinnest, and the clearing under
            // the words fades whatever ends up behind them.
            (
                sw * 0.60,
                sh * 0.60,
                sw - GUTTER * 2.0,
                sh - GUTTER * 2.0,
            )
        };

        // Room for the ring, the stars outside it, and — the part that is easy
        // to forget — the names. A skill's label is fifteen cells of text hung
        // off a two-subpixel star, so the thing being framed is a good deal
        // wider than the geometry says it is.
        let aw = (aw - LABEL_ROOM * 2.0).max(aw * 0.55);
        let sx = aw / (2.0 * crate::layout::REST_X * SPREAD);
        let sy = ah / (2.0 * crate::layout::REST_Y * crate::sky::PIXEL_ASPECT * SPREAD);
        self.view.zoom = sx
            .min(sy)
            .max(1e-6)
            .log2()
            .clamp(crate::sky::MIN_ZOOM, crate::sky::MAX_ZOOM);
        self.view.place(self.sky.cons[con].at, [cx, cy]);
    }

    /// Open a project: fly into its ring and set its description in the middle.
    pub fn focus_con(&mut self, con: usize) {
        if con >= self.sky.cons.len() {
            return;
        }
        self.focus = Some(con);
        // Opening a project shows the project. Leaving the previously-read
        // skill's story up would answer a question nobody just asked.
        self.selected = None;
        self.story_scroll = 0;
        self.pending_fit = None;
        self.pending_focus = Some(con);
        self.dirty = true;
    }

    /// The project waiting to be framed, if any. Taken by the renderer once it
    /// knows how much room the description needs.
    pub fn take_pending_focus(&mut self) -> Option<usize> {
        self.pending_focus.take()
    }

    /// Open a skill: whichever project taught it, with that skill's story up.
    ///
    /// One gesture rather than two. Clicking a star in the wide sky and getting
    /// a paragraph floating over an unrelated patch of nothing is worse than
    /// arriving inside the constellation the paragraph is about.
    pub fn open_star(&mut self, star: usize) {
        if star >= self.sky.stars.len() {
            return;
        }
        let home = self.sky.stars[star].home();
        let known = self.focus.is_some_and(|f| self.sky.stars[star].members.contains(&f));
        if !known {
            self.focus_con(home);
        }
        self.select(Some(star));
    }

    pub fn select(&mut self, star: Option<usize>) {
        self.pending_fit = None;
        if self.selected != star {
            self.story_scroll = 0;
        }
        // Crossing between a project's description and a skill's story changes
        // the text block from sixteen rows to seven, which moves where the free
        // space is — so the framing is worked out again. Walking from one skill
        // to the next does not: those differ by a line or two, and a camera
        // that shuffles on every keypress is worse than one that is slightly
        // off-optimal.
        if self.selected.is_none() != star.is_none() {
            if let Some(c) = self.focus {
                self.pending_focus = Some(c);
            }
        }
        self.selected = star;
        // Deferred, like the framing: whether a star is on screen depends on a
        // camera that has not been placed yet this frame.
        self.pending_reveal = star;
        self.dirty = true;
    }

    /// Nudge the camera, if the star just chosen ended up outside the frame.
    pub fn reveal_pending(&mut self) {
        if let Some(i) = self.pending_reveal.take() {
            self.ensure_visible(i);
        }
    }

    /// Pan the least amount that brings a star into the frame.
    ///
    /// Walking a project's skills reaches the ones it shares with projects on
    /// the other side of the sky, and those sit outside the framing. Without
    /// this the reader presses `n`, the words change, and nothing lights up.
    fn ensure_visible(&mut self, star: usize) {
        let p = self.view.project(self.lay.pos[star]);
        let (mx, my) = (28.0, 20.0);
        let dx = (p[0] - (self.view.sw - mx)).max(0.0) + (p[0] - mx).min(0.0);
        let dy = (p[1] - (self.view.sh - my)).max(0.0) + (p[1] - my).min(0.0);
        if dx != 0.0 || dy != 0.0 {
            self.view.pan(dx, dy);
        }
    }

    /// Move through the sky one star at a time, staying inside the focus if
    /// there is one.
    fn step(&mut self, delta: isize) {
        let pool: Vec<usize> = match self.focus {
            Some(f) => self
                .order
                .iter()
                .copied()
                .filter(|&s| self.sky.stars[s].members.contains(&f))
                .collect(),
            None => self.order.clone(),
        };
        if pool.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|s| pool.iter().position(|&p| p == s))
            .map(|i| (i as isize + delta).rem_euclid(pool.len() as isize) as usize)
            .unwrap_or(if delta >= 0 { 0 } else { pool.len() - 1 });
        let star = pool[at];
        // Inside a project the camera stays put: the ring is already framed and
        // the reader is watching one star of it light up while the middle of
        // the frame changes under them. Moving would undo both.
        if self.focus.is_some() {
            self.select(Some(star));
        } else {
            self.open_star(star);
        }
    }

    /// Set the search and rebuild the result set.
    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.refresh_matches();
        self.dirty = true;
    }

    fn refresh_matches(&mut self) {
        let q = self.query.trim().to_lowercase();
        self.matches.clear();
        if q.is_empty() {
            return;
        }

        // Ranked, not just filtered. Stories are searched too, which is most of
        // the value — "drop" finds nftables, "median" finds Theil-Sen — but it
        // also means a two-letter query like "go" hits every "algorithm" and
        // "going" in the sheet. Without an order the one star actually called
        // Go is buried in prose that merely contains it.
        let mut hits: Vec<(u8, usize)> = Vec::new();
        for (i, star) in self.sky.stars.iter().enumerate() {
            let name = star.name.to_lowercase();
            let rank = if name == q {
                0
            } else if name.starts_with(&q) || star.id.to_lowercase().starts_with(&q) {
                1
            } else if name.contains(&q) || star.id.to_lowercase().contains(&q) {
                2
            } else if star
                .members
                .iter()
                .any(|&m| self.sky.cons[m].name.to_lowercase().contains(&q))
            {
                3
            } else if star.story.to_lowercase().contains(&q) {
                4
            } else {
                continue;
            };
            hits.push((rank, i));
        }
        // Stable within a rank, so equally-good matches stay in sheet order
        // and the list does not reshuffle as you type.
        hits.sort_by_key(|&(r, _)| r);
        self.matches = hits.into_iter().map(|(_, i)| i).collect();
    }

    /// Back out one layer at a time, rather than dropping straight to nothing.
    fn escape(&mut self) {
        if self.mode != Mode::Browse {
            self.mode = Mode::Browse;
        } else if !self.query.is_empty() {
            self.query.clear();
            self.matches.clear();
        } else if self.selected.is_some() {
            self.selected = None;
        } else if self.focus.is_some() {
            self.focus = None;
            self.fit_all();
        }
        self.dirty = true;
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        self.dirty = true;

        if self.mode == Mode::Search {
            // Ctrl-C has to work from inside the search too. Everything else
            // here is a character, so without this the only key that always
            // means "stop" would silently become part of the query.
            if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                self.quit = true;
                return;
            }
            match k.code {
                KeyCode::Esc => {
                    self.query.clear();
                    self.matches.clear();
                    self.mode = Mode::Browse;
                }
                // Enter keeps the filter and hands the keyboard back, so the
                // result can be walked with n/p without retyping anything.
                KeyCode::Enter => {
                    self.mode = Mode::Browse;
                    if let Some(&first) = self.matches.first() {
                        self.open_star(first);
                    }
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.refresh_matches();
                }
                // Modified keys are chords, not text. Terminals report
                // alt-anything as a plain character with a flag, so without
                // the guard a stray escape sequence types itself into the
                // query and the user cannot see why.
                KeyCode::Char(c)
                    if !k
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.query.push(c);
                    self.refresh_matches();
                }
                _ => {}
            }
            return;
        }

        let pan = 12.0;
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Esc => self.escape(),

            KeyCode::Char('?') => {
                self.mode = if self.mode == Mode::Help { Mode::Browse } else { Mode::Help };
            }

            KeyCode::Char('h') | KeyCode::Left => {
                self.take_the_wheel();
                self.view.pan(-pan, 0.0);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.take_the_wheel();
                self.view.pan(pan, 0.0);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.take_the_wheel();
                self.view.pan(0.0, -pan);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.take_the_wheel();
                self.view.pan(0.0, pan);
            }

            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.take_the_wheel();
                let a = [self.view.sw * 0.5, self.view.sh * 0.5];
                self.view.zoom_at(a, 0.45);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.take_the_wheel();
                let a = [self.view.sw * 0.5, self.view.sh * 0.5];
                self.view.zoom_at(a, -0.45);
            }

            KeyCode::Char('g') => {
                self.focus = None;
                self.fit_all();
            }
            KeyCode::Char('0') => {
                self.focus = None;
                self.fit_all();
            }
            KeyCode::Char(d @ '1'..='9') => {
                let i = d as usize - '1' as usize;
                if self.focus == Some(i) {
                    self.focus = None;
                    self.fit_all();
                } else {
                    self.focus_con(i);
                }
            }

            KeyCode::Tab | KeyCode::Char('n') => self.step(1),
            KeyCode::BackTab | KeyCode::Char('p') => self.step(-1),
            KeyCode::Enter => {
                if let Some(c) = self.hover_con {
                    self.focus_con(c);
                } else if let Some(h) = self.hover {
                    self.open_star(h);
                } else if self.selected.is_none() {
                    self.step(1);
                }
            }

            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.query.clear();
                self.matches.clear();
            }

            KeyCode::Char('s') => self.dust = !self.dust,
            KeyCode::Char('f') => self.figures = !self.figures,
            KeyCode::Char('m') => self.mono = !self.mono,

            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.story_scroll = self.story_scroll.saturating_add(5)
            }
            KeyCode::PageUp => self.story_scroll = self.story_scroll.saturating_sub(5),

            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let in_sky = contains(self.sky_area, m.column, m.row);

        match m.kind {
            MouseEventKind::Moved => {
                if in_sky {
                    self.cursor = Some((m.column - self.sky_area.x, m.row - self.sky_area.y));
                } else if self.cursor.is_some() {
                    self.cursor = None;
                    self.hover = None;
                }
                self.dirty = true;
            }
            MouseEventKind::Down(MouseButton::Left) if in_sky => {
                self.drag = Some(((m.column, m.row), false));
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((last, _)) = self.drag {
                    let dx = m.column as f64 - last.0 as f64;
                    let dy = m.row as f64 - last.1 as f64;
                    if dx != 0.0 || dy != 0.0 {
                        self.take_the_wheel();
                        self.view.pan(-dx * SUB_X as f64, -dy * SUB_Y as f64);
                        self.drag = Some(((m.column, m.row), true));
                        self.dirty = true;
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // A click is a drag that never moved. Testing the flag rather
                // than the distance means a shaky hand still pans instead of
                // silently selecting whatever it passed over.
                if let Some((_, moved)) = self.drag.take() {
                    if !moved && in_sky {
                        match (self.hover_con, self.hover) {
                            (Some(c), _) => self.focus_con(c),
                            (None, Some(s)) => self.open_star(s),
                            (None, None) => {}
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let dz = if matches!(m.kind, MouseEventKind::ScrollUp) { 0.32 } else { -0.32 };
                let anchor = match (in_sky, self.cursor) {
                    (true, Some((cx, cy))) => [
                        (cx as f64 + 0.5) * SUB_X as f64,
                        (cy as f64 + 0.5) * SUB_Y as f64,
                    ],
                    _ => [self.view.sw * 0.5, self.view.sh * 0.5],
                };
                self.take_the_wheel();
                self.view.zoom_at(anchor, dz);
                self.dirty = true;
            }
            _ => {}
        }
    }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut a = App::new(include_str!("../data/skills.sky")).unwrap();
        a.view.sw = 320.0;
        a.view.sh = 184.0;
        a.fit_all();
        a.apply_pending();
        a
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn stepping_from_a_cold_sky_opens_a_project() {
        let mut a = app();
        assert_eq!(a.focus, None);
        a.on_key(key('n'));
        // Walking with nothing open has to land somewhere, and a story floating
        // over an unrelated patch of sky is not somewhere.
        let s = a.selected.unwrap();
        assert_eq!(a.focus, Some(a.sky.stars[s].home()));
    }

    #[test]
    fn stepping_covers_the_open_project_and_wraps() {
        let mut a = app();
        let con = a.sky.con_by_id("netjail").unwrap();
        a.focus_con(con);
        let members = a.sky.members_of(con);

        let mut seen = std::collections::HashSet::new();
        for _ in 0..members.len() {
            a.on_key(key('n'));
            seen.insert(a.selected.unwrap());
        }
        assert_eq!(seen.len(), members.len(), "did not reach every skill");
        // One more wraps rather than falling off the end or escaping the
        // project.
        a.on_key(key('n'));
        assert!(seen.contains(&a.selected.unwrap()));
        assert_eq!(a.focus, Some(con));
    }

    #[test]
    fn opening_a_skill_shared_with_the_open_project_stays_put() {
        let mut a = app();
        let netjail = a.sky.con_by_id("netjail").unwrap();
        a.focus_con(netjail);
        let go = a.sky.star_by_id("go").unwrap();
        // `go` is taught by netjail but claimed by four projects. Opening it
        // from inside netjail must not throw the reader into logify.
        a.open_star(go);
        assert_eq!(a.focus, Some(netjail));
        assert_eq!(a.selected, Some(go));
    }

    #[test]
    fn opening_a_skill_from_the_sky_goes_where_it_was_learned() {
        let mut a = app();
        let s = a.sky.star_by_id("theil-sen").unwrap();
        a.open_star(s);
        assert_eq!(a.focus, Some(a.sky.con_by_id("watch-party").unwrap()));
        assert_eq!(a.selected, Some(s));
    }

    #[test]
    fn stepping_inside_a_focus_stays_inside_it() {
        let mut a = app();
        let con = a.sky.con_by_id("netjail").unwrap();
        a.focus_con(con);
        a.apply_pending();
        for _ in 0..40 {
            a.on_key(key('n'));
            let s = a.selected.unwrap();
            assert!(a.sky.stars[s].members.contains(&con));
        }
    }

    #[test]
    fn escape_backs_out_one_layer_at_a_time() {
        let mut a = app();
        a.focus_con(0);
        a.select(Some(3));
        a.set_query("go".into());

        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(a.query.is_empty());
        assert_eq!(a.selected, Some(3), "the query should go before the star");

        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.selected, None);
        assert_eq!(a.focus, Some(0), "the star should go before the focus");

        a.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(a.focus, None);
    }

    #[test]
    fn search_matches_names_stories_and_projects() {
        let mut a = app();
        a.set_query("nftables".into());
        assert!(a.matches.contains(&a.sky.star_by_id("nftables").unwrap()));

        // A project name finds everything that project claims.
        a.set_query("Noter".into());
        let noter = a.sky.con_by_id("noter").unwrap();
        for &m in &a.matches {
            assert!(
                a.sky.stars[m].members.contains(&noter)
                    || a.sky.stars[m].story.to_lowercase().contains("noter")
            );
        }
        assert!(a.matches.len() >= a.sky.members_of(noter).len());
    }

    #[test]
    fn an_exact_name_outranks_the_prose_that_mentions_it() {
        let mut a = app();
        a.set_query("go".into());
        let go = a.sky.star_by_id("go").unwrap();
        assert_eq!(
            a.matches.first(),
            Some(&go),
            "matched {:?}",
            a.matches
                .iter()
                .take(4)
                .map(|&m| a.sky.stars[m].name.as_str())
                .collect::<Vec<_>>()
        );
        // and the prose hits are still there, just behind it
        assert!(a.matches.len() > 1);
    }

    #[test]
    fn search_finds_a_skill_by_something_only_its_story_says() {
        let mut a = app();
        a.set_query("corduroy".into());
        assert_eq!(a.matches.len(), 1);
        assert_eq!(a.sky.stars[a.matches[0]].id, "dithering");
    }

    #[test]
    fn a_repeated_digit_toggles_the_focus_off() {
        let mut a = app();
        a.on_key(key('3'));
        assert_eq!(a.focus, Some(2));
        a.on_key(key('3'));
        assert_eq!(a.focus, None);
    }

    #[test]
    fn search_takes_text_but_not_chords() {
        let mut a = app();
        a.on_key(key('/'));
        assert_eq!(a.mode, Mode::Search);
        for c in "netns".chars() {
            a.on_key(key(c));
        }
        assert_eq!(a.query, "netns");

        // alt-x is a chord that happens to arrive as a character
        a.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        assert_eq!(a.query, "netns");

        // and ctrl-c still means stop, even mid-query
        a.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.quit);
    }

    #[test]
    fn enter_leaves_the_search_and_opens_the_best_match() {
        let mut a = app();
        a.on_key(key('/'));
        for c in "nftables".chars() {
            a.on_key(key(c));
        }
        a.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(a.mode, Mode::Browse);
        assert_eq!(a.selected, a.sky.star_by_id("nftables"));
        // and it took us to the project the story is about
        assert_eq!(a.focus, a.sky.con_by_id("netjail"));
        // the filter survives, so n/p walks the results
        assert_eq!(a.query, "nftables");

        a.on_key(key('q'));
        assert!(a.quit);
    }

    #[test]
    fn walking_never_selects_a_star_you_cannot_see() {
        let mut a = app();
        for c in 0..a.sky.cons.len() {
            a.focus_con(c);
            a.apply_pending();
            if let Some(con) = a.take_pending_focus() {
                a.frame_project(con, (0, 0, 46, 18));
            }
            a.reveal_pending();
            for _ in 0..a.sky.members_of(c).len() + 2 {
                a.on_key(key('n'));
                if let Some(con) = a.take_pending_focus() {
                    a.frame_project(con, (0, 0, 46, 18));
                }
                a.reveal_pending();
                let s = a.selected.unwrap();
                let p = a.view.project(a.lay.pos[s]);
                assert!(
                    p[0] >= 0.0 && p[0] <= a.view.sw && p[1] >= 0.0 && p[1] <= a.view.sh,
                    "{} landed at {p:?} outside {}x{}",
                    a.sky.stars[s].id,
                    a.view.sw,
                    a.view.sh
                );
            }
        }
    }

    #[test]
    fn selecting_a_new_star_resets_the_story_scroll() {
        let mut a = app();
        a.select(Some(1));
        a.story_scroll = 9;
        a.select(Some(2));
        assert_eq!(a.story_scroll, 0);
    }

    #[test]
    fn focusing_a_project_frames_most_of_what_it_taught() {
        let mut a = app();
        for c in 0..a.sky.cons.len() {
            a.focus_con(c);
            a.apply_pending();
            let taught: Vec<usize> = a
                .sky
                .members_of(c)
                .into_iter()
                .filter(|&s| a.sky.stars[s].home() == c)
                .collect();
            let framed = taught
                .iter()
                .filter(|&&s| {
                    let p = a.view.project(a.lay.pos[s]);
                    p[0] >= 0.0 && p[1] >= 0.0 && p[0] <= a.view.sw && p[1] <= a.view.sh
                })
                .count();
            // Not all of them: a skill shared with a distant project is
            // deliberately left hanging off the edge. But most, or the framing
            // is showing the gaps between projects rather than a project.
            assert!(
                framed * 2 >= taught.len(),
                "{}: only {framed} of {} own skills framed",
                a.sky.cons[c].id,
                taught.len()
            );
        }
    }

    #[test]
    fn focusing_does_not_pin_the_camera_to_a_limit() {
        let mut a = app();
        for c in 0..a.sky.cons.len() {
            a.focus_con(c);
            a.apply_pending();
            assert!(
                a.view.zoom > crate::sky::MIN_ZOOM && a.view.zoom < crate::sky::MAX_ZOOM,
                "{} fitted to z{:.2}",
                a.sky.cons[c].id,
                a.view.zoom
            );
        }
    }

    #[test]
    fn fit_frames_every_star() {
        let mut a = app();
        a.fit_all();
        a.apply_pending();
        for p in &a.lay.pos {
            let s = a.view.project(*p);
            assert!(s[0] >= 0.0 && s[0] <= a.view.sw, "{s:?}");
            assert!(s[1] >= 0.0 && s[1] <= a.view.sh, "{s:?}");
        }
    }
}
