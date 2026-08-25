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
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
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
    crate::visits::operational("info", "web_listen", &bind.to_string());
    // `into_make_service_with_connect_info` rather than `into_make_service`:
    // without it there is no peer address to record, and behind a proxy there
    // is none worth recording either -- see `client_ip`.
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(crate::net::shutdown_signal())
        .await?;
    Ok(())
}

async fn page() -> impl IntoResponse {
    Html(INDEX)
}

/// The visitor's address, preferring what a reverse proxy says over the socket.
///
/// Deployed behind nginx or Traefik the socket is the proxy, so every visitor
/// would look like they came from the same machine. `X-Forwarded-For` is the
/// first hop and is what the proxy was asked to pass along; it is trusted here
/// because the only thing in front of this is one we put there.
fn client_ip(headers: &HeaderMap, socket: SocketAddr) -> String {
    let trusted_proxy = match socket.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    };
    if trusted_proxy {
        for h in ["x-forwarded-for", "x-real-ip"] {
            if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
                if let Some(first) = v.split(',').next() {
                    let first = first.trim();
                    if !first.is_empty() {
                        return first.to_string();
                    }
                }
            }
        }
    }
    socket.ip().to_string()
}

async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Web>,
    ConnectInfo(socket): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let who = crate::visits::Who {
        via: "web",
        // A browser has no username to offer and no key to be known by. The
        // client sends an id it keeps in localStorage; until that arrives this
        // visitor is simply new, which is the honest default.
        user: String::new(),
        id: String::new(),
        ip: client_ip(&headers, socket),
        client: headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string(),
    };
    ws.on_upgrade(move |socket| async move {
        if state.sessions.fetch_add(1, Ordering::SeqCst) >= MAX_SESSIONS {
            state.sessions.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        drive(socket, who).await;
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
async fn drive(socket: WebSocket, who: crate::visits::Who) {
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
    // only knows it after xterm.js has measured the font. So the opening
    // messages are text: an optional `i<id>` naming the visitor, then the size,
    // and the session does not start until the size arrives.
    let mut who = who;
    let mut reduced_motion = false;
    let (cols, rows) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => {
                    if let Some(id) = t.strip_prefix('i') {
                        who.id = sanitise_id(id);
                        continue;
                    }
                    if t == "m1" {
                        reduced_motion = true;
                        continue;
                    }
                    if let Some(size) = parse_resize(&t) {
                        break size;
                    }
                }
                Some(Ok(_)) => continue,
                _ => break (100, 30),
            }
        }
    })
    .await
    .unwrap_or((100, 30));
    let mut rate_keys = vec![format!("ip:{}", who.ip)];
    if !who.id.is_empty() {
        rate_keys.push(format!("web-id:{}", who.id));
    }
    if !crate::budget::admit_visit(&rate_keys) {
        return;
    }
    if reduced_motion {
        let _ = in_tx.send(session::In::ReducedMotion(true));
    }

    let reader_tx = in_tx.clone();
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            let sent = match msg {
                Message::Binary(b) => reader_tx.send(session::In::Bytes(b.to_vec())),
                // Text is only ever a resize. Anything else is ignored rather
                // than fed to the decoder: the input path takes bytes from one
                // place only, and that is the binary channel.
                Message::Text(t) => match parse_text(&t) {
                    Some(message) => reader_tx.send(message),
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
        rt.block_on(async move {
            // Said out loud. A session that dies of an error used to end exactly
            // like one that was closed politely -- the socket shut and nothing
            // written anywhere -- which is how a resize that killed every
            // session went unnoticed.
            match session::run(out_tx, in_rx, cols, rows, who).await {
                Ok(()) => Some(()),
                Err(e) => {
                    crate::visits::operational("warn", "web_session_error", &format!("{e:#}"));
                    None
                }
            }
        })
    });
    let _ = done.await;
    reader.abort();

    // Not aborted with the reader. The session's last act is to queue the frame
    // that turns mouse reporting off and shows the cursor again; killing the
    // writer here would throw it away, and the visitor's terminal would keep
    // reporting scroll events to whatever they went back to. The sender is
    // dropped as the session returns, so the writer drains what is left and
    // ends on its own -- with a bound, because a browser that has stopped
    // reading must not hold the task open.
    let drained = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
    if drained.is_err() {
        crate::visits::operational("warn", "web_close_frame_dropped", "client disconnected");
    }
}

/// A browser-supplied visitor id, reduced to something safe to key a log on.
///
/// This arrives from the page, so it is a stranger's string: bounded, and cut
/// down to an alphabet that cannot be confused for anything else in the record
/// it lands in. It identifies a browser that chose to keep it, and clearing it
/// makes somebody new -- which is the right amount of control to leave with the
/// person it describes.
fn sanitise_id(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect()
}

/// `r<cols>x<rows>`, e.g. `r120x40`.
fn parse_resize(s: &str) -> Option<(u16, u16)> {
    let body = s.strip_prefix('r')?;
    let (c, r) = body.split_once('x')?;
    Some((c.trim().parse().ok()?, r.trim().parse().ok()?))
}

/// The whole text channel: a size, or the width of the client's own chrome.
///
/// Bytes typed at the terminal come in on the binary channel and nowhere else,
/// so anything arriving here is a statement about the window rather than input.
/// Anything that is neither of these two is dropped rather than guessed at.
fn parse_text(s: &str) -> Option<session::In> {
    if let Some((c, r)) = parse_resize(s) {
        return Some(session::In::Resize(c, r));
    }
    let cols = s.strip_prefix('g')?.trim().parse().ok()?;
    Some(session::In::Gutter(cols))
}

/// The whole client. One file, no build step, no npm.
///
/// xterm.js comes from a CDN rather than being vendored, which is the one
/// outside dependency on this page -- swap it for a local copy if that matters.
///
/// The CRT switch in the corner is a real post-processing chain, not a stack
/// of CSS overlays: the terminal is rendered to a canvas, that canvas is
/// uploaded as a texture, and four programs turn it into a photograph of a
/// display -- a composite signal, a phosphor with a memory, a bloom, and then
/// the glass, the beam and the mask. The switch beside it says which display,
/// and there are seven of them: two colour tubes, a television, amber and
/// green monochrome, the same television fed off a tape, and an early panel.
///
/// Doing it in shaders is what buys the parts that are not overlays at all.
/// Bending the image means sampling it somewhere other than where the pixel
/// is; a scanline that gets fatter as it gets brighter means asking four rows
/// what they contribute here; a phosphor that lets go slowly means keeping the
/// last frame. CSS cannot express any of those.
///
/// It is also the one feature on this page that costs nothing on the wire. The
/// server sends the same bytes either way; the whole effect is the visitor's
/// own GPU rewriting frames it has already been given. That is the reverse of
/// every other trade in this project, and the reason this one gets to be
/// expensive.
///
/// Which renderer xterm uses is part of the mechanism rather than a detail.
/// Its WebGL renderer owns a drawing buffer that is not reliably readable as a
/// texture from another context -- without `preserveDrawingBuffer` the contents
/// are undefined once composited -- so turning the CRT on switches xterm to its
/// canvas renderer, whose 2D canvas is always a valid texture source, and
/// turning it off switches back.
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
  /* With the CRT on, the terminal is still the thing being rendered and still
     the thing taking clicks -- it is just not the thing you look at. It stays
     laid out and hit-testable underneath, and the shader draws what it holds. */
  #term.shaded .xterm-screen { opacity: 0; }
  #glass {
    position: absolute; inset: 0; z-index: 1; display: none;
    /* Both of these are load-bearing and `inset: 0` is not enough on its own.
       A canvas is a *replaced* element, so with `width: auto` the used width is
       its intrinsic width -- the `width` attribute -- and `right` is ignored
       rather than stretching it. Without this the glass laid out at whatever
       the attribute happened to say, which `resize()` then read back as
       `clientWidth` and multiplied by the pixel ratio to set the attribute
       again: every toggle scaled the tube by another factor of dpr, starting
       from the 300x150 a canvas defaults to. Sized here, layout is the
       viewport and the attribute is only ever the backing store. */
    width: 100%; height: 100%;
    pointer-events: none;      /* mouse still belongs to the terminal */
  }
  #glass.on { display: block; }
  #hint {
    position: absolute; left: 50%; bottom: 1.2rem; transform: translateX(-50%);
    color: #3a3e46; font: 12px ui-monospace, monospace; pointer-events: none;
    transition: opacity .6s ease; z-index: 2;
  }
  #hint.gone { opacity: 0; }
  /* Floats over the top rather than sitting above it: a bar with its own row
     would take that row off the terminal, and the app lays out to the rows it
     is given. The app is told how many columns this covers -- see `sendGutter`
     -- so it keeps the start of that row clear rather than drawing underneath.

     Top left, on the same row as the section rail, because these are chrome and
     that row is where this app keeps its chrome.

     Written as `[crt]` in the terminal's own font and palette rather than as
     buttons: they sit on a terminal, and a rounded pill with a border and a
     hover box on top of one looks like a browser that has landed on it. The
     brackets are the same idiom the rail uses two inches to the right. */
  #chrome {
    position: absolute; top: 0; left: 0; height: 100%; z-index: 3;
    display: flex; flex-direction: column;
    justify-content: center; align-items: flex-start; gap: .45em;
    /* Two columns in rather than hard against the edge. The tube bends the
       edges away from the viewer, and the leftmost thing on the screen is the
       first to go over the horizon. */
    padding: 0 1ch 0 2ch;
    font: 14px "DejaVu Sans Mono", "Menlo", ui-monospace, monospace;
    line-height: 1.2;
    /* The strip is as tall as the window so the switches can sit in the middle
       of it, which would otherwise make the whole left edge unclickable. */
    pointer-events: none;
  }
  #chrome button { pointer-events: auto; }
  #chrome button {
    background: transparent; border: 0; padding: 0; margin: 0;
    font: inherit; cursor: pointer;
    --ink: #3a3e46; color: var(--ink);
    transition: color .2s ease;
  }
  #chrome button::before { content: "["; }
  #chrome button::after { content: "]"; }
  #chrome button:hover { --ink: #c4c8ce; }
  #chrome button[aria-pressed="true"] { --ink: #ffb040; }
  #chrome button[disabled] { opacity: .35; cursor: default; }
  /* Which screen, rather than whether. It carries no pressed state because it
     is not a switch -- it is a dial with seven positions, and it is only there
     at all while there is a tube to point it at. */
  #chrome #screen { --ink: #606670; }
  #chrome #screen:hover { --ink: #c4c8ce; }
  /* With the tube on, these give up their paint and keep their clicks: the
     same three words are drawn into the picture instead, so they arrive
     through the glass with everything else. The colour still resolves here --
     that is what `--ink` is for -- it is only the ink that goes. */
  #chrome.shaded button { color: transparent; }
  @media (prefers-reduced-motion: reduce) {
    #hint, #chrome button { transition: none; }
  }
</style>
</head>
<body>
<div id="term"></div>
<canvas id="glass"></canvas>
<div id="chrome">
  <button id="screen" type="button" title="which screen" hidden>p22</button>
  <button id="crt" type="button" aria-pressed="false" title="old tube">crt</button>
  <button id="full" type="button" aria-pressed="false" title="full screen (ctrl-f)">full</button>
</div>
<div id="hint">click to focus &middot; ctrl-f for full screen &middot; this is the same program you get over ssh</div>
<script src="https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-fit@0.8.0/lib/xterm-addon-fit.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-webgl@0.16.0/lib/xterm-addon-webgl.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-canvas@0.5.0/lib/xterm-addon-canvas.js"></script>
<script>
// The one thing on this page that comes from somewhere else.
//
// If it did not arrive there is no terminal, and every line below this throws
// on the first one -- which leaves a black rectangle and no explanation, and
// the failure is not the visitor's to debug. jsdelivr is blocked outright in
// some countries and merely down in the rest, so this is a state the page
// reaches in production and not a theoretical one.
//
// The ssh line is the point: the same program is a connection away, and unlike
// this one it depends on nothing but a socket.
if (typeof Terminal === 'undefined') {
  document.getElementById('hint').remove();
  document.getElementById('chrome').remove();
  const said = document.createElement('pre');
  said.style.cssText =
    'position:absolute;inset:0;display:flex;align-items:center;' +
    'justify-content:center;margin:0;text-align:center;color:#c4c8ce;' +
    'font:14px "DejaVu Sans Mono",ui-monospace,monospace;line-height:1.6';
  said.textContent =
    'the terminal emulator this page needs did not load.\n\n' +
    'it comes from cdn.jsdelivr.net, which is either blocked here or down.\n\n' +
    'the same program, without the browser in the way:\n\n' +
    '    ssh -p 2222 ' + location.hostname;
  document.body.appendChild(said);
  throw new Error('xterm did not load');
}

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
const screen = document.getElementById('term');
const switches = document.getElementById('chrome');
const fit = new FitAddon.FitAddon();
term.open(screen);
term.loadAddon(fit);

// Which renderer is loaded is the CRT's business, so it is swappable rather
// than set once. WebGL is the fast path and the default; the shader needs the
// canvas one, for the reason in this module's doc comment. If neither addon is
// present xterm falls back to its DOM renderer on its own, which is slower but
// correct -- and which the shader cannot read, so the switch stays disabled.
let renderer = null;
const useRenderer = (kind) => {
  if (renderer) {
    try { renderer.dispose(); } catch (e) { /* already gone */ }
    renderer = null;
  }
  try {
    if (kind === 'canvas') renderer = new CanvasAddon.CanvasAddon();
    else renderer = new WebglAddon.WebglAddon();
    term.loadAddon(renderer);
  } catch (e) {
    renderer = null;
  }
};
useRenderer('webgl');
fit.fit();

const proto = location.protocol === 'https:' ? 'wss' : 'ws';
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;
const ws = new WebSocket(`${proto}://${location.host}/ws`);
ws.binaryType = 'arraybuffer';

const sendSize = () => {
  if (ws.readyState === WebSocket.OPEN) ws.send(`r${term.cols}x${term.rows}`);
};

// How many columns the switches in the corner cover, so the app can keep the
// start of the header row clear instead of drawing the name underneath them.
//
// Measured rather than declared: the label on the screen switch is whichever
// screen is selected, so this is four columns wider on `one-piece` than on
// `p22`, and it disappears entirely when the tube is off. A number written down
// here would be wrong most of the time.
//
// The cell width comes from the terminal's own measurement of itself -- its
// width in pixels over its width in columns -- because that is the same
// division the app's layout is on the other side of.
let gutter = -1;
const sendGutter = () => {
  if (ws.readyState !== WebSocket.OPEN || !term.cols) return;
  const cell = screen.clientWidth / term.cols;
  // `offsetWidth` rather than a measured rect, because the switches carry a
  // transform of their own -- see `place` -- and what the app needs is the
  // column they were laid out in, not the one the glass shows them in.
  const cols = cell > 0 ? Math.ceil(switches.offsetWidth / cell) : 0;
  if (cols === gutter) return;
  gutter = cols;
  ws.send(`g${cols}`);
};

// ---------------------------------------------------------------------------
// Where the switches are, and where they end up.
//
// They are painted into the frame and the frame is then bent, so the label a
// visitor is looking at is not where the element that answers the click is.
// Near the middle that is nothing; down the left edge, which is exactly where
// these live, it is most of a character -- a subtle offset, and the kind that
// makes a control feel broken without anybody being able to say why.
//
// So the paint and the hit-target are separated. `laidOut` is where each switch
// was laid out, which is where it is painted; `place` then moves the element
// itself to wherever that paint comes out on the glass.

const laidOut = [];

/// The shader reads the frame through `bend`: the fragment at `uv` shows the
/// picture from `bend(uv)`. So something painted at `p` is *seen* at whichever
/// uv bends to p -- the other direction, which has no closed form and does not
/// need one. `bend` is barely more than the identity, so walking toward it four
/// times lands well inside a pixel.
const unbend = (u, v) => {
  const s = tube.live();
  const flags = Object.assign({}, SCREEN_BASE.flags, s.flags || {});
  const n = Object.assign({}, SCREEN_BASE.nums, s.nums || {});

  // Undo the underscan first, because it is the last thing the shader does on
  // the way in. Every screen has some, including the flat one -- so this is
  // where a panel stops being a no-op even though nothing about it curves.
  const k = underscan(n);
  const t = [0.5 + (u - 0.5) / k, 0.5 + (v - 0.5) / k];
  if (!flags.CURVE) return t;

  const bend = (x, y) => {
    let px = x * 2 - 1;
    let py = y * 2 - 1;
    const kx = Math.abs(py) / n.CURVE_X;
    const ky = Math.abs(px) / n.CURVE_Y;
    px += px * kx * kx;
    py += py * ky * ky;
    return [px * 0.5 + 0.5, py * 0.5 + 0.5];
  };
  let x = t[0];
  let y = t[1];
  for (let i = 0; i < 4; i++) {
    const [bx, by] = bend(x, y);
    x += t[0] - bx;
    y += t[1] - by;
  }
  return [x, y];
};

const place = () => {
  const bent = tube.on && glass.clientWidth > 0 && glass.clientHeight > 0;
  for (const it of laidOut) {
    if (!bent) {
      it.el.style.transform = 'none';
      continue;
    }
    // The centre of the label, because a run of text does not bend uniformly
    // and these are four characters wide. At that size the difference across
    // one is under a tenth of a pixel.
    const cx = (it.x + it.w / 2) / glass.clientWidth;
    const cy = (it.y + it.h / 2) / glass.clientHeight;
    const [ux, uy] = unbend(cx, cy);
    const dx = (ux - cx) * glass.clientWidth;
    const dy = (uy - cy) * glass.clientHeight;
    it.el.style.transform = `translate(${dx.toFixed(2)}px, ${dy.toFixed(2)}px)`;
  }
};

/// Read the layout back, with the transforms off so it is the layout that is
/// read and not the last answer this gave.
const measure = () => {
  laidOut.length = 0;
  for (const el of switches.children) {
    el.style.transform = 'none';
  }
  for (const el of switches.children) {
    if (el.hidden) continue;
    const at = el.getBoundingClientRect();
    laidOut.push({ el, x: at.left, y: at.top, w: at.width, h: at.height });
  }
  place();
  sendGutter();
};

// An id this browser keeps, so a returning visitor is recognised as one. Made
// here rather than handed out by the server: it never leaves this machine
// except to say "the same browser as last time", and clearing site data is a
// visitor deciding to be a stranger again, which is theirs to decide.
let visitorId = null;
try {
  visitorId = localStorage.getItem('visitor');
  if (!visitorId) {
    visitorId = 'w-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 10);
    localStorage.setItem('visitor', visitorId);
  }
} catch (e) { /* private mode: a stranger every time, which is fine */ }

ws.onopen = () => {
  if (visitorId) ws.send('i' + visitorId);
  if (reducedMotion) ws.send('m1');
  sendSize();                       // the session waits for this one
  measure();
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

// Modified Enter and Backspace have no reliable legacy terminal encoding.
// Send CSI-u explicitly so the browser and a modern SSH terminal produce the
// same KeyEvent instead of collapsing these chords to their plain keys.
term.attachCustomKeyEventHandler((e) => {
  if (e.type !== 'keydown') return true;

  // Keys this app owns that the browser also has a use for. xterm sends them
  // either way; what this stops is the browser *also* acting on them, which is
  // how ctrl-u ended up opening view-source over the top of a cleared input and
  // ctrl-b moved the bookmarks bar instead of the route.
  //
  // Not a list of everything the app binds -- only the overlap. Ctrl-c, ctrl-v
  // and ctrl-x stay the browser's, because a terminal somebody cannot copy out
  // of is a worse thing to be right about. And ctrl-n, ctrl-t and ctrl-w are
  // not on it because they cannot be: those never reach the page, which is why
  // the route also answers to shift-left and shift-right.
  if (e.ctrlKey && !e.altKey && !e.metaKey && 'beu'.indexOf(e.key) >= 0) {
    e.preventDefault();
    return true;
  }

  let sequence = null;
  if (e.key === 'Enter' && e.shiftKey && !e.ctrlKey && !e.altKey) {
    sequence = '\x1b[13;2u';
  } else if (e.key === 'Backspace' && e.ctrlKey && !e.altKey) {
    sequence = '\x1b[127;5u';
  } else if (e.key === 'Backspace' && e.altKey && !e.ctrlKey) {
    sequence = '\x1b[127;3u';
  }
  if (!sequence) return true;
  if (ws.readyState === WebSocket.OPEN) {
    ws.send(new TextEncoder().encode(sequence));
  }
  return false;
});

let resizeTimer;
addEventListener('resize', () => {
  clearTimeout(resizeTimer);
  // Debounced: dragging a window edge fires this continuously, and each one
  // costs a full redraw of a full-screen TUI.
  resizeTimer = setTimeout(() => {
    fit.fit();
    sendSize();
    tube.resize();
    measure();
  }, 120);
});

// ---------------------------------------------------------------------------
// Full screen.
//
// The app lays out to the rows and columns it is given, so more of them is the
// only thing that makes the map bigger. On a laptop this is the difference
// between a tour that reads and one that does not.
//
// `ctrl-f` because it is free: the app binds plain `f` (the map's depth focus)
// and `ctrl-c`, and nothing binds this. The browser does -- it is Find -- so it
// has to be taken before either the browser or xterm sees it.
const fullButton = document.getElementById('full');

const isFull = () => !!(document.fullscreenElement || document.webkitFullscreenElement);

const setFull = (on) => {
  const el = document.documentElement;
  try {
    if (on) {
      const go = el.requestFullscreen || el.webkitRequestFullscreen;
      // A promise on modern browsers, undefined on older ones; a rejection
      // means the browser declined and there is nothing to recover.
      if (go) Promise.resolve(go.call(el)).catch(() => {});
    } else {
      const stop = document.exitFullscreen || document.webkitExitFullscreen;
      if (stop) Promise.resolve(stop.call(document)).catch(() => {});
    }
  } catch (e) { /* not permitted here */ }
};

const toggleFull = () => {
  // The hint below retires itself on the first keystroke, and the listener that
  // does it never sees this one -- the capture handler stops the key before it
  // gets there. Since the hint is what advertises this key, using it should put
  // the hint away.
  const h = document.getElementById('hint');
  if (h) h.classList.add('gone');
  setFull(!isFull());
};

// Entering and leaving both change how many cells there are, so the terminal is
// remeasured and the new size sent. Not left to the `resize` event above: that
// one is debounced for window dragging, and this is a single discrete jump the
// visitor is watching for.
const refit = () => {
  fit.fit();
  sendSize();
  tube.resize();
  measure();
  fullButton.setAttribute('aria-pressed', isFull() ? 'true' : 'false');
  term.focus();
};
addEventListener('fullscreenchange', refit);
addEventListener('webkitfullscreenchange', refit);

const wantsFull = (e) =>
  e.type === 'keydown' &&
  (e.ctrlKey || e.metaKey) &&
  !e.altKey && !e.shiftKey &&
  (e.key === 'f' || e.key === 'F');

// One listener, and in the capture phase deliberately. Capture runs from the
// window down, so this sees the key before xterm's own handler on the textarea
// and can stop it there: `preventDefault` keeps the browser from opening Find,
// `stopPropagation` keeps xterm from turning it into a `\x06` and sending it
// down the socket. Handling it in both places instead -- xterm's custom key
// handler *and* a listener here -- fires twice and toggles straight back off.
addEventListener('keydown', (e) => {
  if (!wantsFull(e)) return;
  e.preventDefault();
  e.stopPropagation();
  toggleFull();
}, true);

fullButton.addEventListener('click', () => toggleFull());

// ---------------------------------------------------------------------------
// No pasting into the question box.
//
// Typed questions only. A pasted wall of text is not a question, and the point
// of the limit is to keep this a conversation rather than a document processor.
// Capture again, so it is refused before xterm turns it into input -- xterm's
// own paste handling reads the clipboard itself, so preventing the event is the
// only place to stop it.
for (const ev of ['paste', 'drop', 'dragover']) {
  addEventListener(ev, (e) => {
    e.preventDefault();
    e.stopPropagation();
  }, true);
}

// ---------------------------------------------------------------------------
// The tube.
//
// A chain of passes rather than one shader. The terminal's canvas goes in at
// the top; what comes out the bottom is a photograph of a display, and which
// display is a setting. The chain is:
//
//   signal    the picture as a broadcast carried it, for the screens that were
//             fed by one -- chroma smeared sideways, luma sharp, the two
//             leaking into each other. Skipped by anything with a cable.
//   phosphor  half resolution, and the only pass that remembers: a decaying
//             copy of every frame so far. Green and amber tubes are mostly
//             this, and it is what makes text drag when you scroll.
//   glow      quarter resolution, thresholded, blurred on each axis. Sampled
//             twice at the end -- once tight for bloom, once spread wide for
//             halation, the light that scatters inside the faceplate rather
//             than in the phosphor.
//   tube      the glass, the beam, the mask, the rim and the room it is in.
//
// Everything except the last pass is optional and every screen turns off what
// it does not need, so an aperture-grille tube costs three passes and a VHS
// deck costs five.
//
// It repaints when the terminal repaints, plus for as long as it has something
// left to say: a phosphor still fading, a tube still warming up, a tape still
// moving. Then it stops. An idle screen that warms somebody's laptop is the
// same mistake as an idle screen that streams 100 KB/s, and the screens that
// never settle are ones a visitor has to go and choose.

const VERT = `
attribute vec2 pos;
varying vec2 uv;
void main() {
  uv = pos * 0.5 + 0.5;
  gl_Position = vec4(pos, 0.0, 1.0);
}
`;

// Phosphor persistence. The only pass with a memory, so the only one that has
// to be ping-ponged between two targets.
//
// `max` rather than a blend: a phosphor that is already lit does not average
// with the beam that hits it again, it simply stays lit. Blending made every
// static frame drift dimmer than the one before it.
const FRAG_PERSIST = `
precision highp float;
uniform sampler2D frame;
uniform sampler2D prev;
uniform vec3 decay;
varying vec2 uv;
void main() {
  vec3 now = texture2D(frame, uv).rgb;
  // A fixed floor as well as a fraction. Eight bits per channel and a purely
  // multiplicative decay leaves the last bit lit forever, which is a screen
  // that never finishes fading and therefore never stops asking for frames.
  vec3 old = max(texture2D(prev, uv).rgb * decay - 0.008, vec3(0.0));
  gl_FragColor = vec4(max(now, old), 1.0);
}
`;

// One axis of a gaussian, five taps riding the hardware's linear filtering to
// cover nine. Run twice with `dir` turned, it is a separable blur.
//
// The threshold is here rather than in a pass of its own: the first of the two
// runs is the only one that sees the original image, and cutting the darks
// there costs nothing.
const FRAG_BLUR = `
precision highp float;
uniform sampler2D frame;
uniform vec2 dir;
uniform float cut;
varying vec2 uv;
vec3 lit(vec2 p) {
  vec3 c = texture2D(frame, p).rgb;
  c = max(c - cut, vec3(0.0)) / max(1.0 - cut, 0.001);
  return c * c;
}
void main() {
  vec3 sum = lit(uv) * 0.2270270270;
  sum += (lit(uv + dir * 1.3846153846) + lit(uv - dir * 1.3846153846)) * 0.3162162162;
  sum += (lit(uv + dir * 3.2307692308) + lit(uv - dir * 3.2307692308)) * 0.0702702703;
  // Written back with a gamma curve on it. The target is eight bits and the
  // interesting part of a glow is all in the bottom of the range.
  gl_FragColor = vec4(sqrt(sum), 1.0);
}
`;

// What a composite cable did to a picture.
//
// Colour and brightness went down one wire, separated at the far end by a
// filter that could not quite do it: the chroma comes back smeared sideways
// because it was carried at a fraction of the luma's bandwidth, and some of it
// never makes it out of the luma at all, which is the crawling chequerboard
// along every vertical edge.
//
// The rest is the tape rather than the signal. Each line is written by a head
// on a drum that is not perfectly in step with the last one, so lines start a
// little to the left or right of where they should; and the drum's two heads
// hand over a few lines from the bottom of the frame, which is the tear that
// lives down there on every VHS ever played.
const FRAG_NTSC = `
precision highp float;
uniform sampler2D frame;
uniform vec2 res;
uniform vec2 px;
uniform float dpr;
uniform float time;
varying vec2 uv;

const float PI = 3.14159265359;

float hash(vec2 p) {
  return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453);
}

vec3 toYIQ(vec3 c) {
  return vec3(
    dot(c, vec3(0.299, 0.587, 0.114)),
    dot(c, vec3(0.596, -0.274, -0.322)),
    dot(c, vec3(0.211, -0.523, 0.312)));
}

vec3 toRGB(vec3 c) {
  return vec3(
    c.x + 0.956 * c.y + 0.621 * c.z,
    c.x - 0.272 * c.y - 0.647 * c.z,
    c.x - 1.106 * c.y + 1.703 * c.z);
}

void main() {
  float line = floor(uv.y * res.y / dpr);

  // Where this line starts. Slow drift for the tape, a per-line jump for the
  // heads, and a hard shove through the switching band at the bottom.
  float drift = (hash(vec2(line * 0.031, floor(time * 3.0))) - 0.5) * WOBBLE;
  drift += sin(uv.y * 9.0 + time * 1.7) * WOBBLE * 0.35;
  float sw = smoothstep(0.055, 0.0, uv.y);
  drift += sw * (hash(vec2(line, floor(time * 24.0))) - 0.5) * 0.09;

  vec2 p = vec2(uv.x + drift, uv.y);

  vec3 here = toYIQ(texture2D(frame, p).rgb);
  float y = here.x;

  // Chroma, dragged to the right of where it belongs because it arrives late.
  vec2 iq = vec2(0.0);
  float wsum = 0.0;
  for (int k = 0; k < 8; k++) {
    float o = float(k) * CHROMA_LAG * px.x;
    float w = 1.0 - float(k) / 9.0;
    iq += toYIQ(texture2D(frame, p - vec2(o, 0.0)).rgb).yz * w;
    wsum += w;
  }
  iq /= wsum;

  // The chroma that never left the luma. Its phase advances a quarter cycle
  // per pixel and half a cycle per line, which is what makes the pattern climb
  // the screen instead of sitting still.
  float phase = (p.x * res.x * 0.5 + p.y * res.y + time * 12.0) * PI;
  y += sin(phase) * length(iq) * DOT_CRAWL;

  // Tape grain, and a band of it that walks slowly down the picture.
  float band = exp(-pow((fract(uv.y + time * 0.037) - 0.5) * 7.0, 2.0));
  float grain = hash(vec2(uv.x * res.x, line + floor(time * 30.0))) - 0.5;
  y += grain * (TAPE_GRAIN + band * 0.10);
  iq *= 1.0 - band * 0.45;

  // Through the switching band the signal is barely there at all.
  y = mix(y, y * 0.55 + hash(vec2(uv.x * 90.0, floor(time * 24.0))) * 0.35, sw * 0.85);

  gl_FragColor = vec4(clamp(toRGB(vec3(y, iq)), 0.0, 1.0), 1.0);
}
`;

// The display itself.
//
// One shader, compiled once per screen with a block of `#define`s in front of
// it. The screens are not variations on a theme -- a shadow mask and an LCD
// grid have nothing in common but the quad they are drawn on -- so the parts
// that differ are compiled out rather than branched over, and the parts that
// are shared are shared honestly.
const FRAG_TUBE = `
precision highp float;

uniform sampler2D frame;
uniform sampler2D glow;
uniform sampler2D haze;
uniform sampler2D ghost;
uniform vec2 res;
uniform vec2 px;
uniform float dpr;
uniform float time;
uniform float warm;

varying vec2 uv;

const float PI = 3.14159265359;

float hash(vec2 p) {
  return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453);
}

// Light adds; sRGB does not. Everything between here and the end of main is in
// linear light, because a scanline that halves the signal should halve the
// light and not the number that encodes it. Gamma 2.0 rather than 2.2: it is
// one multiply against three pows, the error is under a percent of a stop, and
// this shader samples the frame a dozen times.
vec3 lin(vec3 c) { return c * c; }
vec3 gam(vec3 c) { return sqrt(max(c, vec3(0.0))); }

// The glass. Pulls the corners out further than the edges, which is what makes
// a rectangle of text read as a curved surface rather than a scaled one.
vec2 bend(vec2 p) {
#if CURVE
  p = p * 2.0 - 1.0;
  vec2 k = abs(p.yx) / vec2(CURVE_X, CURVE_Y);
  p += p * k * k;
  return p * 0.5 + 0.5;
#else
  return p;
#endif
}

// Three guns that never quite converge, and converge worse toward the rim.
//
// The coefficient is small because the text is small. At 0.008 the corners
// split by five or six device pixels, which is *most of a glyph* at a 14px
// font: the section rail and the footer stopped being readable, and a monitor
// effect that eats the navigation is a broken monitor.
vec3 tap(vec2 p) {
  vec2 d = p - 0.5;
  vec2 o = d * dot(d, d) * CONVERGE;
  return lin(vec3(
    texture2D(frame, p + o).r,
    texture2D(frame, p).g,
    texture2D(frame, p - o).b));
}

// The beam, as a line of light with a width rather than a row of pixels.
//
// Four scanlines are asked what they contribute here, each weighted by a
// gaussian on its distance. The width of that gaussian rises with the
// brightness of the line, which is the whole trick: a dark line stays a thin
// bright wire with black either side, and a white one swells until it closes
// the gap to its neighbours. That single relationship is most of why a CRT
// looks like a CRT and a scanline overlay does not.
vec3 beam(vec2 p) {
#if SCAN
  float lines = res.y / (SCAN_PITCH * dpr);
  float s = p.y * lines - 0.5;
  float base = floor(s);
  vec3 sum = vec3(0.0);
  for (int i = -1; i <= 2; i++) {
    float ln = base + float(i);
    vec3 c = tap(vec2(p.x, (ln + 0.5) / lines));
    float lum = dot(c, vec3(0.2126, 0.7152, 0.0722));
    float sig = mix(SIG_MIN, SIG_MAX, sqrt(clamp(lum, 0.0, 1.0)));
    float d = s - ln;
    sum += c * exp(-(d * d) / (2.0 * sig * sig));
  }
  // Normalised on a fixed width, not on the one that was used. Dividing by the
  // actual width would cancel exactly the brightness-to-thickness link the
  // loop above exists to create.
  return sum * (1.0 / (SIG_MID * 2.5066283));
#else
  return tap(p);
#endif
}

// What the screen is made of, at the pitch it is made at.
//
// Every one of these is a period in device pixels, so the mask is a property
// of the visitor's display rather than of the picture: it stays the same size
// whether the window is a phone or a wall, exactly as the real thing does.
vec3 mask(vec2 frag) {
#if MASK == 0
  return vec3(1.0);
#else
  float pitch = MASK_PITCH * dpr;
  float x = frag.x;
  float y = frag.y;

#if MASK == 2
  // A shadow mask is drilled holes, and the rows of holes are staggered so the
  // triads pack hexagonally rather than in columns.
  float rowh = pitch * 0.8660254;
  x += mod(floor(y / rowh), 2.0) * pitch * 0.5;
#endif
#if MASK == 3
  // A slot mask is the compromise between the two: grille stripes, chopped
  // into slots and offset row to row.
  float rowh = pitch * 1.6;
  x += mod(floor(y / rowh), 2.0) * pitch * 0.5;
#endif

  float s = fract(x / pitch) * 3.0;
  vec3 d = abs(vec3(s) - vec3(0.5, 1.5, 2.5));
  d = min(d, 3.0 - d);
  vec3 m = exp(-d * d * MASK_SHARP);
  // Renormalised so a mask changes the colour of a pixel and not the
  // brightness of the picture. Without this every mask is also a dimmer, and
  // the compensation gets made somewhere else where it does not belong.
  m *= 3.0 / max(m.r + m.g + m.b, 0.001);

#if MASK == 2
  float dy = abs(fract(y / rowh) - 0.5) * 2.0;
  m *= 1.0 - 0.5 * dy * dy;
#endif
#if MASK == 3
  float slot = fract(y / rowh);
  m *= 0.45 + 0.55 * smoothstep(0.0, 0.14, slot) * smoothstep(1.0, 0.86, slot);
#endif
#if MASK == 4
  // A panel is a grid, not a weave: hard edges, a black gap between every
  // subpixel and a wider one between every row of them.
  vec3 hard = step(abs(vec3(s) - vec3(0.5, 1.5, 2.5)), vec3(0.42));
  m = mix(m, hard * 2.6, 0.75);
  float gy = fract(y / pitch);
  m *= smoothstep(0.0, 0.12, gy) * smoothstep(1.0, 0.9, gy);
#endif

  return mix(vec3(1.0), m, MASK_DEPTH);
#endif
}

void main() {
  // Two coordinates, because two different things are being shaped. The glass
  // is a fixed object and its rim never moves; the picture is the beam's
  // deflection, and switching the set on and off is that deflection collapsing
  // to a line and then to a point.
  vec2 b = bend(uv);

  // Width first, then height. A tube going out collapses to a line across the
  // middle and then to a point in the centre of it, so a tube coming on does
  // that backwards -- and getting the two the wrong way round reads as a
  // window opening rather than a picture arriving.
  float openX = smoothstep(0.00, 0.42, warm);
  float openY = smoothstep(0.30, 1.00, warm);

  // Underscan: the picture drawn a little smaller than the glass it is in.
  //
  // The rim is a rounded rectangle, and a rounded corner takes a bite out of
  // whatever is behind it -- measured on the curviest screen here, a column and
  // a row along each edge and two or three cells diagonally into each corner.
  // Broadcast solved this the other way round, with an overscanned picture and
  // a title-safe area nobody put anything outside of; there is no such margin
  // in a terminal, where the corners are exactly where the name, the key hints
  // and the clock go. So the raster shrinks instead until all of it clears the
  // rim. How much comes from the radius of that rim -- see 'underscan' in
  // the script below.
  vec2 p = (b - 0.5) * (UNDERSCAN / vec2(max(openX, 0.0012), max(openY, 0.0012))) + 0.5;

  // The rim, as a rounded rectangle with a soft edge rather than a reject.
  // Derivatives are an extension in this version of GL and one device pixel is
  // known here anyway.
  vec2 dd = abs(b * 2.0 - 1.0) - (1.0 - ROUND);
  float sd = length(max(dd, vec2(0.0))) + min(max(dd.x, dd.y), 0.0) - ROUND;
  float aa = 2.5 * max(px.x, px.y);
  float glassy = 1.0 - smoothstep(-aa, aa, sd);

  // Past the edge of the picture is the unlit part of the phosphor, which is
  // not the same colour as the bezel and not black either.
  vec2 g2 = step(vec2(0.0), p) * step(p, vec2(1.0));
  float painted = g2.x * g2.y;

  vec3 c = beam(p) * painted;

#if GHOSTING
  // Only the part of the memory that is brighter than what is on screen now.
  // The rest of it is the picture, and adding the picture to itself is just a
  // gain control with extra steps.
  vec3 old = lin(texture2D(ghost, p).rgb) * painted;
  c += max(old - c, vec3(0.0)) * GHOST_GAIN;
#endif

#if BLOOM
  // Halation is not bloom. Bloom is the phosphor spilling into its neighbours;
  // halation is light that made it into the faceplate, bounced off the front
  // of the glass and came back out somewhere else entirely -- so it is wide,
  // weak, and warmer than what caused it, because the glass passes red best.
  // Two blurs rather than one sampled at an offset: four taps around a thin
  // bright line is four copies of that line, which is not what scattering
  // looks like from any distance at all.
  c += lin(texture2D(glow, p).rgb) * BLOOM_GAIN;
  c += lin(texture2D(haze, p).rgb) * vec3(HALO_R, HALO_G, HALO_B);
#endif

  // The phosphor, after everything that is made of phosphor light and before
  // anything that is not. A single-gun tube has no way to be told what colour
  // to be, and that has to be as true of its afterglow and its halation as it
  // is of the beam -- monochroming only the beam left a green screen glowing
  // in the colours of the thing it was supposed to have forgotten.
#if MONO
  c = vec3(dot(c, vec3(0.2126, 0.7152, 0.0722))) * vec3(TINT_R, TINT_G, TINT_B);
#else
  c *= vec3(TINT_R, TINT_G, TINT_B);
#endif

  c *= mask(gl_FragCoord.xy);

#if FLICKER
  // The picture and the mains are never quite the same frequency, so their
  // difference walks up the screen as a broad band about a stop down.
  float hum = sin((b.y * 1.7 - time * 0.31) * PI * 2.0);
  c *= 1.0 + hum * FLICKER_AMT;
#endif

  // Falls off toward the corners, the way the gun does.
  //
  // Weighted low on purpose: the four corners of this app are the name, the
  // section rail, the key hints and the work index -- every piece of
  // navigation it has. Measured off a real frame, 0.6 here left the rail with
  // 57% of its contrast and the corner hints with 44%, which is a mood bought
  // with the interface.
  vec2 v = b * (1.0 - b.yx);
  c *= mix(1.0, clamp(pow(v.x * v.y * 24.0, 0.22), 0.0, 1.0), VIGNETTE);

#if BACKLIGHT
  // A panel is lit from behind by tubes down its edges, and they leak.
  float edge = max(abs(b.x - 0.5), abs(b.y - 0.5)) * 2.0;
  c += pow(edge, 8.0) * vec3(0.026, 0.028, 0.040);
#endif

#if SHEEN
  // The room the screen is in. A band of window across the face and a soft
  // patch of ceiling light above it -- both stuck to the glass rather than to
  // the picture, which is what tells you there is glass there at all.
  float band = smoothstep(0.42, 0.0, abs(b.x * 0.62 + b.y - 0.78));
  c += band * SHEEN_AMT * vec3(0.55, 0.62, 0.80);
  vec2 r = (b - vec2(0.26, 0.84)) * vec2(1.9, 3.1);
  c += exp(-dot(r, r)) * SHEEN_AMT * vec3(0.9, 0.9, 1.0);
#endif

  // Phosphor is never entirely off, and neither is the glass: a dead-black CRT
  // is the one thing a real one never manages.
  c += vec3(GLOW_R, GLOW_G, GLOW_B);

  c += (hash(gl_FragCoord.xy + fract(time) * 311.0) - 0.5) * GRAIN;

  // Warming up, or going out. The flash is the last of the charge on the
  // deflection plates dumping into a single line across the middle.
  c *= smoothstep(0.0, 0.55, warm);
  float shut = 1.0 - abs(warm * 2.0 - 1.0);
  float wire = exp(-pow((uv.y - 0.5) * 90.0, 2.0));
  c += vec3(0.85, 0.92, 1.0) * shut * shut * wire * 0.7;

  // Outside the glass is the inside of the bezel. Not more terminal, and not
  // pure black either: some of the tube's own light lands on it.
  vec3 rim = vec3(0.011, 0.011, 0.013) * (1.0 - smoothstep(0.0, 0.05, sd));
  c = mix(rim, c, glassy);

  gl_FragColor = vec4(gam(c), 1.0);
}
`;

// The screens.
//
// Ordered as the switch cycles them, which is roughly the order they were sat
// in front of. `flags` are compiled as integers and decide which parts of the
// shader exist at all; `nums` are the numbers those parts use. Everything a
// screen does not set gets the default below, so a new one is a short entry
// rather than a full copy.
const SCREEN_BASE = {
  flags: {
    CURVE: 1, SCAN: 1, MASK: 1, MONO: 0, GHOSTING: 0, BLOOM: 1,
    FLICKER: 0, SHEEN: 1, BACKLIGHT: 0,
  },
  nums: {
    CURVE_X: 7.0, CURVE_Y: 5.0,
    SCAN_PITCH: 1.5, SIG_MIN: 0.34, SIG_MAX: 0.68,
    MASK_PITCH: 3.0, MASK_DEPTH: 0.30, MASK_SHARP: 3.4,
    CONVERGE: 0.0038,
    BLOOM_GAIN: 0.42, HALO_R: 0.16, HALO_G: 0.10, HALO_B: 0.07,
    GHOST_GAIN: 0.0,
    TINT_R: 1.0, TINT_G: 1.0, TINT_B: 1.0,
    VIGNETTE: 0.34, ROUND: 0.06, SHEEN_AMT: 0.012, GRAIN: 0.010,
    FLICKER_AMT: 0.0,
    GLOW_R: 0.010, GLOW_G: 0.011, GLOW_B: 0.015,
    WOBBLE: 0.0, CHROMA_LAG: 0.0, DOT_CRAWL: 0.0, TAPE_GRAIN: 0.0,
  },
};

const SCREENS = [
  {
    id: 'p22',
    hint: 'a shadow-mask colour tube, the one on the desk',
    flags: { MASK: 2 },
    nums: { MASK_PITCH: 3.2, MASK_DEPTH: 0.26, MASK_SHARP: 2.6 },
  },
  {
    id: 'grille',
    hint: 'an aperture grille: flatter glass, brighter, and two wires across it',
    flags: { MASK: 1 },
    nums: {
      CURVE_X: 14.0, CURVE_Y: 11.0,
      MASK_PITCH: 2.8, MASK_DEPTH: 0.34, MASK_SHARP: 4.2,
      SIG_MIN: 0.32, SIG_MAX: 0.64,
      TINT_R: 0.98, TINT_G: 1.0, TINT_B: 1.04,
      BLOOM_GAIN: 0.5,
    },
  },
  {
    id: 'slot',
    hint: 'a slot mask, off the back of a television',
    flags: { MASK: 3, FLICKER: 1 },
    nums: {
      CURVE_X: 5.0, CURVE_Y: 3.6,
      MASK_PITCH: 3.6, MASK_DEPTH: 0.32, MASK_SHARP: 3.0,
      SCAN_PITCH: 1.8, SIG_MIN: 0.40, SIG_MAX: 0.80,
      CONVERGE: 0.0062, FLICKER_AMT: 0.035, VIGNETTE: 0.42,
      HALO_R: 0.22, HALO_G: 0.13, HALO_B: 0.09,
    },
  },
  {
    id: 'amber',
    hint: 'P3 amber, one gun and a long memory',
    flags: { MASK: 0, MONO: 1, GHOSTING: 1 },
    persist: [0.90, 0.86, 0.72],
    nums: {
      SCAN_PITCH: 1.4, SIG_MIN: 0.32, SIG_MAX: 0.62,
      CONVERGE: 0.0,
      TINT_R: 1.0, TINT_G: 0.62, TINT_B: 0.12,
      GHOST_GAIN: 0.55,
      BLOOM_GAIN: 0.55, HALO_R: 0.26, HALO_G: 0.15, HALO_B: 0.04,
      GLOW_R: 0.014, GLOW_G: 0.009, GLOW_B: 0.004,
      GRAIN: 0.006,
    },
  },
  {
    id: 'green',
    hint: 'P1 green, and it lets go of nothing',
    flags: { MASK: 0, MONO: 1, GHOSTING: 1 },
    persist: [0.80, 0.945, 0.82],
    nums: {
      CURVE_X: 5.5, CURVE_Y: 4.0,
      SCAN_PITCH: 1.4, SIG_MIN: 0.34, SIG_MAX: 0.66,
      CONVERGE: 0.0,
      TINT_R: 0.24, TINT_G: 1.0, TINT_B: 0.40,
      GHOST_GAIN: 0.8,
      BLOOM_GAIN: 0.6, HALO_R: 0.10, HALO_G: 0.28, HALO_B: 0.12,
      GLOW_R: 0.005, GLOW_G: 0.016, GLOW_B: 0.007,
      VIGNETTE: 0.40, GRAIN: 0.008,
    },
  },
  {
    id: 'vhs',
    hint: 'the same tube, fed off a worn tape',
    flags: { MASK: 3, FLICKER: 1, GHOSTING: 1 },
    signal: true,
    persist: [0.62, 0.62, 0.66],
    animated: true,
    nums: {
      CURVE_X: 5.0, CURVE_Y: 3.6,
      MASK_PITCH: 3.6, MASK_DEPTH: 0.30, MASK_SHARP: 3.0,
      SCAN_PITCH: 1.9, SIG_MIN: 0.44, SIG_MAX: 0.86,
      CONVERGE: 0.0085, GHOST_GAIN: 0.35,
      FLICKER_AMT: 0.045, VIGNETTE: 0.45,
      BLOOM_GAIN: 0.55, HALO_R: 0.24, HALO_G: 0.15, HALO_B: 0.12,
      GRAIN: 0.020,
      WOBBLE: 0.0016, CHROMA_LAG: 2.6, DOT_CRAWL: 0.30, TAPE_GRAIN: 0.045,
    },
  },
  {
    id: 'lcd',
    hint: 'an early colour panel: square, slow, and lit from the edges',
    flags: { CURVE: 0, SCAN: 0, MASK: 4, GHOSTING: 1, BACKLIGHT: 1, SHEEN: 1 },
    persist: [0.55, 0.58, 0.52],
    nums: {
      MASK_PITCH: 3.0, MASK_DEPTH: 0.42, MASK_SHARP: 3.0,
      CONVERGE: 0.0,
      GHOST_GAIN: 0.65,
      BLOOM_GAIN: 0.14, HALO_R: 0.05, HALO_G: 0.05, HALO_B: 0.06,
      TINT_R: 0.97, TINT_G: 1.0, TINT_B: 1.03,
      VIGNETTE: 0.10, ROUND: 0.012, SHEEN_AMT: 0.020, GRAIN: 0.004,
      GLOW_R: 0.013, GLOW_G: 0.014, GLOW_B: 0.018,
    },
  },
];

const screenById = (id) => SCREENS.filter((s) => s.id === id)[0] || SCREENS[0];

/// How much smaller than the glass the picture has to be drawn.
///
/// The rim is a rounded rectangle of half-size `1 - ROUND` grown by a disc of
/// radius `ROUND`, so along the axes it reaches exactly the edge and on the
/// diagonal it stops short at `1 - ROUND(1 - 1/sqrt2)`. The corner of the
/// picture is the point that has to clear that, and the scale which puts it
/// there is one over that number.
///
/// Plus a little, for the half-pixel the rim is feathered over. Both terms are
/// small: at the radius these screens use it is a shade over two percent, which
/// is about a column and a half at each edge -- which is what was being lost.
const underscan = (nums) => 1.0 / (1.0 - nums.ROUND * (1.0 - Math.SQRT1_2)) + 0.004;

// `#define`s, not uniforms. A screen is chosen a handful of times in a session
// and read a few million times a frame, so the numbers belong in the compile.
const preamble = (s) => {
  const flags = Object.assign({}, SCREEN_BASE.flags, s.flags || {});
  const nums = Object.assign({}, SCREEN_BASE.nums, s.nums || {});
  const out = [];
  for (const k in flags) out.push(`#define ${k} ${flags[k] | 0}`);
  // Always with a decimal point on it: GLSL will not quietly widen an integer
  // literal into a float, and `mix(1, x, y)` is a compile error rather than a
  // rounding error.
  for (const k in nums) out.push(`#define ${k} ${nums[k].toFixed(6)}`);
  out.push(`#define SIG_MID ${((nums.SIG_MIN + nums.SIG_MAX) * 0.5).toFixed(6)}`);
  out.push(`#define UNDERSCAN ${underscan(nums).toFixed(6)}`);
  return out.join('\n') + '\n';
};

const glass = document.getElementById('glass');

const tube = {
  on: false,
  gl: null,
  screen: SCREENS[0].id,
  progs: {},
  tex: {},
  fbo: {},
  size: { w: 0, h: 0 },
  pending: 0,
  settle: 0,
  warm: 0,
  warmTo: 0,
  last: 0,
  t0: 0,
  after: null,
  chain: true,

  now() {
    return performance.now() / 1000;
  },

  // Compile on the first time somebody asks for it. Nobody who leaves the
  // switch alone pays for a GL context.
  start() {
    const gl = glass.getContext('webgl', {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      preserveDrawingBuffer: false,
      powerPreference: 'low-power',
    });
    if (!gl) return false;
    this.gl = gl;
    this.t0 = performance.now() / 1000;

    const quad = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, quad);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]),
                  gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);

    try {
      // Neither of these carries a screen's numbers -- what they do is set by
      // uniforms -- so both are compiled once here rather than per screen.
      this.progs.persist = this.link(FRAG_PERSIST);
      this.progs.blur = this.link(FRAG_BLUR);
    } catch (e) {
      // The chain is a luxury; the last pass is not. A driver that will not
      // take one of these still gets a tube, just a plainer one.
      this.chain = false;
    }
    if (!this.program(this.screen)) {
      this.gl = null;
      return false;
    }

    this.source = gl.createTexture();
    this.bind(this.source, gl.LINEAR);
    // A 2D canvas has its origin top-left and a texture has it bottom-left.
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);

    // Something valid to hand the samplers a screen is not using. Sampling an
    // unbound unit is undefined, and on some drivers that is a black frame and
    // on others it is whatever was there before.
    this.blank = gl.createTexture();
    this.bind(this.blank, gl.NEAREST);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB, 1, 1, 0, gl.RGB, gl.UNSIGNED_BYTE,
                  new Uint8Array([0, 0, 0]));

    this.scratch = document.createElement('canvas');
    this.sctx = this.scratch.getContext('2d', { alpha: false });
    return true;
  },

  link(fsrc, defs) {
    const gl = this.gl;
    const build = (type, src) => {
      const s = gl.createShader(type);
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
        throw new Error(gl.getShaderInfoLog(s) || 'shader would not compile');
      }
      return s;
    };
    const prog = gl.createProgram();
    gl.attachShader(prog, build(gl.VERTEX_SHADER, VERT));
    gl.attachShader(prog, build(gl.FRAGMENT_SHADER, (defs || '') + fsrc));
    // Bound rather than looked up, so every program in the chain reads the one
    // quad that was bound at startup and nothing has to rebind between passes.
    gl.bindAttribLocation(prog, 0, 'pos');
    gl.linkProgram(prog);
    if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
      throw new Error(gl.getProgramInfoLog(prog) || 'program would not link');
    }
    return prog;
  },

  // One program per screen, compiled the first time it is chosen and kept --
  // including the failures, as null. A shader that would not compile will not
  // compile on the next frame either, and trying it sixty times a second is
  // how a screen that merely looks wrong becomes a screen that also stutters.
  compile(key, src, id) {
    if (this.progs[key] !== undefined) return this.progs[key];
    try {
      this.progs[key] = this.link(src, preamble(screenById(id)));
    } catch (e) {
      this.progs[key] = null;
    }
    return this.progs[key];
  },

  program(id) {
    return this.compile('tube:' + id, FRAG_TUBE, id);
  },

  // The signal pass carries the screen's numbers as well, so it is compiled
  // per screen too, and only for the screens that are fed by one.
  signal(id) {
    return this.compile('signal:' + id, FRAG_NTSC, id);
  },

  bind(tex, filter) {
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  },

  target(name, w, h) {
    const gl = this.gl;
    if (!this.tex[name]) {
      this.tex[name] = gl.createTexture();
      this.fbo[name] = gl.createFramebuffer();
    }
    this.bind(this.tex[name], gl.LINEAR);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
    gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo[name]);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D,
                            this.tex[name], 0);
    const ok = gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    return ok;
  },

  resize() {
    // Nothing to size while it is hidden: a `display: none` canvas measures
    // zero, and reallocating every target to one pixel is not a useful answer
    // to a window being dragged with the tube switched off.
    if (!this.gl || !this.on) return;
    // Capped at 2, and capped again on area: past either of those this is
    // several million fragments a pass to simulate a monitor that never had
    // that many pixels.
    let dpr = Math.min(window.devicePixelRatio || 1, 2);
    let w = Math.max(1, Math.round(glass.clientWidth * dpr));
    let h = Math.max(1, Math.round(glass.clientHeight * dpr));
    const budget = 4.2e6;
    if (w * h > budget) {
      const k = Math.sqrt(budget / (w * h));
      w = Math.max(1, Math.round(w * k));
      h = Math.max(1, Math.round(h * k));
      dpr *= k;
    }
    this.dpr = dpr;
    if (this.size.w === w && this.size.h === h) return;
    this.size = { w, h };

    glass.width = w;
    glass.height = h;
    this.scratch.width = w;
    this.scratch.height = h;
    this.gl.viewport(0, 0, w, h);

    if (this.chain) {
      const half = [Math.max(1, w >> 1), Math.max(1, h >> 1)];
      const quarter = [Math.max(1, w >> 2), Math.max(1, h >> 2)];
      this.chain =
        this.target('signal', w, h) &&
        this.target('keepA', half[0], half[1]) &&
        this.target('keepB', half[0], half[1]) &&
        this.target('blurA', quarter[0], quarter[1]) &&
        this.target('blurB', quarter[0], quarter[1]) &&
        this.target('blurC', quarter[0], quarter[1]);
    }
  },

  // What this screen is allowed to do here. Reduced motion is not a request to
  // make the picture worse, it is a request for it to hold still -- so the
  // glass, the mask and the beam all stay and only the parts that move go.
  live() {
    const s = screenById(this.screen);
    if (!reducedMotion) return s;
    const still = Object.assign({}, s);
    still.persist = null;
    still.animated = false;
    return still;
  },

  // A repaint of the terminal, and however many more it takes for whatever is
  // still moving to stop moving.
  schedule() {
    if (!this.on) return;
    const s = this.live();
    if (s.persist) this.settle = 48;
    this.wake();
  },

  wake() {
    if (this.pending || !this.gl || document.hidden) return;
    this.pending = requestAnimationFrame(() => {
      this.pending = 0;
      this.frame();
    });
  },

  frame() {
    const now = this.now();
    const dt = this.last ? Math.min(now - this.last, 0.1) : 0.016;
    this.last = now;

    // Toward the switch, at a speed that reads as a tube rather than a fade --
    // unless the visitor has asked for less of that, in which case the picture
    // is simply there or simply gone. Collapsing to a line is the single most
    // motion-like thing on this page.
    const rate = reducedMotion ? 1e6 : (this.warmTo > this.warm ? 1.6 : 2.4);
    const step = rate * dt;
    if (Math.abs(this.warmTo - this.warm) <= step) this.warm = this.warmTo;
    else this.warm += Math.sign(this.warmTo - this.warm) * step;

    this.draw();

    if (this.settle > 0) this.settle--;
    const s = this.live();
    const moving = this.warm !== this.warmTo || (this.on && (s.animated || this.settle > 0));
    if (moving) this.wake();
    else if (this.after && this.warm === this.warmTo) {
      const done = this.after;
      this.after = null;
      done();
    }
  },

  // Every pass is the same quad, so a pass is a program, a target and a list of
  // textures to read.
  pass(prog, target, w, h, reads, set) {
    const gl = this.gl;
    gl.bindFramebuffer(gl.FRAMEBUFFER, target ? this.fbo[target] : null);
    gl.viewport(0, 0, w, h);
    gl.useProgram(prog);
    for (let i = 0; i < reads.length; i++) {
      gl.activeTexture(gl.TEXTURE0 + i);
      gl.bindTexture(gl.TEXTURE_2D, reads[i][1]);
      gl.uniform1i(gl.getUniformLocation(prog, reads[i][0]), i);
    }
    if (set) set(gl, prog);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  },

  // The switches, painted into the picture rather than over it.
  //
  // They have to be DOM -- they are the things you click, and they are the only
  // part of this page a screen reader can be told about. But a crisp label
  // sitting on top of a curved, scanlined picture is the browser-landed-on-a-
  // terminal look that the rest of this went to some trouble to avoid. So while
  // the tube is on the DOM keeps the clicks and gives up the ink, and the same
  // three words go onto the frame before it reaches the glass: they curve with
  // it, they get the mask, they dim into the corner.
  //
  // Positioned and coloured off the elements themselves rather than off a copy
  // of their state. There is one arrangement of these words and one set of
  // rules for what colour they are, and both of them are in the stylesheet.
  switches() {
    if (!glass.clientWidth) return;
    const scale = this.size.w / glass.clientWidth;
    const ctx = this.sctx;
    ctx.textBaseline = 'top';
    ctx.font = `${14 * scale}px "DejaVu Sans Mono", "Menlo", ui-monospace, monospace`;
    for (const it of laidOut) {
      ctx.fillStyle = getComputedStyle(it.el).getPropertyValue('--ink').trim() || '#3a3e46';
      ctx.fillText(`[${it.el.textContent}]`, it.x * scale, it.y * scale);
    }
  },

  draw() {
    if (!this.gl) return;
    const gl = this.gl;
    const s = this.live();
    const prog = this.program(this.screen);
    if (!prog) return;
    const { w, h } = this.size;
    const time = this.now() - this.t0;

    // xterm's canvas renderer stacks its layers as separate canvases in one
    // element, so they are flattened here in the order it draws them.
    this.sctx.fillStyle = '#08090b';
    this.sctx.fillRect(0, 0, w, h);
    const layers = screen.querySelectorAll('.xterm-screen canvas');
    for (let i = 0; i < layers.length; i++) {
      this.sctx.drawImage(layers[i], 0, 0, w, h);
    }
    this.switches();
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.source);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB, gl.RGB, gl.UNSIGNED_BYTE, this.scratch);

    let lit = this.source;
    let ghost = this.blank;
    let glow = this.blank;
    let haze = this.blank;

    const signal = s.signal ? this.signal(this.screen) : null;
    if (this.chain && signal) {
      this.pass(signal, 'signal', w, h, [['frame', lit]], (gl, p) => {
        gl.uniform2f(gl.getUniformLocation(p, 'res'), w, h);
        gl.uniform2f(gl.getUniformLocation(p, 'px'), 1 / w, 1 / h);
        gl.uniform1f(gl.getUniformLocation(p, 'dpr'), this.dpr);
        // Frozen when nothing is meant to move: a still frame of a tape rather
        // than a tape that jumps once per keystroke.
        gl.uniform1f(gl.getUniformLocation(p, 'time'), s.animated ? time : 0);
      });
      lit = this.tex.signal;
    }

    if (this.chain && s.persist) {
      const hw = Math.max(1, w >> 1);
      const hh = Math.max(1, h >> 1);
      const from = this.flip ? 'keepB' : 'keepA';
      const to = this.flip ? 'keepA' : 'keepB';
      this.pass(this.progs.persist, to, hw, hh,
        [['frame', lit], ['prev', this.tex[from]]], (gl, p) => {
          gl.uniform3f(gl.getUniformLocation(p, 'decay'),
                       s.persist[0], s.persist[1], s.persist[2]);
        });
      this.flip = !this.flip;
      ghost = this.tex[to];
    }

    if (this.chain) {
      const qw = Math.max(1, w >> 2);
      const qh = Math.max(1, h >> 2);
      // Four runs of one axis each: a tight pair off the picture for the
      // bloom, and a wide pair off the tight one for the halation. Blurring an
      // already blurred image is how a large radius is bought cheaply, and the
      // second pair costs the same as the first because both are at a quarter.
      const blur = (into, from, dx, dy, cut) =>
        this.pass(this.progs.blur, into, qw, qh, [['frame', from]], (gl, p) => {
          gl.uniform2f(gl.getUniformLocation(p, 'dir'), dx, dy);
          gl.uniform1f(gl.getUniformLocation(p, 'cut'), cut);
        });
      blur('blurA', lit, 1 / qw, 0, 0.22);
      blur('blurB', this.tex.blurA, 0, 1 / qh, 0);
      blur('blurA', this.tex.blurB, 3.5 / qw, 0, 0);
      blur('blurC', this.tex.blurA, 0, 3.5 / qh, 0);
      glow = this.tex.blurB;
      haze = this.tex.blurC;
    }

    this.pass(prog, null, w, h,
      [['frame', lit], ['glow', glow], ['haze', haze], ['ghost', ghost]], (gl, p) => {
        gl.uniform2f(gl.getUniformLocation(p, 'res'), w, h);
        gl.uniform2f(gl.getUniformLocation(p, 'px'), 1 / w, 1 / h);
        gl.uniform1f(gl.getUniformLocation(p, 'dpr'), this.dpr);
        gl.uniform1f(gl.getUniformLocation(p, 'time'), s.animated ? time : 0);
        gl.uniform1f(gl.getUniformLocation(p, 'warm'), this.warm);
      });
  },
};
const button = document.getElementById('crt');
const picker = document.getElementById('screen');

const showScreen = () => {
  const s = screenById(tube.screen);
  picker.textContent = s.id;
  picker.title = s.hint;
  // `one-piece` is six columns wider than `p22`, and the app is keeping that
  // many columns clear for it. The glass may bend differently too -- an
  // aperture grille is flatter than a television and a panel is flat.
  measure();
};

const setScreen = (id) => {
  // A screen that will not compile is not a screen this browser has. Better to
  // stay on the one that works than to hand somebody a black rectangle.
  if (!tube.program(id)) return;
  tube.screen = id;
  showScreen();
  try { localStorage.setItem('crt-screen', id); } catch (e) { /* private mode */ }
  if (tube.on) {
    // One frame is enough to change screens. The run of them is for a phosphor
    // that has to fade the old picture out, and only a screen with one needs it.
    tube.settle = tube.live().persist ? 48 : 0;
    tube.wake();
  }
};

const setCrt = (on) => {
  if (on && !tube.gl && !tube.start()) {
    button.disabled = true;
    button.title = 'no webgl in this browser';
    return;
  }
  button.setAttribute('aria-pressed', on ? 'true' : 'false');
  picker.hidden = !on;
  // The switch appears with the tube and goes with it, so the room it needs
  // does too. `place` comes after the branch below rather than here, because
  // whether these are being bent at all is `tube.on`, and that has not moved
  // yet -- on the way out it does not move until the picture has finished
  // going.
  measure();
  try { localStorage.setItem('crt', on ? '1' : '0'); } catch (e) { /* private mode */ }
  if (on) {
    tube.on = true;
    useRenderer('canvas');
    screen.classList.add('shaded');
    switches.classList.add('shaded');
    glass.classList.add('on');
    tube.resize();
    tube.warmTo = 1;
    tube.after = null;
    // The renderer just changed under it, and a screen nobody has typed on
    // has nothing to repaint on its own.
    term.refresh(0, term.rows - 1);
    place();
    tube.schedule();
  } else {
    // Going out is an animation, so everything it is drawn from has to stay
    // where it is until it has finished: the canvas renderer, the hidden
    // terminal underneath and the canvas on top all wait for `after`.
    tube.warmTo = 0;
    tube.after = () => {
      tube.on = false;
      glass.classList.remove('on');
      screen.classList.remove('shaded');
      switches.classList.remove('shaded');
      place();
      useRenderer('webgl');
    };
    tube.wake();
  }
  // Clicking the switch must not be a way to lose the keyboard.
  term.focus();
};

// Every repaint of the terminal is a repaint of the tube, and nothing else is
// -- except the switches, whose hover is drawn into the frame now and so needs
// a frame to be drawn into.
term.onRender(() => tube.schedule());
switches.addEventListener('mouseover', () => tube.wake());
switches.addEventListener('mouseout', () => tube.wake());
switches.addEventListener('focusin', () => tube.wake());
switches.addEventListener('focusout', () => tube.wake());

// A tab in the background is not a tube anybody is looking at. The frames it
// would have drawn are not owed to it afterwards either -- whatever was fading
// has finished fading by the time it comes back.
addEventListener('visibilitychange', () => {
  if (document.hidden) {
    if (tube.pending) cancelAnimationFrame(tube.pending);
    tube.pending = 0;
    tube.last = 0;
  } else if (tube.on) {
    tube.settle = 2;
    tube.wake();
  }
});

if (window.CanvasAddon) {
  let want = null;
  let chosen = null;
  try {
    want = localStorage.getItem('crt');
    chosen = localStorage.getItem('crt-screen');
  } catch (e) { /* private mode */ }
  if (chosen && screenById(chosen).id === chosen) tube.screen = chosen;
  showScreen();
  button.addEventListener('click', () => {
    setCrt(button.getAttribute('aria-pressed') !== 'true');
  });
  picker.addEventListener('click', () => {
    const ids = SCREENS.map((s) => s.id);
    setScreen(ids[(ids.indexOf(tube.screen) + 1) % ids.length]);
  });
  if (want === '1' && !reducedMotion) setCrt(true);
} else {
  button.disabled = true;
  picker.hidden = true;
  button.title = 'the canvas renderer this needs did not load';
}


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

    #[test]
    fn browser_preserves_modified_editing_keys() {
        for sequence in [r"\x1b[13;2u", r"\x1b[127;5u", r"\x1b[127;3u"] {
            assert!(INDEX.contains(sequence), "browser does not send {sequence}");
        }
    }

    /// The page says something when the one thing it fetches does not arrive.
    ///
    /// xterm comes off a CDN, and a CDN is a thing that is blocked in some
    /// countries and down in the rest. Every line of the client is written
    /// against `Terminal` existing, so without it the first one throws and the
    /// visitor gets a black rectangle and no reason for it -- and the reason is
    /// not theirs to go and find. The check has to come before that first line,
    /// which is what this pins.
    #[test]
    fn a_page_whose_terminal_did_not_arrive_says_so() {
        let script = part("<script>\n// The one thing on this page", "</script>");
        let guard = script
            .find("typeof Terminal === 'undefined'")
            .expect("nothing checks whether the terminal loaded");
        let first_use = script
            .find("new Terminal(")
            .expect("the client stopped making a terminal");
        assert!(
            guard < first_use,
            "the check for a missing terminal comes after the code that needs one"
        );
        // And it points at the way in that does not depend on a CDN at all.
        assert!(script.contains("ssh -p 2222"), "the fallback offers no way in");
    }

    /// The text channel carries statements about the window and nothing else.
    #[test]
    fn the_text_channel_is_a_size_or_a_gutter_and_never_input() {
        assert!(matches!(parse_text("r120x40"), Some(session::In::Resize(120, 40))));
        assert!(matches!(parse_text("g16"), Some(session::In::Gutter(16))));
        assert!(matches!(parse_text("g0"), Some(session::In::Gutter(0))));
        // Typed characters come in on the binary channel. Anything here that is
        // neither of the two is dropped rather than guessed at -- a `g` is a
        // gutter, and `great` is not a gutter of any width.
        for junk in ["great", "g", "gx", "rm -rf /", "", "i-visitor", "m1"] {
            assert!(parse_text(junk).is_none(), "`{junk}` was taken as a message");
        }
    }

    #[test]
    fn public_clients_cannot_spoof_forwarded_addresses() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.8".parse().unwrap());
        let public = "203.0.113.7:1234".parse().unwrap();
        let proxy = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(client_ip(&headers, public), "203.0.113.7");
        assert_eq!(client_ip(&headers, proxy), "198.51.100.8");
    }

    /// The client between two markers, for reading one declaration out of it.
    fn part<'a>(from: &str, to: &str) -> &'a str {
        let at = INDEX.find(from).unwrap_or_else(|| panic!("no `{from}` in the client"))
            + from.len();
        let end = INDEX[at..]
            .find(to)
            .unwrap_or_else(|| panic!("`{from}` is never closed by `{to}`"));
        &INDEX[at..at + end]
    }

    /// Line comments out. Every one of these shaders talks about CRTs and LCDs
    /// and NTSC in its prose, and none of those are constants.
    fn code(src: &str) -> String {
        src.lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<&str>>()
            .join("\n")
    }

    /// SHOUTING identifiers, whole words only.
    fn shouted(src: &str) -> Vec<String> {
        let bytes: Vec<char> = src.chars().collect();
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let mut out = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if !bytes[at].is_ascii_uppercase() || (at > 0 && ident(bytes[at - 1])) {
                at += 1;
                continue;
            }
            let mut end = at;
            while end < bytes.len() && (bytes[end].is_ascii_uppercase() || bytes[end] == '_'
                || bytes[end].is_ascii_digit())
            {
                end += 1;
            }
            // `toYIQ` is not a constant and neither is `PI`; a name has to be a
            // whole word and worth more than two letters to be one.
            if (end == bytes.len() || !ident(bytes[end])) && end - at >= 3 {
                out.push(bytes[at..end].iter().collect());
            }
            at = end.max(at + 1);
        }
        out.sort();
        out.dedup();
        out
    }

    /// Names being declared, as `NAME:` in an object literal.
    fn declared(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for name in shouted(&code(src)) {
            for token in src.match_indices(&name) {
                let after = src[token.0 + name.len()..].trim_start();
                if after.starts_with(':') {
                    out.push(name.clone());
                    break;
                }
            }
        }
        out
    }

    /// Every constant the screens are compiled with is one the screens define.
    ///
    /// Written for a bug that no amount of reading found. The signal shader was
    /// linked without the `#define` block in front of it, so the four numbers it
    /// reads did not exist, so it would not compile -- and because one `catch`
    /// covered three programs, the phosphor and the bloom went with it. Nothing
    /// reported anything. Every screen simply came out worse than it was meant
    /// to, in a way only a side-by-side would show.
    ///
    /// A browser is the only thing that can compile these, and there is no
    /// browser in here. Checking the names is what there is, and the names are
    /// where that bug lived.
    #[test]
    fn every_constant_a_shader_reads_is_one_a_screen_defines() {
        let base = part("const SCREEN_BASE = {", "\n};");
        let mut known = declared(base);
        // Emitted by `preamble` rather than written in the table: one is the
        // midpoint of two that are, the other is worked out from the radius of
        // the rim.
        known.push("SIG_MID".to_string());
        known.push("UNDERSCAN".to_string());

        for (what, src) in [
            ("the tube", part("const FRAG_TUBE = `", "\n`;")),
            ("the signal", part("const FRAG_NTSC = `", "\n`;")),
        ] {
            for name in shouted(&code(src)) {
                assert!(
                    known.contains(&name),
                    "{what} shader reads `{name}`, which no screen defines"
                );
            }
        }
    }

    /// No shader contains the character that ends a shader.
    ///
    /// They are template literals, so a backtick anywhere inside one closes it
    /// early and the rest of the file becomes whatever the parser makes of the
    /// remains. Which is nothing: the page throws on load and every switch on
    /// it is dead. It got in through a comment -- prose about a function, in
    /// the punctuation prose about a function is written in.
    ///
    /// The name check below does not catch it. A stray backtick still leaves
    /// the real closing one where it was, so the slice it reads is intact and
    /// says nothing is wrong.
    #[test]
    fn no_shader_ends_itself_early() {
        for (what, from) in [
            ("VERT", "const VERT = `"),
            ("persist", "const FRAG_PERSIST = `"),
            ("blur", "const FRAG_BLUR = `"),
            ("signal", "const FRAG_NTSC = `"),
            ("tube", "const FRAG_TUBE = `"),
        ] {
            let body = part(from, "\n`;");
            assert!(
                !body.contains('`'),
                "the {what} shader has a backtick in it, which ends it here:\n{}",
                body.split('`').next().unwrap_or("").lines().last().unwrap_or("")
            );
            assert!(
                body.contains("void main"),
                "the {what} shader has no main; it was cut short"
            );
        }
    }

    /// And every constant a screen sets is one a shader would read.
    ///
    /// The other half of the same mistake, and the quieter one: a misspelled
    /// override is not an error anywhere. It is silently ignored, the screen
    /// keeps the default, and the only symptom is that turning a number did
    /// nothing.
    #[test]
    fn every_constant_a_screen_sets_is_one_the_shaders_know() {
        let known = declared(part("const SCREEN_BASE = {", "\n};"));
        for name in declared(part("const SCREENS = [", "\n];")) {
            assert!(
                known.contains(&name),
                "a screen sets `{name}`, which is not one of the constants"
            );
        }
    }

    /// The switch has something to say before any of this has run.
    ///
    /// The label is in the markup rather than written in by script, so it is
    /// what a visitor sees for the first frame -- and if it names a screen that
    /// is not the one the tube starts on, that first frame is a lie.
    #[test]
    fn the_screen_switch_opens_on_the_screen_the_tube_starts_on() {
        let first = part("const SCREENS = [\n  {\n    id: '", "'");
        assert!(
            INDEX.contains(&format!(r#"title="which screen" hidden>{first}<"#)),
            "the switch does not open on `{first}`"
        );
    }
}
