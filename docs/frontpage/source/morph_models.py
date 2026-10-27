# SPDX-License-Identifier: Apache-2.0
"""Author a TPMS-to-cooled-component morph with native Implexity controls.

This is a prescribed geometry demonstration, not an optimization trajectory.
The control tensor, CSG features and export reports make every state reproducible.
"""
from pathlib import Path
import argparse, base64, hashlib, json
import numpy as np

def model(t, fine=False):
    grid=(7,5,5)
    x,y,z=np.meshgrid(np.linspace(-1,1,grid[0]),np.linspace(-1,1,grid[1]),np.linspace(-1,1,grid[2]),indexing='ij')
    c=np.zeros((20,*grid),dtype='<f8')
    c[0]=-2 + t*(1.6*(z+1)/2 + 1.5*np.exp(-((np.abs(x)-.45)**2+(z+.25)**2)/.14))
    c[1]=2
    c[2]=t*1.6*np.sin(np.pi*z)*np.cos(np.pi*x/2)
    c[3]=t*.7*np.sin(np.pi*x)*np.cos(np.pi*z/2)
    c[4]=t*1.8*np.sin(np.pi*x)*np.cos(np.pi*z/2)
    c[5]=t*.8*z
    c[6]=-t*.45*z
    c[7]=-t*.5*z
    c[8]=t*.32*(z+1)/2
    c[9]=1
    c[16]=-2
    raw=c.tobytes()
    nodes={}
    def n(name,kind,params=None,children=(),attrs=None):
        nodes[name]={'kind':kind,'params':params or {},'children':[{'name':f'child_{i}','node':v} for i,v in enumerate(children)]}
        if attrs: nodes[name]['attrs']=attrs
        return name
    n('lattice','lattice.controlled',{'control':{'array':'controls'}},attrs={
        'origin_mm':[-12,-8,-6],'domain_mm':[24,16,12],
        'analysis_grid':[24,16,12],'geometry_grid':[160,108,80] if fine else [96,64,48],
        'control_grid':list(grid),'period_mm':6,'thickness_range_mm':[.17,1.0],
        'interface_mm':.12 if fine else .18,'stretch_limit':.25,
        'beta_mask':8,'beta_phase':8,'beta_topology':8})
    n('envelope','rounded_box',{'bx_mm':12,'by_mm':8,'bz_mm':6,'radius_mm':.7})
    root=n('clipped_lattice','intersect',children=('lattice','envelope'))
    if t>0:
        # Two coolant passages; only the rear half of the roof is retained to expose the lattice.
        for i,cx in enumerate([-5.4,5.4]):
            p={'ax_mm':cx,'ay_mm':-20,'az_mm':-1.5,'bx_mm':cx,'by_mm':20,'bz_mm':-1.5,'radius_mm':2.7*t}
            n(f'jacket_{i}','capsule',p)
            p=dict(p,radius_mm=2.05*t)
            n(f'passage_{i}','capsule',p)
        n('jackets','union',children=('jacket_0','jacket_1'))
        n('passages','union',children=('passage_0','passage_1'))
        n('roof_shape','box',{'bx_mm':12,'by_mm':4,'bz_mm':.55*t})
        n('roof','translate',{'dx_mm':0,'dy_mm':4,'dz_mm':6-.55*t},('roof_shape',))
        n('with_jackets','union',children=(root,'jackets'))
        n('with_roof','union',children=('with_jackets','roof'))
        n('with_passages','difference',children=('with_roof','passages'))
        root=n('component','intersect',children=('with_passages','envelope'))
    return {'schema':'implexity-model/1','name':'TPMS to cooled component — illustrative morph',
        'doc':'Prescribed spatial-control and CSG morph. Not a physics solve or optimized result. The roof is shown partially cut away.',
        'units':'mm','parameters':{},'arrays':{'controls':{'dtype':'float64','shape':list(c.shape),'sha256':hashlib.sha256(raw).hexdigest(),'b64':base64.b64encode(raw).decode()}},
        'nodes':nodes,'root':root}

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('output',type=Path);p.add_argument('--fine',action='store_true');p.add_argument('--frames',type=int,default=3)
    a=p.parse_args();a.output.mkdir(parents=True,exist_ok=True)
    for i,t in enumerate(np.linspace(0,1,a.frames)):
        (a.output/f'morph-{i:02d}.json').write_text(json.dumps(model(float(t),a.fine),indent=2)+'\n',encoding='utf-8')
    print(f'Wrote {a.frames} native Implexity geometry states.')
