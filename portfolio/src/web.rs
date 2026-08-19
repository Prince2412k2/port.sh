//! The portfolio as a web page, without giving anyone a shell.
//!
//! A browser opens a WebSocket, gets its own `Shell`, and renders the ANSI
//! frames with xterm.js. It is the same session code SSH drives -- see
//! `session::run` -- so the two transports cannot drift apart, and neither one
//! involves a pty, a subprocess, or a login.
//!
//! Nothing here executes anything on the visitor's behalf. There is no shell
//! to escape from because there is no shell: the only thing on the far end of
//! that socket is a `Shell` struct drawing frames, and the only bytes it
//! accepts are the ones `wire::Decoder` recognises as keys and mouse events.
//! A visitor cannot run a command here for the same reason they cannot run one
//! over SSH -- the code path does not exist.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::Router;
use tokio::sync::mpsc::unbounded_channel;

use crate::session;

/// Concurrent browser sessions. Same reasoning as the SSH ceiling: opening
/// sockets should not be a free way to grow the process without bound.
const MAX_SESSIONS: usize = 128;

#[derive(Clone)]
struct Web {
    sessions: Arc<AtomicUsize>,
}

pub async fn serve(addr: &str, port: u16) -> anyhow::Result<()> {
    let state = Web { sessions: Arc::new(AtomicUsize::new(0)) };
    let app = Router::new()
        .route("/", get(page))
        .route("/ws", get(upgrade))
        .with_state(state);

    let bind: SocketAddr = format!("{addr}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("portfolio: web terminal on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn page() -> impl IntoResponse {
    Html(INDEX)
}

async fn upgrade(ws: WebSocketUpgrade, State(state): State<Web>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if state.sessions.fetch_add(1, Ordering::SeqCst) >= MAX_SESSIONS {
            state.sessions.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        drive(socket).await;
        state.sessions.fetch_sub(1, Ordering::SeqCst);
    })
}

/// Bridge one WebSocket to one session.
///
/// The client sends two kinds of message: binary frames are raw terminal input
/// (exactly what xterm.js's `onData` produces, the same bytes an SSH channel
/// would carry), and a short text message starting with `r` carries a resize.
/// Keeping resize out of band avoids inventing an escape sequence for it and
/// means the input path stays byte-identical to the SSH one.
async fn drive(socket: WebSocket) {
    use futures_util::{SinkExt, StreamExt};

    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = unbounded_channel::<Vec<u8>>();
    let (in_tx, in_rx) = unbounded_channel::<session::In>();

    // Frames out. Binary, not text: these are ANSI bytes and some of them are
    // not valid UTF-8 on their own when a frame splits a multi-byte glyph.
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if sink.send(Message::Binary(frame.into())).await.is_err() {
                break;
            }
        }
    });

    // The session needs a size before it can lay anything out, and the browser
    // only knows it after xterm.js has measured the font. So the first message
    // is always a resize, and the session does not start until it arrives.
    let first = stream.next().await;
    let (cols, rows) = match first {
        Some(Ok(Message::Text(t))) => parse_resize(&t).unwrap_or((100, 30)),
        _ => (100, 30),
    };

    let reader_tx = in_tx.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let sent = match msg {
                Message::Binary(b) => reader_tx.send(session::In::Bytes(b.to_vec())),
                // Text is only ever a resize. Anything else is ignored rather
                // than fed to the decoder: the input path takes bytes from one
                // place only, and that is the binary channel.
                Message::Text(t) => match parse_resize(&t) {
                    Some((c, r)) => reader_tx.send(session::In::Resize(c, r)),
                    None => Ok(()),
                },
                Message::Close(_) => break,
                _ => Ok(()),
            };
            if sent.is_err() {
                break;
            }
        }
        let _ = reader_tx.send(session::In::Hangup);
    });

    // Its own OS thread and its own single-threaded runtime, for the same
    // reason the SSH path does it: `Shell` holds termap's tile cache, which
    // shares tiles with `Rc` rather than `Arc` and so is not `Send`. Axum's
    // executor requires `Send` of anything spawned on it. Handing each session
    // a thread sidesteps that without forcing an `Arc` on a crate that has no
    // use for one, and keeps one visitor's tile decoding from stalling others.
    let done = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
        rt.block_on(async move { session::run(out_tx, in_rx, cols, rows).await.ok() })
    });
    let _ = done.await;
    reader.abort();
    writer.abort();
}

/// `r<cols>x<rows>`, e.g. `r120x40`.
fn parse_resize(s: &str) -> Option<(u16, u16)> {
    let body = s.strip_prefix('r')?;
    let (c, r) = body.split_once('x')?;
    Some((c.trim().parse().ok()?, r.trim().parse().ok()?))
}

/// The whole client. One file, no build step, no npm.
///
/// xterm.js comes from a CDN rather than being vendored, which is the one
/// outside dependency on this page -- swap it for a local copy if that matters.
/// The shader hook is deliberate groundwork: xterm.js's WebGL addon renders
/// into a canvas, and a post-processing pass over that canvas is where CRT
/// curvature, bloom or scanlines would go later.
const INDEX: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Prince Patel</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/xterm@5.3.0/css/xterm.css">
<style>
  html, body {
    margin: 0; padding: 0; height: 100%;
    background: #08090b; overflow: hidden;
  }
  #term { position: absolute; inset: 0; }
  #hint {
    position: absolute; left: 50%; bottom: 1.2rem; transform: translateX(-50%);
    color: #3a3e46; font: 12px ui-monospace, monospace; pointer-events: none;
    transition: opacity .6s ease; z-index: 2;
  }
  #hint.gone { opacity: 0; }
</style>
</head>
<body>
<div id="term"></div>
<div id="hint">click to focus &middot; this is the same program you get over ssh</div>
<script src="https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-fit@0.8.0/lib/xterm-addon-fit.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-webgl@0.16.0/lib/xterm-addon-webgl.js"></script>
<script>
const term = new Terminal({
  allowProposedApi: true,
  cursorBlink: false,
  // The app hides the cursor and draws everything itself; a blinking block
  // parked wherever the last write landed only ever looks like a bug.
  fontFamily: '"DejaVu Sans Mono", "Menlo", ui-monospace, monospace',
  fontSize: 14,
  theme: { background: '#08090b', foreground: '#c4c8ce' },
  // Braille and half-block glyphs are the whole renderer here, and letting
  // xterm draw them from the font rather than its own box-drawing shortcuts
  // is what keeps the map looking like the map.
  customGlyphs: false,
  scrollback: 0,
});
const fit = new FitAddon.FitAddon();
term.open(document.getElementById('term'));
term.loadAddon(fit);
try { term.loadAddon(new WebglAddon.WebglAddon()); } catch (e) { /* canvas fallback */ }
fit.fit();

const proto = location.protocol === 'https:' ? 'wss' : 'ws';
const ws = new WebSocket(`${proto}://${location.host}/ws`);
ws.binaryType = 'arraybuffer';

const sendSize = () => {
  if (ws.readyState === WebSocket.OPEN) ws.send(`r${term.cols}x${term.rows}`);
};

ws.onopen = () => {
  sendSize();                       // must be first: the session waits for it
  term.focus();
};
ws.onmessage = (e) => term.write(new Uint8Array(e.data));
ws.onclose = () => {
  term.write('\r\n\x1b[38;2;120;126;136m  disconnected. reload to start again.\x1b[0m\r\n');
};

// Everything the user types goes straight out as bytes, exactly as an SSH
// channel would carry it. The server decodes it with the same parser.
term.onData((d) => {
  if (ws.readyState !== WebSocket.OPEN) return;
  ws.send(new TextEncoder().encode(d));
});
term.onBinary((d) => {
  if (ws.readyState !== WebSocket.OPEN) return;
  const buf = new Uint8Array(d.length);
  for (let i = 0; i < d.length; i++) buf[i] = d.charCodeAt(i) & 255;
  ws.send(buf);
});

let resizeTimer;
addEventListener('resize', () => {
  clearTimeout(resizeTimer);
  // Debounced: dragging a window edge fires this continuously, and each one
  // costs a full redraw of a full-screen TUI.
  resizeTimer = setTimeout(() => { fit.fit(); sendSize(); }, 120);
});

const hint = document.getElementById('hint');
const dismiss = () => hint.classList.add('gone');
addEventListener('keydown', dismiss, { once: true });
addEventListener('mousedown', dismiss, { once: true });
setTimeout(dismiss, 8000);
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_messages_parse_and_junk_does_not() {
        assert_eq!(parse_resize("r120x40"), Some((120, 40)));
        assert_eq!(parse_resize("r80x24"), Some((80, 24)));
        assert_eq!(parse_resize("120x40"), None);
        assert_eq!(parse_resize("rx"), None);
        assert_eq!(parse_resize("r120"), None);
        assert_eq!(parse_resize(""), None);
        // Not a resize, and must not be mistaken for one.
        assert_eq!(parse_resize("rm -rf /"), None);
    }
}
