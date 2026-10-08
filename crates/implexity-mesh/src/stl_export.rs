// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use base64::Engine as _;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::MeshError;
use crate::cast::{f32_of, i64_of};
use crate::formats::{Triangle32, read_stl_bytes, stl_bytes};
use crate::model_view::{ModelView, evaluate_model_blocks, registered_field_sampler};
use crate::partition::{PartitionError, closed_grid_axes, mesh_partition, superposition_check};
use implexity_core::json::{DumpOptions, dumps};
use implexity_core::pyobj::repr as py_repr;

use crate::pyfmt::fmt_g;
use crate::topology::surface_report;
use crate::zip::{ZipMethod, write_zip};

pub const SCHEMA: &str = "implexity-stl-export/1";
pub const MANIFEST_SCHEMA: &str = "implexity-stl-export-manifest/1";
pub const FILE_SCHEMA: &str = "implexity-inline-file/1";
pub const STL_MIME: &str = "model/stl";
pub const ZIP_MIME: &str = "application/zip";
pub const MAX_EXPORT_NODES: usize = 4_000_000;
pub const MAX_INLINE_BYTES: usize = 40 * 1024 * 1024;
pub const ZIP_DEFLATE_ALLOWANCE: usize = 4;
pub const MAX_ZIP_UNCOMPRESSED_BYTES: usize = ZIP_DEFLATE_ALLOWANCE * MAX_INLINE_BYTES;
pub const MAX_CATEGORIES: usize = 16;
pub const REFINEMENTS: [i64; 4] = [1, 2, 3, 4];
pub const ANALYTIC_AXIS_CELLS: usize = 128;
pub const VOLUME_RTOL: f64 = 1e-9;
pub const APPROACH: &str = "One closed cell-centred grid over the export box (box faces are grid nodes); the model \
     field and the material field are sampled once on it. Every cell is split into six Freudenthal tetrahedra, \
     on which both fields are linear, and the arrangement of the solid level set and the material thresholds is \
     meshed once: each interface polygon is emitted a single time with the regions on its two sides, and every \
     file takes the polygons that bound its region set, outward oriented.  Shared interfaces are therefore the \
     same triangles in both files, and the material files superpose to the solid file exactly.";

fn err(m: impl Into<String>) -> MeshError {
    MeshError::Invalid(m.into())
}

fn is_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn is_field(s: &str) -> bool {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-');
    if !s.is_empty() && s.chars().all(plain) {
        return true;
    }
    let seg = |p: &str| {
        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    let Some((path, name)) = s.rsplit_once(':') else { return false };
    let mut parts = path.split('/');
    parts.next() == Some("model") && parts.all(seg) && seg(name)
}

fn is_job(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn finite(
    value: Option<&Value>,
    label: &str,
    lower: Option<f64>,
    upper: Option<f64>,
) -> Result<f64, MeshError> {
    let Some(v) = value.and_then(Value::as_f64).filter(|_| !value.is_some_and(Value::is_boolean)) else {
        return Err(err(format!("{label} must be a number")));
    };
    if !v.is_finite() {
        return Err(err(format!("{label} must be finite")));
    }
    if let Some(lo) = lower
        && v < lo
    {
        return Err(err(format!("{label} must be >= {}", fmt_g(lo, 6))));
    }
    if let Some(hi) = upper
        && v > hi
    {
        return Err(err(format!("{label} must be <= {}", fmt_g(hi, 6))));
    }
    Ok(v)
}

fn source(raw: Option<&Value>) -> Result<Value, MeshError> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else { return Ok(json!({"kind": "current_model"})) };
    let Some(src) = raw.as_object() else { return Err(err("export source must be an object")) };
    let kind = src.get("kind").and_then(Value::as_str);
    if kind == Some("current_model") {
        if src.len() != 1 {
            return Err(err("current_model source takes only its kind"));
        }
        return Ok(json!({"kind": "current_model"}));
    }
    let Some(kind @ ("current_optimization_state" | "optimization_epoch")) = kind else {
        return Err(err(
            "export source kind must be current_model, current_optimization_state or optimization_epoch",
        ));
    };
    let keys: Vec<&str> = src.keys().map(String::as_str).collect();
    if keys.len() != 3 || !["epoch", "job_id", "kind"].iter().all(|k| keys.contains(k)) {
        return Err(err(format!("{kind} source requires exactly kind, job_id and epoch")));
    }
    let job = src.get("job_id").and_then(Value::as_str).filter(|j| is_job(j));
    let Some(job) = job else { return Err(err("source job_id must be 12 lowercase hex characters")) };
    let epoch = &src["epoch"];
    let live_current = kind == "current_optimization_state" && epoch.as_str() == Some("current");
    let nonnegative_int = epoch.is_u64() || epoch.as_i64().is_some_and(|e| e >= 0);
    if !live_current && !nonnegative_int {
        return Err(err(format!(
            "source epoch must be a nonnegative integer{}",
            if kind == "current_optimization_state" { " or current" } else { "" }
        )));
    }
    Ok(json!({"kind": kind, "job_id": job, "epoch": epoch}))
}

fn category(raw: &Value, label: &str) -> Result<Value, MeshError> {
    let obj = raw.as_object().filter(|o| o.keys().all(|k| k == "id" || k == "label"));
    let Some(obj) = obj else {
        return Err(err(format!("{label} must be an object with id and optional label")));
    };
    let ident = obj.get("id").and_then(Value::as_str).filter(|s| is_id(s));
    let Some(ident) = ident else {
        return Err(err(format!("{label} id must match [A-Za-z0-9][A-Za-z0-9_.-]{{0,63}}")));
    };
    let text = match obj.get("label") {
        None => Some(ident),
        Some(v) => v.as_str(),
    };
    let text = text.filter(|t| (1..=160).contains(&t.chars().count()));
    let Some(text) = text else { return Err(err(format!("{label} label must be 1..160 characters"))) };
    Ok(json!({"id": ident, "label": text}))
}



#[allow(clippy::too_many_lines)]
pub fn normalise_request(raw: &Value) -> Result<Value, MeshError> {
    let Some(raw) = raw.as_object() else { return Err(err("export_stl payload must be an object")) };
    let allowed = [
        "source",
        "extent_mm",
        "resolution",
        "include_solid",
        "materials",
        "complement",
        "bundle",
        "file_stem",
    ];
    let mut unknown: Vec<&str> = raw.keys().map(String::as_str).filter(|k| !allowed.contains(k)).collect();
    if !unknown.is_empty() {
        unknown.sort_unstable();
        let list = Value::Array(unknown.iter().map(|k| json!(k)).collect());
        return Err(err(format!("export_stl has unknown fields {}", py_repr(&list))));
    }
    let mut request = Map::new();
    request.insert("source".into(), source(raw.get("source"))?);

    let extent = match raw.get("extent_mm").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(e) => {
            let rows = e
                .as_array()
                .filter(|r| r.len() == 2 && r.iter().all(|x| x.as_array().is_some_and(|x| x.len() == 3)));
            let Some(rows) = rows else { return Err(err("extent_mm must be [[x0,y0,z0],[x1,y1,z1]]")) };
            let mut out = [[0.0; 3]; 2];
            for (r, row) in rows.iter().enumerate() {
                for (c, v) in row.as_array().into_iter().flatten().enumerate() {
                    out[r][c] = finite(Some(v), "extent_mm", Some(-1e6), Some(1e6))?;
                }
            }
            if (0..3).any(|a| out[1][a] <= out[0][a]) {
                return Err(err("extent_mm upper bounds must exceed lower bounds"));
            }
            json!(out)
        }
    };
    request.insert("extent_mm".into(), extent);

    let default_resolution = json!({"refinement": 1});
    let resolution = raw.get("resolution").unwrap_or(&default_resolution);
    let res = resolution
        .as_object()
        .filter(|r| r.len() == 1 && r.keys().all(|k| k == "refinement" || k == "spacing_mm"));
    let Some(res) = res else {
        return Err(err("resolution is exactly one of {refinement: 1..4} or {spacing_mm: length}"));
    };
    if let Some(v) = res.get("refinement") {
        let ok = v.as_i64().filter(|r| v.is_i64() && REFINEMENTS.contains(r));
        let Some(r) = ok else { return Err(err("resolution refinement must be 1, 2, 3 or 4")) };
        request.insert("resolution".into(), json!({"refinement": r}));
    } else {
        let s = finite(res.get("spacing_mm"), "resolution spacing_mm", Some(1e-3), Some(1e3))?;
        request.insert("resolution".into(), json!({"spacing_mm": s}));
    }

    let include_solid = match raw.get("include_solid") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(err("include_solid must be boolean")),
    };
    request.insert("include_solid".into(), json!(include_solid));

    let materials = match raw.get("materials").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(m) => {
            let obj = m.as_object().filter(|o| {
                o.len() == 3 && ["field", "thresholds", "categories"].iter().all(|k| o.contains_key(*k))
            });
            let Some(obj) = obj else {
                return Err(err("materials requires exactly field, thresholds and categories"));
            };
            let field =
                obj["field"].as_str().filter(|f| (1..=240).contains(&f.chars().count()) && is_field(f));
            let Some(field) = field else {
                return Err(err("materials field must be a registered field name"));
            };
            let cats = obj["categories"].as_array().filter(|c| (2..=MAX_CATEGORIES).contains(&c.len()));
            let Some(cats) = cats else {
                return Err(err(format!("materials categories must list 2..{MAX_CATEGORIES} categories")));
            };
            let categories: Vec<Value> =
                cats.iter().map(|c| category(c, "material category")).collect::<Result<_, _>>()?;
            let th = obj["thresholds"].as_array().filter(|t| t.len() + 1 == categories.len());
            let Some(th) = th else { return Err(err("materials needs one fewer threshold than categories")) };
            let thresholds: Vec<f64> = th
                .iter()
                .map(|t| finite(Some(t), "material threshold", Some(-1e12), Some(1e12)))
                .collect::<Result<_, _>>()?;
            if thresholds.windows(2).any(|p| p[1] <= p[0]) {
                return Err(err("material thresholds must strictly increase"));
            }
            json!({"field": field, "thresholds": thresholds, "categories": categories})
        }
    };
    request.insert("materials".into(), materials.clone());

    let complement = match raw.get("complement").filter(|v| !v.is_null()) {
        None => Value::Null,
        Some(c) => match c.as_object() {
            Some(obj) => {
                let mut merged = Map::new();
                merged.insert("id".into(), json!("complement"));
                for (k, v) in obj {
                    merged.insert(k.clone(), v.clone());
                }
                category(&Value::Object(merged), "complement")?
            }
            None => category(c, "complement")?,
        },
    };
    request.insert("complement".into(), complement.clone());

    let mut ids: Vec<String> = if include_solid { vec!["solid".into()] } else { vec![] };
    if let Some(cats) = materials.get("categories").and_then(Value::as_array) {
        ids.extend(cats.iter().filter_map(|c| c["id"].as_str().map(String::from)));
    }
    if let Some(id) = complement.get("id").and_then(Value::as_str) {
        ids.push(id.to_string());
    }
    if ids.is_empty() {
        return Err(err("nothing to export: include the solid, materials or the complement"));
    }
    let lowered: Vec<String> = ids.iter().map(|i| i.to_lowercase()).collect();
    let mut unique = lowered.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != lowered.len() || (!include_solid && lowered.iter().any(|i| i == "solid")) {
        return Err(err("part ids must be unique (case-insensitive) and 'solid' is reserved"));
    }
    let bundle = match raw.get("bundle") {
        None => "files",
        Some(v) => match v.as_str() {
            Some(b @ ("files" | "zip")) => b,
            _ => return Err(err("bundle must be files or zip")),
        },
    };
    request.insert("bundle".into(), json!(bundle));
    let stem = match raw.get("file_stem") {
        None => "implexity",
        Some(v) => match v.as_str().filter(|s| is_id(s)) {
            Some(s) => s,
            None => return Err(err("file_stem must match [A-Za-z0-9][A-Za-z0-9_.-]{0,63}")),
        },
    };
    request.insert("file_stem".into(), json!(stem));
    Ok(Value::Object(request))
}

fn spacing(request: &Value, view: &dyn ModelView, extent: [[f64; 3]; 2]) -> Result<(f64, Value), MeshError> {
    let resolution = &request["resolution"];
    if let Some(s) = resolution.get("spacing_mm").and_then(Value::as_f64) {
        return Ok((
            s,
            json!({"spacing_source": "request_spacing_mm", "target_spacing_mm": s, "refinement": null,
                   "declared_spacing_mm": null}),
        ));
    }
    let refinement = resolution["refinement"].as_i64().unwrap_or(1);
    if let Some(hint) = view.geometry_sampling_hint()? {
        let target = hint.spacing_mm / refinement as f64;
        return Ok((
            target,
            json!({"spacing_source": "model_declared_geometry_lattice", "declared_by": hint.declared_by,
                   "declared_spacing_mm": hint.spacing_mm, "refinement": refinement,
                   "target_spacing_mm": target}),
        ));
    }
    let span = (0..3).map(|a| extent[1][a] - extent[0][a]).fold(f64::NEG_INFINITY, f64::max);
    let cells = ANALYTIC_AXIS_CELLS * usize::try_from(refinement).unwrap_or(1);
    let target = span / cells as f64;
    Ok((
        target,
        json!({"spacing_source": "analytic_default_longest_axis", "analytic_longest_axis_cells": cells,
               "declared_spacing_mm": null, "refinement": refinement, "target_spacing_mm": target}),
    ))
}

struct Sampled {
    axes: [Vec<f64>; 3],
    solid: Vec<f64>,
    phase: Option<Vec<f64>>,
    grid: Map<String, Value>,
    material_record: Value,
}

fn stale(m: &str) -> MeshError {
    MeshError::Stale(m.to_string())
}

#[allow(clippy::too_many_lines)]
fn sample(view: &dyn ModelView, request: &Value, expected: &str) -> Result<Sampled, MeshError> {
    let (sampled, evaluated) = {
        let _guard = view.live_lock();
        let status = view.status()?;
        if status.content_id != expected {
            return Err(stale("the model changed before it was exported"));
        }
        let (extent, extent_source) = if let Some(rows) = request["extent_mm"].as_array() {
            let r = |i: usize, a: usize| rows[i][a].as_f64().unwrap_or(0.0);
            ([[r(0, 0), r(0, 1), r(0, 2)], [r(1, 0), r(1, 1), r(1, 2)]], "request_extent_mm")
        } else {
            {
                let known = status.aabb.as_ref().filter(|b| b.get("known") == Some(&Value::Bool(true)));
                let Some(b) = known else {
                    return Err(err("the model has no known bounded extent; provide extent_mm"));
                };
                let g = |i: usize, a: usize| b["bbox_mm"][i][a].as_f64().unwrap_or(0.0);
                ([[g(0, 0), g(0, 1), g(0, 2)], [g(1, 0), g(1, 1), g(1, 2)]], "known_model_aabb")
            }
        };
        let (target, resolution) = spacing(request, view, extent)?;
        let (axes, grid) = match closed_grid_axes(extent[0], extent[1], [target; 3], Some(MAX_EXPORT_NODES)) {
            Ok(v) => v,
            Err(PartitionError::GridTooLarge { nodes, .. }) => {
                return Err(err(format!(
                    "the export grid needs {nodes} samples at {} mm, above the explicit {MAX_EXPORT_NODES}-sample \
                     bound; lower the refinement, raise spacing_mm or crop extent_mm",
                    fmt_g(target, 6)
                )));
            }
            Err(e) => return Err(err(e.to_string())),
        };
        let mut points = Vec::with_capacity(axes[0].len() * axes[1].len() * axes[2].len());
        for &x in &axes[0] {
            for &y in &axes[1] {
                for &z in &axes[2] {
                    points.push([x, y, z]);
                }
            }
        }
        let (values, evaluated) = match evaluate_model_blocks(view, &points, expected) {
            Ok(v) => v,
            Err(MeshError::Invalid(m)) => return Err(MeshError::Stale(m)),
            Err(e) => return Err(e),
        };
        let mut material_record = Value::Null;
        let mut phase = None;
        if let Some(materials) = request["materials"].as_object() {
            let field = materials["field"].as_str().unwrap_or("");
            let rows = view.registered_fields()?;
            let descriptor = rows.iter().find(|r| r["field"].as_str() == Some(field));
            let Some(descriptor) = descriptor else {
                return Err(err(
                    "the materials field is not a registered render field of this model (inspect_renderables lists them)",
                ));
            };
            let categorical = descriptor.get("categorical").is_some_and(|c| !c.is_null());
            let (sampler, registration) = registered_field_sampler(view, field, categorical)?;
            let (p, valid) = sampler.sample(&points, 0.0)?;
            if !valid.iter().all(|&v| v) {
                return Err(err(format!(
                    "the materials field registration does not cover the export extent; crop extent_mm to its \
                     bbox_mm {}",
                    py_repr(&descriptor["registration"]["bbox_mm"])
                )));
            }
            if registration.get("content_id").and_then(Value::as_str) != Some(expected) {
                return Err(stale("the model changed while its material field was read"));
            }
            phase = Some(p);
            let thresholds: Vec<f64> =
                materials["thresholds"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect();
            let cats = materials["categories"].as_array().cloned().unwrap_or_default();
            let n = cats.len();
            let categories: Vec<Value> = cats
                .into_iter()
                .enumerate()
                .map(|(k, c)| {
                    let mut c = c.as_object().cloned().unwrap_or_default();
                    let lo = if k == 0 { Value::Null } else { json!(thresholds[k - 1]) };
                    let hi = if k == n - 1 { Value::Null } else { json!(thresholds[k]) };
                    c.insert("range".into(), json!([lo, hi]));
                    Value::Object(c)
                })
                .collect();
            let get = |k: &str| descriptor.get(k).cloned().unwrap_or(Value::Null);
            material_record = json!({
                "field": field, "label": get("label"), "units": get("units"), "node": get("node"),
                "source": get("source"), "registration": get("registration"),
                "value_range": get("value_range"),
                "sampling": if categorical { "nearest_registered_cell" } else { "trilinear_registered_grid" },
                "thresholds": thresholds,
                "rule": "category k holds field values in [threshold[k-1], threshold[k]); the first is unbounded \
                         below, the last above; a value equal to a threshold belongs to the upper category",
                "categories": categories,
            });
        }
        let mut grid = grid.as_object().cloned().unwrap_or_default();
        for (k, v) in resolution.as_object().into_iter().flatten() {
            grid.insert(k.clone(), v.clone());
        }
        grid.insert("extent_source".into(), json!(extent_source));
        (Sampled { axes, solid: values, phase, grid, material_record }, evaluated)
    };
    if view.status()?.content_id != expected {
        return Err(stale("the model changed while it was sampled"));
    }
    let mut sampled = sampled;
    sampled.grid.insert("evaluator".into(), evaluated.get("evaluator").cloned().unwrap_or(Value::Null));
    sampled.grid.insert("evaluation_mode".into(), evaluated.get("mode").cloned().unwrap_or(Value::Null));
    sampled.grid.insert("evaluation_blocks".into(), evaluated.get("blocks").cloned().unwrap_or(Value::Null));
    sampled.grid.insert("solid_rule".into(), json!("model field < 0 is solid; = 0 is outside"));
    Ok(sampled)
}

fn inline(filename: &str, mime: &str, blob: &[u8]) -> Value {
    json!({"schema": FILE_SCHEMA, "filename": filename, "mime_type": mime, "bytes": blob.len(),
           "sha256": hex::encode(Sha256::digest(blob)),
           "data_base64": base64::engine::general_purpose::STANDARD.encode(blob)})
}

fn header(part_id: &str, content_id: &str) -> Vec<u8> {
    let text = format!("implexity binary STL mm part={part_id} model={content_id}");
    text.as_bytes().iter().copied().filter(u8::is_ascii).take(80).collect()
}

type Written = (usize, String, Vec<u8>, Vec<Triangle32>, Value);

struct Part {
    kind: &'static str,
    id: String,
    label: String,
    regions: Vec<i64>,
    category_index: Option<usize>,
}



#[allow(clippy::too_many_lines)]
pub fn export(
    view: &dyn ModelView,
    request: &Value,
    source_identity: &Value,
    truth_status: &str,
    expected: &str,
) -> Result<Value, MeshError> {
    let t_start = std::time::Instant::now();
    let mut timing = Map::new();
    let s = sample(view, request, expected)?;
    timing.insert("sampling".into(), json!(t_start.elapsed().as_secs_f64()));
    let materials = request["materials"].as_object();
    let thresholds: Vec<f64> = materials
        .map(|m| m["thresholds"].as_array().into_iter().flatten().filter_map(Value::as_f64).collect())
        .unwrap_or_default();
    let t = std::time::Instant::now();
    let mesh =
        mesh_partition(&s.axes, &s.solid, s.phase.as_deref(), &thresholds).map_err(|e| err(e.to_string()))?;
    timing.insert("partition".into(), json!(t.elapsed().as_secs_f64()));

    let categories: Vec<Value> = materials
        .and_then(|m| m["categories"].as_array().cloned())
        .unwrap_or_else(|| vec![json!({"id": "solid", "label": "solid"})]);
    let solid_regions: Vec<i64> = (1..=i64_of(categories.len())).collect();
    let mut parts: Vec<Part> = Vec::new();
    if request["include_solid"].as_bool().unwrap_or(true) {
        parts.push(Part {
            kind: "solid",
            id: "solid".into(),
            label: "solid".into(),
            regions: solid_regions.clone(),
            category_index: None,
        });
    }
    if materials.is_some() {
        for (k, c) in categories.iter().enumerate() {
            parts.push(Part {
                kind: "material",
                id: c["id"].as_str().unwrap_or("").to_string(),
                label: c["label"].as_str().unwrap_or("").to_string(),
                regions: vec![i64_of(k) + 1],
                category_index: Some(k),
            });
        }
    }
    if let Some(c) = request["complement"].as_object() {
        parts.push(Part {
            kind: "complement",
            id: c["id"].as_str().unwrap_or("").to_string(),
            label: c["label"].as_str().unwrap_or("").to_string(),
            regions: vec![0],
            category_index: None,
        });
    }

    let t = std::time::Instant::now();
    let stem = request["file_stem"].as_str().unwrap_or("implexity");
    let content_id = source_identity
        .get("content_id")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .unwrap_or(expected);
    let bundle = request["bundle"].as_str().unwrap_or("files");
    let mut surfaces: Vec<(String, crate::partition::WeldedSurface)> = Vec::new();
    let mut estimated = 0usize;
    for part in &parts {
        if !surfaces.iter().any(|(k, _)| *k == part.id) {
            let surf = mesh.surface(&part.regions).map_err(|e| err(e.to_string()))?;
            surfaces.push((part.id.clone(), surf));
        }
        let surf = &surfaces.iter().find(|(k, _)| *k == part.id).map(|(_, s)| s);
        estimated += 84 + 50 * surf.map_or(0, |s| s.1.len());
    }
    if estimated > MAX_INLINE_BYTES && bundle == "files" {
        return Err(err(format!(
            "the requested files total {estimated} bytes, above the {MAX_INLINE_BYTES}-byte inline bound; request \
             bundle=zip, a coarser resolution, a cropped extent_mm or fewer parts"
        )));
    }
    if estimated > MAX_ZIP_UNCOMPRESSED_BYTES && bundle == "zip" {
        return Err(err(format!(
            "the requested files total {estimated} uncompressed bytes, above the {MAX_ZIP_UNCOMPRESSED_BYTES}-byte \
             bound for a zip bundle (the {MAX_INLINE_BYTES}-byte inline bound at the assumed best-case deflate ratio \
             {ZIP_DEFLATE_ALLOWANCE}:1); use a coarser resolution, a cropped extent_mm or fewer parts"
        )));
    }
    let surface_of = |id: &str| surfaces.iter().find(|(k, _)| k == id).map(|(_, s)| s);
    let mut written: Vec<Written> = Vec::new();
    for (pi, part) in parts.iter().enumerate() {
        let Some((v, f, dropped)) = surface_of(&part.id) else { continue };
        let v64: Vec<[f64; 3]> = v.iter().map(|p| p.map(f64::from)).collect();
        let blob = stl_bytes(&v64, f, 1.0, &header(&part.id, content_id))?;
        let (triangles, _head) = read_stl_bytes(&blob)?;
        let mut report = surface_report(v, f);
        report["collapsed_triangles_dropped"] = json!(dropped);
        let filename = format!("{stem}_{}.stl", part.id);
        written.push((pi, filename, blob, triangles, report));
    }
    timing.insert("files".into(), json!(t.elapsed().as_secs_f64()));

    let t = std::time::Instant::now();
    let mut checks = Map::new();
    checks.insert(
        "all_files_watertight".into(),
        json!(written.iter().all(|w| w.4["watertight"] == json!(true) || w.4["empty"] == json!(true))),
    );
    let by_id = |id: &str| written.iter().find(|w| parts[w.0].id == id).map(|w| (&w.3, &w.4));
    let (solid_tris, solid_rep): (Vec<Triangle32>, Value) = match by_id("solid") {
        Some((tri, rep)) => (tri.clone(), rep.clone()),
        None => {
            if materials.is_some() || request["complement"].is_object() {
                let (v, f, _d) = mesh.surface(&solid_regions).map_err(|e| err(e.to_string()))?;
                (f.iter().map(|t| t.map(|i| v[i])).collect(), surface_report(&v, &f))
            } else {
                (Vec::new(), Value::Null)
            }
        }
    };
    let lo32: Vec<f64> = s.axes.iter().map(|a| f64::from(f32_of(a[0]))).collect();
    let hi32: Vec<f64> = s.axes.iter().map(|a| f64::from(f32_of(a[a.len() - 1]))).collect();
    let box_volume = (hi32[0] - lo32[0]) * (hi32[1] - lo32[1]) * (hi32[2] - lo32[2]);
    let tolerance = VOLUME_RTOL * box_volume;
    let solid_volume = solid_rep["volume_mm3"].as_f64().unwrap_or(0.0);
    if materials.is_some() {
        let ids: Vec<String> =
            categories.iter().map(|c| c["id"].as_str().unwrap_or("").to_string()).collect();
        let mats: Vec<(&Vec<Triangle32>, &Value)> = ids.iter().filter_map(|id| by_id(id)).collect();
        let volume_sum =
            crate::numeric::py_sum(mats.iter().map(|(_t, r)| r["volume_mm3"].as_f64().unwrap_or(0.0)));
        let part_tris: Vec<Vec<Triangle32>> = mats.iter().map(|(t, _)| (*t).clone()).collect();
        let superposed = superposition_check(&part_tris, Some(&solid_tris));
        let difference = volume_sum - solid_volume;
        let vols: Map<String, Value> =
            ids.iter().zip(&mats).map(|(id, (_t, r))| (id.clone(), r["volume_mm3"].clone())).collect();
        let ok = difference.abs() <= tolerance && superposed["passed"] == json!(true);
        checks.insert(
            "material_partition".into(),
            json!({"materials": ids, "material_volumes_mm3": vols, "sum_of_material_volumes_mm3": volume_sum,
                   "solid_volume_mm3": solid_volume, "volume_difference_mm3": difference,
                   "volume_tolerance_mm3": tolerance, "volume_within_tolerance": difference.abs() <= tolerance,
                   "superposition": superposed, "passed": ok}),
        );
    }
    if let Some(c) = request["complement"].as_object() {
        let cid = c["id"].as_str().unwrap_or("");
        if let Some((comp_tris, comp_rep)) = by_id(cid) {
            let superposed = superposition_check(&[solid_tris.clone(), comp_tris.clone()], None);
            let comp_volume = comp_rep["volume_mm3"].as_f64().unwrap_or(0.0);
            let total = solid_volume + comp_volume;
            let difference = total - box_volume;
            let ok = difference.abs() <= tolerance && superposed["passed"] == json!(true);
            checks.insert(
                "complement_partition".into(),
                json!({"complement": cid, "solid_volume_mm3": solid_volume, "complement_volume_mm3": comp_volume,
                       "sum_mm3": total, "extent_box_volume_mm3": box_volume, "volume_difference_mm3": difference,
                       "volume_tolerance_mm3": tolerance, "volume_within_tolerance": difference.abs() <= tolerance,
                       "superposition": superposed, "passed": ok}),
            );
        }
    }
    let sub_ok = |k: &str| checks.get(k).is_none_or(|c| c["passed"] == json!(true));
    let passed = checks["all_files_watertight"] == json!(true)
        && sub_ok("material_partition")
        && sub_ok("complement_partition");
    checks.insert("passed".into(), json!(passed));
    checks.insert(
        "volume_tolerance_rule".into(),
        json!(format!(
            "|difference| <= {} x extent box volume, on float64 sums over the float32 vertices read back from the \
             written files",
            fmt_g(VOLUME_RTOL, 6)
        )),
    );
    timing.insert("checks".into(), json!(t.elapsed().as_secs_f64()));

    let mut file_rows = Vec::new();
    for (pi, filename, blob, _tri, report) in &written {
        let part = &parts[*pi];
        let mut row = Map::new();
        row.insert("filename".into(), json!(filename));
        row.insert("part".into(), json!(part.kind));
        row.insert("id".into(), json!(part.id));
        row.insert("label".into(), json!(part.label));
        row.insert("regions".into(), json!(part.regions));
        row.insert("mime_type".into(), json!(STL_MIME));
        row.insert("bytes".into(), json!(blob.len()));
        row.insert("sha256".into(), json!(hex::encode(Sha256::digest(blob))));
        for k in [
            "triangles",
            "vertices",
            "empty",
            "watertight",
            "boundary_edges",
            "nonmanifold_edges",
            "orientation_consistent",
            "volume_mm3",
            "area_mm2",
            "bbox_mm",
        ] {
            row.insert(k.into(), report.get(k).cloned().unwrap_or(Value::Null));
        }
        row.insert("components".into(), report.get("components").cloned().unwrap_or(Value::Null));
        row.insert("genus_total".into(), report.get("genus_total").cloned().unwrap_or(Value::Null));
        row.insert("collapsed_triangles_dropped".into(), report["collapsed_triangles_dropped"].clone());
        if let Some(k) = part.category_index {
            row.insert("material_range".into(), s.material_record["categories"][k]["range"].clone());
        }
        file_rows.push(Value::Object(row));
    }
    let mut resolution: Map<String, Value> =
        s.grid.iter().filter(|(k, _)| k.as_str() != "nodes").map(|(k, v)| (k.clone(), v.clone())).collect();
    resolution.insert("samples".into(), s.grid.get("nodes").cloned().unwrap_or(Value::Null));
    let manifest = json!({
        "schema": MANIFEST_SCHEMA, "units": "mm", "format": "binary_stl",
        "source": source_identity, "truth_status": truth_status,
        "resolution": resolution, "materials": s.material_record,
        "files": file_rows, "checks": checks, "approach": APPROACH,
    });
    let files: Vec<Value> = if bundle == "zip" {
        let manifest_blob = (dumps(&manifest, &DumpOptions::indented(2).sorted(true)) + "\n").into_bytes();
        let mut entries: Vec<(String, Vec<u8>)> =
            written.iter().map(|w| (w.1.clone(), w.2.clone())).collect();
        entries.push((format!("{stem}_manifest.json"), manifest_blob));
        entries.sort();
        let archive =
            write_zip(&entries.into_iter().map(|(n, b)| (n, b, ZipMethod::Deflated)).collect::<Vec<_>>())?;
        vec![inline(&format!("{stem}_stl.zip"), ZIP_MIME, &archive)]
    } else {
        written.iter().map(|w| inline(&w.1, STL_MIME, &w.2)).collect()
    };
    let total: u64 = files.iter().filter_map(|f| f["bytes"].as_u64()).sum();
    if total > MAX_INLINE_BYTES as u64 {
        return Err(err(format!(
            "the export is {total} bytes, above the {MAX_INLINE_BYTES}-byte inline bound; use a coarser resolution, \
             a cropped extent_mm or fewer parts"
        )));
    }
    let timing: Map<String, Value> = timing
        .into_iter()
        .map(|(k, v)| (k, json!((v.as_f64().unwrap_or(0.0) * 1000.0).round_ties_even() / 1000.0)))
        .collect();
    Ok(json!({
        "schema": SCHEMA, "kind": "export_stl", "truth_status": truth_status, "source": source_identity,
        "units": "mm", "format": "binary_stl",
        "representation_truth": {
            "source_identity_bound": true,
            "surface_is_piecewise_linear_interpolant_of_sampled_fields": true,
            "surface_is_exact_geometry": false, "geometry_smoothing": "none",
            "closed_at_extent_box": true, "shared_interfaces_identical_across_files": true,
            "manufacturing_qualified": false,
        },
        "resolution": manifest["resolution"], "materials": manifest["materials"],
        "files": manifest["files"], "checks": manifest["checks"],
        "partition": mesh.stats, "bundle": bundle, "manifest": manifest,
        "delivery": {"mode": "inline_base64", "files": files, "total_bytes": total,
                     "max_inline_bytes": MAX_INLINE_BYTES},
        "approach": APPROACH, "timing_s": timing,
    }))
}

