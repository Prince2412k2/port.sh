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

use crate::acp::{self, Event};
use crate::paint::{wrap, ACCENT, CYAN, DIM, FAINT, FG};

/// The measure. Same as the essay's, so the two read as one publication.
const TEXT: u16 = 62;

#[derive(Debug, Clone, Default)]
pub struct Turn {
    pub q: String,
    pub a: String,
    /// The last thing it said it was doing, shown small. Not the answer.
    pub thought: String,
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
        let Some(c) = &self.client else { return };
        c.send(&q);
        self.turns.push(Turn { q, ..Default::default() });
        self.input.clear();
        self.state = State::Thinking;
        // Always show the newest exchange; the alternative is typing a question
        // and watching nothing happen because you were scrolled up.
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

const OPENING: &str = "Ask about the work, the places, the taste, or anything \
    else. This runs a local agent in plan mode — it can read and reason and \
    say so, and it cannot write anything.";

const SUGGESTIONS: [&str; 4] = [
    "what is the hardest thing in netjail?",
    "why braille for the map?",
    "what do Ikiru and The Bear have in common?",
    "would he be any good on a systems team?",
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
