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
use crate::boot;
use crate::ask::{self, Ask};
use crate::context;
use crate::home;
use crate::museum::Museum;
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
            Section::Home => "tab  move between sections    /  help",
            // `p` rather than the layer keys themselves: the panel is where they
            // are written down, next to what each one draws.
            Section::Experience => {
                "n b  places    ?  find    drag  pan    wheel  zoom    p  layers    /  help"
            }
            Section::Projects => "← →  browse projects    /  help",
            Section::Skills => "drag / wheel  slide    hover  raise a tile    space  drift",
            Section::Taste => "← →  walk the room    home / end    /  help",
            Section::Ask => "type a question    enter  send    esc  stop    tab  leave",
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
    pub museum: Museum,
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
    /// Seconds into the opening. Counts up to `boot::SECS` and then stops
    /// mattering; any key cuts it short, because a title card you cannot skip
    /// is a title card that gets resented on the second visit.
    boot: f64,
    /// Seconds since the session started, never reset. Only the baked
    /// animations read it, and they want a clock that does not restart every
    /// time somebody changes section.
    clock: f64,
    /// Seconds since this section was opened. The home portrait loops for a
    /// while after somebody arrives and then holds, the same rule the museum
    /// uses, so a tab left open is not still sending frames.
    since: f64,
    /// Where the chat's map thumbnail is looking, when there is one.
    locator: Option<Locator>,
}

/// The chat's map thumbnail, and the flight it is on.
///
/// The agent moves this by calling `show_map` again with a different point, and
/// a cut from one city to another reads as a glitch rather than as a move -- the
/// picture simply becomes a different picture. So the camera flies, and the
/// flight is what tells you the two places are related.
///
/// Not the tour's Van Wijk path. That derivation is about holding *perceived*
/// speed constant across a screen-sized journey; this is 46 columns wide and
/// three quarters of a second long, and what it needs is the same idea in
/// miniature -- pull back, cross, come in -- which is one term.
#[derive(Debug, Clone, Copy)]
struct Locator {
    from: (f64, f64),
    from_zoom: f64,
    to: (f64, f64),
    to_zoom: f64,
    /// Seconds into the move.
    t: f64,
    /// How long this one takes. An arrival is slower than a crossing: it is the
    /// entrance, and it is the only one anybody watches from the beginning.
    span: f64,
}

/// A crossing: the agent moved the map to somewhere else.
const FLIGHT: f64 = 0.75;
/// An arrival: the map was not there a moment ago.
const ARRIVAL: f64 = 1.5;
/// How far the camera leans once it has landed, in degrees.
///
/// Chosen for the picture rather than taken from `view::auto_tilt`, which is a
/// function of zoom and gives nine degrees here -- not a lean, a rounding error.
/// This is a thumbnail of one place that never moves once it arrives, so it can
/// afford the angle the experience section arrives at.
const LEAN_DEG: f64 = 44.0;
/// Convergence to go with it. Modest on purpose: perspective has a near plane,
/// and at the wide zooms a locator sits on it buys little and risks clipping.
const CONVERGE: f64 = 0.28;

/// Zoom levels an arrival falls through. Enough that the region is legible
/// before the town is, which is what makes it read as a descent rather than a
/// zoom.
const DESCENT: f64 = 3.6;

impl Locator {
    /// The entrance: a descent onto the point, tilting up as it lands.
    ///
    /// Not a cut and not a fade from nothing. The tour's opening does the same
    /// thing at full size for the same reason -- a map that is simply *there*
    /// reads as a picture of a map, and one that arrives reads as a camera.
    fn arriving(to: (f64, f64), zoom: f64) -> Locator {
        Locator { from: to, from_zoom: zoom - DESCENT, to, to_zoom: zoom, t: 0.0, span: ARRIVAL }
    }

    fn flying(&self) -> bool {
        self.t < self.span
    }

    /// Send it somewhere else, from wherever it currently is.
    fn go(&mut self, to: (f64, f64), zoom: f64) {
        let (at, at_zoom, _, _) = self.now();
        *self = Locator { from: at, from_zoom: at_zoom, to, to_zoom: zoom, t: 0.0, span: FLIGHT };
    }

    /// Where the camera is, how far it has tilted, and where the pin is.
    ///
    /// All four off one clock, so a frame is a pure function of its time and a
    /// snapshot at 0.4 s is the same picture every run.
    fn now(&self) -> ((f64, f64), f64, f64, f32) {
        let k = (self.t / self.span).clamp(0.0, 1.0);
        let e = crate::paint::ease(k);
        let lon = self.from.0 + (self.to.0 - self.from.0) * e;
        let lat = self.from.1 + (self.to.1 - self.from.1) * e;
        // Pull back over the middle of a crossing, by how far there is to go.
        // Without it a long move is a smear of tiles at street zoom; with it the
        // two places are visibly in the same country. An arrival is already a
        // descent and needs no arc on top of it.
        let far = (self.to.0 - self.from.0).hypot(self.to.1 - self.from.1);
        let out = (far * 0.55).min(4.0) * (std::f64::consts::PI * k).sin();
        let zoom = self.from_zoom + (self.to_zoom - self.from_zoom) * e - out;

        // Travel flat, arrive tilted. Leaning while the ground is still moving
        // is where a 46-column map turns to mush, so the lean is the last thing
        // that happens -- the same order the tour lands in.
        // Clamped on the way out, not just on the way in: smootherstep of a
        // value a hair under 1 comes back a hair over it, and a `lean` of
        // 1.0000000000000013 is a camera that has tilted slightly too far and a
        // test that cannot say what it means.
        let ramp = |from: f64, over: f64| {
            crate::paint::ease(((k - from) / over).clamp(0.0, 1.0)).clamp(0.0, 1.0)
        };
        let lean = ramp(0.55, 0.45);
        // And the pin drops last of all, onto a camera that has stopped.
        let pin = ramp(0.68, 0.32) as f32;
        ((lon, lat), zoom.clamp(3.0, 16.0), lean, pin)
    }
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
            museum: Museum::new(&sheet_taste),
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
            boot: 0.0,
            clock: 0.0,
            since: 0.0,
            locator: None,
        };
        shell.context = context::build(&shell.about, &sheet_taste, &shell.sheet.projects);
        // The chat turns a question into a point on its own -- but only the map
        // knows how much of the world this deployment has tiles for, and a
        // thumbnail of somewhere the archive does not cover is a black
        // rectangle with a caption under it.
        shell.ask.atlas.covers = shell.map.source.has_basemap().then(|| shell.map.source.bounds());
        shell
    }

    /// Exchanges finished since the last call, for the visit log.
    pub fn drain_logged(&mut self) -> Vec<(String, String)> {
        self.ask.drain_logged()
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
        self.since = 0.0;
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

    /// Milliseconds to wait for input before drawing again.
    ///
    /// Not one number for the whole app. A camera flight is a few hundred cells
    /// and wants to be smooth; the skills sheet is a full screen of colour tiles
    /// and repaints every cell of it, so the same frame rate there costs an
    /// order of magnitude more bandwidth. Over SSH that is the difference
    /// between fluid and unusable, and half the frames are not worth it.
    pub fn frame_ms(&self) -> u64 {
        if self.booting() {
            // The opening is a full screen of braille that changes everywhere
            // at once, so every frame is close to a full repaint -- measured
            // over a WebSocket, 30 ms frames cost ~1 MB for 2.4 seconds. The
            // motion is slow swell, which reads the same at half that rate,
            // and this is the very first thing anyone downloads.
            return 60;
        }
        if !self.animating() {
            return 120;
        }
        match self.section {
            Section::Skills => 45,
            // Nothing on these two moves except a baked portrait, and those
            // play at PORTRAIT_FPS. Asking for 40 frames a second to advance
            // an eight-frame loop is bandwidth spent on frames identical to
            // the ones before them.
            Section::Home => 125,
            // Two rates, because the room has two kinds of motion. A slide is
            // a third of a second and wants to be smooth; a plate looping at
            // PORTRAIT_FPS does not, and asking for 25 frames a second to
            // advance a 6 fps loop repaints the whole contour field four times
            // for nothing. Measured, that distinction is 157 KB/s against 60.
            Section::Taste if self.museum.sliding() => 40,
            Section::Taste => (1000.0 / crate::paint::PORTRAIT_FPS) as u64,
            _ => 25,
        }
    }

    /// True while anything is animating and the loop must keep drawing.
    pub fn animating(&self) -> bool {
        self.booting()
            || self.switch > 0.0
            || match self.section {
                Section::Experience => self.map.animating(),
                Section::Projects | Section::Skills => self.sheet.moving(),
                Section::Taste => self.museum.moving(self.body),
                // Only while the tide is running. Idle, the screen is static
                // and the stream still gets polled on the slow heartbeat --
                // asking for 40 frames a second to render a blinking caret is
                // how a portfolio ends up warming someone's laptop.
                Section::Ask => {
                    self.ask.busy()
                        || self.ask.panel.as_ref().is_some_and(|p| p.moving())
                        || self.locator.is_some_and(|l| l.flying())
                }
                // Both halves of this come from the bake actually on screen: a
                // window too narrow for the portrait has nothing animating at
                // all, and a wider one that earns the large bake pays for it in
                // seconds rather than in bandwidth.
                Section::Home => home::plate(self.body, &self.about)
                    .is_some_and(|p| self.since < crate::paint::lively_for(p)),
            }
    }

    /// True while the opening is still on screen.
    pub fn booting(&self) -> bool {
        self.boot < boot::SECS
    }

    /// Skip the rest of the opening.
    pub fn skip_boot(&mut self) {
        self.boot = boot::SECS;
    }

    pub fn tick(&mut self, dt: f64) {
        self.clock += dt;
        self.since += dt;
        if self.booting() {
            self.boot += dt;
            return;
        }
        if self.switch > 0.0 {
            self.switch = (self.switch - dt).max(0.0);
        }
        // Only the visible section advances. A map flying in the background
        // burns frames nobody is watching, and over SSH that is bandwidth.
        match self.section {
            Section::Experience => self.map.tick(dt),
            Section::Projects | Section::Skills => self.sheet.tick(dt),
            Section::Taste => self.museum.tick(dt),
            Section::Ask => {
                self.ask.tick(dt);
                self.aim(dt);
                // The map goes on ticking while the chat has the screen, but
                // only while a thumbnail is on it: the tiles it wants are
                // fetched on the frame that draws them and nothing here
                // animates, so this is bookkeeping rather than motion.
                if let Some(want) = self.ask.goto.take() {
                    // The place first: `go` wakes the section, and a tour told
                    // where to open after it has opened has already gone
                    // somewhere else.
                    if let Some(id) = self.ask.goto_place.take() {
                        if let Some(i) = self.map.tour.places.iter().position(|p| p.id == id) {
                            self.map.start_tour(i);
                        }
                    }
                    if let Some(to) = Section::ALL.into_iter().find(|s| s.label() == want) {
                        self.go(to);
                    }
                } else {
                    // Dropped rather than kept: a stop nobody navigated to
                    // would otherwise be waiting to hijack the next `/map`.
                    self.ask.goto_place = None;
                }
            }
            Section::Home => {}
        }
    }

    /// Point the thumbnail at whatever the page is showing, and fly it there.
    ///
    /// Read off the panel each frame rather than pushed when it changes, so
    /// there is one place that decides where the camera is and it cannot get out
    /// of step with what the page thinks it is drawing.
    fn aim(&mut self, dt: f64) {
        let want = crate::ask::showing_place(&self.ask).cloned();
        match (want, &mut self.locator) {
            (Some(spot), Some(loc)) => {
                loc.t += dt;
                // A hair of tolerance: these come off a JSON number and back
                // through an f64, and restarting a flight every frame because
                // the last decimal place moved would be a camera that never
                // lands.
                let moved = (loc.to.0 - spot.lonlat.0).abs() > 1e-6
                    || (loc.to.1 - spot.lonlat.1).abs() > 1e-6
                    || (loc.to_zoom - spot.zoom).abs() > 1e-3;
                if moved {
                    loc.go(spot.lonlat, spot.zoom);
                }
            }
            (Some(spot), None) => {
                self.locator = Some(Locator::arriving(spot.lonlat, spot.zoom))
            }
            // The panel has gone. Dropped rather than kept, so the next map
            // arrives where it was asked for instead of flying in from whatever
            // the last conversation was about.
            (None, _) => self.locator = None,
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if self.booting() {
            // Anything at all, except the one key that should always mean quit.
            if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
                self.quit = true;
            }
            self.skip_boot();
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

        // Text input owns the keyboard. Everywhere else, navigation is the
        // shell's -- including the digits. They used to fall through to the
        // section, and the sheet binds 1 and 2 to its own two tabs, so `1`
        // meant "experience" on the landing page and "projects" one section
        // later. A key that means different things in different places is not
        // navigation.
        //
        // The map wanted them too, for its layer toggles, and lost: `1`-`5` are
        // sections everywhere and `6`-`9` are reserved for sections that do not
        // exist yet. Its toggles are Shift and a digit now -- `!` through `*`,
        // see `termap::app::LAYER_KEYS` -- which is punctuation and falls
        // straight through this to the section below.
        if self.section == Section::Ask {
            self.ask.on_key(k);
            return;
        }
        match k.code {
            KeyCode::Char('/') => self.show_help = true,
            KeyCode::Char(c @ '1'..='9') => {
                let i = c as usize - '1' as usize;
                if let Some(s) = Section::ALL.get(i + 1) {
                    self.go(*s);
                }
            }
            KeyCode::Char('q') => self.quit = true,
            _ => match self.section {
                Section::Home => self.home_key(k),
                Section::Experience => self.map.on_key(k),
                Section::Taste => self.museum_key(k),
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

    /// Walking the room. Left and right only -- there is no second axis here,
    /// and arrow-up on a wall of pictures does not mean anything.
    fn museum_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Right | KeyCode::Down | KeyCode::Char('l') | KeyCode::Char(' ') => {
                self.museum.next()
            }
            KeyCode::Left | KeyCode::Up | KeyCode::Char('h') => self.museum.prev(),
            KeyCode::Home => self.museum.go(0),
            KeyCode::End => self.museum.go(self.museum.len().saturating_sub(1)),
            _ => {}
        }
    }

    /// Jump straight to a work. For snapshots, which need a frame to be a
    /// pure function of the flags that produced it.
    pub fn set_scroll(&mut self, n: u16) {
        self.museum.jump(n as usize);
    }

    fn home_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Enter {
            self.go(Section::Experience);
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        use crossterm::event::MouseEventKind;
        match self.section {
            Section::Experience => self.map.on_mouse(m),
            Section::Projects | Section::Skills => self.sheet.on_mouse(m),
            // A wheel walks the room a work at a time. There is nothing to
            // scroll here, and momentum on a wall of pictures would overshoot
            // past whatever somebody was reaching for.
            Section::Taste => match m.kind {
                MouseEventKind::ScrollDown => self.museum.next(),
                MouseEventKind::ScrollUp => self.museum.prev(),
                _ => {}
            },
            Section::Ask => match m.kind {
                MouseEventKind::ScrollUp => self.ask.on_scroll(true),
                MouseEventKind::ScrollDown => self.ask.on_scroll(false),
                _ => {}
            },
            Section::Home => {}
        }
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        Block::default().style(Style::default().bg(BG)).render(area, f.buffer_mut());

        if self.booting() {
            boot::render(f, area, self.boot);
            return;
        }

        // The ask page pulls in once a conversation has started: the rail
        // goes, and the reading column gets the rows the chrome was using.
        // Everywhere else the tabs are how you know what else exists, but a
        // page you are reading an answer on is not a page you are navigating,
        // and five section names above somebody's reply is furniture.
        let zoomed = self.section == Section::Ask && !self.ask.turns.is_empty();

        let [head, body, foot] = Layout::vertical([
            Constraint::Length(if zoomed { 0 } else { 1 }),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.body = body;

        if !zoomed {
            self.rail(f, head);
        }

        match self.section {
            Section::Home => home::render(f, body, &self.about, self.since),
            Section::Taste => {
                crate::museum::render(f, body, &self.museum)
            }
            Section::Ask => {
                ask::render(f, body, &self.ask);
                // The chat says where the picture goes; the map draws it. It
                // cannot draw itself from in there -- the renderer wants an
                // `App`, and one of those is a terrain grid and a tile cache
                // that this shell already owns exactly one of.
                if let Some((at, spot, fade)) = ask::map_panel(body, &self.ask) {
                    let pin = spot.id.is_none();
                    // Mid-flight the camera is between two places, so it is the
                    // flight that says where to draw, not the destination.
                    let (lonlat, zoom, lean, drop) = self
                        .locator
                        .map_or((spot.lonlat, spot.zoom, 1.0, 1.0), |l| l.now());
                    let cam = termap::ui::Camera {
                        lonlat,
                        zoom,
                        tilt: LEAN_DEG.to_radians() * lean,
                        persp: CONVERGE * lean,
                    };
                    // The tour draws its own marker for a stop on the sheet;
                    // anywhere else has nothing but ours.
                    termap::ui::render_locator(f, at, &mut self.map, cam, pin.then_some(drop));
                    // Composited afterwards, like the section dissolve: the map
                    // renderer knows nothing about this page's fade and should
                    // not have to.
                    paint::veil(f, at, fade);
                }
            }
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
        if self.section == Section::Experience && !self.map.source.has_basemap() {
            right.push(Span::styled(
                "no basemap mounted     ".to_string(),
                Style::default().fg(ACCENT),
            ));
        }
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
                format!("{n}/{} questions     ", crate::gates::GATES.turns),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// A shell past its opening. Every one of these is about what the keyboard
    /// does once you are in, and a fresh `Shell` is still showing the title
    /// card, where the first key means "skip".
    fn shell() -> Shell {
        let mut s = Shell::new();
        s.skip_boot();
        s
    }

    /// Navigation is the one thing that does not wait for the answer.
    #[test]
    fn the_screen_can_still_be_left_while_an_answer_is_arriving() {
        let mut a = crate::ask::Ask::new();
        a.state = crate::ask::State::Thinking;
        a.input = "/map".into();
        a.submit();
        assert_eq!(a.goto.as_deref(), Some("experience"), "`/map` was swallowed by the wait");

        // ...and asking a second question still is not.
        let mut a = crate::ask::Ask::new();
        a.state = crate::ask::State::Thinking;
        a.input = "and what about the map?".into();
        a.submit();
        assert!(a.turns.is_empty(), "a second question jumped the queue");
    }

    /// Look at the arrival, frame by frame. `cargo test look_at_the_arrival --
    /// --nocapture` and read down the page.
    #[test]
    fn look_at_the_arrival() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut s = Shell::new();
        if !s.map.source.has_basemap() {
            return;
        }
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        let board = s.ask.board_token().expect("no board").to_string();
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":23.0386,"lon":72.5129,"zoom":11.5,"label":"Ahmedabad"}}}"#,
        )
        .unwrap();

        let (w, h) = (150u16, 40u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let mut shown = 0;
        for step in 0..=14 {
            s.tick(if step == 0 { 0.016 } else { ARRIVAL / 12.0 });
            term.draw(|f| s.render(f)).unwrap();
            let l = s.locator.expect("no camera");
            let (_, zoom, lean, pin) = l.now();
            let body = Rect { x: 0, y: 1, width: w, height: h - 2 };
            let ink = crate::ask::map_panel(body, &s.ask)
                .map(|(at, _, _)| {
                    let buf = term.backend().buffer();
                    (at.y..at.y + at.height)
                        .flat_map(|y| (at.x..at.x + at.width).map(move |x| (x, y)))
                        .filter(|(x, y)| {
                            buf.cell((*x, *y)).is_some_and(|c| c.symbol().trim() != "")
                        })
                        .count()
                })
                .unwrap_or(0);
            if ink > 0 {
                shown += 1;
            }
            if let Ok(dir) = std::env::var("ARRIVAL_FRAMES") {
                let ansi = termap::snapshot::ansi(term.backend().buffer());
                let _ = std::fs::write(format!("{dir}/f{step:02}.ans"), ansi);
            }
            println!(
                "t={:>5.2}  zoom {:>5.2}  lean {:>4.2}  pin {:>4.2}  ink {ink:>4}  flying {}",
                l.t, zoom, lean, pin, l.flying()
            );
        }
        assert!(shown > 10, "the map was blank for most of its arrival");
    }

    /// A place question puts a real map on the chat page.
    ///
    /// Driven through the whole shell rather than through `ask::render`, because
    /// the picture is the one thing on that page the chat does not draw: it says
    /// where the map goes and this file puts it there, and a test of either half
    /// alone would pass with the two of them not speaking.
    #[test]
    fn a_place_question_draws_a_map_beside_the_answer() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        // Opening the section wakes the agent, and a waking agent is busy --
        // `submit` refuses while it is. A visitor types after it is up.
        s.ask.state = crate::ask::State::Ready;
        s.ask.input = "where does he work?".into();
        s.ask.submit();
        s.tick(1.0);

        let (w, h) = (160, 44);
        let body = Rect { x: 0, y: 1, width: w, height: h - 2 };
        let Some((at, spot, fade)) = crate::ask::map_panel(body, &s.ask) else {
            panic!("no map panel for a place question");
        };
        assert_eq!(spot.id.as_deref(), Some("gateway"));
        assert_eq!(fade, 1.0, "the panel never finished arriving");
        assert!(at.width > 20 && at.height > 6, "the picture has no room: {at:?}");

        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| s.render(f)).unwrap();
        let buf = term.backend().buffer().clone();

        // Something is actually in the hole the chat left. Without a basemap
        // mounted there is nothing to draw and this is the map saying so, which
        // is still a drawn panel -- so the caption is what gets asserted, and
        // the tiles are checked only when there are tiles.
        let plain = termap::snapshot::plain(&buf);
        assert!(plain.contains("Gateway Corp"), "the panel lost its caption:\n{plain}");

        if std::env::var_os("LOOK").is_some() {
            println!("\n{plain}");
        }
        if s.map.source.has_basemap() {
            let ink = (at.y..at.y + at.height)
                .flat_map(|y| (at.x..at.x + at.width).map(move |x| (x, y)))
                .filter(|(x, y)| buf.cell((*x, *y)).is_some_and(|c| c.symbol() != " "))
                .count();
            assert!(ink > 40, "the map drew {ink} cells into {at:?}");
        }
    }


    /// The whole point, end to end: the agent asks and the page obeys.
    #[test]
    fn a_tool_call_from_the_agent_puts_a_map_up_and_takes_it_away() {
        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;

        // A question in flight, because that is when a tool call happens: the
        // row belongs to the turn being answered, and a call with no turn to
        // land on is dropped.
        s.ask.input = "tell me about the terminal".into();
        s.ask.submit();
        assert!(s.ask.panel.is_none(), "the guess fired on a question with no place in it");

        // The token the tool server would address this page by. `go` already
        // registered one; this is the same call the agent's tool would make.
        let board = s.ask.board_token().expect("no board registered").to_string();
        let out = crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":26.91,"lon":75.79,"zoom":11.0,"label":"Jaipur"}}}"#,
        )
        .expect("no reply");
        assert!(!out.contains("isError"), "{out}");

        s.tick(0.016);
        let Some(crate::ask::Panel { what: crate::ask::Show::Place(spot), .. }) = &s.ask.panel
        else {
            panic!("the tool call did not raise a map");
        };
        assert_eq!(spot.name, "Jaipur");
        assert!((spot.lonlat.0 - 75.79).abs() < 1e-9);
        assert!(s.ask.agent_drives, "the page did not notice the agent driving");

        // The row says what was called and with what. The ACP stream cannot say
        // -- no name in the protocol, empty title from Copilot -- so this comes
        // from our own tool server, which knows exactly.
        let row = s.ask.turns.last().map(|t| t.calls.clone()).unwrap_or_default();
        assert!(
            row.iter().any(|c| c.title == "show_map" && c.detail.contains("Jaipur")),
            "no named tool row: {row:?}"
        );

        // The turn that asked for it ends. The map stays: that turn wanted it.
        s.ask.finish_for_test();
        s.tick(0.016);
        assert_eq!(
            s.ask.panel.as_ref().map(|p| p.life),
            Some(crate::ask::Life::Arriving),
            "the map left on the turn that asked for it"
        );

        // ...and once the agent is driving, the keyword guess stops. Asking
        // about Gateway must not overrule the map the agent flew to Jaipur.
        s.ask.input = "where does he work?".into();
        s.ask.submit();
        s.tick(0.016);
        let Some(crate::ask::Panel { what: crate::ask::Show::Place(spot), .. }) = &s.ask.panel
        else {
            panic!("the map vanished");
        };
        assert_eq!(spot.name, "Jaipur", "the keyword guess fought the agent");

        // And *this* answer comes in having asked for nothing, so it leaves.
        s.ask.finish_for_test();
        s.tick(0.016);
        assert_eq!(
            s.ask.panel.as_ref().map(|p| p.life),
            Some(crate::ask::Life::Leaving),
            "an answer that wanted no map left one up"
        );
        s.tick(1.0);
        assert!(s.ask.panel.is_none(), "it never finished leaving");
        assert!(s.locator.is_none(), "the camera outlived the panel");
    }

    /// A second `show_map` flies rather than cuts.
    #[test]
    fn moving_the_map_is_a_flight_and_not_a_cut() {
        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        let board = s.ask.board_token().expect("no board").to_string();
        let call = |lat: f64, lon: f64| {
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"show_map","arguments":{{"lat":{lat},"lon":{lon},"zoom":11.0}}}}}}"#
            )
        };

        crate::mcp::handle(&board, &call(23.03, 72.51)).unwrap();
        s.tick(0.016);
        // An arrival is a descent, so it *is* flying -- and it starts wide and
        // comes down onto the point.
        assert!(s.locator.unwrap().flying(), "the first map did not arrive, it appeared");
        let (_, wide, lean, pin) = s.locator.unwrap().now();
        assert!(wide < 11.0 - 2.0, "it did not start high up: {wide}");
        assert_eq!(lean, 0.0, "it arrived already tilted");
        assert_eq!(pin, 0.0, "the pin was down before the camera stopped");
        s.tick(ARRIVAL);
        let (_, landed, lean, pin) = s.locator.unwrap().now();
        assert!((landed - 11.0).abs() < 1e-9, "it did not land on the zoom asked for: {landed}");
        assert_eq!(lean, 1.0, "it never tilted");
        assert_eq!(pin, 1.0, "the pin never landed");

        crate::mcp::handle(&board, &call(26.91, 75.79)).unwrap();
        s.tick(0.016);
        assert!(s.locator.unwrap().flying(), "the second map cut instead of flying");

        // Part way across it is between the two, and pulled back from both.
        s.tick(FLIGHT / 2.0);
        let (mid, mid_zoom, _, _) = s.locator.unwrap().now();
        assert!(mid.0 > 72.51 && mid.0 < 75.79, "not between the two: {mid:?}");
        assert!(mid_zoom < 11.0, "it crossed at street zoom: {mid_zoom}");

        // And it lands, exactly, rather than easing forever.
        s.tick(FLIGHT);
        assert!(!s.locator.unwrap().flying());
        let (end, end_zoom, _, _) = s.locator.unwrap().now();
        assert!((end.0 - 75.79).abs() < 1e-9 && (end.1 - 26.91).abs() < 1e-9, "{end:?}");
        assert!((end_zoom - 11.0).abs() < 1e-9, "{end_zoom}");
    }

    /// `hide_map` takes it down without waiting for the answer to end.
    #[test]
    fn the_agent_can_put_the_map_away_itself() {
        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        let board = s.ask.board_token().expect("no board").to_string();
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map","arguments":{"lat":23.0,"lon":72.5}}}"#,
        )
        .unwrap();
        s.tick(0.016);
        assert!(s.ask.panel.is_some());
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"hide_map","arguments":{}}}"#,
        )
        .unwrap();
        s.tick(0.016);
        assert_eq!(s.ask.panel.as_ref().map(|p| p.life), Some(crate::ask::Life::Leaving));
    }

    /// The locator draws where it is told, and gives the camera back.
    ///
    /// Two calls that differ *only* in where they are pointed, so the frames
    /// have nothing else they could differ by. Two earlier versions of this
    /// test had no teeth and both looked fine: comparing whole frames passed on
    /// the captions alone, and comparing two real places passed on their
    /// different zooms. Both were found by nailing `park_camera` to one point on
    /// purpose and watching the test stay green -- which is the only way to know
    /// a test of a picture is testing the picture.
    #[test]
    fn the_locator_draws_where_it_is_told_and_hands_the_camera_back() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut s = Shell::new();
        if !s.map.source.has_basemap() {
            return; // Nothing to draw, and nothing this could prove.
        }
        let at = Rect { x: 0, y: 0, width: 46, height: 20 };
        let shot = |m: &mut termap::app::App, lonlat: (f64, f64)| {
            let mut t = Terminal::new(TestBackend::new(at.width, at.height)).unwrap();
            let cam = termap::ui::Camera { lonlat, zoom: 13.0, tilt: 0.0, persp: 0.0 };
            t.draw(|f| termap::ui::render_locator(f, at, m, cam, None)).unwrap();
            termap::snapshot::plain(t.backend().buffer())
        };

        let before = s.map.vp;
        // Ahmedabad and Kapadwanj, at one zoom. Same rect, same everything else.
        let here = shot(&mut s.map, (72.512934, 23.038583));
        let there = shot(&mut s.map, (73.070, 23.020));
        assert_ne!(here, there, "the locator ignored the point it was given");

        assert_eq!(s.map.vp.center, before.center, "the camera was left where the panel put it");
        assert_eq!(s.map.vp.zoom, before.zoom, "the zoom was left where the panel put it");
    }

    /// `/map` with a place at the side opens the tour on that stop, rather than
    /// at the beginning of it.
    #[test]
    fn the_map_command_carries_the_place_to_the_tour() {
        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        let want = s.map.tour.places.iter().position(|p| p.id == "silver-oak");
        let Some(want) = want else { return };

        s.ask.input = "where did he go to university".into();
        s.ask.submit();
        s.ask.input = "/map".into();
        s.ask.submit();
        s.tick(0.016);

        assert_eq!(s.section, Section::Experience, "`/map` did not move the screen");
        assert_eq!(s.map.tour_opens_on(), Some(want), "the tour opened somewhere else");
    }

    /// The map's layer toggles must survive the trip through the shell. They are
    /// punctuation precisely so they can: digits never reach a section.
    #[test]
    fn the_maps_layer_keys_reach_the_map_and_toggle_a_layer() {
        for (i, key) in termap::app::LAYER_KEYS.iter().enumerate() {
            let mut s = shell();
            s.go(Section::Experience);
            let layer = termap::app::TOGGLES[i];
            let before = s.map.layers[layer.index()];
            s.on_key(press(*key));
            assert_eq!(
                s.section,
                Section::Experience,
                "`{key}` navigated away instead of toggling {}",
                layer.label()
            );
            assert_ne!(
                before,
                s.map.layers[layer.index()],
                "`{key}` did not toggle {}",
                layer.label()
            );
        }
    }

    /// Terrain and all-layers-on, which used to be `9` and `0`. `9` was swallowed
    /// by the shell's digit arm and reached nothing at all.
    #[test]
    fn terrain_and_all_layers_have_keys_that_arrive() {
        let mut s = shell();
        s.go(Section::Experience);
        let before = s.map.show_terrain;
        s.on_key(press(termap::app::TERRAIN_KEY));
        assert_ne!(before, s.map.show_terrain, "terrain did not toggle");

        for l in s.map.layers.iter_mut() {
            *l = false;
        }
        s.on_key(press(termap::app::ALL_LAYERS_KEY));
        assert!(s.map.layers.iter().all(|&on| on), "layers did not all come back");
        assert_eq!(s.section, Section::Experience, "navigated away");
    }

    /// The other half of the same rule: a digit in the map section is still
    /// navigation, and never a layer.
    #[test]
    fn digits_in_the_map_section_still_navigate() {
        let mut s = shell();
        s.go(Section::Experience);
        let before = s.map.layers;
        s.on_key(press('3'));
        assert_eq!(s.section, Section::Skills, "`3` stopped navigating");
        assert_eq!(before, s.map.layers, "`3` toggled a layer on its way out");
    }

    /// The bug this pins: the digits used to fall through to whichever section
    /// had the screen, and the sheet binds 1 and 2 to its own two tabs. So `1`
    /// meant "experience" on the landing page and "projects" one section later.
    /// Navigation that changes meaning depending on where you are standing is
    /// not navigation.
    #[test]
    fn a_number_key_means_the_same_thing_from_every_section() {
        let want = [
            ('1', Section::Experience),
            ('2', Section::Projects),
            ('3', Section::Skills),
            ('4', Section::Taste),
        ];
        // Ask is left out as a starting point on purpose: it spawns an agent,
        // and it is the one place digits are text. That case is below.
        for from in [Section::Home, Section::Experience, Section::Projects, Section::Skills, Section::Taste] {
            for (key, to) in want {
                let mut s = shell();
                s.go(from);
                assert_eq!(s.section, from, "could not get to {:?}", from.label());
                s.on_key(press(key));
                assert_eq!(s.section, to, "from {:?}, `{key}` went wrong", from.label());
            }
        }
    }

    /// And the exception: where there is a text field, the keyboard is text.
    #[test]
    fn digits_are_text_in_the_chat() {
        let mut s = shell();
        s.section = Section::Ask;
        s.on_key(press('2'));
        s.on_key(press('q'));
        assert_eq!(s.section, Section::Ask, "typing navigated away");
        assert_eq!(s.ask.input, "2q");
    }

    /// The title card has to be escapable. One that is not gets resented on
    /// the second visit, and this is a page people may come back to.
    #[test]
    fn any_key_skips_the_opening() {
        let mut s = Shell::new();
        assert!(s.booting());
        s.on_key(press('x'));
        assert!(!s.booting());
        // And it does not also do whatever that key normally means.
        assert_eq!(s.section, Section::Home);
    }

    #[test]
    fn tab_wraps_in_both_directions() {
        let mut s = shell();
        s.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(s.section, *Section::ALL.last().unwrap());
        s.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(s.section, Section::Home);
    }
}
