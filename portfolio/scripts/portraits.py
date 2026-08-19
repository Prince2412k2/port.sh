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

# The set asked for. `stipple` and `dot` are separate classes in chafa; there
# is no `dot-stipple`. Sextants need a font with the 1FB00 block -- which is
# the trade for this much detail per cell, and every modern terminal font has
# had them for years.
SYMBOLS = "sextant+ascii+braille+border+dot+stipple"

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

# id, source file, columns, rows, colours.
#
# Sizes are generous on purpose: this is a museum wall now, not a contact
# sheet, and the whole point of sextants is that detail survives being small
# enough to fit. Each plate is baked at exactly the size it is drawn -- there
# is no resampling a sextant, so a second size means a second bake.
PLATES = [
    ("snufkin-home", "0a9a46ba2536b06ab2c5e8841e6aacd3.gif", 52, 26, QUANTISED),
    ("snufkin", "0a9a46ba2536b06ab2c5e8841e6aacd3.gif", 64, 32, QUANTISED),
    ("one-piece", "onepeace.gif", 64, 32, QUANTISED),
    ("bourdain", "bourdain.jpeg", 64, 32, FULL),
    ("iroh", "iroh.png", 64, 32, FULL),
    ("ted", "ted.jpg", 64, 32, FULL),
    ("miles", "miles.jpeg", 64, 32, FULL),
    ("little-prince", "llprince.jpeg", 64, 32, FULL),
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


def render(src: str, cols: int, rows: int, colours: str) -> str:
    """One frame of ANSI from chafa."""
    return subprocess.run(
        ["chafa", "-f", "symbols", "-c", colours, "--symbols", SYMBOLS,
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
    out.append("//! plate. Animated sources keep several frames; still ones keep one, and")
    out.append("//! nothing here decodes an image at runtime.")
    out.append("//!")
    out.append("//! Distinct from emblems.rs, which stores coverage rather than colour so a")
    out.append("//! drawing can be tinted against the page. These are photographs; there is")
    out.append("//! nothing to tint.")
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
    for pid, name, cols, rows, colours in PLATES:
        path = ASSETS / name
        if not path.exists():
            print(f"skipping {pid}: no {path}", file=sys.stderr)
            continue

        with tempfile.TemporaryDirectory() as tmp:
            files = coalesce(path, Path(tmp))
            total = len(files)
            if total <= 1:
                picks = [0]
            else:
                n = min(total, MAX_FRAMES)
                picks = sorted({round(k * (total - 1) / (n - 1)) for k in range(n)})
            sheets = [parse(render(files[i], cols, rows, colours)) for i in picks]

        shape, frames = None, []
        for idx, (w, h, flat) in zip(picks, sheets):
            if not flat:
                continue
            # Frames of one animation must agree, or the blitter would need a
            # size per frame. chafa fits to the aspect ratio of each frame
            # independently, and a GIF whose frames differ in size would
            # otherwise produce a ragged array.
            if shape is None:
                shape = (w, h)
            elif (w, h) != shape:
                print(f"{pid}: frame {idx} came out {w}x{h}, want {shape[0]}x{shape[1]}",
                      file=sys.stderr)
                continue
            frames.append(flat)

        if not frames:
            print(f"skipping {pid}: chafa produced nothing", file=sys.stderr)
            continue

        w, h = shape
        sym = pid.upper().replace("-", "_")
        for k, flat in enumerate(frames):
            out.append(f"static {sym}_{k}: [Cell; {len(flat)}] = [{cells(flat)}];")
        joined = ",".join(f"&{sym}_{k}" for k in range(len(frames)))
        out.append(f"static {sym}_F: [&[Cell]; {len(frames)}] = [{joined}];")
        out.append("")
        rgb = tint(frames[0])
        made.append((pid, sym, w, h, rgb))
        print(f"{pid}: {w}x{h}, {len(frames)} frame(s) from {total}, "
              f"-c {colours}, tint {rgb}", file=sys.stderr)

    out.append(f"pub static PORTRAITS: [Portrait; {len(made)}] = [")
    for pid, sym, w, h, rgb in made:
        out.append(f'    Portrait {{ id: "{pid}", cols: {w}, rows: {h}, '
                   f"tint: ({rgb[0]}, {rgb[1]}, {rgb[2]}), frames: &{sym}_F }},")
    out.append("];")
    out.append("")
    out.append("pub fn find(id: &str) -> Option<&'static Portrait> {")
    out.append("    PORTRAITS.iter().find(|p| p.id == id)")
    out.append("}")

    out.append("")

    sys.stdout.write("\n".join(out))


if __name__ == "__main__":
    main()
