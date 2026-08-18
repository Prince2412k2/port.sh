#!/usr/bin/env python3
"""Emblems for the taste section.

    python3 portfolio/scripts/emblems.py     # -> portfolio/src/emblems.rs

Each entry in `taste.txt` gets a drawing. They are *not* stills from the films
and shows, for two reasons. A dithered forty-column crop of a copyrighted frame
is a bad idea on a public server. And more usefully: the object a character
carries says more about them than a low-resolution picture of their face does.
Snufkin is a hat and a wandering line; Iroh is a cup of tea. That is the whole
argument of the section, drawn.

Composed, not typed. Hand-drawn ASCII grids come out wrong -- geometry is not a
thing to eyeball a character at a time -- so these are built from shape
functions sampled on a pixel grid, which makes them correct by construction and,
more usefully, adjustable: a radius is a number here rather than forty lines to
retype.

Flat rather than extruded, unlike the project marks. Those are logos and want to
look like objects; these want to look like woodcuts, so there is one face, a lit
rim where the shape turns away from the light, and nothing behind.
"""
import os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(os.path.dirname(HERE), "src", "emblems.rs")

W, H = 34, 28

# The face, and the rim that says where the light is (up and to the left).
FACE = 0.80
EDGE = 1.0


def blank():
    return [[False] * W for _ in range(H)]


def paint(f):
    g = blank()
    for y in range(H):
        for x in range(W):
            # A half-block pixel is square on screen, so the grid is isotropic
            # and x is not scaled anywhere.
            g[y][x] = f(x + 0.5, y + 0.5)
    return g


def disc(cx, cy, r):
    return lambda x, y: (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def ellipse(cx, cy, rx, ry):
    return lambda x, y: ((x - cx) / rx) ** 2 + ((y - cy) / ry) ** 2 <= 1.0


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


def quad(a, b, c, d):
    return union(tri(*a, *b, *c), tri(*a, *c, *d))


def seg(x0, y0, x1, y1, t):
    """A thick line segment, ends included."""
    dx, dy = x1 - x0, y1 - y0
    L2 = dx * dx + dy * dy

    def f(x, y):
        if L2 == 0:
            u = 0.0
        else:
            u = max(0.0, min(1.0, ((x - x0) * dx + (y - y0) * dy) / L2))
        px, py = x0 + u * dx, y0 + u * dy
        return (x - px) ** 2 + (y - py) ** 2 <= (t / 2) ** 2
    return f


def wave(x0, x1, y, amp, period, t):
    """A sine stroke, for steam and for anything that should look drawn."""
    import math

    def f(x, y_):
        if not (x0 <= x <= x1):
            return False
        yy = y + amp * math.sin((x - x0) / period * math.tau)
        return abs(y_ - yy) <= t / 2
    return f


def union(*fs):
    return lambda x, y: any(f(x, y) for f in fs)


def sub(a, *fs):
    return lambda x, y: a(x, y) and not any(f(x, y) for f in fs)


def render(f):
    g = paint(f)
    return "\n".join("".join("#" if c else "." for c in row) for row in g)


# ── the drawings ──────────────────────────────────────────────────────────
#
# Tints are muted on purpose. The rest of the app sits in a narrow luminance
# band and a saturated emblem would jump out of the page like a sticker.

EMBLEMS = {}

# Snufkin: the hat, and the line he walks off along.
EMBLEMS["hat"] = ((150, 178, 132), render(union(
    # The crown is a cone with the point rounded off. A true apex lands on one
    # pixel at this size and reads as a shape that has been cut, not pointed.
    # A narrow crown on a wide flat brim. When the crown is as wide as the
    # brim the two merge and the whole thing reads as a mountain.
    # A soft felt crown, not a cone. A cone on a brim reads as a mountain on a
    # plain; a dome that swells out and tucks back in reads as something worn.
    ellipse(17, 13.5, 6.0, 6.2),
    tri(13.5, 13, 17.5, 6.5, 20.5, 13),
    ellipse(17, 18.4, 15, 2.3),
)))

# The Little Prince: his planet, his rose, his two volcanoes.
EMBLEMS["planet"] = ((228, 202, 142), render(union(
    disc(17, 21, 8.5),
    # Two volcanoes, seated far enough into the disc that they read as part of
    # it rather than as objects balanced on top.
    tri(10.5, 13.0, 12.5, 9.2, 14.5, 13.0),
    tri(20.0, 13.4, 22.0, 10.0, 24.0, 13.4),
    seg(17, 13, 17, 7, 1.0),
    sub(disc(17, 5, 2.8), ring(17, 5, 1.3, 0.9)),
)))

# Iroh: tea, and the practice of noticing small good things on purpose.
EMBLEMS["teacup"] = ((146, 196, 176), render(union(
    quad((9, 13), (25, 13), (22, 23), (12, 23)),
    ring(26, 17, 3.4, 1.6),
    ellipse(17, 25, 11, 1.8),
    wave(12, 16, 8, 1.6, 7, 1.1),
    wave(18, 22, 7, 1.6, 7, 1.1),
)))

# Ted Lasso: the sign over the door.
EMBLEMS["sign"] = ((236, 202, 104), render(sub(
    rrect(4, 8, 30, 21, 1.5),
    rect(7, 11.5, 27, 13),
    rect(7, 15.5, 24, 17),
)))

# Miles: a web, drawn from the corner the way he shoots it.
EMBLEMS["web"] = ((226, 100, 100), render(union(
    # A whole web rather than the corner-shot version, which loses its rings to
    # the crop and comes back looking like a shopping basket.
    seg(17, 14, 28.09, 20.40, 1.0),
    seg(17, 14, 17.00, 26.80, 1.0),
    seg(17, 14, 5.91, 20.40, 1.0),
    seg(17, 14, 5.91, 7.60, 1.0),
    seg(17, 14, 17.00, 1.20, 1.0),
    seg(17, 14, 28.09, 7.60, 1.0),
    ring(17, 14, 4.5, 1.0),
    ring(17, 14, 8.6, 1.0),
    ring(17, 14, 12.7, 1.0),
)))

# Bourdain: a bowl, and the conversation that happens over it.
EMBLEMS["bowl"] = ((236, 172, 104), render(union(
    sub(disc(17, 16, 10.5), rect(0, 0, 34, 16)),
    rect(6, 15, 28, 16.4),
    seg(21, 13, 30, 5, 1.1),
    seg(23, 13.5, 32, 6, 1.1),
    wave(10, 15, 9, 1.4, 6, 1.0),
)))

# Ikiru: the swing, in the snow.
EMBLEMS["swing"] = ((174, 190, 210), render(union(
    rect(4, 3, 30, 4.2),
    seg(9, 4, 9, 18, 1.0),
    seg(25, 4, 25, 18, 1.0),
    rrect(6, 18, 28, 20.4, 1.0),
    *[disc(x, y, 0.8) for x, y in
      [(6, 24), (13, 26), (20, 23.5), (27, 25.5), (10, 22), (30, 22)]],
)))

# A Silent Voice: the notebook they pass back and forth.
EMBLEMS["notebook"] = ((216, 174, 184), render(sub(
    rrect(6, 4, 28, 25, 1.5),
    *[rect(11, 8 + i * 3.4, 24, 9.2 + i * 3.4) for i in range(4)],
    *[disc(8.5, 7 + i * 4.2, 1.0) for i in range(5)],
)))

# Spider-Verse: one shape, printed three times slightly out of register.
EMBLEMS["glitch"] = ((228, 122, 198), render(union(
    *[sub(rrect(6 + d, 6 + d, 24 + d, 22 + d, 2), rrect(9 + d, 9 + d, 21 + d, 19 + d, 1))
      for d in (0, 4, 8)],
)))

# The Bear: the knife, which is the wound and the cure.
EMBLEMS["knife"] = ((190, 198, 208), render(union(
    # Tip at the left, spine rising to the heel, handle at the right. The
    # previous version was a quadrilateral so close to a rectangle that it
    # rendered as a grey bar.
    # The blade has to be several times deeper than the handle. Made merely
    # thicker, the two read as one cylinder and the emblem becomes a rolling
    # pin -- which is a different kitchen entirely.
    quad((2.0, 16.5), (10, 10.0), (22.0, 10.0), (22.0, 17.2)),
    rect(21.6, 10.4, 23.0, 16.0),
    rrect(23.0, 11.6, 32, 14.6, 1.3),
)))

# Haikyuu: the ball, and the net it keeps crossing.
EMBLEMS["ball"] = ((236, 212, 154), render(sub(
    disc(17, 14, 11),
    ring(6, 6, 12.5, 1.2),
    ring(30, 8, 11.5, 1.2),
    ring(20, 30, 13.0, 1.2),
)))

# One Piece: the hat, and the promise attached to it.
EMBLEMS["strawhat"] = ((238, 198, 122), render(union(
    sub(disc(17, 15, 8.2), rect(0, 15, 34, 28)),
    ellipse(17, 15.6, 15.5, 3.6),
    rect(9, 12.6, 25, 14.4),
)))


def build(art):
    """-> alpha per pixel."""
    rows = art.strip("\n").split("\n")
    w = max(len(r) for r in rows)
    rows = [r.ljust(w, ".") for r in rows]
    h = len(rows)
    solid = lambda x, y: 0 <= x < w and 0 <= y < h and rows[y][x] == "#"

    px = [[0.0] * w for _ in range(h)]
    for y in range(h):
        for x in range(w):
            if not solid(x, y):
                continue
            # Lit wherever the shape turns away from the light. Without it a
            # flat fill is a silhouette, and a silhouette of a teacup at this
            # size is a blob.
            rim = not solid(x - 1, y) or not solid(x, y - 1)
            px[y][x] = EDGE if rim else FACE
    return px, w, h


def crop(px, w, h):
    """Trim to the ink, so every emblem is its own size and centring is honest."""
    ys = [y for y in range(h) if any(px[y])]
    xs = [x for x in range(w) if any(px[y][x] for y in range(h))]
    if not ys or not xs:
        return px, w, h
    y0, y1, x0, x1 = ys[0], ys[-1], xs[0], xs[-1]
    # Keep the row count even so the half-block pairing has no orphan row.
    if (y1 - y0 + 1) % 2:
        y1 = min(h - 1, y1 + 1) if y1 + 1 < h else y1
        if (y1 - y0 + 1) % 2:
            y0 = max(0, y0 - 1)
    out = [row[x0:x1 + 1] for row in px[y0:y1 + 1]]
    return out, x1 - x0 + 1, y1 - y0 + 1


def cells(px, w, h):
    out = []
    for cy in range(0, h, 2):
        for x in range(w):
            t = px[cy][x]
            b = px[cy + 1][x] if cy + 1 < h else 0.0
            out.append((" " if t == 0 and b == 0 else "▀", round(t * 255), round(b * 255)))
    return out, w, (h + 1) // 2


SHEET = r'''
/// Every emblem side by side, for looking at what the script actually drew.
/// It composes these blind; the only way to know a teacup reads as a teacup is
/// to print it.
pub fn sheet() -> String {
    const BG: (u8, u8, u8) = (8, 9, 11);
    let blend = |c: (u8, u8, u8), a: u8| {
        let a = a as f32 / 255.0;
        (
            (BG.0 as f32 + (c.0 as f32 - BG.0 as f32) * a) as u8,
            (BG.1 as f32 + (c.1 as f32 - BG.1 as f32) * a) as u8,
            (BG.2 as f32 + (c.2 as f32 - BG.2 as f32) * a) as u8,
        )
    };
    let mut out = String::new();
    for row in EMBLEMS.chunks(4) {
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
                out.push_str(&" ".repeat(38usize.saturating_sub(a.cols as usize)));
            }
            out.push('\n');
        }
        for m in row {
            out.push_str(&format!("\x1b[38;2;150;155;165m{:<38}\x1b[0m", m.id));
        }
        out.push_str("\n\n");
    }
    out
}
'''


def main():
    body, index = [], []
    for eid, (rgb, art) in EMBLEMS.items():
        px, w, h = build(art)
        px, w, h = crop(px, w, h)
        cs, cols, rows = cells(px, w, h)
        name = eid.upper().replace("-", "_")
        packed = ",".join(f"('{ch}',{f},{b})" for ch, f, b in cs)
        body.append(f"static {name}: [Cell; {len(cs)}] = [{packed}];")
        index.append(
            f'    Emblem {{ id: "{eid}", rgb: {rgb}, '
            f"art: Art {{ cols: {cols}, rows: {rows}, cells: &{name} }} }},"
        )

    src = [
        "//! GENERATED by portfolio/scripts/emblems.py -- do not edit.",
        "//!",
        "//! One drawing per entry in the taste sheet. Alphas rather than colours,",
        "//! so a plate can be dimmed or lit at runtime without regenerating it.",
        "",
        "/// glyph, foreground alpha, background alpha.",
        "pub type Cell = (char, u8, u8);",
        "",
        "pub struct Art {",
        "    pub cols: u16,",
        "    pub rows: u16,",
        "    pub cells: &'static [Cell],",
        "}",
        "",
        "pub struct Emblem {",
        "    pub id: &'static str,",
        "    pub rgb: (u8, u8, u8),",
        "    pub art: Art,",
        "}",
        "",
        *body,
        "",
        f"pub static EMBLEMS: [Emblem; {len(EMBLEMS)}] = [",
        *index,
        "];",
        "",
        "pub fn find(id: &str) -> Option<&'static Emblem> {",
        "    EMBLEMS.iter().find(|e| e.id == id)",
        "}",
        SHEET,
    ]
    with open(OUT, "w") as f:
        f.write("\n".join(src))
    print(f"wrote {OUT}: {len(EMBLEMS)} emblems")


if __name__ == "__main__":
    main()
