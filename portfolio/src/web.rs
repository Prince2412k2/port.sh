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

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::crowd::{self, Crowd, Meter, Seat};
use tokio::sync::mpsc::unbounded_channel;

use crate::session;

/// Concurrent browser sessions. Same reasoning as the SSH ceiling: opening
/// sockets should not be a free way to grow the process without bound.
const MAX_SESSIONS: usize = 128;

/// And how many of those one address may hold.
///
/// This was missing, and a global ceiling on its own is not a limit: one
/// address could hold all hundred and twenty-eight and the box would be full
/// while being read by nobody.
///
/// Three rather than the ssh side's one. A browser is a thing people have two
/// of -- a tab left on the map and another in the chat is a real way to read
/// this -- and refusing the second is a bug from where the visitor is standing.
/// Not many more than that: each one is a `Shell` and a tile cache.
const PER_ADDRESS_SESSIONS: usize = 3;

/// How often one address may start a session, and ask for the page.
///
/// A different question from the one above, and neither answers the other.
/// Concurrency is what a visitor is holding and is given back when they leave;
/// a rate is how often they turn up and is not. Something that connects and
/// disconnects in a loop never holds two sessions and is still a load.
///
/// The page is its own reason: it is 62 KB of HTML, shaders included, and
/// serving it is the cheapest thing here to ask for and not the cheapest to
/// answer. Sixty a minute is far more than reading it takes and far less than
/// a loop costs.
const NEW_SESSIONS: usize = 12;
const PAGE_READS: usize = 60;
const WINDOW: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Web {
    sessions: Arc<AtomicUsize>,
    /// Who is here, by address.
    here: Arc<Crowd>,
    /// How fast they are arriving, by address.
    starting: Arc<Meter>,
    reading: Arc<Meter>,
}

pub async fn serve(addr: &str, port: u16) -> anyhow::Result<()> {
    let state = Web {
        sessions: Arc::new(AtomicUsize::new(0)),
        here: Arc::new(Crowd::default()),
        starting: Arc::new(Meter::new(NEW_SESSIONS, WINDOW)),
        reading: Arc::new(Meter::new(PAGE_READS, WINDOW)),
    };
    let app = Router::new()
        .route("/", get(page))
        .route("/ws", get(upgrade))
        .route(FONT_URL, get(font))
        .route("/vendor/v1/xterm.css", get(xterm_css))
        .route("/vendor/v1/xterm.js", get(xterm_js))
        .route("/vendor/v1/addon-fit.js", get(addon_fit_js))
        .route("/vendor/v1/addon-webgl.js", get(addon_webgl_js))
        .route("/vendor/v1/addon-canvas.js", get(addon_canvas_js))
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

/// The one font this page may not do without, and why it is served rather
/// than named.
///
/// The art here is 114,632 sextants -- U+1FB00..U+1FB3B, "Symbols for Legacy
/// Computing", Unicode 13 -- and 28,705 braille cells. DejaVu Sans Mono, which
/// this asked for first for a long time, has *none* of either: 0/60 sextants
/// and 0/256 braille. Every plate and the whole map were therefore drawn by
/// whatever font the browser went and found on its own, and the metrics of the
/// two do not agree. Noto Sans Symbols2, which is what it finds on Debian,
/// draws a sextant one full em wide with its ink only 0.333 em tall, into a
/// cell measured at 0.602 em from DejaVu. Too wide, so it bleeds into the cell
/// beside it; too short, so every row of the picture has a seam above and
/// below it. That is the "lines and boxes" this fixes.
///
/// Naming a better fallback cannot fix it, and was tried twice. Any fallback
/// is wrong, because the cell is measured from one font and the glyph comes
/// from another. It takes one family that has all of it, and Iosevka is the
/// one -- 60/60 sextants, 256/256 braille, 32/32 block elements, every last
/// glyph on the same 0.5 em advance, with the block elements landing on the
/// exact halves and thirds of the cell so they tile without a seam.
///
/// It is also cut to shape. Stock Iosevka sets a 0.5 em advance against a
/// 1.25 em line, so its cell is 1:2.50 where DejaVu's was 1:1.93 -- and every
/// plate in here was baked by chafa for the *old* one. Dropping the stock font
/// in fixed the glyphs and stretched every picture 29% tall, which is a
/// different bug wearing the first one's clothes. Matching the cell's width and
/// forgetting its height is the trap, and there is no fixing it from this side:
/// squashing rows with xterm's `lineHeight` would clip the very block glyphs
/// this is here to draw, and padding columns with `letterSpacing` would leave
/// them short of the cell they have to tile with. The cell's shape belongs to
/// the font. So the outlines and advances are scaled 1.2953x horizontally to a
/// 0.648 em advance, which puts the cell back at 1:1.929, and at 13px it
/// measures 8.42 x 16.25 against DejaVu-at-14's 8.43 x 16.30. Nothing else in
/// the layout had to move.
///
/// Subset to the 943 glyphs this app can emit, it is 22 KB, which is small
/// enough to carry. The terminal runtime is vendored here too: startup has no
/// third-party network dependency and every immutable asset shares one origin.
const FONT_URL: &str = "/iosevka.woff2";
const FONT: &[u8] = include_bytes!("../data/iosevka-portfolio.woff2");

async fn font() -> Response {
    immutable("font/woff2", FONT)
}

fn immutable(content_type: &'static str, body: &'static [u8]) -> Response {
    (
        [
            ("content-type", content_type),
            ("cache-control", "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response()
}

macro_rules! vendor_asset {
    ($handler:ident, $mime:literal, $path:literal) => {
        async fn $handler() -> Response {
            immutable($mime, include_bytes!($path))
        }
    };
}

vendor_asset!(xterm_css, "text/css; charset=utf-8", "../data/vendor/v1/xterm.css");
vendor_asset!(xterm_js, "text/javascript; charset=utf-8", "../data/vendor/v1/xterm.js");
vendor_asset!(addon_fit_js, "text/javascript; charset=utf-8", "../data/vendor/v1/addon-fit.js");
vendor_asset!(addon_webgl_js, "text/javascript; charset=utf-8", "../data/vendor/v1/addon-webgl.js");
vendor_asset!(addon_canvas_js, "text/javascript; charset=utf-8", "../data/vendor/v1/addon-canvas.js");

async fn page(
    State(state): State<Web>,
    ConnectInfo(socket): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    match crowd::key(client_ip(&headers, socket)) {
        Some(key) if !state.reading.allow(&key, Instant::now()) => {
            crate::visits::operational("warn", "web_page_refused", &key);
            too_many()
        }
        _ => (
            [("cache-control", "public, max-age=300, stale-while-revalidate=86400")],
            Html(INDEX),
        )
            .into_response(),
    }
}

/// What a visitor over their limit is told.
///
/// A number rather than a closed door: `Retry-After` is the difference between
/// a limit and a fault, and the one thing that lets a well-behaved client back
/// off instead of retrying into it. Plain text because whoever reads this is
/// either a script or somebody looking at a terminal.
fn too_many() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after", "60")],
        "too many requests from your address. try again in a minute.\n",
    )
        .into_response()
}

/// The visitor's address, preferring what a reverse proxy says over the socket.
///
/// Deployed behind nginx or Traefik the socket is the proxy, so every visitor
/// would look like they came from the same machine. `X-Forwarded-For` is the
/// first hop and is what the proxy was asked to pass along; it is trusted here
/// because the only thing in front of this is one we put there.
///
/// An address rather than a string, so a header carrying something that is not
/// one falls back to the socket instead of becoming a limit key of its own.
/// Every unparseable value being its own key is every unparseable value having
/// its own allowance.
fn client_ip(headers: &HeaderMap, socket: SocketAddr) -> IpAddr {
    let trusted_proxy = match socket.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    };
    if trusted_proxy {
        for h in ["x-forwarded-for", "x-real-ip"] {
            if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
                if let Some(first) = v.split(',').next() {
                    if let Ok(ip) = first.trim().parse() {
                        return ip;
                    }
                }
            }
        }
    }
    socket.ip()
}

async fn upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Web>,
    ConnectInfo(socket): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let ip = client_ip(&headers, socket);
    let key = crowd::key(ip);

    // Both checks happen out here, before the handshake, so a refusal is an
    // HTTP status a client can read rather than a socket that opens and shuts
    // with no reason given. Refusing inside `on_upgrade` -- which is what the
    // ceiling below used to do on its own -- looks identical to a crash from
    // the other end.
    if let Some(key) = &key {
        if !state.starting.allow(key, Instant::now()) {
            crate::visits::operational("warn", "web_session_refused", key);
            return too_many();
        }
    }
    let Some(seat) = Seat::take(&state.here, key.clone(), PER_ADDRESS_SESSIONS) else {
        crate::visits::operational("warn", "web_session_crowded", key.as_deref().unwrap_or(""));
        return too_many();
    };

    let who = crate::visits::Who {
        via: "web",
        // A browser has no username to offer and no key to be known by. The
        // client sends an id it keeps in localStorage; until that arrives this
        // visitor is simply new, which is the honest default.
        user: String::new(),
        id: String::new(),
        ip: ip.to_string(),
        client: headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string(),
    };
    ws.on_upgrade(move |socket| async move {
        // Held for the life of the session and given back on the way out,
        // whichever way that happens -- including a panic in the middle of it.
        let _seat = seat;
        if state.sessions.fetch_add(1, Ordering::SeqCst) >= MAX_SESSIONS {
            state.sessions.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        drive(socket, who).await;
        state.sessions.fetch_sub(1, Ordering::SeqCst);
    })
    .into_response()
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
    // One frame deep, matching ssh. See `session::FrameSink`.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let (in_tx, in_rx) = unbounded_channel::<session::In>();
    let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();

    // A socket accepting bytes is not a browser painting them. Prefix each
    // frame with an id and retain the output slot until xterm has parsed it and
    // the browser has crossed a paint boundary. This puts backpressure at the
    // real bottleneck instead of allowing a hidden queue inside xterm.
    let writer = tokio::spawn(async move {
        let mut id = 0u32;
        while let Some(frame) = out_rx.recv().await {
            id = id.wrapping_add(1);
            let mut message = Vec::with_capacity(frame.len() + 4);
            message.extend_from_slice(&id.to_be_bytes());
            message.extend_from_slice(&frame);
            if sink.send(Message::Binary(message.into())).await.is_err() {
                break;
            }
            loop {
                match ack_rx.recv().await {
                    Some(acked) if acked == id => break,
                    Some(_) => continue,
                    None => return,
                }
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
                Message::Text(t) => {
                    if let Some(id) = t.strip_prefix('a').and_then(|v| v.parse::<u32>().ok()) {
                        let _ = ack_tx.send(id);
                        Ok(())
                    } else {
                        match parse_text(&t) {
                            Some(message) => reader_tx.send(message),
                            None => Ok(()),
                        }
                    }
                }
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
            match session::run(out_tx, in_rx, cols, rows, who, session::Profile::Rich).await {
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

/// `b#rrggbb`, the colour the page is drawn on.
fn parse_ground(s: &str) -> Option<[u8; 3]> {
    let hex = s.strip_prefix("b#")?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let at = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([at(0)?, at(2)?, at(4)?])
}

/// The whole text channel: a size, the width of the client's own chrome, or
/// the colour it is drawing on.
///
/// Bytes typed at the terminal come in on the binary channel and nowhere else,
/// so anything arriving here is a statement about the window rather than input.
/// Anything that is none of the three is dropped rather than guessed at.
fn parse_text(s: &str) -> Option<session::In> {
    if let Some((c, r)) = parse_resize(s) {
        return Some(session::In::Resize(c, r));
    }
    if let Some(rgb) = parse_ground(s) {
        return Some(session::In::Ground(rgb));
    }
    let cols = s.strip_prefix('g')?.trim().parse().ok()?;
    Some(session::In::Gutter(cols))
}

/// The whole client. One HTML file, no build step and no runtime npm. The
/// pinned xterm distributions beside it are served as immutable local assets.
///
/// The shader switch in the corner is a real post-processing chain, not a stack
/// of CSS overlays: the terminal is rendered to a canvas, that canvas is
/// uploaded as a texture, and four programs turn it into a photograph of a
/// display -- a composite signal, a phosphor with a memory, a bloom, and then
/// the glass, the beam and the mask. The dial beside it says which one, and
/// there are nine: two colour tubes, a television, amber and green
/// monochrome, the same television fed off a tape, an early panel -- and two
/// that are not displays at all, a sheet of paper and a page of newsprint,
/// which run the whole thing backwards and take ink out of a page instead of
/// adding light to a screen.
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
/// are undefined once composited -- so turning a shader on switches xterm to its
/// canvas renderer, whose 2D canvas is always a valid texture source, and
/// turning it off switches back.
const INDEX: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Prince Patel</title>
<link rel="preload" href="/iosevka.woff2" as="font" type="font/woff2" crossorigin>
<link rel="preload" href="/vendor/v1/xterm.js" as="script">
<link rel="stylesheet" href="/vendor/v1/xterm.css">
<style>
  /* Everything the terminal draws comes out of this one file -- letters, box
     drawing, braille and the sextants every portrait is built from. One family
     for all of it is the point: see `FONT_URL` above for what happens when the
     cell is measured from one font and the picture is drawn with another.
     `swap` rather than `block`, because text in the wrong font for a moment is
     better than no text at all if this somehow does not arrive. */
  @font-face {
    font-family: "Iosevka Portfolio";
    src: url("/iosevka.woff2") format("woff2");
    font-weight: 400;
    font-style: normal;
    font-display: swap;
  }
  html, body {
    margin: 0; padding: 0; height: 100%;
    background: #08090b; overflow: hidden;
  }
  #term { position: absolute; inset: 0; }
  /* With a shader on, the terminal is still the thing being rendered and still
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

     A panel, not a row of words. Two controls and nothing else: a power
     button, and a knob that says which of the nine it is on. They were three
     bracketed words, which is the idiom the section rail uses two inches to
     the right -- and reading a word to find out what a control does is what a
     control with a shape does not make you do. The shapes are drawn rather
     than lettered, so they say what they are before they are read. */
  #chrome {
    position: absolute; top: 0; left: 0; height: 100%; z-index: 3;
    display: flex; flex-direction: column;
    justify-content: center; align-items: center; gap: 1.1em;
    /* Two columns in rather than hard against the edge. The tube bends the
       edges away from the viewer, and the leftmost thing on the screen is the
       first to go over the horizon. */
    padding: 0 1.4ch 0 2ch;
    font: 13px "Iosevka Portfolio", "DejaVu Sans Mono", "Menlo", ui-monospace, monospace;
    line-height: 1.2;
    /* The strip is as tall as the window so the controls can sit in the
       middle of it, which would otherwise make the whole left edge
       unclickable. */
    pointer-events: none;
  }
  #chrome > * { pointer-events: auto; }
  #chrome #power {
    background: transparent; border: 0; padding: 0; margin: 0; cursor: pointer;
    display: block; line-height: 0;
    /* Grown from the middle: these are round, and an off-centre origin walks
       them out from under the pointer. */
    transform-origin: center;
    transform: var(--bend, none);
    transition: transform .12s ease;
  }
  /* `[hidden]` first, and it has to be: the rule below sets `display`, and a
     stylesheet `display: flex` beats the user agent's `[hidden] { display:
     none }` -- so the knob stayed on screen with the shader off, drawn but
     not laid out. */
  #chrome #knob[hidden] { display: none; }
  #chrome #knob {
    display: flex; flex-direction: column; align-items: center; gap: .15em;
    cursor: pointer; transform-origin: center;
    transform: var(--bend, none);
    transition: transform .12s ease;
  }
  /* Bigger under the pointer, because at this size on the far left of a wide
     window these are small targets a long way from wherever the eye is. The
     size is the affordance: it says these are the things on this page that
     answer to a pointer, which nothing else here does. */
  #chrome #power:hover, #chrome #power:focus-visible,
  #chrome #knob:hover, #chrome #knob:focus-within {
    transform: var(--bend, none) scale(1.3);
  }
  /* While the layout is being read. See `measure`. */
  #chrome.measuring #power, #chrome.measuring #knob {
    transform: none; transition: none;
  }
  #chrome #knob-name {
    font-size: 9px; letter-spacing: .18em; text-transform: uppercase;
    color: #606670; transition: color .2s ease;
  }
  #chrome #knob:hover #knob-name { color: #c4c8ce; }
  /* With a shader on, these give up their paint and keep their clicks: the
     same two controls are drawn into the picture instead, so they arrive
     through the glass with everything else. */
  #chrome.shaded #power-face, #chrome.shaded #knob-face { opacity: 0; }
  #chrome.shaded #knob-name { color: transparent; }
  @media (prefers-reduced-motion: reduce) {
    /* The size and the colour still change -- those are the affordance, not
       the animation. Only the easing goes. */
    #hint, #chrome #power, #chrome #knob, #chrome #knob-name { transition: none; }
  }
</style>
</head>
<body>
<div id="term"></div>
<canvas id="glass"></canvas>
<div id="chrome">
  <button id="power" type="button" aria-pressed="false" title="shader">
    <canvas id="power-face" width="22" height="22"></canvas>
  </button>
  <div id="knob" hidden>
    <canvas id="knob-face" width="38" height="38"></canvas>
    <span id="knob-name">p22</span>
  </div>
</div>
<div id="hint">click to focus &middot; ctrl-f for full screen &middot; this is the same program you get over ssh</div>
<script src="/vendor/v1/xterm.js"></script>
<script src="/vendor/v1/addon-fit.js"></script>
<script src="/vendor/v1/addon-webgl.js"></script>
<script src="/vendor/v1/addon-canvas.js"></script>
<script>
// If the local terminal runtime did not arrive, every line below this throws
// on the first one. Replace the resulting black rectangle with an actionable
// failure instead; a broken or incomplete cached response is still possible.
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
    'font:13px "Iosevka Portfolio","DejaVu Sans Mono",ui-monospace,monospace;line-height:1.6';
  said.textContent =
    'the terminal emulator this page needs did not load.\n\n' +
    'its local terminal assets did not load. reload once to try again.\n\n' +
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
  fontFamily: '"Iosevka Portfolio", "DejaVu Sans Mono", "Menlo", ui-monospace, monospace',
  // 13 against DejaVu's 14, and the two come out on the same cell: 8.42 x
  // 16.25 px against 8.43 x 16.30. Matching the *width* alone was not enough
  // and is what stretched every picture tall -- see `FONT_URL`.
  fontSize: 13,
  theme: { background: '#08090b', foreground: '#c4c8ce' },
  // Braille and half-block glyphs are the whole renderer here, and letting
  // xterm draw them from the font rather than its own box-drawing shortcuts
  // is what keeps the map looking like the map.
  customGlyphs: false,
  scrollback: 0,
});
// The two grounds a shader can ask for.
//
// `dark` is the page's own, and the same `#08090b` the app calls night and the
// portraits were baked against. `paper` is the app's light page, and the ink
// on it is the app's own pen -- both taken from `termap::canvas`, so the parts
// of the frame the shader does not touch agree with the parts it does.
const GROUNDS = {
  dark: { bg: '#08090b', fg: '#c4c8ce' },
  paper: { bg: '#eeeae0', fg: '#22201e' },
};

const screen = document.getElementById('term');
const switches = document.getElementById('chrome');
const fit = new FitAddon.FitAddon();
term.open(screen);
term.loadAddon(fit);

// Keep the server-side framebuffer bounded on very large displays. Past this
// point more viewport becomes breathing room rather than more ANSI cells to
// diff, transmit, parse and paint on every full redraw.
const fitBounded = () => {
  const proposed = fit.proposeDimensions();
  if (!proposed) return;
  term.resize(Math.max(20, Math.min(180, proposed.cols)), Math.max(6, Math.min(60, proposed.rows)));
};

// Which renderer is loaded is the shader's business, so it is swappable rather
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
fitBounded();

// Measure the cell again once the real font is actually here.
//
// xterm works out how wide a character is the moment it opens, and with
// `font-display: swap` that can happen against the fallback -- which sets a
// cell a fifth wider than Iosevka's and so a column count that is wrong for
// the font that ends up drawing. Everything downstream is built on that
// number: how many columns the app is told it has, where the switches think
// they are, where a click lands.
//
// The round trip through a different size is not decoration. xterm only
// re-measures when an option it is watching actually changes value, so
// assigning the size it already holds does nothing at all.
if (document.fonts && document.fonts.load) {
  document.fonts.load('13px "Iosevka Portfolio"').then(() => {
    if (term.options.fontSize === 13) {
      term.options.fontSize = 12;
      term.options.fontSize = 13;
    }
    fitBounded();
    sendSize();
    measure();
  }).catch(() => { /* the fallback is already drawing; nothing to undo */ });
}

const proto = location.protocol === 'https:' ? 'wss' : 'ws';
const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)').matches;
const ws = new WebSocket(`${proto}://${location.host}/ws`);
ws.binaryType = 'arraybuffer';

const sendSize = () => {
  if (ws.readyState === WebSocket.OPEN) ws.send(`r${term.cols}x${term.rows}`);
};

// Said once at the start and again whenever it changes. A session that opens
// on a light shader has to arrive light, not flip a frame later.

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
// Tell the app what it is drawing on.
//
// The browser's half of the question a terminal answers with OSC 11. There is
// no terminal under this to ask -- the page is the terminal -- so it says so,
// and the app's system theme follows it exactly as it follows a real one.
let ground = null;
const setGround = (name) => {
  const g = GROUNDS[name] || GROUNDS.dark;
  if (ground === name) return;
  ground = name;
  document.body.style.background = g.bg;
  term.options.theme = { background: g.bg, foreground: g.fg };
  sendGround();
  // The renderer keeps the old colours until something asks it to draw again,
  // and a page nobody has typed on has nothing of its own to repaint.
  term.refresh(0, term.rows - 1);
};

const sendGround = () => {
  const g = GROUNDS[ground] || GROUNDS.dark;
  if (ws.readyState === WebSocket.OPEN) ws.send('b' + g.bg);
};

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
      it.el.style.removeProperty('--bend');
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
    // A custom property rather than `transform` itself. The hover below also
    // has something to say about this element's transform, and an inline
    // `transform` beats every rule in the stylesheet -- which is why the
    // first version of the hover did nothing at all. One owner each: the bend
    // is this, the size is CSS, and the two compose in the rule.
    it.el.style.setProperty('--bend', `translate(${dx.toFixed(2)}px, ${dy.toFixed(2)}px)`);
  }
};

/// Read the layout back, with the transforms off so it is the layout that is
/// read and not the last answer this gave.
///
/// A class rather than clearing `transform` on each element. Two things now
/// have a claim on it -- the bend and the hover -- and this has to ignore
/// both: an inline `none` left behind beats the rule that composes them, and
/// measuring a button while the pointer is on it reads it 35% too wide and
/// tells the app to keep that many columns clear.
const measure = () => {
  laidOut.length = 0;
  switches.classList.add('measuring');
  const kinds = { 'power-face': 'power', 'knob-face': 'knob', 'knob-name': 'label' };
  for (const el of switches.children) {
    if (el.hidden) continue;
    // A control is drawn from its own box, and the knob is two boxes -- the
    // face and the name under it -- which are painted and bent separately.
    const parts = el.children.length ? [...el.children] : [el];
    for (const part of parts) {
      const box = part.getBoundingClientRect();
      laidOut.push({
        el: part,
        x: box.left, y: box.top, w: box.width, h: box.height,
        kind: kinds[part.id] || 'label',
      });
    }
  }
  switches.classList.remove('measuring');
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
  sendGround();                     // before the first frame is drawn
  sendSize();                       // the session waits for this one
  measure();
  term.focus();
};
// Four-byte frame id followed by ANSI. Acknowledge only after xterm has parsed
// it and the browser reaches a paint boundary. The server keeps one frame in
// flight and coalesces all state changes that happen behind it.
let paintedFrames = 0;
let paintedBytes = 0;
let paintMs = 0;
ws.onmessage = (e) => {
  const packet = new Uint8Array(e.data);
  if (packet.byteLength < 4) return;
  const id = new DataView(packet.buffer, packet.byteOffset, 4).getUint32(0, false);
  const started = performance.now();
  term.write(packet.subarray(4), () => {
    requestAnimationFrame(() => {
      paintedFrames++;
      paintedBytes += packet.byteLength - 4;
      paintMs += performance.now() - started;
      if (ws.readyState === WebSocket.OPEN) ws.send('a' + id);
    });
  });
};
// Available without network logging or visitor data: `portfolioPerf()` in the
// console tells us whether xterm painting, rather than transport, is expensive.
window.portfolioPerf = () => ({
  frames: paintedFrames,
  ansiBytes: paintedBytes,
  averagePaintMs: paintedFrames ? paintMs / paintedFrames : 0,
  cols: term.cols,
  rows: term.rows,
  renderer: renderer ? renderer.constructor.name : 'dom',
});
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
    fitBounded();
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
//
// There is no button for it any more. It was a third bracketed word in a
// corner that is now two drawn controls, and a chrome that says `[full]` is
// chrome about the browser rather than about this program -- the key stays,
// and the hint under the terminal is where it is advertised.
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
  fitBounded();
  sendSize();
  tube.resize();
  measure();
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
#if ARRIVAL == 1
  // A page does not open. It is the same size the whole way through.
  float openX = 1.0;
  float openY = 1.0;
#else
  float openX = smoothstep(0.00, 0.42, warm);
  float openY = smoothstep(0.30, 1.00, warm);
#endif

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

#if INK
  // Ink spreads into the paper it is sitting on, and it spreads *darker*.
  //
  // Which is the whole difference between this and a tube, and why it is not
  // the bloom pass with a sign flipped. Bloom is light added to its
  // neighbours; a fibre pulls pigment sideways and takes brightness *out* of
  // them. So the blur is composited with a minimum -- wherever the neighbourhood
  // is darker than this pixel, some of that darkness arrives here -- and the
  // page can never come out brighter than the page.
  c = mix(c, min(c, lin(texture2D(glow, p).rgb)), BLEED);
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

  // Contrast and brightness, per style.
  //
  // Every filter here changes how much of the range the picture actually uses
  // -- a mask is a dimmer, a bleed is a smear, a halftone throws away
  // everything between black and white -- and the amount differs enough
  // between them that one setting for all of them is one setting wrong for
  // most. Around 0.18 rather than around 0.5: that is mid grey in linear
  // light, and pivoting around the arithmetic middle instead crushes the
  // shadows of anything dark and blows the highlights of anything bright.
  c = (c - 0.18) * CONTRAST + 0.18 + BRIGHTNESS;
  c = max(c, vec3(0.0));

  // Phosphor is never entirely off, and neither is the glass: a dead-black CRT
  // is the one thing a real one never manages.
  c += vec3(GLOW_R, GLOW_G, GLOW_B);

#if HALFTONE
  // A screentone: ink laid down as a grid of dots whose size is the density,
  // which is how a comic gets grey out of one colour of ink. Rotated, because
  // a screen square to the page reads as a screen and one at an angle reads as
  // tone -- the same reason every printer in the world puts it at 45 degrees.
  float ia = radians(TONE_ANGLE);
  vec2 tp = mat2(cos(ia), -sin(ia), sin(ia), cos(ia)) * gl_FragCoord.xy / (TONE_PITCH * dpr);
  // Ink density, as the page sees it: how far below the paper this pixel is.
  float dens = clamp(1.0 - dot(c, vec3(0.2126, 0.7152, 0.0722)), 0.0, 1.0);
  float dot2 = length(fract(tp) - 0.5) * 1.4142;
  // Radius from density, and a soft edge one pixel wide so the dots do not
  // crawl when the page moves under them.
  float ink = smoothstep(dot2 + 0.12, dot2 - 0.12, sqrt(dens));
  // Screen the flats, not the line.
  //
  // Which is what an inker does, and here it is not a style note but the
  // difference between a page and a mess. A screen laid over everything lands
  // on the letterforms too, and a 13px glyph is about four dots across -- the
  // first version of this was legible as texture and not as text.
  //
  // An edge is where this pixel and its neighbourhood disagree, and the blur
  // is already in hand from the bleed. Line art disagrees; a flat does not.
  float soft = dot(lin(texture2D(glow, p).rgb), vec3(0.2126, 0.7152, 0.0722));
  float edge = abs((1.0 - soft) - dens);
  float flat_ = 1.0 - smoothstep(0.02, 0.10, edge);
  // ...and only in the middle of the range. Solid ink stays solid and paper
  // stays paper; a screen on those turns the whole page grey.
  float toning = smoothstep(0.03, 0.20, dens) * smoothstep(0.98, 0.78, dens);
  c = mix(c, mix(vec3(PAPER_WHITE), vec3(PAPER_INK), ink), flat_ * toning * TONE_AMT);
#endif

#if FIBRE
  // Paper is not flat and not one colour. Two scales of it: a coarse cloud
  // that is the sheet, and a fine one that is the fibre in it.
  float f = hash(floor(gl_FragCoord.xy / (3.0 * dpr))) * 0.6
          + hash(gl_FragCoord.xy) * 0.4;
  c *= 1.0 - (f - 0.5) * FIBRE_AMT;
#endif

  c += (hash(gl_FragCoord.xy + fract(time) * 311.0) - 0.5) * GRAIN;

  // Arriving, or leaving.
  //
  // Not one animation for all of them. A tube collapses to a line and flashes
  // as the last of the charge on the deflection plates dumps into it -- which
  // is a fact about deflection plates, and a page of print has none. So the
  // style says how it comes and goes, and the geometry above already follows
  // it: openX and openY collapse the raster only where there is a raster
  // to collapse.
#if ARRIVAL == 1
  // Print: the ink develops onto a page that was already there. Nothing
  // collapses and nothing flashes -- the paper does not go anywhere, the
  // impression on it does.
  c = mix(vec3(1.0) * PAPER_WHITE, c, smoothstep(0.0, 1.0, warm));
#else
  c *= smoothstep(0.0, 0.55, warm);
  float shut = 1.0 - abs(warm * 2.0 - 1.0);
  float wire = exp(-pow((uv.y - 0.5) * 90.0, 2.0));
  c += vec3(0.85, 0.92, 1.0) * shut * shut * wire * 0.7;
#endif

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
  // Which ground the picture wants under it. A tube is a light source and
  // everything about it -- phosphor, bloom, the glow it never quite loses --
  // is addition to black, so all of these are dark and the ones that are not
  // say so.
  ground: 'dark',
  flags: {
    CURVE: 1, SCAN: 1, MASK: 1, MONO: 0, GHOSTING: 0, BLOOM: 1,
    FLICKER: 0, SHEEN: 1, BACKLIGHT: 0,
    // The paper half. `INK` swaps the bloom for a bleed, `FIBRE` gives the
    // sheet a surface, `HALFTONE` lays the ink down as a screen -- and
    // `ARRIVAL` says whether this thing collapses like a tube or develops
    // like a print.
    INK: 0, FIBRE: 0, HALFTONE: 0, ARRIVAL: 0,
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
    // Per style, because every filter here uses a different amount of the
    // range and one setting for all of them is one setting wrong for most.
    CONTRAST: 1.0, BRIGHTNESS: 0.0,
    BLEED: 0.0, FIBRE_AMT: 0.0,
    TONE_PITCH: 3.4, TONE_ANGLE: 45.0, TONE_AMT: 0.0,
    PAPER_WHITE: 1.0,
    PAPER_INK: 0.0,
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
  // The two that are not tubes at all.
  //
  // Everything above is a light source: the picture is what the phosphor adds
  // to a black screen, and every part of it -- bloom, halation, the glow it
  // never quite loses -- is addition. Paper is the other way round. The sheet
  // is already there and already bright, the ink is what is taken out of it,
  // and the app is told to draw dark on light before any of this runs -- see
  // `setGround`. Nothing below adds anything to the page.
  {
    id: 'paper',
    hint: 'ink on paper: a bleed into the fibre and no light of its own',
    ground: 'paper',
    flags: {
      CURVE: 0, SCAN: 0, MASK: 0, BLOOM: 0, SHEEN: 0,
      INK: 1, FIBRE: 1, ARRIVAL: 1,
    },
    nums: {
      // A whisper of a curve is still a curve. This is a flat sheet.
      ROUND: 0.004, VIGNETTE: 0.06,
      // No phosphor to keep glowing, and nothing for the glass to catch.
      GLOW_R: 0.0, GLOW_G: 0.0, GLOW_B: 0.0,
      BLEED: 0.34, FIBRE_AMT: 0.055, GRAIN: 0.004,
      // Print holds less range than a screen, and what it has it pushes
      // apart. Gently: the pivot is mid grey, so anything much above 1.1
      // takes the bright end of a photograph straight to paper white.
      CONTRAST: 1.10, BRIGHTNESS: -0.008,
    },
  },
  {
    id: 'comic',
    hint: 'one colour of ink and a screen to make grey out of it',
    ground: 'paper',
    flags: {
      CURVE: 0, SCAN: 0, MASK: 0, BLOOM: 0, SHEEN: 0,
      INK: 1, FIBRE: 1, HALFTONE: 1, ARRIVAL: 1,
    },
    nums: {
      ROUND: 0.004, VIGNETTE: 0.10,
      GLOW_R: 0.0, GLOW_G: 0.0, GLOW_B: 0.0,
      // Heavier than the plain sheet: newsprint takes more ink and holds it
      // worse, and the screen below needs something to bite into.
      BLEED: 0.46, FIBRE_AMT: 0.085, GRAIN: 0.006,
      TONE_PITCH: 3.2, TONE_ANGLE: 45.0, TONE_AMT: 0.85,
      // The screen throws away everything between paper and ink, so what is
      // left has to be pushed apart to survive it -- but 1.34 was measured
      // against text and bleached every photograph on the page.
      CONTRAST: 1.14, BRIGHTNESS: -0.014,
      PAPER_WHITE: 0.985,
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
  /// Seconds of black to hold after the picture has gone, before the terminal
  /// underneath is handed back. See the step function.
  ///
  /// Not `blank`: that name is taken, by the empty texture the ghosting pass
  /// reads on its first frame, and taking it made the tube composite a float
  /// where it wanted a texture.
  dark: 0,
  /// How long that hold should be, set by whoever threw the switch.
  darkFor: 0,
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
      // Not cached. A failure here used to be written into the cache as
      // `null`, which made it permanent: `program` returns the cached null
      // without retrying, `start` gives up, and the switch disables itself
      // and says "no webgl in this browser" -- which by then is not true and
      // stays untrue for the rest of the session. One miss while the canvas
      // still had no size was enough to do it.
      //
      // A shader that genuinely cannot compile fails again next time, at the
      // cost of one compile per attempt, and the switch reports it then.
      return null;
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
    const was = this.warm;
    if (Math.abs(this.warmTo - this.warm) <= step) this.warm = this.warmTo;
    else this.warm += Math.sign(this.warmTo - this.warm) * step;
    // The moment the picture finished going out.
    if (was !== 0 && this.warm === 0 && this.darkFor > 0) {
      this.dark = this.darkFor;
      this.darkFor = 0;
    }

    this.draw();

    if (this.settle > 0) this.settle--;
    // A screen that has just gone out is a dark screen, not the desktop.
    //
    // The picture collapses to a line and then to a point, and then -- with
    // nothing held here -- the plain terminal was back in the same frame, at
    // full brightness, which undoes the whole gesture: the set never looks
    // off, it looks like the effect stopped. So the glass stays up and black
    // for a beat afterwards, and only then hands back.
    if (this.dark > 0) {
      this.dark -= dt;
      this.wake();
      return;
    }
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
    const hot = power.matches(':hover, :focus-visible') || knob.matches(':hover, :focus-within');
    const ink = hot ? '#c4c8ce' : '#606670';
    for (const it of laidOut) {
      const x = it.x * scale, y = it.y * scale;
      const w = it.w * scale, h = it.h * scale;
      // The same two functions the corner canvases use, at the tube's scale.
      // Not a lettered stand-in: what comes through the glass has to be the
      // control, or the picture and the page disagree about what is there.
      if (it.kind === 'power') {
        drawPower(ctx, x + w / 2, y + h / 2, scale,
                  this.on ? '#ffb040' : ink);
      } else if (it.kind === 'knob') {
        const at = SCREENS.indexOf(screenById(this.screen));
        drawKnob(ctx, x + w / 2, y + h / 2, scale, at, SCREENS.length, ink, '#ffb040');
      } else {
        ctx.textBaseline = 'top';
        ctx.font = `${9 * scale}px "Iosevka Portfolio", "DejaVu Sans Mono", "Menlo", ui-monospace, monospace`;
        ctx.fillStyle = ink;
        // The label is letterspaced in CSS and canvas has no such property,
        // so it is placed a character at a time to the same rhythm.
        const text = it.el.textContent.toUpperCase();
        const step = w / Math.max(text.length, 1);
        for (let i = 0; i < text.length; i++) {
          ctx.fillText(text[i], x + i * step, y);
        }
      }
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
const power = document.getElementById('power');
const powerFace = document.getElementById('power-face');
const knob = document.getElementById('knob');
const knobFace = document.getElementById('knob-face');
const knobName = document.getElementById('knob-name');

// The two controls, as drawings.
//
// One function each, and each is called twice: once into the small canvas in
// the corner, and once -- at the tube's own scale -- into the picture, so what
// arrives through the glass is the same object and not a lettered stand-in for
// it. Everything is in CSS pixels and multiplied by `k`, which is the only
// thing that differs between the two calls.
const KNOB_R = 11;
const POWER_R = 7;
/// The sweep a knob turns through, and where it starts. Three quarters of a
/// circle with the gap at the bottom, which is where every panel knob has its
/// gap because that is where the shaft comes out.
const SWEEP_FROM = Math.PI * 0.75;
const SWEEP_TO = Math.PI * 2.25;

const knobAngle = (at, n) => SWEEP_FROM + (SWEEP_TO - SWEEP_FROM) * (n < 2 ? 0.5 : at / (n - 1));

const drawKnob = (ctx, cx, cy, k, at, n, ink, lit) => {
  ctx.save();
  ctx.translate(cx, cy);
  ctx.lineCap = 'butt';
  // A tick per position, so the knob says how many there are as well as which
  // one -- the thing a button that advances by one cannot say.
  for (let i = 0; i < n; i++) {
    const a = knobAngle(i, n);
    const r0 = (KNOB_R + 3) * k;
    const r1 = r0 + (i === at ? 4.5 : 2.5) * k;
    ctx.strokeStyle = i === at ? lit : ink;
    ctx.lineWidth = (i === at ? 1.6 : 1.0) * k;
    ctx.beginPath();
    ctx.moveTo(Math.cos(a) * r0, Math.sin(a) * r0);
    ctx.lineTo(Math.cos(a) * r1, Math.sin(a) * r1);
    ctx.stroke();
  }
  ctx.strokeStyle = ink;
  ctx.lineWidth = 1.2 * k;
  ctx.beginPath();
  ctx.arc(0, 0, KNOB_R * k, 0, Math.PI * 2);
  ctx.stroke();
  // The pointer, from the middle out to the rim.
  const a = knobAngle(at, n);
  ctx.strokeStyle = lit;
  ctx.lineWidth = 1.8 * k;
  ctx.lineCap = 'round';
  ctx.beginPath();
  ctx.moveTo(Math.cos(a) * KNOB_R * k * 0.18, Math.sin(a) * KNOB_R * k * 0.18);
  ctx.lineTo(Math.cos(a) * KNOB_R * k * 0.82, Math.sin(a) * KNOB_R * k * 0.82);
  ctx.stroke();
  ctx.restore();
};

const drawPower = (ctx, cx, cy, k, ink) => {
  ctx.save();
  ctx.translate(cx, cy);
  ctx.strokeStyle = ink;
  ctx.lineWidth = 1.6 * k;
  ctx.lineCap = 'round';
  // The IEC mark: a ring with a gap at the top and a stem through it. Drawn
  // rather than written, so it needs no language and no legend.
  const gap = 0.42;
  ctx.beginPath();
  ctx.arc(0, 0, POWER_R * k, -Math.PI / 2 + gap, -Math.PI / 2 - gap + Math.PI * 2);
  ctx.stroke();
  ctx.beginPath();
  ctx.moveTo(0, -(POWER_R + 2.4) * k);
  ctx.lineTo(0, -POWER_R * k * 0.2);
  ctx.stroke();
  ctx.restore();
};

/// Redraw the two little canvases in the corner.
///
/// The picture-side copies are painted by `tube.switches` from the same two
/// functions; this is the version you see when no shader is on.
const paintChrome = () => {
  const dpr = window.devicePixelRatio || 1;
  const fit = (c, w, h) => {
    if (c.width !== Math.round(w * dpr)) {
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
      c.style.width = w + 'px';
      c.style.height = h + 'px';
    }
    const g = c.getContext('2d');
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);
    return g;
  };
  const hot = power.matches(':hover, :focus-visible') || knob.matches(':hover, :focus-within');
  const ink = hot ? '#c4c8ce' : '#606670';
  const on = power.getAttribute('aria-pressed') === 'true';

  drawPower(fit(powerFace, 22, 22), 11, 11, 1, on ? '#ffb040' : ink);
  if (!knob.hidden) {
    const at = SCREENS.indexOf(screenById(tube.screen));
    drawKnob(fit(knobFace, 38, 38), 19, 19, 1, at, SCREENS.length, ink, '#ffb040');
  }
};

/// The power button, under the name the rest of this file already used for
/// the thing that switches the shader on.
const button = power;

const showScreen = () => {
  const s = screenById(tube.screen);
  const at = SCREENS.indexOf(s);
  // One tick per position, the one in use filled. Drawn in the terminal's own
  // font like everything else in this corner, so it arrives through the glass
  // with the rest of the picture rather than sitting on top of it.
  // `\u25aa` and `\u00b7`, and the choice is width and not taste. The obvious
  // pair for this is the diamonds at U+25C6/U+25C7, whose East Asian Width is
  // Ambiguous -- so a terminal that honours it draws each one two columns
  // wide, nine of them come to eighteen columns, and the gutter this asks the
  // app to keep clear went to 27. Both of these are width N.
  knobName.textContent = s.id;
  knob.title = `${s.id} \u2014 ${s.hint}`;
  paintChrome();
  // The shader decides the ground, and only while it is on: with nothing over
  // the terminal the page is its own dark self again.
  setGround(tube.on ? (s.ground || SCREEN_BASE.ground) : 'dark');
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
  try { localStorage.setItem('shader-screen', id); } catch (e) { /* private mode */ }
  if (tube.on) {
    // One frame is enough to change screens. The run of them is for a phosphor
    // that has to fade the old picture out, and only a screen with one needs it.
    tube.settle = tube.live().persist ? 48 : 0;
    tube.wake();
  }
};

const setShader = (on) => {
  if (on && !tube.gl && !tube.start()) {
    button.disabled = true;
    button.title = 'no webgl in this browser';
    return;
  }
  button.setAttribute('aria-pressed', on ? 'true' : 'false');
  knob.hidden = !on;
  // The switch appears with the tube and goes with it, so the room it needs
  // does too. `place` comes after the branch below rather than here, because
  // whether these are being bent at all is `tube.on`, and that has not moved
  // yet -- on the way out it does not move until the picture has finished
  // going.
  measure();
  try { localStorage.setItem('shader', on ? '1' : '0'); } catch (e) { /* private mode */ }
  if (on) {
    tube.on = true;
    setGround(tube.live().ground || SCREEN_BASE.ground);
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
    // Long enough to read as the set being off and not as a dropped frame.
    // Nothing to hold for a visitor who asked for less motion -- for them
    // there was no collapse to follow.
    tube.darkFor = reducedMotion ? 0 : 0.22;
    tube.after = () => {
      tube.on = false;
      setGround('dark');
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
switches.addEventListener('mouseover', () => { paintChrome(); tube.wake(); });
switches.addEventListener('mouseout', () => { paintChrome(); tube.wake(); });
switches.addEventListener('focusin', () => { paintChrome(); tube.wake(); });
switches.addEventListener('focusout', () => { paintChrome(); tube.wake(); });

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
    want = localStorage.getItem('shader');
    chosen = localStorage.getItem('shader-screen');
  } catch (e) { /* private mode */ }
  if (chosen && screenById(chosen).id === chosen) tube.screen = chosen;
  showScreen();
  button.addEventListener('click', () => {
    setShader(button.getAttribute('aria-pressed') !== 'true');
  });
  // A knob turns. Click steps one on, the wheel turns it either way, and the
  // arrow keys do the same for anyone not using a pointer.
  const turn = (by) => {
    const ids = SCREENS.map((s) => s.id);
    const at = (ids.indexOf(tube.screen) + by + ids.length) % ids.length;
    setScreen(ids[at]);
  };
  knob.addEventListener('click', () => turn(1));
  knob.addEventListener('wheel', (e) => {
    e.preventDefault();
    turn(e.deltaY > 0 ? 1 : -1);
  }, { passive: false });
  knob.tabIndex = 0;
  knob.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowRight' || e.key === 'ArrowUp' || e.key === ' ') turn(1);
    else if (e.key === 'ArrowLeft' || e.key === 'ArrowDown') turn(-1);
    else return;
    e.preventDefault();
  });
  if (want === '1' && !reducedMotion) setShader(true);
} else {
  button.disabled = true;
  knob.hidden = true;
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

    /// The font is here, and it is a font.
    #[test]
    fn the_font_this_serves_is_shipped_with_the_binary() {
        assert_eq!(&FONT[..4], b"wOF2", "the shipped font is not woff2");
        // 943 glyphs, of which 60 sextants and 256 braille cells. Anything
        // near empty means the file was replaced by something that is not it.
        assert!(
            FONT.len() > 20_000,
            "too small to be carrying the glyphs the art is made of: {} bytes",
            FONT.len()
        );
        assert!(INDEX.contains(FONT_URL), "the page never asks for the font this serves");
    }

    /// Every font stack on the page asks for the shipped font before it falls
    /// back to anything.
    ///
    /// There are four of them -- the chrome strip's CSS, the message shown when
    /// xterm never loaded, the terminal's own option, and the canvas that
    /// paints the switches over the shader -- and they have to agree. The last
    /// time they did not, the portraits came apart down every row: DejaVu Sans
    /// Mono has 0 of the 60 sextants and 0 of the 256 braille cells this draws
    /// with, so the cell was measured from one font and the picture drawn with
    /// whatever the browser found instead, an em wide against a cell 0.602 em
    /// wide and a third as tall as it needed to be.
    ///
    /// Falling back to DejaVu is still right -- text in the wrong font beats no
    /// text -- but it must never be what gets asked for first.
    #[test]
    fn every_font_stack_asks_for_the_shipped_font_first() {
        let stacks: Vec<&str> = INDEX
            .match_indices("\"DejaVu Sans Mono\"")
            .map(|(at, _)| &INDEX[at.saturating_sub(90)..at])
            .collect();
        assert!(
            stacks.len() >= 4,
            "the page has lost a font stack: found {} of the 4 that should be there",
            stacks.len()
        );
        for before in stacks {
            assert!(
                before.contains("Iosevka Portfolio"),
                "a stack falls through to DejaVu without asking for the shipped font: ...{before}"
            );
        }
    }

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

    /// The page says something when its terminal runtime does not arrive.
    /// Every line of the client is written against `Terminal` existing, so the
    /// check has to come before the first use.
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

    /// No shader source may contain a backtick.
    ///
    /// Every one of them is a JavaScript template literal, so a backtick in
    /// the GLSL -- including in a comment, which is where it happened -- ends
    /// the string early and the rest of the page is parsed as code. What that
    /// produces is `Uncaught SyntaxError: Unexpected identifier` pointing at a
    /// GLSL token several hundred lines below the quote that caused it.
    #[test]
    fn no_shader_source_carries_a_quote_that_would_end_it() {
        let names = ["VERT", "FRAG_PERSIST", "FRAG_BLUR", "FRAG_NTSC", "FRAG_TUBE"];
        for name in names {
            let open = INDEX
                .find(&format!("const {name} = `"))
                .unwrap_or_else(|| panic!("no shader called {name} any more"));
            let body = &INDEX[open + format!("const {name} = `").len()..];
            let close = body.find('`').expect("a shader that is never closed");
            let src = &body[..close];
            // If a stray backtick ended it early, what is left is a fragment
            // with no GLSL in it -- which is exactly the state the page was in.
            assert!(
                src.contains("gl_Position") || src.contains("gl_FragColor"),
                "{name} has no GLSL in it: a backtick inside it closed the string early"
            );
        }
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
        assert!(matches!(parse_text("b#eeeae0"), Some(session::In::Ground([238, 234, 224]))));
        assert!(matches!(parse_text("b#08090B"), Some(session::In::Ground([8, 9, 11]))));
        // Typed characters come in on the binary channel. Anything here that is
        // none of the three is dropped rather than guessed at -- a `g` is a
        // gutter, and `great` is not a gutter of any width.
        for junk in [
            "great", "g", "gx", "rm -rf /", "", "i-visitor", "m1",
            "b", "b#", "b#fff", "b#gggggg", "b#eeeae0f", "beeeae0",
        ] {
            assert!(parse_text(junk).is_none(), "`{junk}` was taken as a message");
        }
    }

    #[test]
    fn public_clients_cannot_spoof_forwarded_addresses() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.8".parse().unwrap());
        let public: SocketAddr = "203.0.113.7:1234".parse().unwrap();
        let proxy: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(client_ip(&headers, public).to_string(), "203.0.113.7");
        assert_eq!(client_ip(&headers, proxy).to_string(), "198.51.100.8");

        // A proxy we trust, saying something that is not an address. It falls
        // back to the socket rather than becoming a key nobody else shares --
        // which would be an allowance per made-up value.
        let mut junk = HeaderMap::new();
        junk.insert("x-forwarded-for", "not-an-address".parse().unwrap());
        assert_eq!(client_ip(&junk, proxy).to_string(), "127.0.0.1");
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
            INDEX.contains(&format!(r#"id="knob-name">{first}<"#)),
            "the knob does not open on `{first}`"
        );
        // ...and the knob is hidden until there is something to point it at.
        assert!(INDEX.contains(r#"<div id="knob" hidden>"#), "the knob ships visible");
    }
}
