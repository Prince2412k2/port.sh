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

use std::net::IpAddr;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, ChannelOpenFailure, ChannelWriteHalf, Pty};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::time::Duration;

use crate::crowd::{key as crowd_key, Crowd};
use crate::session;

/// Concurrent sessions. Without a ceiling, opening connections is a free way
/// to grow the process without bound -- each session is a `Shell`, a decoder
/// and a couple of channels, not large, but not nothing at a thousand of them.
const MAX_SESSIONS: usize = 128;
const MAX_CONNECTIONS: usize = 192;
const PER_ADDRESS_CONNECTIONS: usize = 4;

/// Sessions one address may hold at once.
///
/// One. A visitor reads a CV in one terminal; anything past that is a script,
/// a stuck client reconnecting, or somebody seeing how many they can open.
///
/// **The cost is real and worth stating.** Everyone behind one office NAT, one
/// university, or one mobile carrier's CGNAT shares an address, so the second
/// person there is refused because of the first. That is the trade this makes
/// on purpose: the thing being protected is a small box that gives every
/// session an OS thread, a `Shell` and a tile cache, and the common case is one
/// person per address. Every refusal is logged with the address so the other
/// reading of a lot of refusals -- that they are all arriving from one place --
/// is visible rather than mysterious.
const PER_ADDRESS: usize = 1;

/// What a session holds while it runs: one of the box's slots, and one of its
/// address's.
///
/// A guard rather than a pair of decrements at the end of the session thread,
/// which is what the box's counter used to be. Two reasons. A panic inside the
/// session would skip a manual decrement and leak the slot for the life of the
/// process -- invisible for the global count, and for an address it means that
/// visitor can never come back. And two things released in two places is one
/// early `return` away from being released once.
struct Seat {
    total: Arc<AtomicUsize>,
    crowd: Arc<Crowd>,
    key: Option<String>,
}

struct Connection {
    total: Arc<AtomicUsize>,
    crowd: Arc<Crowd>,
    key: Option<String>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.total.fetch_sub(1, Ordering::SeqCst);
        if let Some(key) = &self.key {
            self.crowd.give_back(key);
        }
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.total.fetch_sub(1, Ordering::SeqCst);
        if let Some(key) = &self.key {
            self.crowd.give_back(key);
        }
    }
}

pub async fn serve(addr: &str, port: u16, host_key: &Path) -> anyhow::Result<()> {
    let key = load_or_create_host_key(host_key)?;
    // Printed on every start, not only when generated: this is the one thing
    // an operator needs to publish so a visitor's client can confirm it is
    // talking to the real host and not something in between. There is no
    // ssh-keygen in this image to compute it after the fact -- no OpenSSH
    // here at all any more -- so the binary is the only thing that can say
    // this to anyone, including you.
    crate::visits::operational(
        "info",
        "ssh_host_key",
        &key.fingerprint(HashAlg::Sha256).to_string(),
    );
    let digest = Sha256::digest(key.public_key().to_bytes()?);
    let sshfp = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    crate::visits::operational("info", "sshfp", &format!("SSHFP 4 2 {sshfp}"));
    let config = Arc::new(Config {
        // Only what this actually offers. Password and keyboard-interactive
        // are never implemented below, so leaving them out of `methods`
        // rather than just declining every attempt is what stops a client
        // from being invited to try them.
        methods: [russh::MethodKind::None, russh::MethodKind::PublicKey].as_slice().into(),
        inactivity_timeout: Some(Duration::from_secs(1_200)),
        auth_rejection_time: Duration::from_secs(1),
        keys: vec![key],
        nodelay: true,
        ..Default::default()
    });

    crate::visits::operational("info", "ssh_listen", &format!("{addr}:{port}"));
    let socket = tokio::net::TcpListener::bind((addr, port)).await?;
    let mut server =
        Listener {
            connections: Arc::new(AtomicUsize::new(0)),
            connection_crowd: Arc::new(Crowd::default()),
            sessions: Arc::new(AtomicUsize::new(0)),
            crowd: Arc::new(Crowd::default()),
        };
    let running = server.run_on_socket(config, &socket);
    let handle = running.handle();
    tokio::pin!(running);
    tokio::select! {
        result = &mut running => result?,
        _ = shutdown_signal() => {
            handle.shutdown("server shutting down".into());
            let _ = tokio::time::timeout(Duration::from_secs(10), &mut running).await;
        }
    }
    Ok(())
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
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
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("new-{}-{nonce}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(key.to_openssh(russh::keys::ssh_key::LineEnding::LF)?.as_bytes())?;
    file.sync_all()?;
    match std::fs::hard_link(&temporary, path) {
        Ok(()) => {
            std::fs::remove_file(&temporary)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(&temporary)?;
            let raw = std::fs::read_to_string(path)?;
            return Ok(PrivateKey::from_openssh(&raw)?);
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.into());
        }
    }
    crate::visits::operational("info", "ssh_host_key_created", &path.display().to_string());
    Ok(key)
}

struct Listener {
    connections: Arc<AtomicUsize>,
    connection_crowd: Arc<Crowd>,
    sessions: Arc<AtomicUsize>,
    crowd: Arc<Crowd>,
}

impl russh::server::Server for Listener {
    type Handler = SessionHandler;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Self::Handler {
        let ip = peer.map(|p| p.ip());
        let connection_key = ip.and_then(crowd_key);
        let below_global = self.connections.fetch_add(1, Ordering::SeqCst) < MAX_CONNECTIONS;
        let mut connection = Connection {
            total: Arc::clone(&self.connections),
            crowd: Arc::clone(&self.connection_crowd),
            key: None,
        };
        let below_address = connection_key.as_ref().is_none_or(|key| {
            if self.connection_crowd.take(key, PER_ADDRESS_CONNECTIONS) {
                connection.key = Some(key.clone());
                true
            } else {
                false
            }
        });
        let peer = peer.map(|p| p.to_string()).unwrap_or_default();
        SessionHandler {
            admitted: below_global && below_address,
            rate_checked: false,
            _connection: connection,
            sessions: Arc::clone(&self.sessions),
            crowd: Arc::clone(&self.crowd),
            // Kept parsed rather than re-split out of the string below: the
            // string form of a v6 peer is `[::1]:22`, and taking the address
            // off it by hand is how a bracket ends up in a map key.
            ip,
            who: crate::visits::Who {
                via: "ssh",
                // The address without the ephemeral port, which is noise.
                ip: peer.rsplit_once(':').map(|(a, _)| a.to_string()).unwrap_or_else(|| peer.clone()),
                ..Default::default()
            },
            peer,
            channel: None,
            write: None,
            pty: None,
            started: false,
            tx: None,
        }
    }
}

struct SessionHandler {
    admitted: bool,
    rate_checked: bool,
    _connection: Connection,
    sessions: Arc<AtomicUsize>,
    crowd: Arc<Crowd>,
    /// Where this connection came from, if the transport said. `None` is
    /// treated as unlimited rather than refused -- see `shell_request`.
    ip: Option<IpAddr>,
    peer: String,
    channel: Option<ChannelId>,
    /// The half of the channel that frames are written through.
    ///
    /// `Handle::data` would be the obvious way to write them and is the wrong
    /// one: it hands the bytes to the session loop, which parks anything the
    /// client has no window for in an unbounded `VecDeque` and returns as if it
    /// had sent them. Nothing upstream can tell a frame that left from a frame
    /// that is being stockpiled. This half's `data_bytes` waits for the window
    /// instead, which is what lets `session::run` find out that the far end is
    /// behind and stop drawing.
    write: Option<ChannelWriteHalf<Msg>>,
    pty: Option<(u16, u16)>,
    started: bool,
    tx: Option<UnboundedSender<Wire>>,
    /// Who is at the other end, as far as the handshake said. Both halves are
    /// offered by the client rather than taken from it: the username is what
    /// they typed in front of the `@`, and the key is the one they chose to
    /// authenticate with.
    who: crate::visits::Who,
}

impl SessionHandler {
    fn admit_identity(&mut self) -> bool {
        if !self.admitted {
            return false;
        }
        if self.rate_checked {
            return true;
        }
        let mut keys = Vec::new();
        match self.ip {
            Some(ip) => {
                if let Some(key) = crowd_key(ip) {
                    keys.push(format!("ip:{key}"));
                }
            }
            None => keys.push("ip:unknown".into()),
        }
        if !self.who.id.is_empty() {
            keys.push(format!("ssh-key:{}", self.who.id));
        }
        self.rate_checked = crate::budget::admit_visit(&keys);
        self.rate_checked
    }
}

/// Say one line to a visitor who is not getting a session, and hang up.
///
/// Not `channel_failure`. A failed channel is the protocol's way of saying the
/// request itself was not understood, and a client that gets one discards
/// whatever was written alongside it -- so the visitor sees `shell request
/// failed on channel 0` and nothing about why. Checked, rather than assumed:
/// that is exactly what the first version of this did.
///
/// So the request *succeeds*, the reason is written to the channel like any
/// other output, and then the channel closes. The visitor reads a sentence and
/// their client exits cleanly.
async fn turn_away(handle: &russh::server::Handle, channel: ChannelId, why: &'static [u8]) {
    let _ = handle.data(channel, bytes::Bytes::from_static(why)).await;
    let _ = handle.eof(channel).await;
    let _ = handle.close(channel).await;
}

/// What the SSH side hands the running `Shell`.
enum Wire {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    Hangup,
}

impl Handler for SessionHandler {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, user: &str) -> Result<Auth, Self::Error> {
        self.who.user = user.to_string();
        if !self.admit_identity() {
            return Ok(Auth::Reject {
                proceed_with_methods: Some([russh::MethodKind::PublicKey].as_slice().into()),
                partial_success: false,
            });
        }
        Ok(Auth::Accept)
    }

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
        if !self.admit_identity() {
            return Ok(Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            });
        }
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel.is_some() {
            reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        }
        self.channel = Some(channel.id());
        // The read half goes on the floor. Incoming bytes reach `data` below
        // either way -- russh delivers them to the handler and to the channel
        // both -- and a read half that is held but never read is worse than no
        // read half at all: its queue is bounded, and once full it blocks the
        // session's entire read loop. Dropped, russh's send to it simply fails
        // and is ignored, which is what already happened before this kept the
        // write half.
        let (_read, write) = channel.split();
        self.write = Some(write);
        reply.accept().await;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        _channel: Channel<Msg>,
        _host_to_connect: &str,
        _port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
        Ok(())
    }

    async fn channel_open_x11(
        &mut self,
        _channel: Channel<Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
        Ok(())
    }

    async fn channel_open_forwarded_tcpip(
        &mut self,
        _channel: Channel<Msg>,
        _host_to_connect: &str,
        _port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
        Ok(())
    }

    async fn channel_open_direct_streamlocal(
        &mut self,
        _channel: Channel<Msg>,
        _socket_path: &str,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
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
        if self.channel != Some(channel) || self.started {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let size = (col_width.min(u16::MAX as u32) as u16, row_height.min(u16::MAX as u32) as u16);
        self.pty = Some(size);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Wire::Resize(size.0, size.1));
        }
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel != Some(channel) || self.started {
            session.channel_failure(channel)?;
            return Ok(());
        }
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
            session.channel_success(channel)?;
            turn_away(
                &session.handle(),
                channel,
                b"busy right now -- try again shortly.\r\n",
            )
            .await;
            return Ok(());
        }
        // The box has room. Now: is this visitor already here? The seat exists
        // from here on and releases both counts however this session ends.
        let key = self.ip.and_then(crowd_key);
        // Holding the box's slot only, until the address's is actually taken.
        //
        // Built the other way round first -- key in hand before the take -- and
        // a refused connection then dropped a seat carrying somebody else's
        // key, which handed *their* slot back. The limit held for exactly one
        // attempt and then let everyone in. A guard must never carry a claim it
        // has not made.
        let mut seat = Seat {
            total: Arc::clone(&self.sessions),
            crowd: Arc::clone(&self.crowd),
            key: None,
        };
        if let Some(key) = &key {
            if !self.crowd.take(key, PER_ADDRESS) {
                // Said plainly, because the visitor can fix it and a silent
                // drop looks like the box being broken. Logged with the
                // address, because a great many of these from one place is the
                // symptom of everything arriving through one gateway rather
                // than of anybody misbehaving.
                crate::visits::operational("warn", "ssh_session_refused", key);
                session.channel_success(channel)?;
                turn_away(
                    &session.handle(),
                    channel,
                    b"already connected from this address.\r\none session at a time -- close the other one and try again.\r\n",
                )
                .await;
                // Returns the box's slot and nothing else, because that is all
                // it ever held.
                drop(seat);
                return Ok(());
            }
            seat.key = Some(key.clone());
        }

        let (tx, rx) = unbounded_channel();
        self.tx = Some(tx);
        self.started = true;
        // Taken, not borrowed: the writing half goes to the session's thread
        // and this handler keeps only the id, which is all it needs afterwards.
        let Some(write) = self.write.take() else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        session.channel_success(channel)?;

        let handle = session.handle();
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
            // Moved in, and dropped when this thread's work is over however
            // that happens -- including a panic unwinding through here, which
            // the two decrements this replaced would have skipped.
            let _seat = seat;
            rt.block_on(async move {
                if let Err(e) = run_session(write, cols, rows, rx, who).await {
                    crate::visits::operational(
                        "warn",
                        "ssh_session_error",
                        &format!("{peer}: {e:#}"),
                    );
                }
                let _ = handle.close(channel).await;
            });
        });
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel != Some(channel) || self.started {
            session.channel_failure(channel)?;
            return Ok(());
        }
        let command = std::str::from_utf8(data).unwrap_or_default().trim();
        if !matches!(command, "portfolio" | "help" | "") {
            session.channel_failure(channel)?;
            return Ok(());
        }
        self.started = true;
        session.channel_success(channel)?;
        let handle = session.handle();
        let _ = handle
            .data(
                channel,
                bytes::Bytes::from_static(
                    b"Prince Patel's portfolio. This endpoint executes no system commands.\nConnect with `ssh -t <host>` for the interactive version.\n",
                ),
            )
            .await;
        let _ = handle.exit_status_request(channel, 0).await;
        let _ = handle.eof(channel).await;
        let _ = handle.close(channel).await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        _variable_name: &str,
        _variable_value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn x11_request(
        &mut self,
        channel: ChannelId,
        _single_connection: bool,
        _x11_auth_protocol: &str,
        _x11_auth_cookie: &str,
        _x11_screen_number: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn agent_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        session.channel_failure(channel)?;
        Ok(false)
    }

    async fn tcpip_forward(
        &mut self,
        _address: &str,
        _port: &mut u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    async fn cancel_tcpip_forward(
        &mut self,
        _address: &str,
        _port: u32,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    async fn streamlocal_forward(
        &mut self,
        _socket_path: &str,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    async fn cancel_streamlocal_forward(
        &mut self,
        _socket_path: &str,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        Ok(false)
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel != Some(channel) || !self.started {
            return Ok(());
        }
        let size = (col_width.min(u16::MAX as u32) as u16, row_height.min(u16::MAX as u32) as u16);
        self.pty = Some(size);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Wire::Resize(size.0, size.1));
        }
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) && self.started {
            let Some(tx) = &self.tx else { return Ok(()) };
            let _ = tx.send(Wire::Bytes(data.to_vec()));
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            let Some(tx) = &self.tx else { return Ok(()) };
            let _ = tx.send(Wire::Hangup);
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channel == Some(channel) {
            let Some(tx) = &self.tx else { return Ok(()) };
            let _ = tx.send(Wire::Hangup);
        }
        Ok(())
    }
}

/// Bridge one SSH channel to a session. The loop itself lives in `session`,
/// shared with the web transport -- this only moves bytes.
async fn run_session(
    write: ChannelWriteHalf<Msg>,
    cols: u16,
    rows: u16,
    mut wire: UnboundedReceiver<Wire>,
    who: crate::visits::Who,
) -> anyhow::Result<()> {
    // One frame deep, on purpose. See `session::FrameSink`: this is the queue
    // that must not exist, so it is made small enough that the loop upstream
    // notices it is full and stops drawing rather than filling it.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            // Waits for window rather than for a queue to accept it, so a frame
            // is only finished here once the far end has room for it. That wait
            // is the whole signal: it holds the one slot in `out_rx`, and
            // `session::run` reads a full slot as "don't draw".
            if write.data_bytes(bytes::Bytes::from(frame)).await.is_err() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn handler() -> SessionHandler {
        SessionHandler {
            admitted: true,
            rate_checked: true,
            _connection: Connection {
                total: Arc::new(AtomicUsize::new(1)),
                crowd: Arc::new(Crowd::default()),
                key: None,
            },
            sessions: Arc::new(AtomicUsize::new(0)),
            crowd: Arc::new(Crowd::default()),
            ip: Some("127.0.0.1".parse().unwrap()),
            peer: "127.0.0.1:22".into(),
            channel: None,
            write: None,
            pty: None,
            started: false,
            tx: None,
            who: crate::visits::Who::default(),
        }
    }

    #[test]
    fn anonymous_authentication_is_accepted_without_a_password_prompt() {
        let mut handler = handler();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert_eq!(runtime.block_on(handler.auth_none("visitor")).unwrap(), Auth::Accept);
        assert_eq!(handler.who.user, "visitor");
    }

    #[test]
    fn an_unknown_public_key_is_accepted() {
        let mut handler = handler();
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        assert_eq!(
            runtime.block_on(handler.auth_publickey("visitor", key.public_key())).unwrap(),
            Auth::Accept
        );
        assert!(!handler.who.id.is_empty());
    }

    #[test]
    fn a_generated_host_key_is_reused() {
        let path = std::env::temp_dir().join(format!("portfolio-host-key-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let first = load_or_create_host_key(&path).unwrap();
        let second = load_or_create_host_key(&path).unwrap();
        assert_eq!(first.fingerprint(HashAlg::Sha256), second.fingerprint(HashAlg::Sha256));
        let _ = std::fs::remove_file(path);
    }

    fn seat(total: &Arc<AtomicUsize>, crowd: &Arc<Crowd>, ip: &str) -> Option<Seat> {
        // The same order as `shell_request`, deliberately: a helper that takes
        // its seat differently from the code under test is a helper that tests
        // itself.
        let key = crowd_key(ip.parse().unwrap());
        total.fetch_add(1, Ordering::SeqCst);
        let mut seat =
            Seat { total: Arc::clone(total), crowd: Arc::clone(crowd), key: None };
        if let Some(key) = key {
            if !crowd.take(&key, PER_ADDRESS) {
                return None;
            }
            seat.key = Some(key);
        }
        Some(seat)
    }

    /// One at a time from an address, and the next one in is refused.
    #[test]
    fn a_second_session_from_one_address_is_refused() {
        let (total, crowd) = (Arc::new(AtomicUsize::new(0)), Arc::new(Crowd::default()));
        let first = seat(&total, &crowd, "203.0.113.7");
        assert!(first.is_some(), "the first visitor was turned away");
        assert!(seat(&total, &crowd, "203.0.113.7").is_none(), "two at once from one address");
        // And again, which is the assertion that matters. A refusal must not
        // give anything back: the first version built the guard with the key in
        // it *before* taking the slot, so a refused attempt dropped a seat
        // carrying somebody else's claim and handed their slot over. The limit
        // held for exactly one attempt and then let everyone in -- and the
        // check above passes the whole time, because the refusal it asserts is
        // the one doing the damage.
        assert!(seat(&total, &crowd, "203.0.113.7").is_none(), "a refusal freed the first seat");
        assert_eq!(
            crowd.held.lock().unwrap().get("203.0.113.7").copied(),
            Some(1),
            "the count moved on a refusal"
        );
        // Somebody else is not affected by it.
        let _other = seat(&total, &crowd, "198.51.100.4").expect("one address blocked another");
        drop(first);
    }

    /// The seat comes back when the session ends -- which is the whole reason
    /// it is a guard. A slot that leaks locks that visitor out for the life of
    /// the process, and nothing on screen would ever say why.
    #[test]
    fn the_seat_comes_back_when_the_session_ends() {
        let (total, crowd) = (Arc::new(AtomicUsize::new(0)), Arc::new(Crowd::default()));
        {
            let _held = seat(&total, &crowd, "203.0.113.7").expect("refused the first");
            assert_eq!(total.load(Ordering::SeqCst), 1);
            assert!(seat(&total, &crowd, "203.0.113.7").is_none());
            // The refused attempt returned the box slot it took to get this far.
            assert_eq!(total.load(Ordering::SeqCst), 1, "a refusal kept a slot");
        }
        assert_eq!(total.load(Ordering::SeqCst), 0, "the box's slot leaked");
        assert!(crowd.held.lock().unwrap().is_empty(), "the address slot leaked");
        assert!(seat(&total, &crowd, "203.0.113.7").is_some(), "they could not come back");
    }

    /// A panic in a session must not lock its visitor out for good.
    #[test]
    fn a_session_that_panics_still_gives_its_seat_back() {
        let (total, crowd) = (Arc::new(AtomicUsize::new(0)), Arc::new(Crowd::default()));
        let (t, c) = (Arc::clone(&total), Arc::clone(&crowd));
        let died = std::thread::spawn(move || {
            let _held = seat(&t, &c, "203.0.113.9").expect("refused");
            panic!("the session fell over");
        })
        .join();
        assert!(died.is_err(), "this test needs the thread to actually panic");
        assert_eq!(total.load(Ordering::SeqCst), 0);
        assert!(seat(&total, &crowd, "203.0.113.9").is_some(), "locked out by a panic");
    }

    /// v6 is counted by its /64, because that is what a visitor is given. A
    /// limit anybody can step around by picking the next number in their own
    /// subnet inconveniences only the people not trying to.
    #[test]
    fn a_v6_visitor_cannot_walk_around_it_inside_their_own_subnet() {
        let (total, crowd) = (Arc::new(AtomicUsize::new(0)), Arc::new(Crowd::default()));
        // Bound, not asserted-and-dropped: a seat inside an `assert!` is
        // released before the next line runs, and the test then passes for the
        // wrong reason -- which is what the first version of this did.
        let _first = seat(&total, &crowd, "2001:db8:1:2::1").expect("refused the first");
        assert!(
            seat(&total, &crowd, "2001:db8:1:2:ffff:ffff:ffff:ffff").is_none(),
            "the same /64 got a second session"
        );
        // A different /64 is a different visitor.
        let _elsewhere = seat(&total, &crowd, "2001:db8:1:3::1").expect("a /64 blocked another");
    }

    /// Loopback is exempt: the operator, the health check and the smoke test
    /// all arrive from there, and a rule that locks you out of your own box
    /// mid-deploy is one that gets switched off at the worst moment.
    #[test]
    fn the_box_itself_is_never_locked_out() {
        assert_eq!(crowd_key("127.0.0.1".parse().unwrap()), None);
        assert_eq!(crowd_key("::1".parse().unwrap()), None);
        let (total, crowd) = (Arc::new(AtomicUsize::new(0)), Arc::new(Crowd::default()));
        let _a = seat(&total, &crowd, "127.0.0.1").expect("refused itself");
        let _b = seat(&total, &crowd, "127.0.0.1").expect("refused itself twice");
        assert!(crowd.held.lock().unwrap().is_empty(), "loopback took a slot");
    }

    /// The key is the address and nothing else -- no port, no brackets. A
    /// per-connection port in the key is a limit that never triggers.
    #[test]
    fn the_key_is_the_address_without_its_port() {
        let of = |s: &str| crowd_key(s.parse().unwrap()).unwrap();
        assert_eq!(of("203.0.113.7"), "203.0.113.7");
        assert_eq!(of("2001:db8:1:2::1"), "2001:db8:1:2::/64");
        assert!(!of("2001:db8:1:2::1").contains('['), "a bracket reached the key");
        // Two connections from one address are one key however they are
        // spelled, which is what makes the count mean anything.
        assert_eq!(of("2001:0db8:0001:0002::5"), of("2001:db8:1:2::9"));
    }
}
