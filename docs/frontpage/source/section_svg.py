# SPDX-License-Identifier: Apache-2.0
"""Draw the central planar section of an Implexity-exported STL.

python section_svg.py specimen.stl inputs/section.svg
Requires trimesh and numpy. Uses mesh intersections, not a fitted lattice.
"""
import sys
from pathlib import Path
import numpy as np
import trimesh

mesh = trimesh.load_mesh(sys.argv[1], process=False)
section = mesh.section(plane_origin=(0, 0, 0), plane_normal=(0, 0, 1))
if section is None:
    raise ValueError('The mesh does not intersect the central plane.')
paths = section.discrete
if any(not np.allclose(p[0], p[-1]) for p in paths):
    raise ValueError('The cross-section contains open contours.')
points = np.vstack(paths)
lo, hi = points[:, :2].min(axis=0), points[:, :2].max(axis=0)
scale = 40
pad = 2
width, height = (hi - lo) * scale + pad * 2
commands = []
for p in paths:
    xy = (p[:, :2] - lo) * scale + pad
    xy[:, 1] = height - xy[:, 1]
    commands.append('M' + 'L'.join(f'{x:.3f},{y:.3f}' for x, y in xy) + 'Z')
svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.3f} {height:.3f}">
<title>Central planar section of the exported gyroid sheet</title>
<path fill="#40494f" fill-rule="evenodd" d="{' '.join(commands)}"/>
</svg>'''
Path(sys.argv[2]).write_text(svg, encoding='utf-8')
print(f'Wrote {len(paths)} closed intersection contours.')
