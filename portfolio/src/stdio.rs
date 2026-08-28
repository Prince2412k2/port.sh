//! The session inside a pty that something else allocated.
//!
//! This is what mosh runs. `mosh-server` forks a pty and execs a command in
//! it; that command is this binary with `--stdio`, and everything below the
//! byte plumbing here is `session::run` -- the same loop ssh and the browser
//! drive.
//!
//! Written this way rather than letting mosh run the ordinary terminal build
//! because that build is a different program in every way that matters here:
//! it opens no visit, honours no idle limit, counts nobody, and draws whenever
//! it likes. A visitor arriving over mosh would have been invisible in the
//! logs and unbounded in the room. Going through `session::run` means the
//! third transport gets all of that for the price of the plumbing, and there
//! is still only one render loop to keep correct.
//!
//! The pty is real here, unlike on the other two transports -- mosh made it.
//! That changes nothing below: `session::run` writes every escape sequence by
//! hand and asks the terminal for nothing, so it neither knows nor cares.

use std::io::{Read as _, Write as _};

use crate::session::{self, In};

/// Who is at the far end, as the ssh bootstrap recorded them.
///
/// Through the environment rather than argv, for two reasons. mosh-server
/// hands its own environment to the child, so it costs nothing; and a key
/// fingerprint passed on a command line is a key fingerprint in `ps` for every
/// process on the box.
fn who() -> crate::visits::Who {
    let said = |key: &str| std::env::var(key).unwrap_or_default();
    crate::visits::Who {
        via: "mosh",
        user: said("PORTFOLIO_VIA_USER"),
        id: said("PORTFOLIO_VIA_ID"),
        ip: said("PORTFOLIO_VIA_IP"),
        client: said("PORTFOLIO_VIA_CLIENT"),
    }
}

/// Run one session over this process's own stdin and stdout.
pub fn run() -> anyhow::Result<()> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((100, 30));
    crossterm::terminal::enable_raw_mode()?;
    let outcome = pump(cols, rows);
    // Best effort and unconditional: the pty outlives this process by however
    // long mosh keeps the session open, and handing it back in raw mode would
    // be handing back a terminal that eats its own newlines.
    let _ = crossterm::terminal::disable_raw_mode();
    outcome
}

fn pump(cols: u16, rows: u16) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async move {
        // One frame deep, the same as the other two transports. See
        // `session::FrameSink`: a pty write blocks once the far end is behind,
        // and that is exactly the signal the loop reads as "do not draw".
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<In>();

        // stdin on a thread of its own, because the read blocks and the loop
        // this feeds must not.
        let keys = in_tx.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if keys.send(In::Bytes(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = keys.send(In::Hangup);
        });

        // A resize arrives as a real SIGWINCH here, which is the one thing
        // this transport gets for free that the other two have to be told.
        tokio::spawn(async move {
            let Ok(mut winch) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
            else {
                return;
            };
            while winch.recv().await.is_some() {
                let Ok((cols, rows)) = crossterm::terminal::size() else {
                    continue;
                };
                if in_tx.send(In::Resize(cols, rows)).is_err() {
                    return;
                }
            }
        });

        let writer = tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                // On the blocking pool. A pty write parks when the reader is
                // behind, and parked on the runtime's only thread it would
                // stop the timers the render loop is counting on.
                let wrote = tokio::task::spawn_blocking(move || {
                    let mut out = std::io::stdout().lock();
                    out.write_all(&frame).and_then(|()| out.flush())
                })
                .await;
                if !matches!(wrote, Ok(Ok(()))) {
                    break;
                }
            }
        });

        let outcome = session::run(out_tx, in_rx, cols, rows, who(), session::Profile::Ssh).await;
        // The last frame a session writes is the one that puts the terminal
        // back. Waiting for the writer is what gets it out, exactly as on ssh.
        let _ = writer.await;
        outcome
    })
}
