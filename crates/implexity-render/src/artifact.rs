// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_possible_wrap)]

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use implexity_mesh::Field3;
use implexity_mesh::raster::{self, Annotation, Background, ScalarSectionOptions, SectionPalette};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::RenderError;
use crate::render3d::{
    self, CapColour, ColourSpec, RasterOptions, Render3dRequest, RenderMesh, VertexColouring,
};

fn err(m: impl Into<String>) -> RenderError {
    RenderError::Invalid(m.into())
}



pub fn validate_field_name(value: Option<&Value>, label: &str) -> Result<String, RenderError> {
    let ok = value
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.chars().count() <= 240 && render3d::is_artifact_field(s));
    ok.map(String::from).ok_or_else(|| err(format!("{label} must be a bounded result-artifact field name")))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Registration {
    pub shape: [usize; 3],
    pub origin: [f64; 3],
    pub matrix: [[f64; 3]; 3],
    pub centering: String,
    pub axis_order: String,
    pub frame: String,
    pub wire: Value,
}

impl Registration {
    fn diag(&self) -> [f64; 3] {
        [self.matrix[0][0], self.matrix[1][1], self.matrix[2][2]]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredArray {
    pub shape: Vec<usize>,
    pub dtype: String,
    pub bytes: Vec<u8>,
    pub values: Vec<f64>,
    pub real_numeric: bool,
}

pub trait ResultArtifactStore {


    fn inspect(&self, artifact_id: &str) -> Result<(Value, BTreeSet<String>), RenderError>;



    fn read_arrays(
        &self,
        artifact_id: &str,
        names: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, StoredArray>, RenderError>;



    fn registration_from_wire(&self, wire: &Value) -> Result<Registration, RenderError>;
}



pub fn registration_bbox(r: &Registration) -> Result<[[f64; 3]; 2], RenderError> {
    let diag = r.diag();
    let off_diag_ok = (0..3).all(|i| (0..3).all(|j| i == j || r.matrix[i][j].abs() <= 1e-12));
    if r.axis_order != "xyz" || r.frame != "model" || !off_diag_ok || diag.iter().any(|d| *d <= 0.0) {
        return Err(err(
            "result-artifact rendering currently requires a positive, axis-aligned model-frame registration",
        ));
    }
    let extent: [f64; 3] = std::array::from_fn(|a| {
        let n = r.shape[a] as f64;
        if r.centering == "cell" { n } else { (n - 1.0).max(0.0) }
    });
    Ok([r.origin, std::array::from_fn(|a| r.origin[a] + diag[a] * extent[a])])
}

fn declared_mask(d: &Map<String, Value>) -> Option<&Value> {
    match d.get("mask") {
        Some(v) => Some(v),
        None => d.get("phase_mask"),
    }
}

fn mask_name(field_name: &str, raw: Option<&Value>) -> Result<Option<String>, RenderError> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else { return Ok(None) };
    let name = match raw {
        Value::String(_) => validate_field_name(Some(raw), "mask field")?,
        Value::Object(o) => validate_field_name(o.get("field"), "mask field")?,
        _ => return Err(err("result-artifact field mask must be an object")),
    };
    if field_name.contains("::") && !name.contains("::") {
        let prefix = field_name.rsplit_once("::").map_or("", |(p, _)| p);
        return Ok(Some(format!("{prefix}::{name}")));
    }
    Ok(Some(name))
}

#[derive(Clone, Debug)]
pub struct FieldRecord {
    pub values: Vec<f64>,
    pub shape: Vec<usize>,
    pub stored: StoredArray,
    pub registration: Registration,
    pub bbox_mm: [[f64; 3]; 2],
    pub descriptor: Value,
}

#[derive(Clone, Debug)]
pub struct ArtifactContext {
    pub manifest: Value,
    pub source: Value,
    pub fields: BTreeMap<String, FieldRecord>,
}



pub fn scalarize_record(record: FieldRecord, selector: Option<&Value>) -> Result<FieldRecord, RenderError> {
    let d = &record.descriptor;
    let reg = &d["registration"];
    let shape: Vec<usize> = reg["shape"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect())
        .unwrap_or_default();
    if d["rank"] == "scalar" && record.shape == shape {
        if selector.is_some() {
            return Err(err("scalar field does not take vector scalarization"));
        }
        return Ok(record);
    }
    let mut vshape = shape.clone();
    vshape.push(3);
    if d["rank"] != "vector"
        || record.shape != vshape
        || !record.stored.real_numeric
        || record.values.iter().any(|v| !v.is_finite())
        || d["association"] != "cell"
        || reg["centering"] != "cell"
    {
        return Err(err("scalarization requires a finite registered three-component cell vector"));
    }
    let components: Vec<String> = d["components"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default();
    let mut sorted = components.clone();
    sorted.sort();
    let units_ok = d["units"].as_str().is_some_and(|u| !u.is_empty());
    if components.len() != 3 || sorted != ["x", "y", "z"] || !units_ok {
        return Err(err("declared x/y/z component order and units required"));
    }
    let Some(sel) = selector.and_then(Value::as_object) else {
        return Err(err("vector section requires explicit scalarization"));
    };
    let n = record.values.len() / 3;
    let (values, operation, suffix) = if sel.len() == 1
        && sel.get("component").and_then(Value::as_str).is_some_and(|c| ["x", "y", "z"].contains(&c))
    {
        let name = sel["component"].as_str().unwrap_or("x");
        let idx = components.iter().position(|c| c == name).unwrap_or(0);
        let v: Vec<f64> = (0..n).map(|i| record.values[3 * i + idx]).collect();
        (v, json!({"component": name}), format!(" component {name}"))
    } else if sel.len() == 1 && sel.get("magnitude") == Some(&Value::Bool(true)) {
        if d["component_frame"] != "model_cartesian" || reg["frame"] != "model" {
            return Err(err("magnitude requires explicit model_cartesian component frame"));
        }
        let v: Vec<f64> = (0..n)
            .map(|i| record.values[3 * i].hypot(record.values[3 * i + 1]).hypot(record.values[3 * i + 2]))
            .collect();
        if v.iter().any(|x| !x.is_finite()) {
            return Err(err("vector magnitude overflows display representation"));
        }
        (v, json!({"magnitude": true}), " magnitude".to_string())
    } else {
        return Err(err("choose exactly one component x/y/z or magnitude true"));
    };
    let mut descriptor = d.as_object().cloned().unwrap_or_default();
    descriptor.insert("rank".into(), json!("scalar"));
    descriptor.insert("shape".into(), json!(shape));
    let base_label = d
        .get("label")
        .and_then(Value::as_str)
        .filter(|l| !l.is_empty())
        .map_or_else(|| d["field"].as_str().unwrap_or("").to_string(), String::from);
    descriptor.insert("label".into(), json!(format!("{base_label}{suffix}")));
    let magnitude = operation.get("magnitude").is_some();
    descriptor.insert(
        "scalarization".into(),
        json!({
            "source_field": d["field"], "source_rank": "vector", "source_shape": record.shape,
            "source_dtype": record.stored.dtype,
            "source_array_bytes_sha256": hex::encode(Sha256::digest(&record.stored.bytes)),
            "components": components, "component_frame": d.get("component_frame").cloned().unwrap_or(Value::Null),
            "operation": operation, "operation_order": "stored_cell_scalarization_then_scalar_interpolation",
            "units_unchanged": d["units"],
            "interpretation": "display-only scalar; original immutable vector unchanged",
        }),
    );
    descriptor.insert("suggested_palette".into(), json!(if magnitude { "sequential" } else { "diverging" }));
    Ok(FieldRecord { values, shape, descriptor: Value::Object(descriptor), ..record })
}



#[allow(clippy::too_many_lines)]
pub fn load_fields(
    store: &dyn ResultArtifactStore,
    artifact_id: &str,
    names: &[String],
    scalarizations: Option<&Map<String, Value>>,
) -> Result<ArtifactContext, RenderError> {
    let mut requested = BTreeSet::new();
    for n in names {
        requested.insert(validate_field_name(Some(&json!(n)), "field")?);
    }
    let (manifest, inspected) = store.inspect(artifact_id)?;
    let metadata_root = manifest.get("metadata").and_then(Value::as_object).cloned().unwrap_or_default();
    let declarations = metadata_root.get("fields").and_then(Value::as_object).cloned().unwrap_or_default();
    let missing: Vec<&String> = requested.iter().filter(|n| !inspected.contains(*n)).collect();
    if !missing.is_empty() {
        return Err(err(format!("result artifact has no fields {}", py_str_list(&missing))));
    }
    let mut expanded = requested.clone();
    for name in &requested {
        let raw = declarations.get(name).and_then(Value::as_object).cloned().unwrap_or_default();
        if let Some(mask) = mask_name(name, declared_mask(&raw))? {
            expanded.insert(mask);
        }
    }
    let missing: Vec<&String> = expanded.iter().filter(|n| !inspected.contains(*n)).collect();
    if !missing.is_empty() {
        return Err(err(format!("result-artifact masks reference missing fields {}", py_str_list(&missing))));
    }
    let arrays = store.read_arrays(artifact_id, &expanded)?;
    if arrays.keys().cloned().collect::<BTreeSet<_>>() != expanded {
        return Err(err("result artifact changed while its fields were read"));
    }
    let global_registration = metadata_root.get("field_registration").cloned();
    let mut fields = BTreeMap::new();
    for (name, array) in arrays {
        let raw = declarations.get(&name).and_then(Value::as_object).cloned().unwrap_or_default();
        let wire =
            raw.get("registration").filter(|v| !v.is_null()).cloned().or_else(|| global_registration.clone());
        let Some(wire) = wire.filter(Value::is_object) else {
            return Err(err(format!("result-artifact field {} has no explicit registration", py_str(&name))));
        };
        let registration = store.registration_from_wire(&wire)?;
        let selector = scalarizations.and_then(|s| s.get(&name));
        let scalar_shape = array.shape.len() == 3 && array.shape[..] == registration.shape[..];
        let mut vshape = registration.shape.to_vec();
        vshape.push(3);
        let vector_shape =
            selector.is_some() && raw.get("rank") == Some(&json!("vector")) && array.shape == vshape;
        if vector_shape
            && (raw.get("association") != Some(&json!("cell"))
                || raw.get("units").is_none_or(|u| u.as_str().is_none_or(str::is_empty)))
        {
            return Err(err("vector section requires declared cell association and units"));
        }
        if !(scalar_shape || vector_shape)
            || !array.real_numeric
            || array.values.iter().any(|v| !v.is_finite())
        {
            return Err(err(format!(
                "result-artifact field {} is not a finite registered scalar field",
                py_str(&name)
            )));
        }
        if registration.centering != "cell" && registration.centering != "node" {
            return Err(err("result-artifact rendering requires cell or node centred fields"));
        }
        if registration.centering == "node" && (!scalar_shape || raw.get("association") != Some(&json!("node")) || raw.get("rank") != Some(&json!("scalar"))) {
            return Err(err("node-centred rendering requires an explicitly registered scalar node field"));
        }
        let bbox = registration_bbox(&registration)?;
        let declared_palette = raw.get("suggested_palette").and_then(Value::as_str).filter(|s| !s.is_empty());
        let suggested = match declared_palette {
            Some(p) => p,
            None if raw.get("categorical").is_some_and(Value::is_object) => "categorical",
            None => "sequential",
        };
        let mut descriptor = raw.clone();
        descriptor.insert("field".into(), json!(name));
        descriptor.insert(
            "label".into(),
            json!(raw.get("label").and_then(Value::as_str).filter(|l| !l.is_empty()).unwrap_or(&name)),
        );
        descriptor.insert(
            "units".into(),
            json!(
                raw.get("units").and_then(Value::as_str).filter(|u| !u.is_empty()).unwrap_or("unspecified")
            ),
        );
        descriptor.insert("association".into(), json!(registration.centering));
        descriptor.insert("rank".into(), json!(if vector_shape { "vector" } else { "scalar" }));
        descriptor.insert("suggested_palette".into(), json!(suggested));
        let mut wire_out = registration.wire.as_object().cloned().unwrap_or_default();
        wire_out.insert("bbox_mm".into(), json!(bbox));
        descriptor.insert("registration".into(), Value::Object(wire_out));
        if suggested == "categorical" {
            let declared = descriptor.get("categorical").filter(|v| !v.is_null());
            let pal = match declared {
                None => render3d::categorical_palette_from_values(&array.values, 256)?,
                Some(d) => render3d::normalise_categorical_palette(Some(d))?,
            };
            descriptor.insert("categorical".into(), pal.to_json());
        }
        let mut record = FieldRecord {
            values: array.values.clone(),
            shape: array.shape.clone(),
            stored: array,
            registration,
            bbox_mm: bbox,
            descriptor: Value::Object(descriptor),
        };
        if selector.is_some() {
            record = scalarize_record(record, selector)?;
        }
        fields.insert(name, record);
    }
    let identities = manifest.get("identities").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut source = json!({"kind": "result_artifact", "artifact_id": artifact_id});
    for key in ["provider", "model_content_id", "solve_id", "design_state_id"] {
        source[key] = identities.get(key).cloned().unwrap_or(Value::Null);
    }
    Ok(ArtifactContext { manifest, source, fields })
}

fn py_str(s: &str) -> String {
    implexity_core::pyobj::repr(&json!(s))
}

fn py_str_list(items: &[&String]) -> String {
    implexity_core::pyobj::repr(&Value::Array(items.iter().map(|s| json!(s)).collect()))
}



pub fn sample(
    record: &FieldRecord,
    points: &[[f64; 3]],
    categorical: bool,
) -> Result<(Vec<f64>, Vec<bool>), RenderError> {
    let reg = &record.registration;
    let diag = reg.diag();
    let bbox = record.bbox_mm;
    let largest = bbox.iter().flatten().map(|v| v.abs()).fold(0.0f64, f64::max);
    let scale = largest.max(1.0);
    let shape = reg.shape;
    let exterior = record.descriptor.get("exterior_value").filter(|v| !v.is_null());
    let exterior = match exterior {
        None => None,
        Some(v) => {
            let e = v.as_f64().unwrap_or(f64::NAN);
            if !e.is_finite() {
                return Err(err("result-artifact exterior_value must be finite"));
            }
            Some(e)
        }
    };
    let f = Field3 { shape, data: &record.values };
    let mut out = Vec::with_capacity(points.len());
    let mut valid = Vec::with_capacity(points.len());
    for p in points {
        let offset = if reg.centering == "node" { 0.0 } else { 0.5 };
        let q: [f64; 3] = std::array::from_fn(|a| (p[a] - reg.origin[a]) / diag[a] - offset);
        valid.push((0..3).all(|a| p[a] >= bbox[0][a] - 1e-10 * scale && p[a] <= bbox[1][a] + 1e-10 * scale));
        if categorical {
            let idx: [usize; 3] =
                std::array::from_fn(|a| (q[a].round_ties_even().max(0.0) as usize).min(shape[a] - 1));
            out.push(f.at(idx[0], idx[1], idx[2]));
            continue;
        }

        let (pad, dshape) =
            if exterior.is_some() { (1.0, [shape[0] + 2, shape[1] + 2, shape[2] + 2]) } else { (0.0, shape) };
        let get = |i: usize, j: usize, k: usize| -> f64 {
            match exterior {
                Some(e) => {
                    if i == 0 || j == 0 || k == 0 || i > shape[0] || j > shape[1] || k > shape[2] {
                        e
                    } else {
                        f.at(i - 1, j - 1, k - 1)
                    }
                }
                None => f.at(i, j, k),
            }
        };
        let coords: [f64; 3] = std::array::from_fn(|a| (q[a] + pad).max(0.0).min(dshape[a] as f64 - 1.0));
        let lo: [usize; 3] = std::array::from_fn(|a| coords[a].floor() as usize);
        let hi: [usize; 3] = std::array::from_fn(|a| (lo[a] + 1).min(dshape[a] - 1));
        let fr: [f64; 3] = std::array::from_fn(|a| coords[a] - lo[a] as f64);
        let mut s = 0.0;
        for bx in 0..2 {
            for by in 0..2 {
                for bz in 0..2 {
                    let i = if bx == 1 { hi[0] } else { lo[0] };
                    let j = if by == 1 { hi[1] } else { lo[1] };
                    let k = if bz == 1 { hi[2] } else { lo[2] };
                    let w = (if bx == 1 { fr[0] } else { 1.0 - fr[0] })
                        * (if by == 1 { fr[1] } else { 1.0 - fr[1] })
                        * (if bz == 1 { fr[2] } else { 1.0 - fr[2] });
                    s += get(i, j, k) * w;
                }
            }
        }
        out.push(s);
    }
    Ok((out, valid))
}



pub fn mask(
    context: &ArtifactContext,
    field_name: &str,
    points: &[[f64; 3]],
) -> Result<Vec<bool>, RenderError> {
    let d = context.fields[field_name].descriptor.as_object().cloned().unwrap_or_default();
    let raw = declared_mask(&d);
    let Some(name) = mask_name(field_name, raw)? else { return Ok(vec![true; points.len()]) };
    let spec = match raw {
        Some(Value::Object(o)) => o.clone(),
        _ => json!({"predicate": "greater_equal", "value": 0.5}).as_object().cloned().unwrap_or_default(),
    };
    if spec.keys().any(|k| !["field", "predicate", "value"].contains(&k.as_str())) {
        return Err(err("result-artifact mask has unknown fields"));
    }
    if spec.get("predicate").map_or("greater_equal", |p| p.as_str().unwrap_or("")) != "greater_equal" {
        return Err(err("result-artifact mask predicate is unsupported"));
    }
    let threshold = spec.get("value").map_or(Some(0.5), Value::as_f64).unwrap_or(f64::NAN);
    if !threshold.is_finite() {
        return Err(err("result-artifact mask threshold must be finite"));
    }
    let record =
        context.fields.get(&name).ok_or_else(|| err("result-artifact masks reference missing fields"))?;
    let (values, valid) = sample(record, points, true)?;
    Ok(values.iter().zip(valid).map(|(v, ok)| ok && *v >= threshold).collect())
}

fn truth(context: &ArtifactContext, field_name: &str) -> String {
    let d = &context.fields[field_name].descriptor;
    if let Some(v) = d.get("truth_status").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return v.to_string();
    }
    context.manifest["metadata"]["truth_status"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("stored_result_artifact")
        .to_string()
}

pub type NativeSamples = (Vec<f64>, [usize; 3], [[f64; 3]; 2], Value);



pub fn native_surface_samples(
    record: &FieldRecord,
    crop: [[f64; 3]; 2],
    quality: &str,
) -> Result<NativeSamples, RenderError> {
    let shape = record.registration.shape;
    let source_samples: usize = shape.iter().product();
    if shape.iter().any(|&n| n < 2) {
        return Err(err("native/analysis rendering requires at least two registered samples on every axis"));
    }
    if source_samples > render3d::NATIVE_RESULT_MAX_SOURCE_SAMPLES {
        return Err(err(format!(
            "native/analysis result field has {source_samples} registered samples, above the strict {}-sample source \
             bound; use a bounded sampled quality",
            render3d::NATIVE_RESULT_MAX_SOURCE_SAMPLES
        )));
    }
    let base = record.bbox_mm;
    let largest = base.iter().flatten().map(|v| v.abs()).fold(0.0f64, f64::max);
    let tolerance = 1e-10 * largest.max(1.0);
    if (0..2).any(|i| (0..3).any(|a| (crop[i][a] - base[i][a]).abs() > tolerance)) {
        return Err(err(
            "native/analysis rendering requires the complete registered field bounds; use clip for a cutaway or a \
             sampled quality for an arbitrary crop",
        ));
    }
    let reg = &record.registration;
    let spacing = reg.diag();
    let origin = reg.origin;
    let offset = if reg.centering == "node" { 0.0 } else { 0.5 };
    let exterior = record.descriptor.get("exterior_value").filter(|v| !v.is_null());
    let (samples, wshape, sbox, halo) = if let Some(e) = exterior {
        {
            let e = if e.is_boolean() { None } else { e.as_f64().filter(|x| x.is_finite()) };
            let Some(e) = e else {
                return Err(err("result-artifact exterior_value must be a finite number"));
            };
            let ws = [shape[0] + 2, shape[1] + 2, shape[2] + 2];
            let mut s = vec![e; ws[0] * ws[1] * ws[2]];
            for i in 0..shape[0] {
                for j in 0..shape[1] {
                    for k in 0..shape[2] {
                        s[((i + 1) * ws[1] + j + 1) * ws[2] + k + 1] =
                            record.values[(i * shape[1] + j) * shape[2] + k];
                    }
                }
            }
            let sbox = [
                std::array::from_fn(|a| origin[a] + (offset - 1.0) * spacing[a]),
                std::array::from_fn(|a| origin[a] + (shape[a] as f64 + offset) * spacing[a]),
            ];
            (s, ws, sbox, 1)
        }
    } else {
        {
            let sbox = [
                std::array::from_fn(|a| origin[a] + offset * spacing[a]),
                std::array::from_fn(|a| origin[a] + (shape[a] as f64 - 1.0 + offset) * spacing[a]),
            ];
            (record.values.clone(), shape, sbox, 0)
        }
    };
    let working = samples.len();
    if working > render3d::NATIVE_RESULT_MAX_WORKING_SAMPLES {
        return Err(err(format!(
            "native/analysis exterior closure requires {working} working samples, above the strict {}-sample render \
             bound",
            render3d::NATIVE_RESULT_MAX_WORKING_SAMPLES
        )));
    }
    let sampling = json!({
        "quality": quality, "effective_quality": "native",
        "sampling_mode": if reg.centering == "node" { "registered_native_node_lattice" } else { "registered_native_cell_lattice" },
        "registered_native_values_used": true, "source_values_resampled": false,
        "source_registration_id": reg.wire["registration_id"],
        "source_centering": reg.centering,
        "source_shape": shape, "source_samples": source_samples,
        "source_spacing_mm": spacing, "source_bbox_mm": base,
        "shape": wshape, "samples": working, "bbox_mm": sbox, "spacing_mm": spacing,
        "derived_exterior_halo_samples_per_side": halo, "derived_exterior_halo": halo == 1,
        "max_source_samples": render3d::NATIVE_RESULT_MAX_SOURCE_SAMPLES,
        "max_working_samples": render3d::NATIVE_RESULT_MAX_WORKING_SAMPLES,
        "max_triangles": render3d::NATIVE_RESULT_MAX_TRIANGLES,
    });
    Ok((samples, wshape, sbox, sampling))
}

pub type DimensionResolver<'a> =
    &'a dyn Fn(&Value, [[f64; 3]; 2]) -> Result<(usize, usize, Map<String, Value>), RenderError>;

fn inline_image(png: &[u8], width: usize, height: usize) -> Value {
    json!({"schema": "implexity-inline-image/1", "mime_type": "image/png",
           "data_base64": base64::engine::general_purpose::STANDARD.encode(png), "bytes": png.len(),
           "sha256": hex::encode(Sha256::digest(png)), "width_px": width, "height_px": height})
}

fn arr3(v: &Value) -> Option<[[f64; 3]; 2]> {
    let r = |i: usize| -> Option<[f64; 3]> {
        let a = v.get(i)?.as_array()?;
        Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?])
    };
    Some([r(0)?, r(1)?])
}




#[allow(clippy::too_many_lines)]
pub fn render_section(
    store: &dyn ResultArtifactStore,
    request: &Value,
    dimension_resolver: DimensionResolver<'_>,
) -> Result<Value, RenderError> {
    let field_name = request["field"].as_str().unwrap_or("").to_string();
    let artifact_id = request["source"]["artifact_id"].as_str().unwrap_or("");
    let scalarizations = request.get("scalarization").map(|s| {
        let mut m = Map::new();
        m.insert(field_name.clone(), s.clone());
        m
    });
    let context =
        load_fields(store, artifact_id, std::slice::from_ref(&field_name), scalarizations.as_ref())?;
    let record = &context.fields[&field_name];
    let descriptor = &record.descriptor;
    let bbox = arr3(&request["bbox_mm"]).unwrap_or(record.bbox_mm);
    let base = record.bbox_mm;
    let largest = base.iter().flatten().map(|v| v.abs()).fold(0.0f64, f64::max);
    let tolerance = 1e-10 * largest.max(1.0);
    if (0..3).any(|a| bbox[0][a] < base[0][a] - tolerance || bbox[1][a] > base[1][a] + tolerance) {
        return Err(err("render section bbox_mm must remain inside the field registration"));
    }
    let (width, height, mut framing) = dimension_resolver(request, bbox)?;
    let plane = request["plane"].as_str().unwrap_or("z");
    let axis = match plane {
        "x" => 0,
        "y" => 1,
        _ => 2,
    };
    let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
    let position = request["position"].as_f64().unwrap_or(0.5);
    let (lower, upper) = (bbox[0], bbox[1]);
    let at = lower[axis] + position * (upper[axis] - lower[axis]);
    let pixel = ((upper[u] - lower[u]) / width as f64).max((upper[v] - lower[v]) / height as f64);
    let (span_u, span_v) = (pixel * width as f64, pixel * height as f64);
    let (centre_u, centre_v) = (0.5 * (lower[u] + upper[u]), 0.5 * (lower[v] + upper[v]));
    let us = implexity_mesh::numeric::linspace(
        centre_u - 0.5 * span_u + 0.5 * pixel,
        centre_u + 0.5 * span_u - 0.5 * pixel,
        width,
    );
    let vs = implexity_mesh::numeric::linspace(
        centre_v - 0.5 * span_v + 0.5 * pixel,
        centre_v + 0.5 * span_v - 0.5 * pixel,
        height,
    );
    let mut points = Vec::with_capacity(width * height);
    for &uu in &us {
        for &vv in &vs {
            let mut p = [0.0; 3];
            p[axis] = at;
            p[u] = uu;
            p[v] = vv;
            points.push(p);
        }
    }
    let categorical = descriptor["suggested_palette"] == "categorical";
    let (values, valid) = sample(record, &points, categorical)?;
    let m = mask(&context, &field_name, &points)?;
    let outside: Vec<bool> = (0..points.len())
        .map(|i| {
            let (uu, vv) = (us[i / height], vs[i % height]);
            !valid[i] || !m[i] || uu < lower[u] || uu > upper[u] || vv < lower[v] || vv > upper[v]
        })
        .collect();
    let mut palette = request["palette"].as_str().unwrap_or("auto").to_string();
    if palette == "auto" {
        palette = descriptor["suggested_palette"].as_str().unwrap_or("sequential").to_string();
    }
    let finite: Vec<f64> = (0..values.len()).filter(|&i| valid[i] && m[i]).map(|i| values[i]).collect();
    if finite.is_empty() {
        return Err(err("render section misses the declared field mask"));
    }
    let display_range =
        arr2(&request["value_range"]).or_else(|| arr2(&descriptor["display_range"])).unwrap_or_else(|| {
            let lo = finite.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            [lo, hi]
        });
    let categorical_spec = if palette == "categorical" {
        Some(render3d::normalise_categorical_palette(descriptor.get("categorical"))?.to_json())
    } else {
        descriptor.get("categorical").cloned()
    };
    let background_name = request["background"].as_str().unwrap_or("dark");
    let background = Background::parse(background_name)?;
    let label = descriptor["label"].as_str().unwrap_or("").to_string();
    let opts = ScalarSectionOptions {
        outside: Some(&outside),
        palette: SectionPalette::from_name(&palette, categorical_spec.as_ref())?,
        value_range: Some(display_range),
        label: label.clone(),
        region_masks: Vec::new(),
        background,
        draw_legend: true,
    };
    let mut annotation = None;
    let rgb = if request.get("annotations").and_then(Value::as_bool).unwrap_or(false) {
        let ann = Annotation {
            units: descriptor["units"].as_str(),
            field_id: &field_name,
            plane,
            at_mm: at,
            bbox_mm: bbox,
            time_s: descriptor.get("time_s").and_then(Value::as_f64),
            sampled_bounds_mm: Some([
                [centre_u - 0.5 * span_u, centre_v - 0.5 * span_v],
                [centre_u + 0.5 * span_u, centre_v + 0.5 * span_v],
            ]),
            label: Some(&label),
        };
        let (img, meta) = raster::annotated_scalar_section_rgb(&values, width, height, &opts, &ann)?;
        annotation = Some(meta);
        img
    } else {
        raster::scalar_section_rgb(&values, width, height, &opts)?
    };
    let png = rgb.to_png();
    if png.len() > 8 * 1024 * 1024 {
        return Err(err("bounded artifact section renderer produced an invalid PNG"));
    }
    framing.insert(
        "bounds_source".into(),
        json!(if request["bbox_mm"].is_null() { "field_registration" } else { "request_bbox_mm" }),
    );
    framing.insert("pixel_size_mm".into(), json!(pixel));
    framing.insert("sampled_span_mm".into(), json!([span_u, span_v]));
    framing.insert("equal_physical_scale".into(), json!(true));
    let mut field = descriptor.as_object().cloned().unwrap_or_default();
    field.insert("name".into(), json!(field_name));
    field.insert("source".into(), json!("immutable_registered_result_artifact"));
    field.insert("palette".into(), json!(palette));
    field.insert("display_range".into(), json!(display_range));
    let masked = declared_mask(descriptor.as_object().unwrap_or(&Map::new())).is_some_and(truthy);
    field.insert("mask_applied".into(), json!(masked));
    let mut presentation = json!({"background": background_name,
        "background_rgb": if background_name == "white" { json!([255, 255, 255]) } else { json!([8, 9, 11]) }});
    if let Some(a) = annotation {
        presentation["annotations"] = a;
    }
    Ok(json!({
        "schema": "implexity-rendered-section/1", "kind": "render_section",
        "truth_status": truth(&context, &field_name), "source": context.source,
        "view": {"plane": plane, "position": position, "at_mm": at, "bbox_mm": bbox, "pixel_size_mm": pixel,
                 "width_px": width, "height_px": height, "framing": framing, "background": background_name},
        "field": field, "overlay_regions": [], "presentation": presentation,
        "render_status": "sampled_result_artifact_preview",
        "image": inline_image(&png, rgb.width, rgb.height),
    }))
}

fn arr2(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}



#[allow(clippy::too_many_lines)]
pub fn render_3d(store: &dyn ResultArtifactStore, request: &Render3dRequest) -> Result<Value, RenderError> {
    let surface_name = request.surface_field.clone();
    let mut names = vec![surface_name.clone()];
    if let Some(c) = &request.color_field {
        names.push(c.clone());
    }
    let artifact_id = request.source["artifact_id"].as_str().unwrap_or("");
    let context = load_fields(store, artifact_id, &names, None)?;
    let surface = &context.fields[&surface_name];
    let base = surface.bbox_mm;
    let crop = request.crop_mm.unwrap_or(base);
    let largest = base.iter().flatten().map(|v| v.abs()).fold(0.0f64, f64::max);
    let tolerance = 1e-10 * largest.max(1.0);
    if (0..3).any(|a| crop[0][a] < base[0][a] - tolerance || crop[1][a] > base[1][a] + tolerance) {
        return Err(err("render 3d crop_mm must remain inside the surface registration"));
    }
    let native = render3d::is_native_quality(&request.quality);
    let policy = render3d::quality_policy(&request.quality);
    let (values, vshape, extraction_bbox, mut sampling) = if native {
        let (s, shape, bbox, rec) = native_surface_samples(surface, crop, &request.quality)?;
        (s, shape, bbox, rec)
    } else {
        let reg = &surface.registration;
        let hint = render3d::GEOMETRY_QUALITIES.iter().any(|(q, _)| *q == request.quality).then(|| {
            let norms: Vec<f64> = (0..3)
                .map(|c| {
                    (reg.matrix[0][c] * reg.matrix[0][c]
                        + reg.matrix[1][c] * reg.matrix[1][c]
                        + reg.matrix[2][c] * reg.matrix[2][c])
                        .sqrt()
                })
                .collect();
            render3d::SpacingHint {
                spacing_mm: norms.iter().copied().fold(f64::INFINITY, f64::min),
                source: Some("result_field_registration".into()),
                declared_by: vec![surface_name.clone()],
            }
        });
        let grid = render3d::sampling_grid(crop, &request.quality, hint.as_ref())?;
        let (vals, valid) = sample(surface, &grid.points(), false)?;
        if !valid.iter().all(|&v| v) {
            return Err(err("render 3d crop reaches outside the surface registration"));
        }
        let mut rec = grid.record.clone();
        rec["source_registration_id"] = reg.wire["registration_id"].clone();
        rec["source_centering"] = json!(reg.centering);
        rec["source_shape"] = json!(reg.shape);
        rec["source_samples"] = json!(reg.shape.iter().product::<usize>());
        rec["source_spacing_mm"] = json!(reg.diag());
        rec["source_bbox_mm"] = json!(surface.bbox_mm);
        (vals, grid.shape, crop, rec)
    };
    let field = Field3 { shape: vshape, data: &values };
    let (mesh, mut mesh_record) =
        render3d::extract_isosurface(&field, extraction_bbox, request.iso_value, policy.max_triangles)?;
    let mut surface_record = surface.descriptor.as_object().cloned().unwrap_or_default();
    surface_record.insert("name".into(), json!(surface_name));
    surface_record.insert("source".into(), json!("immutable_registered_result_artifact"));
    surface_record.insert("iso_value".into(), json!(request.iso_value));
    surface_record.insert("sampled_value_range".into(), mesh_record["sampled_value_range"].clone());
    let centroids: Vec<[f64; 3]> = mesh
        .faces
        .iter()
        .map(|f| {
            std::array::from_fn(|a| {
                (mesh.vertices[f[0]][a] + mesh.vertices[f[1]][a] + mesh.vertices[f[2]][a]) / 3.0
            })
        })
        .collect();
    let mut keep = mask(&context, &surface_name, &centroids)?;
    let mut attributes: Option<Vec<f64>> = None;
    let mut colour_spec: Option<ColourSpec> = None;
    let mut color_record: Option<Map<String, Value>> = None;
    if let Some(color_name) = &request.color_field {
        let colour = &context.fields[color_name];
        if native
            && surface.registration.wire["registration_id"] != colour.registration.wire["registration_id"]
        {
            return Err(err(
                "native/analysis color fields must share the exact registered cell grid of the surface field",
            ));
        }
        let categorical = colour.descriptor["suggested_palette"] == "categorical"
            || colour.descriptor.get("categorical").is_some_and(Value::is_object);
        let (attrs, color_valid) = sample(colour, &mesh.vertices, categorical)?;
        let cmask = mask(&context, color_name, &centroids)?;
        for (i, f) in mesh.faces.iter().enumerate() {
            keep[i] = keep[i] && f.iter().all(|&v| color_valid[v]) && cmask[i];
        }
        if native {
            sampling["color_sampling"] = json!({
                "field": color_name,
                "source_registration_id": colour.registration.wire["registration_id"],
                "registration_matches_surface": true,
                "mode": if categorical { "nearest_registered_native_cell" } else { "trilinear_registered_native_cell" },
            });
        }
        attributes = Some(attrs);
    }
    let faces: Vec<[usize; 3]> = mesh.faces.iter().zip(&keep).filter(|(_, k)| **k).map(|(f, _)| *f).collect();
    if faces.is_empty() {
        return Err(err("declared result-field mask removes the complete surface"));
    }
    let mesh = RenderMesh { faces, ..mesh };
    if let Some(color_name) = &request.color_field {
        let descriptor = &context.fields[color_name].descriptor;
        let (spec, specification) = render3d::resolve_colour_mapping(request, descriptor)?;
        let (_c, mapping) = render3d::colour_vertices(attributes.as_deref().unwrap_or(&[]), &spec)?;
        let mut rec = descriptor.as_object().cloned().unwrap_or_default();
        rec.insert("name".into(), json!(color_name));
        rec.insert("source".into(), json!("immutable_registered_result_artifact"));
        rec.insert("color_specification".into(), json!(specification));
        for (k, v) in mapping.as_object().into_iter().flatten() {
            rec.insert(k.clone(), v.clone());
        }
        let masked = declared_mask(descriptor.as_object().unwrap_or(&Map::new())).is_some_and(truthy);
        rec.insert("mask_applied".into(), json!(masked));

        let display = arr2(&rec["display_range"]);
        colour_spec = Some(match spec {
            ColourSpec::Ramp { palette, .. } => ColourSpec::Ramp { palette, value_range: display },
            ColourSpec::Categorical(p, _) => ColourSpec::Categorical(p, display),
            s @ ColourSpec::Stops(_) => s,
        });
        color_record = Some(rec);
    }
    let (mesh, attributes) =
        render3d::clip_mesh(&mesh, attributes.as_deref(), request.clip.as_ref(), policy.max_triangles)?;
    let vertex_colours = match (&colour_spec, &attributes) {
        (Some(spec), Some(a)) => Some(render3d::colour_vertices(a, spec)?.0),
        _ => None,
    };
    let cap = if let Some((clip, cap_req)) =
        request.clip.as_ref().and_then(|c| c.cap.as_ref().map(|cap| (c, cap)))
    {
        {
            let inside =
                if cap_req.inside == "auto" { "above_iso".to_string() } else { cap_req.inside.clone() };
            let solid: CapColour<'_> =
                if let (Some(color_name), Some(spec)) = (&request.color_field, &colour_spec) {
                    let colour_field = &context.fields[color_name];
                    let categorical = colour_field.descriptor["suggested_palette"] == "categorical"
                        || colour_field.descriptor.get("categorical").is_some_and(Value::is_object);
                    let spec = spec.clone();
                    Box::new(move |pts: &[[f64; 3]]| {
                        let (vals, valid) = sample(colour_field, pts, categorical)?;
                        let (c, _) = render3d::colour_vertices(&vals, &spec)?;
                        Ok((c, valid))
                    })
                } else {
                    let rgb = request.surface_color_rgb.unwrap_or(render3d::BASE_COLOUR);
                    Box::new(move |pts: &[[f64; 3]]| Ok((vec![rgb; pts.len()], vec![true; pts.len()])))
                };
            let mut cap = render3d::grid_section_cap(
                &field,
                extraction_bbox,
                request.iso_value,
                &inside,
                extraction_bbox,
                clip,
                solid,
                cap_req.complement_color_rgb,
            )?;
            cap.record["inside_rule_source"] =
                json!(if cap_req.inside == "auto" { "registered_field_values_above_iso" } else { "request" });
            cap.record["complement_bounds_source"] = json!("render_crop");
            let mut mask_fields = vec![surface_name.clone()];
            if let Some(c) = &request.color_field {
                mask_fields.push(c.clone());
            }
            let classify = cap.classify;
            let ctx = &context;
            cap.classify = Box::new(move |pts: &[[f64; 3]]| {
                let mut states = classify(pts);
                for name in &mask_fields {
                    if let Ok(m) = mask(ctx, name, pts) {
                        for (s, k) in states.iter_mut().zip(m) {
                            if !k {
                                *s = render3d::CapState::None;
                            }
                        }
                    }
                }
                states
            });
            cap.record["declared_masks_applied_to_cap"] = json!(true);
            Some(cap)
        }
    } else {
        None
    };
    let (camera_bounds, bounds_source) = if matches!(request.camera, render3d::CameraRequest::Preset { .. }) {
        let mut used: Vec<usize> = mesh.faces.iter().flatten().copied().collect();
        used.sort_unstable();
        used.dedup();
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &i in &used {
            for a in 0..3 {
                lo[a] = lo[a].min(mesh.vertices[i][a]);
                hi[a] = hi[a].max(mesh.vertices[i][a]);
            }
        }
        let span: [f64; 3] = std::array::from_fn(|a| (hi[a] - lo[a]).max(1e-9));
        (
            [
                std::array::from_fn(|a| lo[a] - 0.04 * span[a]),
                std::array::from_fn(|a| hi[a] + 0.04 * span[a]),
            ],
            "masked_surface_bounds",
        )
    } else {
        (crop, "masked_surface_bounds")
    };
    let mut camera =
        render3d::resolve_camera(&request.camera, camera_bounds, request.width_px, request.height_px)?;
    camera.record["framing_bounds_mm"] = json!(camera_bounds);
    camera.record["framing_bounds_source"] = json!(bounds_source);
    let palette_cat = match (&colour_spec, &color_record) {
        (Some(ColourSpec::Categorical(p, _)), Some(r)) if r["palette"] == "categorical" => Some(p.clone()),
        _ => None,
    };
    let colouring = match (&palette_cat, &vertex_colours, &attributes) {
        (Some(p), _, Some(a)) => VertexColouring::Categorical { values: a, palette: p },
        (None, Some(c), _) => VertexColouring::Colours(c),
        _ => VertexColouring::Uniform,
    };
    let opts = RasterOptions {
        width: request.width_px,
        height: request.height_px,
        camera: &camera,
        colouring,
        background: &request.background,
        supersample: request.antialias,
        base_colour: request.surface_color_rgb,
        cap: cap.as_ref(),
    };
    let (rgb, raster_record) = render3d::rasterize(&mesh, &opts)?;
    if let Some(c) = request.surface_color_rgb {
        surface_record.insert("display_color_rgb".into(), json!(c));
    }
    let png = rgb.to_png();
    if png.len() > render3d::MAX_PNG_BYTES {
        return Err(err("bounded artifact renderer produced an invalid PNG"));
    }
    mesh_record["vertices_after_clip"] = json!(mesh.vertices.len());
    mesh_record["triangles_after_clip"] = json!(mesh.faces.len());
    mesh_record["clip"] = request.clip.as_ref().map_or(Value::Null, render3d::ClipPlane::to_json);
    mesh_record["mask_filter_geometry_moved"] = json!(false);
    mesh_record["max_triangles"] = json!(policy.max_triangles);
    sampling["bounds_source"] =
        json!(if request.crop_mm.is_some() { "request_crop_mm" } else { "field_registration" });
    sampling["base_bounds_authoritative"] = json!(true);
    let bg_rgb = if request.background == "white" { json!([255, 255, 255]) } else { json!([17, 25, 38]) };
    Ok(json!({
        "schema": "implexity-rendered-3d/1", "kind": "render_3d",
        "truth_status": truth(&context, &surface_name),
        "render_status": if native { "native_registered_result_artifact_render_only" }
                         else { "sampled_result_artifact_isosurface_preview" },
        "representation_truth": {
            "source_identity_bound": true, "registered_native_grid_used": native,
            "native_values_preserved_without_resampling": native,
            "surface_is_sampled_approximation": !native, "surface_is_exact_geometry": false,
            "surface_representation": if native { "native_lattice_piecewise_linear_isosurface" }
                                      else { "bounded_resampled_piecewise_linear_isosurface" },
            "render_only": true, "authoritative_for_acceptance_or_export": false,
        },
        "source": context.source, "surface": surface_record, "color": color_record,
        "view": {"camera": camera.record, "crop_mm": crop, "width_px": request.width_px,
                 "height_px": request.height_px, "antialias": request.antialias, "background": request.background},
        "sampling": sampling, "mesh": mesh_record,
        "presentation_smoothing": {"mode": "none", "geometry_moved": false, "normal_interpolation_only": true},
        "raster": raster_record,
        "presentation": {"background": request.background, "background_rgb": bg_rgb},
        "image": inline_image(&png, request.width_px, request.height_px),
    }))
}
