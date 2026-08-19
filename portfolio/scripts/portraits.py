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

MAX_FRAMES = 14

# id, source file, columns, rows. Sizes are what the layout has room for:
# the home portrait gets its own column, the shelf plates are half that.
PLATES = [
    # The home page gives the portrait its own column, so it is baked twice:
    # once large for there, once at shelf size like every other plate. Scaling
    # one of them at runtime would mean resampling glyphs, which is not a
    # thing you can do to a sextant.
    ("snufkin-home", "0a9a46ba2536b06ab2c5e8841e6aacd3.gif", 34, 17),
    ("snufkin", "0a9a46ba2536b06ab2c5e8841e6aacd3.gif", 18, 9),
    ("bourdain", "bourdain.jpeg", 18, 9),
    ("iroh", "iroh.png", 18, 9),
    ("ted", "ted.jpg", 18, 9),
    ("miles", "miles.jpeg", 18, 9),
    ("little-prince", "llprince.jpeg", 18, 9),
    ("one-piece", "onepeace.gif", 18, 9),
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


def render(src: str, cols: int, rows: int) -> str:
    """One frame of ANSI from chafa."""
    return subprocess.run(
        ["chafa", "-f", "symbols", "-c", "full", "--symbols", SYMBOLS,
         "--size", f"{cols}x{rows}", "--bg", BG, "-t", "1", str(src)],
        capture_output=True, text=True, check=True,
    ).stdout


def parse(ansi: str):
    """ANSI -> (cols, rows, [(ch, fr,fg,fb, br,bg,bb)]).

    chafa emits a foreground/background pair and then the glyphs that use it,
    so the colours are carried across cells rather than repeated. Rows are
    ragged only if chafa clipped one, so the grid is padded to the widest.
    """
    grid, row = [], []
    fg = bg = (0, 0, 0)
    i = 0
    while i < len(ansi):
        m = SGR.match(ansi, i)
        if m:
            parts = [int(p) for p in m.group(1).split(";") if p != ""] or [0]
            j = 0
            while j < len(parts):
                if parts[j] == 0:
                    fg = bg = (0, 0, 0)
                    j += 1
                elif parts[j] in (38, 48) and parts[j + 1] == 2:
                    rgb = tuple(parts[j + 2:j + 5])
                    if parts[j] == 38:
                        fg = rgb
                    else:
                        bg = rgb
                    j += 5
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
        row.append((ch, *fg, *bg))
    if row:
        grid.append(row)

    grid = [r for r in grid if r]
    if not grid:
        return 0, 0, []
    w = max(len(r) for r in grid)
    blank = (" ", 0, 0, 0, 0, 0, 0)
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


def cells(flat) -> str:
    return ",".join(
        f"({rust_char(c[0])},{c[1]},{c[2]},{c[3]},{c[4]},{c[5]},{c[6]})" for c in flat
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
    out.append("/// glyph, then foreground rgb, then background rgb.")
    out.append("pub type Cell = (char, u8, u8, u8, u8, u8, u8);")
    out.append("")
    out.append("pub struct Portrait {")
    out.append("    pub id: &'static str,")
    out.append("    pub cols: u16,")
    out.append("    pub rows: u16,")
    out.append("    /// One entry per frame. Still images have exactly one.")
    out.append("    pub frames: &'static [&'static [Cell]],")
    out.append("}")
    out.append("")

    made = []
    for pid, name, cols, rows in PLATES:
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
            sheets = [parse(render(files[i], cols, rows)) for i in picks]

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
        made.append((pid, sym, w, h, len(frames)))
        print(f"{pid}: {w}x{h}, {len(frames)} frame(s) from {total}", file=sys.stderr)

    out.append(f"pub static PORTRAITS: [Portrait; {len(made)}] = [")
    for pid, sym, w, h, _ in made:
        out.append(f'    Portrait {{ id: "{pid}", cols: {w}, rows: {h}, frames: &{sym}_F }},')
    out.append("];")
    out.append("")
    out.append("pub fn find(id: &str) -> Option<&'static Portrait> {")
    out.append("    PORTRAITS.iter().find(|p| p.id == id)")
    out.append("}")

    out.append("")

    sys.stdout.write("\n".join(out))


if __name__ == "__main__":
    main()
