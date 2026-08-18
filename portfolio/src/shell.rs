//! The one app: four sections, one set of chrome, one event loop.
//!
//! Each section is a view over something that already exists. Experience drives
//! `termap`'s renderer and its scripted camera; Projects and Skills drive
//! `skysheet`'s. The shell owns the rail, the footer, the transition and the
//! four keys it needs, and forwards every other keystroke to whichever section
//! has the screen — so the map still pans with `hjkl` and the sheet still
//! slides, without either of them knowing it has been embedded.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget};
use ratatui::Frame;

use crate::about::{self, About};
use crate::ask::{self, Ask};
use crate::context;
use crate::home;
use crate::page::Page;
use crate::paint::{self, ACCENT, BG, DIM, FAINT, FG};
use crate::taste;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Home,
    Experience,
    Projects,
    Skills,
    Taste,
    Ask,
}

impl Section {
    pub const ALL: [Section; 6] = [
        Section::Home,
        Section::Experience,
        Section::Projects,
        Section::Skills,
        Section::Taste,
        Section::Ask,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Section::Home => "home",
            Section::Experience => "experience",
            Section::Projects => "projects",
            Section::Skills => "skills",
            Section::Taste => "taste",
            Section::Ask => "ask",
        }
    }

    /// What the footer offers here. Every section has different verbs and a
    /// single global hint line would be wrong in three places out of four.
    fn hints(self) -> &'static str {
        match self {
            Section::Home => "tab  move between sections",
            Section::Experience => {
                "n b  next / previous place    drag  pan    wheel  zoom    ?  map keys"
            }
            Section::Projects => "← →  browse projects",
            Section::Skills => "drag / wheel  slide the sheet    hover  raise a tile",
            Section::Taste => "↑ ↓  read    space  page    home / end",
            Section::Ask => "type a question    enter  send    esc  clear    ctrl-c  quit",
        }
    }
}

/// How long the cross-dissolve between sections takes.
const SWITCH: f64 = 0.28;

pub struct Shell {
    pub section: Section,
    pub about: About,
    pub map: termap::app::App,
    pub sheet: skysheet::app::App,
    pub quit: bool,
    pub page: Page,
    pub ask: Ask,
    /// Rows scrolled into the essay, and the velocity carrying it. Fractional
    /// so momentum can decay smoothly; only the whole part is ever drawn.
    scroll: f64,
    vel: f64,
    /// Seconds left in the transition, counting down.
    switch: f64,
    /// Where the body was last drawn, so mouse events can be made local to it.
    body: Rect,
    show_help: bool,
    /// Everything the agent is told, assembled once.
    context: String,
}

impl Shell {
    pub fn new() -> Self {
        let sheet_taste = taste::load();
        let mut map = termap::app::App::new(termap::tiles::Source::open(None));
        // The experience section is the tour; there is no other reason for a
        // portfolio to open a map. It arms itself on the first frame, once the
        // viewport knows how many subpixels wide it is.
        map.start_tour(0);

        let mut shell = Shell {
            section: Section::Home,
            about: about::load(),
            page: Page::build(&sheet_taste),
            ask: Ask::new(),
            scroll: 0.0,
            vel: 0.0,
            map,
            sheet: skysheet::app::App::new(),
            quit: false,
            switch: 0.0,
            body: Rect::default(),
            show_help: false,
            context: String::new(),
        };
        shell.context = context::build(&shell.about, &sheet_taste, &shell.sheet.projects);
        shell
    }

    pub fn go(&mut self, s: Section) {
        if s == self.section {
            return;
        }
        // Entering Projects or Skills sets the sheet's own tab, so the two
        // sections are one renderer wearing two hats rather than two states
        // that can disagree.
        match s {
            Section::Projects => self.sheet.tab = skysheet::app::Tab::Projects,
            Section::Skills => self.sheet.tab = skysheet::app::Tab::Skills,
            _ => {}
        }
        // The essay opens at the top each time. Returning to it halfway down
        // where you left it sounds considerate and reads as a bug.
        if s == Section::Taste {
            self.scroll = 0.0;
            self.vel = 0.0;
        }
        if s == Section::Ask {
            self.ask.wake(&self.context);
        }
        self.section = s;
        self.switch = SWITCH;
    }

    fn step(&mut self, by: i32) {
        let n = Section::ALL.len() as i32;
        let i = Section::ALL.iter().position(|s| *s == self.section).unwrap_or(0) as i32;
        self.go(Section::ALL[(i + by).rem_euclid(n) as usize]);
    }

    /// True while anything is animating and the loop must keep drawing.
    pub fn animating(&self) -> bool {
        self.switch > 0.0
            || match self.section {
                Section::Experience => self.map.animating(),
                Section::Projects | Section::Skills => self.sheet.moving(),
                Section::Taste => self.vel.abs() > 0.01,
                // Only while the tide is running. Idle, the screen is static
                // and the stream still gets polled on the slow heartbeat --
                // asking for 40 frames a second to render a blinking caret is
                // how a portfolio ends up warming someone's laptop.
                Section::Ask => self.ask.busy(),
                Section::Home => false,
            }
    }

    pub fn tick(&mut self, dt: f64) {
        if self.switch > 0.0 {
            self.switch = (self.switch - dt).max(0.0);
        }
        // Only the visible section advances. A map flying in the background
        // burns frames nobody is watching, and over SSH that is bandwidth.
        match self.section {
            Section::Experience => self.map.tick(dt),
            Section::Projects | Section::Skills => self.sheet.tick(dt),
            Section::Taste => self.scroll_tick(dt),
            Section::Ask => self.ask.tick(dt),
            Section::Home => {}
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        // Ctrl-C and the section keys are the shell's wherever you are. `q` is
        // not: in the chat it is a letter, and a portfolio that quits when you
        // type "what does netjail do" is a portfolio nobody uses twice.
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
                return;
            }
            KeyCode::Tab => {
                self.step(1);
                return;
            }
            KeyCode::BackTab => {
                self.step(-1);
                return;
            }
            _ => {}
        }
        if self.section == Section::Ask {
            self.ask.on_key(k);
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            _ => match self.section {
                Section::Home => self.home_key(k),
                Section::Experience => self.map.on_key(k),
                Section::Taste => self.scroll_key(k),
                Section::Ask => {}
                Section::Projects | Section::Skills => {
                    self.sheet.on_key(k);
                    // The sheet owns its own tab switching; if it changed tabs
                    // under us, the rail has to follow or the two disagree.
                    self.section = match self.sheet.tab {
                        skysheet::app::Tab::Projects => Section::Projects,
                        skysheet::app::Tab::Skills => Section::Skills,
                    };
                }
            },
        }
    }

    /// How far the essay can go: far enough to read the last line, and not one
    /// row further. Scrolling into empty space below the text looks like the
    /// page has broken.
    fn scroll_max(&self) -> f64 {
        (self.page.height as f64 - self.body.height.max(1) as f64).max(0.0)
    }

    fn nudge(&mut self, rows: f64) {
        self.vel += rows;
    }

    fn scroll_tick(&mut self, dt: f64) {
        if self.vel.abs() < 1e-6 {
            return;
        }
        self.scroll = (self.scroll + self.vel * dt).clamp(0.0, self.scroll_max());
        // Exponential friction, plus a floor: decay never actually arrives, so
        // without the snap the page creeps a hundredth of a row forever and
        // never stops asking to be redrawn.
        self.vel *= (-8.0 * dt).exp();
        if self.vel.abs() < 0.6 {
            self.vel = 0.0;
        }
        // Hitting either end kills the momentum rather than letting it grind.
        if self.scroll <= 0.0 || self.scroll >= self.scroll_max() {
            self.vel = 0.0;
        }
    }

    fn scroll_key(&mut self, k: KeyEvent) {
        let page = self.body.height.saturating_sub(2) as f64;
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.nudge(26.0),
            KeyCode::Up | KeyCode::Char('k') => self.nudge(-26.0),
            KeyCode::PageDown | KeyCode::Char(' ') => self.nudge(page * 5.0),
            KeyCode::PageUp => self.nudge(-page * 5.0),
            KeyCode::Home => {
                self.scroll = 0.0;
                self.vel = 0.0;
            }
            KeyCode::End => {
                self.scroll = self.scroll_max();
                self.vel = 0.0;
            }
            _ => {}
        }
    }

    /// Put the essay at a known row. For snapshots, which need a frame to be a
    /// pure function of the flags that produced it.
    pub fn set_scroll(&mut self, rows: u16) {
        self.scroll = (rows as f64).min(self.scroll_max());
        self.vel = 0.0;
    }

    fn home_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('1') | KeyCode::Enter => self.go(Section::Experience),
            KeyCode::Char('2') => self.go(Section::Projects),
            KeyCode::Char('3') => self.go(Section::Skills),
            KeyCode::Char('4') => self.go(Section::Taste),
            KeyCode::Char('5') => self.go(Section::Ask),
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        use crossterm::event::MouseEventKind;
        match self.section {
            Section::Experience => self.map.on_mouse(m),
            Section::Projects | Section::Skills => self.sheet.on_mouse(m),
            Section::Taste => match m.kind {
                MouseEventKind::ScrollDown => self.nudge(34.0),
                MouseEventKind::ScrollUp => self.nudge(-34.0),
                _ => {}
            },
            Section::Home | Section::Ask => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        Block::default().style(Style::default().bg(BG)).render(area, f.buffer_mut());

        let [head, body, foot] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.body = body;

        self.rail(f, head);

        match self.section {
            Section::Home => home::render(f, body, &self.about),
            Section::Taste => {
                self.page.render(f, body, self.scroll.round().max(0.0) as u16)
            }
            Section::Ask => ask::render(f, body, &self.ask),
            Section::Experience => termap::ui::render_map_only(f, body, &mut self.map),
            Section::Projects | Section::Skills => {
                skysheet::ui::render_body(f, body, &mut self.sheet)
            }
        }

        // The dissolve is composited over the finished body rather than passed
        // down into it, which is the only way one effect can cover three
        // renderers that know nothing about each other.
        if self.switch > 0.0 {
            let k = paint::ease(1.0 - self.switch / SWITCH) as f32;
            paint::veil(f, body, k);
        }

        self.footer(f, foot);
        if self.show_help {
            home::help(f, area);
        }
    }

    fn rail(&self, f: &mut Frame, area: Rect) {
        // Not on Home: the landing screen sets the name as its headline three
        // rows down, and the same words twice on one screen reads as a mistake.
        if self.section != Section::Home {
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        self.about.name.to_uppercase(),
                        Style::default().fg(FG).add_modifier(Modifier::BOLD),
                    ),
                ])),
                area,
            );
        }

        let mut spans = Vec::new();
        for s in Section::ALL {
            let on = s == self.section;
            spans.push(Span::styled(
                if on { "● " } else { "· " },
                Style::default().fg(if on { ACCENT } else { FAINT }),
            ));
            spans.push(Span::styled(
                format!("{}   ", s.label()),
                Style::default().fg(if on { FG } else { FAINT }),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).right_aligned(), area);
    }

    fn footer(&self, f: &mut Frame, area: Rect) {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(self.section.hints(), Style::default().fg(FAINT)),
            ])),
            area,
        );

        let mut right = Vec::new();
        // The map's instruments live here now that it no longer draws its own
        // status line. They are readings, not decoration: what scale you are
        // looking at and how far the camera is leaning.
        if self.section == Section::Experience {
            let vp = &self.map.vp;
            right.push(Span::styled(
                format!(
                    "{}   z{:.1}   tilt {:.0}°     ",
                    self.map.mode().label(),
                    vp.zoom,
                    vp.tilt.to_degrees()
                ),
                Style::default().fg(FAINT),
            ));
        }
        if self.section == Section::Ask {
            let n = self.ask.turns.len();
            right.push(Span::styled(
                format!("{n}/{} questions     ", crate::acp::MAX_TURNS),
                Style::default().fg(FAINT),
            ));
        }
        if self.section != Section::Ask {
            right.push(Span::styled("q  quit  ", Style::default().fg(DIM)));
        }
        f.render_widget(Paragraph::new(Line::from(right)).right_aligned(), area);
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}
