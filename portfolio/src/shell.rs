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
    /// The camera the visitor has driven the chat map to, once they have.
    ///
    /// While this is set it *is* the chat map's camera and the flight is not
    /// consulted: somebody who has panned somewhere has said where they want to
    /// be. Cleared when the agent asks for a new place, and by ctrl-g.
    manual: Option<termap::geo::Viewport>,
    /// Whether the keyboard belongs to the map rather than to the question.
    ///
    /// Ctrl chords were the wrong shape for this. `u` is `VKILL` and `o` is
    /// `VDISCARD` in the tty line discipline, and a browser claims ctrl-U and
    /// ctrl-O for itself as well -- so the two keys the map uses for tilt are
    /// among the least likely bytes in the set to survive the trip, and which
    /// ones do depends on the client. Chasing them one at a time is a losing
    /// game.
    ///
    /// So there is a mode instead: while it is on, the map gets the keys it gets
    /// in the experience section, bare and unmodified, and escape gives them
    /// back. A mode you cannot see you are in is a trap, so the footer says so
    /// and the input line stops pretending to be one.
    driving: bool,
    /// What the last map chord did, and how long ago.
    ///
    /// The map already writes these -- `set_tilt` says "tilt 39 degrees" -- and
    /// this side was throwing them away. Worth keeping for a plain reason: a
    /// tilt step is four and a half degrees, which on a small map is almost
    /// invisible, so "ctrl-o does nothing" and "ctrl-o never arrived" looked
    /// exactly alike. Now one of them says what it did.
    chord: Option<(String, f64)>,
}

/// How long a chord's report stays up.
const CHORD_SECS: f64 = 1.8;

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
    to: (f64, f64),
    to_zoom: f64,
    /// The path itself, from `termap::tour` -- the same Van Wijk & Nuij
    /// derivation the experience section flies, rather than a second
    /// hand-rolled easing sitting beside it. Interpolating centre and zoom
    /// independently is unwatchable at street scale: the middle of every
    /// journey is a blur. This holds *perceived* speed constant instead, and the
    /// altitude it climbs to is not chosen -- it falls out of the derivation as
    /// the height where both ends frame.
    path: termap::tour::Flight,
    /// Seconds into the move.
    t: f64,
    span: f64,
    /// Subpixels across the panel, which is what turns a zoom into the width the
    /// flight is derived in. Kept current: a resize mid-flight should not bend
    /// the path.
    sw: f64,
}

/// An arrival: the map was not there a moment ago. A *crossing* takes however
/// long the path says it should -- that is most of the point of using the real
/// derivation -- but an entrance is a fixed beat, because it is the one anybody
/// watches from the beginning.
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

/// `ctrl-o` reads better than `Char('o')` in a read-out.
fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

impl Locator {
    /// Build the flight between two (point, zoom) pairs.
    fn path(
        from: (f64, f64),
        from_zoom: f64,
        to: (f64, f64),
        to_zoom: f64,
        sw: f64,
    ) -> termap::tour::Flight {
        let c0 = termap::geo::lonlat_to_world(from.0, from.1);
        let c1 = termap::geo::lonlat_to_world(to.0, to.1);
        let w = |z: f64| {
            let mut vp = termap::geo::Viewport::new([0.5, 0.5], z);
            vp.sw = sw;
            termap::tour::width_of(&vp)
        };
        termap::tour::Flight::new(c0, w(from_zoom), c1, w(to_zoom))
    }

    /// The entrance: a descent onto the point, tilting up as it lands.
    ///
    /// Not a cut and not a fade from nothing. The tour's opening does the same
    /// thing at full size for the same reason -- a map that is simply *there*
    /// reads as a picture of a map, and one that arrives reads as a camera.
    fn arriving(to: (f64, f64), zoom: f64, sw: f64) -> Locator {
        Locator {
            to,
            to_zoom: zoom,
            path: Self::path(to, zoom - DESCENT, to, zoom, sw),
            t: 0.0,
            span: ARRIVAL,
            sw,
        }
    }

    /// A journey: set out from somewhere the agent named rather than from
    /// wherever the camera happened to be.
    ///
    /// The span is the path's, not the arrival's fixed beat -- the point of
    /// being given both ends is that the distance between them decides how long
    /// watching it should take.
    fn journey(from: (f64, f64), from_zoom: f64, to: (f64, f64), zoom: f64, sw: f64) -> Locator {
        let path = Self::path(from, from_zoom, to, zoom, sw);
        let span = path.duration();
        Locator { to, to_zoom: zoom, path, t: 0.0, span, sw }
    }

    fn flying(&self) -> bool {
        self.t < self.span
    }

    /// Send it somewhere else, from wherever it currently is.
    ///
    /// The duration comes from the path rather than a constant: crossing a state
    /// and crossing a street are not the same journey, and the derivation
    /// already knows how long each should take.
    fn go(&mut self, to: (f64, f64), zoom: f64) {
        let (at, at_zoom, _, _) = self.now();
        let path = Self::path(at, at_zoom, to, zoom, self.sw);
        let span = path.duration();
        *self = Locator { to, to_zoom: zoom, path, t: 0.0, span, sw: self.sw };
    }

    /// Where the camera is, how far it has tilted, and where the pin is.
    ///
    /// All four off one clock, so a frame is a pure function of its time and a
    /// snapshot at 0.4 s is the same picture every run.
    fn now(&self) -> ((f64, f64), f64, f64, f32) {
        let k = (self.t / self.span).clamp(0.0, 1.0);
        let (c, w) = self.path.at(k);
        let (lon, lat) = termap::geo::world_to_lonlat(c[0], c[1]);
        let zoom = termap::tour::zoom_of(w, self.sw);

        // Travel flat, arrive tilted. Leaning while the ground is still moving
        // is where a small map turns to mush, so the lean is the last thing that
        // happens -- the same order the tour lands in.
        //
        // Clamped on the way out, not just on the way in: smootherstep of a
        // value a hair under 1 comes back a hair over it, and a lean of
        // 1.0000000000000013 is a camera tilted slightly too far and a test that
        // cannot say what it means.
        let ramp = |from: f64, over: f64| {
            crate::paint::ease(((k - from) / over).clamp(0.0, 1.0)).clamp(0.0, 1.0)
        };
        let lean = ramp(0.55, 0.45);
        // And the pin drops last of all, onto a camera that has stopped.
        let pin = ramp(0.68, 0.32) as f32;
        ((lon, lat), zoom.clamp(3.0, 16.5), lean, pin)
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
            manual: None,
            chord: None,
            driving: false,
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
    pub fn drain_logged(&mut self) -> Vec<crate::ask::Logged> {
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
                        || self.chord.is_some()
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

    /// Whether the keyboard is currently the map's.
    ///
    /// For the tests. The page learns it from the mirrored flag on `Ask`, set in
    /// `tick`, because the page is drawn from `&Ask` and not from the shell.
    #[cfg(test)]
    fn driving(&self) -> bool {
        self.driving
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
                if std::mem::take(&mut self.ask.drive) {
                    self.driving = crate::ask::showing_place(&self.ask).is_some();
                    let said = match self.driving {
                        true => "driving the map",
                        false => "no map to drive",
                    };
                    self.chord = Some((said.into(), 0.0));
                }
                // A map that has gone takes the keyboard back with it.
                if self.driving && crate::ask::showing_place(&self.ask).is_none() {
                    self.driving = false;
                }
                // Mirrored so the page can say so: the input line must not go
                // on inviting a question while every letter is the map's.
                self.ask.driving = self.driving;
                if let Some((_, age)) = &mut self.chord {
                    *age += dt;
                }
                if self.chord.as_ref().is_some_and(|(_, age)| *age > CHORD_SECS) {
                    self.chord = None;
                }
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

    /// Subpixels across the map panel, for the flight's width conversion.
    ///
    /// Read from the rect the map is actually drawn into rather than assumed, so
    /// a resize changes the path's idea of a screen width along with the screen.
    fn locator_sw(&self) -> f64 {
        let w = crate::ask::map_panel(self.body, &self.ask)
            .map_or(self.body.width, |(at, _, _)| at.width);
        (w.max(8) as f64) * termap::canvas::SUB_X as f64
    }

    /// Point the thumbnail at whatever the page is showing, and fly it there.
    ///
    /// Read off the panel each frame rather than pushed when it changes, so
    /// there is one place that decides where the camera is and it cannot get out
    /// of step with what the page thinks it is drawing.
    fn aim(&mut self, dt: f64) {
        let sw = self.locator_sw();
        let want = crate::ask::showing_place(&self.ask).cloned();
        match (want, &mut self.locator) {
            (Some(spot), Some(loc)) => {
                loc.t += dt;
                loc.sw = sw;
                // A hair of tolerance: these come off a JSON number and back
                // through an f64, and restarting a flight every frame because
                // the last decimal place moved would be a camera that never
                // lands.
                let moved = (loc.to.0 - spot.lonlat.0).abs() > 1e-6
                    || (loc.to.1 - spot.lonlat.1).abs() > 1e-6
                    || (loc.to_zoom - spot.zoom).abs() > 1e-3;
                if moved {
                    self.manual = None;
                    // A stop that names its own start is flown from there even
                    // when a camera is already up: the agent asked for a
                    // journey, and starting it from wherever the last answer
                    // left the map would be a different journey.
                    match spot.from {
                        Some((from, from_zoom)) => {
                            *loc = Locator::journey(from, from_zoom, spot.lonlat, spot.zoom, sw)
                        }
                        None => loc.go(spot.lonlat, spot.zoom),
                    }
                }
            }
            (Some(spot), None) => {
                // A new place is the agent taking the camera back.
                self.manual = None;
                self.locator = Some(match spot.from {
                    Some((from, from_zoom)) => {
                        Locator::journey(from, from_zoom, spot.lonlat, spot.zoom, sw)
                    }
                    None => Locator::arriving(spot.lonlat, spot.zoom, sw),
                })
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
        if self.section == Section::Ask && self.driving {
            match k.code {
                // Out. Also on `tab`, which everywhere else in this app means
                // "leave", so it would be strange for it to type into a map.
                KeyCode::Esc | KeyCode::Tab | KeyCode::BackTab => {
                    self.driving = false;
                    self.chord = Some(("the keyboard is yours".into(), 0.0));
                    return;
                }
                // Ours rather than the map's: these step the route the agent
                // sent, which is a different thing from the tour's own stops.
                KeyCode::Char('n') => return self.ask.walk(1),
                KeyCode::Char('b') => return self.ask.walk(-1),
                // Search is the one thing that stays behind. It opens a mode
                // inside a mode, and the place to drive a map with a search box
                // is the section built around one.
                KeyCode::Char('?') | KeyCode::Char('/') => return,
                // `q` quits the map's own binary and means nothing here.
                KeyCode::Char('q') => return,
                _ => return self.map_key(k),
            }
        }
        if self.section == Section::Ask {
            // The map's own camera, before the chat gets the key. Ctrl because
            // every bare key here is a letter somebody is typing, and these have
            // to work mid-question like the route keys do.
            // Hold ctrl and the chat speaks the experience section's own map
            // vocabulary, because it is literally the same handler: the key goes
            // to `termap::app::App::on_key` with the modifier stripped. So `u`
            // and `o` tilt, `+` and `-` zoom, `hjkl` pans, `,` and `.` swing the
            // bearing, and a layer toggle is whatever that section says it is --
            // none of it restated here to drift.
            if k.modifiers.contains(KeyModifiers::CONTROL) && self.ask.panel.is_some() {
                match k.code {
                    // Ours, not the map's: `n` and `b` step the route the agent
                    // sent, which is a different thing from the tour's own stops.
                    KeyCode::Char('n') => return self.ask.walk(1),
                    KeyCode::Char('b') => return self.ask.walk(-1),
                    // Hand the camera back to the flight.
                    KeyCode::Char('g') => {
                        self.manual = None;
                        return;
                    }
                    // Into the mode, for the keys a chord cannot reach.
                    KeyCode::Char('e') if crate::ask::showing_place(&self.ask).is_some() => {
                        self.driving = true;
                        self.chord = Some(("driving the map".into(), 0.0));
                        return;
                    }
                    KeyCode::Char(_) => return self.map_key(k),
                    _ => {}
                }
            }
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
        // Whichever section is open decides what a number means. In the room it
        // is which work is on the wall; in the chat it is which project's panel
        // to raise, which is the only way to draw one without a live agent --
        // and drawing one to a PNG is how the panel gets tuned at all.
        match self.section {
            Section::Ask => self.ask.show_work(n as usize),
            _ => self.museum.jump(n as usize),
        }
    }

    fn home_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Enter {
            self.go(Section::Experience);
        }
    }

    /// Where the chat map's camera is this frame.
    ///
    /// The visitor's viewport if they have driven one, otherwise the flight's.
    fn chat_camera(&self) -> Option<termap::ui::Camera> {
        if let Some(vp) = self.manual {
            let (lon, lat) = vp.center_lonlat();
            return Some(termap::ui::Camera {
                lonlat: (lon, lat),
                zoom: vp.zoom,
                tilt: vp.tilt,
                persp: vp.persp,
                bearing: vp.bearing,
            });
        }
        let l = self.locator?;
        let (lonlat, zoom, lean, _) = l.now();
        Some(termap::ui::Camera {
            lonlat,
            zoom,
            tilt: LEAN_DEG.to_radians() * lean,
            persp: CONVERGE * lean,
            bearing: 0.0,
        })
    }

    /// Give one key to the map and keep what it did with it.
    ///
    /// Parked, handled, read back, unparked: the map's own `on_key` is the only
    /// thing that decides what a key means or how far it moves, and the
    /// experience section's camera is left exactly as it was. Anything that is
    /// not the viewport -- a layer toggled, terrain switched off -- is shared on
    /// purpose. It is one map.
    fn map_key(&mut self, k: KeyEvent) {
        let Some(cam) = self.chat_camera() else { return };
        let mut vp = termap::geo::Viewport::new(
            termap::geo::lonlat_to_world(cam.lonlat.0, cam.lonlat.1),
            cam.zoom,
        );
        vp.tilt = cam.tilt;
        vp.persp = cam.persp;
        vp.bearing = cam.bearing;

        let was = self.map.park_viewport(vp);
        self.map.on_key(KeyEvent::new(k.code, KeyModifiers::NONE));
        let after = self.map.vp;
        let said = self.map.toast.take();
        self.map.unpark_camera(was);
        self.manual = Some(after);
        // Whatever the map said about it, or the key itself if it said nothing.
        // A chord that reports nothing is indistinguishable from one that never
        // arrived, and that ambiguity has already cost a round of "it is not
        // working" against code that was working.
        self.chord = Some((said.unwrap_or_else(|| format!("^{}", key_name(k.code))), 0.0));
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
                // Over the caption column the wheel drives the map; over the
                // words it scrolls them. The map is the whole page's ground now,
                // so "where the pointer is" is the only thing that can tell the
                // two apart -- and the right-hand column is the part of it that
                // is not covered in prose.
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let up = m.kind == MouseEventKind::ScrollUp;
                    // Ctrl held, or the pointer over the map's own side: either
                    // way it is the map being scrolled. Ctrl is the one that
                    // works no matter where the pointer happens to be, which is
                    // why the modifiers had to start surviving `wire.rs`.
                    let held = m.modifiers.contains(KeyModifiers::CONTROL);
                    let over = crate::ask::map_rect(self.body, &self.ask)
                        .is_some_and(|at| m.column >= at.x + at.width / 3);
                    match (held || over) && self.ask.panel.is_some() {
                        // Through the map's own zoom keys, so a notch here moves
                        // exactly as far as a notch there.
                        true => self.map_key(KeyEvent::new(
                            KeyCode::Char(if up { '+' } else { '-' }),
                            KeyModifiers::NONE,
                        )),
                        false => self.ask.on_scroll(up),
                    }
                }
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
                // The map goes down first and the words go on top of it. It
                // cannot draw itself from in there -- the renderer wants an
                // `App`, and one of those is a terrain grid and a tile cache
                // that this shell already owns exactly one of.
                if let Some((at, spot, fade)) = ask::map_panel(body, &self.ask) {
                    let pin = spot.id.is_none();
                    // Mid-flight the camera is between two places, so it is the
                    // flight that says where to draw, not the destination.
                    let drop = self.locator.map_or(1.0, |l| l.now().3);
                    let cam = self.chat_camera().unwrap_or(termap::ui::Camera {
                        lonlat: spot.lonlat,
                        zoom: spot.zoom,
                        tilt: 0.0,
                        persp: 0.0,
                        bearing: 0.0,
                    });
                    // The pin marks the middle of the frame, so it is only the
                    // truth while the frame is centred on the place. Somebody
                    // who has panned away is looking at somewhere else.
                    let mark = (pin && self.manual.is_none()).then_some(drop);
                    termap::ui::render_locator(f, at, &mut self.map, cam, mark);
                    let chord = self.chord.clone();
                    // Composited afterwards, like the section dissolve: the map
                    // renderer knows nothing about this page's fade, or about
                    // having no edges, and should not have to.
                    paint::feather(f, at, fade);
                    // Knocked back only where the map's edge reaches over the
                    // words. Braille under prose is unreadable at full strength,
                    // and the fix is not to move the map aside -- it is to leave
                    // a suggestion of it under the text and the whole thing
                    // everywhere else. Lighter than it was, because the map no
                    // longer covers the reading column, only breaks into it.
                    // Ramped across the overlap rather than flat over it. A
                    // flat dim is a rectangle, and a rectangle of darker map on
                    // top of a soft fade is a hard line down the middle of the
                    // screen -- which was the seam this whole shape exists to
                    // avoid. It arrives gradually now: heaviest where the words
                    // are, nothing by the time the map is on its own.
                    let over = ask::prose_rect(body, &self.ask).intersection(at);
                    paint::veil_ramp(f, over, 0.34, 1.0);
                    // On top of the dissolve, not under it: it is a read-out
                    // rather than part of the picture, and it fades on its own.
                    if let Some((said, age)) = chord {
                        ask::chord_note(f, at, &said, (1.0 - age / CHORD_SECS) as f32);
                    }
                }
                // A project, the same way and in the same place as a map: the
                // art and the scenes live in `skysheet`, which this shell
                // already holds a sheet of, and `ask.rs` holds neither.
                if let Some((at, work, fade)) = ask::work_panel(body, &self.ask) {
                    if let Some(p) = crate::mcp::project(&work.id) {
                        // The caption sits at the foot of the column, so the
                        // picture gets everything above it.
                        let stage = Rect { height: at.height.saturating_sub(6), ..at };
                        let mark = work.mark.then(|| skysheet::marks::find(&p.mark)).flatten();
                        let (dw, dh) = skysheet::scene::footprint(&p.id);
                        // Scenes are laid out by hand at a fixed size, so a
                        // stage smaller than the diagram crops it rather than
                        // scaling it. Better the mark alone than half a diagram:
                        // a picture cut off at its edge reads as a fault, and a
                        // mark is a whole thing at the only size it has.
                        let room = work.diagram && stage.width >= dw && stage.height >= dh;
                        // Stacked, not overlaid. The first version put the mark
                        // in the stage's top-left corner when both were asked
                        // for, which is inside the diagram -- so the emblem was
                        // drawn underneath a box of its own diagram and neither
                        // read.
                        let cap = mark.map_or(0, |m| m.art.rows + 2);
                        let both = room && mark.is_some() && stage.height >= dh + cap;
                        let mut drew = false;
                        if room {
                            let top = match both {
                                true => stage.y + cap + (stage.height - cap - dh) / 2,
                                false => stage.y + (stage.height - dh) / 2,
                            };
                            drew = skysheet::scene::draw(
                                f.buffer_mut(),
                                Rect {
                                    x: stage.x + (stage.width - dw) / 2,
                                    y: top,
                                    width: dw,
                                    height: dh,
                                },
                                p,
                                self.ask.t,
                            );
                        }
                        // The mark: asked for, or standing in for a diagram that
                        // had nowhere to go, because an empty column beside an
                        // answer about a project is worse than either.
                        if let Some(m) = mark.or_else(|| (!drew).then(|| skysheet::marks::find(&p.mark)).flatten())
                        {
                            let room = match (drew, both) {
                                // Above the diagram, in the strip left for it.
                                (true, true) => Rect { height: cap, ..stage },
                                // The diagram took the room; the mark waits for
                                // a wider window rather than sitting on top of
                                // it.
                                (true, false) => Rect { height: 0, ..stage },
                                (false, _) => stage,
                            };
                            if room.height > 0 {
                                skysheet::cards::mark_into(f.buffer_mut(), room, m, 1.0);
                            }
                        }
                    }
                    // The same dissolve and the same knock-back as the map. Two
                    // pictures at the side of one page should not arrive by two
                    // different rules.
                    paint::feather(f, at, fade);
                    let over = ask::prose_rect(body, &self.ask).intersection(at);
                    paint::veil_ramp(f, over, 0.34, 1.0);
                }
                ask::render(f, body, &self.ask);
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
                Span::styled(
                    match self.driving && self.section == Section::Ask {
                        true => "driving the map    n b  places    esc  give the keys back                                     everything else goes to the map",
                        false => self.section.hints(),
                    },
                    Style::default().fg(if self.driving { ACCENT } else { FAINT }),
                ),
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
        let Some(crate::ask::Panel { what: crate::ask::Show::Place(tour), .. }) = &s.ask.panel
        else {
            panic!("the tool call did not raise a map");
        };
        let spot = tour.here();
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
        let Some(crate::ask::Panel { what: crate::ask::Show::Place(tour), .. }) = &s.ask.panel
        else {
            panic!("the map vanished");
        };
        let spot = tour.here();
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

    /// Drive mode: the map's own bare keys, and escape gives them back.
    ///
    /// This exists because chasing control bytes one at a time was a losing
    /// game. `u` is VKILL and `o` is VDISCARD in the tty line discipline, and a
    /// browser claims ctrl-U and ctrl-O for itself -- so the two keys the map
    /// uses for tilt are among the least likely in the set to survive, and which
    /// ones do depends on the client. Bare keys have no such problem.
    #[test]
    fn drive_mode_hands_the_map_the_keys_it_uses_in_its_own_section() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let plain = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        s.ask.input = "where is jaipur".into();
        s.ask.submit();
        let board = s.ask.board_token().expect("no board").to_string();
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":26.91,"lon":75.79,"zoom":11.0,"label":"Jaipur"}}}"#,
        )
        .unwrap();
        s.tick(0.016);
        s.tick(3.0);

        // In by command, which is the way that cannot be swallowed.
        s.ask.input = "/drive".into();
        s.ask.submit();
        s.tick(0.016);
        assert!(s.driving(), "`/drive` did not hand over the keyboard");

        // The section's own keys, bare. These are the two that were reported
        // broken as chords.
        let flat = s.chat_camera().unwrap().tilt;
        s.on_key(plain('u'));
        let leaned = s.chat_camera().unwrap().tilt;
        assert!(leaned > flat, "bare `u` did not lean it: {flat} to {leaned}");
        s.on_key(plain('o'));
        assert!(s.chat_camera().unwrap().tilt < leaned, "bare `o` did not flatten it");

        // And the rest of the vocabulary, none of which is restated in this
        // crate: zoom, pan, bearing.
        let z = s.chat_camera().unwrap().zoom;
        s.on_key(plain('+'));
        assert!(s.chat_camera().unwrap().zoom > z, "bare `+` did not zoom");
        let lon = s.chat_camera().unwrap().lonlat.0;
        s.on_key(plain('l'));
        assert!(s.chat_camera().unwrap().lonlat.0 > lon, "bare `l` did not pan");
        s.on_key(plain('.'));
        assert_ne!(s.chat_camera().unwrap().bearing, 0.0, "bare `.` did not turn it");

        // Nothing typed into the question the whole time.
        assert_eq!(s.ask.input, "", "driving the map typed into the line");

        // Search stays behind: it is a mode inside a mode.
        s.on_key(plain('?'));
        assert!(s.map.query.is_none(), "the search box opened inside the chat");

        // Escape gives the keyboard back, and then letters are letters again.
        s.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!s.driving(), "escape did not end it");
        s.on_key(plain('u'));
        assert_eq!(s.ask.input, "u", "a letter did not go back to being a letter");
    }

    /// The same thing again, but from the bytes a terminal really sends.
    ///
    /// The synthesized-key test above proves the routing; this proves the
    /// decoding, which is the half that can be wrong without anything failing:
    /// ctrl-o is byte 0x0F and every one of these chords is a single control
    /// byte that has to survive `wire.rs` and arrive as a letter with a
    /// modifier on it.
    #[test]
    fn the_map_chords_survive_being_typed() {
        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        s.ask.input = "where is jaipur".into();
        s.ask.submit();
        let board = s.ask.board_token().expect("no board").to_string();
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":26.91,"lon":75.79,"zoom":11.0,"label":"Jaipur"}}}"#,
        )
        .unwrap();
        s.tick(0.016);
        s.tick(3.0);

        let mut decoder = crate::wire::Decoder::default();
        let mut typed = |s: &mut Shell, bytes: &[u8]| {
            for ev in decoder.feed(bytes) {
                if let crossterm::event::Event::Key(k) = ev {
                    s.on_key(k);
                }
            }
        };

        // ctrl-u, 0x15: lean over.
        let flat = s.chat_camera().unwrap().tilt;
        typed(&mut s, &[0x15]);
        let leaned = s.chat_camera().unwrap().tilt;
        assert!(leaned > flat, "ctrl-u as a byte did nothing: {flat} -> {leaned}");

        // ctrl-o, 0x0F: back the other way. This is the one that was reported
        // as not working, so it is asserted from the byte and not from a
        // KeyEvent somebody built by hand.
        typed(&mut s, &[0x0f]);
        let back = s.chat_camera().unwrap().tilt;
        assert!(back < leaned, "ctrl-o as byte 0x0f did nothing: {leaned} -> {back}");

        // And the line is untouched by all of it -- these are chords, not text.
        assert_eq!(s.ask.input, "", "a map chord typed into the question");

        // Each one leaves a read-out, which is the difference between a chord
        // that did something too small to see and one that never arrived.
        let (said, _) = s.chord.clone().expect("the chord said nothing");
        assert!(said.to_lowercase().contains("tilt"), "unhelpful read-out: {said}");
        s.tick(CHORD_SECS + 0.1);
        assert!(s.chord.is_none(), "the read-out never went away");
    }

    /// Ctrl and a key is the experience section's own map handler.
    ///
    /// Not a second set of bindings to keep in step with that one -- the key
    /// really goes to `termap::app::App::on_key`, so `u` and `o` tilt by
    /// whatever that file says a tilt step is. What this checks is the plumbing:
    /// that the camera handed over is the one the chat is looking through, that
    /// what came back is kept, and that the experience section's own camera is
    /// exactly where it was afterwards.
    #[test]
    fn ctrl_and_a_key_drives_the_map_with_the_maps_own_handler() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        s.ask.input = "where is jaipur".into();
        s.ask.submit();
        let board = s.ask.board_token().expect("no board").to_string();
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":26.91,"lon":75.79,"zoom":11.0,"label":"Jaipur"}}}"#,
        )
        .unwrap();
        s.tick(0.016);
        s.tick(3.0);

        let section = s.map.vp;
        let before = s.chat_camera().expect("no camera");
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);

        s.on_key(ctrl('u'));
        let after = s.chat_camera().expect("no camera");
        assert!(after.tilt > before.tilt, "ctrl-u did not lean it: {} to {}", before.tilt, after.tilt);
        s.on_key(ctrl('o'));
        s.on_key(ctrl('o'));
        assert!(s.chat_camera().unwrap().tilt < after.tilt, "ctrl-o did not flatten it");

        let z = s.chat_camera().unwrap().zoom;
        s.on_key(ctrl('+'));
        assert!(s.chat_camera().unwrap().zoom > z, "ctrl-+ did not zoom in");
        let lon = s.chat_camera().unwrap().lonlat.0;
        s.on_key(ctrl('l'));
        assert!(s.chat_camera().unwrap().lonlat.0 > lon, "ctrl-l did not pan east");
        s.on_key(ctrl('.'));
        assert_ne!(s.chat_camera().unwrap().bearing, 0.0, "ctrl-. did not swing the bearing");

        // Through all of that, the experience section's camera never moved.
        assert_eq!(s.map.vp.center, section.center, "the section's camera was dragged along");
        assert_eq!(s.map.vp.zoom, section.zoom);
        assert_eq!(s.map.vp.tilt, section.tilt);

        // ctrl-g hands it back to the flight.
        s.on_key(ctrl('g'));
        let back = s.chat_camera().expect("no camera");
        assert!((back.lonlat.0 - 75.79).abs() < 1e-6, "ctrl-g did not return to the place");
        assert_eq!(back.bearing, 0.0);

        // A ctrl-wheel zooms wherever the pointer is, which is why the decoder
        // had to start keeping the modifiers at all.
        let wheel = |mods| MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 2,
            row: 2,
            modifiers: mods,
        };
        let z = s.chat_camera().unwrap().zoom;
        s.on_mouse(wheel(KeyModifiers::CONTROL));
        assert!(s.chat_camera().unwrap().zoom > z, "ctrl-wheel over the words did not zoom");

        // Without ctrl, over the words, it is the transcript that scrolls.
        let z = s.chat_camera().unwrap().zoom;
        s.on_mouse(wheel(KeyModifiers::NONE));
        assert_eq!(s.chat_camera().unwrap().zoom, z, "a plain wheel moved the map");
    }

    /// A stop that names its own start is flown from there.
    ///
    /// The journey is the answer to "how far is Kapadwanj from Ahmedabad" in a
    /// way a still of the destination is not, so the camera has to set out from
    /// the place the agent named rather than from wherever the last answer left
    /// it -- otherwise it is a different journey that happens to end in the
    /// right place.
    #[test]
    fn a_stop_with_a_start_is_flown_from_it() {
        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        s.ask.input = "how far is kapadwanj from ahmedabad".into();
        s.ask.submit();
        let board = s.ask.board_token().expect("no board").to_string();

        // Somewhere else entirely first, so "from wherever the camera was" and
        // "from the named start" cannot be confused.
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":26.91,"lon":75.79,"label":"Jaipur"}}}"#,
        )
        .unwrap();
        s.tick(0.016);
        s.tick(4.0);

        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":23.020,"lon":73.070,"label":"Kapadwanj",
                            "note":"two hours out of the city",
                            "from":{"lat":23.022,"lon":72.580,"zoom":10.0}}}}"#,
        )
        .unwrap();
        s.tick(0.016);

        // The first frame of the flight is at the start it was given -- not at
        // Jaipur, where the camera was sitting.
        let ((lon, lat), _, _, _) = s.locator.expect("no camera").now();
        assert!((lon - 72.580).abs() < 0.05, "set out from lon {lon}, not the named start");
        assert!((lat - 23.022).abs() < 0.05, "set out from lat {lat}, not the named start");
        assert!(s.locator.unwrap().flying(), "a journey that was over before it began");

        // And it ends where it was told to.
        s.tick(6.0);
        let ((lon, lat), _, _, pin) = s.locator.unwrap().now();
        assert!((lon - 73.070).abs() < 1e-6 && (lat - 23.020).abs() < 1e-6, "{lon},{lat}");
        assert_eq!(pin, 1.0, "the pin never landed");

        // Without a start it still flies from wherever it is, which is the
        // ordinary case and must not have changed.
        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"lat":22.300,"lon":73.200,"label":"Vadodara"}}}"#,
        )
        .unwrap();
        s.tick(0.016);
        let ((lon, _), _, _, _) = s.locator.unwrap().now();
        assert!((lon - 73.070).abs() < 0.2, "it did not set out from where it had landed: {lon}");
    }

    /// A route: several places in one call, walked with ctrl-n and ctrl-b.
    #[test]
    fn a_route_can_be_walked_and_the_camera_follows() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut s = Shell::new();
        s.skip_boot();
        s.go(Section::Ask);
        s.ask.state = crate::ask::State::Ready;
        s.ask.input = "cool places in uttar pradesh".into();
        s.ask.submit();
        let board = s.ask.board_token().expect("no board").to_string();

        crate::mcp::handle(
            &board,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"show_map",
               "arguments":{"places":[
                 {"lat":27.175,"lon":78.010,"label":"Agra","note":"the Taj Mahal is here"},
                 {"lat":25.336,"lon":83.008,"label":"Varanasi","note":"the ghats"},
                 {"lat":26.799,"lon":82.205,"label":"Ayodhya","note":"a pilgrimage town"}
               ]}}}"#,
        )
        .unwrap();
        s.tick(0.016);

        let names = |s: &Shell| -> (String, usize, usize) {
            match &s.ask.panel {
                Some(crate::ask::Panel { what: crate::ask::Show::Place(t), .. }) => {
                    (t.here().name.clone(), t.at, t.stops.len())
                }
                _ => panic!("no route on the page"),
            }
        };
        assert_eq!(names(&s), ("Agra".into(), 0, 3), "the route did not arrive whole");
        // The note came with it -- a pin without one is worth much less.
        assert!(crate::ask::showing_place(&s.ask).unwrap().note.contains("Taj Mahal"));

        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        s.on_key(ctrl('n'));
        s.tick(0.016);
        assert_eq!(names(&s).0, "Varanasi", "ctrl-n did not move");
        assert!(s.locator.unwrap().flying(), "it cut to the next stop instead of flying");

        s.on_key(ctrl('b'));
        s.tick(0.016);
        assert_eq!(names(&s).0, "Agra", "ctrl-b did not go back");

        // A route is a loop rather than a dead end.
        s.on_key(ctrl('b'));
        assert_eq!(names(&s).0, "Ayodhya", "walking back off the start stopped dead");
        s.on_key(ctrl('n'));
        assert_eq!(names(&s).0, "Agra", "walking on off the end stopped dead");

        // And the keys stay usable while a question is being typed.
        s.ask.input = "and what about".into();
        s.on_key(ctrl('n'));
        assert_eq!(names(&s).0, "Varanasi", "the route froze while typing");
        assert_eq!(s.ask.input, "and what about", "walking the route typed into the line");
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

        // Part way across it is between the two, and pulled back from both. The
        // crossing takes as long as the path says, so the test asks the path
        // rather than a constant that no longer exists.
        let span = s.locator.unwrap().span;
        s.tick(span / 2.0);
        let (mid, mid_zoom, _, _) = s.locator.unwrap().now();
        assert!(mid.0 > 72.51 && mid.0 < 75.79, "not between the two: {mid:?}");
        assert!(mid_zoom < 11.0, "it crossed at street zoom: {mid_zoom}");

        // And it lands, exactly, rather than easing forever.
        s.tick(span);
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
            let cam =
                termap::ui::Camera { lonlat, zoom: 13.0, tilt: 0.0, persp: 0.0, bearing: 0.0 };
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
