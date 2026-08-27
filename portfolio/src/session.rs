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

use ratatui::backend::{Backend, ClearType as BackendClearType, CrosstermBackend, WindowSize};
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::{Position, Rect, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::mpsc::{Sender, UnboundedReceiver};
use tokio::time::{interval, Duration, Instant};

use crate::shell::Shell;
use crate::wire::{Decoder, DISABLE_KEYS, DISABLE_MOUSE, ENABLE_KEYS, ENABLE_MOUSE};

// A remote SSH client may need more than one round trip to answer the initial
// terminal capability query. Falling back too quickly makes higher-latency
// production connections render the portfolio as ASCII permanently.
const TERMINAL_PROBE_WINDOW: Duration = Duration::from_secs(2);

/// What a transport sends *to* a session.
pub enum In {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    ReducedMotion(bool),
    /// Columns at the start of the header row that the client is using for
    /// chrome of its own, and that the app must not draw into.
    ///
    /// Only the browser ever sends one. Over ssh there is nothing on top of the
    /// terminal, so the answer is zero and stays zero.
    Gutter(u16),
    /// The colour the client is drawing its own page on.
    ///
    /// The browser's answer to the question a terminal answers with OSC 11.
    /// There is no terminal under the web client to ask -- the page *is* the
    /// terminal -- so it states it instead, and the app's system theme follows
    /// it exactly as it follows a real one. Which is what lets a shader pick
    /// the ground: an ink-on-paper filter says the page is paper, and the
    /// renderer stops drawing light on black.
    Ground([u8; 3]),
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

pub fn max_limit() -> Duration {
    let secs = std::env::var("PORTFOLIO_MAX_SESSION_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3_600);
    Duration::from_secs(secs)
}

/// A sink that hands finished frames back to whichever transport is driving.
///
/// ratatui's crossterm backend wants an `io::Write`; a WebSocket and an SSH
/// channel both want whole messages. Buffering until `flush` is what turns one
/// into the other, and it means a frame crosses the network as one message
/// rather than as however many small writes ratatui happened to make.
///
/// The channel out is deliberately one frame deep. It used to be unbounded,
/// which sounds harmless and is not: russh's own handle is bounded and so is a
/// socket, so an unbounded queue in front of them is a place for a slow link to
/// pile up minutes of stale screen that the visitor then has to watch arrive.
/// Bounded, the sink can be *asked* whether there is room, and the loop above
/// declines to draw when there isn't.
pub struct FrameSink {
    tx: Sender<Vec<u8>>,
    buf: Vec<u8>,
    ascii: Option<Arc<AtomicBool>>,
}

impl FrameSink {
    pub fn new(tx: Sender<Vec<u8>>) -> Self {
        Self {
            tx,
            buf: Vec::new(),
            ascii: None,
        }
    }

    fn negotiating(tx: Sender<Vec<u8>>, ascii: Arc<AtomicBool>) -> Self {
        let mut sink = Self::new(tx);
        sink.ascii = Some(ascii);
        sink
    }
}

fn ascii_frame(bytes: Vec<u8>) -> Vec<u8> {
    String::from_utf8_lossy(&bytes)
        .chars()
        .map(|ch| match ch {
            '\u{2500}'..='\u{257f}' => match ch {
                '│' | '┃' | '║' => '|',
                '─' | '━' | '═' => '-',
                _ => '+',
            },
            '\u{2580}'..='\u{259f}' => '#',
            '\u{2800}'..='\u{28ff}' => '.',
            '→' | '›' | '»' => '>',
            '←' | '‹' | '«' => '<',
            '↑' => '^',
            '↓' => 'v',
            '•' | '·' | '◆' | '◇' => '*',
            '…' => '.',
            ch if ch.is_ascii() => ch,
            _ => '?',
        })
        .collect::<String>()
        .into_bytes()
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
        let mut payload = std::mem::take(&mut self.buf);
        if self.ascii.as_ref().is_some_and(|ascii| ascii.load(Ordering::Relaxed)) {
            payload = ascii_frame(payload);
        }
        match self.tx.try_send(payload) {
            Ok(()) => Ok(()),
            // Still busy with the frame before this one. These bytes are not
            // dropped -- a frame is a *diff* against what the client has on
            // screen, so a lost one corrupts every frame after it. They go back
            // in the buffer and leave with the next flush, concatenated in
            // order, which is all the far end's parser wanted anyway.
            //
            // This is only ever the hand-written escape sequences: a frame from
            // `draw` is only ever produced when `ready` has already said there
            // is room for it.
            Err(TrySendError::Full(payload)) => {
                self.buf = payload;
                Ok(())
            }
            Err(TrySendError::Closed(_)) => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the transport hung up",
            )),
        }
    }
}

/// The crossterm backend, with the one question it must never be asked
/// answered from here instead.
///
/// The promise at the top of this file -- that nothing queries the server's
/// controlling terminal -- held for drawing and stopped holding for resizing.
/// `Terminal::resize` on a fixed viewport clears the viewport first, and to
/// pick the cheapest way to clear it, it asks the backend how big the screen
/// is. There is no screen: crossterm goes looking for a tty, finds none, and
/// returns `ENXIO`. That error rode the `?` in the resize arm straight out of
/// `run`, so the first resize of a session ended it -- over SSH and over the
/// web alike, because this is the file both share.
///
/// The size was never crossterm's to know. The transport negotiates it and
/// tells us; keeping it here and answering from it is both correct and the
/// only answer that describes the right machine.
pub struct SessionBackend {
    inner: CrosstermBackend<FrameSink>,
    size: Size,
    /// A second handle on the sink's channel, kept only to be asked about it.
    /// ratatui's crossterm backend does not lend its writer back out, so this
    /// is how the loop reaches the one fact it needs: is there room.
    out: Sender<Vec<u8>>,
}

impl SessionBackend {
    fn new(sink: FrameSink, cols: u16, rows: u16) -> Self {
        Self {
            out: sink.tx.clone(),
            inner: CrosstermBackend::new(sink),
            size: Size::new(cols, rows),
        }
    }

    /// Whether the transport can take another frame right now.
    ///
    /// Only meaningful straight after a flush, and that is enough: a flush with
    /// room always empties the buffer, so room left over means nothing is
    /// pending either.
    pub fn ready(&self) -> bool {
        self.out.capacity() > 0
    }

    /// Wait until it can. False once the transport has gone, which is what
    /// stops this from waiting forever on a visitor who has already left.
    async fn room(&self) -> bool {
        self.out.reserve().await.is_ok()
    }

    /// Tell the backend what the far end is now. Must happen before
    /// `Terminal::resize`, which asks during the clear it does on the way.
    fn set_size(&mut self, cols: u16, rows: u16) {
        self.size = Size::new(cols, rows);
    }
}

/// The escape sequences this file writes by hand -- alternate screen, mouse
/// reporting -- go through the backend as bytes, so the wrapper has to carry
/// `Write` across as well as `Backend`.
impl std::io::Write for SessionBackend {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut self.inner, b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.inner)
    }
}

impl Backend for SessionBackend {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    /// Not delegated, and not a stub for its own sake: crossterm answers this
    /// by writing a cursor-position query and *reading the reply back off the
    /// tty*. Down a one-way socket that is the same missing device, and on a
    /// real one it would eat a keystroke. Nothing here moves the cursor by
    /// asking where it is, so the origin is a true answer to a question that
    /// is never really asked.
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(Position::ORIGIN)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: BackendClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        // Pixels are documented as commonly unreported, and nothing here reads
        // them; the cell grid is the part that has to be right.
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::new(0, 0),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Backend::flush(&mut self.inner)
    }
}

pub type Term = Terminal<SessionBackend>;

/// Drive one session to completion. Returns when the visitor quits, hangs up,
/// or goes idle for long enough.
pub async fn run(
    out: Sender<Vec<u8>>,
    mut input: UnboundedReceiver<In>,
    cols: u16,
    rows: u16,
    who: crate::visits::Who,
) -> anyhow::Result<()> {
    let options = TerminalOptions {
        viewport: Viewport::Fixed(Rect::new(0, 0, cols.max(20), rows.max(6))),
    };
    let ascii = Arc::new(AtomicBool::new(true));
    let mut terminal: Term = Terminal::with_options(
        SessionBackend::new(
            FrameSink::negotiating(out, Arc::clone(&ascii)),
            cols.max(20),
            rows.max(6),
        ),
        options,
    )?;

    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        Hide,
        Clear(ClearType::All),
    )?;
    terminal.backend_mut().write_all(b"\x1b[c\x1b[?u")?;
    // What colour is your background? Asked once, alongside the capability
    // probes, because the answer decides which way the whole palette runs
    // under the default theme -- and because a terminal that ignores it costs
    // nothing but the seven bytes.
    terminal.backend_mut().write_all(crate::wire::ASK_BG)?;
    // Both traits the backend implements have a `flush`; this one is pushing
    // the bytes just written, so it is the writer's.
    std::io::Write::flush(terminal.backend_mut())?;

    // Opened here rather than in either transport, so SSH and the web record
    // the same things in the same order. What differs between them -- a key
    // fingerprint versus a browser id -- has already been resolved into `who`
    // by the time it arrives.
    let mut visit = crate::visits::Visit::open(who);
    let mut shell = Shell::new();
    shell.ask.restore(visit.take_saved());
    let outcome = pump(&mut terminal, &mut shell, &mut input, &mut visit, &ascii).await;
    visit.close();

    // Whatever happened up there, the visitor's terminal gets put back. This
    // used to sit at the bottom of the loop, which meant every `?` above
    // skipped it -- and the thing not being undone is mouse reporting, so the
    // session left the terminal emitting `35;82;28M` at the shell for every
    // scroll until it was reset by hand. Best effort, and its error is dropped:
    // if this cannot be written the client has already gone, and there is
    // nothing left to put back.
    // Room first. `restore` writes straight through the sink, and if the
    // transport were still busy with the last frame those bytes would be held
    // back for a flush that is never going to come -- the terminal left with
    // mouse reporting on, which is the exact failure the comment above is
    // about. Waiting is bounded: it returns as soon as the far end goes.
    settle(&mut terminal).await;
    let _ = restore(&mut terminal);
    outcome
}

/// Wait until the transport can take a frame, pushing out anything written by
/// hand that is still waiting for room.
///
/// Two passes: the first sends what is pending, the second waits for room for
/// what comes next. Used either side of the loop, where there is no frame to
/// skip and the bytes simply have to leave.
async fn settle(terminal: &mut Term) {
    for _ in 0..2 {
        if !terminal.backend().room().await {
            return;
        }
        if std::io::Write::flush(terminal.backend_mut()).is_err() {
            return;
        }
    }
}

/// Undo everything the session did to the terminal on the way in.
///
/// The flush is `execute!`'s, and it matters: `execute!` queues and then flushes
/// the writer, which is what actually pushes these bytes through `FrameSink` and
/// out to the client. `Terminal::flush` would not -- it writes the buffer diff
/// and leaves anything hand-written sitting in the sink.
fn restore(terminal: &mut Term) -> anyhow::Result<()> {
    let w = terminal.backend_mut();
    w.write_all(DISABLE_KEYS)?;
    w.write_all(DISABLE_MOUSE)?;
    // `Show` pairs with the `Hide` on the way in. Cursor visibility is not part
    // of what the alternate screen puts back, so without it a visitor leaves
    // with an invisible cursor in the shell they came from.
    execute!(w, LeaveAlternateScreen, Show)?;
    Ok(())
}

/// The session proper: everything between the terminal being set up and being
/// put back. Split out so that `run` can restore on every path out of it.
async fn pump(
    terminal: &mut Term,
    shell: &mut Shell,
    input: &mut UnboundedReceiver<In>,
    visit: &mut crate::visits::Visit,
    ascii: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut decoder = Decoder::default();
    let probe_until = Instant::now() + TERMINAL_PROBE_WINDOW;
    let idle_after = idle_limit();
    let max_after = max_limit();
    let started = Instant::now();
    let mut last_input = Instant::now();

    settle(terminal).await;
    terminal.draw(|f| shell.render(f))?;

    loop {
        // The frame interval is whatever the current section asks for, so a
        // still page costs nothing and a camera flight gets smooth frames.
        let wait = Duration::from_millis(shell.frame_ms());
        let mut ticker = interval(wait);
        ticker.tick().await; // the first tick fires immediately; skip it

        let mut live = true;
        tokio::select! {
            got = input.recv() => {
                live = match got {
                    Some(msg) => absorb(msg, terminal, shell, &mut decoder, &mut last_input, &Probe { until: probe_until, ascii })?,
                    None => false,
                };
                // And everything already queued behind it, before the frame
                // rather than after one apiece. A held-down arrow key arriving
                // as ten separate reads used to cost ten frames -- ten diffs of
                // the selection moving one row -- on a link that had just
                // finished proving it could not keep up. It costs one frame
                // now: row three straight to row thirteen. No input is
                // discarded, only the pictures of the rows in between.
                while live {
                    match input.try_recv() {
                        Ok(msg) => {
                            live = absorb(msg, terminal, shell, &mut decoder, &mut last_input, &Probe { until: probe_until, ascii })?;
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => live = false,
                    }
                }
            }
            _ = ticker.tick() => {}
        }
        if !live {
            break;
        }

        for question in shell.drain_submitted() {
            visit.question(&question);
        }
        for (question, status) in shell.drain_statuses() {
            visit.question_status(&question, status);
        }
        if shell.quit
            || (!idle_after.is_zero() && last_input.elapsed() > idle_after)
            || (!max_after.is_zero() && started.elapsed() > max_after)
        {
            break;
        }
        shell.tick(wait.as_secs_f64());
        // Written as they finish rather than all at once at the end: a session
        // that is killed mid-conversation should still have the conversation.
        for turn in shell.drain_logged() {
            visit.asked_with_panel(&turn.q, &turn.a, turn.spent, turn.panel.as_ref());
        }

        // Push out whatever the arms above hand-wrote, then draw only if the
        // transport can take the frame.
        //
        // When it can't, no frame is produced at all -- not produced and then
        // queued, which is what turns a slow link into a time machine: the
        // state is at frame 105 while the visitor watches 101 arrive. Skipping
        // the draw leaves ratatui's record of the screen exactly as the client
        // last saw it, so the next frame is a diff from *there* to wherever the
        // state has got to by then. One frame, and no catching up to watch.
        //
        // That last part is why this is a gate before the draw and not a filter
        // after it. Dropping a finished frame would be the cheaper-looking
        // change and a corrupting one: ratatui would have already advanced its
        // record to a screen the client never received, and every diff after it
        // would be computed against a lie.
        std::io::Write::flush(terminal.backend_mut())?;
        if terminal.backend().ready() {
            terminal.draw(|f| shell.render(f))?;
        }
    }
    Ok(())
}

/// What the opening capability probe still needs from the loop, once its two
/// halves stopped being local variables in it.
struct Probe<'a> {
    until: Instant,
    ascii: &'a Arc<AtomicBool>,
}

/// Take one message from a transport into the shell.
///
/// Lifted out of the loop so that the messages already queued behind the one
/// `select!` woke on can go through the very same path, rather than a second
/// copy of it that drifts.
///
/// Returns false when the visitor has gone.
fn absorb(
    msg: In,
    terminal: &mut Term,
    shell: &mut Shell,
    decoder: &mut Decoder,
    last_input: &mut Instant,
    probe: &Probe<'_>,
) -> anyhow::Result<bool> {
    match msg {
        In::Bytes(bytes) => {
            *last_input = Instant::now();
            for ev in decoder.feed(&bytes) {
                match ev {
                    crossterm::event::Event::Key(k) => shell.on_key(k),
                    crossterm::event::Event::Mouse(m) => shell.on_mouse(m),
                    _ => {}
                }
            }
            if let Some(rgb) = decoder.take_bg() {
                shell.set_ground(termap::canvas::Ground::of(rgb));
            }
            if decoder.take_da1()
                && Instant::now() <= probe.until
                && probe.ascii.swap(false, Ordering::Relaxed)
            {
                terminal.backend_mut().write_all(ENABLE_MOUSE)?;
                std::io::Write::flush(terminal.backend_mut())?;
                terminal.clear()?;
            }
            if decoder.take_keyboard() && Instant::now() <= probe.until {
                terminal.backend_mut().write_all(ENABLE_KEYS)?;
                std::io::Write::flush(terminal.backend_mut())?;
            }
        }
        In::Resize(c, r) => {
            let (c, r) = (c.max(20), r.max(6));
            // Before the resize, not after: `resize` clears the viewport on its
            // way through and asks the backend how big the screen is to decide
            // how. Told afterwards, it would clear against the *previous* size.
            terminal.backend_mut().set_size(c, r);
            terminal.resize(Rect::new(0, 0, c, r))?;
            // `draw` only sends cells that differ from the last frame, and
            // `resize` throws away the record it diffs against -- so without
            // this the next frame computes "nothing changed" against an empty
            // baseline and sends nothing, leaving whatever the client already
            // had on screen. `clear` forces the next draw to be a full repaint.
            terminal.clear()?;
        }
        In::ReducedMotion(reduced) => shell.set_reduced_motion(reduced),
        In::Ground(rgb) => {
            shell.set_ground(termap::canvas::Ground::of((rgb[0], rgb[1], rgb[2])));
        }
        In::Gutter(cols) => shell.set_gutter(cols),
        In::Hangup => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a whole session from the outside, as a transport would.
    ///
    /// On its own thread with its own single-threaded runtime, because `Shell`
    /// is not `Send` and so neither is this future -- the same reason both real
    /// transports give it a thread.
    fn drive(cols: u16, rows: u16, msgs: Vec<In>) -> anyhow::Result<Vec<u8>> {
        // Somewhere of its own. `run` opens a visit, and a visit appends to the
        // real log unless it is told otherwise -- these tests were writing
        // anonymous arrivals into `portfolio/data/visits.jsonl` and burying the
        // actual visitors under them.
        let _guard = crate::visits::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let log = std::env::temp_dir().join("portfolio-session-test-visits.jsonl");
        std::env::set_var("PORTFOLIO_VISITS", &log);
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel();
        in_tx
            .send(In::Bytes(b"\x1b[?1;2c".to_vec()))
            .expect("session not started yet");
        for m in msgs {
            in_tx.send(m).expect("session not started yet");
        }
        in_tx.send(In::Hangup).expect("session not started yet");

        // Drained as it goes, the way a transport does. The channel out is one
        // frame deep and the loop waits for room in it at both ends of a
        // session, so a test that collected only after `run` returned would
        // wait for a reader that had not started yet -- and never return.
        //
        // The receiver still outlives the session either way: `FrameSink`
        // reports a dropped one as a broken pipe, which would fail the session
        // for the wrong reason.
        let collected = Arc::new(std::sync::Mutex::new(Vec::new()));
        let filling = Arc::clone(&collected);
        let worker = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            let draining = rt.spawn(async move {
                while let Some(frame) = out_rx.recv().await {
                    filling.lock().expect("frames").extend_from_slice(&frame);
                }
            });
            let outcome = rt.block_on(run(
                out_tx,
                in_rx,
                cols,
                rows,
                crate::visits::Who::default(),
            ));
            // `run` has dropped its sender by now, so this ends on its own.
            let _ = rt.block_on(draining);
            outcome
        });
        worker.join().expect("session thread panicked")?;

        let bytes = collected.lock().expect("frames").clone();
        Ok(bytes)
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn an_unidentified_terminal_gets_ascii_art() {
        assert_eq!(String::from_utf8(ascii_frame("╭─◆→⣿".as_bytes().to_vec())).unwrap(), "+-*>.");
    }

    /// The regression this file earned the hard way.
    ///
    /// `Terminal::resize` clears the viewport on its way through, and asks the
    /// backend how big the screen is to decide how. Asked of crossterm on a
    /// server there is no tty to answer, so it returned `ENXIO`, the `?` in the
    /// resize arm carried it out of `run`, and the session ended -- on both
    /// transports, on the first resize anybody made. It reported nothing,
    /// because the web transport threw the error away and a closed socket looks
    /// the same either way.
    ///
    /// Note where this one's teeth are. Run from a terminal, crossterm finds a
    /// tty and answers, so this passes even unfixed; it bites where the bug
    /// actually lived -- a server, a container, CI, anything without one. The
    /// test below is the one that fails either way, because it asks the
    /// question directly rather than depending on the room it is run in.
    #[test]
    fn a_resize_does_not_end_the_session() {
        let out = drive(100, 30, vec![In::Resize(120, 40)]).expect("a resize ended the session");
        assert!(!out.is_empty(), "nothing was drawn");
    }

    /// The visitor's terminal is handed back the way it was found.
    ///
    /// Mouse reporting is the one that hurts: left on, the shell the visitor
    /// returns to prints `35;82;28M` at them for every scroll of the wheel,
    /// because it is being sent mouse packets it never asked for and has no
    /// idea what to do with.
    #[test]
    fn a_session_turns_the_mouse_back_off_on_the_way_out() {
        let out = drive(100, 30, vec![]).expect("session failed");
        let on = find(&out, ENABLE_MOUSE).expect("mouse was never enabled");
        let off = find(&out, DISABLE_MOUSE).expect("mouse was never turned back off");
        assert!(off > on, "the mouse was disabled before it was enabled");
        // And the cursor comes back, which `LeaveAlternateScreen` does not do.
        assert!(
            find(&out, b"\x1b[?25h").is_some(),
            "the cursor was left hidden"
        );
    }

    /// Restoration is a separate step precisely so that it cannot be skipped by
    /// an early return, which is how the mouse got left on in the first place.
    /// This checks the step itself, whatever route reached it.
    #[test]
    fn restoring_emits_both_halves_even_with_nothing_drawn() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let options = TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        };
        let mut terminal: Term =
            Terminal::with_options(SessionBackend::new(FrameSink::new(tx), 80, 24), options)
                .expect("terminal");

        restore(&mut terminal).expect("restore failed");

        let mut out = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            out.extend_from_slice(&frame);
        }
        assert!(
            find(&out, DISABLE_MOUSE).is_some(),
            "no mouse reset: {out:?}"
        );
        assert!(
            find(&out, b"\x1b[?25h").is_some(),
            "no cursor restore: {out:?}"
        );
    }

    /// Every direction, and repeatedly: the size is held on the backend now, so
    /// a stale one would show up as a wrong-sized clear rather than an error.
    #[test]
    fn resizes_in_both_directions_are_survivable() {
        let sizes = vec![
            In::Resize(200, 60),
            In::Resize(80, 24),
            In::Resize(200, 60),
            In::Resize(60, 20),
            In::Resize(120, 40),
        ];
        assert!(
            drive(100, 30, sizes).is_ok(),
            "a sequence of resizes ended the session"
        );
    }

    /// The floor still applies. A one-column terminal is not a terminal, and
    /// the layout code below assumes it has somewhere to draw.
    #[test]
    fn an_absurd_size_is_raised_to_the_floor_rather_than_refused() {
        assert!(drive(100, 30, vec![In::Resize(1, 1)]).is_ok());
        assert!(drive(100, 30, vec![In::Resize(0, 0)]).is_ok());
    }

    /// A frame is a *diff* against what the client has on screen. Drop one and
    /// every frame after it is measured from a screen that never existed, so a
    /// full channel has to mean "later" and can never mean "no".
    #[test]
    fn a_frame_the_transport_cannot_take_yet_is_deferred_not_dropped() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let mut sink = FrameSink::new(tx);

        sink.write_all(b"first").expect("write");
        sink.flush().expect("flush");
        sink.write_all(b"second").expect("write");
        sink.flush().expect("flush");

        assert_eq!(rx.try_recv().expect("first frame"), b"first".to_vec());
        assert!(
            rx.try_recv().is_err(),
            "the second frame went out while the first was still in flight"
        );

        // It left with the next one, concatenated in order, which is all the
        // far end's parser ever wanted.
        sink.write_all(b"third").expect("write");
        sink.flush().expect("flush");
        assert_eq!(
            rx.try_recv().expect("the deferred bytes"),
            b"secondthird".to_vec()
        );
    }

    /// The gate the render loop opens on. Room in the channel means room for a
    /// whole frame, because a flush that finds room always empties the buffer.
    #[test]
    fn the_backend_says_it_is_full_until_the_frame_has_been_taken() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let mut b = SessionBackend::new(FrameSink::new(tx), 80, 24);
        assert!(b.ready(), "nothing has been sent yet");

        b.write_all(b"a frame").expect("write");
        std::io::Write::flush(&mut b).expect("flush");
        assert!(!b.ready(), "a frame is still in flight");

        rx.try_recv().expect("frame");
        assert!(b.ready(), "the transport has taken it");
    }

    /// Ten messages waiting when the loop wakes up are ten changes to the
    /// state, not ten pictures of it.
    ///
    /// Measured in bytes, because that is where it showed. Each resize forces a
    /// full repaint, so drawing them one apiece put ten screens on the wire to
    /// arrive at the screen one would have reached. The two runs differ only in
    /// how much input is queued ahead of the session, which makes this a
    /// self-calibrating ratio rather than a number that has to be guessed: with
    /// neither the drain nor the gate it measured 72 KB against 307 KB.
    ///
    /// It cannot say which of the two is doing the work -- either alone is
    /// enough to flatten it -- only that the backlog is gone. The two tests
    /// above pin the gate's own mechanism.
    #[test]
    fn a_burst_of_queued_input_costs_one_pass_not_one_apiece() {
        let once = drive(100, 30, vec![In::Resize(120, 40)]).expect("session failed");
        let ten = drive(
            100,
            30,
            (0..10).map(|_| In::Resize(120, 40)).collect(),
        )
        .expect("session failed");
        assert!(
            ten.len() < once.len() * 2,
            "ten queued resizes cost {} bytes against {} for one -- \
             each is still being drawn separately",
            ten.len(),
            once.len()
        );
    }

    /// The backend answers about the session, not about whatever tty this
    /// process was started from -- which is the whole point of wrapping it.
    #[test]
    fn the_backend_reports_the_sessions_size_and_never_asks_a_tty() {
        let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let mut b = SessionBackend::new(FrameSink::new(tx), 133, 47);
        assert_eq!(b.size().expect("size"), Size::new(133, 47));
        b.set_size(90, 25);
        assert_eq!(b.size().expect("size"), Size::new(90, 25));
        assert_eq!(
            b.window_size().expect("window size").columns_rows,
            Size::new(90, 25)
        );
        // Answered from here rather than by asking the terminal where the
        // cursor is, which would query a device that is not there.
        assert_eq!(b.get_cursor_position().expect("cursor"), Position::ORIGIN);
    }
}
