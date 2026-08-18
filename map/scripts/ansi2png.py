#!/usr/bin/env python3
"""Render `termap --snapshot` ANSI output to a PNG.

A development aid: it lets you look at the actual pixels the renderer produces
without eyeballing them through a terminal emulator.

    ./target/debug/termap --snapshot 180x48 | python3 scripts/ansi2png.py out.png
"""
import re
import sys

from PIL import Image, ImageDraw, ImageFont

FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
# DejaVu Sans Mono has no Braille Patterns block, so braille is drawn from the
# proportional face. Fine here -- every glyph is placed on a fixed cell grid.
BRAILLE_FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
FONT_SIZE = 16
CELL_W, CELL_H = 10, 20
BG = (8, 9, 11)

SGR = re.compile(r"\x1b\[([0-9;]*)m")

# The standard 256-colour palette: 16 system colours, a 6x6x6 cube, then a
# 24-step grey ramp. The renderer emits the ramp for neutral cells because the
# escape is half the size of a truecolor one, so decoding it correctly here is
# the difference between reviewing the map and reviewing a bug in this script.
_CUBE = (0, 95, 135, 175, 215, 255)
_SYS = [
    (0, 0, 0), (170, 0, 0), (0, 170, 0), (170, 85, 0),
    (0, 0, 170), (170, 0, 170), (0, 170, 170), (170, 170, 170),
    (85, 85, 85), (255, 85, 85), (85, 255, 85), (255, 255, 85),
    (85, 85, 255), (255, 85, 255), (85, 255, 255), (255, 255, 255),
]


def xterm256(i):
    if i < 16:
        return _SYS[i]
    if i < 232:
        i -= 16
        return (_CUBE[i // 36], _CUBE[(i // 6) % 6], _CUBE[i % 6])
    return (8 + 10 * (i - 232),) * 3


def parse(text):
    """-> list of rows, each a list of (char, fg, bg, bold)."""
    rows = []
    fg, bg, bold = (200, 200, 200), None, False

    for raw in text.split("\n"):
        row = []
        pos = 0
        for m in SGR.finditer(raw):
            for ch in raw[pos:m.start()]:
                row.append((ch, fg, bg, bold))
            pos = m.end()

            parts = [p for p in m.group(1).split(";") if p != ""] or ["0"]
            i = 0
            while i < len(parts):
                p = int(parts[i])
                if p == 0:
                    fg, bg, bold = (200, 200, 200), None, False
                elif p == 1:
                    bold = True
                elif p in (38, 48) and i + 1 < len(parts):
                    mode = int(parts[i + 1])
                    if mode == 2 and i + 4 < len(parts):
                        col = tuple(int(parts[i + 2 + k]) for k in range(3))
                        i += 4
                    elif mode == 5 and i + 2 < len(parts):
                        col = xterm256(int(parts[i + 2]))
                        i += 2
                    else:
                        col = (200, 200, 200)
                    if p == 38:
                        fg = col
                    else:
                        bg = col
                i += 1

        for ch in raw[pos:]:
            row.append((ch, fg, bg, bold))
        rows.append(row)

    while rows and not rows[-1]:
        rows.pop()
    return rows


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "snapshot.png"
    rows = parse(sys.stdin.read())
    if not rows:
        sys.exit("no input")

    w = max(len(r) for r in rows)
    img = Image.new("RGB", (w * CELL_W, len(rows) * CELL_H), BG)
    d = ImageDraw.Draw(img)
    font = ImageFont.truetype(FONT, FONT_SIZE)
    braille = ImageFont.truetype(BRAILLE_FONT, FONT_SIZE)
    bold_font = font
    try:
        bold_font = ImageFont.truetype(FONT.replace(".ttf", "-Bold.ttf"), FONT_SIZE)
    except OSError:
        pass

    for y, row in enumerate(rows):
        for x, (ch, fg, bg, bold) in enumerate(row):
            px, py = x * CELL_W, y * CELL_H
            if bg:
                d.rectangle([px, py, px + CELL_W - 1, py + CELL_H - 1], fill=bg)
            if not ch or ch == " ":
                continue
            if 0x2800 <= ord(ch) <= 0x28FF:
                d.text((px, py + 1), ch, font=braille, fill=fg)
            else:
                d.text((px, py + 1), ch, font=bold_font if bold else font, fill=fg)

    img.save(out)
    print(f"wrote {out} ({img.width}x{img.height})")


if __name__ == "__main__":
    main()
