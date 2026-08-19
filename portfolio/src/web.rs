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
</div>
<div id="hint">click to focus &middot; this is the same program you get over ssh</div>
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
  resizeTimer = setTimeout(() => { fit.fit(); sendSize(); tube.resize(); }, 120);
});

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
