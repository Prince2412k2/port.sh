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
    eprintln!("portfolio: web terminal on http://{bind}");
    // `into_make_service_with_connect_info` rather than `into_make_service`:
    // without it there is no peer address to record, and behind a proxy there
    // is none worth recording either -- see `client_ip`.
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
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
    let (cols, rows) = loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => {
                if let Some(id) = t.strip_prefix('i') {
                    who.id = sanitise_id(id);
                    continue;
                }
                if let Some(size) = parse_resize(&t) {
                    break size;
                }
            }
            // Binary before a size is input for a session that does not exist
            // yet. Dropped rather than buffered.
            Some(Ok(_)) => continue,
            _ => break (100, 30),
        }
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
        rt.block_on(async move {
            // Said out loud. A session that dies of an error used to end exactly
            // like one that was closed politely -- the socket shut and nothing
            // written anywhere -- which is how a resize that killed every
            // session went unnoticed.
            match session::run(out_tx, in_rx, cols, rows, who).await {
                Ok(()) => Some(()),
                Err(e) => {
                    eprintln!("portfolio: web session ended early: {e:#}");
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
        eprintln!("portfolio: web client did not take the closing frame");
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

/// The whole client. One file, no build step, no npm.
///
/// xterm.js comes from a CDN rather than being vendored, which is the one
/// outside dependency on this page -- swap it for a local copy if that matters.
///
/// The CRT switch in the corner is a real post-processing pass, not a stack of
/// CSS overlays: the terminal is rendered to a canvas, that canvas is uploaded
/// as a texture, and a fragment shader draws it back with tube curvature,
/// scanlines, a shadow mask, phosphor bloom and a little convergence error.
/// Doing it in a shader is what buys the curvature -- bending the image means
/// sampling it somewhere other than where the pixel is, and CSS cannot express
/// that.
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
     is given. */
  #chrome {
    position: absolute; top: 0; right: 0; z-index: 3;
    padding: .55rem .7rem; font: 12px ui-monospace, monospace;
  }
  #chrome button {
    background: transparent; color: #3a3e46; cursor: pointer;
    border: 1px solid #23262c; border-radius: 3px;
    padding: .18rem .5rem; font: inherit; letter-spacing: .06em;
    transition: color .2s ease, border-color .2s ease;
  }
  #chrome button + button { margin-left: .35rem; }
  #chrome button:hover { color: #60666f; border-color: #33373f; }
  #chrome button[aria-pressed="true"] { color: #ffb040; border-color: #6b4d1c; }
  #chrome button[disabled] { opacity: .35; cursor: default; }
</style>
</head>
<body>
<div id="term"></div>
<canvas id="glass"></canvas>
<div id="chrome">
  <button id="crt" type="button" aria-pressed="false" title="old tube">crt</button>
  <button id="full" type="button" aria-pressed="false" title="full screen (ctrl-f)">full</button>
</div>
<div id="hint">click to focus &middot; ctrl-f for full screen &middot; this is the same program you get over ssh</div>
<script src="https://cdn.jsdelivr.net/npm/xterm@5.3.0/lib/xterm.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-fit@0.8.0/lib/xterm-addon-fit.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-webgl@0.16.0/lib/xterm-addon-webgl.js"></script>
<script src="https://cdn.jsdelivr.net/npm/xterm-addon-canvas@0.5.0/lib/xterm-addon-canvas.js"></script>
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
const screen = document.getElementById('term');
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
const ws = new WebSocket(`${proto}://${location.host}/ws`);
ws.binaryType = 'arraybuffer';

const sendSize = () => {
  if (ws.readyState === WebSocket.OPEN) ws.send(`r${term.cols}x${term.rows}`);
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
  sendSize();                       // the session waits for this one
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
  resizeTimer = setTimeout(() => { fit.fit(); sendSize(); tube.resize(); }, 120);
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
// One full-screen quad, one texture, one fragment shader. The texture is
// whatever xterm's canvas renderer last drew; the shader is where it becomes a
// picture of a monitor rather than a monitor.
//
// It repaints when the terminal repaints and at no other time. A CRT wants a
// flicker and it is deliberately not here: driving one means running the GPU
// sixty times a second for as long as the tab is open, and an idle screen that
// warms somebody's laptop is the same mistake as an idle screen that streams
// 100 KB/s. Curvature, scanlines, mask and bloom are all static, and they are
// most of the look anyway.

const VERT = `
attribute vec2 pos;
varying vec2 uv;
void main() {
  uv = pos * 0.5 + 0.5;
  gl_Position = vec4(pos, 0.0, 1.0);
}
`;

const FRAG = `
precision highp float;

uniform sampler2D frame;
uniform vec2 res;

varying vec2 uv;

const float PI = 3.14159265;

// The glass. Pulls the corners out further than the edges, which is what makes
// a rectangle of text read as a curved surface rather than a scaled one.
vec2 bend(vec2 p) {
  p = p * 2.0 - 1.0;
  vec2 k = abs(p.yx) / vec2(7.0, 5.0);
  p += p * k * k;
  return p * 0.5 + 0.5;
}

// Three guns that never quite converge, and converge worse toward the rim.
//
// The coefficient is small because the text is small. At 0.008 the corners
// split by five or six device pixels, which is *most of a glyph* at a 14px
// font: the section rail and the footer stopped being readable, and a monitor
// effect that eats the navigation is a broken monitor. This is about half of
// what looks right on a still and exactly as much as the corners will carry.
vec3 guns(vec2 p) {
  vec2 off = (p - 0.5) * length(p - 0.5) * 0.0038;
  return vec3(
    texture2D(frame, p + off).r,
    texture2D(frame, p).g,
    texture2D(frame, p - off).b
  );
}

void main() {
  vec2 p = bend(uv);

  // Past the edge of the glass is the inside of the bezel, not more terminal.
  if (p.x < 0.0 || p.x > 1.0 || p.y < 0.0 || p.y > 1.0) {
    gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
    return;
  }

  vec3 c = guns(p);

  // Phosphor bloom. Added rather than mixed: a bright cell on a tube spills
  // light onto its neighbours, it does not blur into them.
  vec2 px = 1.5 / res;
  vec3 spill = guns(p + vec2(px.x, 0.0)) + guns(p - vec2(px.x, 0.0))
             + guns(p + vec2(0.0, px.y)) + guns(p - vec2(0.0, px.y));
  c += spill * 0.085;

  // Scanlines, one dark band per pair of device rows.
  c *= 0.86 + 0.14 * sin(p.y * res.y * PI);

  // The shadow mask: each device column leans toward one gun. Subtle on
  // purpose -- at three columns per triad this is a texture you notice at the
  // size of a letter, not a stripe you read as a defect.
  float m = mod(gl_FragCoord.x, 3.0);
  vec3 mask = vec3(0.93);
  if (m < 1.0) mask.r = 1.11;
  else if (m < 2.0) mask.g = 1.11;
  else mask.b = 1.11;
  c *= mask;

  // Falls off toward the corners, the way the gun does.
  //
  // Weighted low for the same reason the convergence error is: the four
  // corners of this app are the name, the section rail, the key hints and the
  // work index -- every piece of navigation it has. Measured off a real frame,
  // 0.6 here left the rail with 57% of its contrast and the corner hints with
  // 44%, which is a mood bought with the interface. At this weight they keep
  // 68% and 55%, and the falloff is still plainly there.
  vec2 v = p * (1.0 - p.yx);
  c *= mix(1.0, clamp(pow(v.x * v.y * 24.0, 0.22), 0.0, 1.0), 0.34);

  // Phosphor is never entirely off, and neither is the glass: a dead-black CRT
  // is the one thing a real one never manages.
  c += vec3(0.010, 0.011, 0.015);

  gl_FragColor = vec4(c, 1.0);
}
`;

const glass = document.getElementById('glass');
const button = document.getElementById('crt');

const tube = {
  on: false,
  gl: null,
  tex: null,
  res: null,
  scratch: null,
  sctx: null,
  pending: 0,

  // Compile once, on the first time somebody asks for it. Nobody who leaves
  // the switch alone pays for a GL context.
  start() {
    const gl = glass.getContext('webgl', { alpha: false, antialias: false });
    if (!gl) return false;

    const build = (type, src) => {
      const s = gl.createShader(type);
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
        throw new Error(gl.getShaderInfoLog(s) || 'shader would not compile');
      }
      return s;
    };

    let prog;
    try {
      prog = gl.createProgram();
      gl.attachShader(prog, build(gl.VERTEX_SHADER, VERT));
      gl.attachShader(prog, build(gl.FRAGMENT_SHADER, FRAG));
      gl.linkProgram(prog);
      if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) return false;
    } catch (e) {
      return false;
    }
    gl.useProgram(prog);

    const quad = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, quad);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]),
                  gl.STATIC_DRAW);
    const pos = gl.getAttribLocation(prog, 'pos');
    gl.enableVertexAttribArray(pos);
    gl.vertexAttribPointer(pos, 2, gl.FLOAT, false, 0, 0);

    this.tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    // A 2D canvas has its origin top-left and a texture has it bottom-left.
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, true);
    // Filtered, not nearest: once the image is bent it is sampled between
    // pixels everywhere, and nearest sampling makes every glyph edge crawl.
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

    this.scratch = document.createElement('canvas');
    this.sctx = this.scratch.getContext('2d', { alpha: false });
    this.res = gl.getUniformLocation(prog, 'res');
    this.gl = gl;
    return true;
  },

  resize() {
    if (!this.on || !this.gl) return;
    // Capped at 2: past that this is several million fragments a frame to
    // simulate a monitor that never had that many pixels.
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const w = Math.max(1, Math.round(glass.clientWidth * dpr));
    const h = Math.max(1, Math.round(glass.clientHeight * dpr));
    if (glass.width !== w || glass.height !== h) {
      glass.width = w;
      glass.height = h;
      this.scratch.width = w;
      this.scratch.height = h;
      this.gl.viewport(0, 0, w, h);
    }
    this.draw();
  },

  // Coalesced into one frame: a burst of writes is one repaint, not thirty.
  schedule() {
    if (!this.on || this.pending) return;
    this.pending = requestAnimationFrame(() => {
      this.pending = 0;
      this.draw();
    });
  },

  draw() {
    if (!this.on || !this.gl) return;
    const gl = this.gl;

    // xterm's canvas renderer stacks its layers as separate canvases in one
    // element, so they are flattened here in the order it draws them.
    this.sctx.fillStyle = '#08090b';
    this.sctx.fillRect(0, 0, this.scratch.width, this.scratch.height);
    const layers = screen.querySelectorAll('.xterm-screen canvas');
    for (let i = 0; i < layers.length; i++) {
      this.sctx.drawImage(layers[i], 0, 0, this.scratch.width, this.scratch.height);
    }

    gl.bindTexture(gl.TEXTURE_2D, this.tex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB, gl.RGB, gl.UNSIGNED_BYTE, this.scratch);
    gl.uniform2f(this.res, glass.width, glass.height);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  },
};

const setCrt = (on) => {
  if (on && !tube.gl && !tube.start()) {
    button.disabled = true;
    button.title = 'no webgl in this browser';
    return;
  }
  tube.on = on;
  useRenderer(on ? 'canvas' : 'webgl');
  screen.classList.toggle('shaded', on);
  glass.classList.toggle('on', on);
  button.setAttribute('aria-pressed', on ? 'true' : 'false');
  try { localStorage.setItem('crt', on ? '1' : '0'); } catch (e) { /* private mode */ }
  if (on) {
    tube.resize();
    // The renderer just changed under it, and a screen nobody has typed on
    // has nothing to repaint on its own.
    term.refresh(0, term.rows - 1);
    tube.schedule();
  }
  // Clicking the switch must not be a way to lose the keyboard.
  term.focus();
};

// Every repaint of the terminal is a repaint of the tube, and nothing else is.
term.onRender(() => tube.schedule());

if (window.CanvasAddon) {
  button.addEventListener('click', () => {
    setCrt(button.getAttribute('aria-pressed') !== 'true');
  });
  let want = null;
  try { want = localStorage.getItem('crt'); } catch (e) { /* private mode */ }
  if (want === '1') setCrt(true);
} else {
  button.disabled = true;
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
}
