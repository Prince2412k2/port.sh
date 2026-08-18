//! State and input.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::cards::Hit;
use crate::data;
use crate::grid::Sheet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Projects,
    Skills,
}

impl Tab {
    pub const ALL: [Tab; 2] = [Tab::Projects, Tab::Skills];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Projects => "projects",
            Tab::Skills => "skills",
        }
    }
}

/// The speed the sheet settles back to when the drift is switched on, in cells
/// per second.
///
/// Off by default, and that is a bandwidth decision rather than a taste one. A
/// permanently drifting sheet of full-colour tiles repaints the whole screen
/// for ever: measured over a pty it costs ~88 KB/s with nobody touching it,
/// which on a remote connection is the difference between a still page and a
/// page that feels broken. Motion now comes from the reader; `space` turns the
/// drift back on for anyone on a local terminal who wants it.
const REST: (f64, f64) = (0.8, 0.45);

/// How fast a throw bleeds off, per second. Low enough that a flick carries a
/// good way, high enough that the sheet does not run away.
const FRICTION: f64 = 1.35;

/// Cells per second added by one notch of the wheel.
const WHEEL: f64 = 26.0;

/// Below this the sheet is treated as being at rest, so an idle session stops
/// asking for frames instead of chasing an asymptote for ever.
const STILL: f64 = 0.02;

pub struct App {
    pub tab: Tab,

    pub projects: Vec<crate::data::Project>,
    /// Which card is live. Wraps: a carousel with ends is a list.
    pub at: usize,
    pub scroll: u16,
    /// Where the ghosts landed last frame, so they can be clicked.
    pub hit: Hit,

    /// Seconds since start. The only clock in the program.
    pub t: f64,

    pub drift: (f64, f64),
    /// Cells per second. Everything that moves the sheet moves this, and the
    /// drift is integrated from it — which is what makes a flick carry instead
    /// of stopping the moment the pointer does.
    pub vel: (f64, f64),
    /// Pointer within the sheet area, in cells.
    pub cursor: Option<(f64, f64)>,
    pub sheet_area: Rect,

    /// Whether the sheet keeps moving on its own. Off is not a lesser mode —
    /// it is the right one on a slow link, and it costs nothing to offer.
    pub animate: bool,
    pub mono: bool,

    pub quit: bool,
    pub dirty: bool,
    drag: Option<(u16, u16)>,
    /// Pointer travel since the last tick, in cells. Converted to a velocity
    /// there rather than here, because two events in the same frame have no
    /// time between them to divide by.
    thrown: (f64, f64),
}

impl App {
    pub fn new() -> Self {
        // The sheet is compiled in: the program is one file, and a portfolio
        // that cannot find its own content is worse than one that cannot start.
        let projects = data::parse(include_str!("../data/projects.txt"))
            .expect("the built-in project sheet must parse; it is covered by a test");
        App {
            tab: Tab::Projects,
            projects,
            at: 0,
            scroll: 0,
            hit: Hit::default(),
            t: 0.0,
            drift: (0.0, 0.0),
            vel: REST,
            cursor: None,
            sheet_area: Rect::default(),
            animate: false,
            mono: false,
            quit: false,
            dirty: true,
            drag: None,
            thrown: (0.0, 0.0),
        }
    }

    /// The speed the sheet is easing back toward.
    fn rest(&self) -> (f64, f64) {
        if self.animate {
            REST
        } else {
            (0.0, 0.0)
        }
    }

    /// Is anything still in motion? A sheet at rest asks for no frames at all.
    pub fn moving(&self) -> bool {
        // The projects tab always has something going: the mark floats and the
        // tool strip loops. Both are small -- a few hundred cells between them
        // -- so they cost a fraction of what a moving sheet does.
        if self.tab == Tab::Projects {
            return true;
        }
        if self.tab != Tab::Skills {
            return false;
        }
        let r = self.rest();
        self.drag.is_some()
            || (self.vel.0 - r.0).abs() > STILL
            || (self.vel.1 - r.1).abs() > STILL
            || r != (0.0, 0.0)
    }

    /// Advance the sheet. Drift is integrated from velocity rather than derived
    /// from `t`, so holding, dragging and releasing never teleport it.
    pub fn tick(&mut self, dt: f64) {
        // The clock always advances: the projects tab reads it for the float
        // and the strip even though it has no sheet to slide.
        self.t += dt;
        if self.tab == Tab::Projects {
            self.dirty = true;
        }
        if self.tab != Tab::Skills || dt <= 0.0 {
            return;
        }

        if self.drag.is_some() {
            // While a drag is live the sheet is nailed to the pointer, and the
            // velocity is measured from how fast that is happening — so letting
            // go hands the motion over rather than dropping it.
            self.drift.0 -= self.thrown.0;
            self.drift.1 -= self.thrown.1;
            let want = (-self.thrown.0 / dt, -self.thrown.1 / dt);
            // Smoothed, or one stuttery frame at the moment of release decides
            // the whole throw.
            self.vel.0 += (want.0 - self.vel.0) * 0.55;
            self.vel.1 += (want.1 - self.vel.1) * 0.55;
            self.thrown = (0.0, 0.0);
        } else {
            // Exponential, not linear: a throw sheds most of its speed early
            // and then coasts, which is what momentum feels like.
            let k = 1.0 - (-FRICTION * dt).exp();
            let r = self.rest();
            self.vel.0 += (r.0 - self.vel.0) * k;
            self.vel.1 += (r.1 - self.vel.1) * k;
            // Exponential decay never actually arrives. Left alone, a held
            // sheet creeps a fraction of a cell for ever and every frame is
            // different from the last one, so nothing ever stops asking to be
            // redrawn. Close enough is snapped to exact.
            if (self.vel.0 - r.0).abs() < STILL && (self.vel.1 - r.1).abs() < STILL {
                self.vel = r;
            }
        }

        self.drift.0 += self.vel.0 * dt;
        self.drift.1 += self.vel.1 * dt;
        self.dirty = true;
    }

    pub fn sheet(&self) -> Sheet {
        Sheet {
            drift: self.drift,
            cursor: self.cursor,
            w: self.sheet_area.width as f64,
            h: self.sheet_area.height as f64,
        }
    }

    /// Move the carousel. Wraps in both directions.
    pub fn step(&mut self, by: isize) {
        let n = self.projects.len() as isize;
        self.at = ((self.at as isize + by).rem_euclid(n)) as usize;
        self.scroll = 0;
        self.dirty = true;
    }

    fn cycle(&mut self, by: isize) {
        let n = Tab::ALL.len() as isize;
        let at = Tab::ALL.iter().position(|&t| t == self.tab).unwrap_or(0) as isize;
        self.tab = Tab::ALL[((at + by).rem_euclid(n)) as usize];
        self.dirty = true;
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true
            }
            KeyCode::Tab => self.cycle(1),
            KeyCode::BackTab => self.cycle(-1),
            KeyCode::Char('1') => self.tab = Tab::Projects,
            KeyCode::Char('2') => self.tab = Tab::Skills,

            // Left and right belong to the carousel where there is one. On the
            // sheet there is nothing to step through, so they do nothing rather
            // than quietly meaning something else.
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n')
                if self.tab == Tab::Projects =>
            {
                self.step(1)
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('p')
                if self.tab == Tab::Projects =>
            {
                self.step(-1)
            }
            KeyCode::Down | KeyCode::Char('j') if self.tab == Tab::Projects => {
                self.scroll = self.scroll.saturating_add(2)
            }
            KeyCode::Up | KeyCode::Char('k') if self.tab == Tab::Projects => {
                self.scroll = self.scroll.saturating_sub(2)
            }
            KeyCode::PageDown if self.tab == Tab::Projects => {
                self.scroll = self.scroll.saturating_add(10)
            }
            KeyCode::PageUp if self.tab == Tab::Projects => {
                self.scroll = self.scroll.saturating_sub(10)
            }
            KeyCode::Char(' ') => self.animate = !self.animate,
            KeyCode::Char('m') => self.mono = !self.mono,
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        if self.tab == Tab::Projects {
            let inside = |r: Rect| {
                r.width > 0
                    && m.column >= r.x
                    && m.row >= r.y
                    && m.column < r.x + r.width
                    && m.row < r.y + r.height
            };
            match m.kind {
                // A pip is two cells wide, so the click lands on whichever
                // one it is nearest rather than only on the dot itself.
                MouseEventKind::Down(_) if inside(self.hit.pips) => {
                    let i = ((m.column - self.hit.pips.x) / 2) as usize;
                    if i < self.projects.len() {
                        self.at = i;
                        self.scroll = 0;
                        self.dirty = true;
                    }
                }
                MouseEventKind::ScrollDown => {
                    self.scroll = self.scroll.saturating_add(2);
                    self.dirty = true;
                }
                MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_sub(2);
                    self.dirty = true;
                }
                _ => {}
            }
            return;
        }

        let inside = self.sheet_area.width > 0
            && m.column >= self.sheet_area.x
            && m.row >= self.sheet_area.y
            && m.column < self.sheet_area.x + self.sheet_area.width
            && m.row < self.sheet_area.y + self.sheet_area.height;

        match m.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                if let Some(last) = self.drag {
                    // Dragging pushes the sheet under the pointer rather than
                    // moving a camera over it: there is no camera, and no edge
                    // to reach.
                    self.thrown.0 += m.column as f64 - last.0 as f64;
                    self.thrown.1 += m.row as f64 - last.1 as f64;
                    self.drag = Some((m.column, m.row));
                }
                self.cursor = inside.then(|| {
                    (
                        (m.column - self.sheet_area.x) as f64,
                        (m.row - self.sheet_area.y) as f64,
                    )
                });
                self.dirty = true;
            }
            MouseEventKind::Down(_) => {
                self.drag = Some((m.column, m.row));
                self.thrown = (0.0, 0.0);
            }
            MouseEventKind::Up(_) => self.drag = None,
            // A notch of the wheel is a shove, not a jump. It adds to whatever
            // the sheet is already doing, so repeated scrolls build speed.
            MouseEventKind::ScrollDown => {
                self.vel.1 += WHEEL;
                self.dirty = true;
            }
            MouseEventKind::ScrollUp => {
                self.vel.1 -= WHEEL;
                self.dirty = true;
            }
            MouseEventKind::ScrollLeft => {
                self.vel.0 -= WHEEL;
                self.dirty = true;
            }
            MouseEventKind::ScrollRight => {
                self.vel.0 += WHEEL;
                self.dirty = true;
            }
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// The app opens on the projects tab, where there is no sheet to move.
    fn sheet_app() -> App {
        let mut a = App::new();
        a.tab = Tab::Skills;
        a
    }

    #[test]
    fn tab_cycles_both_ways_and_wraps() {
        let mut a = App::new();
        a.tab = Tab::Projects;
        a.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(a.tab, Tab::Skills);
        a.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(a.tab, Tab::Projects, "should wrap");
        a.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(a.tab, Tab::Skills);
    }

    /// Run the loop the way `main` does, in small steps.
    fn advance(a: &mut App, secs: f64) {
        let step: f64 = 1.0 / 30.0;
        let mut left = secs;
        while left > 0.0 {
            a.tick(step.min(left));
            left -= step;
        }
    }

    #[test]
    fn the_sheet_slides_on_its_own_once_asked() {
        let mut a = sheet_app();
        a.animate = true;
        advance(&mut a, 1.0);
        assert!((a.drift.0 - REST.0).abs() < 0.05, "drifted {}", a.drift.0);
        assert!((a.drift.1 - REST.1).abs() < 0.05, "drifted {}", a.drift.1);
    }

    /// The one that matters over a network: left alone, the sheet must stop
    /// moving *and* stop asking to be redrawn.
    #[test]
    fn an_untouched_sheet_goes_quiet() {
        let mut a = sheet_app();
        advance(&mut a, 3.0);
        assert!(!a.moving(), "the sheet never settled");
        let before = a.drift;
        advance(&mut a, 2.0);
        assert_eq!(a.drift, before, "it is still creeping");
    }

    #[test]
    fn holding_it_coasts_to_a_stop_rather_than_freezing() {
        let mut a = sheet_app();
        // Drift on, so the space press is the one that holds it.
        a.animate = true;
        advance(&mut a, 0.5);
        a.on_key(key(' '));

        // It keeps going for a moment: stopping dead is what makes a thing feel
        // like a rendered image rather than something with weight.
        let at_press = a.drift;
        a.tick(1.0 / 30.0);
        assert!(a.drift.0 > at_press.0, "stopped instantly");

        advance(&mut a, 6.0);
        assert!(!a.moving(), "never settled: vel still {:?}", a.vel);
        let settled = a.drift;
        advance(&mut a, 2.0);
        assert_eq!(a.drift, settled, "should stay put once held");
    }

    #[test]
    fn a_scroll_is_a_shove_that_bleeds_off() {
        let mut a = sheet_app();
        a.on_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert!(a.vel.1 > WHEEL * 0.9, "the shove did not land");

        // Most of the speed goes early, then it coasts back to whatever the
        // resting speed is -- a standstill by default, the slow drift if the
        // reader has switched it on.
        let rest = a.rest().1;
        advance(&mut a, 0.5);
        let mid = a.vel.1;
        assert!(mid < WHEEL * 0.6 && mid > rest, "vel {mid}");
        advance(&mut a, 6.0);
        assert!((a.vel.1 - rest).abs() < 0.05, "settled at {}", a.vel.1);
    }

    #[test]
    fn scrolls_stack_instead_of_replacing_each_other() {
        let mut a = sheet_app();
        let notch = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        a.on_mouse(notch);
        let one = a.vel.1;
        a.on_mouse(notch);
        assert!(a.vel.1 < one - WHEEL * 0.9, "second notch did nothing");
    }

    #[test]
    fn letting_go_of_a_drag_hands_over_the_motion() {
        let mut a = sheet_app();
        a.sheet_area = Rect::new(0, 0, 120, 30);
        let at = |c: u16| MouseEvent {
            kind: MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: c,
            row: 10,
            modifiers: KeyModifiers::NONE,
        };
        a.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 60,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        // Hauled left across several frames.
        for c in [54u16, 48, 42, 36] {
            a.on_mouse(at(c));
            a.tick(1.0 / 30.0);
        }
        assert!(a.drift.0 > 40.0, "the sheet did not follow the hand");

        a.on_mouse(MouseEvent {
            kind: MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: 36,
            row: 10,
            modifiers: KeyModifiers::NONE,
        });
        let released = a.drift.0;
        a.tick(1.0 / 30.0);
        assert!(a.drift.0 > released + 1.0, "the throw was dropped on release");
    }

    #[test]
    fn a_settled_sheet_asks_for_no_frames() {
        let mut a = sheet_app();
        a.animate = false;
        advance(&mut a, 6.0);
        assert!(!a.moving(), "the sheet never settled");
    }

    #[test]
    fn the_projects_tab_always_has_something_going() {
        // The mark floats and the tool strip loops, so it never goes quiet --
        // but between them they are a few hundred cells, against a sheet that
        // repaints most of the screen.
        let mut a = App::new();
        a.animate = false;
        advance(&mut a, 6.0);
        assert!(a.moving());
        assert!(a.t > 5.0, "the clock has to run for the float to move");
    }

    #[test]
    fn leaving_the_sheet_releases_the_magnet() {
        let mut a = sheet_app();
        a.sheet_area = Rect::new(0, 2, 80, 20);
        a.on_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 10,
            row: 8,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(a.cursor, Some((10.0, 6.0)));
        a.on_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 10,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(a.cursor, None, "outside the sheet the field is flat");
    }
}
