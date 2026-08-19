//! The chat, as a page rather than a terminal.
//!
//! Deliberately not a REPL. A chat client with a prompt, a scrollback and a
//! spinner is a shape everyone already has, and putting one inside a portfolio
//! adds nothing to it. So: questions are set as marginalia in the accent, the
//! answer sets as prose in a measure, and the wait is not a spinner but a
//! **tide** — contour lines drawn on the same braille subpixel canvas the map
//! uses, running while the agent thinks and going still when it answers.
//!
//! The tide is a pure function of a clock, like every other animation here, so
//! it can be snapshotted at an exact moment and looked at.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::acp::{self, Call, Event, Status};
use crate::paint::{wrap, ACCENT, CYAN, DIM, FAINT, FG};

/// The measure. Same as the essay's, so the two read as one publication.
const TEXT: u16 = 62;

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
    pub done: bool,
    /// Stopped at the visitor's request rather than finished.
    pub cancelled: bool,
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
    /// Clock for the tide.
    pub t: f64,
    scroll: f64,
}

impl Default for Ask {
    fn default() -> Self {
        Self::new()
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
            scroll: 0.0,
        }
    }

    /// Called when the section is first opened. Spawning a language model to
    /// render a landing page would be rude to both the machine and the account.
    pub fn wake(&mut self, context: &str) {
        if self.client.is_some() {
            return;
        }
        self.client = Some(acp::Ask::spawn(context.to_string()));
        self.state = State::Starting;
    }

    pub fn busy(&self) -> bool {
        matches!(self.state, State::Starting | State::Thinking)
    }

    pub fn tick(&mut self, dt: f64) {
        self.t += dt;
        let Some(c) = &self.client else { return };
        for e in c.poll() {
            self.apply(e);
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
        if q.is_empty() || self.busy() {
            return;
        }
        // Handled here rather than by the agent. A message for Prince should
        // arrive whether or not a model is up, whether or not it is out of
        // quota, and word for word rather than as something's summary of it.
        if let Some(body) = q.strip_prefix("/reach") {
            self.leave(body);
            return;
        }
        let Some(c) = &self.client else { return };
        c.send(&q);
        self.turns.push(Turn { q, ..Default::default() });
        self.input.clear();
        self.state = State::Thinking;
        // Always show the newest exchange; the alternative is typing a question
        // and watching nothing happen because you were scrolled up.
        self.scroll = 0.0;
    }

    /// Put a message in the file, and answer in the transcript so it reads as
    /// part of the conversation rather than as a status bar somewhere.
    fn leave(&mut self, body: &str) {
        use crate::reach::Sent;
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
        self.turns.push(Turn {
            q: format!("/reach{body}"),
            a: said,
            done: true,
            ..Default::default()
        });
        self.input.clear();
        self.scroll = 0.0;
    }

    pub fn on_key(&mut self, k: crossterm::event::KeyEvent) {
        use crossterm::event::{KeyCode, KeyModifiers};
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.input.pop();
            }
            // Escape means "stop" while something is running and "clear the
            // line" otherwise. Cancelling is cooperative, so the wait carries on
            // until the agent answers -- the key is a request, not a kill.
            KeyCode::Esc => {
                if self.state == State::Thinking {
                    if let Some(c) = &self.client {
                        c.cancel();
                    }
                } else {
                    self.input.clear();
                }
            }
            KeyCode::Char('u') if ctrl => self.input.clear(),
            // Guarded: without this, Ctrl-C types a `c` into the question
            // instead of quitting.
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                if self.input.chars().count() < 400 {
                    self.input.push(c);
                }
            }
            _ => {}
        }
    }
}

pub fn render(f: &mut Frame, area: Rect, a: &Ask) {
    if area.width < 30 || area.height < 8 {
        return;
    }
    let w = TEXT.min(area.width.saturating_sub(8));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    // Once there is an answer to read, the whole panel collapses to a single
    // faint line and the reading space is given back. The full version is an
    // invitation; two rows of it above somebody's answer would be furniture.
    let head: u16 = if a.turns.is_empty() { 0 } else { 2 };
    if head > 0 && area.height > 6 {
        status(f, Rect { x, y: area.y, width: w, height: 1 }, a);
        f.render_widget(
            Paragraph::new(Span::styled(
                "\u{2500}".repeat(w as usize),
                Style::default().fg(FAINT),
            )),
            Rect { x, y: area.y + 1, width: w, height: 1 },
        );
    }
    // The question line lives on the last row; everything else is above it.
    let body = Rect {
        y: area.y + head,
        height: area.height.saturating_sub(2 + head),
        ..area
    };

    let put = |f: &mut Frame, y: i32, spans: Vec<Span<'static>>| {
        if y < 0 || y >= body.height as i32 {
            return;
        }
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect { x, y: body.y + y as u16, width: w, height: 1 },
        );
    };

    // Lay the transcript out bottom-up: the newest exchange is the one being
    // read, and it should not move as the answer grows.
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    if a.turns.is_empty() {
        panel(&mut lines, w, a);
        for l in wrap(OPENING, w as usize) {
            lines.push(vec![Span::styled(l, Style::default().fg(DIM))]);
        }
        lines.push(vec![]);
        for s in SUGGESTIONS {
            lines.push(vec![Span::styled(
                format!("  {s}"),
                Style::default().fg(FAINT),
            )]);
        }
    }
    for t in &a.turns {
        for (i, l) in wrap(&t.q, w.saturating_sub(2) as usize).into_iter().enumerate() {
            lines.push(vec![
                Span::styled(if i == 0 { "› " } else { "  " }, Style::default().fg(ACCENT)),
                Span::styled(l, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            ]);
        }
        lines.push(vec![]);
        // What it reached for, live. These sit above the answer because that
        // is the order they happened in, and they stay there afterwards as a
        // record of where the answer came from.
        for c in &t.calls {
            let (glyph, colour) = match c.status {
                // Four frames of a quarter-turn. The clock is the tide's, so
                // everything on the page that moves moves together.
                Status::Running => (
                    ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"]
                        [((a.t * 6.0) as usize) % 4],
                    CYAN,
                ),
                Status::Done => ("\u{2713}", DIM),
                Status::Failed => ("\u{00d7}", ACCENT),
                Status::Refused => ("\u{2300}", ACCENT),
            };
            let label = if c.title.is_empty() { "tool" } else { c.title.as_str() };
            // The URL is the interesting half and the one most likely to be
            // long, so it is what gets trimmed rather than the label.
            let room = (w as usize).saturating_sub(label.chars().count() + 4);
            let detail = ellipsis(&c.detail, room);
            lines.push(vec![
                Span::styled(format!("{glyph} "), Style::default().fg(colour)),
                Span::styled(label.to_string(), Style::default().fg(DIM)),
                Span::styled(
                    if detail.is_empty() { String::new() } else { format!("  {detail}") },
                    Style::default().fg(FAINT),
                ),
            ]);
        }
        if !t.calls.is_empty() {
            lines.push(vec![]);
        }
        if t.a.is_empty() && !t.thought.is_empty() {
            lines.push(vec![Span::styled(
                t.thought.clone(),
                Style::default().fg(FAINT).add_modifier(Modifier::ITALIC),
            )]);
        }
        for para in t.a.split('\n') {
            if para.trim().is_empty() {
                lines.push(vec![]);
                continue;
            }
            for l in wrap(para, w as usize) {
                lines.push(vec![Span::styled(l, Style::default().fg(FG))]);
            }
        }
        lines.push(vec![]);
    }

    // The tide runs where the answer will be, so the wait happens in the space
    // the words are about to occupy rather than beside it.
    if a.busy() {
        let top = (lines.len() as i32).max(0);
        let h = (body.height as i32 - top).max(0) as u16;
        if h >= 3 {
            tide(f, Rect { x, y: body.y + top as u16, width: w, height: h }, a.t);
        }
    }

    // A conversation is anchored to the bottom, because the newest exchange is
    // the one being read and it must not move as the answer grows. The opening
    // is anchored to the top instead: it is taller than a narrow screen once the
    // gate rows wrap, and scrolling it from the bottom would push the policy off
    // the top and leave the suggestions -- exactly the wrong half to keep.
    let start = if a.turns.is_empty() {
        0
    } else {
        lines.len() as i32 - body.height as i32
    };
    for (i, spans) in lines.into_iter().enumerate() {
        put(f, i as i32 - start.max(0), spans);
    }

    // The question line.
    let y = area.y + area.height.saturating_sub(1);
    let rule = Rect { x, y: y.saturating_sub(1), width: w, height: 1 };
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(w as usize),
            Style::default().fg(FAINT),
        )),
        rule,
    );

    // Which tier answered the last hourly check. Shown while waiting because
    // that wait is the one moment it explains anything: the tiers differ in
    // how fast they start, and "waking ollama cloud" is a different promise
    // from "waking github copilot".
    let waking = match crate::health::note() {
        Some(t) => format!("waking {t}…"),
        None => "waking the agent…".to_string(),
    };
    let (mark, hint, style) = match &a.state {
        State::Cold | State::Starting => ("·", waking.as_str(), Style::default().fg(FAINT)),
        State::Ready => ("›", "", Style::default().fg(CYAN)),
        State::Thinking => ("·", "thinking", Style::default().fg(FAINT)),
        State::Failed(m) => ("×", m.as_str(), Style::default().fg(ACCENT)),
    };
    let mut spans = vec![Span::styled(format!("{mark} "), style)];
    if a.input.is_empty() && !hint.is_empty() {
        spans.push(Span::styled(hint.to_string(), Style::default().fg(FAINT)));
    } else {
        spans.push(Span::styled(a.input.clone(), Style::default().fg(FG)));
        if !a.busy() {
            spans.push(Span::styled("▌", Style::default().fg(CYAN)));
        }
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect { x, y, width: w, height: 1 },
    );
}

/// Filled, for a gate that is open. The shut ones carry no marker: their rows
/// are labelled `refused` and `off`, and a hollow dot on each would spend eight
/// columns of a sixty-two column measure repeating the label.
const OPEN: &str = "\u{25cf}";

/// How many tool calls the screen has watched happen.
///
/// Counted from the transcript rather than reported by the client, so it is
/// honestly "what you have seen" -- the authoritative budget lives in `acp.rs`
/// and is the one that actually refuses. Refusals do not count against it,
/// because they were never spent.
fn spent(a: &Ask) -> usize {
    a.turns
        .iter()
        .flat_map(|t| &t.calls)
        .filter(|c| c.status != Status::Refused)
        .count()
}

/// What we are connected to, in one line. Right-aligned, faint, skippable.
///
/// Names the server rather than assuming one: the whole point of `servers.rs` is
/// that this could be anything, and a line that said "opencode" whatever was
/// running would be a lie the moment somebody used the feature.
fn wired(a: &Ask) -> String {
    let Some(l) = &a.link else {
        return match crate::health::checked() {
            true => crate::health::note().unwrap_or_else(|| "no tier answered".into()),
            // Before the first check the tier on screen is the file's first
            // line, which is a guess rather than a verdict.
            false => "not yet checked".into(),
        };
    };
    let mut bits = Vec::new();
    if !l.tier.is_empty() {
        bits.push(l.tier.clone());
    }
    // The server is named because it is genuinely variable now, and a line that
    // said "opencode" whatever was running would be a lie the first time
    // somebody pointed a tier at something else.
    bits.push(l.server.clone());
    if !l.mode.is_empty() {
        bits.push(l.mode.clone());
    }
    bits.push(format!("v{}", l.version));
    bits.join(" \u{b7} ")
}

/// The collapsed header, once there is an answer worth the room.
fn status(f: &mut Frame, area: Rect, a: &Ask) {
    let n = spent(a);
    let left = Span::styled(
        if n > 0 {
            format!("acp  \u{b7}  {n}/{} tools", crate::gates::GATES.tool_calls)
        } else {
            "acp".to_string()
        },
        Style::default().fg(FAINT),
    );
    f.render_widget(Paragraph::new(Line::from(vec![left])), area);
    f.render_widget(
        Paragraph::new(Span::styled(wired(a), Style::default().fg(FAINT))).right_aligned(),
        area,
    );
}

/// What the agent may and may not do, stated before it is asked anything.
///
/// This is the part worth putting on a portfolio. A chat box that says "it
/// cannot run anything" is a claim; a list of every tool with the shut ones
/// still on it, drawn from the same table that does the refusing, is the claim
/// and its evidence in one place. The dots are the gates in `gates.rs` -- if
/// somebody opens one, this fills in without being edited.
fn panel(lines: &mut Vec<Vec<Span<'static>>>, w: u16, a: &Ask) {
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

const SUGGESTIONS: [&str; 5] = [
    "what is the hardest part of netjail?",
    "why braille for the map?",
    "what would he be like to work with?",
    "what should I read from all this?",
    "/reach  ...to leave him a message instead",
];

/// Contour lines that drift while the agent thinks.
///
/// Drawn on termap's subpixel canvas, so this is the same braille the map is
/// made of — the wait belongs to the same object as the rest of the app rather
/// than being borrowed from a loading spinner. Amplitude falls off toward the
/// edges so the field has somewhere to be quiet.
fn tide(f: &mut Frame, area: Rect, t: f64) {
    use termap::canvas::{Canvas, Fog, MAT_DOT, TINT_MONO};
    use termap::raster::{self, Pen};

    let (cw, ch) = (area.width as usize, area.height as usize);
    let mut canvas = Canvas::new(cw, ch);
    let (sw, sh) = (canvas.sw as f64, canvas.sh as f64);

    const LINES: usize = 7;
    for i in 0..LINES {
        let fy = (i as f64 + 0.5) / LINES as f64;
        // Depth carries brightness on this canvas, so the far lines are the
        // faint ones and the band reads as having a middle.
        let depth = ((fy - 0.5).abs() * 2.0) as f32;
        let pen = Pen {
            width: 1.0,
            alpha: 0.55,
            depth: 0.15 + depth * 0.8,
            tint: TINT_MONO,
            mat: MAT_DOT,
            pick: u32::MAX,
            occlude: false,
        };
        // Each line gets its own phase *and* its own rate. With a shared rate
        // they are translated copies of one curve and the field reads as
        // corrugated iron rather than as water.
        let phase = i as f64 * 1.9;
        let k1 = 5.0 + i as f64 * 0.7;
        let k2 = 2.3 + i as f64 * 0.31;
        let mut prev: Option<[f64; 2]> = None;
        let steps = (sw as usize / 2).max(8);
        for s in 0..=steps {
            let u = s as f64 / steps as f64;
            let x = u * sw;
            // Two waves at different rates so the pattern never obviously
            // repeats, and an envelope that dies at both ends.
            let env = (u * std::f64::consts::PI).sin().powf(1.4);
            let a = (u * k1 + t * 0.9 + phase).sin() * 0.55
                + (u * k2 - t * 0.6 + phase * 1.7).sin() * 0.45;
            let y = fy * sh + a * env * sh * 0.11;
            let p = [x, y];
            if let Some(q) = prev {
                raster::line(&mut canvas, q, p, &pen);
            }
            prev = Some(p);
        }
    }
    canvas.resolve(f.buffer_mut(), area, &Fog::default(), true);
}

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
    fn drawn(a: &Ask, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, f.area(), a)).unwrap();
        termap::snapshot::plain(term.backend().buffer())
    }

    fn ready(a: &mut Ask) {
        a.apply(Event::Ready(acp::Ready {
            tier: "github copilot".into(),
            server: "opencode".into(),
            version: 1,
            mode: "plan".into(),
        }));
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
        assert!(s.contains("opencode"), "the server is not named:\n{s}");
        assert!(s.contains("plan"), "the mode is not named:\n{s}");
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
        // The collapsed line still says what we are talking to.
        assert!(full.contains("opencode"), "the server went missing:\n{full}");
    }

    /// A refused call is not a spent one -- it never reached anything.
    #[test]
    fn the_tool_count_ignores_what_was_refused() {
        let mut a = Ask::new();
        a.turns.push(Turn { q: "hi".into(), ..Default::default() });
        a.apply(Event::Tool(call("t1", Status::Done)));
        a.apply(Event::Tool(call("t2", Status::Refused)));
        assert_eq!(spent(&a), 1);
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
