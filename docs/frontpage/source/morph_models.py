# SPDX-License-Identifier: Apache-2.0
"""Author a TPMS-to-cooled-component morph with native Implexity controls.

This is a prescribed geometry demonstration, not an optimization trajectory.
The control tensor, CSG features and export reports make every state reproducible.
"""
from pathlib import Path
import argparse, base64, hashlib, json
import numpy as np

def model(t, fine=False):
    # A smooth parameter path, with the same native field representation at every state.
    t=t*t*(3-2*t)
    grid=(9,9,7)
    x,y,z=np.meshgrid(np.linspace(-1,1,grid[0]),np.linspace(-1,1,grid[1]),np.linspace(-1,1,grid[2]),indexing='ij')
    c=np.zeros((20,*grid),dtype='<f8')
    junction=np.exp(-(x*x/.22+y*y/.24+(z+.1)**2/.35))
    near_ports=np.exp(-((np.abs(y)-1)**2/.10+(z-.05)**2/.25))
    c[0]=-2.15+t*(2.4*junction+1.1*near_ports+.65*(1-z))
    c[1]=2-t*2.6*np.exp(-(x*x/.045+(y-.72)**2/.11+z*z/.65))
    c[2]=t*(2.1*np.sin(np.pi*y/2)*np.cos(np.pi*z/2)+.75*np.sin(np.pi*z))
    c[3]=t*(1.4*np.sin(np.pi*x)*np.cos(np.pi*z/2)+.65*np.sin(np.pi*z)*np.cos(np.pi*y/2))
    c[4]=t*(2.0*np.sin(np.pi*x)*np.cos(np.pi*y/2)+.9*np.sin(np.pi*y))
    c[5]=t*(1.1*z-.6*y)
    c[6]=t*(-.8*z+.9*junction)
    c[7]=t*(.65*y-.6*junction)
    # Spatial blends from gyroid to Schwarz-P and diamond basis functions.
    p_weight=t*.62*(1-x)/2*(1+y)/2
    d_weight=t*.72*(1+x)/2*(1+y)/2
    c[8]=p_weight
    c[9]=1-.65*(p_weight+d_weight)
    c[10]=d_weight/np.sqrt(2)
    c[11]=-d_weight/np.sqrt(2)
    c[16]=-1.1+t*(.95*near_ports+.8*junction)
    c[18]=t*.35*np.sin(np.pi*x)*np.sin(np.pi*y)*np.cos(np.pi*z/2)
    raw=c.tobytes()
    nodes={}
    def n(name,kind,params=None,children=(),attrs=None):
        nodes[name]={'kind':kind,'params':params or {},'children':[{'name':f'child_{i}','node':v} for i,v in enumerate(children)]}
        if attrs: nodes[name]['attrs']=attrs
        return name
    n('lattice','lattice.controlled',{'control':{'array':'controls'}},attrs={
        'origin_mm':[-15,-12,-8],'domain_mm':[30,24,16],
        'analysis_grid':[30,24,16],'geometry_grid':[200,160,108] if fine else [150,120,80],
        'control_grid':list(grid),'period_mm':5.8,'thickness_range_mm':[.22,1.0],
        'interface_mm':.12 if fine else .15,'stretch_limit':.34,
        'beta_mask':8,'beta_phase':8,'beta_topology':8})
    n('blank','rounded_box',{'bx_mm':15,'by_mm':12,'bz_mm':8,'radius_mm':.85})
    # A curved inlet divides into two rising outlet branches. Capsule chains are
    # combined by the kernel's polynomial fillet, not by mesh deformation.
    paths=[[(0,-19,-2.8),(0,-9,-2.8),(.8,-4,-2.2),(0,0,-.8)]]
    for side in [-1,1]:
        paths.append([(0,0,-.8),(side*2.8,3,.1),(side*6.5,6.5,1.8),(side*9,11,2.6),(side*9,19,2.6)])
    def sweep(name,radius,blend):
        pieces=[]
        for pidx,path in enumerate(paths):
            for j,(a,b) in enumerate(zip(path,path[1:])):
                pieces.append(n(f'{name}_{pidx}_{j}','capsule',dict(zip(
                    ['ax_mm','ay_mm','az_mm','bx_mm','by_mm','bz_mm','radius_mm'],[*a,*b,radius]))))
        root=pieces[0]
        for j,part in enumerate(pieces[1:]):
            root=n(f'{name}_join_{j}','fillet',{'radius_mm':blend},(root,part),{'op':'union'})
        return root
    outer=sweep('branch_envelope',5.5,2.8)
    n('morphed_envelope','interpolate',{'t':t},('blank',outer))
    n('envelope','intersect',children=('blank','morphed_envelope'))
    root=n('clipped_lattice','intersect',children=('lattice','envelope'))
    if t>0:
        jacket=sweep('channel_wall',3.2*t,.85*t)
        passage=sweep('coolant',2.22*t,.65*t)
        n('with_channel_walls','fillet',{'radius_mm':.32*t},(root,jacket),{'op':'union'})
        root='with_channel_walls'
        for i,(px,py,pz) in enumerate([(0,-11.4,-2.8),(-9,11.4,2.6),(9,11.4,2.6)]):
            n(f'collar_shape_{i}','cylinder',{'radius_mm':4.65*t,'half_height_mm':.75*t})
            n(f'collar_axis_{i}','rotate',{'rx_deg':90},(f'collar_shape_{i}',))
            n(f'collar_{i}','translate',{'dx_mm':px,'dy_mm':py,'dz_mm':pz},(f'collar_axis_{i}',))
            root=n(f'with_collar_{i}','fillet',{'radius_mm':.35*t},(root,f'collar_{i}'),{'op':'union'})
        n('with_passages','difference',children=(root,passage))
        root=n('component','intersect',children=('with_passages','envelope'))
    return {'schema':'implexity-model/1','name':'TPMS to branching cooled manifold — illustrative morph',
        'doc':'Prescribed spatial-control and CSG morph, not a physics solve or optimized result. The envelope branches around a curved coolant network; phase, stretch, basis, thickness, occupancy, sheet/network and residual controls vary locally.',
        'units':'mm','parameters':{},'arrays':{'controls':{'dtype':'float64','shape':list(c.shape),'sha256':hashlib.sha256(raw).hexdigest(),'b64':base64.b64encode(raw).decode()}},
        'nodes':nodes,'root':root}

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('output',type=Path);p.add_argument('--fine',action='store_true');p.add_argument('--frames',type=int,default=3)
    a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
    for i,t in enumerate(np.linspace(0,1,a.frames)):
        (a.output/f'morph-{i:02d}.json').write_text(json.dumps(model(float(t),a.fine),indent=2)+'\n',encoding='utf-8')
    print(f'Wrote {a.frames} native Implexity geometry states.')
