// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use implexity_geometry::document::model::Model;
use implexity_geometry::node::{Node, NodeRef, ParamRef};
use implexity_mesh::model_view::{Centering, RegisteredGrid, SamplingHint};
use serde_json::{Map, Value, json};

use crate::RenderError;
use crate::viewer_scene::DerivedFieldSource;

fn err(m: impl Into<String>) -> RenderError {
    RenderError::Invalid(m.into())
}

fn geo(e: impl std::fmt::Display) -> RenderError {
    RenderError::Invalid(e.to_string())
}


pub fn geometry_sampling_hint(root: &NodeRef) -> Result<Option<SamplingHint>, RenderError> {
    fn finest(
        node: &NodeRef,
        memo: &mut HashMap<usize, Option<f64>>,
        declared: &mut Vec<String>,
    ) -> Result<Option<f64>, RenderError> {
        let key = Arc::as_ptr(node) as usize;
        if let Some(v) = memo.get(&key) {
            return Ok(*v);
        }
        let mut candidates = Vec::new();
        if let Some(value) = node.op().render_sampling_spacing_mm(node) {
            if !value.is_finite() || value <= 0.0 {
                return Err(err("render_sampling_spacing_mm must be a positive finite length"));
            }
            candidates.push(value);
            let kind = node.kind().to_string();
            if !declared.contains(&kind) {
                declared.push(kind);
            }
        }
        let mut children = Vec::new();
        for child in node.children() {
            if let Some(v) = finest(child, memo, declared)? {
                children.push(v);
            }
        }
        if !children.is_empty() {
            let scale = node.op().render_sampling_scale(node).unwrap_or(1.0);
            if !scale.is_finite() || scale <= 0.0 {
                return Err(err("render_sampling_scale must be a positive finite factor"));
            }
            candidates.push(children.iter().copied().fold(f64::INFINITY, f64::min) * scale);
        }
        let result =
            (!candidates.is_empty()).then(|| candidates.iter().copied().fold(f64::INFINITY, f64::min));
        memo.insert(key, result);
        Ok(result)
    }
    let mut declared = Vec::new();
    let spacing = finest(root, &mut HashMap::new(), &mut declared)?;
    declared.sort();
    Ok(spacing.map(|spacing_mm| SamplingHint {
        spacing_mm,
        declared_by: declared,
        frame: "model_root_mm".into(),
    }))
}

#[derive(Clone, Debug)]
pub struct FieldSpec {
    pub parameter: String,
    pub derived: bool,
    pub label: String,
    pub description: String,
    pub units: String,
    pub origin: [f64; 3],
    pub spacing: [f64; 3],
    pub centering: String,
    pub suggested_palette: String,
    pub categorical: Option<Value>,
    pub shape: [usize; 3],
    pub values: Vec<f64>,
    pub parameter_transform: Value,
    pub parameter_transform_applied: bool,
    pub native_analysis_grid: bool,
    pub exterior_value: Option<f64>,
}

fn apply_transform(
    node: &Node,
    values: &[f64],
    transform: Option<&Value>,
) -> Result<(Vec<f64>, Value, bool), RenderError> {
    let identity = json!({"kind": "identity"});
    let t = transform.filter(|t| !t.is_null()).unwrap_or(&identity);
    let Some(obj) = t.as_object() else {
        return Err(err("declared render-field transform must be an object"));
    };
    let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("identity");
    let kind = if obj.contains_key("kind") && obj["kind"].as_str().is_none() { "\u{0}" } else { kind };
    match kind {
        "identity" => {
            if obj.keys().any(|k| k != "kind") {
                return Err(err("identity render-field transform has unknown fields"));
            }
            Ok((values.to_vec(), json!({"kind": "identity"}), false))
        }
        "sigmoid" => {
            if obj.keys().any(|k| k != "kind" && k != "scale" && k != "scale_parameter") {
                return Err(err("sigmoid render-field transform has unknown fields"));
            }
            if obj.contains_key("scale") && obj.contains_key("scale_parameter") {
                return Err(err("sigmoid render-field transform chooses scale or scale_parameter"));
            }
            let (scale, record) = if let Some(p) = obj.get("scale_parameter") {
                let name = p.as_str().filter(|n| node.info().param(n).is_some());
                let Some(name) = name else {
                    return Err(err("sigmoid render-field scale_parameter is not a node parameter"));
                };
                let (shape, data) = node
                    .param(name)
                    .ok_or_else(|| err("sigmoid render-field scale_parameter is not a node parameter"))?
                    .to_f64_array()
                    .map_err(|_| err("sigmoid render-field scale_parameter must be scalar"))?;
                if !shape.is_empty() || data.len() != 1 {
                    return Err(err("sigmoid render-field scale_parameter must be scalar"));
                }
                (data[0], json!({"kind": "sigmoid", "scale": data[0], "scale_parameter": name}))
            } else {
                let s = match obj.get("scale") {
                    None => 1.0,
                    Some(v) => py_float(v)?,
                };
                (s, json!({"kind": "sigmoid", "scale": s}))
            };
            if !scale.is_finite() {
                return Err(err("sigmoid render-field scale must be finite"));
            }
            Ok((values.iter().map(|&x| 0.5 * ((0.5 * scale * x).tanh() + 1.0)).collect(), record, true))
        }
        _ => Err(err("declared render-field transform kind is not supported")),
    }
}

fn py_float(v: &Value) -> Result<f64, RenderError> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.trim().parse().map_err(|_| {
            err(format!("could not convert string to float: {}", implexity_core::py_repr::repr_str(s)))
        }),
        _ => Err(err("sigmoid render-field scale must be a number")),
    }
}

type DerivedKey = (String, Vec<String>, Vec<String>);
type Grids = BTreeMap<String, ([usize; 3], Vec<f64>)>;
type DerivedCache = Mutex<Vec<(DerivedKey, Arc<Grids>)>>;

fn derived_cache() -> &'static DerivedCache {
    static CACHE: OnceLock<DerivedCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

const DERIVED_CACHE_ENTRIES: usize = 8;

fn derived_grids(
    node: &Node,
    identity: &(String, Vec<String>),
    names: &[String],
) -> Result<Arc<Grids>, RenderError> {
    let mut sorted = names.to_vec();
    sorted.sort();
    let key: DerivedKey = (identity.0.clone(), identity.1.clone(), sorted.clone());
    {
        let mut c = derived_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pos) = c.iter().position(|(k, _)| *k == key) {
            let entry = c.remove(pos);
            let grids = Arc::clone(&entry.1);
            c.push(entry);
            return Ok(grids);
        }
    }
    let Some(raw) = node.op().render_derived_grids(node, &sorted) else {
        return Err(err("a node declaring derived render fields must implement render_derived_grids"));
    };
    let raw = raw.map_err(geo)?;
    let mut got: Vec<&str> = raw.iter().map(|(n, _, _)| n.as_str()).collect();
    got.sort_unstable();
    got.dedup();
    let mut want: Vec<&str> = names.iter().map(String::as_str).collect();
    want.sort_unstable();
    want.dedup();
    if got != want || raw.len() != got.len() {
        return Err(err("render_derived_grids must return exactly the declared derived fields"));
    }
    let mut grids = BTreeMap::new();
    for (name, shape, values) in raw {
        if values.len() != shape.iter().product::<usize>() || values.iter().any(|v| !v.is_finite()) {
            return Err(err(format!(
                "derived render field {} must be a finite three-dimensional grid",
                implexity_core::py_repr::repr_str(&name)
            )));
        }
        grids.insert(name, (shape, values));
    }
    let grids = Arc::new(grids);
    let mut c = derived_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    c.push((key, Arc::clone(&grids)));
    while c.len() > DERIVED_CACHE_ENTRIES {
        c.remove(0);
    }
    Ok(grids)
}

fn derived_id_ok(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 80
        && b[0].is_ascii_alphabetic()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

fn vec3(v: Option<&Value>) -> Option<[f64; 3]> {
    let a = v?.as_array()?;
    if a.len() != 3 {
        return None;
    }
    let mut out = [0.0; 3];
    for (o, x) in out.iter_mut().zip(a) {
        *o = match x {
            Value::Number(n) => n.as_f64()?,
            Value::Bool(b) => f64::from(u8::from(*b)),
            _ => return None,
        };
    }
    Some(out)
}

fn array3(node: &Node, name: &str) -> Option<([usize; 3], Vec<f64>)> {
    let pv = node.param(name)?;
    let (shape, data) = pv.to_f64_array().ok()?;
    (shape.len() == 3).then(|| ([shape[0], shape[1], shape[2]], data))
}

const ALLOWED_DESCRIPTOR_KEYS: [&str; 13] = [
    "parameter",
    "derived",
    "label",
    "description",
    "units",
    "origin_mm",
    "spacing_mm",
    "centering",
    "transform",
    "suggested_palette",
    "categorical",
    "native_analysis_grid",
    "exterior_value",
];


#[allow(clippy::too_many_lines)]
pub fn declared_specs(node: &Node, identity: &(String, Vec<String>)) -> Result<Vec<FieldSpec>, RenderError> {
    let Some(raw) = node.op().render_field_descriptors(node) else { return Ok(Vec::new()) };
    let raw = raw.map_err(geo)?;

    let mut derived_names = Vec::new();
    for (index, item) in raw.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            return Err(err(format!("declared render field {index} must be an object")));
        };
        let mut unknown: Vec<&String> =
            obj.keys().filter(|k| !ALLOWED_DESCRIPTOR_KEYS.contains(&k.as_str())).collect();
        if !unknown.is_empty() {
            unknown.sort();
            return Err(err(format!(
                "declared render field {index} has unknown fields {}",
                implexity_core::pyobj::list_repr(&unknown)
            )));
        }
        if obj.contains_key("parameter") == obj.contains_key("derived") {
            return Err(err(format!(
                "declared render field {index} names exactly one of parameter or derived"
            )));
        }
        if let Some(d) = obj.get("derived") {
            let name = d.as_str().filter(|n| derived_id_ok(n) && node.info().param(n).is_none());
            let Some(name) = name else {
                return Err(err(format!(
                    "declared derived render field {index} needs a bounded id that is not a node parameter"
                )));
            };
            derived_names.push(name.to_string());
        }
    }
    let grids = if derived_names.is_empty() {
        Arc::new(BTreeMap::new())
    } else {
        derived_grids(node, identity, &derived_names)?
    };
    let mut out: Vec<FieldSpec> = Vec::new();
    for (index, item) in raw.iter().enumerate() {
        let obj = item.as_object().ok_or_else(|| err("declared render field must be an object"))?;
        let derived = obj.contains_key("derived");
        let (parameter, shape, values) = if derived {
            let name = obj["derived"].as_str().unwrap_or_default().to_string();
            let (shape, values) = grids
                .get(&name)
                .cloned()
                .ok_or_else(|| err("render_derived_grids must return exactly the declared derived fields"))?;
            (name, shape, values)
        } else {
            let name =
                obj.get("parameter").and_then(Value::as_str).filter(|n| node.info().param(n).is_some());
            let Some(name) = name else {
                return Err(err(format!("declared render field {index} names no node parameter")));
            };
            let Some((shape, values)) = array3(node, name).filter(|(_, v)| v.iter().all(|x| x.is_finite()))
            else {
                return Err(err(format!(
                    "declared render field {} must be a finite numeric 3d array",
                    implexity_core::py_repr::repr_str(name)
                )));
            };
            (name.to_string(), shape, values)
        };
        let (Some(origin), Some(spacing)) = (vec3(obj.get("origin_mm")), vec3(obj.get("spacing_mm"))) else {
            return Err(err(format!(
                "declared render field {} has invalid physical registration",
                implexity_core::py_repr::repr_str(&parameter)
            )));
        };
        if origin.iter().chain(spacing.iter()).any(|v| !v.is_finite()) || spacing.iter().any(|&s| s <= 0.0) {
            return Err(err(format!(
                "declared render field {} has invalid physical registration",
                implexity_core::py_repr::repr_str(&parameter)
            )));
        }
        let centering = obj.get("centering").map_or(Some("node"), Value::as_str);
        let Some(centering) = centering.filter(|c| *c == "node" || *c == "cell") else {
            return Err(err("declared render field centering must be node or cell"));
        };
        let native = match obj.get("native_analysis_grid") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(err("declared render field native_analysis_grid must be boolean")),
        };
        let exterior = match obj.get("exterior_value") {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) if n.as_f64().is_some_and(f64::is_finite) => n.as_f64(),
            Some(_) => return Err(err("declared render field exterior_value must be a finite number")),
        };
        let palette = obj.get("suggested_palette").map_or(Some("sequential"), Value::as_str);
        let Some(palette) = palette.filter(|p| ["sequential", "diverging", "categorical"].contains(p)) else {
            return Err(err("declared render field has an unknown palette"));
        };
        let (transformed, transform, applied) = apply_transform(node, &values, obj.get("transform"))?;
        let categorical = obj.get("categorical").filter(|c| !c.is_null());
        let categorical = if palette == "categorical" {
            Some(match categorical {
                None => crate::render3d::categorical_palette_from_values(&transformed, 256)?.to_json(),
                Some(c) => crate::render3d::normalise_categorical_palette(Some(c))?.to_json(),
            })
        } else if categorical.is_some() {
            return Err(err("categorical metadata requires the categorical palette"));
        } else {
            None
        };
        let text = |k: &str, default: &str| {
            obj.get(k)
                .filter(|v| implexity_core::pyobj::truthy(v))
                .map_or_else(|| default.to_string(), implexity_core::pyobj::py_str)
        };
        out.push(FieldSpec {
            label: text("label", &parameter),
            parameter,
            derived,
            description: text("description", ""),
            units: text("units", "provider_native"),
            origin,
            spacing,
            centering: centering.to_string(),
            suggested_palette: palette.to_string(),
            categorical,
            shape,
            values: transformed,
            parameter_transform: transform,
            parameter_transform_applied: applied,
            native_analysis_grid: native,
            exterior_value: exterior,
        });
    }
    let mut names: Vec<&str> = out.iter().map(|s| s.parameter.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    if names.len() != out.len() {
        return Err(err("declared render fields contain duplicate parameters"));
    }
    Ok(out)
}

fn min_max(v: &[f64]) -> (f64, f64) {
    (v.iter().copied().fold(f64::INFINITY, f64::min), v.iter().copied().fold(f64::NEG_INFINITY, f64::max))
}

fn samples_spec(node: &Node, id: &str, visual: &Map<String, Value>) -> Option<FieldSpec> {
    node.info().param("samples")?;
    let (shape, values) = array3(node, "samples")?;
    if values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let origin = node
        .param("origin")
        .and_then(|p| p.to_f64_array().ok())
        .map_or(Some([0.0; 3]), |(_, d)| (d.len() == 3).then(|| [d[0], d[1], d[2]]))?;
    let spacing = node
        .param("spacing")
        .and_then(|p| p.to_f64_array().ok())
        .map_or(Some([1.0; 3]), |(_, d)| (d.len() == 3).then(|| [d[0], d[1], d[2]]))?;
    if origin.iter().any(|v| !v.is_finite())
        || spacing.iter().any(|s| s.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
    {
        return None;
    }
    let (lo, hi) = min_max(&values);
    let palette = match visual.get("palette").and_then(Value::as_str) {
        Some(p @ ("sequential" | "diverging")) => p.to_string(),
        _ => if lo < 0.0 && 0.0 < hi { "diverging" } else { "sequential" }.to_string(),
    };
    let text = |k: &str, default: &str| {
        visual
            .get(k)
            .filter(|v| implexity_core::pyobj::truthy(v))
            .map_or_else(|| default.to_string(), implexity_core::pyobj::py_str)
    };
    Some(FieldSpec {
        parameter: "samples".into(),
        derived: false,
        label: text("label", id),
        description: String::new(),
        units: text("units", "provider_native"),
        origin,
        spacing,
        centering: if node.kind() == "cell_grid_field" { "cell" } else { "node" }.into(),
        suggested_palette: palette,
        categorical: None,
        shape,
        values,
        parameter_transform: json!({"kind": "identity"}),
        parameter_transform_applied: false,
        native_analysis_grid: false,
        exterior_value: None,
    })
}

fn upper(spec_origin: [f64; 3], spacing: [f64; 3], shape: [usize; 3], cell: bool) -> [f64; 3] {
    #[allow(clippy::cast_precision_loss)]
    std::array::from_fn(|a| {
        spec_origin[a] + spacing[a] * if cell { shape[a] as f64 } else { shape[a].saturating_sub(1) as f64 }
    })
}


pub fn registered_fields(model: &Model) -> Result<Vec<Value>, RenderError> {
    let root = model.root().ok_or_else(|| err("the model has no root"))?;
    let content_id = model.content_id().unwrap_or_default();
    let mut rows = Vec::new();
    for (path, node) in root.walk() {
        let id = model.id_of(&node).unwrap_or_default();
        let visual = model
            .doc
            .get("nodes")
            .and_then(|n| n.get(&id))
            .and_then(|d| d.get("attrs"))
            .and_then(|a| a.get("visualization"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut specs = Vec::new();
        if let Some(s) = samples_spec(&node, &id, &visual) {
            specs.push(s);
        }
        specs.extend(declared_specs(&node, &(content_id.clone(), path.clone()))?);
        for spec in specs {
            let cell = spec.centering == "cell";
            let hi = upper(spec.origin, spec.spacing, spec.shape, cell);
            let mut full = vec!["model".to_string()];
            full.extend(path.iter().cloned());
            let field = ParamRef::new(full, spec.parameter.clone()).as_str();
            let label = if spec.label.is_empty() {
                if id.is_empty() { field.clone() } else { id.clone() }
            } else {
                spec.label.clone()
            };
            let (lo_v, hi_v) = min_max(&spec.values);
            let mut row = json!({
                "field": field, "node": id, "kind": node.kind(), "label": label,
                "description": spec.description, "units": spec.units, "shape": spec.shape,
                "registration": {"origin_mm": spec.origin, "spacing_mm": spec.spacing,
                                 "bbox_mm": [spec.origin, hi], "centering": spec.centering},
                "value_range": [lo_v, hi_v], "suggested_palette": spec.suggested_palette,
                "parameter_transform": spec.parameter_transform,
                "parameter_transform_applied": spec.parameter_transform_applied,
                "native_analysis_grid": spec.native_analysis_grid,
                "source": if spec.derived { "node_declared_derived_grid" } else { "node_parameter" },
                "read_only": spec.derived,
            });
            if let Some(e) = spec.exterior_value {
                row["exterior_value"] = json!(e);
            }
            if let Some(c) = spec.categorical {
                row["display_projection"] = c["projection"].clone();
                row["categorical"] = c;
            }
            rows.push(row);
        }
    }
    Ok(rows)
}


pub fn resolve_registered_field(model: &Model, reference: &str) -> Result<RegisteredGrid, RenderError> {
    let parsed = ParamRef::parse(reference).map_err(|_| err("invalid model field reference"))?;
    if parsed.path.first().map(String::as_str) != Some("model") {
        return Err(err("field reference must start at model"));
    }
    let path: Vec<String> = parsed.path[1..].to_vec();
    let root = model.root().ok_or_else(|| err("the model has no root"))?;
    let node = root.at(&path).map_err(geo)?;
    let content_id = model.content_id().unwrap_or_default();
    let declared = declared_specs(&node, &(content_id.clone(), path.clone()))?;
    let spec = declared.into_iter().find(|s| s.parameter == parsed.name);
    if spec.is_none() && node.info().param(&parsed.name).is_none() {
        return Err(err("field reference names no model parameter or declared derived field"));
    }
    let (shape, values, origin, spacing, centering) = if let Some(s) = spec {
        (s.shape, s.values, s.origin, s.spacing, s.centering)
    } else {
        let Some((shape, values)) = array3(&node, &parsed.name) else {
            return Err(err("renderable model field must be a finite three-dimensional array"));
        };
        let o =
            node.param("origin").and_then(|p| p.to_f64_array().ok()).map_or_else(|| vec![0.0; 3], |(_, d)| d);
        let h = node
            .param("spacing")
            .and_then(|p| p.to_f64_array().ok())
            .map_or_else(|| vec![1.0; 3], |(_, d)| d);
        if o.len() != 3 || h.len() != 3 || h.iter().any(|&x| x <= 0.0) {
            return Err(err("renderable model field has invalid registration"));
        }
        let centering = if node.kind() == "cell_grid_field" { "cell" } else { "node" };
        (shape, values, [o[0], o[1], o[2]], [h[0], h[1], h[2]], centering.to_string())
    };
    if values.iter().any(|v| !v.is_finite()) {
        return Err(err("renderable model field must be a finite three-dimensional array"));
    }
    if spacing.iter().any(|&x| x <= 0.0) {
        return Err(err("renderable model field has invalid registration"));
    }
    let cell = centering == "cell";
    let hi = upper(origin, spacing, shape, cell);
    let record = json!({
        "content_id": content_id, "node": model.id_of(&node), "kind": node.kind(),
        "registration": {"origin_mm": origin, "spacing_mm": spacing, "bbox_mm": [origin, hi],
                         "centering": if cell { "cell" } else { "node" }},
    });
    Ok(RegisteredGrid {
        values: implexity_mesh::Grid3 { shape, data: values },
        origin,
        spacing,
        centering: if cell { Centering::Cell } else { Centering::Node },
        record,
    })
}

pub struct RootDerived(pub NodeRef);

impl DerivedFieldSource for RootDerived {
    fn derived_field_specs(&self) -> Result<Vec<Value>, RenderError> {
        Ok(self
            .0
            .op()
            .occupancy_source()
            .map(implexity_geometry::occupancy::OccupancySource::derived_field_specs)
            .unwrap_or_default())
    }
    fn derived_field_values(&self, id: &str, points: &[[f64; 3]]) -> Result<Vec<f64>, RenderError> {
        let source = self
            .0
            .op()
            .occupancy_source()
            .ok_or_else(|| err("color field is not declared by the authoritative root"))?;
        let mut all = source.render_derived_fields(&self.0, points).map_err(geo)?;
        all.remove(id).ok_or_else(|| err("color field is not declared by the authoritative root"))
    }
}
