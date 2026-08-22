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

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

/// Who is connected, by address.
///
/// Separate from the `MAX_SESSIONS` counter because they answer different
/// questions: that one is "is this box full", this one is "is this visitor
/// already here". A seat holds both, so neither can be released without the
/// other.
#[derive(Default)]
struct Crowd {
    held: Mutex<HashMap<String, usize>>,
}

impl Crowd {
    /// Take a seat for this address, or say no.
    fn take(&self, key: &str) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let n = held.entry(key.to_string()).or_insert(0);
        if *n >= PER_ADDRESS {
            return false;
        }
        *n += 1;
        true
    }

    fn give_back(&self, key: &str) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = held.get_mut(key) {
            *n = n.saturating_sub(1);
            // Removed at zero rather than left behind. The map is keyed by
            // something a stranger chooses, so entries that are never cleaned
            // up are a slow leak somebody else decides the size of.
            if *n == 0 {
                held.remove(key);
            }
        }
    }
}

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

impl Drop for Seat {
    fn drop(&mut self) {
        self.total.fetch_sub(1, Ordering::SeqCst);
        if let Some(key) = &self.key {
            self.crowd.give_back(key);
        }
    }
}

/// How an address is counted.
///
/// A v4 address is the address. A v6 address is its **/64**, because a visitor
/// is not given one v6 address, they are given a whole /64 -- counting single
/// addresses there would mean a limit anybody can step around by picking the
/// next number in their own subnet, which is a rule that inconveniences only
/// the people not trying to get around it.
///
/// Loopback is exempt, and returns `None`: it is the operator, the health
/// check and the smoke test, and a rule that locks you out of your own box
/// halfway through a deploy is a rule that gets switched off at the worst
/// possible moment.
fn crowd_key(ip: IpAddr) -> Option<String> {
    if ip.is_loopback() {
        return None;
    }
    Some(match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
    })
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
    let mut server =
        Listener { sessions: Arc::new(AtomicUsize::new(0)), crowd: Arc::new(Crowd::default()) };
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
    crowd: Arc<Crowd>,
}

impl russh::server::Server for Listener {
    type Handler = SessionHandler;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Self::Handler {
        let ip = peer.map(|p| p.ip());
        let peer = peer.map(|p| p.to_string()).unwrap_or_default();
        SessionHandler {
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
            pty: None,
            tx: None,
        }
    }
}

struct SessionHandler {
    sessions: Arc<AtomicUsize>,
    crowd: Arc<Crowd>,
    /// Where this connection came from, if the transport said. `None` is
    /// treated as unlimited rather than refused -- see `shell_request`.
    ip: Option<IpAddr>,
    peer: String,
    pty: Option<(u16, u16)>,
    tx: Option<UnboundedSender<Wire>>,
    /// Who is at the other end, as far as the handshake said. Both halves are
    /// offered by the client rather than taken from it: the username is what
    /// they typed in front of the `@`, and the key is the one they chose to
    /// authenticate with.
    who: crate::visits::Who,
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
            if !self.crowd.take(key) {
                // Said plainly, because the visitor can fix it and a silent
                // drop looks like the box being broken. Logged with the
                // address, because a great many of these from one place is the
                // symptom of everything arriving through one gateway rather
                // than of anybody misbehaving.
                eprintln!("portfolio: refused a second session from {key}");
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
                if let Err(e) = run_session(handle.clone(), channel, cols, rows, rx, who).await {
                    eprintln!("portfolio: session from {peer} ended: {e:#}");
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(total: &Arc<AtomicUsize>, crowd: &Arc<Crowd>, ip: &str) -> Option<Seat> {
        // The same order as `shell_request`, deliberately: a helper that takes
        // its seat differently from the code under test is a helper that tests
        // itself.
        let key = crowd_key(ip.parse().unwrap());
        total.fetch_add(1, Ordering::SeqCst);
        let mut seat =
            Seat { total: Arc::clone(total), crowd: Arc::clone(crowd), key: None };
        if let Some(key) = key {
            if !crowd.take(&key) {
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
