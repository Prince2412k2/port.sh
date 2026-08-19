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
                Event::Ready => {
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
            Sent::Unwritable(_) => "That did not save -- the message box is not \
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
            KeyCode::Esc => self.input.clear(),
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
    // The question line lives on the last row; everything else is above it.
    let body = Rect { height: area.height.saturating_sub(2), ..area };

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

    let start = lines.len() as i32 - body.height as i32;
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

    let (mark, hint, style) = match &a.state {
        State::Cold | State::Starting => ("·", "waking the agent…", Style::default().fg(FAINT)),
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

const OPENING: &str = "Ask about the work, the places, or anything else. There \
    is an agent on this box. It can read the web and it can leave Prince a \
    message, and you will see it reach for those as it goes. It cannot run \
    anything or write to this machine.";

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
