#!/usr/bin/env python3
"""Bake the images in assets/ into full-colour terminal art.

    python3 portfolio/scripts/portraits.py > portfolio/src/portraits.rs

Why bake rather than render at runtime: chafa is a C program with its own
image-decoding stack, and shelling out to it once per session -- on a box
that may have a hundred of them -- to redraw art that never changes is a
subprocess and a decode for a constant. Baking makes it a static array.

The emblems in emblems.rs are the other half of this and are *not* replaced.
They store coverage rather than colour, so they can be tinted at runtime and
they cost one byte a cell; these store real pixels. Both exist because a
photograph of Bourdain and a drawn teacup want different things.

Frames: an animated GIF is subsampled to at most MAX_FRAMES evenly spaced
frames. The full 359 frames of the One Piece loop is a megabyte of source for
motion nobody is watching that closely, and every frame is cells that have to
cross a network when they change.
"""

import re
import shutil
import tempfile
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "assets"

# The app's own ground, so anything chafa decides is background blends into
# the page instead of sitting on a black tile of its own.
BG = "#08090b"

# Which symbol classes chafa may choose from, per plate. `stipple` and `dot`
# are separate classes in chafa; there is no `dot-stipple`. Sextants need a
# font with the 1FB00 block -- which is the trade for this much detail per
# cell, and every modern terminal font has had them for years.
#
# A still gets coverage classes only. Sextants, braille and the two dither
# classes all answer the same question -- how much of this cell is ink, and
# where in the cell is it -- which is the question a photograph is asking.
# ASCII answers a different one, and a face assembled out of `%` and `#` reads
# as ASCII art: a medium of its own, and not the one the rest of this room is
# in. It was 5-7% of the ink on the photographs, which is little enough to
# lose and quite enough to notice.
STILL = "sextant+braille+border+dot+stipple"

# The same, plus ASCII, for anything that moves.
#
# Not a bandwidth decision, though it looks like one: measured over both GIFs,
# keeping ASCII means 24% fewer cells change between frames -- the glyphs are
# large flat shapes, so they survive a step where a sextant would be re-picked
# -- but each cell that does change costs a little more, and the two effects
# cancel to within a few percent of each other on the wire. What is left is
# that the objection to ASCII is that you can read it, and at six frames a
# second nobody is reading it.
MOTION = "sextant+ascii+braille+border+dot+stipple"

MAX_FRAMES = 18

# How many colours chafa may use, per plate. This is a bandwidth decision and
# the numbers are not close.
#
# At 64x24 a still frame is 1536 cells. In truecolor, *every* cell differs
# from the frame before -- chafa re-picks glyph and colour per frame, so two
# frames that look nearly identical share almost nothing -- and each changed
# cell costs ~38 bytes of SGR. That is 57 KB a frame, 456 KB/s at 8 fps.
#
# Quantised to the xterm palette, half the cells survive unchanged between
# frames and each one that does change costs ~22 bytes. 17 KB a frame: 3.4x
# cheaper, and both halves of that come free from the same decision.
#
# So anything that moves is quantised and anything that holds still is not.
# A still is paid for exactly once, so it gets the full range; a loop is paid
# for every frame, for as long as somebody is looking at it.
FULL = "full"
QUANTISED = "256"

# The sizes each plate is baked at, largest first.
#
# A size here is a bounding box, not a grid: chafa fits the source's aspect
# ratio inside it, so a tall photograph binds on rows and a wide GIF binds on
# columns. `--size 112x52` gets 74x52 out of Bourdain and 112x31 out of the
# One Piece loop.
#
# Three of them, because a plate cannot be resampled at runtime -- a sextant is
# not a pixel and there is nothing to interpolate -- so the only way to have a
# large picture on a large terminal and any picture at all on a small one is to
# bake each size and pick. `fit` in the generated file does the picking, and
# this is the whole reason assets/ is committed rather than just the output.
#
# The bound that matters is rows, not columns, and not for aesthetic reasons: a
# terminal has columns to spare and never has rows. The museum needs about ten
# of them for the mount, the caption and the index, so a 40-row plate wants a
# 50-row window.
#
# - The large tier is for a maximised browser on a 1080p screen, which has the
#   rows for it. Bounding it at 52 rather than 40 would look generous and
#   select almost never: the upright photographs would come out 52 rows and ask
#   for a 62-row terminal before they could be seen at all.
# - The middle tier is what every plate used to be, full stop.
# - The small tier exists because the middle one is already 32 rows, which is
#   more than a 36-row window has after the caption. Without it a windowed
#   browser or a tmux pane got no picture at all for the four upright works --
#   the tiers have to degrade as well as upgrade, and a small picture beats a
#   caption on an empty wall.
WALL = [(112, 40), (64, 32), (48, 24)]

# Home hangs its portrait beside a text block about 80 columns wide with the
# section's furniture either side, so it has far less to spend than a museum
# wall and gets its own set of bounds. Wider than the largest of these and the
# quietest screen in the app stops being the quietest.
BESIDE_TEXT = [(72, 36), (52, 26), (40, 20)]

# id, source file, colours, symbol classes, size bounds.
PLATES = [
    ("snufkin-home", "0a9a46ba2536b06ab2c5e8841e6aacd3.gif", QUANTISED, MOTION, BESIDE_TEXT),
    ("snufkin", "0a9a46ba2536b06ab2c5e8841e6aacd3.gif", QUANTISED, MOTION, WALL),
    ("one-piece", "onepeace.gif", QUANTISED, MOTION, WALL),
    ("bourdain", "bourdain.jpeg", FULL, STILL, WALL),
    ("iroh", "iroh.png", FULL, STILL, WALL),
    ("ted", "ted.jpg", FULL, STILL, WALL),
    ("miles", "miles.jpeg", FULL, STILL, WALL),
    ("little-prince", "llprince.jpeg", FULL, STILL, WALL),
]

SGR = re.compile(r"\x1b\[([0-9;]*)m")


def frame_count(path: Path) -> int:
    if path.suffix.lower() != ".gif":
        return 1
    out = subprocess.run(
        ["identify", "-format", "%n\n", str(path)],
        capture_output=True, text=True, check=True,
    ).stdout.split()
    return int(out[0]) if out else 1


def coalesce(path: Path, into: Path) -> list:
    """Flatten an animation to one full-canvas PNG per frame.

    An optimised GIF stores most frames as a small patch over the previous
    one, with its own offset and size. Handing chafa `file.gif[7]` gets the
    patch, not the picture -- which showed up as frames of the One Piece loop
    coming out 15x9 and 18x4 against a 18x5 first frame. Coalescing rebuilds
    each frame as the whole canvas first.
    """
    if path.suffix.lower() != ".gif":
        return [path]
    # ImageMagick 7 renamed the tool; 6 is still what most distributions ship.
    tool = shutil.which("magick") or shutil.which("convert")
    if tool is None:
        sys.exit("need ImageMagick (`magick` or `convert`) to split an animation")
    subprocess.run([tool, str(path), "-coalesce", str(into / "f-%04d.png")], check=True)
    return sorted(into.glob("f-*.png"))


def render(src: str, cols: int, rows: int, colours: str, symbols: str) -> str:
    """One frame of ANSI from chafa."""
    return subprocess.run(
        ["chafa", "-f", "symbols", "-c", colours, "--symbols", symbols,
         "--size", f"{cols}x{rows}", "--bg", BG, "-t", "1", str(src)],
        capture_output=True, text=True, check=True,
    ).stdout


# The xterm-256 palette, used only to turn an index back into RGB so the tint
# and the "is this our own background" test can be computed. The index itself
# is what gets baked -- see the note on Ink in the generated file.
def _palette():
    p = [(0, 0, 0), (128, 0, 0), (0, 128, 0), (128, 128, 0), (0, 0, 128),
         (128, 0, 128), (0, 128, 128), (192, 192, 192), (128, 128, 128),
         (255, 0, 0), (0, 255, 0), (255, 255, 0), (0, 0, 255), (255, 0, 255),
         (0, 255, 255), (255, 255, 255)]
    lv = (0, 95, 135, 175, 215, 255)
    for r in lv:
        for g in lv:
            for b in lv:
                p.append((r, g, b))
    p.extend((v, v, v) for v in range(8, 239, 10))
    return p


PALETTE = _palette()


def rgb_of(ink):
    """An Ink as RGB, whichever kind it is."""
    kind, a, b, c = ink
    return PALETTE[a] if kind == "i" else (a, b, c)


def tint(flat) -> tuple:
    """The plate's own colour, for the field drawn behind it.

    The mean is muddy and the single most common colour is usually the
    background, so this takes the mean of the most *saturated* fifth: the
    colour you would name if asked, rather than the colour there is most of.
    """
    px = []
    for _, f, b_ in flat:
        for r, g, b in (rgb_of(f), rgb_of(b_)):
            hi, lo = max(r, g, b), min(r, g, b)
            if hi < 24:
                continue          # near-black, including our own ground
            px.append((hi - lo, r, g, b))
    if not px:
        return (128, 128, 128)
    px.sort(reverse=True)
    top = px[: max(1, len(px) // 5)]
    n = len(top)
    return tuple(sum(p[i] for p in top) // n for i in (1, 2, 3))


def parse(ansi: str):
    """ANSI -> (cols, rows, [(ch, fg_ink, bg_ink)]).

    chafa emits a foreground/background pair and then the glyphs that use it,
    so the colours are carried across cells rather than repeated. Rows are
    ragged only if chafa clipped one, so the grid is padded to the widest.

    Both colour forms are kept as they were written. `-c 256` emits `38;5;N`
    and `-c full` emits `38;2;r;g;b`; converting the first into the second here
    would throw away the entire reason for quantising, because what reaches the
    terminal would be a truecolor escape again.
    """
    grid, row = [], []
    fg = bg = ("c", 0, 0, 0)
    i = 0
    while i < len(ansi):
        m = SGR.match(ansi, i)
        if m:
            parts = [int(p) for p in m.group(1).split(";") if p != ""] or [0]
            j = 0
            while j < len(parts):
                if parts[j] == 0:
                    fg = bg = ("c", 0, 0, 0)
                    j += 1
                elif parts[j] in (38, 48) and parts[j + 1] == 2:
                    ink = ("c", *parts[j + 2:j + 5])
                    fg, bg = (ink, bg) if parts[j] == 38 else (fg, ink)
                    j += 5
                elif parts[j] in (38, 48) and parts[j + 1] == 5:
                    ink = ("i", parts[j + 2], 0, 0)
                    fg, bg = (ink, bg) if parts[j] == 38 else (fg, ink)
                    j += 3
                else:
                    j += 1
            i = m.end()
            continue
        ch = ansi[i]
        i += 1
        if ch == "\n":
            grid.append(row)
            row = []
            continue
        if ch == "\r":
            continue
        row.append((ch, fg, bg))
    if row:
        grid.append(row)

    grid = [r for r in grid if r]
    if not grid:
        return 0, 0, []
    w = max(len(r) for r in grid)
    blank = (" ", ("c", 0, 0, 0), ("c", 0, 0, 0))
    flat = []
    for r in grid:
        flat.extend(r + [blank] * (w - len(r)))
    return w, len(grid), flat


def rust_char(ch: str) -> str:
    if ch == "'":
        return r"'\''"
    if ch == "\\":
        return r"'\\'"
    if ord(ch) < 0x20 or ord(ch) == 0x7F:
        return f"'\\u{{{ord(ch):x}}}'"
    return f"'{ch}'"


def ink_lit(ink) -> str:
    kind, a, b, c = ink
    return f"I({a})" if kind == "i" else f"C({a},{b},{c})"


def cells(flat) -> str:
    return ",".join(
        f"({rust_char(c[0])},{ink_lit(c[1])},{ink_lit(c[2])})" for c in flat
    )


def main() -> None:
    if not ASSETS.is_dir():
        sys.exit(f"no assets directory at {ASSETS}")

    out = []
    out.append("//! GENERATED by portfolio/scripts/portraits.py -- do not edit.")
    out.append("//!")
    out.append("//! Full-colour art baked from assets/ with chafa, one static array per")
    out.append("//! frame. Animated sources keep several frames; still ones keep one, and")
    out.append("//! nothing here decodes an image at runtime.")
    out.append("//!")
    out.append("//! Every plate appears more than once, at a different size each time, and")
    out.append("//! `fit` picks the largest one a given rect can hold. The sizes are baked")
    out.append("//! rather than scaled because there is no scaling a sextant: the glyph is")
    out.append("//! chosen for the pixels it covers, so a different size is a different")
    out.append("//! choice of glyph and a second run of chafa.")
    out.append("//!")
    out.append("//! Distinct from emblems.rs, which stores coverage rather than colour so a")
    out.append("//! drawing can be tinted against the page. These are photographs; there is")
    out.append("//! nothing to tint.")
    out.append("")
    out.append("/// The ground these were baked against.")
    out.append("///")
    out.append("/// chafa was handed this as `--bg`, so it is what filled every")
    out.append("/// transparent pixel in the source. A cell that still carries it is not")
    out.append("/// part of the picture, and `paint::is_ground` skips those so a plate")
    out.append("/// sits on the page instead of on a rectangle of its own.")
    out.append("///")
    out.append("/// Emitted rather than written twice. It is a fact about the bake, not")
    out.append("/// about the theme -- the reader has no way to know it otherwise, and the")
    out.append("/// consumer had drifted to comparing against the live page colour, which")
    out.append("/// is a different number in every theme and the right one in none.")
    out.append(f"pub const BAKED_BG: (u8, u8, u8) = ({int(BG[1:3],16)}, {int(BG[3:5],16)}, {int(BG[5:7],16)});")
    out.append("")
    out.append("/// One colour, as chafa wrote it.")
    out.append("///")
    out.append("/// `I` is an xterm palette index and `C` is truecolor, and the")
    out.append("/// difference survives all the way to the terminal on purpose: an")
    out.append("/// indexed cell goes out as `ESC[38;5;N` (about 22 bytes with its")
    out.append("/// background) where a truecolor one costs about 38. Converting I to C")
    out.append("/// anywhere in here would throw away the reason the animated plates are")
    out.append("/// quantised at all.")
    out.append("#[derive(Clone, Copy)]")
    out.append("pub enum Ink {")
    out.append("    I(u8),")
    out.append("    C(u8, u8, u8),")
    out.append("}")
    out.append("")
    out.append("pub use Ink::{C, I};")
    out.append("")
    out.append("/// glyph, foreground, background.")
    out.append("pub type Cell = (char, Ink, Ink);")
    out.append("")
    out.append("pub struct Portrait {")
    out.append("    pub id: &'static str,")
    out.append("    pub cols: u16,")
    out.append("    pub rows: u16,")
    out.append("    /// The plate's own colour, for the field drawn behind it.")
    out.append("    pub tint: (u8, u8, u8),")
    out.append("    /// One entry per frame. Still images have exactly one.")
    out.append("    pub frames: &'static [&'static [Cell]],")
    out.append("}")
    out.append("")

    made = []
    for pid, name, colours, symbols, bounds in PLATES:
        path = ASSETS / name
        if not path.exists():
            print(f"skipping {pid}: no {path}", file=sys.stderr)
            continue

        # Coalesced once and rendered at every size, rather than once per size.
        # Rebuilding 359 One Piece frames twice is most of this script's
        # runtime for a result that cannot differ.
        with tempfile.TemporaryDirectory() as tmp:
            files = coalesce(path, Path(tmp))
            total = len(files)
            if total <= 1:
                picks = [0]
            else:
                n = min(total, MAX_FRAMES)
                picks = sorted({round(k * (total - 1) / (n - 1)) for k in range(n)})

            # The plate's colour comes from the largest tier and is then shared
            # by all of them. It is the same photograph either way, and the
            # museum blends these across a slide -- a tint that shifted when
            # the terminal was resized would change the colour of the room.
            rgb = None
            for cols, rows in bounds:
                sheets = [parse(render(files[i], cols, rows, colours, symbols))
                          for i in picks]

                shape, frames = None, []
                for idx, (w, h, flat) in zip(picks, sheets):
                    if not flat:
                        continue
                    # Frames of one animation must agree, or the blitter would
                    # need a size per frame. chafa fits to the aspect ratio of
                    # each frame independently, and a GIF whose frames differ
                    # in size would otherwise produce a ragged array.
                    if shape is None:
                        shape = (w, h)
                    elif (w, h) != shape:
                        print(f"{pid} {cols}x{rows}: frame {idx} came out {w}x{h}, "
                              f"want {shape[0]}x{shape[1]}", file=sys.stderr)
                        continue
                    frames.append(flat)

                if not frames:
                    print(f"skipping {pid} at {cols}x{rows}: chafa produced nothing",
                          file=sys.stderr)
                    continue

                w, h = shape
                if rgb is None:
                    rgb = tint(frames[0])
                sym = f"{pid.upper().replace('-', '_')}_{w}X{h}"
                for k, flat in enumerate(frames):
                    out.append(f"static {sym}_{k}: [Cell; {len(flat)}] = [{cells(flat)}];")
                joined = ",".join(f"&{sym}_{k}" for k in range(len(frames)))
                out.append(f"static {sym}_F: [&[Cell]; {len(frames)}] = [{joined}];")
                out.append("")
                made.append((pid, sym, w, h, rgb))
                print(f"{pid}: {w}x{h} (bound {cols}x{rows}), {len(frames)} frame(s) "
                      f"from {total}, -c {colours}, tint {rgb}", file=sys.stderr)

    # Ordered largest first within each plate, which is the order `fit` walks.
    out.append(f"pub static PORTRAITS: [Portrait; {len(made)}] = [")
    for pid, sym, w, h, rgb in made:
        out.append(f'    Portrait {{ id: "{pid}", cols: {w}, rows: {h}, '
                   f"tint: ({rgb[0]}, {rgb[1]}, {rgb[2]}), frames: &{sym}_F }},")
    out.append("];")
    out.append("")
    out.append("/// The largest bake of `id` that fits in `cols` x `rows`.")
    out.append("///")
    out.append("/// `None` when even the smallest does not fit, which is a caller's cue to")
    out.append("/// hang the caption on its own rather than to draw a picture off the side")
    out.append("/// of the screen. A plate cannot be scaled -- a sextant is not a pixel --")
    out.append("/// so this picks between bakes instead of resizing one.")
    out.append("pub fn fit(id: &str, cols: u16, rows: u16) -> Option<&'static Portrait> {")
    out.append("    PORTRAITS")
    out.append("        .iter()")
    out.append("        .filter(|p| p.id == id && p.cols <= cols && p.rows <= rows)")
    out.append("        .max_by_key(|p| p.cols as u32 * p.rows as u32)")
    out.append("}")
    out.append("")
    out.append("/// The smallest bake of `id`, whatever the terminal is.")
    out.append("///")
    out.append("/// For the questions that are about the plate rather than about the size it")
    out.append("/// is drawn at: whether this work has a picture at all, what colour it is,")
    out.append("/// and whether it moves. Every tier of a plate shares those answers.")
    out.append("pub fn find(id: &str) -> Option<&'static Portrait> {")
    out.append("    PORTRAITS")
    out.append("        .iter()")
    out.append("        .filter(|p| p.id == id)")
    out.append("        .min_by_key(|p| p.cols as u32 * p.rows as u32)")
    out.append("}")

    out.append("")

    sys.stdout.write("\n".join(out))


if __name__ == "__main__":
    main()
