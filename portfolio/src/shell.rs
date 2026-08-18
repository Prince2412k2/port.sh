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
use crate::home;
use crate::paint::{self, ACCENT, BG, DIM, FAINT, FG};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Home,
    Experience,
    Projects,
    Skills,
}

impl Section {
    pub const ALL: [Section; 4] =
        [Section::Home, Section::Experience, Section::Projects, Section::Skills];

    pub fn label(self) -> &'static str {
        match self {
            Section::Home => "home",
            Section::Experience => "experience",
            Section::Projects => "projects",
            Section::Skills => "skills",
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
    /// Seconds left in the transition, counting down.
    switch: f64,
    /// Where the body was last drawn, so mouse events can be made local to it.
    body: Rect,
    show_help: bool,
}

impl Shell {
    pub fn new() -> Self {
        let mut map = termap::app::App::new(termap::tiles::Source::open(None));
        // The experience section is the tour; there is no other reason for a
        // portfolio to open a map. It arms itself on the first frame, once the
        // viewport knows how many subpixels wide it is.
        map.start_tour(0);

        Shell {
            section: Section::Home,
            about: about::load(),
            map,
            sheet: skysheet::app::App::new(),
            quit: false,
            switch: 0.0,
            body: Rect::default(),
            show_help: false,
        }
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
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Tab => self.step(1),
            KeyCode::BackTab => self.step(-1),
            _ => match self.section {
                Section::Home => self.home_key(k),
                Section::Experience => self.map.on_key(k),
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

    fn home_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('1') | KeyCode::Enter => self.go(Section::Experience),
            KeyCode::Char('2') => self.go(Section::Projects),
            KeyCode::Char('3') => self.go(Section::Skills),
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        match self.section {
            Section::Experience => self.map.on_mouse(m),
            Section::Projects | Section::Skills => self.sheet.on_mouse(m),
            Section::Home => {}
        }
        let _ = self.body;
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
        right.push(Span::styled("q  quit  ", Style::default().fg(DIM)));
        f.render_widget(Paragraph::new(Line::from(right)).right_aligned(), area);
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}
