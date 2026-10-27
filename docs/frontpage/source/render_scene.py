# SPDX-License-Identifier: Apache-2.0
"""Render the Implexity-exported specimen with Blender 4.5 LTS.

blender -b --python render_scene.py -- specimen.stl specimen.png [samples]
The finish and illumination are art direction, not simulated physical fields.
"""
import bpy, sys, math
from pathlib import Path
from mathutils import Vector

args=sys.argv[sys.argv.index('--')+1:]
mesh_path, output=map(lambda x: str(Path(x).resolve()),args[:2])
samples=int(args[2]) if len(args)>2 else 128
bpy.ops.object.select_all(action='SELECT')
bpy.ops.object.delete(use_global=False)
bpy.ops.wm.stl_import(filepath=mesh_path)
obj=bpy.context.object
obj.name='Implexity · native implicit gyroid'
for p in obj.data.polygons: p.use_smooth=True
# Cosmetic render-only smoothing of sampling-scale surface ripples.
# The exported mesh and its topology report remain unchanged on disk.
smooth=obj.modifiers.new('Render-only sampling smoothing','SMOOTH')
smooth.factor=.55
smooth.iterations=4
mat=bpy.data.materials.new('Neutral surface · illustrative geometry')
mat.use_nodes=True
bs=mat.node_tree.nodes.get('Principled BSDF')
bs.inputs['Base Color'].default_value=(0.29,0.32,0.34,1)
bs.inputs['Metallic'].default_value=.08
bs.inputs['Roughness'].default_value=.52
obj.data.materials.append(mat)

scene=bpy.context.scene
scene.render.engine='CYCLES'
scene.cycles.samples=samples
scene.cycles.use_denoising=True
scene.cycles.seed=27
scene.cycles.max_bounces=8
scene.world.color=(.18,.18,.18)
scene.world.use_nodes=True
scene.world.node_tree.nodes['Background'].inputs[0].default_value=(.8,.8,.8,1)
scene.world.node_tree.nodes['Background'].inputs[1].default_value=.55

def area(name,pos,power,color,size,target=(0,0,0),size_y=None):
    data=bpy.data.lights.new(name,'AREA'); data.energy=power;data.color=color
    data.shape='RECTANGLE';data.size=size;data.size_y=size_y or size
    ob=bpy.data.objects.new(name,data);scene.collection.objects.link(ob);ob.location=pos
    ob.rotation_euler=(Vector(target)-ob.location).to_track_quat('-Z','Y').to_euler()

area('Broad key',(-12,-18,26),14000,(1,1,1),22)
area('Rear fill',(14,8,17),10000,(1,1,1),18,size_y=24)
area('Side fill',(-6,14,4),7000,(1,1,1),14,size_y=30)
area('Front fill',(5,-25,2),3000,(1,1,1),14)

camdata=bpy.data.cameras.new('Camera');cam=bpy.data.objects.new('Camera',camdata)
scene.collection.objects.link(cam);scene.camera=cam
cam.location=(31,-43,30)
cam.rotation_euler=(Vector((0,0,0))-cam.location).to_track_quat('-Z','Y').to_euler()
camdata.type='ORTHO';camdata.ortho_scale=37
scene.render.resolution_x=1800;scene.render.resolution_y=1500;scene.render.resolution_percentage=100
scene.render.film_transparent=True
scene.render.image_settings.file_format='PNG';scene.render.image_settings.color_mode='RGBA'
scene.view_settings.view_transform='AgX'
scene.view_settings.look='AgX - Medium High Contrast'
scene.render.filepath=output
bpy.ops.wm.save_as_mainfile(filepath=str(Path(output).with_suffix('.blend')))
bpy.ops.render.render(write_still=True)
