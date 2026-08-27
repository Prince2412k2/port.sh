//! Animated diagrams: how each project actually works, shown rather than
//! described.
//!
//! A card carries an argument better than a paragraph does, but only if the
//! motion is the argument. Every scene here is driven by one clock and has no
//! state of its own — given `t` it draws the frame for `t`, so it can be
//! paused, snapshotted, resumed and diffed, and two runs at the same `t` are
//! the same picture. Nothing accumulates.
//!
//! The kit is small on purpose: boxes, rules, labels, and things that travel
//! along a path. Almost every mechanism worth showing in this portfolio is
//! something moving through something else and being stopped, transformed or
//! duplicated on the way.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use termap::canvas::Theme;

use crate::data::Project;

pub const INK: (u8, u8, u8) = (188, 194, 206);
pub const MUTE: (u8, u8, u8) = (108, 116, 132);
pub const FAINT: (u8, u8, u8) = (62, 68, 82);
pub const PASS: (u8, u8, u8) = (126, 200, 140);
pub const STOP: (u8, u8, u8) = (226, 110, 96);

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// A cell, if it is inside the frame.
fn put(buf: &mut Buffer, clip: Rect, x: i32, y: i32, ch: char, c: (u8, u8, u8), bold: bool) {
    if x < clip.x as i32
        || y < clip.y as i32
        || x >= (clip.x + clip.width) as i32
        || y >= (clip.y + clip.height) as i32
    {
        return;
    }
    if let Some(cell) = buf.cell_mut((x as u16, y as u16)) {
        let mut s = Style::default().fg(rgb(c));
        if bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        cell.set_char(ch).set_style(s);
    }
}

pub fn text(buf: &mut Buffer, clip: Rect, x: i32, y: i32, s: &str, c: (u8, u8, u8), bold: bool) {
    for (i, ch) in s.chars().enumerate() {
        put(buf, clip, x + i as i32, y, ch, c, bold);
    }
}

/// A light box with an optional name sitting in its top rule.
pub fn boxed(buf: &mut Buffer, clip: Rect, r: Rect, title: &str, c: (u8, u8, u8), heavy: bool) {
    let (tl, tr, bl, br, h, v) = if heavy {
        ('┏', '┓', '┗', '┛', '━', '┃')
    } else {
        ('╭', '╮', '╰', '╯', '─', '│')
    };
    let (x0, y0) = (r.x as i32, r.y as i32);
    let (x1, y1) = (x0 + r.width as i32 - 1, y0 + r.height as i32 - 1);
    for x in x0..=x1 {
        put(buf, clip, x, y0, h, c, false);
        put(buf, clip, x, y1, h, c, false);
    }
    for y in y0..=y1 {
        put(buf, clip, x0, y, v, c, false);
        put(buf, clip, x1, y, v, c, false);
    }
    put(buf, clip, x0, y0, tl, c, false);
    put(buf, clip, x1, y0, tr, c, false);
    put(buf, clip, x0, y1, bl, c, false);
    put(buf, clip, x1, y1, br, c, false);
    if !title.is_empty() {
        text(buf, clip, x0 + 2, y0, &format!(" {title} "), c, true);
    }
}

/// Things travelling down a vertical run, evenly spaced and looping.
///
/// `phase` shifts the whole train, so several runs can be made to look like one
/// continuous flow by handing them offsets of the same clock rather than by
/// tracking anything between frames.
#[allow(clippy::too_many_arguments)]
pub fn fall(
    buf: &mut Buffer,
    clip: Rect,
    x: i32,
    y0: i32,
    y1: i32,
    t: f64,
    phase: f64,
    n: usize,
    ch: char,
    c: (u8, u8, u8),
) {
    let span = (y1 - y0).max(1) as f64;
    for k in 0..n {
        let u = ((t + phase + k as f64 / n as f64) % 1.0).clamp(0.0, 1.0);
        put(buf, clip, x, y0 + (u * span) as i32, ch, c, true);
    }
}


/// Things travelling along a horizontal run.
#[allow(clippy::too_many_arguments)]
pub fn travel(
    buf: &mut Buffer,
    clip: Rect,
    x0: i32,
    x1: i32,
    y: i32,
    t: f64,
    phase: f64,
    n: usize,
    ch: char,
    c: (u8, u8, u8),
) {
    let span = (x1 - x0).max(1) as f64;
    for k in 0..n {
        let u = ((t + phase + k as f64 / n as f64) % 1.0).clamp(0.0, 1.0);
        put(buf, clip, x0 + (u * span) as i32, y, ch, c, true);
    }
}

/// A rule with an arrowhead, and an optional word sitting on it.
pub fn arrow(buf: &mut Buffer, clip: Rect, x0: i32, x1: i32, y: i32, label: &str, c: (u8, u8, u8)) {
    for x in x0..x1 {
        put(buf, clip, x, y, '─', c, false);
    }
    put(buf, clip, x1, y, '▶', c, false);
    if !label.is_empty() {
        let mid = x0 + (x1 - x0 - label.chars().count() as i32) / 2;
        text(buf, clip, mid, y, label, c, false);
    }
}

/// A filled bar. `frac` clamps, so a caller may overshoot without checking.
pub fn bar(buf: &mut Buffer, clip: Rect, x: i32, y: i32, w: i32, frac: f64, c: (u8, u8, u8)) {
    let n = (w as f64 * frac.clamp(0.0, 1.0)).round() as i32;
    for i in 0..w {
        put(buf, clip, x + i, y, if i < n { '▓' } else { '░' },
            if i < n { c } else { FAINT }, false);
    }
}

/// Text wrapped to a column, returning the rows it used.
///
/// Scenes lay themselves out by hand, which is fine for boxes and terrible for
/// sentences: a footnote written one word too long silently runs into the
/// column beside it, and the two halves of a diagram start overwriting each
/// other. Anything longer than a label goes through here.
pub fn note(
    buf: &mut Buffer,
    clip: Rect,
    x: i32,
    y: i32,
    w: usize,
    s: &str,
    c: (u8, u8, u8),
) -> i32 {
    let mut row = 0;
    let mut line = String::new();
    let mut flush = |line: &mut String, row: &mut i32| {
        if !line.is_empty() {
            text(buf, clip, x, y + *row, line, c, false);
            *row += 1;
            line.clear();
        }
    };
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > w {
            flush(&mut line, &mut row);
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    flush(&mut line, &mut row);
    row
}

/// A row of little columns, for anything that varies over a window.
pub fn spark(buf: &mut Buffer, clip: Rect, x: i32, y: i32, vals: &[f64], c: (u8, u8, u8)) {
    const RAMP: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    for (i, v) in vals.iter().enumerate() {
        let k = ((v.clamp(0.0, 1.0)) * 7.0).round() as usize;
        put(buf, clip, x + i as i32, y, RAMP[k], c, false);
    }
}

/// Draw a project's diagram. Returns false when it has none yet, so the caller
/// can fall back to prose rather than leaving a hole.
pub fn draw(buf: &mut Buffer, area: Rect, p: &Project, t: f64, th: Theme) -> bool {
    let f: fn(&mut Buffer, Rect, f64) = match p.id.as_str() {
        "netjail" => netjail,
        "watch-party" => watch_party,
        "logify" => logify,
        "clippy" => clippy,
        "stylized-maps" => stylized_maps,
        "termap" => termap,
        "noter" => noter,
        "harbr" => harbr,
        "gitswitch" => gitswitch,
        "vcs" => vcs,
        _ => return false,
    };
    f(buf, area, t);
    // Every colour in here is a literal chosen against black; this is where
    // they are turned round for a light page. One sweep of the rect rather
    // than a theme threaded through forty-four `put` calls.
    termap::canvas::recast_region(buf, area, th);
    true
}

/// How much room a diagram needs, in cells.
///
/// Scenes are laid out by hand at a fixed size, so the layout has to be able to
/// ask rather than guess. Without this the stage is sized by a rule of thumb and
/// the wider diagrams are quietly cropped at the edge — which looks like a
/// rendering bug and is really a layout one.
pub fn footprint(id: &str) -> (u16, u16) {
    // Measured, not estimated — `cargo test measure_footprints -- --ignored`
    // reports what each scene actually covers, and a test asserts these are
    // still right. Guessed numbers here are how a diagram ends up cropped.
    match id {
        "netjail" => (80, 29),
        "watch-party" => (74, 27),
        "logify" => (66, 19),
        "clippy" => (62, 16),
        "stylized-maps" => (67, 25),
        "termap" => (57, 19),
        "noter" => (58, 17),
        "gitswitch" => (55, 17),
        "harbr" => (78, 26),
        "vcs" => (56, 17),
        _ => (0, 0),
    }
}

/// Top-left of a diagram of `h` rows, centred in whatever it was given.
fn origin(a: Rect, w: u16, h: u16) -> (i32, i32) {
    (
        a.x as i32 + (a.width.saturating_sub(w) / 2) as i32,
        a.y as i32 + (a.height.saturating_sub(h) / 2) as i32,
    )
}

/// netjail: two halves that answer two different questions — what can this
/// process reach, and what did it try to reach.
///
/// The containment half is drawn as a stack because that is what it is: five
/// independent layers, the last three of which are on the host, so tearing down
/// the namespace's own firewall does not open anything. The observability half
/// matters just as much and is usually the part a sandbox gets wrong — a log
/// that records enough to be useful usually records enough to be a leak. Here
/// the redaction is structural, and the diagram says which fields cannot exist
/// rather than which ones are filtered.
fn netjail(buf: &mut Buffer, a: Rect, t: f64) {
    // Two columns with a gutter between them. Every string below is written to
    // fit one of these, and anything sentence-length goes through `note`.
    const L: usize = 40;
    const R: usize = 36;
    const GUTTER: i32 = 4;
    const W: u16 = (L + R) as u16 + GUTTER as u16;
    const H: u16 = 30;
    let (x, y) = origin(a, W, H);
    let ox = x + L as i32 + GUTTER;

    text(buf, a, x, y, "ISOLATION", MUTE, true);
    text(buf, a, ox, y, "OBSERVABILITY", MUTE, true);

    // ── left: one wire out, and five layers behind it ────────────────────
    boxed(buf, a, Rect { x: x as u16, y: (y + 2) as u16, width: 30, height: 3 },
          "your process", INK, false);
    text(buf, a, x + 2, y + 3, "pytest · npm · an agent", MUTE, false);

    for k in 0..2 {
        put(buf, a, x + 15, y + 5 + k, '│', FAINT, false);
    }
    fall(buf, a, x + 15, y + 5, y + 7, t * 0.9, 0.0, 2, '•', INK);
    text(buf, a, x + 18, y + 5, "veth — the only wire", FAINT, false);

    boxed(buf, a, Rect { x: x as u16, y: (y + 7) as u16, width: L as u16, height: 5 },
          "network namespace", INK, true);
    // Which layer is lit walks, so all five get read rather than one being the
    // permanent subject.
    let lit = ((t * 0.45) % 1.0 * 5.0) as usize;
    for (i, (id, what, drop)) in [
        ("L1", "nft output chain · policy drop", true),
        ("L2", ":80 :443 :53 → the proxy", false),
    ]
    .iter()
    .enumerate()
    {
        let ly = y + 9 + i as i32;
        let on = lit == i;
        text(buf, a, x + 2, ly, id, if on { INK } else { FAINT }, on);
        text(buf, a, x + 5, ly, what, if on { INK } else { MUTE }, false);
        put(buf, a, x + L as i32 - 3, ly, if *drop { '✗' } else { '→' },
            if *drop { STOP } else { PASS }, on);
    }

    text(buf, a, x, y + 13, "on the host — outside the namespace", FAINT, false);
    for (i, (id, what)) in [
        ("L3", "forwarding = 0 on the veth"),
        ("L4", "host_guard drops forwards"),
        ("L5", "no masquerade for the subnet"),
    ]
    .iter()
    .enumerate()
    {
        let ly = y + 14 + i as i32;
        let on = lit == i + 2;
        text(buf, a, x + 2, ly, id, if on { INK } else { FAINT }, on);
        text(buf, a, x + 5, ly, what, if on { INK } else { MUTE }, false);
        put(buf, a, x + L as i32 - 3, ly, '✗', STOP, on);
    }
    note(buf, a, x, y + 18, L, "so tearing down the namespace's own firewall opens nothing", FAINT);

    boxed(buf, a, Rect { x: x as u16, y: (y + 21) as u16, width: L as u16, height: 5 },
          "filtering proxy", INK, false);
    let beat = (t * 0.55) % 1.0;
    for (i, (line, ok)) in [("GET  example.com/v1/models", true),
                            ("POST example.com/v1/keys", false)].iter().enumerate()
    {
        let ry = y + 23 + i as i32;
        let live = (beat * 2.0) as usize == i;
        text(buf, a, x + 2, ry, line, if live { INK } else { FAINT }, false);
        if live {
            put(buf, a, x + L as i32 - 3, ry, if *ok { '✓' } else { '✗' },
                if *ok { PASS } else { STOP }, true);
        }
    }
    note(buf, a, x, y + 27, L, "same host, different paths — a host allowlist cannot express this at all", FAINT);

    // ── right: what the run may remember about itself ────────────────────
    boxed(buf, a, Rect { x: ox as u16, y: (y + 2) as u16, width: R as u16, height: 7 },
          "events", INK, false);
    let evs = [
        ("api.example.com", "/v1/models", true),
        ("registry.npmjs.org", "/lodash", true),
        ("api.example.com", "/v1/keys", false),
        ("t.example.net", "/collect", false),
    ];
    let head = ((t * 0.6) as usize) % evs.len();
    for r in 0..4 {
        let (host, path, ok) = evs[(head + r) % evs.len()];
        let ry = y + 4 + r as i32;
        let fresh = r == 0;
        text(buf, a, ox + 2, ry, host, if fresh { INK } else { MUTE }, false);
        text(buf, a, ox + 21, ry, path, if fresh { MUTE } else { FAINT }, false);
        put(buf, a, ox + R as i32 - 3, ry, if ok { '✓' } else { '✗' },
            if ok { PASS } else { STOP }, fresh);
    }

    put(buf, a, ox + 15, y + 9, '│', FAINT, false);
    fall(buf, a, ox + 15, y + 9, y + 11, t * 1.1, 0.0, 1, '•', PASS);
    text(buf, a, ox + 18, y + 9, "a unix socket", FAINT, false);
    boxed(buf, a, Rect { x: ox as u16, y: (y + 11) as u16, width: R as u16, height: 3 },
          "netjail logs", INK, false);
    text(buf, a, ox + 2, y + 12, "backlog, then every new event", MUTE, false);
    note(buf, a, ox, y + 15, R, "not a tail, so rotation cannot make it miss anything", FAINT);

    text(buf, a, ox, y + 18, "recorded", MUTE, true);
    text(buf, a, ox + 18, y + 18, "cannot exist", MUTE, true);
    for (i, (keep, drop)) in [
        ("host", "query params"),
        ("path", "bodies"),
        ("verdict", "any header"),
        ("when", "cookies · keys"),
    ]
    .iter()
    .enumerate()
    {
        text(buf, a, ox + 1, y + 19 + i as i32, keep, PASS, false);
        text(buf, a, ox + 18, y + 19 + i as i32, drop, STOP, false);
    }
    note(buf, a, ox, y + 24, R,
         "internal/events imports nothing from net/http — no type in it can hold a header", FAINT);

    text(buf, a, ox, y + 27, "$XDG_RUNTIME_DIR", MUTE, false);
    text(buf, a, ox + 18, y + 27, "0600 · tmpfs", PASS, false);
    text(buf, a, ox, y + 28, "unset?", MUTE, false);
    text(buf, a, ox + 18, y + 28, "refuse to start", STOP, true);
}

/// watch-party: two clients that are not the same program, a cache that decides
/// whether a seek is cheap, and a second real-time system running beside the
/// first.
///
/// Three things are worth showing at once and none of them make sense alone. The
/// clients are a web app and a native desktop build, so the decision core exists
/// twice and something has to hold the copies equal. The sync loop is only as
/// good as what is already buffered, so the cache is part of the mechanism
/// rather than an implementation detail. And the voice channel is a whole second
/// clock with its own jitter buffer, sitting next to the one being carefully
/// steered — keeping them from fighting was most of the work.
fn watch_party(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 74;
    const H: u16 = 28;
    let (x, y) = origin(a, W, H);
    let cycle = (t * 0.16) % 1.0;

    // ── the two clients, and the thing that keeps them honest ────────────
    boxed(buf, a, Rect { x: x as u16, y: y as u16, width: 30, height: 4 }, "web", INK, false);
    text(buf, a, x + 2, y + 1, "browser · JavaScript", MUTE, false);
    text(buf, a, x + 2, y + 2, "decision core", PASS, false);

    boxed(buf, a, Rect { x: (x + 40) as u16, y: y as u16, width: 34, height: 4 },
          "desktop", INK, false);
    text(buf, a, x + 42, y + 1, "Flutter · macOS Windows Linux", MUTE, false);
    text(buf, a, x + 42, y + 2, "the same core, in Dart", PASS, false);

    let blink = ((t * 0.7) % 1.0) < 0.5;
    text(buf, a, x + 30, y + 2, if blink { "◀──▶" } else { "◀╌╌▶" }, FAINT, false);
    note(buf, a, x + 12, y + 4, 50, "generated conformance vectors run in both CIs", FAINT);

    // ── the playheads ────────────────────────────────────────────────────
    let gap = (1.0 - cycle).powf(1.6) * 16.0;
    for (i, (name, off)) in [("web", 0.0), ("desktop", gap)].iter().enumerate() {
        let ty = y + 6 + i as i32;
        text(buf, a, x, ty, name, INK, true);
        put(buf, a, x + 9, ty, '├', FAINT, false);
        for k in 1..46 {
            put(buf, a, x + 9 + k, ty, '─', FAINT, false);
        }
        put(buf, a, x + 55, ty, '┤', FAINT, false);
        let head = 9 + 8 + (cycle * 26.0) as i32 + *off as i32;
        put(buf, a, x + head, ty, '●', if i == 0 { INK } else { PASS }, true);
    }
    text(buf, a, x + 58, y + 6, &format!("{:>3} ms", (gap * 7.0) as i32),
         if gap < 3.0 { PASS } else { MUTE }, gap < 3.0);
    text(buf, a, x + 58, y + 7, "apart", FAINT, false);

    // ── the cache, which is what makes a seek cheap or ruinous ───────────
    text(buf, a, x, y + 9, "segments", MUTE, false);
    for k in 0..24i32 {
        let held = (k as f64) < 6.0 + cycle * 14.0;
        let playing = k == (2.0 + cycle * 14.0) as i32;
        put(buf, a, x + 10 + k, y + 9,
            if playing { '▮' } else if held { '▪' } else { '·' },
            if playing { PASS } else if held { INK } else { FAINT }, playing);
    }
    text(buf, a, x + 36, y + 9, "buffered ahead", FAINT, false);
    text(buf, a, x, y + 10, "seek", MUTE, false);
    let cheap = cycle > 0.4;
    text(buf, a, x + 10, y + 10,
         if cheap { "inside the buffer — free" } else { "outside it — costs a fetch" },
         if cheap { PASS } else { STOP }, true);
    text(buf, a, x, y + 11, "so a seek is", MUTE, false);
    text(buf, a, x + 14, y + 11, "planned as a rendezvous, never a chase", INK, false);

    // ── the estimator and the gate ───────────────────────────────────────
    text(buf, a, x, y + 13, "offset samples", MUTE, false);
    for k in 0..16i32 {
        let phase = (t * 1.4 + k as f64 * 0.37) % 1.0;
        let outlier = (k as usize * 7 + (t * 0.5) as usize).is_multiple_of(9);
        let v = if outlier { 1.0 } else { 0.35 + 0.25 * phase };
        spark(buf, a, x + 18 + k, y + 13, &[v], if outlier { STOP } else { INK });
    }
    text(buf, a, x + 36, y + 13, "outliers rejected · Theil–Sen skew", FAINT, false);

    let certain = cycle > 0.28;
    text(buf, a, x, y + 14, "certainty gate", MUTE, false);
    text(buf, a, x + 18, y + 14,
         if certain { "open — correcting" } else { "shut — not enough signal to act on" },
         if certain { PASS } else { STOP }, true);
    text(buf, a, x, y + 15, "playback rate", MUTE, false);
    let rate = if certain { 1.0 + 0.004 * (1.0 - cycle) } else { 1.0 };
    text(buf, a, x + 18, y + 15, &format!("{rate:.4}×"), INK, false);
    bar(buf, a, x + 27, y + 15, 16, if certain { (1.0 - cycle) * 0.9 } else { 0.0 }, PASS);

    // ── the other real-time system ───────────────────────────────────────
    let lk = Rect { x: x as u16, y: (y + 17) as u16, width: 52, height: 5 };
    boxed(buf, a, lk, "LiveKit voice", INK, false);
    text(buf, a, x + 2, y + 19, "its own clock, its own jitter buffer", MUTE, false);
    let wave: Vec<f64> = (0..18)
        .map(|k| ((t * 5.0 + k as f64 * 0.8).sin() * 0.5 + 0.5).abs())
        .collect();
    spark(buf, a, x + 40, y + 19, &wave, PASS);
    text(buf, a, x + 2, y + 20, "running beside the loop, not inside it", FAINT, false);

    text(buf, a, x, y + 23, "drift", MUTE, false);
    let vals: Vec<f64> = (0..26)
        .map(|k| {
            let u = (cycle + k as f64 / 26.0) % 1.0;
            (1.0 - u).powf(1.6) * 0.9
        })
        .collect();
    spark(buf, a, x + 18, y + 23, &vals, INK);
    text(buf, a, x + 48, y + 23, "SLO 120 ms p95", FAINT, false);
    note(buf, a, x, y + 25, W as usize,
        "drift and rate-flutter are numeric targets, asserted by an eleven-suite harness", FAINT);
}

/// logify: log lines leave containers, cross a gateway that knows what this key
/// is allowed to see, and arrive in a terminal.
fn logify(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 66;
    const H: u16 = 18;
    let (x, y) = origin(a, W, H);
    text(buf, a, x, y, "docker host", MUTE, false);
    text(buf, a, x + 48, y, "your terminal", MUTE, false);

    let names = ["api", "worker", "db"];
    let allowed = [true, true, false];
    for (i, (name, ok)) in names.iter().zip(allowed).enumerate() {
        let cy = y + 2 + i as i32 * 4;
        boxed(buf, a, Rect { x: (x) as u16, y: cy as u16, width: 16, height: 3 }, name, INK, false);
        // Lines leaving each container. The denied one still produces them —
        // the point is that the gateway is what stops them, not the container.
        travel(buf, a, x + 17, x + 26, cy + 1, t * 0.8, i as f64 * 0.33, 2, '▪',
               if ok { INK } else { STOP });
        if !ok {
            put(buf, a, x + 27, cy + 1, '✗', STOP, true);
        }
    }

    let gw = Rect { x: (x + 28) as u16, y: (y + 5) as u16, width: 14, height: 5 };
    boxed(buf, a, gw, "gateway", INK, true);
    text(buf, a, x + 30, y + 7, "REST + ws", MUTE, false);
    text(buf, a, x + 30, y + 8, "key scope", PASS, false);

    arrow(buf, a, x + 43, x + 47, y + 7, "", FAINT);
    travel(buf, a, x + 43, x + 47, y + 7, t * 0.9, 0.0, 1, '▪', PASS);

    let term = Rect { x: (x + 48) as u16, y: (y + 2) as u16, width: 18, height: 11 };
    boxed(buf, a, term, "logify", INK, false);
    // A scrolling tail. The list moves because logs move.
    let lines = ["GET /v1 200", "worker tick", "GET /v1 200", "warn: retry", "GET /v1 404", "worker done"];
    let head = ((t * 0.7) as usize) % lines.len();
    for r in 0..7 {
        let l = lines[(head + r) % lines.len()];
        text(buf, a, x + 50, y + 4 + r as i32, l, if r == 0 { INK } else { MUTE }, r == 0);
    }

    let r = note(buf, a, x, y + 15, W as usize,
        "the key can see api and worker; db is refused at the gateway, not at the container", FAINT);
    note(buf, a, x, y + 15 + r, W as usize,
        "one static binary, one-line installers, and three reverse proxies documented", FAINT);
}

/// clippy: a copy crosses the tailnet to one chosen machine, and does not come
/// back.
fn clippy(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 62;
    const H: u16 = 16;
    let (x, y) = origin(a, W, H);
    text(buf, a, x + 20, y, "your tailnet — the trust boundary", MUTE, false);

    let boxes = [("laptop", 0), ("desktop", 24), ("phone", 48)];
    for (name, dx) in boxes {
        boxed(buf, a, Rect { x: (x + dx) as u16, y: (y + 2) as u16, width: 14, height: 4 },
              name, INK, false);
    }
    text(buf, a, x + 2, y + 4, "copy", PASS, true);
    text(buf, a, x + 26, y + 4, "paste", INK, true);
    text(buf, a, x + 50, y + 4, "webapp", MUTE, false);

    // One direction only. The return trip is drawn and struck out, because the
    // absence of an echo is the invariant and an absence cannot be seen.
    arrow(buf, a, x + 14, x + 23, y + 4, "", FAINT);
    travel(buf, a, x + 14, x + 23, y + 4, t * 0.55, 0.0, 1, '●', PASS);

    for k in x + 15..x + 23 {
        put(buf, a, k, y + 7, '─', FAINT, false);
    }
    put(buf, a, x + 14, y + 7, '◀', FAINT, false);
    put(buf, a, x + 19, y + 7, '✗', STOP, true);
    text(buf, a, x + 25, y + 7, "echo suppressed — no A→B→A loop", STOP, false);

    text(buf, a, x, y + 10, "bound to the tailscale interface", MUTE, false);
    text(buf, a, x + 34, y + 10, "never 0.0.0.0", PASS, true);
    text(buf, a, x, y + 11, "identical repeats", MUTE, false);
    let dupe = ((t * 0.5) % 1.0) > 0.5;
    text(buf, a, x + 34, y + 11,
         if dupe { "deduped by content hash" } else { "sent once" },
         if dupe { STOP } else { PASS }, true);

    note(buf, a, x, y + 14, W as usize,
        "no application-level auth: a second home-made boundary would be the weaker one", FAINT);
}

/// stylized-maps: a heavy pipeline at the top, and the thing it exists to
/// produce at the bottom.
///
/// The build is the part that had to be engineered — it does not fit in the
/// machine's spare capacity, so it has to survive being stopped — but the build
/// is not the point. The point is the styling: a map is a set of decisions about
/// what to leave out and how to shade what remains, and those decisions are the
/// only reason to run a custom profile rather than the stock one.
fn stylized_maps(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 70;
    const H: u16 = 26;
    let (x, y) = origin(a, W, H);
    let cycle = (t * 0.1) % 1.0;

    text(buf, a, x, y, "PIPELINE", MUTE, true);

    let stages = ["osm.pbf", "planetiler", "tiles", "API", "client"];
    let paused = (0.55..0.75).contains(&cycle);
    let done = if cycle < 0.55 { cycle / 0.55 } else { (cycle - 0.2) / 0.8 };
    let mut cx = x;
    for (i, s) in stages.iter().enumerate() {
        let w = s.chars().count() as i32 + 4;
        let active = (done * stages.len() as f64) as usize == i;
        boxed(buf, a, Rect { x: cx as u16, y: (y + 2) as u16, width: w as u16, height: 3 },
              "", if active { INK } else { FAINT }, active);
        text(buf, a, cx + 2, y + 3, s, if active { INK } else { MUTE }, active);
        if i + 1 < stages.len() {
            arrow(buf, a, cx + w, cx + w + 2, y + 3, "", FAINT);
        }
        cx += w + 3;
    }
    text(buf, a, x, y + 6, "custom Java profile", MUTE, false);
    text(buf, a, x + 22, y + 6, "the stock one drops admin boundaries", FAINT, false);
    text(buf, a, x, y + 7, "normalized schema", MUTE, false);
    text(buf, a, x + 22, y + 7, "versioned — a rerun cannot silently change it", FAINT, false);

    text(buf, a, x, y + 9, "build", MUTE, false);
    bar(buf, a, x + 8, y + 9, 34, done, if paused { MUTE } else { PASS });
    text(buf, a, x + 44, y + 9, &format!("{:>3.0}%", done * 100.0), INK, false);
    text(buf, a, x + 50, y + 9, if paused { "▮▮ paused" } else { "▶ running" },
         if paused { STOP } else { PASS }, true);
    text(buf, a, x, y + 10, "budget", MUTE, false);
    for (i, m) in ["auto", "full", "manual"].iter().enumerate() {
        let on = i == 0;
        text(buf, a, x + 8 + i as i32 * 8, y + 10, m, if on { PASS } else { FAINT }, on);
    }
    text(buf, a, x + 34, y + 10, "start · pause · resume", FAINT, false);

    // ── what the styling does ────────────────────────────────────────────
    text(buf, a, x, y + 13, "STYLING", MUTE, true);
    let layers = [
        ("landuse", 0.22, "dithered, so it reads as behind"),
        ("water", 0.34, "one flat tone, no outline"),
        ("roads", 0.72, "tiers cut from the real histogram"),
        ("labels", 0.95, "dropped rather than overlapped"),
    ];
    // Composited bottom-up, one layer at a time, because that is the order the
    // renderer works in and the order the decisions were made in.
    let upto = ((cycle * 1.6) * layers.len() as f64) as usize;
    for (i, (name, tone, note)) in layers.iter().enumerate() {
        let ly = y + 15 + i as i32;
        let on = i <= upto;
        text(buf, a, x, ly, name, if on { INK } else { FAINT }, on);
        for k in 0..22i32 {
            // A shading ramp per layer: the same eight-step ladder the renderer
            // has to work with, which is the whole constraint.
            let v = tone * (0.55 + 0.45 * ((k as f64 * 0.5 + i as f64).sin() * 0.5 + 0.5));
            if on {
                spark(buf, a, x + 10 + k, ly, &[v], INK);
            } else {
                put(buf, a, x + 10 + k, ly, '·', FAINT, false);
            }
        }
        text(buf, a, x + 34, ly, note, if on { MUTE } else { FAINT }, false);
    }

    text(buf, a, x, y + 20, "hillshade", MUTE, false);
    let sun = (t * 0.35).sin() * 0.5 + 0.5;
    for k in 0..22i32 {
        // Slope, not elevation: flat ground stays empty and only real relief
        // shows, which is what stops a city being buried in texture.
        let slope = ((k as f64 * 0.6).sin() * 0.5 + 0.5).powf(1.6);
        spark(buf, a, x + 10 + k, y + 20, &[slope * (0.35 + 0.65 * sun)], INK);
    }
    text(buf, a, x + 34, y + 20, "driven by slope, never by height", MUTE, false);

    note(buf, a, x, y + 23, W as usize,
        "a map is a set of decisions about what to leave out — the pipeline exists to make those decisions expressible", FAINT);
}

/// termap: archive size stops mattering once tiles are read on demand.
fn termap(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 58;
    const H: u16 = 18;
    let (x, y) = origin(a, W, H);
    let cycle = (t * 0.12) % 1.0;
    let z = 4.0 + cycle * 10.0;

    text(buf, a, x, y, "zoom", MUTE, false);
    bar(buf, a, x + 8, y, 30, cycle, INK);
    text(buf, a, x + 40, y, &format!("z{z:.1}"), INK, true);

    // A grid filling in as tiles arrive. Only what the viewport needs is ever
    // fetched, which is the whole argument.
    for gy in 0..6i32 {
        for gx in 0..12i32 {
            let want = (gx + gy * 12) as f64 / 72.0;
            let here = want < (cycle * 1.4);
            put(buf, a, x + 8 + gx * 2, y + 3 + gy,
                if here { '▪' } else { '·' },
                if here { INK } else { FAINT }, false);
        }
    }
    text(buf, a, x, y + 3, "tiles", MUTE, false);
    travel(buf, a, x + 33, x + 40, y + 5, t * 0.9, 0.0, 2, '•', PASS);
    text(buf, a, x + 42, y + 5, "on demand", PASS, false);

    text(buf, a, x, y + 10, "archive", MUTE, false);
    text(buf, a, x + 10, y + 10, "1.6 GB — all of India, z4–z14", INK, false);
    text(buf, a, x, y + 11, "resident", MUTE, false);
    text(buf, a, x + 10, y + 11, "19 MB", PASS, true);
    text(buf, a, x + 18, y + 11, "· 96 tiles held", MUTE, false);
    text(buf, a, x, y + 12, "eager", MUTE, false);
    text(buf, a, x + 10, y + 12, "~220 bytes a feature — caps out at 2–3M", STOP, false);

    let r = note(buf, a, x, y + 15, W as usize,
        "every cell is 2×4 braille dots, so a 160×46 terminal is a 320×184 framebuffer", FAINT);
    note(buf, a, x, y + 15 + r, W as usize,
        "coverage says which dots light, depth says how bright — two axes out of one", FAINT);
}

/// Noter: the editor is somebody else's, and the history is a real commit graph.
fn noter(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 58;
    const H: u16 = 16;
    let (x, y) = origin(a, W, H);
    let beat = (t * 0.28) % 1.0;

    for (i, (name, note)) in [("$EDITOR", "nvim, yours"), ("note", "duckdb"), ("git", "one commit")]
        .iter()
        .enumerate()
    {
        let bx = x + i as i32 * 20;
        let live = (beat * 3.0) as usize == i;
        boxed(buf, a, Rect { x: bx as u16, y: (y + 2) as u16, width: 16, height: 4 },
              name, if live { INK } else { FAINT }, live);
        text(buf, a, bx + 2, y + 4, note, if live { INK } else { MUTE }, false);
        if i < 2 {
            arrow(buf, a, bx + 16, bx + 19, y + 4, "", FAINT);
            travel(buf, a, bx + 16, bx + 19, y + 4, t * 0.9, i as f64 * 0.3, 1, '•', PASS);
        }
    }

    text(buf, a, x, y + 7, "history", MUTE, false);
    let hashes = ["a3f9c1", "7b2e04", "51dd8a", "c07f13", "9e4b22"];
    let head = ((t * 0.28) as usize) % hashes.len();
    for r in 0..4 {
        let h = hashes[(head + r) % hashes.len()];
        text(buf, a, x + 10, y + 7 + r as i32, h, if r == 0 { PASS } else { MUTE }, r == 0);
        text(buf, a, x + 18, y + 7 + r as i32,
             ["just now", "3 min ago", "an hour ago", "yesterday"][r], FAINT, false);
    }

    let r = note(buf, a, x, y + 13, W as usize,
        "reimplementing an editor is months of work to arrive somewhere worse", FAINT);
    note(buf, a, x, y + 13 + r, W as usize,
        "notes outlive the program that wrote them, because git can still read them", FAINT);
}

/// gitswitch: the name and the key move together, and only locally.
fn gitswitch(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 60;
    const H: u16 = 17;
    let (x, y) = origin(a, W, H);
    let work = ((t * 0.22) % 1.0) < 0.5;

    text(buf, a, x, y, "~/.config/git_conf/", MUTE, false);
    for (i, (file, on)) in [("work.toml", work), ("personal.toml", !work)].iter().enumerate() {
        let bx = x + i as i32 * 20;
        boxed(buf, a, Rect { x: bx as u16, y: (y + 2) as u16, width: 18, height: 3 },
              "", if *on { PASS } else { FAINT }, *on);
        text(buf, a, bx + 2, y + 3, file, if *on { INK } else { MUTE }, *on);
        if *on {
            put(buf, a, bx + 8, y + 5, '│', PASS, false);
            put(buf, a, bx + 8, y + 6, '▼', PASS, false);
        }
    }

    let cfg = Rect { x: x as u16, y: (y + 7) as u16, width: 52, height: 7 };
    boxed(buf, a, cfg, ".git/config — local only", INK, true);
    let (mail, key) = if work {
        ("priya@work.com", "~/.ssh/id_ed25519_work")
    } else {
        ("priya@home.dev", "~/.ssh/id_ed25519_personal")
    };
    text(buf, a, x + 2, y + 9, "user.email", MUTE, false);
    text(buf, a, x + 20, y + 9, mail, INK, true);
    text(buf, a, x + 2, y + 10, "core.sshCommand", MUTE, false);
    text(buf, a, x + 20, y + 10, key, INK, true);
    note(buf, a, x + 2, y + 12, 48,
        "both change together — the correct email with the wrong key is the failure", FAINT);

    note(buf, a, x, y + 15, W as usize,
        "~/.gitconfig is never touched: global config cannot say 'this repo, this person'", FAINT);
}


/// harbr: one door in, and one way for anything to get deployed.
fn harbr(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 78;
    const H: u16 = 26;
    let (x, y) = origin(a, W, H);
    let r = x + 42;
    const LW: usize = 38;
    const RW: usize = 36;

    text(buf, a, x, y, "ONE DOOR", MUTE, true);
    text(buf, a, r, y, "ONE DEPLOY", MUTE, true);

    // ── the gate ──────────────────────────────────────────────────────────
    text(buf, a, x, y + 2, "ssh harbr@your-box", INK, true);
    put(buf, a, x + 3, y + 3, '│', FAINT, false);
    put(buf, a, x + 3, y + 4, '▼', FAINT, false);

    boxed(buf, a, Rect { x: x as u16, y: (y + 5) as u16, width: 36, height: 3 },
          "the key you connected with", INK, true);
    text(buf, a, x + 2, y + 6, "SHA256:ax9…", INK, true);
    text(buf, a, x + 16, y + 6, "ana@laptop", MUTE, false);

    // A known key is one step; the path worth drawing is the other one.
    text(buf, a, x + 3, y + 9, "unknown", STOP, false);
    arrow(buf, a, x + 20, x + 30, y + 9, " known ", PASS);
    text(buf, a, x + 32, y + 9, "in", PASS, true);
    put(buf, a, x + 5, y + 10, '│', FAINT, false);
    put(buf, a, x + 5, y + 11, '▼', FAINT, false);

    boxed(buf, a, Rect { x: x as u16, y: (y + 12) as u16, width: 32, height: 3 },
          "access request", MUTE, false);
    text(buf, a, x + 2, y + 13, "pending · settings › access", MUTE, false);
    text(buf, a, x + 5, y + 15, "│  an admin presses  a", MUTE, false);
    put(buf, a, x + 5, y + 16, '▼', PASS, false);
    text(buf, a, x + 8, y + 16, "in, without reconnecting", PASS, true);

    note(buf, a, x, y + 18, LW,
        "the first key ever to connect claims the instance as admin; HARBR_TOFU=0 \
         turns that off", FAINT);

    // ── the deploy ────────────────────────────────────────────────────────
    text(buf, a, r, y + 2, "git", MUTE, false);
    arrow(buf, a, r + 4, r + 10, y + 2, "", FAINT);
    text(buf, a, r + 12, y + 2, "docker-compose.yml", INK, true);
    text(buf, a, r + 2, y + 3, "the compose CLI is never invoked", FAINT, false);

    boxed(buf, a, Rect { x: r as u16, y: (y + 5) as u16, width: 34, height: 3 },
          "BuildKit", INK, true);
    text(buf, a, r + 2, y + 6, "--platform  RUN --mount  heredocs", MUTE, false);

    // Health-aware ordering, walked so the waiting is visible.
    let step = ((t * 0.5) % 1.0 * 5.0) as usize;
    for (i, name) in ["db", "cache", "api", "web"].iter().enumerate() {
        let row = y + 9 + i as i32;
        let (up, now) = (i < step, i == step);
        let (mark, c) = if up {
            ('●', PASS)
        } else if now {
            ('◍', INK)
        } else {
            ('·', FAINT)
        };
        put(buf, a, r, row, mark, c, up || now);
        text(buf, a, r + 2, row, name, if up || now { INK } else { FAINT }, up);
        let state = if up {
            "healthy"
        } else if now {
            "starting"
        } else {
            "waiting"
        };
        text(buf, a, r + 10, row, state, c, false);
    }
    text(buf, a, r + 20, y + 9, "depends_on:", FAINT, false);
    text(buf, a, r + 20, y + 10, "service_healthy", FAINT, false);

    note(buf, a, r, y + 14, RW,
        "one that never goes healthy fails the deploy, rather than succeeding over \
         a crash-looping container", FAINT);

    text(buf, a, r, y + 19, "rollback", INK, true);
    arrow(buf, a, r + 9, r + 15, y + 19, "", FAINT);
    text(buf, a, r + 17, y + 19, "the exact image ids", PASS, true);
    note(buf, a, r, y + 20, RW,
        "not the tags — a moved tag or a Dockerfile that no longer builds cannot \
         defeat it", FAINT);

    note(buf, a, x, y + 23, W as usize,
        "bind mounts resolve to absolute paths that the Docker daemon reads on the \
         *host*, so harbr's own data directory has to sit at the same path inside its \
         container: /var/lib/harbr at /var/lib/harbr is load-bearing, not tidiness",
        FAINT);
}

/// vcs: the name of a thing is its hash, and everything else follows.
fn vcs(buf: &mut Buffer, a: Rect, t: f64) {
    const W: u16 = 58;
    const H: u16 = 17;
    let (x, y) = origin(a, W, H);

    text(buf, a, x, y, "content", MUTE, false);
    text(buf, a, x + 16, y, "sha", MUTE, false);
    text(buf, a, x + 30, y, "objects", MUTE, false);

    let rows = [("'hello'", "a1b2c3", false), ("'hello'", "a1b2c3", true), ("'world'", "d4e5f6", false)];
    let live = ((t * 0.4) % 1.0 * 3.0) as usize;
    for (i, (c, h, dupe)) in rows.iter().enumerate() {
        let ry = y + 2 + i as i32;
        let on = i <= live;
        text(buf, a, x, ry, c, if on { INK } else { FAINT }, false);
        if on {
            arrow(buf, a, x + 9, x + 14, ry, "", FAINT);
            text(buf, a, x + 16, ry, h, if *dupe { STOP } else { PASS }, true);
            if *dupe {
                text(buf, a, x + 24, ry, "already there", STOP, false);
            } else {
                arrow(buf, a, x + 24, x + 28, ry, "", FAINT);
            }
        }
    }
    boxed(buf, a, Rect { x: (x + 30) as u16, y: (y + 1) as u16, width: 14, height: 5 }, "", INK, false);
    for (i, h) in ["a1b2c3", "d4e5f6"].iter().enumerate() {
        if i == 0 || live >= 2 {
            text(buf, a, x + 32, y + 2 + i as i32 * 2, h, INK, false);
        }
    }
    note(buf, a, x, y + 6, W as usize, "two identical contents cannot occupy two addresses", FAINT);

    text(buf, a, x, y + 9, "merge", MUTE, false);
    text(buf, a, x + 10, y + 8, "base", MUTE, false);
    put(buf, a, x + 16, y + 8, '┬', FAINT, false);
    text(buf, a, x + 18, y + 8, "ours", INK, false);
    put(buf, a, x + 16, y + 9, '│', FAINT, false);
    put(buf, a, x + 16, y + 10, '┴', FAINT, false);
    text(buf, a, x + 18, y + 10, "theirs", INK, false);
    arrow(buf, a, x + 26, x + 30, y + 9, "", FAINT);
    let clash = ((t * 0.4) % 1.0) > 0.5;
    text(buf, a, x + 32, y + 9, if clash { "conflict" } else { "merged" },
         if clash { STOP } else { PASS }, true);

    let r = note(buf, a, x, y + 13, W as usize,
        "deduplication, integrity and immutability are consequences, not features", FAINT);
    note(buf, a, x, y + 13 + r, W as usize,
        "merge is the one command you cannot learn by reading its output", FAINT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn all() -> Vec<crate::data::Project> {
        crate::data::parse(include_str!("../data/projects.txt")).unwrap()
    }

    fn frame(p: &crate::data::Project, t: f64, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        assert!(draw(&mut buf, area, p, t, Theme::Night), "{} has no diagram", p.id);
        buf
    }

    #[test]
    fn nothing_is_cropped_at_the_size_it_asked_for() {
        // A scene given exactly its own footprint must draw the same thing it
        // draws with room to spare. Anything else means the declared size is a
        // lie and the layout will crop it.
        for p in all() {
            let (w, h) = footprint(&p.id);
            let ink = |aw: u16, ah: u16| {
                let area = Rect::new(0, 0, aw, ah);
                let mut buf = Buffer::empty(area);
                draw(&mut buf, area, &p, 2.0, Theme::Night);
                (0..ah)
                    .flat_map(|y| (0..aw).map(move |x| (x, y)))
                    .filter(|&(x, y)| buf.cell((x, y)).unwrap().symbol() != " ")
                    .count()
            };
            assert_eq!(
                ink(w, h),
                ink(w + 40, h + 12),
                "{} loses ink at its own {w}x{h} footprint",
                p.id
            );
        }
    }

    #[test]
    fn every_project_has_a_diagram() {
        // The prose fallback exists so a half-finished set still renders, but
        // nothing should be using it: a card of paragraphs next to a card of
        // working mechanism looks like the mechanism was abandoned.
        let area = Rect::new(0, 0, 80, 30);
        for p in all() {
            let mut buf = Buffer::empty(area);
            assert!(draw(&mut buf, area, &p, 1.0, Theme::Night), "{} still falls back to prose", p.id);
        }
    }

    #[test]
    fn a_scene_is_a_pure_function_of_its_clock() {
        // Nothing accumulates, so the same instant is the same picture. That is
        // what lets a snapshot be trusted, a pause be a pause, and two runs be
        // compared at all.
        for p in all() {
            let a = frame(&p, 3.25, 80, 30);
            assert_eq!(a, frame(&p, 3.25, 80, 30), "{} is not deterministic", p.id);
            let mut moved = false;
            for t in [0.4, 1.1, 2.0, 4.7, 9.3] {
                if frame(&p, t, 80, 30) != a {
                    moved = true;
                    break;
                }
            }
            assert!(moved, "{} never moves — it is a still picture", p.id);
        }
    }

    #[test]
    fn nothing_draws_outside_its_frame() {
        // Every primitive clips, so a diagram laid out for a wide terminal must
        // not corrupt a narrow one — and the narrow ones are the interesting
        // case, because that is where the coordinates go negative.
        for p in all() {
            for w in [30u16, 48, 66, 90, 130] {
                for h in [10u16, 18, 26, 40] {
                    let area = Rect::new(3, 2, w, h);
                    let mut buf = Buffer::empty(Rect::new(0, 0, w + 6, h + 4));
                    draw(&mut buf, area, &p, 1.7, Theme::Night);
                    for y in 0..h + 4 {
                        for x in 0..w + 6 {
                            let inside = x >= 3 && x < 3 + w && y >= 2 && y < 2 + h;
                            if !inside {
                                assert_eq!(
                                    buf.cell((x, y)).unwrap().symbol(),
                                    " ",
                                    "{} drew at {x},{y}, outside a {w}x{h} area",
                                    p.id
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_scene_actually_puts_something_on_screen() {
        // A clipped-to-nothing diagram passes the bounds test trivially. This is
        // the other half: at a normal size, each one has to draw.
        for p in all() {
            let buf = frame(&p, 2.0, 90, 30);
            let ink = (0..30u16)
                .flat_map(|y| (0..90u16).map(move |x| (x, y)))
                .filter(|&(x, y)| buf.cell((x, y)).unwrap().symbol() != " ")
                .count();
            assert!(ink > 120, "{} drew only {ink} cells", p.id);
        }
    }
}

#[cfg(test)]
mod probe {
    use super::*;
    use ratatui::buffer::Buffer;

    /// Not a test: a measuring tape. `cargo test measure_footprints -- --ignored
    /// --nocapture` prints the table that `footprint` should contain, which is
    /// how those numbers were arrived at and how they get corrected.
    #[test]
    #[ignore]
    fn measure_footprints() {
        let ps = crate::data::parse(include_str!("../data/projects.txt")).unwrap();
        for p in &ps {
            let ink = |aw: u16, ah: u16| {
                let area = Rect::new(0, 0, aw, ah);
                let mut buf = Buffer::empty(area);
                draw(&mut buf, area, p, 2.0, Theme::Night);
                (0..ah)
                    .flat_map(|y| (0..aw).map(move |x| (x, y)))
                    .filter(|&(x, y)| buf.cell((x, y)).unwrap().symbol() != " ")
                    .count()
            };
            let full = ink(200, 60);
            let w = (30..=140u16).find(|&w| ink(w, 60) == full).unwrap_or(0);
            let h = (8..=60u16).find(|&h| ink(200, h) == full).unwrap_or(0);
            println!("        \"{}\" => ({w}, {h}),", p.id);
        }
    }
}
