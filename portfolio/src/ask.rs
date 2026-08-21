//! The chat, as a page rather than a terminal.
//!
//! Deliberately not a REPL. A chat client with a prompt, a scrollback and a
//! spinner is a shape everyone already has, and putting one inside a portfolio
//! adds nothing to it.
//!
//! **The page takes the width it is given**, and anything that is a picture --
//! a code, and the map and the marks when tools can ask for them -- arrives at
//! the side rather than in the middle of the prose. Not in a box: it fades up
//! out of the background and the words keep their own column. A bordered widget
//! in the centre of a terminal is a dialog, which is the look this is avoiding.
//!
//! **The wait is the answer arriving.** A run of glyphs churns at the head of
//! what has been written and settles as the tokens land behind it, so the motion
//! is bounded by the reply rather than by a timer -- and when there is no reply
//! coming, there is nothing moving at all.
//!
//! Everything that moves is a pure function of a clock, like the rest of this
//! app, so a frame can be snapshotted at an exact moment and looked at.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::acp::{self, Call, Event, Status};
use crate::paint::{wrap, ACCENT, CYAN, DIM, FAINT, FG};

/// The longest question anybody can ask.
///
/// Counted in words rather than characters because a question is a question
/// whatever its spelling, and because the thing being kept out is a pasted
/// document rather than a long sentence.
pub const MAX_WORDS: usize = 1000;

/// Words in a line, by whitespace. Counts the word being typed, so the limit
/// bites at the thousandth word and not part way through it.
pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

#[derive(Debug, Clone, Default)]
pub struct Turn {
    pub q: String,
    pub a: String,
    /// The last thing it said it was doing, shown small. Not the answer.
    pub thought: String,
    /// Every tool call this turn made, in the order they started, kept after
    /// the answer arrives. Watching it reach for things is half the point,
    /// and a reader who scrolls back should still see what it looked at.
    pub calls: Vec<Call>,
    /// A code to draw under the answer. The first thing on this page that is a
    /// picture rather than prose, and the shape the tool-driven panels will take
    /// when they arrive: a turn carries what to show, the renderer knows how.
    pub code: Option<&'static crate::coffee::Code>,
    pub done: bool,
    /// Stopped at the visitor's request rather than finished.
    pub cancelled: bool,
}

/// Something showing at the side of the page.
///
/// Not in a box and not in the middle: it arrives at the edge, fades up out of
/// the background, and the prose keeps its own column. A terminal is a canvas
/// and a bordered widget in the centre of one is a dialog box -- which is the
/// look this is trying not to have.
#[derive(Debug, Clone)]
pub struct Panel {
    pub what: Show,
    /// Seconds in the current state. Drives both fades, and stops mattering
    /// once one is over -- nothing here loops.
    pub since: f64,
    pub life: Life,
}

/// Arriving, sitting there, or on its way out.
///
/// A panel that can only arrive is a panel that never leaves, which is what
/// this was: the map from the first place question stayed beside every answer
/// after it until `/clear`. Leaving is a state rather than a deletion so the
/// last frames of it are still drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Life {
    Arriving,
    Held,
    Leaving,
}

/// What a panel is showing.
///
/// The project marks belong here too and will arrive with the tools that ask
/// for them -- adding a variant is the whole change, because the layout above
/// already keeps a column free for it.
#[derive(Debug, Clone)]
pub enum Show {
    Code(&'static crate::coffee::Code),
    /// A map of one or more real points, drawn by the map renderer the
    /// experience section uses. This file does not draw it: it has no `App` and
    /// should not have one, so it says where the picture goes and the shell puts
    /// it there.
    Place(Tour),
    /// The certification badge, and the code that verifies it.
    Cert,
}

/// A route the visitor can walk, one stop at a time.
///
/// One place is a tour of length one, so there is a single shape to draw rather
/// than a special case for the common thing.
#[derive(Debug, Clone, PartialEq)]
pub struct Tour {
    pub stops: Vec<Spot>,
    pub at: usize,
}

impl Tour {
    pub fn one(spot: Spot) -> Tour {
        Tour { stops: vec![spot], at: 0 }
    }

    pub fn here(&self) -> &Spot {
        &self.stops[self.at.min(self.stops.len() - 1)]
    }

    /// Step, wrapping. A route is a loop rather than a dead end: walking off the
    /// last stop of five and being told no is a worse answer than going back to
    /// the first.
    fn step(&mut self, by: i32) {
        let n = self.stops.len() as i32;
        if n > 1 {
            self.at = (self.at as i32 + by).rem_euclid(n) as usize;
        }
    }
}

/// Somewhere on the map, and what to say underneath it.
#[derive(Debug, Clone, PartialEq)]
pub struct Spot {
    /// The tour stop this is, when it is one. `/map` flies there, and the map
    /// draws the stop's own pin, so the thumbnail does not add a second marker.
    pub id: Option<String>,
    pub name: String,
    /// One line under the picture: role and years for a stop, or what the
    /// address lookup made of a visitor.
    pub note: String,
    pub lonlat: (f64, f64),
    pub zoom: f64,
    /// Where to set out from, as ((lon, lat), zoom), when the agent asked for a
    /// journey rather than a destination.
    pub from: Option<((f64, f64), f64)>,
}

/// What this file knows about the world, so a question can be turned into a
/// point without reaching for the map itself.
///
/// The places are the same sheet the experience tour flies; `covers` is the
/// basemap's extent, and it is here because a thumbnail of somewhere the
/// archive has no tiles for is a black rectangle with a caption under it.
pub struct Atlas {
    pub places: Vec<termap::place::Place>,
    pub covers: Option<[f64; 4]>,
}

impl Default for Atlas {
    fn default() -> Self {
        Atlas { places: termap::place::load(), covers: None }
    }
}

impl Atlas {
    fn drawable(&self, lonlat: (f64, f64)) -> bool {
        let Some([x0, y0, x1, y1]) = self.covers else { return false };
        let [x, y] = termap::geo::lonlat_to_world(lonlat.0, lonlat.1);
        x >= x0 && x <= x1 && y >= y0 && y <= y1
    }
}

/// A stop as something to draw.
fn spot_of(p: &termap::place::Place) -> Spot {
    let note = [p.role.as_str(), p.years.as_str(), p.where_.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  \u{b7}  ");
    Spot {
        id: Some(p.id.clone()),
        name: p.name.clone(),
        note,
        lonlat: p.lonlat,
        // The sheet describes places, not journeys.
        from: None,
        // A little further out than the tour lands: the tour arrives at a
        // building and this is answering "where is that", which is a question
        // about a neighbourhood.
        zoom: (p.zoom - 1.4).max(9.0),
    }
}

/// Every word that should find a given stop.
///
/// The sheet's own vocabulary, including `kind` -- "where did he go to
/// university" names Silver Oak without saying Silver Oak, and that is the
/// commonest way anybody asks.
fn aliases(p: &termap::place::Place) -> Vec<String> {
    let mut out = vec![
        p.name.to_ascii_lowercase(),
        p.id.replace('-', " "),
        p.kind.to_ascii_lowercase(),
    ];
    // The city, not the state: "gujarat" is every stop on the sheet and would
    // make the panel a coin toss.
    if let Some(city) = p.where_.split(',').next() {
        let city = city.trim().to_ascii_lowercase();
        if !city.is_empty() {
            out.push(city);
        }
    }
    out.retain(|a| !a.is_empty());
    out
}

/// Whether `hay` says `needle`, as a word rather than as a run of letters.
///
/// Plain containment is not usable here and the failures are not subtle: "this"
/// contains "his", so every question with the word `this` in it read as a
/// question about him, and "network" contains "work". Both fired the panel on
/// questions that had nothing to do with anywhere.
fn mentions(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let edge = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    let mut from = 0;
    while let Some(at) = hay[from..].find(needle) {
        let at = from + at;
        let before = hay[..at].chars().next_back();
        let after = hay[at + needle.len()..].chars().next();
        if edge(before) && edge(after) {
            return true;
        }
        from = at + 1;
    }
    false
}

/// Asking about the person at the keyboard rather than about Prince.
fn about_you(q: &str) -> bool {
    [
        "where am i",
        "where i am",
        "where are we",
        "where do i live",
        "where am i located",
        "my location",
        "my city",
        "locate me",
        "where is my",
    ]
    .iter()
    .any(|w| q.contains(w))
}

/// A question about where something is, as opposed to what it was.
fn about_where(q: &str) -> bool {
    ["where", "located", "location", "city", "based", "commute"]
        .iter()
        .any(|w| mentions(q, w))
}

/// ...and about him, so "where are you from" draws a map and "where does this
/// data come from" does not.
fn about_him(q: &str) -> bool {
    ["he", "hes", "him", "his", "prince", "you", "your", "yours"]
        .iter()
        .any(|w| mentions(q, w))
}

/// A question about the certification.
///
/// Narrow on purpose, like the place trigger: `certified` and `certification`
/// both stem to `certif`, and the rest are the words somebody actually uses for
/// a badge. "Is he certified" draws it; "is this map certified accurate" does
/// not, because that one is not about him -- so the subject is checked too.
pub fn asks_about_the_cert(q: &str) -> bool {
    let text = q.to_ascii_lowercase();
    let named = text.contains("certif")
        || mentions(&text, "credential")
        || mentions(&text, "credentials")
        || mentions(&text, "credly")
        || mentions(&text, "badge")
        || text.contains("claude certified");
    named && (about_him(&text) || mentions(&text, "anthropic") || mentions(&text, "credly"))
}

/// The place a question is about, if it is about one.
///
/// Worth naming as a stopgap: the right shape is a tool the agent calls, so the
/// picture follows the answer rather than guessing ahead of it, and that needs
/// the MCP server this does not have yet. Reading the question is what can be
/// built today, and it has one advantage the tool will not -- the map is up
/// while the answer is still arriving.
///
/// Two ways in, both deliberate, because a panel that fires on anything is
/// furniture: a place is named, or it is a location question about him. "What
/// did he build at Gateway" draws Gateway. "What is he good at" draws nothing.
pub fn spot_for(q: &str, atlas: &Atlas) -> Option<Spot> {
    let text = q.to_ascii_lowercase();

    // First, because "where am i" is also a question with "where" in it, and
    // answering it with somebody else's office would be a strange thing to do.
    if about_you(&text) {
        let w = crate::visits::here()?;
        let lonlat = (w.lon, w.lat);
        if !atlas.drawable(lonlat) {
            return None;
        }
        return Some(Spot {
            id: None,
            name: w.label(),
            note: "as near as an address can place you".into(),
            lonlat,
            from: None,
            // A city and the country around it: the lookup is accurate to
            // about that, and a street view of a guess is a lie about it.
            zoom: 9.5,
        });
    }

    // Newest first: a city names four stops on this sheet and the current one
    // is the better answer to "is he in Ahmedabad".
    if let Some(p) = atlas
        .places
        .iter()
        .rev()
        .find(|p| aliases(p).iter().any(|a| mentions(&text, a)))
    {
        return Some(spot_of(p));
    }

    if about_where(&text) && about_him(&text) {
        return atlas.places.last().map(spot_of);
    }
    None
}

impl Panel {
    pub fn new(what: Show) -> Panel {
        Panel { what, since: 0.0, life: Life::Arriving }
    }

    /// How visible it is, 0 to 1. Smoothstepped so it comes up and goes down
    /// softly rather than snapping at either end.
    pub fn fade(&self) -> f32 {
        const OVER: f64 = 0.55;
        let x = (self.since / OVER).clamp(0.0, 1.0) as f32;
        let eased = x * x * (3.0 - 2.0 * x);
        match self.life {
            Life::Arriving => eased,
            Life::Held => 1.0,
            Life::Leaving => 1.0 - eased,
        }
    }

    /// Whether a fade is still running, so the shell knows to keep drawing.
    pub fn moving(&self) -> bool {
        self.life != Life::Held
    }

    /// Start leaving, from wherever it is now. Already leaving is left alone --
    /// restarting the fade would make it brighten first.
    fn leave(&mut self) {
        if self.life != Life::Leaving {
            self.life = Life::Leaving;
            self.since = 0.0;
        }
    }

    /// Advance, and say whether it has finished leaving and should be dropped.
    fn step(&mut self, dt: f64) -> bool {
        self.since += dt;
        const OVER: f64 = 0.55;
        match self.life {
            Life::Arriving if self.since >= OVER => {
                self.life = Life::Held;
                false
            }
            Life::Leaving if self.since >= OVER => true,
            _ => false,
        }
    }
}

/// Everything `/` offers. Name, what it does, and whether it takes an argument.
///
/// One table: the palette lists it, the fuzzy filter searches it, `/help` prints
/// it, and `submit` dispatches from it. A command that exists in one of those
/// four places and not the others is the bug this shape prevents.
pub const COMMANDS: &[(&str, &str)] = &[
    ("/help", "what all of this does"),
    ("/clear", "empty the conversation"),
    ("/coffee", "buy him one -- add `card` for the other code"),
    ("/cert", "the certification, and how to check it"),
    ("/reach", "leave him a message, agent or no agent"),
    ("/map", "go to the places -- add a name to land on one"),
    ("/projects", "go to the work"),
    ("/skills", "go to the tools"),
    ("/taste", "go to the room"),
    ("/home", "back to the start"),
    ("/whoami", "where you are connecting from"),
    ("/uptime", "how long this box has been answering"),
    ("/keys", "what the keyboard does here"),
    ("/theme", "warm or cool"),
];

/// Which command goes to which section.
///
/// The two names are not the same word and never were: the chat offers `/map`
/// because that is what a visitor is asking for, and the shell calls the
/// section `experience` because that is what is in it. Keeping the pairing in
/// one table is the fix for the bug where `/map` quietly did nothing -- the
/// command was dispatched by trimming its slash and hoping a section answered
/// to the result, and only four of the five did.
pub const NAV: &[(&str, &str)] = &[
    ("/map", "experience"),
    ("/projects", "projects"),
    ("/skills", "skills"),
    ("/taste", "taste"),
    ("/home", "home"),
];

/// What `local` did with a line.
enum Local {
    /// Answered here, and this is the answer.
    Said(String),
    /// Handled here, and there is nothing to say -- the screen changing is the
    /// reply. Emphatically not the same as `Not`: this line must not reach the
    /// model.
    Done,
    /// Not a command this file knows. Goes to the agent like any other question.
    Not,
}

/// Commands matching what has been typed, best first.
///
/// Fuzzy in the way that is actually useful at this size: a subsequence match,
/// so `/pj` finds `/projects` and `/cf` finds `/coffee`, ranked by how tightly
/// the letters sat together. Nothing here needs a scoring library -- there are
/// thirteen commands and the list has to feel instant, not clever.
pub fn matches(typed: &str) -> Vec<(&'static str, &'static str)> {
    let needle: String = typed.trim_start_matches('/').to_ascii_lowercase();
    let mut hits: Vec<(usize, (&'static str, &'static str))> = Vec::new();
    for (name, help) in COMMANDS {
        let hay = name.trim_start_matches('/');
        if needle.is_empty() {
            hits.push((0, (name, help)));
            continue;
        }
        if let Some(score) = subsequence(hay, &needle) {
            hits.push((score, (name, help)));
        }
    }
    hits.sort_by_key(|(score, (name, _))| (*score, name.len()));
    hits.into_iter().map(|(_, hit)| hit).collect()
}

/// How spread out `needle` is inside `hay`, or `None` if it is not in there in
/// order. Lower is tighter, so a prefix scores best.
fn subsequence(hay: &str, needle: &str) -> Option<usize> {
    let hay: Vec<char> = hay.chars().collect();
    let mut at = 0;
    let mut first = None;
    let mut gaps = 0;
    for want in needle.chars() {
        let found = hay[at..].iter().position(|c| c.eq_ignore_ascii_case(&want))?;
        if first.is_none() {
            first = Some(at + found);
        } else {
            gaps += found;
        }
        at += found + 1;
    }
    Some(first.unwrap_or(0) + gaps)
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Nothing spawned yet — the agent starts on first visit, not at boot.
    Cold,
    Starting,
    Ready,
    Thinking,
    Failed(String),
}

pub struct Ask {
    client: Option<acp::Ask>,
    pub state: State,
    pub input: String,
    pub turns: Vec<Turn>,
    /// What the handshake settled on, once it has. Drives the header: the
    /// section can say which server and which protocol version answered rather
    /// than implying there is only one possibility.
    pub link: Option<acp::Ready>,
    /// Seconds since the section opened. Everything that moves reads this, so
    /// the whole page moves together.
    pub t: f64,
    /// Rows scrolled back from the newest exchange. Zero is the bottom, and
    /// submitting anything returns there.
    scroll: usize,
    /// How many rows the last frame could have scrolled. Kept from the render
    /// so the key handler can clamp without laying the page out twice.
    reach: std::cell::Cell<usize>,
    /// Questions asked this session, oldest first, for the up arrow.
    history: Vec<String>,
    /// Where the up arrow has walked to. `None` is the live line.
    hist_at: Option<usize>,
    /// The command palette, open while the line starts with `/`.
    pick: usize,
    /// What is showing at the side, and when it arrived, for the fade.
    pub panel: Option<Panel>,
    /// A section the chat has asked the shell to move to. Drained by the shell,
    /// which owns what has the screen; this file only knows it was asked for.
    pub goto: Option<String>,
    /// The tour stop to open on, when the move is to the map and there is a
    /// place in mind. Drained with `goto`, and meaningless without it.
    pub goto_place: Option<String>,
    /// The places, and how much of the world the basemap has. Populated by the
    /// shell, which owns the map and therefore knows its extent.
    pub atlas: Atlas,
    /// Where tool calls arrive. The agent decides what goes on this page; this
    /// is the wire it decides down.
    orders: Option<std::sync::mpsc::Receiver<crate::mcp::Directive>>,
    /// The token the tool server addresses this page by, kept so the session can
    /// be forgotten when the page goes.
    board: Option<String>,
    /// Whether the answer in flight has asked for a panel. A turn that ends
    /// without asking is a turn that does not want one, and whatever is showing
    /// goes.
    directed: bool,
    /// Set the first time a tool call arrives. From then on the question is not
    /// second-guessed -- see `look`.
    pub agent_drives: bool,
    /// Exchanges finished since the last drain, waiting to be written to the
    /// visit log. Kept here rather than logged from here: this file draws a
    /// page, and what the log does with a finished turn is `session.rs`'s
    /// business and the same for both transports.
    logged: Vec<(String, String)>,
}

impl Default for Ask {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Ask {
    /// Stop the tool server addressing a page that is gone.
    ///
    /// Without this the table in `mcp.rs` grows by one entry for every visitor
    /// for the life of the process, and a late tool call from an agent that has
    /// not noticed its session ended is answered rather than refused.
    fn drop(&mut self) {
        if let Some(board) = &self.board {
            crate::mcp::forget(board);
        }
    }
}

impl Ask {
    pub fn new() -> Ask {
        Ask {
            client: None,
            state: State::Cold,
            input: String::new(),
            turns: Vec::new(),
            link: None,
            t: 0.0,
            scroll: 0,
            reach: std::cell::Cell::new(0),
            history: Vec::new(),
            hist_at: None,
            pick: 0,
            panel: None,
            goto: None,
            goto_place: None,
            atlas: Atlas::default(),
            orders: None,
            board: None,
            directed: false,
            agent_drives: false,
            logged: Vec::new(),
        }
    }

    /// Called when the section is first opened. Spawning a language model to
    /// render a landing page would be rude to both the machine and the account.
    pub fn wake(&mut self, context: &str) {
        if self.client.is_some() {
            return;
        }
        // The tool server first: the agent is handed a URL at `session/new`,
        // so it has to exist before the agent is spawned.
        crate::mcp::serve();
        crate::mcp::warm_index();
        let (tx, rx) = std::sync::mpsc::channel();
        let board = crate::mcp::register(tx, crate::visits::here_slot());
        self.orders = Some(rx);
        self.board = Some(board.clone());
        self.client = Some(acp::Ask::spawn(context.to_string(), Some(board)));
        self.state = State::Starting;
    }

    /// The token the tool server addresses this page by, for the tests that
    /// drive a tool call the way an agent would.
    #[cfg(test)]
    pub fn board_token(&self) -> Option<&str> {
        self.board.as_deref()
    }

    /// Finish the turn in flight, as `Event::Done` would. For tests, which have
    /// no agent to send one.
    #[cfg(test)]
    pub fn finish_for_test(&mut self) {
        self.apply(acp::Event::Done);
    }

    /// Exchanges finished since the last call, for the visit log.
    pub fn drain_logged(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.logged)
    }

    pub fn busy(&self) -> bool {
        matches!(self.state, State::Starting | State::Thinking)
    }

    pub fn tick(&mut self, dt: f64) {
        self.t += dt;
        if self.panel.as_mut().is_some_and(|p| p.step(dt)) {
            self.panel = None;
        }
        // Tool calls before agent events, so a `show_map` that arrived in the
        // same instant as the answer finishing is not treated as a turn that
        // never asked for one.
        let orders: Vec<_> =
            self.orders.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for d in orders {
            self.obey(d);
        }
        let Some(c) = &self.client else { return };
        for e in c.poll() {
            self.apply(e);
        }
    }

    /// Do what the agent asked.
    fn obey(&mut self, d: crate::mcp::Directive) {
        use crate::mcp::Directive;
        self.agent_drives = true;
        // A row on the page is not a request for a panel: a turn whose only
        // tool call was a lookup that found nothing still wants whatever is
        // showing to go away.
        if !matches!(d, Directive::Called { .. }) {
            self.directed = true;
        }
        match d {
            // Straight onto the turn in flight, as a tool call with a name on
            // it. The ACP stream carries the same call with no name at all, so
            // this replaces that row rather than adding a second one -- keyed by
            // the tool and its argument, which is what makes them the same call.
            Directive::Called { tool, detail } => {
                if let Some(t) = self.turns.last_mut() {
                    let id = format!("ours-{}-{}", tool, t.calls.len());
                    t.calls.push(Call { id, title: tool, status: Status::Done, detail });
                }
            }
            Directive::Map { stops } => {
                let stops: Vec<Spot> = stops
                    .into_iter()
                    .map(|s| Spot {
                        // Not a stop on the experience sheet, so `/map` has
                        // nowhere specific to fly and the thumbnail draws its
                        // own pin.
                        id: None,
                        name: if s.label.trim().is_empty() {
                            "here".to_string()
                        } else {
                            s.label
                        },
                        note: s.note,
                        lonlat: (s.lon, s.lat),
                        zoom: s.zoom,
                        from: s.from.map(|(lat, lon, zoom)| ((lon, lat), zoom)),
                    })
                    .collect();
                if stops.is_empty() {
                    return;
                }
                let tour = Tour { stops, at: 0 };
                match &mut self.panel {
                    // Already showing a map: change where it is looking rather
                    // than fading one out and another in. The flight is the
                    // shell's -- see `Locator` -- and a cross-fade would hide
                    // exactly the motion that makes the move legible.
                    Some(p @ Panel { what: Show::Place(_), .. }) => {
                        p.what = Show::Place(tour);
                        if p.life == Life::Leaving {
                            p.life = Life::Arriving;
                            p.since = 0.0;
                        }
                    }
                    _ => self.panel = Some(Panel::new(Show::Place(tour))),
                }
            }
            Directive::Clear => {
                if let Some(p) = &mut self.panel {
                    p.leave();
                }
            }
        }
    }

    /// Fold one event from the agent into the page.
    ///
    /// Split out of `tick` so it can be driven from recorded events: this
    /// machine cannot reach a model, and the ordering rules here -- an update
    /// landing on the call that opened, an answer appending to the newest turn
    /// -- are exactly the part worth checking.
    fn apply(&mut self, e: Event) {
        {
            match e {
                Event::Ready(r) => {
                    // To the log, not to the page. Whoever runs this wants to
                    // know which tier and which server came up; the visitor
                    // wants to ask a question.
                    eprintln!(
                        "portfolio: agent ready -- tier `{}` via `{}`, acp v{}{}",
                        r.tier,
                        r.server,
                        r.version,
                        if r.mode.is_empty() {
                            String::new()
                        } else {
                            format!(", {} mode", r.mode)
                        }
                    );
                    // The agent has the tools, so the guess stands down --
                    // before the first question, not after the first tool call.
                    self.agent_drives = r.tools;
                    self.link = Some(r);
                    if self.state == State::Starting {
                        self.state = State::Ready;
                    }
                }
                Event::Chunk(s) => {
                    if let Some(t) = self.turns.last_mut() {
                        t.a.push_str(&s);
                    }
                }
                Event::Thought(s) => {
                    if let Some(t) = self.turns.last_mut() {
                        // Only the latest line: the point is to show that
                        // something is happening, not to publish a transcript
                        // of the model's reasoning to a stranger.
                        t.thought = s.lines().last().unwrap_or("").trim().to_string();
                    }
                }
                Event::Tool(c) => {
                    // A row with no name on it says nothing. ACP's `ToolCall`
                    // carries no tool name and Copilot leaves the title empty,
                    // so these arrived as `\u{2713} tool` -- three of them in a
                    // row, telling a visitor only that *something* happened.
                    if c.title.trim().is_empty() && c.detail.trim().is_empty() {
                        return;
                    }
                    // And a row for a tool we serve is a *second* row for a call
                    // we already describe better. The agent's version carries
                    // the name it renamed ours to and no arguments; ours carries
                    // what it was actually asked for. Eight lookups in a turn
                    // came out as eight `portfolio-locate_place` beside eight
                    // `locate_place  Ward's Lake` -- the same eight calls,
                    // twice, once uselessly.
                    if crate::gates::ours(&c.title) {
                        return;
                    }
                    if let Some(t) = self.turns.last_mut() {
                        // Updates arrive under the same id as the call that
                        // opened, so this is an upsert rather than a push --
                        // otherwise one fetch becomes three rows as it moves
                        // from pending to running to completed.
                        match t.calls.iter_mut().find(|e| e.id == c.id) {
                            Some(e) => *e = c,
                            None => t.calls.push(c),
                        }
                    }
                }
                Event::Done => {
                    if let Some(t) = self.turns.last_mut() {
                        t.done = true;
                        self.logged.push((t.q.clone(), t.a.clone()));
                    }
                    // The answer is in and it never asked for a picture, so
                    // whatever is beside it belongs to an older question. It
                    // goes -- but only now, not when the question was asked:
                    // clearing on submit empties the column and then refills it,
                    // and a follow-up about the same place should not flicker.
                    if !self.directed {
                        if let Some(p) = &mut self.panel {
                            p.leave();
                        }
                    }
                    self.state = State::Ready;
                }
                // Stopped on purpose, so the section goes back to ready rather
                // than to failed -- nothing is wrong and the tier is fine.
                Event::Cancelled => {
                    if let Some(t) = self.turns.last_mut() {
                        t.done = true;
                        t.cancelled = true;
                        if t.a.trim().is_empty() {
                            t.a = "Stopped.".into();
                        }
                    }
                    self.state = State::Ready;
                }
                Event::Failed(m) => {
                    if let Some(t) = self.turns.last_mut() {
                        t.done = true;
                    }
                    self.state = State::Failed(m);
                }
            }
        }
    }

    pub fn submit(&mut self) {
        let q = self.input.trim().to_string();
        if q.is_empty() {
            return;
        }
        // Everything waits for the answer in flight -- except leaving. A
        // command that only moves the screen needs nothing from the agent, and
        // swallowing `/map` because a reply is still arriving is the section
        // ignoring somebody who has decided to go and look at the map instead.
        // The rest wait because the rest write into the transcript the answer
        // is landing in.
        let leaving = NAV.iter().any(|(cmd, _)| q.split(' ').next() == Some(*cmd));
        if self.busy() && !leaving {
            return;
        }
        if q.starts_with('/') {
            match self.local(&q) {
                Local::Said(said) => {
                    self.history.push(q.clone());
                    self.hist_at = None;
                    self.logged.push((q.clone(), said.clone()));
                    self.turns.push(Turn { q, a: said, done: true, ..Default::default() });
                    self.input.clear();
                    self.scroll = 0;
                    return;
                }
                // Handled, and nothing to say. Falling through here is what sent
                // `/map` to a language model as a question.
                Local::Done => {
                    self.history.push(q);
                    self.hist_at = None;
                    self.scroll = 0;
                    return;
                }
                Local::Not => {}
            }
        }
        // Nothing asked for a panel yet this turn. Whatever is on screen stays
        // until the answer says otherwise.
        self.directed = false;
        self.look(&q);
        let Some(c) = &self.client else { return };
        c.send(&q);
        self.history.push(q.clone());
        self.hist_at = None;
        self.turns.push(Turn { q, ..Default::default() });
        self.input.clear();
        self.state = State::Thinking;
        // Always show the newest exchange; the alternative is typing a question
        // and watching nothing happen because you were scrolled up.
        self.scroll = 0;
    }

    /// Commands answered here, without troubling the agent.
    ///
    /// `Not` means "not one of ours", and the line goes to the model like any
    /// other question -- so a stray slash is a question about a slash rather
    /// than an error message.
    ///
    /// `Done` is the distinction this had to grow: a command that is handled
    /// here and has nothing to say. `/map` moves the screen and `/clear` empties
    /// it, and neither wants a reply in the transcript. Both of them used to
    /// return the same `None` as an unknown command, so both were **also sent to
    /// the model** -- `/clear` wiped the page and then asked a language model
    /// what `/clear` meant.
    fn local(&mut self, q: &str) -> Local {
        let (name, arg) = q.split_once(' ').unwrap_or((q, ""));
        Local::Said(match name {
            "/help" => {
                let mut s = String::from(
                    "Type a question and press enter. `/` opens the commands; the \
                     arrows walk them, tab takes one. Up on an empty line brings \
                     back what you asked before, the wheel and page up scroll, and \
                     escape stops an answer that is still coming.\n\n",
                );
                for (cmd, help) in COMMANDS {
                    s.push_str(&format!("{cmd}  --  {help}\n"));
                }
                s.push_str(
                    "\nThere is more it can do than this list: the rest is reached \
                     by asking for it.",
                );
                s
            }
            // Handled here rather than by the agent. A message for Prince
            // should arrive whether or not a model is up, whether or not it is
            // out of quota, and word for word rather than as something's
            // summary of it. Same for a payment string.
            "/reach" => {
                self.leave(arg.trim());
                return Local::Done;
            }
            "/coffee" => {
                self.coffee(arg.trim());
                return Local::Done;
            }
            "/cert" => {
                self.cert();
                return Local::Done;
            }
            "/clear" => {
                self.turns.clear();
                self.panel = None;
                self.scroll = 0;
                self.input.clear();
                return Local::Done;
            }
            "/keys" => "enter  ask          /  commands     up  what you asked before\n\
                        esc    stop         tab  take one    wheel / pgup  scroll\n\
                        ctrl-u clear the line              tab  leave the section\n\
                        \n\
                        hold ctrl and the map answers to the keys it does in the\n\
                        experience section -- hjkl or the arrows pan, + and - zoom,\n\
                        u and o tilt, , and . swing it round, and the layer keys\n\
                        work too. ctrl and the wheel zooms from anywhere.\n\
                        ctrl-n next place   ctrl-b  back    ctrl-g  give it back"
                .into(),
            "/whoami" => {
                let w = crate::visits::last_seen();
                match w {
                    Some(where_) => format!(
                        "You look like you are connecting from {where_}. That is an \
                         address lookup, so it is a guess with a city's worth of \
                         precision and no better."
                    ),
                    None => "Somewhere this box could not place. A private address, \
                             or a lookup that did not come back."
                        .into(),
                }
            }
            "/uptime" => {
                let secs = crate::visits::uptime();
                let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
                let mut s = String::from("This has been answering for ");
                if d > 0 {
                    s.push_str(&format!("{d} day{}, ", if d == 1 { "" } else { "s" }));
                }
                if d > 0 || h > 0 {
                    s.push_str(&format!("{h} hour{}, ", if h == 1 { "" } else { "s" }));
                }
                s.push_str(&format!("{m} minute{}.", if m == 1 { "" } else { "s" }));
                s
            }
            "/theme" => {
                let warm = crate::paint::flip_theme();
                format!("{}.", if warm { "Warmer" } else { "Cooler" })
            }
            // Handled by the shell, which owns which section has the screen.
            //
            // The section a command names is not always the section's own name:
            // `/map` goes to `experience`, and for a while it went nowhere at
            // all, because the shell looks a section up by label and no section
            // is called "map". The table is the single place that pairing is
            // written down, and a test walks it.
            _ if NAV.iter().any(|(cmd, _)| *cmd == name) => {
                let to = NAV.iter().find(|(cmd, _)| *cmd == name).map(|(_, s)| *s);
                self.goto = to.map(str::to_string);
                // `/map gateway`, or `/map` with a place already at the side:
                // the flight opens on that stop rather than at the beginning of
                // the tour.
                if name == "/map" {
                    self.goto_place = self.aimed_at(arg);
                }
                self.input.clear();
                return Local::Done;
            }
            _ => {
                let _ = arg;
                return Local::Not;
            }
        })
    }

    /// Which stop `/map` should open on: the one named on the line, else the one
    /// already showing at the side, else none and the tour starts where it does.
    fn aimed_at(&self, arg: &str) -> Option<String> {
        let arg = arg.trim().to_ascii_lowercase();
        if !arg.is_empty() {
            if let Some(p) = self
                .atlas
                .places
                .iter()
                .rev()
                .find(|p| aliases(p).iter().any(|a| !a.is_empty() && a.contains(&arg)))
            {
                return Some(p.id.clone());
            }
        }
        match &self.panel {
            Some(Panel { what: Show::Place(tour), .. }) => tour.here().id.clone(),
            _ => None,
        }
    }

    /// Put a picture at the side, if the question called for one.
    ///
    /// The credential first, because it is the narrower match: a question about
    /// a badge is about a badge, and one that also happens to contain a place
    /// name should still draw the badge.
    fn look(&mut self, q: &str) {
        // Once the agent has driven this page even once, stop reading the
        // question. Guessing was only ever a stand-in for a model that could
        // not ask, and two opinions about what belongs on screen fight: the
        // keyword match would raise Gateway Corp while the agent was flying the
        // map to Jaipur. Tiers that cannot reach the tool server keep the guess.
        if self.agent_drives {
            return;
        }
        if asks_about_the_cert(q) {
            if !matches!(&self.panel, Some(Panel { what: Show::Cert, .. })) {
                self.panel = Some(Panel::new(Show::Cert));
            }
            return;
        }
        if let Some(spot) = spot_for(q, &self.atlas) {
            // Not re-raised when it is already the thing on screen: a second
            // question about the same place should leave the picture alone
            // rather than fade it in again underneath the answer.
            let same =
                matches!(&self.panel, Some(Panel { what: Show::Place(t), .. }) if *t.here() == spot);
            if !same {
                self.panel = Some(Panel::new(Show::Place(Tour::one(spot))));
            }
        }
    }

    /// Put a code on the page.
    ///
    /// Handled here rather than by the agent, like `/reach`: a payment string is
    /// the one kind of text that must arrive exactly as written, and a model in
    /// the middle of it is a model that can get a digit wrong.
    fn coffee(&mut self, which: &str) {
        // Looked up in the generated table rather than matched by hand, so
        // adding a code to the script is the only edit adding a code takes.
        let code = crate::coffee::ALL
            .iter()
            .find(|(key, _)| *key == which)
            .map(|(_, c)| *c)
            .unwrap_or(&crate::coffee::UPI);
        let said = format!(
            "If any of this was worth something to you. {} -- or type it: {}\n\n\
             The other one is /coffee card.",
            code.how, code.payload
        );
        self.logged.push((format!("/coffee {which}").trim_end().to_string(), said.clone()));
        self.panel = Some(Panel::new(Show::Code(code)));
        self.turns.push(Turn {
            q: if which.is_empty() { "/coffee".into() } else { format!("/coffee {which}") },
            a: said,
            code: Some(code),
            done: true,
            ..Default::default()
        });
        self.input.clear();
        self.scroll = 0;
    }

    /// Put the badge on the page.
    ///
    /// Answered here rather than by the agent for the same reason `/coffee` is:
    /// a verification link has to arrive exactly as issued. A model in the
    /// middle of a URL is a model that can drop a character out of a UUID, and
    /// the failure is silent -- a link that goes nowhere, in somebody else's
    /// browser, and nobody tells you.
    fn cert(&mut self) {
        let said = format!(
            "{} -- {}, issued by {}. It is a public badge, so it can be checked \
             without taking my word for it: {}",
            crate::cert::NAME,
            crate::cert::TIER.to_ascii_lowercase(),
            crate::cert::ISSUER,
            crate::cert::SHOWN,
        );
        self.logged.push(("/cert".to_string(), said.clone()));
        self.panel = Some(Panel::new(Show::Cert));
        self.turns.push(Turn { q: "/cert".into(), a: said, done: true, ..Default::default() });
        self.input.clear();
        self.scroll = 0;
    }

    /// Put a message in the file, and answer in the transcript so it reads as
    /// part of the conversation rather than as a status bar somewhere.
    fn leave(&mut self, body: &str) {
        use crate::reach::Sent;
        // What went on the line, rebuilt rather than kept: `local` splits the
        // command off the message, and the transcript should read back the way
        // it was typed.
        let line = if body.is_empty() { "/reach".to_string() } else { format!("/reach {body}") };
        let said = match crate::reach::leave("", body, &crate::reach::origin()) {
            Sent::Ok => "Left with him. He reads these by hand, so it may be a \
                         few days -- and there is no reply address unless you \
                         put one in the message."
                .to_string(),
            Sent::Empty => "Nothing to send. `/reach` and then what you want to say.".to_string(),
            Sent::TooLong(n) => format!(
                "That is {n} characters and the limit is {}. Shorten it, or use the \
                 email address on the home page.",
                crate::reach::MAX_LEN
            ),
            Sent::Unwritable => "That did not save -- the message box is not \
                                    reachable from here. The email address on the \
                                    home page still works."
                .to_string(),
        };
        // Logged like any other exchange. It never reaches the agent, but it is
        // still something somebody said here, and the message itself is already
        // in reach.jsonl -- this is the record that they said it during *this*
        // visit.
        self.logged.push((line.clone(), said.clone()));
        self.turns.push(Turn {
            q: line,
            a: said,
            done: true,
            ..Default::default()
        });
        self.input.clear();
        self.scroll = 0;
    }

    /// Whether the command palette is open: the line is a command being typed
    /// and has no argument yet.
    pub fn picking(&self) -> bool {
        // Not while the up arrow is walking the history. Recalling `/keys` puts
        // a slash on the line without anybody typing one, and if that reopened
        // the palette the next press would move through commands instead of
        // carrying on backwards -- which is what it was pressed for.
        self.hist_at.is_none()
            && self.input.starts_with('/')
            && !self.input.contains(' ')
            && !self.choices().is_empty()
    }

    /// The palette's current contents.
    pub fn choices(&self) -> Vec<(&'static str, &'static str)> {
        if self.input.starts_with('/') {
            matches(&self.input)
        } else {
            Vec::new()
        }
    }

    /// The highlighted row, clamped to what is actually on offer.
    pub fn picked(&self) -> usize {
        self.pick.min(self.choices().len().saturating_sub(1))
    }

    /// Rows of scrollback available, published by the last render.
    pub fn set_reach(&self, rows: usize) {
        self.reach.set(rows);
    }

    pub fn scrolled(&self) -> usize {
        self.scroll
    }

    /// Move through the scrollback. Positive is backwards, into the past.
    pub fn scroll_by(&mut self, rows: i32) {
        let reach = self.reach.get();
        let want = self.scroll as i32 + rows;
        self.scroll = want.clamp(0, reach as i32) as usize;
    }

    /// Walk the questions already asked. `-1` is older, `+1` newer.
    fn walk_history(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        let at = match (self.hist_at, dir) {
            (None, -1) => self.history.len() - 1,
            (None, _) => return,
            (Some(0), -1) => 0,
            (Some(i), -1) => i - 1,
            (Some(i), _) if i + 1 < self.history.len() => i + 1,
            // Walked forward off the end: back to whatever was being typed,
            // which is an empty line because that is where this started.
            (Some(_), _) => {
                self.hist_at = None;
                self.input.clear();
                return;
            }
        };
        self.hist_at = Some(at);
        self.input = self.history[at].clone();
    }

    pub fn on_key(&mut self, k: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        // Three things want the arrows, so the order is fixed here rather than
        // fought over further down: the palette while it is open, then the
        // history, and the scrollback is on its own keys entirely.
        match k.code {
            KeyCode::Up if self.picking() => {
                self.pick = self.picked().saturating_sub(1);
                return;
            }
            KeyCode::Down if self.picking() => {
                self.pick = (self.picked() + 1).min(self.choices().len() - 1);
                return;
            }
            KeyCode::Tab if self.picking() => {
                self.complete();
                return;
            }
            KeyCode::Up => {
                self.walk_history(-1);
                return;
            }
            KeyCode::Down => {
                self.walk_history(1);
                return;
            }
            KeyCode::PageUp => {
                self.scroll_by(10);
                return;
            }
            KeyCode::PageDown => {
                self.scroll_by(-10);
                return;
            }
            _ => {}
        }

        match k.code {
            // Enter takes the highlighted command rather than the half-typed
            // one: having arrowed to `/projects`, pressing enter should not run
            // `/pj`.
            KeyCode::Enter if self.picking() => {
                self.complete();
                let whole = COMMANDS.iter().any(|(n, _)| *n == self.input.trim());
                if whole {
                    self.submit();
                }
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.input.pop();
                self.pick = 0;
            }
            // Escape does the most local thing available: close the palette,
            // else stop the answer, else clear the line. Cancelling is
            // cooperative -- the wait carries on until the agent answers.
            KeyCode::Esc => {
                if self.picking() {
                    self.input.clear();
                } else if self.state == State::Thinking {
                    if let Some(c) = &self.client {
                        c.cancel();
                    }
                } else if self.scroll > 0 {
                    self.scroll = 0;
                } else {
                    self.input.clear();
                }
            }
            KeyCode::Char('u') if ctrl => self.input.clear(),
            // Walking the route at the side. Ctrl rather than a bare key
            // because every bare key in this section is a letter somebody is
            // trying to type, and these have to work while a question is being
            // written.
            KeyCode::Char('n') if ctrl => self.walk(1),
            KeyCode::Char('b') if ctrl => self.walk(-1),
            // Guarded: without this, Ctrl-C types a `c` into the question
            // instead of quitting.
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                if words(&self.input) < MAX_WORDS {
                    self.input.push(c);
                    self.pick = 0;
                    self.hist_at = None;
                }
            }
            _ => {}
        }
    }

    /// Step through the places on the map, if there are several.
    ///
    /// The camera flies rather than cuts -- the shell reads the current stop
    /// each frame and moves to it, so this only has to say which one.
    pub fn walk(&mut self, by: i32) {
        if let Some(Panel { what: Show::Place(tour), .. }) = &mut self.panel {
            tour.step(by);
        }
    }

    /// Take the highlighted command onto the line.
    fn complete(&mut self) {
        let choices = self.choices();
        if let Some((name, _)) = choices.get(self.picked()) {
            self.input = name.to_string();
            self.pick = 0;
        }
    }

    /// A wheel notch, from the shell.
    pub fn on_scroll(&mut self, up: bool) {
        self.scroll_by(if up { 3 } else { -3 });
    }
}

/// Whether the page is wide enough to put a picture beside the words, and how
/// much of it that picture gets.
///
/// Shared by the renderer and by `map_panel`, so the shell cannot draw a map
/// into a column the prose has already been given.
fn panel_cols(area: Rect, a: &Ask) -> u16 {
    match &a.panel {
        // A code has to be drawn at its own size or not at all, so it waits for
        // a wide page. A place needs only somewhere to write its name: the map
        // itself is the page's ground now and takes whatever room there is, so
        // the column is for the caption and can be much narrower.
        Some(p @ Panel { what: Show::Place(_), .. }) if area.width >= 60 => {
            panel_width(p, area).min(area.width / 2)
        }
        Some(p) if area.width >= 96 => panel_width(p, area).min(area.width / 2),
        _ => 0,
    }
}

/// Where the panel sits on the page, if there is one.
fn panel_rect(area: Rect, a: &Ask) -> Option<Rect> {
    let w = panel_cols(area, a);
    if w == 0 {
        return None;
    }
    let picks = if a.picking() { a.choices().len().min(7) as u16 } else { 0 };
    let body = area.height.saturating_sub(2 + picks);
    Some(Rect {
        x: area.x + area.width - w - 1,
        y: area.y + 1,
        width: w,
        height: body.saturating_sub(1),
    })
}


/// The map a place panel wants drawn, and where.
///
/// This file cannot draw it. The renderer for it lives in the map crate and
/// wants an `App`, which is one terrain grid and one tile cache -- the shell has
/// exactly one and the chat should not own a second. So the chat says where the
/// picture goes and what should be in it, and the shell puts it there, the same
/// division `goto` already uses for navigation.
/// The place the page is showing, whatever the window is doing.
///
/// Separate from `map_panel` on purpose. Where the camera is pointed is a fact
/// about the conversation; whether there is room to draw it is a fact about the
/// window. Reading the target through the layout meant a window too narrow for a
/// panel silently reset the camera, and a resize mid-flight restarted it.
pub fn showing_place(a: &Ask) -> Option<&Spot> {
    match &a.panel {
        Some(Panel { what: Show::Place(tour), .. }) => Some(tour.here()),
        _ => None,
    }
}

/// The map, and the whole page to draw it on.
///
/// It used to be a panel in a column: a picture the page made room for, with a
/// third of the screen left empty under it. It is the page's ground now -- the
/// full body, behind everything, with the words written over the top. A
/// `Paragraph` writes only the cells its text covers, so the map shows through
/// around every line, and the shell knocks it back under the reading column so
/// the prose still reads. Overlap is the point rather than a thing to avoid.
pub fn map_panel(area: Rect, a: &Ask) -> Option<(Rect, Spot, f32)> {
    if area.width < 30 || area.height < 8 {
        return None;
    }
    let p = a.panel.as_ref()?;
    let Show::Place(tour) = &p.what else { return None };
    let spot = tour.here().clone();
    // The right of the page, and all of its height above the question line.
    //
    // Neither a widget in the corner nor the whole page. It was both in turn and
    // both were wrong: a panel in a column is a picture the page made room for,
    // and full bleed is wallpaper with words on it. This is a shape that lives
    // on the right and ends when it ends -- `paint::feather` gives it an
    // irregular edge, so its left side breaks up over the prose instead of
    // ruling a line down the middle of the screen.
    let w = (area.width * 3 / 5).max(40).min(area.width);
    let at = Rect {
        x: area.x + area.width - w,
        width: w,
        height: area.height.saturating_sub(2),
        ..area
    };
    (at.height >= 8 && at.width >= 30).then_some((at, spot, p.fade()))
}

/// Where the reading column sits, so the shell can dim the map under it.
///
/// Text over braille is unreadable at full strength, and the answer to that is
/// not to move the map out of the way -- it is to take the map down to a
/// suggestion exactly where the words are, and leave it alone everywhere else.
pub fn prose_rect(area: Rect, a: &Ask) -> Rect {
    let gutter = 3u16;
    let w = area.width.saturating_sub(gutter * 2 + panel_cols(area, a)).min(104);
    Rect { x: area.x + gutter.saturating_sub(1), width: w + 2, ..area }
}

/// Where the map's own rect sits, for the shell's pointer test. Separate from
/// `map_panel` only so a caller that wants the geometry need not want the spot.
pub fn map_rect(area: Rect, a: &Ask) -> Option<Rect> {
    map_panel(area, a).map(|(at, _, _)| at)
}

pub fn render(f: &mut Frame, area: Rect, a: &Ask) {
    if area.width < 30 || area.height < 8 {
        return;
    }

    // The page takes the width it is given. The panel, when there is one, takes
    // a slice off the right rather than sitting in the middle of the prose --
    // the words keep their own column and the picture arrives beside them.
    let panel_w = panel_cols(area, a);
    let gutter = 3u16;
    let w = area.width.saturating_sub(gutter * 2 + panel_w);
    let x = area.x + gutter;

    // Two rows at the bottom for the question line and its rule, plus whatever
    // the palette is showing above them.
    let picks = if a.picking() { a.choices() } else { Vec::new() };
    let pick_rows = picks.len().min(7) as u16;
    let foot = 2 + pick_rows;
    let body = Rect { y: area.y, height: area.height.saturating_sub(foot), ..area };

    // The page is as wide as the terminal; the prose inside it is not. A line
    // of a hundred and forty columns is a line the eye loses its place on, and
    // the width is better spent on the panel beside it than on the measure.
    let prose = w.min(104);

    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    if a.turns.is_empty() {
        opening(&mut lines, prose, a);
    }
    for (i, t) in a.turns.iter().enumerate() {
        let live = i + 1 == a.turns.len() && a.busy();
        transcript(&mut lines, prose, t, a, live, panel_w == 0);
    }

    // Short pages sit on the question line rather than floating at the top of a
    // screen of nothing: the eye should start where the typing happens.
    if lines.len() < body.height as usize {
        let pad = body.height as usize - lines.len();
        let mut settled = vec![Vec::new(); pad];
        settled.append(&mut lines);
        lines = settled;
    }

    // How far back this page can be scrolled, published so the key handler can
    // clamp without laying the whole thing out a second time.
    let over = lines.len().saturating_sub(body.height as usize);
    a.set_reach(over);
    // A conversation is anchored to the bottom, because the newest exchange is
    // the one being read. The opening is anchored to the top: it is taller than
    // a narrow screen once the gate rows wrap, and scrolling it from the bottom
    // would push the policy off and leave the suggestions.
    let top = if a.turns.is_empty() {
        0
    } else {
        over.saturating_sub(a.scrolled().min(over))
    };

    for (row, spans) in lines.into_iter().skip(top).take(body.height as usize).enumerate() {
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x, y: body.y + row as u16, width: w, height: 1 },
        );
    }

    // Anything scrolled away is worth saying so, or the page looks like it lost
    // the conversation.
    if a.scrolled() > 0 {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("\u{2191} {} more", a.scrolled()),
                Style::default().fg(FAINT),
            ))
            .right_aligned(),
            Rect { x, y: body.y, width: w, height: 1 },
        );
    }

    if let (Some(at), Some(p)) = (panel_rect(area, a), &a.panel) {
        side(f, at, p);
    }

    palette(f, Rect { x, y: area.y + area.height - foot, width: w, height: pick_rows }, &picks, a.picked());
    question(f, Rect { x, y: area.y + area.height - 2, width: w, height: 2 }, a);
}

/// The invitation, before anybody has asked anything.
fn opening(lines: &mut Vec<Vec<Span<'static>>>, w: u16, a: &Ask) {
    lines.push(vec![]);
    panel_gates(lines, w, a);
    for l in wrap(OPENING, w as usize) {
        lines.push(vec![Span::styled(l, Style::default().fg(DIM))]);
    }
    lines.push(vec![]);
    for s in SUGGESTIONS {
        lines.push(vec![Span::styled(format!("  {s}"), Style::default().fg(FAINT))]);
    }
}

/// One exchange, question then tools then answer then whatever it drew.
fn transcript(
    lines: &mut Vec<Vec<Span<'static>>>,
    w: u16,
    t: &Turn,
    a: &Ask,
    live: bool,
    // Whether a code belongs in the flow. False when the panel beside the prose
    // is already showing it, which is where it goes when there is room -- drawn
    // both ways, a wide screen showed the same code twice.
    inline_code: bool,
) {
    let lead = crate::paint::lead();
    for (i, l) in wrap(&t.q, w.saturating_sub(2) as usize).into_iter().enumerate() {
        lines.push(vec![
            Span::styled(if i == 0 { "\u{203a} " } else { "  " }, Style::default().fg(lead)),
            Span::styled(l, Style::default().fg(lead).add_modifier(Modifier::BOLD)),
        ]);
    }
    lines.push(vec![]);

    // Grouped by tool, not one flat row per call. An answer that walks a
    // visitor through eight places calls the same tool eight times, and eight
    // rows repeating the same name is a wall with the interesting part -- which
    // places -- hidden in the right-hand column. The name is said once and its
    // arguments listed under it.
    const NAME_COL: usize = 15;
    let mut at = 0;
    while at < t.calls.len() {
        let title = t.calls[at].title.as_str();
        let mut run = at;
        while run < t.calls.len() && t.calls[run].title == title {
            run += 1;
        }
        let group = &t.calls[at..run];
        at = run;

        // The worst status in the group leads it: one failure among eight
        // successes is the thing worth seeing.
        let status = group
            .iter()
            .map(|c| c.status)
            .max_by_key(|s| match s {
                Status::Running => 3,
                Status::Refused => 2,
                Status::Failed => 1,
                Status::Done => 0,
            })
            .unwrap_or(Status::Done);
        let (glyph, colour) = match status {
            Status::Running => (
                ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"][((a.t * 6.0) as usize) % 4],
                lead,
            ),
            Status::Done => ("\u{2713}", DIM),
            Status::Failed => ("\u{00d7}", ACCENT),
            Status::Refused => ("\u{2300}", ACCENT),
        };
        let label = if title.is_empty() { "tool" } else { title };
        // A count only when there is one, and only when the arguments do not
        // already say it: `locate_place x8` above eight named places is telling
        // the reader something they can see.
        let named: Vec<&Call> = group.iter().filter(|c| !c.detail.trim().is_empty()).collect();
        let head = if named.is_empty() && group.len() > 1 {
            format!("{label}  \u{00d7}{}", group.len())
        } else {
            label.to_string()
        };
        let room = (w as usize).saturating_sub(NAME_COL + 4);

        let first = named.first().map(|c| ellipsis(&c.detail, room)).unwrap_or_default();
        lines.push(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(colour)),
            Span::styled(format!("{head:<NAME_COL$}"), Style::default().fg(DIM)),
            Span::styled(first, Style::default().fg(FAINT)),
        ]);
        // The rest sit under the first, in the argument column, so the eye reads
        // a list of places rather than a list of calls.
        for c in named.iter().skip(1) {
            lines.push(vec![
                Span::raw(" ".repeat(NAME_COL + 2)),
                Span::styled(ellipsis(&c.detail, room), Style::default().fg(FAINT)),
            ]);
        }
    }
    if !t.calls.is_empty() {
        lines.push(vec![]);
    }

    let mut body: Vec<String> = Vec::new();
    for para in t.a.split('\n') {
        if para.trim().is_empty() {
            body.push(String::new());
            continue;
        }
        body.extend(wrap(para, w as usize));
    }
    if live {
        // Everything except the last line has settled. The last line is where
        // the answer is still arriving, so it is drawn with its tip dissolving
        // into noise rather than having a row of noise underneath it: the churn
        // and the words are the same line of text at different stages, which is
        // what makes it read as settling instead of as two things.
        let tip = body.pop().unwrap_or_default();
        for l in body {
            lines.push(vec![Span::styled(l, Style::default().fg(FG))]);
        }
        lines.push(settling(&tip, w, a.t));
    } else {
        for l in body {
            lines.push(vec![Span::styled(l, Style::default().fg(FG))]);
        }
    }

    if let Some(code) = t.code.filter(|_| inline_code) {
        let span = code.size + QUIET * 2;
        if (span as u16) <= w {
            lines.push(vec![]);
            for row in qr_lines(code, 1.0) {
                lines.push(row);
            }
        } else {
            lines.push(vec![]);
            lines.push(vec![Span::styled(
                "(the window is too narrow to draw the code)".to_string(),
                Style::default().fg(FAINT),
            )]);
        }
    }
    lines.push(vec![]);
}

/// Glyphs that have not become words yet.
///
/// The wait is the answer arriving rather than a thing beside it: a run of
/// characters churns at the head of what has been written, and each one stops
/// churning as the token behind it lands. Nothing else on the page moves, and
/// when the answer is done this row is simply not drawn -- so the motion is
/// bounded by the reply and not by a timer.
const NOISE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789<>/\\|=+*#$%&@";

/// Churn rate. Slower than it was: at eighteen a second the noise reads as
/// static, and static does not look like something resolving into a word.
const CHURN: f64 = 11.0;
/// How far back from the tip a character can still flicker. The gradient across
/// this is the whole effect -- with a hard edge the boundary between noise and
/// text is a wall that jumps a character at a time, which is what "snapping"
/// was.
const SETTLING: usize = 14;

/// One deterministic byte from a frame and a position.
///
/// Seeded from both, so a character flickers on its own schedule rather than the
/// whole run changing together, and a snapshot at a given clock is the same
/// picture on every terminal watching.
fn churn(frame: u64, at: usize) -> u64 {
    let mut x = frame
        .wrapping_mul(6364136223846793005)
        .wrapping_add(at as u64)
        .wrapping_add(1442695040888963407);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 29;
    x
}

/// The line an answer is still arriving on: settled words, then a stretch that
/// is still deciding, then glyphs that are not words yet.
fn settling(tip: &str, w: u16, t: f64) -> Vec<Span<'static>> {
    let frame = (t * CHURN) as u64;
    let chars: Vec<char> = tip.chars().collect();
    let run = 12usize.min(w.saturating_sub(4) as usize);

    // Room for the noise to run on past the words without wrapping the line.
    let keep = chars.len().min(w.saturating_sub(run as u16 + 1) as usize);
    let chars = &chars[chars.len() - keep..];

    let mut out = String::with_capacity(keep + run);
    for (i, c) in chars.iter().enumerate() {
        let back = keep - i; // 1 at the tip, larger further into settled text
        if back <= SETTLING && !c.is_whitespace() {
            // Likeliest at the tip and nothing at all by the far edge. Squared,
            // so most of the line is calm and only the last few characters are
            // still moving.
            let near = 1.0 - (back as f64 / SETTLING as f64);
            let odds = (near * near * 0.62 * 255.0) as u64;
            if churn(frame, i) % 255 < odds {
                out.push(NOISE[(churn(frame, i + 7919) >> 17) as usize % NOISE.len()] as char);
                continue;
            }
        }
        out.push(*c);
    }
    let settled_len = out.chars().count();
    for k in 0..run {
        out.push(NOISE[(churn(frame, 1000 + k) >> 13) as usize % NOISE.len()] as char);
    }

    // Three weights rather than one: the words, the part still deciding, and the
    // run ahead of them, which fades out instead of stopping.
    let cut = settled_len.saturating_sub(SETTLING.min(settled_len));
    let solid: String = out.chars().take(cut).collect();
    let deciding: String = out.chars().skip(cut).take(settled_len - cut).collect();
    let ahead: String = out.chars().skip(settled_len).collect();
    let lead = crate::paint::lead();
    let mut spans = Vec::new();
    if !solid.is_empty() {
        spans.push(Span::styled(solid, Style::default().fg(FG)));
    }
    if !deciding.is_empty() {
        spans.push(Span::styled(deciding, Style::default().fg(lead)));
    }
    if !ahead.is_empty() {
        let half = ahead.chars().count() / 2;
        let (near, far): (String, String) =
            (ahead.chars().take(half).collect(), ahead.chars().skip(half).collect());
        spans.push(Span::styled(near, Style::default().fg(DIM)));
        spans.push(Span::styled(far, Style::default().fg(FAINT)));
    }
    spans
}

/// How wide the panel wants to be.
fn panel_width(p: &Panel, area: Rect) -> u16 {
    match &p.what {
        Show::Code(c) => (c.size + QUIET * 2) as u16,
        // A share of the page rather than a fixed 46, which was wide enough for
        // a city to read as a city and no wider -- on a full-screen terminal
        // that left the map a stamp in the corner of a lot of empty room. Two
        // fifths, floored at what was there before and capped so the prose keeps
        // a readable measure.
        Show::Place(_) => (area.width * 2 / 5).clamp(46, 92),
        // The badge and its code are the same width on purpose -- see the
        // generator. Whichever is wider decides, so neither is ever clipped by
        // a column the other one chose.
        Show::Cert => (crate::cert::BADGE.w as u16).max((crate::cert::QR.size + QUIET * 2) as u16),
    }
}

/// Draw whatever is at the side, faded in.
fn side(f: &mut Frame, area: Rect, p: &Panel) {
    let fade = p.fade();
    match &p.what {
        Show::Code(code) => {
            let rows = qr_lines(code, fade);
            for (i, spans) in rows.into_iter().enumerate() {
                if i as u16 >= area.height {
                    break;
                }
                f.render_widget(
                    Paragraph::new(Line::from(spans)),
                    Rect { x: area.x, y: area.y + i as u16, width: area.width, height: 1 },
                );
            }
        }
        Show::Cert => badge(f, area, fade),
        // Only the caption. The picture above it belongs to the map renderer
        // and the shell draws it there -- see `map_panel`.
        Show::Place(tour) => {
            let spot = tour.here();
            // Anchored to the foot of its column rather than hung under a
            // picture, because the picture is the whole page now. Counted from
            // the bottom so the block sits still whether the note wraps to one
            // line or three.
            let name = wrap(&spot.name, area.width as usize).len() as u16;
            let note = wrap(&spot.note, area.width as usize).len() as u16;
            let footer = if tour.stops.len() > 1 || spot.id.is_some() { 2 } else { 0 };
            let bottom = area.y + area.height;
            let mut y = bottom.saturating_sub(name + note + footer);
            let row = |f: &mut Frame, y: &mut u16, spans: Vec<Span<'static>>| {
                if *y < bottom {
                    f.render_widget(
                        Paragraph::new(Line::from(spans)),
                        Rect { x: area.x, y: *y, width: area.width, height: 1 },
                    );
                    *y += 1;
                }
            };
            let lead = crate::paint::lead();
            for l in wrap(&spot.name, area.width as usize) {
                row(
                    f,
                    &mut y,
                    vec![Span::styled(
                        l,
                        Style::default()
                            .fg(crate::paint::dim_to(lead, fade))
                            .add_modifier(Modifier::BOLD),
                    )],
                );
            }
            for l in wrap(&spot.note, area.width as usize) {
                row(
                    f,
                    &mut y,
                    vec![Span::styled(l, Style::default().fg(crate::paint::dim_to(DIM, fade)))],
                );
            }
            // Where you are in the route, and how to walk it. Only when there
            // is a route: one place needs no directions, and a footer offering
            // to step through a list of one is furniture.
            if tour.stops.len() > 1 {
                row(f, &mut y, vec![]);
                row(
                    f,
                    &mut y,
                    vec![
                        Span::styled(
                            format!("{}/{}", tour.at + 1, tour.stops.len()),
                            Style::default().fg(crate::paint::dim_to(lead, fade)),
                        ),
                        Span::styled(
                            "   ^n".to_string(),
                            Style::default().fg(crate::paint::dim_to(CYAN, fade)),
                        ),
                        Span::styled(
                            " next".to_string(),
                            Style::default().fg(crate::paint::dim_to(FAINT, fade)),
                        ),
                        Span::styled(
                            "   ^b".to_string(),
                            Style::default().fg(crate::paint::dim_to(CYAN, fade)),
                        ),
                        Span::styled(
                            " back".to_string(),
                            Style::default().fg(crate::paint::dim_to(FAINT, fade)),
                        ),
                    ],
                );
            } else if spot.id.is_some() {
                // A stop on the experience sheet: the full map can fly there.
                row(f, &mut y, vec![]);
                row(
                    f,
                    &mut y,
                    vec![
                        Span::styled(
                            "/map".to_string(),
                            Style::default().fg(crate::paint::dim_to(CYAN, fade)),
                        ),
                        Span::styled(
                            "  fly there".to_string(),
                            Style::default().fg(crate::paint::dim_to(FAINT, fade)),
                        ),
                    ],
                );
            }
        }
    }
}

/// The command palette, in the space above the question line.
fn palette(f: &mut Frame, area: Rect, picks: &[(&'static str, &'static str)], on: usize) {
    if area.height == 0 {
        return;
    }
    let lead = crate::paint::lead();
    for (i, (name, help)) in picks.iter().take(area.height as usize).enumerate() {
        let here = i == on;
        let spans = vec![
            Span::styled(
                if here { "\u{25b8} " } else { "  " },
                Style::default().fg(lead),
            ),
            Span::styled(
                format!("{name:<11}"),
                if here {
                    Style::default().fg(lead).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(FG)
                },
            ),
            Span::styled(help.to_string(), Style::default().fg(FAINT)),
        ];
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x: area.x, y: area.y + i as u16, width: area.width, height: 1 },
        );
    }
}

/// The rule and the line being typed.
fn question(f: &mut Frame, area: Rect, a: &Ask) {
    let lead = crate::paint::lead();
    f.render_widget(
        Paragraph::new(Span::styled(
            "\u{2500}".repeat(area.width as usize),
            Style::default().fg(FAINT),
        )),
        Rect { height: 1, ..area },
    );

    let waking = match crate::health::note() {
        Some(_) => "waking the agent\u{2026}".to_string(),
        None => "waking the agent\u{2026}".to_string(),
    };
    let (mark, hint, style) = match &a.state {
        State::Cold | State::Starting => ("\u{b7}", waking.as_str(), Style::default().fg(FAINT)),
        State::Ready => ("\u{203a}", "", Style::default().fg(lead)),
        State::Thinking => ("\u{b7}", "", Style::default().fg(FAINT)),
        State::Failed(m) => ("\u{d7}", m.as_str(), Style::default().fg(ACCENT)),
    };
    let mut spans = vec![Span::styled(format!("{mark} "), style)];
    if a.input.is_empty() && !hint.is_empty() {
        spans.push(Span::styled(hint.to_string(), Style::default().fg(FAINT)));
    } else if a.input.is_empty() && a.state == State::Ready {
        spans.push(Span::styled(
            "ask, or / for commands".to_string(),
            Style::default().fg(FAINT),
        ));
    } else {
        spans.push(Span::styled(a.input.clone(), Style::default().fg(FG)));
        if !a.busy() {
            spans.push(Span::styled("\u{258c}", Style::default().fg(lead)));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { y: area.y + 1, height: 1, ..area },
    );
}

/// Blend a colour up out of the page's background.
///
/// `t` of 0 is the background exactly, so a panel starts invisible and arrives;
/// 1 is the colour untouched, which is what a QR code has to end at.
fn mix(rgb: (u8, u8, u8), t: f32) -> ratatui::style::Color {
    const BG: (u8, u8, u8) = (8, 9, 11);
    let t = t.clamp(0.0, 1.0);
    let f = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    ratatui::style::Color::Rgb(f(BG.0, rgb.0), f(BG.1, rgb.1), f(BG.2, rgb.2))
}

/// The badge, and the code that verifies it under it when the page is tall
/// enough to hold both.
///
/// Two kinds of drawing in one picture, which is the whole design: the plate is
/// a shape, so it is a grid of half blocks baked by `scripts/cert.py`; the three
/// lines and the mark have detail in them, so they are characters and the
/// terminal draws them in its own font at its own size. Dithering a letter a
/// font already knows how to draw is how the words came out unreadable when
/// this was tried as a picture of the real PNG.
fn badge(f: &mut Frame, area: Rect, fade: f32) {
    let b = &crate::cert::BADGE;
    let plate = mix(crate::cert::PLATE, fade);
    let ink = mix(crate::cert::INK, fade);
    let page = mix((8, 9, 11), fade);
    let hue = |c: u8| match c {
        b'p' => plate,
        b'w' => ink,
        _ => page,
    };

    let left = area.x + (area.width.saturating_sub(b.w as u16)) / 2;
    let mut row = 0u16;
    for y in 0..b.h {
        if row >= area.height {
            return;
        }
        let (up, down) = (b.pixels[y * 2].as_bytes(), b.pixels[y * 2 + 1].as_bytes());
        // One cell is two pixels stacked: the upper is the foreground of an
        // upper half block and the lower is its background. A cell with nothing
        // in it is a space rather than a block on the page's own colour --
        // fewer bytes, and identical on screen.
        let mut cells: Vec<(char, ratatui::style::Color, ratatui::style::Color)> = (0..b.w)
            .map(|x| match (up[x], down[x]) {
                (b'.', b'.') => (' ', page, page),
                (a, c) => ('\u{2580}', hue(a), hue(c)),
            })
            .collect();
        for (wr, wc, text) in b.words {
            if *wr != y {
                continue;
            }
            for (i, ch) in text.chars().enumerate() {
                if let Some(cell) = cells.get_mut(wc + i) {
                    // On the plate, not on the page: `fit` in the generator
                    // refuses to place a line that would hang off the shape, so
                    // the ground behind every one of these is the plate.
                    *cell = (ch, ink, plate);
                }
            }
        }

        // Runs rather than a span per cell. The plate is forty cells of one
        // colour and this takes it to a handful of spans a row.
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut style = None;
        for (ch, fg, bg) in cells {
            let want = Style::default().fg(fg).bg(bg);
            if style != Some(want) && !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), style.unwrap_or_default()));
            }
            style = Some(want);
            run.push(ch);
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, style.unwrap_or_default()));
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x: left, y: area.y + row, width: area.width, height: 1 },
        );
        row += 1;
    }

    // The code, if the window can hold it. When it cannot, the link is already
    // in the answer beside this as something to type -- a code clipped in half
    // is worse than no code, because it looks scannable.
    let qr = qr_lines(&crate::cert::QR, fade);
    if area.height < row + 1 + qr.len() as u16 {
        return;
    }
    row += 1;
    let span = (crate::cert::QR.size + QUIET * 2) as u16;
    let at = area.x + (area.width.saturating_sub(span)) / 2;
    for line in qr {
        f.render_widget(
            Paragraph::new(Line::from(line)),
            Rect { x: at, y: area.y + row, width: span.min(area.width), height: 1 },
        );
        row += 1;
    }
}

/// Modules of quiet zone around a code. Four is the spec's minimum, and the
/// thing it buys is a scanner finding the code at all.
const QUIET: usize = 4;

/// A QR code as terminal rows.
///
/// Half blocks, so a module is square: one text cell is two module rows, the
/// upper drawn as the foreground of `▀` and the lower as its background. A code
/// drawn one module per cell is twice as tall as it is wide and scanners refuse
/// it.
///
/// Black on white regardless of the palette, and the quiet zone is white too.
/// Everything else on this page is light text on a dark ground; a code has to be
/// the other way round or the contrast is inverted and phones will not read it.
/// Runs of the same pair are merged into one span rather than emitted per cell,
/// which takes a 45-column code from 45 spans a row to about a dozen.
fn qr_lines(code: &crate::coffee::Code, fade: f32) -> Vec<Vec<Span<'static>>> {
    // Mixed toward the page's own background by the fade, so the code rises out
    // of the dark rather than being switched on. At rest it is pure black on
    // pure white, which is what a scanner wants.
    let dark = mix((0, 0, 0), fade);
    let light = mix((255, 255, 255), fade);

    let span = code.size + QUIET * 2;
    // `true` is a dark module. Outside the matrix is quiet zone, which is light.
    let at = |x: usize, y: usize| -> bool {
        let (Some(mx), Some(my)) = (x.checked_sub(QUIET), y.checked_sub(QUIET)) else {
            return false;
        };
        if mx >= code.size || my >= code.size {
            return false;
        }
        code.rows[my].as_bytes()[mx] == b'#'
    };

    let mut out = Vec::new();
    for row in (0..span).step_by(2) {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut pair: Option<(bool, bool)> = None;
        for x in 0..span {
            // The last row of an odd-height code has no lower half; light, so
            // it reads as quiet zone rather than as a row of modules.
            let cell = (at(x, row), row + 1 < span && at(x, row + 1));
            if pair != Some(cell) && !run.is_empty() {
                let (up, down) = pair.expect("run is non-empty");
                spans.push(Span::styled(
                    std::mem::take(&mut run),
                    Style::default()
                        .fg(if up { dark } else { light })
                        .bg(if down { dark } else { light }),
                ));
            }
            pair = Some(cell);
            run.push('\u{2580}');
        }
        if let Some((up, down)) = pair {
            spans.push(Span::styled(
                run,
                Style::default()
                    .fg(if up { dark } else { light })
                    .bg(if down { dark } else { light }),
            ));
        }
        out.push(spans);
    }
    out
}

/// Filled, for a gate that is open. The shut ones carry no marker: their rows
/// are labelled `refused` and `off`, and a hollow dot on each would spend eight
/// columns of a sixty-two column measure repeating the label.
const OPEN: &str = "\u{25cf}";
/// What we are connected to, in one line. Right-aligned, faint, skippable.
///
/// Names the server rather than assuming one: the whole point of `servers.rs` is
/// that this could be anything, and a line that said "opencode" whatever was
/// running would be a lie the moment somebody used the feature.
/// Whether the agent is up, and nothing about what it is.
///
/// Which tier, which server, which model and which protocol version were all on
/// screen here and are all gone. None of it is the visitor's business: they came
/// to ask something, and a line reading `github copilot · opencode · plan · v1`
/// tells them only which company's quota they are spending. The machinery is in
/// `--probe` for whoever runs this, and nowhere a visitor can see.
fn wired(a: &Ask) -> String {
    match (&a.link, &a.state) {
        (Some(_), _) => "ready".into(),
        (None, State::Failed(_)) => "unavailable".into(),
        (None, _) => "waking".into(),
    }
}
/// What the agent may and may not do, stated before it is asked anything.
///
/// This is the part worth putting on a portfolio. A chat box that says "it
/// cannot run anything" is a claim; a list of every tool with the shut ones
/// still on it, drawn from the same table that does the refusing, is the claim
/// and its evidence in one place. The dots are the gates in `gates.rs` -- if
/// somebody opens one, this fills in without being edited.
fn panel_gates(lines: &mut Vec<Vec<Span<'static>>>, w: u16, a: &Ask) {
    let rule = |lines: &mut Vec<Vec<Span<'static>>>| {
        lines.push(vec![Span::styled(
            "\u{2500}".repeat(w as usize),
            Style::default().fg(FAINT),
        )]);
    };

    // Clear of the rail above. Without this the heading sits directly under the
    // navigation and reads as part of it.
    lines.push(vec![]);

    // The heading, with what we are wired to on the right of the same row.
    let title = "what it may do";
    let right = wired(a);
    let gap = (w as usize).saturating_sub(title.chars().count() + right.chars().count());
    lines.push(vec![
        Span::styled(title.to_string(), Style::default().fg(DIM)),
        Span::styled(" ".repeat(gap), Style::default()),
        Span::styled(right, Style::default().fg(FAINT)),
    ]);
    rule(lines);

    // The granted tools, one to a row with what they are for. These are the
    // only rows that get the accent: they are the whole grant.
    for t in crate::gates::TOOLS.iter().filter(|t| t.open) {
        lines.push(vec![
            Span::styled(format!("  {OPEN} "), Style::default().fg(CYAN)),
            Span::styled(
                format!("{:<12}", t.name),
                Style::default().fg(FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(t.blurb.to_string(), Style::default().fg(FAINT)),
        ]);
    }

    // Everything refused, compactly. One row rather than one each: the reader
    // does not need a paragraph per thing that cannot happen, but the names
    // have to be *there*, because a list of what is allowed proves nothing on
    // its own.
    let shut: Vec<&str> =
        crate::gates::TOOLS.iter().filter(|t| !t.open).map(|t| t.name).collect();
    let off: Vec<&str> = crate::gates::capabilities()
        .into_iter()
        .filter(|(_, open)| !open)
        .map(|(n, _)| n)
        .collect();
    // No marker on these rows: the label already says they are shut, and four
    // of them would spend eight columns saying it again. Wrapped rather than
    // clipped -- a list of refusals that loses its last entry to the measure is
    // the one kind of truncation this panel cannot afford.
    // The label sits in a gutter beside the list where there is room for one.
    // On a narrow screen it takes its own row instead -- twelve columns of
    // gutter out of twenty-two is how "elicitation" ends up as "elicitatio".
    const LABEL: usize = 12;
    let gutter = if (w as usize) >= 40 { LABEL } else { 0 };
    for (label, list) in [("refused", shut), ("off", off)] {
        if list.is_empty() {
            continue;
        }
        if gutter == 0 {
            lines.push(vec![Span::styled(
                format!("  {label}"),
                Style::default().fg(FAINT),
            )]);
        }
        let room = (w as usize).saturating_sub(gutter.max(4)).max(8);
        for (i, run) in wrap(&list.join(" \u{b7} "), room).into_iter().enumerate() {
            let head = match (gutter, i) {
                (0, _) => "    ".to_string(),
                (_, 0) => format!("  {:<width$}", label, width = LABEL - 2),
                _ => " ".repeat(LABEL),
            };
            lines.push(vec![
                Span::styled(head, Style::default().fg(FAINT)),
                Span::styled(run, Style::default().fg(DIM)),
            ]);
        }
    }
    rule(lines);
    lines.push(vec![]);
}

/// The panel above says what the agent may do, so this no longer lists it. It
/// used to promise that the agent could leave Prince a message, which the panel
/// would now contradict -- `reach_out` is shut and `/reach` is handled here.
const OPENING: &str = "Ask about the work, the places, or anything else. There \
    is an agent on this box, and you will see it reach for the web as it goes. \
    To leave Prince a message type /reach, which is handled here rather than by \
    the agent -- so it arrives whether or not a model is up, and word for word.";

/// Trim to `room` columns, marking that something was cut.
///
/// Counts characters rather than bytes: a URL with an accent in it is not a
/// reason to panic on a byte index that lands mid-codepoint.
fn ellipsis(s: &str, room: usize) -> String {
    if room == 0 {
        return String::new();
    }
    if s.chars().count() <= room {
        return s.to_string();
    }
    let keep = room.saturating_sub(1);
    s.chars().take(keep).collect::<String>() + "\u{2026}"
}

const SUGGESTIONS: [&str; 6] = [
    "what is the hardest part of netjail?",
    "why braille for the map?",
    // Deliberately one of these: a location question draws the place at the
    // side, and nothing else on the page says that pictures happen here.
    "where does he work now?",
    "what would he be like to work with?",
    "what should I read from all this?",
    "/reach  ...to leave him a message instead",
];
#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, status: Status) -> Call {
        Call {
            id: id.into(),
            title: "Fetch".into(),
            status,
            detail: "https://example.com".into(),
        }
    }

    /// Draw a frame and read it back as text.
    ///
    /// The panel is the part of this change worth checking, and the two states
    /// it has cannot both be reached from `--snapshot`: the collapsed one needs
    /// a conversation, and there is no flag that invents one.
    pub(super) fn drawn(a: &Ask, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, f.area(), a)).unwrap();
        termap::snapshot::plain(term.backend().buffer())
    }

    /// A session with no tools, which is what most of these tests want: they
    /// check the page, and the page keeps its keyword guess when the agent
    /// cannot decide for itself.
    fn ready(a: &mut Ask) {
        a.apply(Event::Ready(acp::Ready {
            tier: "github copilot".into(),
            server: "opencode".into(),
            version: 1,
            mode: "plan".into(),
            tools: false,
        }));
    }

    /// The guess stands down as soon as the agent has the tools -- before any
    /// question is asked, not after the first tool call.
    #[test]
    fn an_agent_with_tools_silences_the_keyword_guess() {
        let mut a = Ask::new();
        a.apply(Event::Ready(acp::Ready {
            tier: "t".into(),
            server: "s".into(),
            version: 1,
            mode: "plan".into(),
            tools: true,
        }));
        assert!(a.agent_drives, "the page is still guessing");
        a.input = "where does he work?".into();
        a.submit();
        assert!(
            a.panel.is_none(),
            "a map appeared on enter, before the agent had said anything"
        );

        // ...and without tools it still guesses, so a tier that cannot reach
        // the server does not simply lose the map.
        let mut a = Ask::new();
        ready(&mut a);
        assert!(!a.agent_drives);
        a.input = "where does he work?".into();
        a.submit();
        assert!(a.panel.is_some(), "the fallback stopped working");
    }

    /// Every gate is on screen before the agent is asked anything, open ones and
    /// shut ones alike. A list of what is permitted proves nothing on its own,
    /// so the refusals have to be visible too -- and they come from the same
    /// table that does the refusing.
    #[test]
    fn the_panel_names_every_gate_open_or_shut() {
        let mut a = Ask::new();
        ready(&mut a);
        let s = drawn(&a, 92, 30);
        for t in crate::gates::TOOLS {
            assert!(s.contains(t.name), "{} is not on screen:\n{s}", t.name);
        }
        for (cap, _) in crate::gates::capabilities() {
            // `cancel` is the one gate that grants nothing, so it is not listed
            // among the refusals; the rest must be.
            if cap != "cancel" {
                assert!(s.contains(cap), "{cap} is not on screen:\n{s}");
            }
        }
    }

    /// What is answering is nobody's business but the operator's. A visitor came
    /// to ask something; the tier, the server, the model and the protocol
    /// version tell them only whose quota they are spending.
    #[test]
    fn nothing_on_screen_names_the_backend() {
        let mut a = Ask::new();
        ready(&mut a);
        let empty = drawn(&a, 92, 30);
        a.turns.push(Turn { q: "hi".into(), a: "Hello.".into(), ..Default::default() });
        let full = drawn(&a, 92, 30);

        for screen in [empty, full] {
            for leak in ["opencode", "copilot", "github", "ollama", "plan mode", "acp v1", "v1 "] {
                assert!(
                    !screen.to_lowercase().contains(leak),
                    "`{leak}` reached the screen:\n{screen}"
                );
            }
        }
    }

    /// Once there is an answer to read the panel collapses to one line, so the
    /// reading space goes back to the prose.
    #[test]
    fn the_panel_gives_its_room_back_once_there_is_an_answer() {
        let mut a = Ask::new();
        ready(&mut a);
        let empty = drawn(&a, 92, 30);
        assert!(empty.contains("what it may do"));

        a.turns.push(Turn { q: "why braille?".into(), a: "Because dots.".into(), ..Default::default() });
        let full = drawn(&a, 92, 30);
        assert!(!full.contains("what it may do"), "the panel stayed:\n{full}");
        assert!(full.contains("Because dots."), "the answer is missing:\n{full}");
    }

    /// The codes are generated, so what is worth testing here is that what was
    /// baked is shaped like a QR code at all -- a truncated or transposed matrix
    /// would still compile and would still draw something square.
    #[test]
    fn every_baked_code_has_the_three_finder_patterns() {
        for (name, code) in crate::coffee::ALL {
            assert_eq!(code.rows.len(), code.size, "{name}: wrong number of rows");
            for r in code.rows {
                assert_eq!(r.len(), code.size, "{name}: a row is not {} wide", code.size);
                assert!(r.bytes().all(|b| b == b'#' || b == b'.'), "{name}: stray byte");
            }
            // A finder is a 7x7 ring: dark border, light inside it, dark core.
            // Present at three corners of every QR code ever made.
            let dark = |x: usize, y: usize| code.rows[y].as_bytes()[x] == b'#';
            let n = code.size;
            for (ox, oy) in [(0, 0), (n - 7, 0), (0, n - 7)] {
                for i in 0..7 {
                    assert!(dark(ox + i, oy), "{name}: finder at {ox},{oy} has no top edge");
                    assert!(dark(ox, oy + i), "{name}: finder at {ox},{oy} has no left edge");
                }
                assert!(!dark(ox + 1, oy + 1), "{name}: finder ring is filled in");
                assert!(dark(ox + 3, oy + 3), "{name}: finder has no core");
            }
        }
    }

    /// Drawn with half blocks so a module is square. One module per cell would
    /// be twice as tall as wide, and scanners refuse that.
    #[test]
    fn a_code_is_drawn_square_with_a_quiet_zone() {
        let code = &crate::coffee::UPI;
        let rows = qr_lines(code, 1.0);
        let span = code.size + QUIET * 2;
        assert_eq!(rows.len(), span.div_ceil(2), "wrong number of text rows");
        let width: usize = rows[0].iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(width, span, "a row is not the full span wide");

        // The outermost rows and columns are quiet zone: light, both halves.
        let first = &rows[0];
        assert!(
            first.iter().all(|s| s.style.fg == Some(ratatui::style::Color::Rgb(255, 255, 255))),
            "the top of the quiet zone is not light"
        );
    }

    /// `/coffee` never reaches the agent. A payment string has to arrive exactly
    /// as written, and a model in the middle of one can get a digit wrong.
    #[test]
    fn coffee_is_answered_here_and_carries_the_payload_verbatim() {
        let mut a = Ask::new();
        a.input = "/coffee".into();
        a.submit();
        assert_eq!(a.turns.len(), 1);
        let t = &a.turns[0];
        assert!(t.done);
        assert!(t.code.is_some(), "no code on the turn");
        assert!(
            t.a.contains(crate::coffee::UPI.payload),
            "the payload was not offered as text: {}",
            t.a
        );

        // And the other one is reachable.
        let mut a = Ask::new();
        a.input = "/coffee card".into();
        a.submit();
        assert!(a.turns[0].a.contains("buymeacoffee.com/snufkin24"), "{}", a.turns[0].a);
    }

    /// Half a QR code is not a smaller QR code.
    #[test]
    fn a_window_too_narrow_for_a_code_says_so_rather_than_drawing_part_of_one() {
        let mut a = Ask::new();
        a.input = "/coffee".into();
        a.submit();
        let s = drawn(&a, 40, 40);
        assert!(s.contains("too narrow"), "expected the note, got:\n{s}");
    }

    /// `/pj` should find `/projects`. A prefix beats a scattered match, and
    /// something that is not a subsequence at all does not appear.
    #[test]
    fn the_palette_matches_loosely_and_ranks_tightly() {
        let names = |typed: &str| -> Vec<&str> {
            matches(typed).into_iter().map(|(n, _)| n).collect()
        };
        assert_eq!(names("/pj").first(), Some(&"/projects"));
        assert_eq!(names("/cf").first(), Some(&"/coffee"));
        assert_eq!(names("/he").first(), Some(&"/help"));
        // A prefix ranks above a match that had to skip letters.
        let cl = names("/cl");
        assert_eq!(cl.first(), Some(&"/clear"));
        // Nonsense matches nothing, which is what closes the palette.
        assert!(names("/zzzz").is_empty());
        // A bare slash offers everything.
        assert_eq!(matches("/").len(), COMMANDS.len());
    }

    #[test]
    fn the_palette_opens_on_a_slash_and_closes_on_an_argument() {
        let mut a = Ask::new();
        a.input = "/co".into();
        assert!(a.picking(), "the palette should be open");
        // Once there is an argument the palette is not what is being typed.
        a.input = "/coffee card".into();
        assert!(!a.picking());
        // And a question is never a palette.
        a.input = "why braille?".into();
        assert!(!a.picking());
    }

    #[test]
    fn arrows_walk_the_palette_and_tab_takes_one() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut a = Ask::new();
        for c in "/c".chars() {
            a.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let first = a.choices()[0].0;
        a.on_key(KeyEvent::from(KeyCode::Down));
        let second = a.choices()[a.picked()].0;
        assert_ne!(first, second, "the arrow did not move the selection");
        a.on_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(a.input, second, "tab did not take the highlighted command");
    }

    /// The up arrow is history, except while the palette has it.
    #[test]
    fn up_walks_the_history_it_has() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut a = Ask::new();
        // `/help` is answered here, so it lands in the history without an agent.
        for q in ["/help", "/keys"] {
            a.input = q.into();
            a.submit();
        }
        assert!(a.input.is_empty());
        a.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(a.input, "/keys", "up did not bring back the last question");
        a.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(a.input, "/help");
        a.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(a.input, "/keys");
        // Forward past the end returns to an empty line.
        a.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(a.input, "");
    }

    /// Scrolling is clamped to what there is, and submitting returns to the
    /// bottom -- typing a question and watching nothing happen because you were
    /// scrolled up is the failure this prevents.
    #[test]
    fn the_transcript_scrolls_within_its_reach() {
        let mut a = Ask::new();
        for i in 0..40 {
            a.turns.push(Turn {
                q: format!("question {i}"),
                a: "an answer".into(),
                done: true,
                ..Default::default()
            });
        }
        let _ = drawn(&a, 92, 20);
        assert!(a.reach.get() > 0, "nothing to scroll but the page is long");

        a.scroll_by(5);
        assert_eq!(a.scrolled(), 5);
        a.scroll_by(-100);
        assert_eq!(a.scrolled(), 0, "scrolled past the bottom");
        a.scroll_by(10_000);
        assert_eq!(a.scrolled(), a.reach.get(), "scrolled past the top");

        a.input = "/help".into();
        a.submit();
        assert_eq!(a.scrolled(), 0, "asking did not return to the newest exchange");
    }

    /// The commands answered here need no agent, and every one of them says
    /// something back.
    #[test]
    fn the_local_commands_all_answer() {
        for cmd in ["/help", "/keys", "/whoami", "/uptime", "/theme"] {
            let mut a = Ask::new();
            a.input = cmd.into();
            a.submit();
            assert_eq!(a.turns.len(), 1, "{cmd} did not answer");
            assert!(!a.turns[0].a.trim().is_empty(), "{cmd} answered with nothing");
            assert!(a.turns[0].done, "{cmd} left the turn open");
        }
    }

    /// Navigation commands are the shell's to carry out; the chat only asks.
    #[test]
    fn a_navigation_command_asks_the_shell_to_move() {
        for (cmd, section) in NAV {
            let mut a = Ask::new();
            a.input = (*cmd).into();
            a.submit();
            assert_eq!(a.goto.as_deref(), Some(*section), "{cmd} did not ask to move");
            assert!(a.turns.is_empty(), "{cmd} left a turn behind");
        }
    }

    /// The bug this pins: `/map` went nowhere.
    ///
    /// The chat asked for the section by trimming the slash off the command, and
    /// the shell looks a section up by its label -- so `/projects`, `/skills`,
    /// `/taste` and `/home` all worked and `/map` silently did not, because the
    /// section is called `experience`. Every target has to be a real one.
    #[test]
    fn every_navigation_command_names_a_real_section() {
        for (cmd, section) in NAV {
            assert!(
                crate::shell::Section::ALL.iter().any(|s| s.label() == *section),
                "{cmd} points at `{section}`, which is not a section"
            );
        }
    }

    /// The other half of the same bug, and the worse half.
    ///
    /// A command handled here with nothing to say used to be indistinguishable
    /// from a line this file had never heard of, so both fell through to the
    /// agent: `/map` was sent to a language model as a question, and `/clear`
    /// emptied the page and then asked one what `/clear` meant.
    #[test]
    fn no_command_on_the_list_ever_reaches_the_agent() {
        for (cmd, _) in COMMANDS {
            let mut a = Ask::new();
            assert!(
                !matches!(a.local(cmd), Local::Not),
                "{cmd} is offered by the palette and then handed to the model"
            );
        }
    }

    /// The badge is baked, so what is checked here is the baked data. The
    /// generator refuses to place a line that hangs off the plate; this is the
    /// same rule enforced against `cert.rs` as it actually ships, which is what
    /// somebody hand-editing a generated file would get past.
    #[test]
    fn every_word_on_the_badge_sits_on_the_plate() {
        let b = &crate::cert::BADGE;
        assert_eq!(b.pixels.len(), b.h * 2, "the grid is not two rows to a cell");
        for (i, row) in b.pixels.iter().enumerate() {
            assert_eq!(row.len(), b.w, "row {i} is not {} wide", b.w);
            assert!(
                row.bytes().all(|c| c == b'.' || c == b'p'),
                "row {i} has something in it that is neither page nor plate"
            );
        }
        for (row, col, text) in b.words {
            assert!(*row < b.h, "`{text}` is on row {row} of {}", b.h);
            for (i, _) in text.chars().enumerate() {
                let x = col + i;
                assert!(x < b.w, "`{text}` runs off the grid at column {x}");
                // Both halves of the cell, because the renderer paints the
                // plate behind a word and a word half off the shape would sit
                // on a rectangle of plate that is not there.
                for y in [row * 2, row * 2 + 1] {
                    assert_eq!(
                        b.pixels[y].as_bytes()[x],
                        b'p',
                        "`{text}` hangs off the plate at cell {row},{x}"
                    );
                }
            }
        }
    }

    /// The link in the code and the link in the prose have to be the same link.
    #[test]
    fn the_badge_code_points_at_the_badge() {
        let payload = crate::cert::QR.payload;
        assert!(payload.starts_with("https://"), "not a URL: {payload}");
        assert!(payload.contains("credly.com/badges/"), "not a credly badge: {payload}");
        // `SHOWN` is what a visitor is invited to type. If it is not a prefix
        // of the real thing, one of the two is wrong and the scannable one is
        // the one that was verified.
        let bare = payload.trim_start_matches("https://").trim_start_matches("www.");
        assert!(bare.starts_with(crate::cert::SHOWN), "{} is not the start of {bare}", crate::cert::SHOWN);
    }

    /// `/cert` answers, draws the badge, and never troubles the agent.
    #[test]
    fn the_cert_command_puts_the_badge_up() {
        let mut a = Ask::new();
        a.input = "/cert".into();
        a.submit();
        assert!(matches!(&a.panel, Some(Panel { what: Show::Cert, .. })), "no badge");
        assert_eq!(a.turns.len(), 1);
        assert!(a.turns[0].a.contains(crate::cert::SHOWN), "the answer has no link in it");
        assert!(a.turns[0].done);

        let drawn = drawn(&a, 130, 50);
        assert!(drawn.contains("Claude Certified"), "the plate lost its type:\n{drawn}");
        assert!(drawn.contains("F O U N D A T I O N S"), "the tier is missing:\n{drawn}");
    }

    /// Asking about it raises it too, and asking about anything else does not.
    #[test]
    fn the_badge_answers_a_question_about_it() {
        for q in [
            "is he certified in anything?",
            "what is his claude certification",
            "does he have a credly badge",
        ] {
            let mut a = Ask::new();
            a.input = q.into();
            a.submit();
            assert!(
                matches!(&a.panel, Some(Panel { what: Show::Cert, .. })),
                "`{q}` did not raise the badge"
            );
        }
        for q in [
            "what does he do?",
            "is this map certified accurate?",
            "where does he work?",
        ] {
            let mut a = Ask::new();
            a.input = q.into();
            a.submit();
            assert!(
                !matches!(&a.panel, Some(Panel { what: Show::Cert, .. })),
                "`{q}` raised the badge for no reason"
            );
        }
    }

    /// A narrow window drops the code rather than clipping it. A QR cut in half
    /// still looks scannable, which is the failure worth avoiding.
    #[test]
    fn the_badge_survives_a_window_too_short_for_the_code() {
        let mut a = Ask::new();
        a.input = "/cert".into();
        a.submit();
        for (w, h) in [(96u16, 10u16), (100, 24), (120, 30), (160, 60)] {
            let out = drawn(&a, w, h);
            assert!(!out.is_empty(), "nothing drawn at {w}x{h}");
        }
        // Tall enough for both, and the code is a code rather than a fragment.
        let tall = drawn(&a, 130, 60);
        let wide_enough = crate::cert::QR.size + QUIET * 2;
        assert!(
            tall.lines().any(|l| l.trim_end().len() >= wide_enough),
            "the code never reached full width"
        );
    }

    /// A place question puts a map at the side, and `/map` flies to that place.
    #[test]
    fn asking_about_a_place_draws_it() {
        let mut a = Ask::new();
        a.input = "where did he go to university?".into();
        a.submit();
        let Some(Panel { what: Show::Place(tour), .. }) = &a.panel else {
            panic!("no map went up for a place question");
        };
        let spot = tour.here();
        assert_eq!(spot.id.as_deref(), Some("silver-oak"));
        assert!(spot.name.contains("Silver Oak"));

        a.input = "/map".into();
        a.submit();
        assert_eq!(a.goto.as_deref(), Some("experience"));
        assert_eq!(a.goto_place.as_deref(), Some("silver-oak"), "the flight lost the place");
    }

    /// `/map <somewhere>` opens the tour on that stop without one having been
    /// asked about first.
    #[test]
    fn the_map_command_takes_a_place() {
        let mut a = Ask::new();
        a.input = "/map innoventa".into();
        a.submit();
        assert_eq!(a.goto.as_deref(), Some("experience"));
        assert_eq!(a.goto_place.as_deref(), Some("innoventa"));
    }

    /// What should and should not raise a map.
    ///
    /// The second list is the point. A panel that fires on anything with a
    /// preposition in it is furniture, and this is a guess at intent -- so the
    /// guess has to be a narrow one.
    #[test]
    fn the_map_panel_is_choosy() {
        let atlas = Atlas { covers: None, ..Atlas::default() };
        for q in [
            "where does he work now?",
            "Where is Prince based?",
            "tell me about gateway corp",
            "what did he do at Innoventa",
            "which city is he in",
            "silver oak university",
        ] {
            assert!(spot_for(q, &atlas).is_some(), "`{q}` drew nothing");
        }
        for q in [
            "what is he good at?",
            "hi",
            "how did you build this terminal?",
            "where does this data come from?",
            "what languages does he write?",
        ] {
            assert!(spot_for(q, &atlas).is_none(), "`{q}` drew a map for no reason");
        }
    }

    /// The current stop answers a bare location question, because "where is he"
    /// is a question about now.
    #[test]
    fn a_bare_location_question_lands_on_the_current_place() {
        let atlas = Atlas { covers: None, ..Atlas::default() };
        let spot = spot_for("where is he these days", &atlas).expect("nothing drawn");
        assert_eq!(spot.id.as_deref(), Some("gateway"));
    }

    /// Asking twice about the same place leaves the picture alone rather than
    /// fading it in again under the second answer.
    #[test]
    fn the_same_place_twice_does_not_re_arrive() {
        let mut a = Ask::new();
        a.input = "where is gateway corp".into();
        a.submit();
        a.tick(1.0);
        assert_eq!(a.panel.as_ref().map(|p| p.fade()), Some(1.0));
        a.input = "what does he do at gateway".into();
        a.submit();
        assert_eq!(a.panel.as_ref().map(|p| p.fade()), Some(1.0), "it faded in a second time");
    }

    /// Somewhere the basemap has no tiles for gets no picture. Without this a
    /// visitor from outside the archive's extent is shown a black rectangle.
    #[test]
    fn a_place_off_the_basemap_is_not_drawn() {
        let atlas = Atlas { places: termap::place::load(), covers: Some([0.0, 0.0, 0.01, 0.01]) };
        // The tour stops are drawn regardless -- they are on the sheet, and the
        // sheet is the deployment saying these are the places. It is the visitor
        // lookup, which can land anywhere on earth, that is checked.
        assert!(spot_for("where does he work", &atlas).is_some());
        assert!(spot_for("where am i", &atlas).is_none());
    }

    #[test]
    fn clear_empties_the_page_and_the_panel() {
        let mut a = Ask::new();
        a.input = "/coffee".into();
        a.submit();
        assert!(a.panel.is_some());
        a.input = "/clear".into();
        a.submit();
        assert!(a.turns.is_empty());
        assert!(a.panel.is_none(), "the panel outlived the conversation");
    }

    /// A panel arrives rather than appearing, and stops once it has.
    #[test]
    fn a_panel_fades_in_and_then_holds() {
        let mut a = Ask::new();
        a.input = "/coffee".into();
        a.submit();
        let p = a.panel.as_ref().expect("no panel");
        assert_eq!(p.fade(), 0.0, "it started already visible");
        assert!(p.moving());
        a.tick(1.0);
        let p = a.panel.as_ref().expect("no panel");
        assert_eq!(p.fade(), 1.0, "it never finished arriving");
        assert!(!p.moving(), "the fade is still running after a second");
    }

    /// The wait is the answer arriving, so it exists only while one is -- and it
    /// is a gradient, not a wall.
    #[test]
    fn the_tip_of_a_live_answer_settles_rather_than_snapping() {
        let line = "the vertical resolution of a half block, which is what lets it read";
        let text = |spans: &Vec<Span<'static>>| -> String {
            spans.iter().map(|s| s.content.to_string()).collect()
        };
        let one = text(&settling(line, 100, 1.0));
        let two = text(&settling(line, 100, 2.0));
        assert_ne!(one, two, "the churn is not moving");

        // The beginning of the line has stopped moving and the end has not.
        // That difference *is* the effect: with a hard edge the two halves would
        // be identical up to a boundary and then unrelated.
        let head = |s: &str| s.chars().take(30).collect::<String>();
        assert_eq!(head(&one), head(&two), "settled words are still flickering");
        assert!(one.starts_with("the vertical resolution"), "the words were eaten: {one}");

        // Somewhere in the middle, characters differ between frames but the line
        // is still mostly the real text.
        let same = one.chars().zip(two.chars()).filter(|(a, b)| a == b).count();
        let total = one.chars().count().min(two.chars().count());
        assert!(same * 100 / total > 55, "too much of the line is noise");
        assert!(same < total, "nothing is churning at all");

        // A finished turn draws none of it.
        let mut a = Ask::new();
        a.turns.push(Turn {
            q: "q".into(),
            a: "done".into(),
            done: true,
            ..Default::default()
        });
        a.state = State::Ready;
        let s = drawn(&a, 92, 20);
        assert!(s.contains("done"));
    }


    /// Stopping is not failing: the section goes back to ready and the tier is
    /// left alone, because the visitor asked for it.
    #[test]
    fn cancelling_leaves_the_section_ready_rather_than_failed() {
        let mut a = Ask::new();
        ready(&mut a);
        a.turns.push(Turn { q: "long one".into(), ..Default::default() });
        a.state = State::Thinking;
        a.apply(Event::Cancelled);
        assert_eq!(a.state, State::Ready);
        assert!(a.turns[0].done);
        assert!(a.turns[0].cancelled);
        assert_eq!(a.turns[0].a, "Stopped.");
    }

    /// Escape means two different things and must not do the wrong one. While an
    /// answer is coming it stops the turn; otherwise it clears the line.
    #[test]
    fn escape_clears_the_line_only_when_nothing_is_running() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut a = Ask::new();
        a.input = "half a question".into();
        a.state = State::Thinking;
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.input, "half a question", "the question was thrown away mid-answer");

        a.state = State::Ready;
        a.on_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(a.input, "");
    }

    /// The narrowest sane terminal must not lose a refusal to the measure, and
    /// must not panic on the arithmetic that lays the panel out.
    #[test]
    fn the_panel_survives_a_narrow_screen() {
        let mut a = Ask::new();
        ready(&mut a);
        for w in [30u16, 40, 62, 80, 200] {
            let s = drawn(&a, w, 30);
            for t in crate::gates::TOOLS {
                assert!(s.contains(t.name), "{} lost at width {w}:\n{s}", t.name);
            }
            // The longest capability name is the one that gets clipped first.
            assert!(s.contains("elicitation"), "clipped at width {w}:\n{s}");
        }
        // Below the section's own floor it draws nothing rather than panicking.
        let _ = drawn(&a, 20, 6);

        // And with a map at the side, which reserves a picture out of the same
        // arithmetic and then puts a caption under whatever is left.
        let mut a = Ask::new();
        ready(&mut a);
        a.input = "where does he work".into();
        a.submit();
        assert!(matches!(&a.panel, Some(Panel { what: Show::Place(_), .. })));
        for (w, h) in [(20u16, 6u16), (30, 8), (95, 12), (96, 9), (100, 40), (240, 60)] {
            let drawn = drawn(&a, w, h);
            // Either the whole panel is off -- there is no room for one -- or
            // the caption is there. What must never happen is a picture with
            // nothing under it saying what it is.
            if let Some((at, _, _)) = map_panel(Rect { x: 0, y: 0, width: w, height: h }, &a) {
                assert!(
                    drawn.contains("Gateway Corp"),
                    "a map at {at:?} with no caption, at {w}x{h}:\n{drawn}"
                );
            }
        }
    }

    /// Eight lookups in one answer read as one block, not eight rows.
    #[test]
    fn calls_of_one_tool_are_grouped_under_its_name() {
        let mut a = Ask::new();
        let places = ["Ward's Lake", "Shillong Peak", "Elephant Falls", "Umiam Lake"];
        let mut calls: Vec<Call> = places
            .iter()
            .enumerate()
            .map(|(i, p)| Call {
                id: format!("c{i}"),
                title: "locate_place".into(),
                status: Status::Done,
                detail: (*p).to_string(),
            })
            .collect();
        calls.push(Call {
            id: "s".into(),
            title: "show_map".into(),
            status: Status::Done,
            detail: "Shillong  25.576, 91.883".into(),
        });
        a.turns.push(Turn { q: "tour me".into(), a: "Here.".into(), calls, done: true, ..Default::default() });
        a.state = State::Ready;

        let out = drawn(&a, 110, 30);
        // The tool's name once, its arguments each on their own line.
        assert_eq!(out.matches("locate_place").count(), 1, "the name repeated:\n{out}");
        for p in places {
            assert!(out.contains(p), "`{p}` is missing:\n{out}");
        }
        assert!(out.contains("show_map"), "the second tool vanished:\n{out}");
    }

    /// The agent's own row for a tool we serve is dropped: it is the same call,
    /// with a renamed title and no arguments.
    #[test]
    fn a_tool_we_serve_is_reported_once() {
        let mut a = Ask::new();
        a.turns.push(Turn { q: "where".into(), ..Default::default() });
        a.apply(Event::Tool(Call {
            id: "acp".into(),
            title: "portfolio-locate_place".into(),
            status: Status::Running,
            detail: String::new(),
        }));
        assert!(a.turns[0].calls.is_empty(), "the agent's duplicate was kept");

        // Something the agent really does own still shows.
        a.apply(Event::Tool(Call {
            id: "w".into(),
            title: "web_fetch".into(),
            status: Status::Done,
            detail: "https://example.com".into(),
        }));
        assert_eq!(a.turns[0].calls.len(), 1, "a real tool call was dropped");
    }

    /// A tool call is reported several times as it runs. Each report carries
    /// the id of the call that opened, so they have to collapse onto one row
    /// -- otherwise a single fetch reads as three separate ones.
    #[test]
    fn a_tool_call_updates_in_place_rather_than_stacking_up() {
        let mut a = Ask::new();
        a.turns.push(Turn { q: "hi".into(), ..Default::default() });

        for e in [
            Event::Tool(call("t1", Status::Running)),
            Event::Tool(call("t2", Status::Running)),
            Event::Tool(call("t1", Status::Done)),
        ] {
            a.apply(e);
        }

        let calls = &a.turns[0].calls;
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0].id, "t1");
        assert_eq!(calls[0].status, Status::Done, "the update did not land");
        // Order is the order they started in, not the order they finished.
        assert_eq!(calls[1].id, "t2");
        assert_eq!(calls[1].status, Status::Running);
    }

    /// `/reach` must never reach the agent: it is a message for a person, and
    /// a model in the middle would paraphrase it, or be down.
    #[test]
    fn a_reach_message_becomes_a_turn_without_asking_the_agent() {
        let _guard = crate::reach::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("askreach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("PORTFOLIO_MESSAGES", dir.join("m.jsonl"));

        let mut a = Ask::new();
        // No client at all -- the agent has never been woken. This still works.
        a.input = "/reach hello, the map is lovely".into();
        a.submit();

        assert_eq!(a.turns.len(), 1);
        assert!(a.turns[0].q.starts_with("/reach"));
        assert!(a.turns[0].done);
        assert!(a.turns[0].a.contains("Left with him"), "{}", a.turns[0].a);
        assert!(a.input.is_empty());
        assert_ne!(a.state, State::Thinking, "it went to the agent");

        let text = std::fs::read_to_string(dir.join("m.jsonl")).unwrap();
        assert!(text.contains("the map is lovely"));

        std::env::remove_var("PORTFOLIO_MESSAGES");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_url_is_trimmed_on_a_character_boundary() {
        assert_eq!(ellipsis("abcdef", 10), "abcdef");
        assert_eq!(ellipsis("abcdef", 4), "abc\u{2026}");
        assert_eq!(ellipsis("", 4), "");
        assert_eq!(ellipsis("abc", 0), "");
        // Would panic on a byte index.
        assert_eq!(ellipsis("héllo wörld", 4), "hél\u{2026}");
    }
}

#[cfg(test)]
mod look {
    use super::*;

    /// Not an assertion so much as a way to look at the thing: `cargo test
    /// look -- --nocapture` prints the states a snapshot cannot reach, because
    /// `--snapshot` has no way to type into the page.
    #[test]
    fn print_the_states() {
        let mut a = Ask::new();
        a.state = State::Ready;
        a.input = "/c".into();
        println!("\n=== palette ===\n{}", super::tests::drawn(&a, 100, 18));

        let mut a = Ask::new();
        a.state = State::Ready;
        a.input = "/coffee".into();
        a.submit();
        a.tick(1.0);
        println!("\n=== coffee, panel faded in ===\n{}", super::tests::drawn(&a, 120, 30));

        let mut a = Ask::new();
        a.state = State::Ready;
        a.input = "/cert".into();
        a.submit();
        a.tick(1.0);
        println!("\n=== cert, badge only ===\n{}", super::tests::drawn(&a, 120, 28));
        println!("\n=== cert, with the code ===\n{}", super::tests::drawn(&a, 130, 50));
        // `ASK_ANSI=/tmp/f.ans cargo test look` writes a real escape-sequence
        // frame, which `map/scripts/ansi2png.py` turns into something you can
        // look at. The plain dump above loses every colour, and the badge is
        // two colours and a shape.
        if let Ok(to) = std::env::var("ASK_ANSI") {
            use ratatui::backend::TestBackend;
            use ratatui::Terminal;
            let mut t = Terminal::new(TestBackend::new(130, 50)).unwrap();
            t.draw(|f| render(f, f.area(), &a)).unwrap();
            std::fs::write(to, termap::snapshot::ansi(t.backend().buffer())).unwrap();
        }

        let mut a = Ask::new();
        a.state = State::Thinking;
        a.turns.push(Turn {
            q: "why braille for the map?".into(),
            a: "Braille gives four times the vertical resolution of a half block, which is\nwhat lets a coastline read as a line".into(),
            ..Default::default()
        });
        // Several frames, because the point of it is that it changes: a still of
        // a settling animation says nothing about whether it settles.
        for step in 0..6 {
            a.t = 1.0 + step as f64 * 0.12;
            println!("\n=== settling, frame {step} ===\n{}", super::tests::drawn(&a, 100, 10));
        }
    }
}
