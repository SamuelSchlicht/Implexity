// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

pub mod field;
pub mod mesh;
pub mod raster;
pub mod region;
pub mod shade;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};

pub use crate::RenderError as SceneError;
pub use field::{Colormap, GridField};
pub use mesh::{SceneMesh, load_stl, weld};
pub use raster::{GBuffer, OrthoCamera, rasterize};
pub use region::{Convex, Region};
pub use shade::{Colouring, Hatch, LayerStyle, ShadeOptions};

pub const SCHEMA: &str = "implexity-mesh-scene/1";
pub const VIEW_SCHEMA: &str = "implexity-mesh-scene-view/1";
pub const MAX_SIDE_PX: usize = 8192;
pub const MAX_RASTER_PIXELS: usize = 80_000_000;
pub const SUPERSAMPLE: [usize; 3] = [1, 2, 3];
pub const MAX_LAYERS: usize = 32;
pub const CUT_TOLERANCE_MM: f64 = 1e-3;

pub(crate) fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub(crate) fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

pub(crate) fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn invalid(m: impl Into<String>) -> SceneError {
    SceneError::Invalid(m.into())
}

pub(crate) fn vec3(v: Option<&Value>, label: &str) -> Result<[f64; 3], SceneError> {
    let a = v.and_then(Value::as_array).filter(|a| a.len() == 3);
    let a = a.ok_or_else(|| invalid(format!("{label} must be three numbers")))?;
    let mut out = [0.0; 3];
    for (i, x) in a.iter().enumerate() {
        out[i] = x
            .as_f64()
            .filter(|x| x.is_finite())
            .ok_or_else(|| invalid(format!("{label} must be three finite numbers")))?;
    }
    Ok(out)
}

pub(crate) fn number_array(v: Option<&Value>, label: &str) -> Result<Vec<f64>, SceneError> {
    let a = v.and_then(Value::as_array).filter(|a| !a.is_empty());
    let a = a.ok_or_else(|| invalid(format!("{label} must be a non-empty array of numbers")))?;
    a.iter()
        .map(|x| {
            x.as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| invalid(format!("{label} must be finite numbers")))
        })
        .collect()
}

fn box_of(v: Option<&Value>, label: &str) -> Result<[[f64; 3]; 2], SceneError> {
    let a = v.and_then(Value::as_array).filter(|a| a.len() == 2);
    let a = a.ok_or_else(|| invalid(format!("{label} must be [[x0, y0, z0], [x1, y1, z1]]")))?;
    let lo = vec3(Some(&a[0]), label)?;
    let hi = vec3(Some(&a[1]), label)?;
    if (0..3).any(|k| hi[k] <= lo[k]) {
        return Err(invalid(format!("{label} must have increasing corners")));
    }
    Ok([lo, hi])
}

fn rgb(v: Option<&Value>, label: &str) -> Result<[f64; 3], SceneError> {
    let c = vec3(v, label)?;
    if c.iter().any(|x| !(0.0..=255.0).contains(x)) {
        return Err(invalid(format!("{label} must be sRGB values in 0..255")));
    }
    Ok(c)
}

fn flag(obj: &Map<String, Value>, key: &str, default: bool) -> Result<bool, SceneError> {
    match obj.get(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(invalid(format!("{key} must be true or false"))),
    }
}

fn positive(obj: &Map<String, Value>, key: &str, default: f64) -> Result<f64, SceneError> {
    match obj.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_f64()
            .filter(|x| x.is_finite() && *x > 0.0)
            .ok_or_else(|| invalid(format!("{key} must be a positive number"))),
    }
}

fn whole(obj: &Map<String, Value>, key: &str) -> Result<usize, SceneError> {
    obj.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| (16..=MAX_SIDE_PX).contains(n))
        .ok_or_else(|| invalid(format!("{key} must be an integer in [16, {MAX_SIDE_PX}]")))
}

fn closed(obj: &Map<String, Value>, allowed: &[&str], label: &str) -> Result<(), SceneError> {
    let mut unknown: Vec<&str> = obj.keys().map(String::as_str).filter(|k| !allowed.contains(k)).collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(invalid(format!("{label} has unknown fields {}", unknown.join(", "))))
}

fn convexes(v: Option<&Value>, label: &str) -> Result<Vec<Convex>, SceneError> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let items = v.as_array().ok_or_else(|| invalid(format!("{label} must be a list")))?;
    items
        .iter()
        .map(|item| {
            let obj = item.as_object().ok_or_else(|| invalid(format!("{label} entries must be objects")))?;
            match (obj.get("box_mm"), obj.get("half_space")) {
                (Some(b), None) if obj.len() == 1 => {
                    let [lo, hi] = box_of(Some(b), &format!("{label} box_mm"))?;
                    Ok(Convex::aabb(lo, hi))
                }
                (None, Some(h)) if obj.len() == 1 => {
                    let p = vec3(h.get("point_mm"), &format!("{label} half_space point_mm"))?;
                    let n = vec3(h.get("outward_normal"), &format!("{label} half_space outward_normal"))?;
                    Convex::half_space(p, n)
                }
                _ => Err(invalid(format!(
                    "{label} entries are {{box_mm}} or {{half_space: {{point_mm, outward_normal}}}}"
                ))),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraSpec(pub OrthoCamera, pub [[f64; 3]; 2]);

fn camera(name: &str, v: &Value) -> Result<CameraSpec, SceneError> {
    let label = format!("camera {name}");
    let obj = v.as_object().ok_or_else(|| invalid(format!("{label} must be an object")))?;
    let dir = vec3(obj.get("view_direction"), &format!("{label} view_direction"))?;
    let up = vec3(obj.get("up"), &format!("{label} up"))?;
    if obj.contains_key("frame_box_mm") {
        closed(obj, &["view_direction", "up", "frame_box_mm", "width_px", "margin"], &label)?;
        let frame = box_of(obj.get("frame_box_mm"), &format!("{label} frame_box_mm"))?;
        let width = whole(obj, "width_px")?;
        let margin = match obj.get("margin") {
            None => 0.04,
            Some(m) => m.as_f64().ok_or_else(|| invalid(format!("{label} margin must be a number")))?,
        };
        let cam = OrthoCamera::fit(dir, up, frame, width, margin)?;
        if cam.height > MAX_SIDE_PX {
            return Err(invalid(format!("{label} height exceeds {MAX_SIDE_PX} px")));
        }
        return Ok(CameraSpec(cam, frame));
    }
    closed(
        obj,
        &["view_direction", "up", "target_mm", "px_per_mm", "width_px", "height_px", "shadow_box_mm"],
        &label,
    )?;
    let target = vec3(obj.get("target_mm"), &format!("{label} target_mm"))?;
    let ppm = positive(obj, "px_per_mm", 0.0)?;
    let cam = OrthoCamera::explicit(target, dir, up, ppm, whole(obj, "width_px")?, whole(obj, "height_px")?)?;
    let frame = box_of(obj.get("shadow_box_mm"), &format!("{label} shadow_box_mm"))?;
    Ok(CameraSpec(cam, frame))
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerRequest {
    pub mesh: String,
    pub style: LayerStyle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewRequest {
    pub name: String,
    pub camera: String,
    pub layers: Vec<LayerRequest>,
    pub region: Region,
    pub options: ShadeOptions,
}

pub struct SceneRequest {
    pub meshes: BTreeMap<String, (String, f64)>,
    pub fields: Vec<GridField>,
    pub field_ids: Vec<String>,
    pub cameras: BTreeMap<String, CameraSpec>,
    pub views: Vec<ViewRequest>,
    pub supersample: usize,
    pub shadow_map_px: usize,
}

fn layer(
    v: &Value,
    label: &str,
    field_ids: &[String],
    fields: &[GridField],
) -> Result<LayerRequest, SceneError> {
    let obj = v.as_object().ok_or_else(|| invalid(format!("{label} must be an object")))?;
    closed(obj, &["mesh", "color_rgb", "color", "cap_rgb"], label)?;
    let mesh = obj
        .get("mesh")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{label} requires a mesh id")))?
        .to_string();
    let colouring = match (obj.get("color_rgb"), obj.get("color")) {
        (Some(c), None) => Colouring::Uniform(rgb(Some(c), &format!("{label} color_rgb"))?),
        (None, Some(c)) => {
            let c = c.as_object().ok_or_else(|| invalid(format!("{label} color must be an object")))?;
            closed(c, &["field", "colormap", "range"], &format!("{label} color"))?;
            let id = c.get("field").and_then(Value::as_str).unwrap_or_default();
            let field = field_ids
                .iter()
                .position(|f| f == id)
                .ok_or_else(|| invalid(format!("{label} colours by the unknown field {id:?}")))?;
            let map = Colormap::from_json(c.get("colormap"), &format!("{label} colormap"))?;
            let range = match c.get("range") {
                None => fields[field].range(),
                Some(r) => {
                    let r = number_array(Some(r), &format!("{label} range"))?;
                    if r.len() != 2 || r[1] <= r[0] {
                        return Err(invalid(format!("{label} range must be two increasing numbers")));
                    }
                    [r[0], r[1]]
                }
            };
            let range = if range[1] > range[0] { range } else { [range[0], range[0] + 1.0] };
            Colouring::Field { field, map, range }
        }
        _ => return Err(invalid(format!("{label} needs exactly one of color_rgb and color"))),
    };
    let cap_rgb = match obj.get("cap_rgb") {
        None | Some(Value::Null) => None,
        Some(c) => Some(rgb(Some(c), &format!("{label} cap_rgb"))?),
    };
    Ok(LayerRequest { mesh, style: LayerStyle { colouring, cap_rgb } })
}

fn view(
    v: &Value,
    index: usize,
    req: &SceneRequest,
    background: [f64; 3],
) -> Result<ViewRequest, SceneError> {
    let obj = v.as_object().ok_or_else(|| invalid(format!("view {index} must be an object")))?;
    let name = obj.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
    let ok_name = !name.is_empty()
        && name.len() <= 96
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        && !name.starts_with('.');
    if !ok_name {
        return Err(invalid(format!("view {index} needs a file-safe name ([A-Za-z0-9_.-], at most 96)")));
    }
    let label = format!("view {name}");
    closed(
        obj,
        &[
            "name",
            "camera",
            "layers",
            "keep",
            "remove",
            "shadows",
            "ambient_occlusion",
            "ao_radius_mm",
            "outline",
            "cap_hatch",
        ],
        &label,
    )?;
    let cam = obj.get("camera").and_then(Value::as_str).unwrap_or_default().to_string();
    if !req.cameras.contains_key(&cam) {
        return Err(invalid(format!("{label} uses the unknown camera {cam:?}")));
    }
    let items =
        obj.get("layers").and_then(Value::as_array).filter(|a| !a.is_empty() && a.len() <= MAX_LAYERS);
    let items = items.ok_or_else(|| invalid(format!("{label} needs 1..{MAX_LAYERS} layers")))?;
    let layers = items
        .iter()
        .enumerate()
        .map(|(k, l)| {
            let l = layer(l, &format!("{label} layer {k}"), &req.field_ids, &req.fields)?;
            if req.meshes.contains_key(&l.mesh) {
                Ok(l)
            } else {
                Err(invalid(format!("{label} layer {k} uses the unknown mesh {:?}", l.mesh)))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let region = Region {
        keep: convexes(obj.get("keep"), &format!("{label} keep"))?,
        remove: convexes(obj.get("remove"), &format!("{label} remove"))?,
        tolerance_mm: CUT_TOLERANCE_MM,
    };
    let hatch = match obj.get("cap_hatch") {
        None | Some(Value::Null) => None,
        Some(h) => {
            let h = h.as_object().ok_or_else(|| invalid(format!("{label} cap_hatch must be an object")))?;
            closed(h, &["spacing_px", "width_px", "color_rgb"], &format!("{label} cap_hatch"))?;
            Some(Hatch {
                spacing_px: positive(h, "spacing_px", 12.0)?,
                width_px: positive(h, "width_px", 2.0)?,
                rgb: rgb(
                    h.get("color_rgb").or(Some(&json!([60, 60, 64]))),
                    &format!("{label} cap_hatch color_rgb"),
                )?,
            })
        }
    };
    let options = ShadeOptions {
        shadows: flag(obj, "shadows", true)?,
        ambient_occlusion: flag(obj, "ambient_occlusion", true)?,
        ao_radius_mm: positive(obj, "ao_radius_mm", 0.3)?,
        outline: flag(obj, "outline", true)?,
        hatch,
        background,
    };
    Ok(ViewRequest { name, camera: cam, layers, region, options })
}



pub fn parse_request(raw: &Value) -> Result<SceneRequest, SceneError> {
    let obj = raw.as_object().ok_or_else(|| invalid("a mesh scene must be an object"))?;
    closed(obj, &["schema", "meshes", "fields", "cameras", "views", "render"], "the mesh scene")?;
    if obj.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(invalid(format!("a mesh scene declares schema {SCHEMA}")));
    }
    let mut meshes = BTreeMap::new();
    let m = obj.get("meshes").and_then(Value::as_object).filter(|m| !m.is_empty());
    for (id, v) in m.ok_or_else(|| invalid("meshes must name at least one mesh"))? {
        let o = v.as_object().ok_or_else(|| invalid(format!("mesh {id} must be an object")))?;
        closed(o, &["stl", "crease_deg"], &format!("mesh {id}"))?;
        let stl =
            o.get("stl").and_then(Value::as_str).ok_or_else(|| invalid(format!("mesh {id} needs stl")))?;
        let crease = match o.get("crease_deg") {
            None => 40.0,
            Some(c) => c
                .as_f64()
                .filter(|c| (0.0..=180.0).contains(c))
                .ok_or_else(|| invalid(format!("mesh {id} crease_deg must be in [0, 180]")))?,
        };
        meshes.insert(id.clone(), (stl.to_string(), crease));
    }
    let mut fields = Vec::new();
    let mut field_ids = Vec::new();
    if let Some(f) = obj.get("fields") {
        for (id, v) in f.as_object().ok_or_else(|| invalid("fields must be an object"))? {
            fields.push(GridField::from_json(id, v)?);
            field_ids.push(id.clone());
        }
    }
    let mut cameras = BTreeMap::new();
    let c = obj.get("cameras").and_then(Value::as_object).filter(|c| !c.is_empty());
    for (id, v) in c.ok_or_else(|| invalid("cameras must name at least one camera"))? {
        cameras.insert(id.clone(), camera(id, v)?);
    }
    let render = obj.get("render").cloned().unwrap_or_else(|| json!({}));
    let render = render.as_object().ok_or_else(|| invalid("render must be an object"))?;
    closed(render, &["supersample", "background_rgb", "shadow_map_px"], "render")?;
    let supersample = match render.get("supersample") {
        None => 2,
        Some(s) => s
            .as_u64()
            .and_then(|s| usize::try_from(s).ok())
            .filter(|s| SUPERSAMPLE.contains(s))
            .ok_or_else(|| invalid("render supersample must be 1, 2 or 3"))?,
    };
    let background = match render.get("background_rgb") {
        None => [255.0; 3],
        Some(b) => rgb(Some(b), "render background_rgb")?,
    };
    let shadow_map_px = match render.get("shadow_map_px") {
        None => 3072,
        Some(_) => whole(render, "shadow_map_px")?,
    };
    for (id, CameraSpec(cam, _)) in &cameras {
        if cam.width * cam.height * supersample * supersample > MAX_RASTER_PIXELS {
            return Err(invalid(format!(
                "camera {id}: the supersampled raster exceeds {MAX_RASTER_PIXELS} pixels"
            )));
        }
    }
    let mut req =
        SceneRequest { meshes, fields, field_ids, cameras, views: Vec::new(), supersample, shadow_map_px };
    let views = obj.get("views").and_then(Value::as_array).filter(|v| !v.is_empty());
    let views = views.ok_or_else(|| invalid("views must list at least one view"))?;
    let parsed =
        views.iter().enumerate().map(|(k, v)| view(v, k, &req, background)).collect::<Result<Vec<_>, _>>()?;
    let mut seen = std::collections::BTreeSet::new();
    for v in &parsed {
        if !seen.insert(v.name.clone()) {
            return Err(invalid(format!("view name {} is used twice", v.name)));
        }
    }
    req.views = parsed;
    Ok(req)
}

pub struct RenderedView {
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
    pub report: Value,
}

impl RenderedView {
    #[must_use]
    pub fn png(&self) -> Vec<u8> {
        implexity_mesh::raster::write_png(&self.rgb, self.width, self.height)
    }
}

pub trait MeshSource {


    fn mesh(&mut self, id: &str) -> Result<Arc<SceneMesh>, SceneError>;
}

pub struct StlFiles<'a> {
    base: &'a Path,
    request: &'a SceneRequest,
    cache: BTreeMap<String, Arc<SceneMesh>>,
}

impl<'a> StlFiles<'a> {
    #[must_use]
    pub fn new(base: &'a Path, request: &'a SceneRequest) -> Self {
        Self { base, request, cache: BTreeMap::new() }
    }
}

impl MeshSource for StlFiles<'_> {
    fn mesh(&mut self, id: &str) -> Result<Arc<SceneMesh>, SceneError> {
        if let Some(m) = self.cache.get(id) {
            return Ok(Arc::clone(m));
        }
        let (path, crease) =
            self.request.meshes.get(id).ok_or_else(|| invalid(format!("unknown mesh {id}")))?;
        let m = Arc::new(load_stl(&self.base.join(path), *crease)?);
        self.cache.insert(id.to_string(), Arc::clone(&m));
        Ok(m)
    }
}



pub fn render_view(
    req: &SceneRequest,
    v: &ViewRequest,
    meshes: &mut dyn MeshSource,
) -> Result<RenderedView, SceneError> {
    let t0 = std::time::Instant::now();
    let CameraSpec(cam, frame) = req.cameras[&v.camera];
    let held = v.layers.iter().map(|l| meshes.mesh(&l.mesh)).collect::<Result<Vec<_>, _>>()?;
    let layers: Vec<&SceneMesh> = held.iter().map(AsRef::as_ref).collect();
    let ss = req.supersample;
    let cam_ss = cam.supersampled(ss);
    let gbuffer = rasterize(&cam_ss, &layers, &v.region);
    let shadow = if v.options.shadows {
        Some(shade::ShadowMap::build(shade::key_light(&cam), frame, &layers, &v.region, req.shadow_map_px)?)
    } else {
        None
    };
    let styles: Vec<LayerStyle> = v.layers.iter().map(|l| l.style.clone()).collect();
    let fields: Vec<&GridField> = req.fields.iter().collect();
    let rgb = shade::shade(&shade::ShadeInput {
        gbuffer: &gbuffer,
        camera: &cam_ss,
        supersample: ss,
        styles: &styles,
        fields: &fields,
        shadow: shadow.as_ref(),
        options: v.options,
    });
    let mut cap = 0_usize;
    let mut covered = 0_usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, f) in gbuffer.px.iter().enumerate() {
        if f.layer != raster::EMPTY {
            covered += 1;
            cap += usize::from(f.flags & raster::FLAG_CAP != 0);
            let (x, y) = ((i % gbuffer.width) / ss, (i / gbuffer.width) / ss);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    let axis = |a: [f64; 3]| json!([dot(a, cam.right), -dot(a, cam.up)]);
    let layer_reports: Vec<Value> = v
        .layers
        .iter()
        .zip(&layers)
        .map(|(l, m)| {
            let colouring = match &l.style.colouring {
                Colouring::Uniform(c) => json!({"uniform_rgb": c}),
                Colouring::Field { field, range, .. } => {
                    json!({"field": req.field_ids[*field], "range": range})
                }
            };
            json!({"mesh": l.mesh, "triangles": m.len(), "vertices": m.positions.len(),
                   "bounds_mm": m.bounds, "colouring": colouring, "cap_rgb": l.style.cap_rgb})
        })
        .collect();
    let report = json!({
        "schema": VIEW_SCHEMA,
        "name": v.name,
        "width_px": cam.width,
        "height_px": cam.height,
        "projection": "orthographic",
        "px_per_mm": cam.px_per_mm,
        "camera": {"id": v.camera, "target_mm": cam.target, "forward": cam.forward, "right": cam.right, "up": cam.up},
        "screen_axes": {"x": axis([1.0, 0.0, 0.0]), "y": axis([0.0, 1.0, 0.0]), "z": axis([0.0, 0.0, 1.0])},
        "supersample": ss,
        "layers": layer_reports,
        "cut": {"keep": v.region.keep.len(), "remove": v.region.remove.len(), "tolerance_mm": v.region.tolerance_mm},
        "coverage": {"fraction": covered as f64 / gbuffer.px.len() as f64,
                     "cap_fraction": if covered > 0 { cap as f64 / covered as f64 } else { 0.0 },
                     "bbox_px": if covered > 0 { json!([x0, y0, x1 + 1, y1 + 1]) } else { Value::Null }},
        "shading": {"shadows": v.options.shadows, "ambient_occlusion": v.options.ambient_occlusion,
                    "ao_radius_mm": v.options.ao_radius_mm, "outline": v.options.outline,
                    "cap_hatch": v.options.hatch.is_some(), "geometry_moved": false},
        "elapsed_s": t0.elapsed().as_secs_f64(),
    });
    Ok(RenderedView { name: v.name.clone(), width: cam.width, height: cam.height, rgb, report })
}
