# SPDX-License-Identifier: Apache-2.0
"""Assemble fixed-camera native geometry renders into a forward/reverse morph."""
from pathlib import Path
import argparse
from PIL import Image, ImageDraw, ImageFont

p=argparse.ArgumentParser();p.add_argument('frames',type=Path);p.add_argument('output',type=Path);a=p.parse_args()
font=ImageFont.truetype('C:/Windows/Fonts/arial.ttf',23)
frames=[]
for path in sorted(a.frames.glob('morph-*.png')):
    render=Image.open(path).convert('RGBA');render.thumbnail((840,700),Image.Resampling.LANCZOS)
    frame=Image.new('RGB',(900,770),'white');frame.paste(render,((900-render.width)//2,0),render)
    d=ImageDraw.Draw(frame)
    d.text((36,726),'Periodic TPMS',fill='#39434a',font=font)
    d.text((864,726),'Cooled manifold',fill='#39434a',font=font,anchor='ra')
    frac=len(frames)/24
    d.line((36,710,864,710),fill='#d7dde0',width=2)
    d.ellipse((36+828*frac-4,706,36+828*frac+4,714),fill='#53636e')
    frames.append(frame)
assert len(frames)==25
# A common palette avoids color flicker; reverse playback describes geometry, not solver iterations.
palette=frames[-1].quantize(colors=192)
frames=[f.quantize(palette=palette,dither=Image.Dither.NONE) for f in frames]
sequence=frames+frames[-2:0:-1]
durations=[140]*len(sequence);durations[0]=1200;durations[24]=1800
sequence[0].save(a.output,save_all=True,append_images=sequence[1:],duration=durations,loop=0,optimize=True,disposal=1)
print(a.output,a.output.stat().st_size)
