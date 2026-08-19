//! Serving the portfolio over SSH directly, with no OS account behind it.
//!
//! OpenSSH was the first attempt, and it works right up until the goal
//! becomes `ssh <domain>` with no username at all: OpenSSH looks up the
//! requested account in the OS's user database *before* it will run any
//! authentication, `AuthorizedKeysCommand` included, and there is no wildcard
//! for that. Getting past it means either faking account lookups with a
//! custom NSS module, or not asking OpenSSH the question in the first place.
//! This is the second one: the whole transport lives in this process, so a
//! username is just a string in the protocol handshake with no meaning beyond
//! it, and every key -- and every name -- is accepted by construction.
//!
//! The shape of this (the `Write` sink over a channel, the byte-to-event
//! decoder, the tokio::select! loop) is adapted from harbr's SSH layer, which
//! solves the identical problem — a ratatui TUI served to many SSH sessions
//! at once — and is already proven there.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Hide;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Pty};
use std::io::Write as _;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::{interval, Duration, Instant};

use crate::shell::Shell;
use crate::wire::{Decoder, DISABLE_MOUSE, ENABLE_MOUSE};

/// Concurrent sessions. Without a ceiling, opening connections is a free way
/// to grow the process without bound -- each session is a `Shell`, a decoder
/// and a couple of channels, not large, but not nothing at a thousand of them.
const MAX_SESSIONS: usize = 128;

/// Same default as the local terminal path: no keystroke for this long ends
/// the session, so an abandoned tab does not hold a slot forever.
fn idle_limit() -> Duration {
    let secs = std::env::var("PORTFOLIO_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    Duration::from_secs(secs)
}

pub async fn serve(addr: &str, port: u16, host_key: &Path) -> anyhow::Result<()> {
    let key = load_or_create_host_key(host_key)?;
    // Printed on every start, not only when generated: this is the one thing
    // an operator needs to publish so a visitor's client can confirm it is
    // talking to the real host and not something in between. There is no
    // ssh-keygen in this image to compute it after the fact -- no OpenSSH
    // here at all any more -- so the binary is the only thing that can say
    // this to anyone, including you.
    eprintln!("portfolio: host key fingerprint {}", key.fingerprint(HashAlg::Sha256));
    let config = Arc::new(Config {
        // Only what this actually offers. Password and keyboard-interactive
        // are never implemented below, so leaving them out of `methods`
        // rather than just declining every attempt is what stops a client
        // from being invited to try them.
        methods: [russh::MethodKind::PublicKey].as_slice().into(),
        inactivity_timeout: Some(Duration::from_secs(120)),
        auth_rejection_time: Duration::from_secs(1),
        keys: vec![key],
        nodelay: true,
        ..Default::default()
    });

    eprintln!("portfolio: listening on {addr}:{port}");
    let mut server = Listener { sessions: Arc::new(AtomicUsize::new(0)) };
    server.run_on_address(config, (addr, port)).await?;
    Ok(())
}

fn load_or_create_host_key(path: &Path) -> anyhow::Result<PrivateKey> {
    if path.is_file() {
        let raw = std::fs::read_to_string(path)?;
        return Ok(PrivateKey::from_openssh(&raw)?);
    }
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    eprintln!("portfolio: generated a new host key at {}", path.display());
    Ok(key)
}

struct Listener {
    sessions: Arc<AtomicUsize>,
}

impl russh::server::Server for Listener {
    type Handler = SessionHandler;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Self::Handler {
        SessionHandler {
            sessions: Arc::clone(&self.sessions),
            peer: peer.map(|p| p.to_string()).unwrap_or_default(),
            pty: None,
            tx: None,
        }
    }
}

struct SessionHandler {
    sessions: Arc<AtomicUsize>,
    peer: String,
    pty: Option<(u16, u16)>,
    tx: Option<UnboundedSender<Wire>>,
}

/// What the SSH side hands the running `Shell`.
enum Wire {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    Hangup,
}

impl Handler for SessionHandler {
    type Error = anyhow::Error;

    /// The whole point. No account, no key list, no state to consult --
    /// anyone who shows up gets a session. There is nothing behind this
    /// login worth protecting with a gate, and gating a public CV would only
    /// turn away the people it exists for.
    async fn auth_publickey(&mut self, _user: &str, _key: &PublicKey) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let _ = channel.id();
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty = Some((col_width as u16, row_height as u16));
        if let Some(tx) = &self.tx {
            let _ = tx.send(Wire::Resize(col_width as u16, row_height as u16));
        }
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some((cols, rows)) = self.pty else {
            let handle = session.handle();
            let _ = handle
                .data(channel, bytes::Bytes::from_static(b"needs a pty: ssh -t <host>\r\n"))
                .await;
            session.channel_failure(channel)?;
            return Ok(());
        };
        if self.sessions.fetch_add(1, Ordering::SeqCst) >= MAX_SESSIONS {
            self.sessions.fetch_sub(1, Ordering::SeqCst);
            let handle = session.handle();
            let _ = handle
                .data(channel, bytes::Bytes::from_static(b"busy right now -- try again shortly\r\n"))
                .await;
            session.channel_failure(channel)?;
            return Ok(());
        }

        let (tx, rx) = unbounded_channel();
        self.tx = Some(tx);
        session.channel_success(channel)?;

        let handle = session.handle();
        let sessions = Arc::clone(&self.sessions);
        let peer = self.peer.clone();
        // A dedicated OS thread and its own single-threaded runtime, not
        // `tokio::spawn` on the shared one. `Shell` holds termap's tile cache,
        // which shares tiles between draws with `Rc` rather than `Arc` -- fine
        // for a synchronous binary talking to one real terminal, but not
        // `Send`, and the shared runtime requires that of anything it spawns.
        // Giving each session its own thread sidesteps the question rather
        // than forcing an `Rc` the rest of that crate has no use for into an
        // `Arc` it does not need. It also means one visitor's tile decoding
        // cannot stall anyone else's session, which a cooperative single
        // thread shared across all sessions would not guarantee.
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            rt.block_on(async move {
                if let Err(e) = run_session(handle.clone(), channel, cols, rows, rx).await {
                    eprintln!("portfolio: session from {peer} ended: {e:#}");
                }
                sessions.fetch_sub(1, Ordering::SeqCst);
                let _ = handle.close(channel).await;
            });
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // This is a TUI, not a command surface -- say so rather than hang.
        let handle = session.handle();
        let _ = handle
            .data(channel, bytes::Bytes::from_static(b"this has no command interface; connect with -t\r\n"))
            .await;
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.pty = Some((col_width as u16, row_height as u16));
        if let Some(tx) = &self.tx {
            let _ = tx.send(Wire::Resize(col_width as u16, row_height as u16));
        }
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Wire::Bytes(data.to_vec()));
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Wire::Hangup);
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Wire::Hangup);
        }
        Ok(())
    }
}

type Term = Terminal<CrosstermBackend<ChannelWriter>>;

/// An `io::Write` sink that ships bytes down an SSH channel, so ratatui's own
/// crossterm backend can render straight into the client's terminal. There is
/// no OS pty here for it to write to instead.
struct ChannelWriter {
    tx: UnboundedSender<Vec<u8>>,
    buf: Vec<u8>,
}

impl std::io::Write for ChannelWriter {
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

async fn run_session(
    handle: russh::server::Handle,
    channel: ChannelId,
    cols: u16,
    rows: u16,
    mut wire: UnboundedReceiver<Wire>,
) -> anyhow::Result<()> {
    let (out_tx, mut out_rx) = unbounded_channel::<Vec<u8>>();
    let out_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            if out_handle.data(channel, bytes::Bytes::from(bytes)).await.is_err() {
                break;
            }
        }
    });

    let backend = CrosstermBackend::new(ChannelWriter { tx: out_tx.clone(), buf: Vec::new() });
    let options = TerminalOptions {
        viewport: Viewport::Fixed(Rect::new(0, 0, cols.max(20), rows.max(6))),
    };
    let mut terminal: Term = Terminal::with_options(backend, options)?;

    // Everything here is a plain ANSI write, never a call that would ask the
    // *server's* controlling terminal something -- there is no such thing on
    // this end, and asking would either error or describe the wrong machine.
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        Hide,
        Clear(ClearType::All),
    )?;
    terminal.backend_mut().write_all(ENABLE_MOUSE)?;
    terminal.backend_mut().flush()?;

    let mut shell = Shell::new();
    let mut decoder = Decoder::default();
    let idle_after = idle_limit();
    let mut last_input = Instant::now();

    terminal.draw(|f| shell.render(f))?;

    loop {
        let wait = Duration::from_millis(shell.frame_ms());
        let mut ticker = interval(wait);
        ticker.tick().await; // the first tick fires immediately; skip it

        tokio::select! {
            got = wire.recv() => match got {
                Some(Wire::Bytes(bytes)) => {
                    last_input = Instant::now();
                    for ev in decoder.feed(&bytes) {
                        match ev {
                            crossterm::event::Event::Key(k) => shell.on_key(k),
                            crossterm::event::Event::Mouse(m) => shell.on_mouse(m),
                            _ => {}
                        }
                    }
                }
                Some(Wire::Resize(c, r)) => {
                    terminal.resize(Rect::new(0, 0, c.max(20), r.max(6)))?;
                }
                Some(Wire::Hangup) | None => break,
            },
            _ = ticker.tick() => {}
        }

        if shell.quit || last_input.elapsed() > idle_after {
            break;
        }
        let now = Instant::now();
        shell.tick(wait.as_secs_f64());
        let _ = now;
        terminal.draw(|f| shell.render(f))?;
    }

    let w = terminal.backend_mut();
    w.write_all(DISABLE_MOUSE)?;
    execute!(w, LeaveAlternateScreen)?;
    terminal.flush()?;
    Ok(())
}
