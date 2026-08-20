#!/usr/bin/env python3
"""Bake the certification badge into portfolio/src/cert.rs.

    python3 portfolio/scripts/cert.py > portfolio/src/cert.rs

Drawn, not baked from the PNG. The badge in `assets/claude-cert.png` is a
decagon with three lines of type on it, and the type is the whole point of a
credential -- run through chafa at the size this has to fit, the plate comes out
as an orange blob and the words are gone completely. That was measured before
this script existed, not assumed.

So the plate is composed from shape functions, the same way the taste emblems
and the project marks are, and the words are left to the terminal: the renderer
draws them as real text over the plate, in the real font, at a real size. A
letter that a font already knows how to draw is not a letter worth dithering.

Two-tone and half-blocked. Every pixel is background, plate or ink, and a cell
is two pixels stacked -- `?` with the upper as foreground and the lower as
background -- so the grid is square on screen and a diagonal edge gets twice the
vertical resolution to turn on.

The QR comes from `coffee.py`'s encoder, imported rather than copied: that one
decodes every code it emits and refuses to write one that does not read back.
A verification link that does not scan is worse than no link, and there is no
reason for this project to own two QR encoders.
"""

import math
import sys

from coffee import encode

# What the badge says, and where it can be checked. Both live here rather than
# in `data/`, unlike the rest of the copy on the landing page. The URL is baked
# into a QR matrix a few lines down, and a second copy of it in a text file
# would be a copy that can disagree with the code -- which is the one failure
# `coffee.py` exists to make impossible.
NAME = "Claude Certified Architect"
TIER = "FOUNDATIONS"
ISSUER = "Anthropic"
URL = "https://www.credly.com/badges/64147a78-2d91-4382-9321-aba7eb052186/public_url"
SHOWN = "credly.com/badges/64147a78"

# Anthropic's own two, sampled from the PNG rather than guessed.
PLATE = (0xD9, 0x77, 0x57)
INK = (0xFA, 0xF9, 0xF5)

# Pixels. Square on screen, so the height is also the number of cell rows
# doubled -- keep it even. Forty because the code underneath it is 33 modules
# and four of quiet zone either side: the badge and the link want to be the
# same width or the panel looks like two panels.
W, H = 40, 40

SIDES = 10

# The mark, as one character rather than as drawn rays.
#
# It was drawn first, as ten tapered petals sampled on the grid, and at the size
# that fits here it came out as a notched blob -- twenty pixels across cannot
# hold ten rays and the gaps between them. Made big enough to resolve, it took
# the whole plate and left nowhere for the type.
#
# U+2733 is an eight-spoked asterisk, which is the shape, and the font draws it
# at whatever size the terminal is set to. Same argument as the words: detail a
# font already knows is not detail worth dithering. What is drawn here is the
# plate, because a plate is a shape and not a character.
MARK = "\u2733"


def polygon(cx, cy, r, n, turn):
    """Inside a regular n-gon. `turn` in turns, 0 puts a vertex due east."""
    pts = [
        (
            cx + r * math.cos(2 * math.pi * (i / n + turn)),
            cy + r * math.sin(2 * math.pi * (i / n + turn)),
        )
        for i in range(n)
    ]

    def f(x, y):
        # Convex, so a point is inside when it is on the same side of every
        # edge. Cheaper and more robust than a ray cast, and this shape is
        # convex by construction.
        for i in range(n):
            ax, ay = pts[i]
            bx, by = pts[(i + 1) % n]
            if (bx - ax) * (y - ay) - (by - ay) * (x - ax) < 0:
                return False
        return True

    return f


def render():
    """The plate as a grid of `.` background, `p` plate, `w` ink."""
    cx, cy = W / 2, H / 2
    r = W / 2

    # Flat side up and flat side down, like the badge. With ten sides that is
    # the unrotated polygon: vertices land either side of the vertical, and the
    # shape is 5% shorter than it is wide, which is why the full width fits a
    # square grid without the top and bottom being sliced off by it.
    plate = polygon(cx, cy, r, SIDES, 0.0)

    grid = []
    for y in range(H):
        # Sampled at pixel centres, so the shape is symmetric about the middle
        # of the grid rather than about a corner of it.
        grid.append("".join("p" if plate(x + 0.5, y + 0.5) else "." for x in range(W)))
    return grid


def spread(s):
    """`FOUNDATIONS` as `F O U N D A T I O N S`, the way the badge sets it."""
    return " ".join(s)


def fit(grid, row, text):
    """Centre `text` on cell row `row`, and refuse if the plate is too narrow.

    Type that runs off the edge of the plate is the failure this catches. It
    would look like a bug in the renderer and it would be a bug here, in a
    number that somebody adjusted -- so it is checked while the numbers are
    still in front of whoever changed them.
    """
    lo, hi = W, -1
    for y in (row * 2, row * 2 + 1):
        line = grid[y]
        for x, c in enumerate(line):
            if c != ".":
                lo, hi = min(lo, x), max(hi, x)
    room = hi - lo + 1
    if hi < lo or len(text) > room - 2:
        raise SystemExit(
            f"`{text}` is {len(text)} wide and the plate is {max(room, 0)} "
            f"at cell row {row}. Move the row, shorten the line, or widen W."
        )
    return lo + (room - len(text)) // 2


def layout(grid, rows):
    """The three lines, and where each one sits.

    Placed against the shape rather than at fractions of it: `fit` measures how
    wide the plate actually is on that row and refuses a line that would hang
    off it.
    """
    return [
        (row, fit(grid, row, text), text)
        for row, text in (
            (3, spread(TIER)),
            (rows // 2 - 3, MARK),
            (rows // 2 + 2, "Claude Certified"),
            (rows // 2 + 4, "Architect"),
        )
    ]


def main():
    grid = render()
    rows = H // 2

    placed = layout(grid, rows)

    qr = encode(URL)

    out = sys.stdout.write
    out(
        "//! GENERATED by portfolio/scripts/cert.py -- do not edit.\n"
        "//!\n"
        "//! The certification badge, drawn rather than photographed. See the\n"
        "//! script for why: at the size this has to fit, a chafa bake of the\n"
        "//! real PNG loses every word on it, and the words are the credential.\n"
        "//!\n"
        "//! `pixels` is the plate, half-blocked -- one cell is two rows of this\n"
        "//! grid -- and `words` is everything with detail in it: the three lines\n"
        "//! and the mark, set by the terminal in its own font over the top.\n"
        "//! A character the font already knows is not one worth dithering.\n\n"
        "/// The badge as a grid: `.` is the page and `p` the plate.\n"
        "/// Two rows to a cell, top to bottom.\n"
        "pub struct Badge {\n"
        "    /// Cells across, and the length of every row in `pixels`.\n"
        "    pub w: usize,\n"
        "    /// Cells down. `pixels` has twice this many rows.\n"
        "    pub h: usize,\n"
        "    pub pixels: &'static [&'static str],\n"
        "    /// Type over the plate: cell row, cell column, and the line. In\n"
        "    /// the terminal's own font, on the plate's colour.\n"
        "    pub words: &'static [(usize, usize, &'static str)],\n"
        "}\n\n"
    )

    out(f"pub const PLATE: (u8, u8, u8) = ({PLATE[0]}, {PLATE[1]}, {PLATE[2]});\n")
    out(f"pub const INK: (u8, u8, u8) = ({INK[0]}, {INK[1]}, {INK[2]});\n\n")

    out("pub const BADGE: Badge = Badge {\n")
    out(f"    w: {W},\n")
    out(f"    h: {rows},\n")
    out("    pixels: &[\n")
    for line in grid:
        out(f'        "{line}",\n')
    out("    ],\n")
    out("    words: &[\n")
    for row, col, text in placed:
        out(f'        ({row}, {col}, "{text}"),\n')
    out("    ],\n")
    out("};\n\n")

    out("/// What it is called, for the one line on the landing page.\n")
    out(f'pub const NAME: &str = "{NAME}";\n')
    out(f'pub const TIER: &str = "{TIER}";\n')
    out(f'pub const ISSUER: &str = "{ISSUER}";\n')
    out("/// Short enough to read off a screen and type.\n")
    out(f'pub const SHOWN: &str = "{SHOWN}";\n\n')

    out(
        "/// The verification link, as a code. Decoded back to the URL by the\n"
        "/// script that wrote it -- see `coffee.rs` for why that is not\n"
        "/// optional. Typed as a tip code because it is the same thing: a\n"
        "/// matrix and the string it stands for.\n"
    )
    out("pub const QR: crate::coffee::Code = crate::coffee::Code {\n")
    out(f"    size: {len(qr)},\n")
    out("    rows: &[\n")
    for row in qr:
        out('        "' + "".join("#" if d else "." for d in row) + '",\n')
    out("    ],\n")
    out('    how: "credly, and it is public",\n')
    out(f'    payload: "{URL}",\n')
    out("};\n")


def look():
    """Print the badge the way a terminal would, for tuning the numbers.

        python3 portfolio/scripts/cert.py --look

    Every shape in here is a number somebody will want to move -- the burst
    radius, where the type sits -- and the loop for that should be one command,
    not a build.
    """
    grid = render()
    rows = H // 2
    words = layout(grid, rows)
    over = {r: (c, t) for r, c, t in words}

    for y in range(rows):
        up, lo = grid[y * 2], grid[y * 2 + 1]
        line = list(
            {("p", "p"): "\u2588", ("w", "w"): "\u2593", (".", "."): " "}.get(
                (a, b), "\u2580" if a != "." else "\u2584"
            )
            for a, b in zip(up, lo)
        )
        if y in over:
            col, text = over[y]
            line[col : col + len(text)] = list(text)
        print(f"{y:2d} |{''.join(line)}|")


if __name__ == "__main__":
    if "--look" in sys.argv:
        look()
    else:
        main()
