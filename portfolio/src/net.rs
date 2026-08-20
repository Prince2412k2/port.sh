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

use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Pty};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::Duration;

use crate::session;

/// Concurrent sessions. Without a ceiling, opening connections is a free way
/// to grow the process without bound -- each session is a `Shell`, a decoder
/// and a couple of channels, not large, but not nothing at a thousand of them.
const MAX_SESSIONS: usize = 128;

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
        let peer = peer.map(|p| p.to_string()).unwrap_or_default();
        SessionHandler {
            sessions: Arc::clone(&self.sessions),
            who: crate::visits::Who {
                via: "ssh",
                // The address without the ephemeral port, which is noise.
                ip: peer.rsplit_once(':').map(|(a, _)| a.to_string()).unwrap_or_else(|| peer.clone()),
                ..Default::default()
            },
            peer,
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
    /// Who is at the other end, as far as the handshake said. Both halves are
    /// offered by the client rather than taken from it: the username is what
    /// they typed in front of the `@`, and the key is the one they chose to
    /// authenticate with.
    who: crate::visits::Who,
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
    ///
    /// Accepting every key does not mean ignoring it. The fingerprint is what
    /// makes a returning visitor recognisable as one -- it is stable across
    /// visits and across addresses, and it costs nobody anything, because
    /// offering it is how they logged in.
    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        self.who.user = user.to_string();
        self.who.id = key.fingerprint(HashAlg::Sha256).to_string();
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
        let who = self.who.clone();
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
                if let Err(e) = run_session(handle.clone(), channel, cols, rows, rx, who).await {
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

/// Bridge one SSH channel to a session. The loop itself lives in `session`,
/// shared with the web transport -- this only moves bytes.
async fn run_session(
    handle: russh::server::Handle,
    channel: ChannelId,
    cols: u16,
    rows: u16,
    mut wire: UnboundedReceiver<Wire>,
    who: crate::visits::Who,
) -> anyhow::Result<()> {
    let (out_tx, mut out_rx) = unbounded_channel::<Vec<u8>>();
    let out_handle = handle.clone();
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if out_handle.data(channel, bytes::Bytes::from(frame)).await.is_err() {
                break;
            }
        }
    });

    let (in_tx, in_rx) = unbounded_channel::<session::In>();
    tokio::spawn(async move {
        while let Some(msg) = wire.recv().await {
            let translated = match msg {
                Wire::Bytes(b) => session::In::Bytes(b),
                Wire::Resize(c, r) => session::In::Resize(c, r),
                Wire::Hangup => session::In::Hangup,
            };
            if in_tx.send(translated).is_err() {
                break;
            }
        }
    });

    let outcome = session::run(out_tx, in_rx, cols, rows, who).await;

    // The last frame a session writes is the one that puts the terminal back --
    // mouse reporting off, cursor shown. `run` has only queued it: the sender
    // it held is dropped as it returns, so waiting for the writer here is what
    // drains that queue. Return without waiting and this handler completes,
    // russh closes the channel, and the visitor is left scrolling `35;82;28M`
    // at their shell.
    let _ = writer.await;
    outcome
}
