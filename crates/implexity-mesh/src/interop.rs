// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use implexity_geometry::domain_sdf::{self, TriMesh};
use implexity_geometry::eval::{self, EvalOptions};
use implexity_geometry::fieldclass::FieldClass;
use implexity_geometry::node::{Attr, Mode, NodeRef, make};
use implexity_geometry::sampled::Sampled;
use implexity_geometry::value::{DType, NdArray, ParamValue};
use serde_json::{Map, Value, json};

use crate::MeshError;
use crate::numeric::{pairwise_sum, percentile};
use crate::pyfmt::{fmt_f, fmt_g};
use crate::topology::Vec3;

pub const DEFAULT_SPACING_MM: f64 = 0.5;
pub const PAD_CELLS: usize = 2;
pub const EXPORT_FORMATS: [&str; 4] = ["stl", "ply", "3mf", "step"];

#[must_use]
pub fn max_samples() -> usize {
    std::env::var("IMPLEXITY_MESHSDF_MAX_SAMPLES")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1 << 21)
}

fn sha16(data: &[u8]) -> String {
    implexity_io::digest::sha256_hex(data)[..16].to_string()
}

fn interop_err(msg: impl Into<String>) -> MeshError {
    MeshError::Invalid(msg.into())
}

fn geo(e: impl std::fmt::Display) -> MeshError {
    MeshError::Invalid(e.to_string())
}


pub fn read_mesh(data: &[u8], name: &str, units_mm: bool) -> Result<TriMesh, MeshError> {
    let label: String = name.chars().take(40).collect();
    let m = domain_sdf::read_mesh(data, name, if units_mm { 1e-3 } else { 1.0 })
        .and_then(|m| domain_sdf::check_mesh(&m, &label).map(|()| m))
        .map_err(|e| {
            interop_err(format!("this mesh cannot become a node:\n  {}", e.problems.join("\n  ")))
        })?;
    Ok(m)
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeshSamples {
    pub samples: Vec<f64>,
    pub shape: [usize; 3],
    pub origin: Vec3,
    pub spacing: f64,
}


pub fn sample_mesh(
    m: &TriMesh,
    spacing_mm: f64,
    pad_cells: usize,
    max_samples: usize,
) -> Result<MeshSamples, MeshError> {
    let h = spacing_mm * 1e-3;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &m.v {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let pad = pad_cells as f64;
    let lo: Vec3 = std::array::from_fn(|a| lo[a] - pad * h);
    let hi: Vec3 = std::array::from_fn(|a| hi[a] + pad * h);
    let n: [usize; 3] = std::array::from_fn(|a| crate::cast::trunc_usize(((hi[a] - lo[a]) / h).ceil()) + 1);
    let total = n[0] * n[1] * n[2];
    if total > max_samples {
        #[allow(clippy::cast_precision_loss)]
        let (t, b) = (total as f64 / 1e6, max_samples as f64 / 1e6);
        return Err(interop_err(format!(
            "spacing {} mm needs a {}x{}x{} = {}M-sample grid against {} triangles, over the budget of {}M \
             (IMPLEXITY_MESHSDF_MAX_SAMPLES).  Ask for a coarser spacing: the interpolation error is measured and \
             reported, so a coarse grid is a stated approximation rather than a hidden one",
            fmt_f(spacing_mm, 4),
            n[0],
            n[1],
            n[2],
            fmt_f(t, 2),
            m.f.len(),
            fmt_f(b, 2)
        )));
    }
    #[allow(clippy::cast_precision_loss)]
    let ax: [Vec<f64>; 3] = std::array::from_fn(|a| (0..n[a]).map(|i| lo[a] + i as f64 * h).collect());
    let mut pts = Vec::with_capacity(total);
    for &x in &ax[0] {
        for &y in &ax[1] {
            for &z in &ax[2] {
                pts.push([x, y, z]);
            }
        }
    }
    let (s, _w, _t) = domain_sdf::sdf_at(m, &pts);
    Ok(MeshSamples {
        samples: s.iter().map(|v| v * 1e3).collect(),
        shape: n,
        origin: lo.map(|v| v * 1e3),
        spacing: h * 1e3,
    })
}

fn f3(v: Vec3) -> ParamValue {
    ParamValue::List(v.iter().map(|x| ParamValue::Float(*x)).collect())
}

fn attr_of(v: &Value) -> Attr {
    match v {
        Value::Null => Attr::Null,
        Value::Bool(b) => Attr::Bool(*b),
        Value::Number(n) => n.as_i64().map_or_else(|| Attr::Float(n.as_f64().unwrap_or(f64::NAN)), Attr::Int),
        Value::String(s) => Attr::Str(s.clone()),
        Value::Array(a) => Attr::List(a.iter().map(attr_of).collect()),
        Value::Object(o) => Attr::Dict(o.iter().map(|(k, v)| (k.clone(), attr_of(v))).collect()),
    }
}


pub fn mesh_sdf_node(
    s: &MeshSamples,
    field_class: Option<FieldClass>,
    source: &Value,
    measurement: &Value,
) -> Result<NodeRef, MeshError> {
    let arr = NdArray::from_f64(s.shape.to_vec(), s.samples.clone())
        .ok_or_else(|| interop_err("sample count disagrees with the shape"))?;
    let params = [
        ("samples", ParamValue::Array(Arc::new(arr))),
        ("origin", f3(s.origin)),
        ("spacing", f3([s.spacing; 3])),
    ];
    let mut attrs: Vec<(&str, Attr)> = Vec::new();
    if let Some(fc) = field_class {
        attrs.push(("field_class", Attr::FieldClass(fc)));
    }
    if source.as_object().is_some_and(|o| !o.is_empty()) {
        attrs.push(("source", attr_of(source)));
    }
    if measurement.as_object().is_some_and(|o| !o.is_empty()) {
        attrs.push(("measurement", attr_of(measurement)));
    }
    make("mesh_sdf", Vec::new(), None, &params, &attrs).map_err(geo)
}


pub fn sample_box(node: &NodeRef) -> Result<(Vec3, Vec3), MeshError> {
    let s = node
        .op()
        .as_any()
        .downcast_ref::<Sampled>()
        .ok_or_else(|| interop_err(format!("{} has no sampled box", node.kind())))?;
    s.sample_box(node).map_err(geo)
}


#[allow(clippy::too_many_lines)]
pub fn measure_interpolation(
    node: &NodeRef,
    m: &TriMesh,
    samples: usize,
    seed: u128,
    grad_eps_frac: f64,
) -> Result<Value, MeshError> {
    let mut rng = implexity_core::rng::default_rng(seed);
    let (lo, hi) = sample_box(node)?;
    let spacing =
        node.param("spacing").and_then(|v| v.to_f64_array().ok()).map(|(_, d)| d).unwrap_or_default();
    let h = spacing.iter().copied().fold(f64::INFINITY, f64::min);
    let r = rng.random_vec(samples * 3);
    let p: Vec<Vec3> =
        (0..samples).map(|i| std::array::from_fn(|a| lo[a] + r[3 * i + a] * (hi[a] - lo[a]))).collect();
    let opts = EvalOptions::exact();
    let fi = eval::eval_points(node, &p, &opts).map_err(geo)?;
    let pm: Vec<Vec3> = p.iter().map(|q| q.map(|v| v * 1e-3)).collect();
    let (fx, _w, _t) = domain_sdf::sdf_at(m, &pm);
    let fx: Vec<f64> = fx.iter().map(|v| v * 1e3).collect();
    let err: Vec<f64> = fi.iter().zip(&fx).map(|(a, b)| a - b).collect();
    let e = grad_eps_frac * h;
    let mut g = vec![[0.0; 3]; samples];
    for a in 0..3 {
        let plus: Vec<Vec3> = p
            .iter()
            .map(|q| {
                let mut x = *q;
                x[a] += e;
                x
            })
            .collect();
        let minus: Vec<Vec3> = p
            .iter()
            .map(|q| {
                let mut x = *q;
                x[a] -= e;
                x
            })
            .collect();
        let fp = eval::eval_points(node, &plus, &opts).map_err(geo)?;
        let fm = eval::eval_points(node, &minus, &opts).map_err(geo)?;
        for i in 0..samples {
            g[i][a] = (fp[i] - fm[i]) / (2.0 * e);
        }
    }
    let gn: Vec<f64> = g.iter().map(|v| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()).collect();
    let near: Vec<usize> = (0..samples).filter(|&i| fx[i].abs() <= 2.0 * h).collect();
    #[allow(clippy::cast_precision_loss)]
    let nf = samples as f64;
    let abs_err: Vec<f64> = err.iter().map(|x| x.abs()).collect();
    let max_abs = abs_err.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let sq: Vec<f64> = err.iter().map(|x| x * x).collect();
    let near_max = near.iter().map(|&i| abs_err[i]).fold(f64::NEG_INFINITY, f64::max);
    Ok(json!({
        "samples": samples, "spacing_mm": h, "seed": seed,
        "against": "implexity.domain.sdf.sdf_at (exact point-triangle distance, generalized winding number sign)",
        "error": {
            "max_abs_mm": max_abs,
            "rms_mm": (pairwise_sum(&sq) / nf).sqrt(),
            "mean_mm": pairwise_sum(&err) / nf,
            "max_over_report_mm": err.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            "max_under_report_mm": err.iter().copied().fold(f64::INFINITY, f64::min),
            "max_abs_frac_of_h": max_abs / h,
            "near_surface_max_abs_mm": if near.is_empty() { Value::Null } else { json!(near_max) },
            "near_surface_points": near.len(),
        },
        "gradient": {
            "max": gn.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            "mean": pairwise_sum(&gn) / nf,
            "p99": percentile(&gn, 99.0),
            "min": gn.iter().copied().fold(f64::INFINITY, f64::min),
            "eps_mm": e,
            "note": "central differences at half the sample spacing; an exact signed distance function has |grad f| = 1 \
                     everywhere",
        },
    }))
}


pub fn class_from_measurement(m: &Value) -> Result<FieldClass, MeshError> {
    let k = m["gradient"]["max"].as_f64().ok_or_else(|| interop_err("measurement lacks gradient.max"))?;
    let samples = m["samples"].as_i64().unwrap_or(0);
    let note = format!(
        "trilinear interpolant of an exact mesh SDF sampled at {} mm; measured |grad f| <= {} and |interp - sdf_at| <= \
         {} mm ({} of a sample spacing) over {} random points in the sampled box",
        fmt_f(m["spacing_mm"].as_f64().unwrap_or(0.0), 4),
        fmt_f(k, 4),
        fmt_f(m["error"]["max_abs_mm"].as_f64().unwrap_or(0.0), 4),
        fmt_f(m["error"]["max_abs_frac_of_h"].as_f64().unwrap_or(0.0), 2),
        samples
    );
    FieldClass::from_measurement(k.max(1e-12), samples, &note).map_err(geo)
}

#[derive(Clone, Debug)]
pub struct MeshSdfOptions<'a> {
    pub units_mm: bool,
    pub spacing_mm: f64,
    pub measure: bool,
    pub measure_samples: usize,
    pub seed: u128,
    pub name: Option<&'a str>,
    pub pad_cells: usize,
}

impl Default for MeshSdfOptions<'_> {
    fn default() -> Self {
        Self {
            units_mm: true,
            spacing_mm: DEFAULT_SPACING_MM,
            measure: true,
            measure_samples: 2000,
            seed: 20_250_828,
            name: None,
            pad_cells: PAD_CELLS,
        }
    }
}

pub enum MeshSource<'a> {
    Path(&'a Path),
    Bytes(&'a [u8]),
}


pub fn mesh_sdf(
    source: &MeshSource<'_>,
    opts: &MeshSdfOptions<'_>,
) -> Result<(NodeRef, Option<Value>), MeshError> {
    let (blob, label) = match source {
        MeshSource::Bytes(b) => (b.to_vec(), opts.name.unwrap_or("<bytes>").to_string()),
        MeshSource::Path(p) => (
            std::fs::read(p).map_err(|e| MeshError::io(format!("reading {}", p.display()), e))?,
            opts.name.map_or_else(
                || p.file_name().map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
                str::to_string,
            ),
        ),
    };
    let read_name = match source {
        MeshSource::Path(p) => p.display().to_string(),
        MeshSource::Bytes(_) => label.clone(),
    };
    let m = read_mesh(&blob, &read_name, opts.units_mm)?;
    let s = sample_mesh(&m, opts.spacing_mm, opts.pad_cells, max_samples())?;
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &m.v {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let sha = sha16(&blob);
    let src = json!({"kind": "mesh", "name": label, "units_mm": opts.units_mm, "sha256": sha, "bytes": blob.len(),
                     "triangles": m.f.len(), "vertices": m.v.len(), "spacing_mm": s.spacing, "grid": s.shape,
                     "bbox_mm": [lo.map(|v| v * 1e3), hi.map(|v| v * 1e3)]});
    let node = mesh_sdf_node(&s, None, &json!({}), &json!({}))?;
    if !opts.measure {
        let node = mesh_sdf_node(&s, None, &src, &json!({}))?;
        return Ok((node, None));
    }
    let meas = measure_interpolation(&node, &m, opts.measure_samples, opts.seed, 0.5)?;
    let fc = class_from_measurement(&meas)?;
    let node = mesh_sdf_node(&s, Some(fc), &src, &meas)?;
    Ok((node, Some(meas)))
}

fn ndarray_of(a: &implexity_io::npy::NpyArray) -> Result<NdArray, MeshError> {
    use implexity_io::npy::NpyData as D;
    let le = |bytes: Vec<u8>, dt: DType| NdArray::from_le_bytes(dt, a.shape.clone(), &bytes);
    let out = match &a.data {
        D::F64(v) => le(v.iter().flat_map(|x| x.to_le_bytes()).collect(), DType::F64),
        D::F32(v) => le(v.iter().flat_map(|x| x.to_le_bytes()).collect(), DType::F32),
        D::I64(v) => le(v.iter().flat_map(|x| x.to_le_bytes()).collect(), DType::I64),
        D::I32(v) => le(v.iter().flat_map(|x| x.to_le_bytes()).collect(), DType::I32),
        D::I16(v) => le(v.iter().flat_map(|x| x.to_le_bytes()).collect(), DType::I16),
        D::I8(v) => le(v.iter().flat_map(|x| x.to_le_bytes()).collect(), DType::I8),
        D::U8(v) => le(v.clone(), DType::U8),
        D::Bool(v) => le(v.iter().map(|&b| u8::from(b)).collect(), DType::Bool),
        _ => a.to_f64().and_then(|x| NdArray::from_f64(a.shape.clone(), x.iter().copied().collect())),
    };
    out.ok_or_else(|| interop_err("this array's dtype cannot become a grid field"))
}


#[allow(clippy::too_many_arguments)]
pub fn grid_field(
    values: NdArray,
    origin_mm: Vec3,
    spacing_mm: Vec3,
    field_class: Option<FieldClass>,
    scale: f64,
    offset: f64,
    source: &Value,
    measurement: &Value,
) -> Result<NodeRef, MeshError> {
    let params = [
        ("samples", ParamValue::Array(Arc::new(values))),
        ("origin", f3(origin_mm)),
        ("spacing", f3(spacing_mm)),
        ("scale", ParamValue::Float(scale)),
        ("offset", ParamValue::Float(offset)),
    ];
    let attrs = [
        ("field_class", Attr::FieldClass(field_class.unwrap_or_else(FieldClass::implicit))),
        ("source", attr_of(if source.is_null() { &Value::Null } else { source })),
        ("measurement", attr_of(if measurement.is_null() { &Value::Null } else { measurement })),
    ];
    let attrs: Vec<(&str, Attr)> = attrs
        .into_iter()
        .filter(|(k, a)| *k == "field_class" || matches!(a, Attr::Dict(d) if !d.is_empty()))
        .collect();
    make("grid_field", Vec::new(), None, &params, &attrs).map_err(geo)
}


pub fn grid_field_from_file(
    path: &Path,
    key: Option<&str>,
    origin_mm: Vec3,
    spacing_mm: Vec3,
    field_class: Option<FieldClass>,
) -> Result<NodeRef, MeshError> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|e| MeshError::io(format!("reading {}", path.display()), e))?;
    let name = path.file_name().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let mut src = Map::new();
    src.insert("kind".into(), json!("file"));
    src.insert("name".into(), json!(name));
    src.insert("bytes".into(), json!(bytes.len()));
    let arr = if ext == "npy" {
        implexity_io::npy::NpyArray::from_bytes(&bytes).map_err(geo)?
    } else if ext == "npz" {
        let z = implexity_io::npz::load(&bytes).map_err(geo)?;
        let names: Vec<String> = z.files().iter().map(|s| (*s).to_string()).collect();
        let k = match key {
            Some(k) => k.to_string(),
            None if names.len() == 1 => names[0].clone(),
            None => {
                return Err(interop_err(format!(
                    "{} holds {} arrays ({}); name one with key=",
                    path.display(),
                    names.len(),
                    names.join(", ")
                )));
            }
        };
        let Some(a) = z.get(&k) else {
            return Err(interop_err(format!(
                "{} has no array {}; it has {}",
                path.display(),
                implexity_core::py_repr::repr_str(&k),
                names.join(", ")
            )));
        };
        src.insert("key".into(), json!(k));
        a.clone()
    } else if ext == "vdb" {
        return Err(interop_err(format!(
            "reading {} needs pyopenvdb, which is not installed here; convert the grid to .npy, or install openvdb's \
             python bindings",
            path.display()
        )));
    } else {
        return Err(interop_err(format!("{}: a grid field is read from .npy, .npz or .vdb", path.display())));
    };
    src.insert("sha256".into(), json!(sha16(&bytes)));
    grid_field(
        ndarray_of(&arr)?,
        origin_mm,
        spacing_mm,
        field_class,
        1.0,
        0.0,
        &Value::Object(src),
        &json!({}),
    )
}

#[must_use]
pub fn subtree_box(node: &NodeRef) -> Option<(Vec3, Vec3)> {
    if let Some(b) = eval::aabb_of(node) {
        return Some(b);
    }
    let boxes: Vec<(Vec3, Vec3)> = node.walk().iter().filter_map(|(_p, n)| sample_box(n).ok()).collect();
    if boxes.is_empty() {
        return None;
    }
    let lo = std::array::from_fn(|a| boxes.iter().map(|b| b.0[a]).fold(f64::INFINITY, f64::min));
    let hi = std::array::from_fn(|a| boxes.iter().map(|b| b.1[a]).fold(f64::NEG_INFINITY, f64::max));
    Some((lo, hi))
}


pub fn sample_field(
    node: &NodeRef,
    lo: Vec3,
    hi: Vec3,
    h: f64,
    opts: &EvalOptions,
) -> Result<(Vec<f64>, Vec3, [usize; 3]), MeshError> {
    let n: [usize; 3] =
        std::array::from_fn(|a| crate::cast::trunc_usize(((hi[a] - lo[a]) / h).floor().max(0.0)) + 1);
    if n.iter().any(|&v| v < 2) {
        return Err(interop_err(format!(
            "spacing {} mm gives a [{}, {}, {}] grid; the box is {} x {} x {} mm",
            fmt_f(h, 4),
            n[0],
            n[1],
            n[2],
            fmt_f(hi[0] - lo[0], 3),
            fmt_f(hi[1] - lo[1], 3),
            fmt_f(hi[2] - lo[2], 3)
        )));
    }
    #[allow(clippy::cast_precision_loss)]
    let ax: [Vec<f64>; 3] = std::array::from_fn(|a| (0..n[a]).map(|i| lo[a] + i as f64 * h).collect());
    let mut p = Vec::with_capacity(n.iter().product());
    for &x in &ax[0] {
        for &y in &ax[1] {
            for &z in &ax[2] {
                p.push([x, y, z]);
            }
        }
    }
    let k = eval::compile_f64(node, opts).map_err(geo)?;
    let mut out = Vec::with_capacity(p.len());
    for chunk in p.chunks(200_000) {
        out.extend(eval::eval_kernel(&k, chunk));
    }
    Ok((out, lo, n))
}

#[derive(Clone, Debug)]
pub struct ExportOptions<'a> {
    pub fmt: Option<&'a str>,
    pub spacing_mm: Option<f64>,
    pub bbox_mm: Option<(Vec3, Vec3)>,
    pub pad_mm: Option<f64>,
    pub mode: Mode,
    pub field_class: Option<FieldClass>,
    pub name: &'a str,
    pub step: crate::step::StepOptions<'a>,
}

impl Default for ExportOptions<'_> {
    fn default() -> Self {
        Self {
            fmt: None,
            spacing_mm: None,
            bbox_mm: None,
            pad_mm: None,
            mode: Mode::Exact,
            field_class: None,
            name: "implexity implicit body",
            step: crate::step::StepOptions::default(),
        }
    }
}

fn fmt_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "stl" => Some("stl"),
        "ply" => Some("ply"),
        "3mf" => Some("3mf"),
        "step" | "stp" => Some("step"),
        _ => None,
    }
}


#[allow(clippy::too_many_lines)]
pub fn export_subtree(node: &NodeRef, path: &Path, opts: &ExportOptions<'_>) -> Result<Value, MeshError> {
    let fmt = opts
        .fmt
        .map(str::to_lowercase)
        .or_else(|| fmt_of(path).map(str::to_string))
        .unwrap_or_else(|| "stl".into());
    if !EXPORT_FORMATS.contains(&fmt.as_str()) {
        return Err(interop_err(format!(
            "format {}; this exports {}",
            implexity_core::py_repr::repr_str(&fmt),
            EXPORT_FORMATS.join(", ")
        )));
    }
    let Some((lo, hi)) = opts.bbox_mm.or_else(|| subtree_box(node)) else {
        return Err(interop_err(
            "this subtree has no sampled leaf, so it has no extent of its own: pass bbox_mm=[[x0,y0,z0],[x1,y1,z1]].  An \
             implicit primitive is defined on all of R^3 and only the caller knows which part of it is the part",
        ));
    };
    let h = opts.spacing_mm.unwrap_or(DEFAULT_SPACING_MM);
    let pad = opts.pad_mm.unwrap_or(2.0 * h);
    let (lo, hi) = (lo.map(|v| v - pad), hi.map(|v| v + pad));
    let eopts = EvalOptions { mode: opts.mode, ..EvalOptions::exact() };
    let (mut phi, origin, shape) = sample_field(node, lo, hi, h, &eopts)?;
    let mode = if opts.mode == Mode::Exact { "exact" } else { "smooth" };
    let mut rep = Map::new();
    for (k, v) in [
        ("kind", json!("implicit_export")),
        ("units", json!("mm")),
        ("format", json!(fmt)),
        ("node", json!(node.kind())),
        ("structure_id", json!(node.structure_id())),
        ("content_id", json!(node.content_id())),
        ("mode", json!(mode)),
        ("spacing_mm", json!(h)),
        ("grid", json!(shape)),
        ("samples", json!(shape.iter().product::<usize>())),
        ("origin_mm", json!(origin)),
        ("bbox_mm", json!([lo, hi])),
    ] {
        rep.insert(k.into(), v);
    }
    let fc = opts.field_class.clone().or_else(|| eval::field_class_of(node, opts.mode).ok());
    if let Some(fc) = fc {
        rep.insert("field_class".into(), fc.as_json());
        rep.insert(
            "meshing_note".into(),
            json!(fc.safe_step_factor().map_or_else(
                || "this field is IMPLICIT: no cell can be proven surface-free, so the sampling density is the only \
                    thing standing between the extraction and a missed feature"
                    .to_string(),
                |s| format!("cells can be pruned: |f| * {} > half the cell diagonal proves no surface inside", fmt_f(s, 4))
            )),
        );
        rep.insert("cell_diagonal_mm".into(), json!(h * 3.0f64.sqrt()));
        if fc.measured() {
            rep.insert(
                "field_class_is_measured".into(),
                json!(format!(
                    "the Lipschitz constant {} was MEASURED over {} samples; the extraction below is correct to the \
                     extent that measurement holds away from those samples",
                    fmt_g(fc.k(), 6),
                    fc.samples()
                )),
            );
        }
    }
    let lo_v = phi.iter().copied().fold(f64::INFINITY, f64::min);
    let hi_v = phi.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    rep.insert("field_range".into(), json!([lo_v, hi_v]));
    if !(lo_v < 0.0 && 0.0 < hi_v) {
        rep.insert("triangles".into(), json!(0));
        rep.insert(
            "empty".into(),
            json!(format!(
                "the field does not change sign anywhere on this grid (range {} .. {} mm), so there is no surface to \
                 extract",
                fmt_g(lo_v, 4),
                fmt_g(hi_v, 4)
            )),
        );
        return Ok(Value::Object(rep));
    }
    let eps = crate::bodyexport::DEADBAND_FRAC * h;
    let mut dead = 0usize;
    for v in &mut phi {
        if v.abs() < eps {
            *v = if *v < 0.0 { -eps } else { eps };
            dead += 1;
        }
    }
    rep.insert("deadband_nodes".into(), json!(dead));
    let (vi, fi) = crate::bodyexport::mc_slabs_f64(&phi, shape, crate::bodyexport::slab_cells(), None)?;
    drop(phi);

    let h32 = crate::cast::f32_of(h);
    let v0: Vec<Vec3> =
        vi.iter().map(|p| std::array::from_fn(|a| origin[a] + f64::from(p[a] * h32))).collect();
    let (v, f, degenerate) = crate::topology::weld_exact(&v0, &fi);
    let tp = crate::topology::topology_fast(&v, &f);
    let watertight = tp.boundary_edges == 0 && tp.nonmanifold_edges == 0;
    rep.insert("triangles".into(), json!(f.len()));
    rep.insert("vertices".into(), json!(v.len()));
    rep.insert("degenerate_dropped".into(), json!(degenerate));
    rep.insert("topology".into(), tp.to_json());
    rep.insert("watertight".into(), json!(watertight));
    rep.insert("volume_mm3".into(), json!(crate::topology::signed_volume(&v, &f)));
    rep.insert("area_mm2".into(), json!(pairwise_sum(&crate::topology::areas(&v, &f))));
    if !watertight {
        rep.insert(
            "watertight_note".into(),
            json!(format!(
                "{} boundary edge(s) and {} non-manifold edge(s): the level set reaches the edge of the sampled box.  \
                 Widen bbox_mm or raise pad_mm",
                tp.boundary_edges, tp.nonmanifold_edges
            )),
        );
    }
    let bytes = match fmt.as_str() {
        "stl" => crate::formats::write_stl(path, &v, &f, 1.0)?,
        "ply" => crate::formats::write_ply(
            path,
            &v,
            &f,
            1.0,
            &[format!("implicit content_id {}", node.content_id()), format!("spacing_mm {}", fmt_f(h, 6))],
        )?,
        "3mf" => crate::formats::write_3mf(path, &v, &f, 1.0, opts.name, &[], &[])?,
        _ => {
            let so = crate::step::StepOptions { name: opts.name, unit_scale: 1.0, ..opts.step.clone() };
            let brep = crate::step::mesh_to_step(&v, &f, path, &so, None)?;
            rep.insert("brep".into(), brep);
            std::fs::metadata(path).map_or(0, |m| m.len())
        }
    };
    rep.insert("bytes".into(), json!(bytes));
    rep.insert("path".into(), json!(path.display().to_string()));
    Ok(Value::Object(rep))
}

#[must_use]
pub fn export_available() -> BTreeMap<String, Value> {
    EXPORT_FORMATS
        .iter()
        .map(|f| {
            let detail = match *f {
                "3mf" => "native OPC/3MF writer (implexity-mesh)",
                "step" => crate::step::have_writer().1,
                _ => "standard library",
            };
            ((*f).to_string(), json!({"available": true, "detail": detail}))
        })
        .collect()
}
