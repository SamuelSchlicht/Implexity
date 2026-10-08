// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use implexity_core::backends::GeometryBuild;
use implexity_core::py_repr::repr_str;
use implexity_geometry::boxes::{MAX_PREVIEW_SAMPLES, SampleBox, plane_box, slab_box};
use implexity_geometry::preview::{
    Channel, Design, Params, RealEvaluator, RunMode, SyntheticEvaluator, TwoRate, trilerp_np,
};
use implexity_mesh::bodyexport::{BodyEvaluator, DesignSnapshot, FastFields, GeffCache};
use implexity_mesh::contours::{SectionFrame, section_polylines};
use implexity_mesh::grid::Field3;
use implexity_mesh::raster::{Background, PhasePalette, section_rgb_with_palette, write_png};
use implexity_mesh::surface::{decimate_polylines, slab_mesh};
use serde_json::{Map, Value, json};

use crate::http::{RouteError, SERVER_VERSION, py_float, py_int, py_truthy};
use crate::jobs::JobError;
use crate::lod::{self, FEATURE_PERIOD_MM_MED, FEATURE_PERIOD_MM_MIN, Lod, py_round};
use crate::preview::{FieldPayload, PreviewBackend, PreviewContext, PreviewResult};

const MM: f64 = 1e-3;

pub const SECTION_FIELDS: [(&str, &str); 6] = [
    ("rho", "solid fraction in [0, 1], interface-smoothed and domain-masked"),
    ("g", "the signed lattice field; < 0 inside the solid, 0 on the surface"),
    ("q", "the folded, gradient-normalised phase distance, before thickness"),
    ("tau", "the thickness field the phase distance is compared against"),
    ("geff", "the effective phase-gradient magnitude used for normalisation"),
    ("f", "the raw blended TPMS phase function"),
];

fn failed(kind: &str, message: impl Into<String>) -> JobError {
    JobError::failed(kind, message.into())
}

fn route_error(e: RouteError) -> JobError {
    match e {
        RouteError::Contract(m) => failed("CAEContractError", m),
        RouteError::Internal { message, detail } => {
            let kind = detail.split_once(':').map_or("ValueError", |(k, _)| k).to_string();
            failed(&kind, message)
        }
    }
}

fn geometry_error(e: &implexity_geometry::GeometryError) -> JobError {
    match e {
        implexity_geometry::GeometryError::Cancelled => JobError::Superseded("cancelled".into()),
        implexity_geometry::GeometryError::Key(_) => failed("KeyError", e.to_string()),
        other => failed("ValueError", other.to_string()),
    }
}

fn get<'a>(req: &'a Value, key: &str) -> Option<&'a Value> {
    req.as_object().and_then(|m| m.get(key))
}

fn float(v: &Value) -> Result<f64, JobError> {
    py_float(v).map_err(|e| failed("TypeError", e.to_string()))
}

fn vec3(v: &Value, what: &str) -> Result<[f64; 3], JobError> {
    let items = v
        .as_array()
        .filter(|a| a.len() == 3)
        .ok_or_else(|| failed("ValueError", format!("{what} must be three numbers")))?;
    Ok([float(&items[0])?, float(&items[1])?, float(&items[2])?])
}

fn vec3_or(req: &Value, key: &str, default: [f64; 3]) -> Result<[f64; 3], JobError> {
    get(req, key).map_or(Ok(default), |v| vec3(v, key))
}

fn round6(v: [f64; 3]) -> String {
    format!("({}, {}, {})", py_round(v[0], 6), py_round(v[1], 6), py_round(v[2], 6))
}

fn text(v: Option<&Value>, default: &str) -> String {
    match v {
        None => default.to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn channel_array(c: &Channel) -> ndarray::ArrayD<f64> {
    ndarray::ArrayD::from_shape_vec(ndarray::IxDyn(&c.shape), c.data.clone())
        .unwrap_or_else(|_| ndarray::ArrayD::zeros(ndarray::IxDyn(&[0])))
}

pub struct GeometryPreview {
    evaluator: Arc<dyn TwoRate>,
    design: Arc<Design>,
    geff: GeffCache,
}

impl std::fmt::Debug for GeometryPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeometryPreview").field("evaluator", &self.evaluator.name()).finish_non_exhaustive()
    }
}

fn domain_mm(meta: &Map<String, Value>) -> [f64; 3] {
    meta.get("domain_mm")
        .and_then(Value::as_array)
        .map_or([0.0; 3], |a| std::array::from_fn(|i| a.get(i).and_then(Value::as_f64).unwrap_or(0.0)))
}

impl GeometryPreview {
    #[must_use]
    pub fn new(evaluator: Arc<dyn TwoRate>, design: Arc<Design>) -> Self {
        Self { evaluator, design, geff: GeffCache::default() }
    }



    pub fn from_build(build: GeometryBuild) -> Result<Self, String> {
        let design = build
            .design
            .downcast::<Arc<Design>>()
            .map_err(|_| "the geometry backend built a design of an unknown type".to_owned())?;
        let evaluator: Arc<dyn TwoRate> = match build.evaluator.downcast::<SyntheticEvaluator>() {
            Ok(ev) => Arc::new(*ev),
            Err(other) => match other.downcast::<RealEvaluator>() {
                Ok(ev) => Arc::new(*ev),
                Err(_) => return Err("the geometry backend built an evaluator of an unknown type".into()),
            },
        };
        Ok(Self::new(evaluator, *design))
    }

    #[must_use]
    pub fn evaluator(&self) -> &Arc<dyn TwoRate> {
        &self.evaluator
    }

    #[must_use]
    pub fn design(&self) -> &Arc<Design> {
        &self.design
    }

    fn meta(&self) -> Result<Map<String, Value>, JobError> {
        self.design.meta().map_err(|e| geometry_error(&e))
    }

    fn envelope(&self) -> Result<[f64; 3], JobError> {
        Ok(domain_mm(&self.meta()?))
    }

    fn version_i64(v: u64) -> i64 {
        i64::try_from(v).unwrap_or(i64::MAX)
    }



    pub fn apply_design(
        &self,
        params: Params,
        meta_updates: Option<&Map<String, Value>>,
        source: Option<&str>,
        replace_meta: bool,
    ) -> Result<(u64, Value), JobError> {

        let mut meta_updates = meta_updates;
        if replace_meta && let Some(meta) = meta_updates {
            self.design.set_meta(meta.clone()).map_err(|e| geometry_error(&e))?;
            meta_updates = None;
        }
        let version =
            self.design.replace_design(params, meta_updates, source).map_err(|e| geometry_error(&e))?;
        let rebound = self.evaluator.rebind().map_err(|e| geometry_error(&e))?;
        let m = self.meta()?;
        let event = json!({"event": "design", "design_version": version,
                           "source": m.get("source").cloned().unwrap_or(Value::Null),
                           "envelope_mm": domain_mm(&m), "rebound": rebound});
        Ok((version, event))
    }

    fn inside(&self, b: &SampleBox) -> Result<(Vec<f64>, Vec<bool>), JobError> {
        let env = self.envelope()?.map(|e| e * MM);
        let pts = b.points();
        let mut mask = Vec::with_capacity(pts.len());
        let mut inside = Vec::with_capacity(pts.len());
        for p in pts {
            let mut d = f64::INFINITY;
            for c in 0..3 {
                d = d.min(p[c].min(env[c] - p[c]));
            }
            mask.push((0.5 + d / b.h).clamp(0.0, 1.0));
            inside.push(d >= 0.0);
        }
        Ok((mask, inside))
    }

    fn fit_extent(&self, u: [f64; 3], v: [f64; 3]) -> Result<(f64, f64), JobError> {
        let env = self.envelope()?;
        let proj = |a: [f64; 3]| (0..3).map(|j| a[j].abs() * env[j]).sum::<f64>();
        Ok((proj(u), proj(v)))
    }

    fn evaluate(
        &self,
        b: &SampleBox,
        fields: &[&str],
        ctx: Option<&PreviewContext<'_>>,
        boundary: &str,
        geff_stride: i64,
    ) -> Result<(BTreeMap<String, Vec<f64>>, u64), JobError> {

        b.admit(MAX_PREVIEW_SAMPLES).map_err(|e| failed("MemoryError", e.to_string()))?;
        let cancel = ctx.map(|c| c.cancel);
        let probe = move || cancel.is_some_and(crate::jobs::CancelToken::is_cancelled);
        self.evaluator
            .evaluate(b, fields, Some(&probe), boundary, usize::try_from(geff_stride.max(1)).unwrap_or(1))
            .map_err(|e| geometry_error(&e))
    }

    #[allow(clippy::too_many_lines)]
    fn section_build(
        &self,
        plane: &Plane,
        want: &[String],
        l: &Lod,
        req: &Value,
        ctx: &PreviewContext<'_>,
        palette: PhasePalette,
    ) -> Result<PreviewResult, JobError> {
        let (b, u, v) = plane_box(plane.origin, plane.normal, plane.up, plane.w, plane.h, l.h_mm(), 1, 8);
        let fname = text(get(req, "field"), "rho");
        let mut fields = vec!["rho"];
        let wants = |k: &str| want.iter().any(|w| w == k);
        if wants("image") || wants("material") {
            fields.push("phase_fraction");
        }
        if !fields.contains(&fname.as_str()) {
            fields.push(fname.as_str());
        }
        let boundary = text(get(req, "boundary"), "clamp");
        let t0 = Instant::now();
        let (out, version) = self.evaluate(&b, &fields, Some(ctx), &boundary, l.geff_stride)?;
        let dt = t0.elapsed().as_secs_f64();
        ctx.observe(l.geff_stride, b.n() as f64, dt);
        let (mask, inside) = self.inside(&b)?;
        let rho: Vec<f64> =
            out.get("rho").map(|r| r.iter().zip(&mask).map(|(a, m)| a.min(*m)).collect()).unwrap_or_default();
        let n_inside = inside.iter().filter(|x| **x).count();
        let mut res = Map::new();
        res.insert("kind".into(), json!("section"));
        res.insert("units".into(), json!("mm"));
        res.insert("backend".into(), json!(self.evaluator.name()));
        res.insert("design_version".into(), json!(version));
        res.insert("lod".into(), l.as_dict());
        res.insert("samples".into(), json!(b.shape));
        res.insert("eval_ms".into(), json!(py_round(dt * 1000.0, 2)));
        res.insert("origin_mm".into(), json!(b.origin.map(|x| x / MM)));
        res.insert("u_axis".into(), json!(u));
        res.insert("v_axis".into(), json!(v));
        res.insert("normal".into(), json!(b.axes[2]));
        res.insert("solid_fraction".into(), json!(rho.iter().sum::<f64>() / n_inside.max(1) as f64));
        res.insert("inside_fraction".into(), json!(n_inside as f64 / inside.len().max(1) as f64));
        let mut result = PreviewResult::default();
        let (nx, ny) = (b.shape[0], b.shape[1]);
        if wants("contours") {
            let frame = SectionFrame { origin: b.origin, h: b.h, u_axis: u, v_axis: v };
            let polys = section_polylines(&rho, nx, ny, &frame, 0.5, true);
            let polys = decimate_polylines(&polys, l.rdp_tol_frac * l.h_mm());
            res.insert("polyline_count".into(), json!(polys.len()));
            res.insert("vertex_count".into(), json!(polys.iter().map(Vec::len).sum::<usize>()));
            result.polylines = Some(Arc::new(polys));
        }
        if wants("image") {
            let outside: Vec<bool> = inside.iter().map(|x| !x).collect();
            let phase_fraction = out.get("phase_fraction").map(Vec::as_slice);
            let img = section_rgb_with_palette(&rho, nx, ny, phase_fraction, None, Some(&outside), Background::Dark, palette);
            result.png = Some(Arc::new(write_png(&img.data, img.width, img.height)));
            res.insert("image_wh".into(), json!([nx, ny]));
        }
        if wants("field") {
            let arr: Vec<f64> =
                if fname == "rho" { rho } else { out.get(&fname).cloned().unwrap_or_default() };
            res.insert("field_name".into(), json!(fname));
            #[allow(clippy::cast_possible_truncation)]
            let values = arr.iter().map(|x| *x as f32).collect();
            result.field = Some(Arc::new(FieldPayload { values, shape: vec![nx, ny] }));
        }
        result.meta = res;
        Ok(result)
    }

    fn slab_build(
        &self,
        centre: [f64; 3],
        axes: [[f64; 3]; 3],
        extent: [f64; 3],
        l: &Lod,
        req: &Value,
        ctx: &PreviewContext<'_>,
    ) -> Result<PreviewResult, JobError> {
        let samples = match get(req, "slab_samples") {
            Some(v) if !v.is_null() => float(v)?,
            _ => l.slab_samples as f64,
        };
        #[allow(clippy::cast_possible_truncation)]
        let samples = [samples.trunc() as i64];
        let b = slab_box(centre, axes, extent, None, Some(&samples), 4).map_err(|e| geometry_error(&e))?;
        let t0 = Instant::now();
        let want_mat = get(req, "material").is_none_or(py_truthy);
        let fields: &[&str] = if want_mat { &["rho", "phase_fraction"] } else { &["rho"] };
        let boundary = text(get(req, "boundary"), "clamp");
        let (out, version) = self.evaluate(&b, fields, Some(ctx), &boundary, l.geff_stride)?;
        let dt = t0.elapsed().as_secs_f64();
        ctx.observe(l.geff_stride, b.n() as f64, dt);
        let (mask, _) = self.inside(&b)?;
        let rho: Vec<f64> =
            out.get("rho").map(|r| r.iter().zip(&mask).map(|(a, m)| a.min(*m)).collect()).unwrap_or_default();
        let field = Field3::new(b.shape, &rho).map_err(|e| failed("ValueError", e.to_string()))?;
        let phase_fraction = out.get("phase_fraction");
        let attr = match phase_fraction {
            Some(c) => Some(Field3::new(b.shape, c).map_err(|e| failed("ValueError", e.to_string()))?),
            None => None,
        };
        let mesh = slab_mesh(&field, b.origin, b.axes, b.h, 0.5, attr.as_ref())
            .map_err(|e| failed("ValueError", e.to_string()))?;
        let h_mm = b.h * 1000.0;
        let actual: Vec<f64> = (0..3).map(|i| py_round((b.shape[i] as f64 - 1.0) * h_mm, 4)).collect();
        let mut lodd = l.as_dict();
        if let Some(m) = lodd.as_object_mut() {
            m.insert("h_mm".into(), json!(py_round(h_mm, 4)));
            m.insert("samples_per_feature".into(), json!(py_round(FEATURE_PERIOD_MM_MIN / h_mm, 2)));
            m.insert("resolves_feature".into(), json!(h_mm <= FEATURE_PERIOD_MM_MIN / 3.0));
        }
        let mut res = Map::new();
        res.insert("kind".into(), json!("slab"));
        res.insert("units".into(), json!("mm"));
        res.insert("backend".into(), json!(self.evaluator.name()));
        res.insert("design_version".into(), json!(version));
        res.insert("lod".into(), lodd);
        res.insert("samples".into(), json!(b.shape));
        res.insert("requested_extent_mm".into(), json!(extent.map(|e| py_round(e, 4))));
        res.insert("extent_mm".into(), json!(actual));
        res.insert("centre_mm".into(), json!(centre.map(|x| py_round(x, 4))));
        res.insert("eval_ms".into(), json!(py_round(dt * 1000.0, 2)));
        res.insert("vertex_count".into(), json!(mesh.vertices.len()));
        res.insert("triangle_count".into(), json!(mesh.triangles.len()));
        res.insert("material_attribute".into(), json!(!mesh.attribute.is_empty()));
        Ok(PreviewResult { meta: res, mesh: Some(Arc::new(mesh)), ..PreviewResult::default() })
    }

    fn section_fields_problem(fname: &str) -> JobError {
        let mut problems = vec![format!("field {} is not a field this endpoint serves", repr_str(fname))];
        let mut sorted = SECTION_FIELDS.to_vec();
        sorted.sort_by_key(|(k, _)| *k);
        problems.extend(sorted.iter().map(|(k, d)| format!("{k:<6} {d}")));
        JobError::Case(problems)
    }
}

struct Plane {
    origin: [f64; 3],
    normal: [f64; 3],
    up: [f64; 3],
    w: f64,
    h: f64,
}

fn want_list(req: &Value, default: &[&str]) -> Result<Vec<String>, JobError> {
    let Some(raw) = get(req, "want") else {
        return Ok(default.iter().map(|s| (*s).to_owned()).collect());
    };
    let items: Vec<String> = match raw {
        Value::Array(a) => a.iter().map(|v| text(Some(v), "")).collect(),
        Value::String(s) => s.chars().map(String::from).collect(),
        other => return Err(failed("TypeError", format!("{other} is not iterable"))),
    };
    let mut set: Vec<String> = items;
    set.sort();
    set.dedup();
    Ok(set)
}

impl PreviewBackend for GeometryPreview {
    fn name(&self) -> String {
        self.evaluator.name().to_owned()
    }

    fn design_version(&self) -> i64 {
        self.design.snapshot().map_or(0, |(_, v)| Self::version_i64(v))
    }

    fn model_info(&self) -> Result<Value, JobError> {
        let (params, meta, version) = self.design.snapshot_full().map_err(|e| geometry_error(&e))?;
        let dom = domain_mm(&meta);
        let cs: Vec<f64> = meta
            .get("control_shape")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        let mut channels = Map::new();
        for (k, c) in &params {
            let n = c.data.len().max(1) as f64;
            let min = c.data.iter().copied().fold(f64::INFINITY, f64::min);
            let max = c.data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mean = implexity_mesh::numeric::pairwise_sum(&c.data) / n;
            channels.insert(k.clone(), json!({"shape": c.shape, "min": min, "max": max, "mean": mean}));
        }
        let m = |k: &str| meta.get(k).cloned().unwrap_or(Value::Null);
        let prov = meta.get("continuation_provenance").cloned().unwrap_or_else(|| json!({}));
        let all_run = prov.as_object().is_none_or(|p| p.values().all(|v| v == "run"));
        let t_offset = meta.get("t_offset").and_then(Value::as_f64).unwrap_or(0.0);
        let h_design = self.evaluator.h_design();
        let env_int = |name: &str, default: i64| {
            std::env::var(name).ok().and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(default)
        };
        let min_tol = std::env::var("IMPLEXITY_MIN_BODY_TOLERANCE")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.001);
        let spacing: Vec<f64> =
            (0..3).map(|i| py_round(dom[i] / (cs.get(i).copied().unwrap_or(2.0) - 1.0), 4)).collect();
        Ok(json!({
            "service": SERVER_VERSION, "backend": self.evaluator.name(), "units": "mm",
            "design_version": version, "source": m("source"),
            "envelope_mm": {"min": [0, 0, 0], "max": m("domain_mm")},
            "control_shape": m("control_shape"),
            "control_spacing_mm": spacing,
            "design_grid": m("design_grid"),
            "h_design_mm": py_round(h_design * 1000.0, 5),
            "period_mm": m("period_mm"),
            "feature_period_mm": [FEATURE_PERIOD_MM_MIN, FEATURE_PERIOD_MM_MED],
            "continuation": {
                "interface_w": m("interface_w"), "beta_mask": m("beta_mask"),
                "beta_mat": m("beta_mat"), "beta_topo": m("beta_topo"),
                "t_offset_mm": py_round(t_offset * 1000.0, 5),
                "t_offset_m": t_offset,
                "provenance": prov,
                "warning": if all_run { Value::Null } else { json!(
                    "some continuation parameters were not in the design file and are ASSUMED; the same \
                     control fields give a different solid at a different continuation stage") },
            },
            "channels": channels,
            "lod": lod::ORDER.iter().map(|n| lod::get(n).as_dict()).collect::<Vec<_>>(),
            "mesh_available": true,
            "case": {"schema": "implexity-case/1",
                     "schemas": ["implexity-case/1", "implexity-case/2", "implexity-case/3"],
                     "endpoint": "/v1/case", "domain_endpoint": "/v1/domain",
                     "max_grid": env_int("IMPLEXITY_MAX_GRID", 16)},
            "body": {"endpoint": "/v1/body", "estimate": "/v1/body/estimate", "jobs": "/v1/body/jobs",
                     "formats": implexity_mesh::exporters::names(),
                     "format_catalogue": implexity_mesh::exporters::catalogue(),
                     "default_tolerance_mm": 0.02, "min_tolerance_mm": min_tol,
                     "components": ["all", "connected_to:<face_group>"]},
            "optimize": {"endpoint": "/v1/optimize", "steer": "/v1/optimize/jobs/<id>/steer",
                         "catalogue": "/v1/optimize/catalogue",
                         "max_grid": env_int("IMPLEXITY_MAX_OPT_GRID", 12)},
        }))
    }

    fn fidelity(&self) -> Result<Value, JobError> {
        let meta = self.meta()?;
        let h = self.evaluator.h_design();
        let iw = meta.get("interface_w").and_then(Value::as_f64).unwrap_or(0.0);
        Ok(fidelity_payload(h, iw))
    }

    fn set_params(&self, req: &Value) -> Result<Value, JobError> {
        let updates: Vec<Value> = match get(req, "updates") {
            Some(Value::Array(a)) if !a.is_empty() => a.clone(),
            _ => vec![req.clone()],
        };
        let mut version = self.design.snapshot().map_err(|e| geometry_error(&e))?.1;
        for u in &updates {
            let channel = get(u, "channel")
                .ok_or_else(|| failed("KeyError", "'channel'"))
                .map(|v| text(Some(v), ""))?;

            let (params, _) = self.design.snapshot().map_err(|e| geometry_error(&e))?;
            let Some(arr) = params.get(&channel) else {
                return Err(failed("KeyError", repr_str(&channel)));
            };
            let index = match get(u, "index") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let i = py_int(v).map_err(route_error)?;
                    if arr.shape.len() > 3 {
                        let n = i64::try_from(arr.shape[0]).unwrap_or(i64::MAX);
                        let j = if i < 0 { i + n } else { i };
                        if !(0..n).contains(&j) {
                            return Err(failed(
                                "IndexError",
                                format!("index {i} is out of bounds for axis 0 with size {n}"),
                            ));
                        }
                        usize::try_from(j).ok()
                    } else {
                        None
                    }
                }
            };
            let opt = |k: &str| -> Result<Option<f64>, JobError> {
                match get(u, k) {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => py_float(v).map(Some).map_err(route_error),
                }
            };
            let absolute = opt("absolute")?;
            let scale = opt("scale")?;
            let delta = opt("delta")?;
            version = self
                .design
                .apply_delta(&channel, delta, scale, absolute, index)
                .map_err(|e| failed("KeyError", e.to_string()))?;
        }
        let (params, _) = self.design.snapshot().map_err(|e| geometry_error(&e))?;
        Ok(json!({"kind": "params", "design_version": version,
                  "channels": params.keys().collect::<Vec<_>>()}))
    }

    fn field_record(&self, req: &Value) -> Result<Value, JobError> {
        let case = |m: String| JobError::Case(vec![m]);
        let Value::Object(body) = req else {
            return Err(case("field provenance takes a JSON object".into()));
        };
        let obj_or_empty = |k: &str| match body.get(k) {
            Some(Value::Object(m)) => Value::Object(m.clone()),
            _ => json!({}),
        };
        let request = obj_or_empty("request");
        let sample = obj_or_empty("sample");
        let pick = |v: Option<&Value>| v.filter(|x| py_truthy(x)).map(|x| text(Some(x), ""));
        let field = pick(request.get("field")).or_else(|| pick(sample.get("field"))).unwrap_or_default();
        if !SECTION_FIELDS.iter().any(|(k, _)| *k == field) {
            let mut names: Vec<&str> = SECTION_FIELDS.iter().map(|(k, _)| *k).collect();
            names.sort_unstable();
            return Err(case(format!(
                "field provenance needs one of {}; got {}",
                names.join(", "),
                repr_str(&field)
            )));
        }
        let representation = pick(request.get("representation")).unwrap_or_else(|| "raw".into());
        if !["raw", "rho_to_phi", "scaled_signed_g"].contains(&representation.as_str()) {
            return Err(case(format!(
                "field representation {} is not one of raw, rho_to_phi, scaled_signed_g",
                repr_str(&representation)
            )));
        }
        if representation == "rho_to_phi" && field != "rho" {
            return Err(case("rho_to_phi provenance requires field='rho'".into()));
        }
        if representation == "scaled_signed_g" && field != "g" {
            return Err(case("scaled_signed_g provenance requires field='g'".into()));
        }
        let (params, meta, version) = self.design.snapshot_full().map_err(|e| geometry_error(&e))?;
        let seen = match sample.get("design_version") {
            None | Some(Value::Null) => {
                return Err(case(
                    "sample.design_version is required: every stacked plane must belong to one live design version"
                        .into(),
                ));
            }
            Some(v) => float(v)?,
        };
        #[allow(clippy::cast_possible_truncation)]
        let seen_int = seen.trunc() as i64;
        if seen_int != Self::version_i64(version) {
            return Err(case(format!(
                "the sampled field belongs to design version {} but the live service is now at version \
                 {version}; restart the export so every plane and its provenance name the same design",
                text(sample.get("design_version"), "")
            )));
        }
        let shape_ok = sample
            .get("shape")
            .and_then(Value::as_array)
            .is_some_and(|s| s.len() == 3 && s.iter().all(|v| py_float(v).is_ok_and(|x| x.trunc() > 0.0)));
        if !shape_ok {
            return Err(case("sample.shape must contain three positive grid dimensions".into()));
        }
        if sample.get("origin_mm").and_then(Value::as_array).is_none_or(|o| o.len() != 3) {
            return Err(case("sample.origin_mm must contain x,y,z in millimetres".into()));
        }
        let h = sample.get("h_mm").and_then(|v| py_float(v).ok()).unwrap_or(-1.0);
        if !h.is_finite() || h <= 0.0 {
            return Err(case("sample.h_mm must be a positive finite spacing".into()));
        }
        let design_params: BTreeMap<String, ndarray::ArrayD<f64>> =
            params.iter().map(|(k, c)| (k.clone(), channel_array(c))).collect();
        let meta_v = Value::Object(meta);
        let links = json!({"record_endpoint": "/v1/provenance/field"});
        let rec =
            implexity_io::provenance::field_export_record(&implexity_io::provenance::FieldExportInputs {
                design_params: &design_params,
                design_meta: &meta_v,
                design_version: Self::version_i64(version),
                request: &request,
                sample: &sample,
                service_version: Some(SERVER_VERSION),
                backend: Some(self.evaluator.name()),
                h_design_mm: Some(self.evaluator.h_design() * 1000.0),
                links: &links,
            })
            .map_err(|e| failed("ValueError", e.to_string()))?;
        let vdb = implexity_io::provenance::vdb_metadata(&rec, None);
        Ok(json!({"kind": "field_provenance", "record": rec, "vdb_metadata": vdb}))
    }

    fn section(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError> {
        let env = self.envelope()?;
        let normal = vec3_or(req, "normal", [0.0, 0.0, 1.0])?;
        let up = vec3_or(req, "up", [0.0, 1.0, 0.0])?;
        let origin = match get(req, "origin_mm") {
            None | Some(Value::Null) => env.map(|e| e / 2.0),
            Some(v) => vec3(v, "origin_mm")?,
        };
        let (_, u0, v0) = plane_box(origin, normal, up, 1.0, 1.0, 1.0, 1, 8);
        let (fw, fh) = self.fit_extent(u0, v0)?;
        let w = get(req, "width_mm").map_or(Ok(fw), float)?;
        let hgt = get(req, "height_mm").map_or(Ok(fh), float)?;
        let want = want_list(req, &["contours"])?;
        let fname = text(get(req, "field"), "rho");
        if !SECTION_FIELDS.iter().any(|(k, _)| *k == fname) {
            return Err(Self::section_fields_problem(&fname));
        }
        let l = ctx.pick_lod(req, &[w, hgt], 1)?;
        let boundary = text(get(req, "boundary"), "clamp");
        let (palette, palette_key) = match get(req, "phase_colours") {
            Some(v) => {
                let mut colours: [[f64; 3]; 2] = serde_json::from_value(v.clone())
                    .map_err(|_| failed("ValueError", "phase_colours must contain two RGB triples"))?;
                for channel in colours.iter_mut().flatten() {
                    if *channel == 0.0 { *channel = 0.0; }
                }
                let palette = PhasePalette::new(colours).map_err(|e| failed("ValueError", e.to_string()))?;
                (palette, json!(colours).to_string())
            }
            None => (PhasePalette::for_background(Background::Dark), "default".to_owned()),
        };
        let keyparts = vec![
            round6(origin),
            round6(normal),
            round6(up),
            w.to_string(),
            hgt.to_string(),
            want.join(","),
            l.name.to_owned(),
            fname,
            boundary,
            palette_key,
        ];
        let use_cache = get(req, "cache").is_none_or(py_truthy);
        let plane = Plane { origin, normal, up, w, h: hgt };
        ctx.cached("section", self.design_version(), &self.name(), &keyparts, use_cache, || {
            self.section_build(&plane, &want, l, req, ctx, palette)
        })
    }

    fn slab(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError> {
        let centre = vec3_or(req, "centre_mm", [4.0, 8.0, 16.0])?;
        let axes: [[f64; 3]; 3] = match get(req, "axes") {
            None => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            Some(v) => {
                let rows = v
                    .as_array()
                    .filter(|a| a.len() == 3)
                    .ok_or_else(|| failed("ValueError", "axes must be three vectors"))?;
                [vec3(&rows[0], "axes")?, vec3(&rows[1], "axes")?, vec3(&rows[2], "axes")?]
            }
        };
        let extent = vec3_or(req, "extent_mm", [8.0, 16.0, 4.0])?;
        #[allow(clippy::cast_possible_truncation)]
        let thickness = 2_i64.max((extent[2] / 0.3_f64.max(1e-6)).trunc() as i64);
        let l = ctx.pick_lod(req, &extent[..2], thickness)?;
        let axes_key: Vec<String> = axes.iter().map(|a| round6(*a)).collect();
        let keyparts = vec![
            round6(centre),
            axes_key.join(","),
            format!("{:?}", extent.map(|e| py_round(e, 4))),
            l.name.to_owned(),
            text(get(req, "boundary"), "clamp"),
            text(get(req, "slab_samples"), "None"),
        ];
        let use_cache = get(req, "cache").is_none_or(py_truthy);
        ctx.cached("slab", self.design_version(), &self.name(), &keyparts, use_cache, || {
            self.slab_build(centre, axes, extent, l, req, ctx)
        })
    }

    fn probe(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError> {
        let raw = get(req, "points_mm").cloned().unwrap_or_else(|| json!([[4.0, 8.0, 16.0]]));
        let rows: Vec<[f64; 3]> = match &raw {
            Value::Array(a) if a.first().is_some_and(Value::is_array) => {
                a.iter().map(|p| vec3(p, "points_mm")).collect::<Result<_, _>>()?
            }
            Value::Array(_) => vec![vec3(&raw, "points_mm")?],
            _ => return Err(failed("ValueError", "points_mm must be a list of points")),
        };
        let (params, meta, version) = self.design.snapshot_full().map_err(|e| geometry_error(&e))?;
        let dom = domain_mm(&meta);
        let nc: Vec<f64> = meta
            .get("control_shape")
            .and_then(Value::as_array)
            .map_or_else(|| vec![2.0; 3], |a| a.iter().filter_map(Value::as_f64).collect());
        let mut control = Vec::with_capacity(rows.len());
        for p in &rows {
            let ci: [f64; 3] =
                std::array::from_fn(|c| (p[c] / dom[c] * (nc[c] - 1.0)).clamp(0.0, nc[c] - 1.0));
            let mut row = Map::new();
            for (k, ch) in &params {
                let sp = ch.spatial();
                if ch.shape.len() == 3 {
                    row.insert(k.clone(), json!(trilerp_np(&ch.data, sp, ci)));
                } else {
                    let vals: Vec<f64> =
                        (0..ch.shape[0]).map(|j| trilerp_np(ch.component(j), sp, ci)).collect();
                    row.insert(k.clone(), json!(vals));
                }
            }
            control.push(Value::Object(row));
        }
        let h = self.evaluator.h_design();
        let mut derived = Vec::with_capacity(rows.len());
        for p in &rows {
            let b = SampleBox {
                origin: p.map(|x| x * MM),
                axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                shape: [1, 1, 1],
                h,
            };
            let (o, _) =
                self.evaluate(&b, &["rho", "f", "q", "tau", "nu", "phase_fraction", "t"], Some(ctx), "clamp", 1)?;
            let row: Map<String, Value> =
                o.into_iter().map(|(k, v)| (k, json!(v.first().copied().unwrap_or(f64::NAN)))).collect();
            derived.push(Value::Object(row));
        }
        let mut meta_out = Map::new();
        meta_out.insert("kind".into(), json!("probe"));
        meta_out.insert("units".into(), json!("mm"));
        meta_out.insert("design_version".into(), json!(version));
        meta_out.insert("points_mm".into(), json!(rows));
        meta_out.insert("control".into(), Value::Array(control));
        meta_out.insert("geometry".into(), Value::Array(derived));
        Ok(PreviewResult::from_meta(meta_out))
    }

    fn compare(&self, req: &Value, ctx: &PreviewContext<'_>) -> Result<PreviewResult, JobError> {
        let env = self.envelope()?;
        let normal = vec3_or(req, "normal", [0.0, 0.0, 1.0])?;
        let up = vec3_or(req, "up", [0.0, 1.0, 0.0])?;
        let origin = match get(req, "origin_mm") {
            Some(v) if py_truthy(v) => vec3(v, "origin_mm")?,
            _ => env.map(|e| e / 2.0),
        };
        let l = lod::get(&text(get(req, "lod"), lod::DEFAULT));
        let (_, u0, v0) = plane_box(origin, normal, up, 1.0, 1.0, 1.0, 1, 8);
        let (fw, fh) = self.fit_extent(u0, v0)?;
        let h_ref = self.evaluator.h_design() * 1000.0;
        let (b, u, v) = plane_box(origin, normal, up, fw, fh, h_ref, 1, 8);
        let t0 = Instant::now();
        let (reference, _) = self.evaluate(&b, &["rho", "q"], Some(ctx), "clamp", 1)?;
        let (current, _) = self.evaluate(&b, &["rho", "q"], Some(ctx), "clamp", l.geff_stride)?;
        let (mask, inside) = self.inside(&b)?;
        let masked = |f: &BTreeMap<String, Vec<f64>>| -> Vec<f64> {
            f.get("rho").map(|r| r.iter().zip(&mask).map(|(a, m)| a.min(*m)).collect()).unwrap_or_default()
        };
        let rr = masked(&reference);
        let rc = masked(&current);
        let empty = Vec::new();
        let (qr, qc) = (reference.get("q").unwrap_or(&empty), current.get("q").unwrap_or(&empty));
        let n = rr.len();
        let dq: Vec<f64> =
            (0..n).map(|i| (qc.get(i).unwrap_or(&0.0) - qr.get(i).unwrap_or(&0.0)).abs() * 1e6).collect();
        let near: Vec<f64> =
            (0..n).filter(|&i| (rr[i] - 0.5).abs() < 0.45 && inside[i]).map(|i| dq[i]).collect();
        let n_in = inside.iter().filter(|x| **x).count().max(1) as f64;
        let feat = FEATURE_PERIOD_MM_MIN * 1000.0;
        let p99 = if near.is_empty() { 0.0 } else { implexity_mesh::numeric::percentile(&near, 99.0) };
        let mx = near.iter().copied().fold(0.0_f64, f64::max);
        let sum = |a: &[f64]| implexity_mesh::numeric::pairwise_sum(a);
        let diff: Vec<f64> = (0..n).map(|i| rc[i] - rr[i]).collect();
        let max_abs = diff.iter().fold(0.0_f64, |m, d| m.max(d.abs()));
        let sq: Vec<f64> = diff.iter().map(|d| d * d).collect();
        let rms = (sum(&sq) / n.max(1) as f64).sqrt();
        let changed =
            (0..n).filter(|&i| inside[i] && (rc[i] - 0.5).signum() != (rr[i] - 0.5).signum()).count() as f64;
        let meta = json!({
            "kind": "compare", "units": "mm", "design_version": self.design_version(),
            "plane": {"origin_mm": origin, "normal": b.axes[2], "u_axis": u, "v_axis": v},
            "sampled_at_h_mm": py_round(b.h * 1000.0, 4),
            "compared": {"preview_lod": l.name, "preview_geff_stride": l.geff_stride,
                         "reference": "geff_stride 1 on the same grid, at the design's own spacing or slightly finer"},
            "solid_fraction": {"reference": sum(&rr) / n_in, "preview": sum(&rc) / n_in,
                               "delta": (sum(&rc) - sum(&rr)) / n_in},
            "iso_line_shift_um": {"p99": py_round(p99, 2), "max": py_round(mx, 2)},
            "iso_line_shift_as_fraction_of_a_feature": {"p99": py_round(p99 / feat, 4), "max": py_round(mx / feat, 4)},
            "rho": {"max_abs": max_abs, "rms": rms},
            "crossings_changed_pct": 100.0 * changed / n.max(1) as f64,
            "eval_ms": py_round(t0.elapsed().as_secs_f64() * 1000.0, 1),
            "note": "still not the exported model: see /v1/fidelity for the boundary layer and the \
                     preview-spacing effects this comparison holds fixed",
        });
        let Value::Object(meta) = meta else { return Err(failed("RuntimeError", "compare payload")) };
        Ok(PreviewResult::from_meta(meta))
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn fidelity_payload(h_design: f64, interface_w: f64) -> Value {
    json!({
        "units": "mm",
        "measured_by": "tests/test_fidelity.py -> tests/fidelity_result.json",
        "statement": "The preview draws the rho = 0.5 iso-surface of the same geometry map the exporter uses, \
                      evaluated at the preview spacing. It is not the exported model.",
        "exact_agreement": {
            "what": "interior of the part, geff_stride = 1, preview spacing = the design's own h",
            "rho_max_abs_deviation": 5.08e-14,
            "f_max_abs_deviation": 8.22e-14,
            "iso_crossings_changed_pct": 0.0,
            "measured_on": "the reference study's full-resolution design, 32x64x128, h=0.25mm"},
        "known_differences": [
            {"name": "part boundary layer",
             "detail": "the reference's box_blur3 replicates the array edge; the preview clamps halo sample \
                        coordinates into the envelope instead",
             "extent": "within r+1 = 5 samples of a face",
             "rho_max": 8.30e-2, "rho_rms": 3.68e-3,
             "d_volume_fraction": -7.38e-4,
             "iso_crossings_changed_pct": 0.174,
             "alternative": "boundary='continue' evaluates the real continuation instead: rho_max 1.97e-1, \
                             rms 1.28e-2, 0.779 % of crossings"},
            {"name": "geff_stride > 1",
             "detail": "the neighbourhood-coupled scalar geff is sampled coarsely and interpolated; the level \
                        set f is always evaluated at the full preview spacing",
             "iso_surface_shift_um_at_h_0p25": {
                 "stride2": {"p99": 29.1, "max": 57.5},
                 "stride3": {"p99": 69.1, "max": 135.3},
                 "stride4": {"p99": 87.4, "max": 181.7},
                 "stride6": {"p99": 139.9, "max": 200.5}},
             "iso_surface_shift_um_at_h_0p15": {
                 "stride2": {"p99": 15.7, "max": 27.2},
                 "stride3": {"p99": 32.7, "max": 66.4},
                 "stride4": {"p99": 53.0, "max": 93.7},
                 "stride6": {"p99": 87.4, "max": 179.9}},
             "against": "a 1160 um minimum feature period"},
            {"name": "preview spacing",
             "detail": "the box blur radius is an integer count of PREVIEW samples, so the physical blur length \
                        is quantised differently at every spacing. This is CAD_INTEGRATION.md section 5.1 seen \
                        from the preview side, and it is a property of the geometry map, not of this service.",
             "section_volume_fraction_by_h_mm": {
                 "0.1435": 0.5472, "0.2238": 0.5441, "0.2883": 0.5418, "0.3404": 0.5404, "0.4103": 0.5361},
             "blur_length_mm_by_h_mm": {
                 "0.1435": 1.0045, "0.2238": 0.8951, "0.2883": 0.8649, "0.3404": 1.0213, "0.4103": 0.8205},
             "h_design_mm": py_round(h_design * 1000.0, 4)},
            {"name": "interface width",
             "detail": "the preview fixes the projection interface at the design's own interface_w*h in METRES \
                        rather than rescaling it with the preview spacing. The rho = 0.5 iso-surface is \
                        unaffected, because the sigmoid is monotone and {rho > 1/2} = {q < tau} whatever the \
                        width; only the grey band would move.",
             "w_interface_mm": py_round(interface_w * h_design * 1000.0, 4)},
            {"name": "stretch potential",
             "detail": "the one globally-coupled quantity in the map -- the cumulative stretch integral -- is \
                        precomputed on the design's own grid and sampled trilinearly, which is deliberately the \
                        'discrete' reading of CAD_INTEGRATION.md section 5.5",
             "f_max_abs_deviation": 8.22e-14},
            {"name": "oblique slab frame",
             "detail": "the box blur is not rotation invariant: a slab aligned to the camera blurs |grad f| over \
                        a rotated box. Measured on a 6 mm cube at 45 deg against the axis-aligned one.",
             "d_mean_rho": 9.0e-3},
        ],
        "not_shown": [
            "the material channel is drawn as colour, not as a separate body",
            "the exported 3MF carries a 16-bit quantisation of the control fields worth 1.59e-04 of rho; the \
             preview does not quantise",
            "the preview caps the solid at the part envelope with a sub-sample mask; the exported model's \
             <levelset> does the same job with a different rule",
        ],
    })
}

fn snapshot_params(snap: &DesignSnapshot) -> Result<Arc<Params>, implexity_mesh::MeshError> {
    Arc::clone(&snap.params).downcast::<Params>().map_err(|_| {
        implexity_mesh::MeshError::invalid("the design snapshot does not hold preview parameters")
    })
}

fn points_of(pts: &[Vec<f64>; 3]) -> Vec<[f64; 3]> {
    (0..pts[0].len()).map(|i| [pts[0][i], pts[1][i], pts[2][i]]).collect()
}

const EYE: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn mesh_error(e: implexity_geometry::GeometryError) -> implexity_mesh::MeshError {
    match e {
        implexity_geometry::GeometryError::Key(k) => implexity_mesh::MeshError::Key(k),
        implexity_geometry::GeometryError::Cancelled => implexity_mesh::MeshError::Cancelled,
        other => implexity_mesh::MeshError::invalid(other.to_string()),
    }
}

impl BodyEvaluator for GeometryPreview {
    fn h_design(&self) -> f64 {
        self.evaluator.h_design()
    }

    fn blur_radius_design(&self) -> i64 {
        i64::try_from(self.evaluator.blur_r(self.evaluator.h_design())).unwrap_or(i64::MAX)
    }

    fn interface_eps(&self) -> f64 {
        self.evaluator.interface_eps()
    }

    fn domain(&self) -> [f64; 3] {
        self.design.meta().map_or([0.0; 3], |m| domain_mm(&m).map(|x| x * MM))
    }

    fn snapshot(&self) -> DesignSnapshot {
        let (params, meta, version) =
            self.design.snapshot_full().unwrap_or_else(|_| (Params::new(), Map::new(), 0));
        DesignSnapshot { version: Self::version_i64(version), meta, params: Arc::new(params) }
    }

    fn bind_continuation(&self, _snap: &DesignSnapshot) {

    }

    fn run_fast(
        &self,
        snap: &DesignSnapshot,
        pts: &[Vec<f64>; 3],
        h: f64,
    ) -> Result<FastFields, implexity_mesh::MeshError> {
        let params = snapshot_params(snap)?;
        let cont = self.evaluator.cont_vec(&snap.meta);
        let p = points_of(pts);
        let n = p.len();
        let mut out = self
            .evaluator
            .run(&params, &p, [n, 1, 1], h, EYE, 0, RunMode::Fast, &cont, None)
            .map_err(mesh_error)?;
        let mut take = |k: &str| out.remove(k).unwrap_or_default();
        Ok(FastFields { f: take("f"), nu: take("nu"), tau: take("tau"), mtilde: take("mtilde") })
    }

    fn run_geff(
        &self,
        snap: &DesignSnapshot,
        pts: &[Vec<f64>; 3],
        shape: [usize; 3],
        h: f64,
        r: i64,
    ) -> Result<Vec<f64>, implexity_mesh::MeshError> {
        let params = snapshot_params(snap)?;
        let cont = self.evaluator.cont_vec(&snap.meta);
        let p = points_of(pts);
        let r = usize::try_from(r.max(0)).unwrap_or(0);
        let mut out = self
            .evaluator
            .run(&params, &p, shape, h, EYE, r, RunMode::Geff, &cont, None)
            .map_err(mesh_error)?;
        Ok(out.remove("geff").unwrap_or_default())
    }

    fn geff_cache(&self) -> &GeffCache {
        &self.geff
    }
}
