//! One visitor's session, independent of how they reached it.
//!
//! SSH and the web terminal are the same program from here down: bytes in,
//! ANSI frames out, a `Shell` in between. Neither transport appears in this
//! file. That is deliberate -- the alternative was a second copy of the render
//! loop for the browser, and two copies of a loop with this much timing in it
//! drift the moment either is touched.
//!
//! There is no OS pty on either path. Nothing here may call anything that
//! would query the *server's* controlling terminal (cursor position, terminal
//! size, raw mode): there isn't one, and the answer would describe the wrong
//! machine anyway. Every escape sequence is written explicitly.

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Hide;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::io::Write as _;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::{interval, Duration, Instant};

use crate::shell::Shell;
use crate::wire::{Decoder, DISABLE_MOUSE, ENABLE_MOUSE};

/// What a transport sends *to* a session.
pub enum In {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    Hangup,
}

/// How long a session may sit with nobody touching it.
///
/// sshd-style keepalives only catch a client that has stopped answering; a
/// browser tab left open in the background answers everything and would hold
/// its session for as long as the machine is on.
pub fn idle_limit() -> Duration {
    let secs = std::env::var("PORTFOLIO_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    Duration::from_secs(secs)
}

/// A sink that hands finished frames back to whichever transport is driving.
///
/// ratatui's crossterm backend wants an `io::Write`; a WebSocket and an SSH
/// channel both want whole messages. Buffering until `flush` is what turns one
/// into the other, and it means a frame crosses the network as one message
/// rather than as however many small writes ratatui happened to make.
pub struct FrameSink {
    tx: UnboundedSender<Vec<u8>>,
    buf: Vec<u8>,
}

impl FrameSink {
    pub fn new(tx: UnboundedSender<Vec<u8>>) -> Self {
        Self { tx, buf: Vec::new() }
    }
}

impl std::io::Write for FrameSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let payload = std::mem::take(&mut self.buf);
        self.tx
            .send(payload)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))
    }
}

pub type Term = Terminal<CrosstermBackend<FrameSink>>;

/// Drive one session to completion. Returns when the visitor quits, hangs up,
/// or goes idle for long enough.
pub async fn run(
    out: UnboundedSender<Vec<u8>>,
    mut input: UnboundedReceiver<In>,
    cols: u16,
    rows: u16,
) -> anyhow::Result<()> {
    let options = TerminalOptions {
        viewport: Viewport::Fixed(Rect::new(0, 0, cols.max(20), rows.max(6))),
    };
    let mut terminal: Term = Terminal::with_options(CrosstermBackend::new(FrameSink::new(out)), options)?;

    execute!(terminal.backend_mut(), EnterAlternateScreen, Hide, Clear(ClearType::All))?;
    terminal.backend_mut().write_all(ENABLE_MOUSE)?;
    terminal.backend_mut().flush()?;

    let mut shell = Shell::new();
    let mut decoder = Decoder::default();
    let idle_after = idle_limit();
    let mut last_input = Instant::now();

    terminal.draw(|f| shell.render(f))?;

    loop {
        // The frame interval is whatever the current section asks for, so a
        // still page costs nothing and a camera flight gets smooth frames.
        let wait = Duration::from_millis(shell.frame_ms());
        let mut ticker = interval(wait);
        ticker.tick().await; // the first tick fires immediately; skip it

        tokio::select! {
            got = input.recv() => match got {
                Some(In::Bytes(bytes)) => {
                    last_input = Instant::now();
                    for ev in decoder.feed(&bytes) {
                        match ev {
                            crossterm::event::Event::Key(k) => shell.on_key(k),
                            crossterm::event::Event::Mouse(m) => shell.on_mouse(m),
                            _ => {}
                        }
                    }
                }
                Some(In::Resize(c, r)) => {
                    terminal.resize(Rect::new(0, 0, c.max(20), r.max(6)))?;
                    // `draw` only sends cells that differ from the last frame,
                    // and `resize` throws away the record it diffs against --
                    // so without this the next frame computes "nothing
                    // changed" against an empty baseline and sends nothing,
                    // leaving whatever the client already had on screen.
                    // `clear` forces the next draw to be a full repaint.
                    terminal.clear()?;
                }
                Some(In::Hangup) | None => break,
            },
            _ = ticker.tick() => {}
        }

        if shell.quit || (!idle_after.is_zero() && last_input.elapsed() > idle_after) {
            break;
        }
        shell.tick(wait.as_secs_f64());
        terminal.draw(|f| shell.render(f))?;
    }

    let w = terminal.backend_mut();
    w.write_all(DISABLE_MOUSE)?;
    execute!(w, LeaveAlternateScreen)?;
    terminal.flush()?;
    Ok(())
}
