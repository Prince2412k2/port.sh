#!/usr/bin/env python3
"""Bake deterministic glyph coverage from the bundled font, once at build time."""
import io,json,math,sys
from pathlib import Path
from fontTools.ttLib import TTFont
from PIL import Image,ImageDraw,ImageFont

root=Path(__file__).resolve().parents[1]
destination=Path(sys.argv[1]);destination.mkdir(parents=True,exist_ok=True)
font=TTFont(root/'portfolio/data/iosevka-portfolio.woff2');font.flavor=None
characters=sorted(set(code for code in font.getBestCmap() if code>=32 and not 0xD800<=code<=0xDFFF)|{0x2713,0x2717})
assert ord('?') in characters
ttf=io.BytesIO();font.save(ttf);ttf.seek(0)
face=ImageFont.truetype(ttf,26)
columns=64 if len(characters)<=3400 else 128
sw,sh=20,38;width=columns*sw;height=math.ceil(len(characters)*2/columns)*sh
assert width<=4096 and height<=4096,'atlas exceeds GPU budget'
coverage=Image.new('L',(width,height),0);draw=ImageDraw.Draw(coverage)
slots=[]
for bold in range(2):
    for index,code in enumerate(characters):
        slot=bold*len(characters)+index;x=slot%columns*sw;y=slot//columns*sh
        # Anchor at the same baseline and 2x cell resolution for every package.
        if code==0x2713:draw.line([(x+4,y+18),(x+8,y+24),(x+16,y+10)],fill=255,width=2+bold)
        elif code==0x2717:
            draw.line([(x+5,y+12),(x+15,y+25)],fill=255,width=2+bold)
            draw.line([(x+15,y+12),(x+5,y+25)],fill=255,width=2+bold)
        else:draw.text((x+2,y+28),chr(code),font=face,anchor='ls',fill=255,stroke_width=bold)
        slots.append([bold,code,(x+2)/width,(y+2)/height,16/width,34/height])
image=Image.new('RGBA',(width,height),(255,255,255,255));image.putalpha(coverage)
image.save(destination/'glyph-atlas.png',optimize=True)
(destination/'glyph-atlas.json').write_text(json.dumps({'abi':1,'width':width,'height':height,'slots':slots},separators=(',',':')))
print(f'Baked {len(characters)} glyphs, two weights, {width}x{height}')
