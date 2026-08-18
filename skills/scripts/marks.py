#!/usr/bin/env python3
"""Extruded marks for the project cards.

    python3 scripts/marks.py     # -> src/marks.rs

The tool logos come from vendored SVG and get resampled, which is why they need
a glyph search to stay crisp. These do not: a project has no logo to fetch, so
each one is drawn here at exactly the resolution it will be shown, one authored
pixel per half-block. Nothing is scaled, so nothing needs sharpening.

Depth is a stack, not a projection. The face is drawn last over `DEPTH` copies
of itself stepped down and to the right, each darker than the one in front, so
the solid reads as having a side. Two details do most of the work: the step is
one pixel on each axis, which is 45 degrees on screen because a half-block
pixel is square; and the face keeps a lit edge along its top-left, which is the
only thing that says where the light is and therefore which way is up.
"""
import os
import re

SHEET = r"""
/// Every project mark, side by side. Same purpose as the tool contact sheet:
/// these are composed by a script that cannot see a terminal.
pub fn sheet() -> String {
    const BG: (u8, u8, u8) = (6, 7, 10);
    let blend = |c: (u8, u8, u8), a: u8| {
        let a = a as f32 / 255.0;
        (
            (BG.0 as f32 + (c.0 as f32 - BG.0 as f32) * a) as u8,
            (BG.1 as f32 + (c.1 as f32 - BG.1 as f32) * a) as u8,
            (BG.2 as f32 + (c.2 as f32 - BG.2 as f32) * a) as u8,
        )
    };
    let mut out = String::new();
    for row in MARKS.chunks(3) {
        let h = row.iter().map(|m| m.art.rows).max().unwrap_or(0);
        for r in 0..h {
            for m in row {
                let a = &m.art;
                for c in 0..a.cols {
                    if r >= a.rows {
                        out.push(' ');
                        continue;
                    }
                    let (ch, f, b) = a.cells[(r * a.cols + c) as usize];
                    if f == 0 && b == 0 {
                        out.push_str("\x1b[0m ");
                        continue;
                    }
                    let (fr, fg, fb) = blend(m.rgb, f);
                    let (br, bg, bb) = blend(m.rgb, b);
                    out.push_str(&format!(
                        "\x1b[38;2;{fr};{fg};{fb}m\x1b[48;2;{br};{bg};{bb}m{ch}"
                    ));
                }
                out.push_str("\x1b[0m");
                out.push_str(&" ".repeat(40usize.saturating_sub(a.cols as usize)));
            }
            out.push('\n');
        }
        for m in row {
            out.push_str(&format!("\x1b[38;2;150;155;165m{:<40}\x1b[0m", m.id));
        }
        out.push_str("\n\n");
    }
    out
}
"""

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "src", "marks.rs")

# Pixels of extrusion behind the face.
DEPTH = 4
# The face, its lit edge, and the range the side ramps across.
FACE = 0.92
EDGE = 1.0
SIDE_NEAR = 0.52
SIDE_FAR = 0.20

# ── a very small shape language ───────────────────────────────────────────────
#
# The first version of these was typed out as ASCII grids by hand, and every
# one of them came out as something other than what was intended -- a shield
# that read as a pill, a fork that read as nothing at all. Geometry is not a
# thing to eyeball a character at a time. These are composed instead, which
# makes them correct by construction and, more usefully, adjustable: a radius
# is a number here rather than forty lines to retype.

W, H = 26, 22


def blank():
    return [[False] * W for _ in range(H)]


def paint(f):
    g = blank()
    for y in range(H):
        for x in range(W):
            # Sampled at pixel centres, and x is halved nowhere: a half-block
            # pixel is square on screen, so the grid is isotropic.
            g[y][x] = f(x + 0.5, y + 0.5)
    return g


def disc(cx, cy, r):
    return lambda x, y: (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def ring(cx, cy, r, t):
    return lambda x, y: abs(((x - cx) ** 2 + (y - cy) ** 2) ** 0.5 - r) <= t / 2


def rect(x0, y0, x1, y1):
    return lambda x, y: x0 <= x <= x1 and y0 <= y <= y1


def rrect(x0, y0, x1, y1, r):
    def f(x, y):
        cx = min(max(x, x0 + r), x1 - r)
        cy = min(max(y, y0 + r), y1 - r)
        return (x - cx) ** 2 + (y - cy) ** 2 <= r * r
    return f


def tri(ax, ay, bx, by, cx, cy):
    def side(px, py, qx, qy, x, y):
        return (qx - px) * (y - py) - (qy - py) * (x - px)
    def f(x, y):
        d1 = side(ax, ay, bx, by, x, y)
        d2 = side(bx, by, cx, cy, x, y)
        d3 = side(cx, cy, ax, ay, x, y)
        return not ((d1 < 0 or d2 < 0 or d3 < 0) and (d1 > 0 or d2 > 0 or d3 > 0))
    return f


def shield(x0, y0, x1, y1):
    mid = y0 + (y1 - y0) * 0.52
    top = rrect(x0, y0, x1, mid, 3)
    def f(x, y):
        if y <= mid:
            return top(x, y)
        t = (y - mid) / (y1 - mid)
        half = (x1 - x0) * 0.5 * (1 - t * t * 0.96)
        return abs(x - (x0 + x1) / 2) <= half
    return f


def union(*fs):
    return lambda x, y: any(f(x, y) for f in fs)


def sub(a, *fs):
    return lambda x, y: a(x, y) and not any(f(x, y) for f in fs)


def bars(x0, x1, y0, step, widths, t=1.6):
    """Stacked lines, the way a page or a terminal has lines on it."""
    parts = []
    for i, frac in enumerate(widths):
        y = y0 + i * step
        parts.append(rect(x0, y, x0 + (x1 - x0) * frac, y + t))
    return union(*parts)


def render(f):
    g = paint(f)
    return "\n".join("".join("#" if c else "." for c in row) for row in g)


def from_svg(name, side, face, ss=8):
    """A mark taken from a real logo instead of composed here.

    Some projects have an actual logo, and where one exists it should win: an
    emblem invented for a project that already has one is a worse answer no
    matter how well it is drawn.

    The face test decides which pixels are the solid and which are holes, which
    is the whole translation problem — these marks carry one colour and an
    extrusion, and a logo carries several. A film reel is a disc with openings
    punched through it, so keeping the body and dropping everything else is not
    a loss of information, it is the drawing.

    The shape decision is made at `ss` times the final size and then averaged
    down, so an edge lands on the pixel it mostly covers rather than on
    whichever side of it the sample happened to fall.
    """
    from PIL import Image
    from reportlab.graphics import renderPM
    from svglib.svglib import svg2rlg

    path = os.path.join(os.path.dirname(HERE), "assets", "marks", f"{name}.svg")
    d = svg2rlg(path)
    big = side * ss
    k = big / max(d.width, d.height)
    d.scale(k, k)
    d.width, d.height = d.width * k, d.height * k
    tmp = path + ".png"
    renderPM.drawToFile(d, tmp, fmt="PNG", bg=0xFFFFFF)
    im = Image.open(tmp).convert("RGB")
    os.remove(tmp)

    # Boolean at high resolution, then averaged into coverage, then thresholded
    # once. Thresholding first would alias every curve.
    px = im.load()
    cov = [[0.0] * side for _ in range(side)]
    for y in range(side):
        for x in range(side):
            n = 0
            for dy in range(ss):
                for dx in range(ss):
                    sx, sy = x * ss + dx, y * ss + dy
                    if sx < im.width and sy < im.height and face(px[sx, sy]):
                        n += 1
            cov[y][x] = n / (ss * ss)
    return "\n".join(
        "".join("#" if c >= 0.5 else "." for c in row) for row in cov
    )


def is_red(c):
    """The reel's body, and not its openings or the paper behind it."""
    r, g, b = c
    return r > 110 and r - max(g, b) > 40


# ── the nine ─────────────────────────────────────────────────────────────────

MARKS = {}

# a shield with the slot of a blocked port cut out of it
MARKS["netjail"] = ((236, 160, 92), render(sub(
    shield(2, 1, 24, 21),
    rect(7, 9.5, 19, 12.5),
)))

# the project's own logo: a film reel, its openings punched through
MARKS["watch-party"] = ((253, 74, 90), from_svg("watch-party", 26, is_red))

# a terminal window: title bar, prompt, output
MARKS["logify"] = ((120, 200, 226), render(union(
    sub(rrect(1, 2, 25, 20, 2), rect(2.5, 6, 23.5, 18.5)),
    rect(3.5, 3.2, 5.5, 4.8), rect(7, 3.2, 9, 4.8),
    tri(4, 8, 4, 13, 8.5, 10.5),
    bars(10, 22, 8, 3.4, [1.0, 0.72, 0.86]),
)))

# a clipboard, with its clip
MARKS["clip"] = ((110, 200, 180), render(union(
    sub(rrect(2, 3, 24, 21, 2), rect(4, 5.5, 22, 19)),
    sub(rrect(9, 0.5, 17, 5, 1.5), rect(11, 2, 15, 4)),
    bars(6, 20, 8, 3.2, [0.9, 0.62, 0.78]),
)))

# a page with a folded corner
MARKS["noter"] = ((214, 158, 178), render(union(
    sub(rect(3, 1, 23, 21), rect(5, 3, 21, 19), tri(17, 1, 23, 1, 23, 7)),
    tri(17, 1, 23, 7, 17, 7),
    bars(6, 20, 5, 3.4, [0.9, 0.55, 0.78, 0.4]),
)))

# a key. A toggle was the first idea and it does not survive the size: the knob
# and the track it runs in are two rounded shapes a couple of pixels apart, and
# they fuse into one lozenge with a slot in it. A key has a hole, a shaft and
# teeth -- three features at three different scales -- so it stays legible when
# everything gets coarse. It is also nearer the truth, since what gitswitch
# actually swaps is which SSH key signs your commits.
MARKS["gitswitch"] = ((222, 190, 148), render(union(
    ring(7, 11, 4.6, 3.0),
    rect(10.5, 9.6, 24, 12.4),
    rect(17.5, 12.4, 19.8, 16.6),
    rect(21.6, 12.4, 23.8, 15.2),
)))

# three commits on one line. Nodes need daylight between them: at the first
# spacing the discs touched and the whole thing read as one column.
MARKS["vcs"] = ((150, 176, 206), render(sub(
    union(
        rect(12.2, 2, 13.8, 20),
        disc(13, 3.4, 3.0),
        disc(13, 11, 3.0),
        disc(13, 18.6, 3.0),
    ),
    disc(13, 3.4, 1.3),
    disc(13, 11, 1.3),
    disc(13, 18.6, 1.3),
)))

# a map pin
MARKS["stylized-maps"] = ((140, 196, 148), render(union(
    sub(disc(13, 9, 8), disc(13, 9, 3.4)),
    tri(6.5, 13.5, 19.5, 13.5, 13, 21.5),
)))

# contour rings
MARKS["harbr"] = ((150, 190, 214), render(sub(
    union(
        rect(7, 3, 18, 9),        # the one on top
        rect(1, 11, 12, 17),
        rect(13.5, 11, 24.5, 17),
    ),
    # Door ridges. Without them three rectangles are three rectangles; with
    # them they are shipping containers, which is the entire idea of the name.
    *[rect(x, 4, x + 0.9, 8) for x in (9.5, 12.5, 15.5)],
    *[rect(x, 12, x + 0.9, 16) for x in (3.5, 6.5, 9.5)],
    *[rect(x, 12, x + 0.9, 16) for x in (16, 19, 22)],
)))

MARKS["termap"] = ((176, 208, 232), render(union(
    ring(13, 11, 9.5, 2),
    ring(13, 11, 6, 2),
    ring(13, 11, 2.6, 2.2),
)))

def grid(art):
    rows = [r for r in art.strip("\n").split("\n")]
    w = max(len(r) for r in rows)
    return [r.ljust(w, ".") for r in rows], w, len(rows)


def build(art):
    """-> alpha per pixel, on a canvas that fits the face and its extrusion."""
    rows, w, h = grid(art)
    W, H = w + DEPTH, h + DEPTH
    px = [[0.0] * W for _ in range(H)]
    solid = lambda x, y: 0 <= x < w and 0 <= y < h and rows[y][x] == "#"

    # Back to front, so nearer slices overwrite further ones.
    for k in range(DEPTH, 0, -1):
        t = (k - 1) / max(1, DEPTH - 1)
        shade = SIDE_NEAR + (SIDE_FAR - SIDE_NEAR) * t
        for y in range(h):
            for x in range(w):
                if solid(x, y):
                    px[y + k][x + k] = shade

    for y in range(h):
        for x in range(w):
            if not solid(x, y):
                continue
            # A lit rim wherever the face turns away from the light, which is
            # up and to the left. Without it the face is a flat silhouette and
            # the extrusion behind it reads as a shadow rather than a side.
            rim = not solid(x - 1, y) or not solid(x, y - 1)
            px[y][x] = EDGE if rim else FACE
    return px, W, H


def cells(px, W, H):
    out = []
    for cy in range(0, H, 2):
        for x in range(W):
            t = px[cy][x]
            b = px[cy + 1][x] if cy + 1 < H else 0.0
            out.append((" " if t == 0 and b == 0 else "▀", round(t * 255), round(b * 255)))
    return out, W, (H + 1) // 2


def main():
    body, index = [], []
    for pid, (rgb, art) in MARKS.items():
        px, W, H = build(art)
        cs, cols, rows = cells(px, W, H)
        name = re.sub(r"[^A-Z0-9]", "_", pid.upper()) + "_MARK"
        body.append(
            f"const {name}: [Cell; {len(cs)}] = [\n    "
            + ",\n    ".join(
                ("(' ', 0, 0)" if c == " " else f"('\\u{{{ord(c):x}}}', {f}, {b})")
                for c, f, b in cs
            )
            + ",\n];"
        )
        index.append(
            f'    Mark {{ id: "{pid}", rgb: ({rgb[0]}, {rgb[1]}, {rgb[2]}),\n'
            f"          art: Art {{ cols: {cols}, rows: {rows}, cells: &{name} }} }},"
        )
        print(f"  {pid:<16} {cols}x{rows} cells")

    src = (
        "#![allow(dead_code)]\n"
        "//! Project marks, extruded. GENERATED by scripts/marks.py -- do not\n"
        "//! edit. Unlike the tool logos these are authored at their final\n"
        "//! resolution, so every cell is a half block and no fitting is needed.\n\n"
        "use crate::logos::{Art, Cell};\n\n"
        "pub struct Mark {\n    pub id: &'static str,\n"
        "    pub rgb: (u8, u8, u8),\n    pub art: Art,\n}\n\n"
        "pub fn find(id: &str) -> Option<&'static Mark> {\n"
        "    MARKS.iter().find(|m| m.id == id)\n}\n\n"
        + "\n\n".join(body)
        + "\n\npub const MARKS: &[Mark] = &[\n"
        + "\n".join(index)
        + "\n];\n"
        + SHEET
    )
    open(OUT, "w").write(src)
    print(f"\n{len(MARKS)} marks -> src/marks.rs ({len(src) // 1024} KB)")


if __name__ == "__main__":
    main()
