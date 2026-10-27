# SPDX-License-Identifier: Apache-2.0
"""Extract selected source panels without recoloring or retouching them.

python prepare_figures.py --scaffold ssrn-7350458.pdf --fusion ssrn-7596080.pdf
Requires PyMuPDF and Pillow. Panel locations refer to the supplied manuscript versions.
"""
import argparse, io, json, hashlib
from pathlib import Path
import pymupdf
from PIL import Image, ImageDraw, ImageFont

parser=argparse.ArgumentParser()
parser.add_argument('--scaffold',type=Path,required=True)
parser.add_argument('--fusion',type=Path,required=True)
args=parser.parse_args()
out=Path(__file__).parent/'inputs';out.mkdir(exist_ok=True)

def blocks(doc,page):
    return [b for b in doc[page-1].get_text('dict')['blocks'] if b['type']==1]

a=pymupdf.open(args.scaffold);b=pymupdf.open(args.fusion)
fig6=Image.open(io.BytesIO(blocks(a,11)[0]['image']))
assert fig6.size==(3317,1395), 'Unexpected manuscript figure dimensions; inspect before cropping.'
boxes=[(96,241,991,543),(96,663,991,963),(96,1079,991,1379)]
for i,box in enumerate(boxes): fig6.crop(box).save(out/f'scaffold-{i}.png')
for name,page,index in [('initial-solid',3,8),('initial-coolant',3,12),('final-solid',4,11),('final-coolant',4,15)]:
    data=blocks(b,page)[index]['image']
    im=Image.open(io.BytesIO(data));im.save(out/f'{name}.png')

# Figure 8 contains the actual reported sequence, every fifth epoch.
# No interpolation between geometric states is introduced.
fig8=Image.open(io.BytesIO(blocks(a,11)[2]['image'])).convert('RGB')
assert fig8.size==(3483,1548), 'Unexpected sequence figure dimensions.'
xs=[(17,615),(660,1258),(1303,1901),(1946,2544),(2589,3186)]
ys=[(97,300),(405,608),(712,916),(1020,1223),(1328,1531)]
fonts=Path(__file__).parent/'fonts'
sans=ImageFont.truetype(str(fonts/'Manrope.ttf'),21)
mono=ImageFont.truetype(str(fonts/'IBMPlexMono-Regular.ttf'),14)
frames=[]
for i in range(25):
    x0,x1=xs[i%5];y0,y1=ys[i//5]
    frame=Image.new('RGB',(800,390),'#ffffff');d=ImageDraw.Draw(frame)
    d.text((38,25),'Unheated point-load optimization',font=sans,fill='#39434a')
    panel=fig8.crop((x0,y0,x1,y1)).resize((724,246),Image.Resampling.LANCZOS)
    frame.paste(panel,(38,78))
    d.line((38,345,762,345),fill='#dce0e3',width=2)
    d.line((38,345,38+724*i/24,345),fill='#5e6c75',width=3)
    d.text((38,359),'Initial',font=mono,fill='#66717a')
    d.text((720,359),'Final',font=mono,fill='#66717a')
    frames.append(frame)
assets=Path(__file__).parents[2]/'assets';assets.mkdir(exist_ok=True)
frames[0].save(assets/'scaffold-evolution.gif',save_all=True,append_images=frames[1:],loop=0,duration=[1500]+[650]*23+[3000],disposal=2,optimize=True)
manifest={
    'scaffold_pdf_sha256':hashlib.sha256(args.scaffold.read_bytes()).hexdigest(),
    'fusion_pdf_sha256':hashlib.sha256(args.fusion.read_bytes()).hexdigest(),
    'scaffold_figure':{'page':11,'figure':6,'image_pixels':[3317,1395],'crop_boxes':boxes},
    'fusion_panels':{'initial':{'page':3,'figure':1,'image_indices':[8,12]},'final':{'page':4,'figure':2,'image_indices':[11,15]}},
    'animation':{'page':11,'figure':8,'epochs':list(range(0,121,5)),'x_intervals':xs,'y_intervals':ys,'interpolated_states':False},
    'processing':'Extraction, cropping, rescaling, and relabelling only. Original geometry and color values retained.'
}
(out/'provenance.json').write_text(json.dumps(manifest,indent=2)+'\n')
print('Prepared seven source panels and provenance manifest.')
