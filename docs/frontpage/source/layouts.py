# SPDX-License-Identifier: Apache-2.0
"""Generate plain technical figure layouts; rasterize with render_assets.mjs."""
from pathlib import Path

ROOT = Path(__file__).parent
css = '''
*{box-sizing:border-box}html,body{margin:0;padding:0}
body{font-family:Arial,Helvetica,sans-serif;color:#39434a}
.canvas{width:1200px;position:relative;overflow:hidden;background:#fff}
.label{font-size:23px;font-weight:400;line-height:1.3}
img{display:block;object-fit:contain}
'''

def save(name, style, body):
    (ROOT/(name+'.html')).write_text(
        '<!doctype html><html lang="en"><meta charset="utf-8"><title>'+name+
        '</title><style>'+css+style+'</style><body>'+body+'</body></html>',encoding='utf-8')

save('geometry', '''
.canvas{height:420px}
.states{position:absolute;left:10px;right:10px;top:16px;display:flex;gap:20px}
.state{flex:1;min-width:0}.state img{width:100%;height:324px}
.label{font-size:23px;text-align:center;margin-top:14px}
.arrow{position:absolute;top:190px;color:#8c969c;font-size:28px}.a{left:390px}.b{left:790px}
''', '''<div class="canvas">
<div class="states">
<div class="state"><img src="inputs/morph-00.png" alt="Regular gyroid sheet"><div class="label">Periodic TPMS</div></div>
<div class="state"><img src="inputs/morph-01.png" alt="Warped and graded sheet"><div class="label">Spatial adaptation</div></div>
<div class="state"><img src="inputs/morph-02.png" alt="Cooled component with exposed interior"><div class="label">Cooled component</div></div>
</div><div class="arrow a">&#8594;</div><div class="arrow b">&#8594;</div>
</div>''')

save('scaffold', '''
.canvas{height:264px}
.panels{position:absolute;left:28px;right:28px;top:26px;display:flex;gap:30px}
.panel{flex:1;min-width:0}.panel img{width:100%;height:144px}
.label{margin-top:24px;font-size:22px;white-space:nowrap}
''', '''<div class="canvas"><div class="panels">
<div class="panel"><img src="inputs/scaffold-0.png" alt="Symmetry-edge point load"><div class="label">Symmetry-edge point load</div></div>
<div class="panel"><img src="inputs/scaffold-1.png" alt="Half-span point load"><div class="label">Half-span point load</div></div>
<div class="panel"><img src="inputs/scaffold-2.png" alt="Distributed top load"><div class="label">Distributed top load</div></div>
</div></div>''')

save('divertor', '''
.canvas{height:720px}
.column{position:absolute;top:25px;font-size:24px}.initial-label{left:288px}.final-label{left:754px}
.row{position:absolute;left:28px;font-size:24px;line-height:1.45}.row span{display:block;font-size:18px;color:#66717a}.solid-label{top:197px}.coolant-label{top:475px}
img{position:absolute;width:404px;height:284px}.solid{top:68px}.coolant{top:354px}.initial{left:215px}.final{left:676px}
.legend{position:absolute;left:288px;bottom:30px;display:flex;gap:36px;font-size:21px;align-items:center}
.legend span{display:flex;gap:10px;align-items:center}.swatch{width:13px;height:13px;background:#555a63}.swatch.cu{background:#b9bdc2}.swatch.water{background:#3985dc}
''', '''<div class="canvas">
<div class="column initial-label">Initial architecture</div><div class="column final-label">Ageing-aware design</div>
<div class="row solid-label">Solid<span>Half-section</span></div>
<div class="row coolant-label">Coolant<span>Complete volume</span></div>
<img class="solid initial" src="inputs/initial-solid.png" alt="Initial solid">
<img class="solid final" src="inputs/final-solid.png" alt="Optimized solid">
<img class="coolant initial" src="inputs/initial-coolant.png" alt="Initial coolant volume">
<img class="coolant final" src="inputs/final-coolant.png" alt="Optimized coolant volume">
<div class="legend"><span><i class="swatch"></i>W</span><span><i class="swatch cu"></i>CuCrZr</span><span><i class="swatch water"></i>Coolant</span></div>
</div>''')
save('geometry-mobile', '''
.canvas{width:600px;height:1160px;padding:16px 28px}
.state{height:376px;position:relative}
.state img{width:430px;height:340px;margin:0 auto}
.label{font-size:25px;text-align:center;line-height:30px}
''', '''<div class="canvas">
<div class="state"><img src="inputs/morph-00.png" alt="Regular gyroid sheet"><div class="label">Periodic TPMS</div></div>
<div class="state"><img src="inputs/morph-01.png" alt="Warped and graded sheet"><div class="label">Spatial adaptation</div></div>
<div class="state"><img src="inputs/morph-02.png" alt="Cooled component with exposed interior"><div class="label">Cooled component</div></div>
</div>''')

save('scaffold-mobile', '''
.canvas{width:600px;height:830px;padding:28px}
.panel{margin-bottom:32px}.panel img{width:544px;height:184px}
.label{margin-top:12px;font-size:25px}
''', '''<div class="canvas">
<div class="panel"><img src="inputs/scaffold-0.png" alt="Symmetry-edge point load"><div class="label">Symmetry-edge point load</div></div>
<div class="panel"><img src="inputs/scaffold-1.png" alt="Half-span point load"><div class="label">Half-span point load</div></div>
<div class="panel"><img src="inputs/scaffold-2.png" alt="Distributed top load"><div class="label">Distributed top load</div></div>
</div>''')

save('divertor-mobile', '''
.canvas{width:600px;height:680px}
.row{position:absolute;left:24px;font-size:25px}.solid-label{top:20px}.coolant-label{top:345px}
.column{position:absolute;top:67px;font-size:23px}.initial-label{left:60px}.final-label{left:340px}
img{position:absolute;width:280px;height:214px}.initial{left:12px}.final{left:308px}.solid{top:106px}.coolant{top:386px}
.legend{position:absolute;left:24px;bottom:22px;display:flex;gap:30px;font-size:22px}
.legend span{display:flex;align-items:center;gap:8px}.swatch{width:13px;height:13px;background:#555a63}.swatch.cu{background:#b9bdc2}.swatch.water{background:#3985dc}
''', '''<div class="canvas">
<div class="row solid-label">Solid half-sections</div>
<div class="column initial-label">Initial</div><div class="column final-label">Ageing-aware</div>
<img class="solid initial" src="inputs/initial-solid.png" alt="Initial solid">
<img class="solid final" src="inputs/final-solid.png" alt="Optimized solid">
<div class="row coolant-label">Coolant volumes</div>
<img class="coolant initial" src="inputs/initial-coolant.png" alt="Initial coolant volume">
<img class="coolant final" src="inputs/final-coolant.png" alt="Optimized coolant volume">
<div class="legend"><span><i class="swatch"></i>W</span><span><i class="swatch cu"></i>CuCrZr</span><span><i class="swatch water"></i>Coolant</span></div>
</div>''')
print('Wrote desktop and narrow-screen technical figure layouts.')
