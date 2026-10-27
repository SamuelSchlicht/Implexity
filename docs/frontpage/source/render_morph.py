# SPDX-License-Identifier: Apache-2.0
"""Fixed-camera technical renders of native Implexity mesh states."""
import bpy, sys
from pathlib import Path
from mathutils import Vector

args=sys.argv[sys.argv.index('--')+1:]
folder=Path(args[0]).resolve()
bpy.ops.object.select_all(action='SELECT');bpy.ops.object.delete(use_global=False)
scene=bpy.context.scene
scene.render.engine='BLENDER_WORKBENCH'
scene.render.resolution_x=1200;scene.render.resolution_y=1000;scene.render.resolution_percentage=100
scene.render.film_transparent=True
scene.render.image_settings.file_format='PNG';scene.render.image_settings.color_mode='RGBA'
sh=scene.display.shading
sh.light='STUDIO';sh.studiolight_rotate_z=.4
sh.color_type='SINGLE';sh.single_color=(.43,.47,.50)
sh.show_shadows=True;sh.show_cavity=True;sh.cavity_type='BOTH'
sh.curvature_ridge_factor=1.2;sh.curvature_valley_factor=.85
sh.cavity_ridge_factor=1.1;sh.cavity_valley_factor=1.0
sh.show_specular_highlight=True;sh.show_object_outline=False
scene.display.render_aa='32'
scene.view_settings.view_transform='Standard'
camdata=bpy.data.cameras.new('Fixed comparison camera');cam=bpy.data.objects.new('Camera',camdata)
scene.collection.objects.link(cam);scene.camera=cam
cam.location=(31,-43,30)
cam.rotation_euler=(Vector((0,0,0))-cam.location).to_track_quat('-Z','Y').to_euler()
camdata.type='ORTHO';camdata.ortho_scale=33
for path in sorted(folder.glob('morph-*.stl')):
    bpy.ops.wm.stl_import(filepath=str(path));obj=bpy.context.object
    for p in obj.data.polygons:p.use_smooth=True
    smooth=obj.modifiers.new('Render-only sampling smoothing','SMOOTH');smooth.factor=.55;smooth.iterations=4
    scene.render.filepath=str(path.with_suffix('.png'))
    bpy.ops.render.render(write_still=True)
    bpy.data.objects.remove(obj,do_unlink=True)
