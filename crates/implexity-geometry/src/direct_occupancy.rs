// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use serde_json::{Map, Value, json};

use crate::error::{GResult, GeometryError};
use crate::field_registration::GridRegistration;
use crate::node::NodeRef;
use crate::sampled::Sampled;
use crate::scalar::Scalar;

const MARKER_KEYS: [&str; 4] =
    ["topologyAlwaysFree", "topology_always_free", "direct_occupancy", "directOccupancy"];
const CONTROL_NAMES: [&str; 4] = ["model:control", "topology:control", "topology_control", "control"];
const ARRAY_KEYS: [&str; 6] = ["values", "density", "rho", "occupancy", "control", "data"];

fn verr(m: impl Into<String>) -> GeometryError {
    GeometryError::Value(m.into())
}

#[derive(Clone, Debug, PartialEq)]
pub struct DirectOccupancyMetadata {
    pub source_shape: Vec<usize>,
    pub analysis_shape: Vec<usize>,
    pub representation: String,
    pub registration: Option<Value>,
    pub source: String,
    pub parameter: String,
}

impl DirectOccupancyMetadata {
    #[must_use]
    pub fn to_wire(&self) -> Value {
        json!({
            "schema": "implexity-direct-occupancy/1",
            "shape": self.source_shape, "source_shape": self.source_shape, "analysis_shape": self.analysis_shape,
            "resampled": self.source_shape != self.analysis_shape, "representation": self.representation,
            "registration": self.registration, "source": self.source, "parameter": self.parameter,
        })
    }

    #[must_use]
    pub fn representation_record(&self) -> DirectOccupancyRepresentation {
        DirectOccupancyRepresentation { source: self.source.clone(), parameter: self.parameter.clone() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectOccupancyRepresentation {
    pub source: String,
    pub parameter: String,
}

impl Default for DirectOccupancyRepresentation {
    fn default() -> Self {
        Self { source: "model:control".into(), parameter: "model:control".into() }
    }
}

impl DirectOccupancyRepresentation {
    #[must_use]
    pub fn to_wire(&self) -> Value {
        json!({
            "schema": "implexity-geometry-representation/1", "kind": "direct_occupancy",
            "field_semantics": "material_fraction", "units": "1", "value_domain": [0.0, 1.0], "field_class": null,
            "signed_distance_bound": false, "distance_conversion": "not_applicable", "safe_step_factor": null,
            "source": self.source, "parameter": self.parameter,
            "note": "Consumed directly by the physics occupancy bridge; no field-to-distance conversion is applied.",
        })
    }
}

impl std::fmt::Display for DirectOccupancyRepresentation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DIRECT_OCCUPANCY(dimensionless material fraction)")
    }
}

#[must_use]
pub fn walk_json(root: &Value, limit: usize) -> Vec<&Value> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(cur) = stack.pop() {
        out.push(cur);
        if out.len() >= limit {
            break;
        }
        match cur {
            Value::Object(m) => stack.extend(m.values()),
            Value::Array(a) => stack.extend(a.iter()),
            _ => {}
        }
    }
    out
}

fn lower_str(v: Option<&Value>) -> String {
    v.map(crate::document::py_str).unwrap_or_default().to_lowercase()
}

fn marker_in_map(m: &Map<String, Value>, found_true: &mut bool) -> Option<bool> {
    for key in MARKER_KEYS {
        match m.get(key) {
            Some(Value::Bool(false)) => return Some(false),
            Some(Value::Bool(true)) => *found_true = true,
            _ => {}
        }
    }
    let semantics = lower_str(m.get("field_semantics").or_else(|| m.get("semantics")));
    let role = lower_str(m.get("design_role").or_else(|| m.get("role")));
    let schema = lower_str(m.get("schema"));
    let kind = lower_str(m.get("kind").or_else(|| m.get("type")));
    if ["occupancy", "density", "material_fraction"].contains(&semantics.as_str())
        && (role.contains("topolog")
            || role.contains("design")
            || schema.contains("topolog")
            || kind.contains("topolog"))
    {
        *found_true = true;
    }
    if ["universal-topology", "direct-occupancy", "topology-space", "topology-control"]
        .iter()
        .any(|t| schema.contains(t) || kind.contains(t))
    {
        *found_true = true;
    }
    None
}

#[must_use]
pub fn marker_value_json(root: &Value) -> Option<bool> {
    let mut found = false;
    for v in walk_json(root, 20_000) {
        if let Value::Object(m) = v
            && let Some(f) = marker_in_map(m, &mut found)
        {
            return Some(f);
        }
    }
    found.then_some(true)
}

fn array_store(root: &Value) -> Option<&Map<String, Value>> {
    for v in walk_json(root, 20_000) {
        if let Value::Object(m) = v {
            for key in ["arrays", "array_store", "arrayStore", "blobs"] {
                if let Some(Value::Object(s)) = m.get(key) {
                    return Some(s);
                }
            }
        }
    }
    None
}

fn resolve_ref<'a>(value: &'a Value, store: Option<&'a Map<String, Value>>) -> &'a Value {
    if let (Value::String(s), Some(st)) = (value, store)
        && let Some(v) = st.get(s)
    {
        return v;
    }
    if let Value::Object(m) = value {
        for key in ["$array", "array", "array_id", "arrayId", "ref", "$ref"] {
            if let (Some(Value::String(r)), Some(st)) = (m.get(key), store)
                && let Some(candidate) = st.get(r)
            {
                if let Value::Object(cm) = candidate {
                    for dk in ARRAY_KEYS {
                        if let Some(d) = cm.get(dk) {
                            return d;
                        }
                    }
                }
                return candidate;
            }
        }
        for key in ARRAY_KEYS {
            if key != "control"
                && let Some(v) = m.get(key)
            {
                return resolve_ref(v, store);
            }
        }
    }
    value
}

fn name_of(value: &Value) -> String {
    if let Value::Object(m) = value {
        for key in ["name", "path", "id", "key", "parameter", "binding"] {
            if let Some(Value::String(s)) = m.get(key) {
                return s.clone();
            }
        }
    }
    String::new()
}

fn extract_control_json(root: &Value) -> Option<(&Value, String)> {
    let store = array_store(root);
    for v in walk_json(root, 20_000) {
        let Value::Object(m) = v else { continue };
        for name in CONTROL_NAMES {
            if let Some(x) = m.get(name) {
                return Some((resolve_ref(x, store), name.to_string()));
            }
        }
        let nm = name_of(v);
        if CONTROL_NAMES.contains(&nm.as_str()) {
            for key in ["value", "values", "array", "data", "initial", "control"] {
                if let Some(x) = m.get(key) {
                    return Some((resolve_ref(x, store), nm));
                }
            }
        }
        for ck in ["parameters", "named_parameters", "namedParameters", "bindings"] {
            match m.get(ck) {
                Some(Value::Object(c)) => {
                    for name in CONTROL_NAMES {
                        if let Some(x) = c.get(name) {
                            return Some((resolve_ref(x, store), name.to_string()));
                        }
                    }
                }
                Some(Value::Array(items)) => {
                    for item in items {
                        let n = name_of(item);
                        if CONTROL_NAMES.contains(&n.as_str()) {
                            return Some((resolve_ref(item, store), n));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn find_semantic_array<'a>(roots: &[&'a Value], names: &[&str]) -> Option<&'a Value> {
    for root in roots {
        let store = array_store(root);
        for v in walk_json(root, 20_000) {
            if let Value::Object(m) = v {
                for (k, c) in m {
                    if names.contains(&k.to_lowercase().as_str()) {
                        return Some(resolve_ref(c, store));
                    }
                }
                if names.contains(&name_of(v).to_lowercase().as_str()) {
                    return Some(resolve_ref(v, store));
                }
            }
        }
    }
    None
}

fn find_scalar(roots: &[&Value], names: &[&str]) -> Option<f64> {
    for root in roots {
        for v in walk_json(root, 20_000) {
            if let Value::Object(m) = v {
                for (k, c) in m {
                    if names.contains(&k.to_lowercase().as_str())
                        && let Value::Number(n) = c
                    {
                        return n.as_f64();
                    }
                }
            }
        }
    }
    None
}

fn representation(roots: &[&Value]) -> String {
    for root in roots {
        for v in walk_json(root, 20_000) {
            if let Value::Object(m) = v
                && let Some(Value::String(r)) =
                    m.get("representation").or_else(|| m.get("control_representation"))
            {
                let l = r.to_lowercase();
                if ["occupancy", "density", "probability", "logit", "logits", "control"].contains(&l.as_str())
                {
                    return l;
                }
            }
        }
    }
    "control".into()
}

fn registration(roots: &[&Value], shape: &[usize]) -> GResult<Option<Value>> {
    for root in roots {
        for v in walk_json(root, 20_000) {
            if let Value::Object(m) = v
                && let Some(reg @ Value::Object(rm)) =
                    m.get("registration").or_else(|| m.get("grid_registration"))
            {
                if rm.get("schema").and_then(Value::as_str) != Some("implexity-grid-registration/1") {
                    return Err(verr("direct occupancy registration must use implexity-grid-registration/1"));
                }
                let parsed = GridRegistration::from_wire(reg)?;
                if parsed.shape.as_slice() != shape {
                    return Err(verr(format!(
                        "direct occupancy source shape {} disagrees with its registration shape {}",
                        shape_repr(shape),
                        shape_repr(&parsed.shape)
                    )));
                }
                let canonical = parsed.to_wire();
                if let Some(id) = rm.get("registration_id").filter(|v| !v.is_null())
                    && Some(id) != canonical.get("registration_id")
                {
                    return Err(verr(
                        "direct occupancy registration_id does not match its exact registration",
                    ));
                }
                return Ok(Some(canonical));
            }
        }
    }
    Ok(None)
}

fn shape_repr(s: &[usize]) -> String {
    crate::pyfmt::PyObj::Tuple(
        s.iter().map(|n| crate::pyfmt::PyObj::Int(i64::try_from(*n).unwrap_or(0))).collect(),
    )
    .repr()
}

fn topology_parameter(roots: &[&Value], source: &str) -> String {
    for root in roots {
        for v in walk_json(root, 20_000) {
            if let Value::Object(m) = v
                && let Some(Value::String(s)) = m.get("topology_ref")
                && !s.is_empty()
            {
                return s.clone();
            }
        }
    }
    if source.ends_with(".samples") { "model:samples".into() } else { "model:control".into() }
}

fn json_array(v: &Value) -> GResult<(Vec<usize>, Vec<f64>)> {
    fn rec(v: &Value, depth: usize, shape: &mut Vec<usize>, out: &mut Vec<f64>) -> GResult<()> {
        match v {
            Value::Array(a) => {
                if shape.len() == depth {
                    shape.push(a.len());
                } else if shape[depth] != a.len() {
                    return Err(verr(
                        "setting an array element with a sequence. The requested array has an inhomogeneous shape",
                    ));
                }
                for x in a {
                    rec(x, depth + 1, shape, out)?;
                }
                Ok(())
            }
            Value::Number(n) => {
                if depth != shape.len() {
                    return Err(verr(
                        "setting an array element with a sequence. The requested array has an inhomogeneous shape",
                    ));
                }
                out.push(n.as_f64().unwrap_or(f64::NAN));
                Ok(())
            }
            Value::Bool(b) => {
                out.push(f64::from(u8::from(*b)));
                Ok(())
            }
            _ => Err(verr("direct occupancy values must be numeric")),
        }
    }
    let mut shape = Vec::new();
    let mut out = Vec::new();
    rec(v, 0, &mut shape, &mut out)?;
    Ok((shape, out))
}

pub fn resample_3d<S: Scalar>(values: &[S], src: [usize; 3], target: [usize; 3]) -> Vec<S> {
    if src == target {
        return values.to_vec();
    }
    let axes: [(Vec<usize>, Vec<usize>, Vec<f64>); 3] = std::array::from_fn(|a| {
        #[allow(clippy::cast_precision_loss)]
        let coords = crate::numpy::linspace(0.0, src[a].saturating_sub(1) as f64, target[a]);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let lo: Vec<usize> = coords.iter().map(|c| c.floor() as usize).collect();
        let hi: Vec<usize> = lo.iter().map(|l| (l + 1).min(src[a] - 1)).collect();
        #[allow(clippy::cast_precision_loss)]
        let fr: Vec<f64> = coords.iter().zip(&lo).map(|(c, l)| c - *l as f64).collect();
        (lo, hi, fr)
    });
    let at = |i: usize, j: usize, k: usize| values[(i * src[1] + j) * src[2] + k];
    let mut out = Vec::with_capacity(target.iter().product());
    for x in 0..target[0] {
        let (x0, x1, fx) = (axes[0].0[x], axes[0].1[x], axes[0].2[x]);
        for y in 0..target[1] {
            let (y0, y1, fy) = (axes[1].0[y], axes[1].1[y], axes[1].2[y]);
            for z in 0..target[2] {
                let (z0, z1, fz) = (axes[2].0[z], axes[2].1[z], axes[2].2[z]);
                let c00 = at(x0, y0, z0) * (1.0 - fx) + at(x1, y0, z0) * fx;
                let c10 = at(x0, y1, z0) * (1.0 - fx) + at(x1, y1, z0) * fx;
                let c01 = at(x0, y0, z1) * (1.0 - fx) + at(x1, y0, z1) * fx;
                let c11 = at(x0, y1, z1) * (1.0 - fx) + at(x1, y1, z1) * fx;
                let c0 = c00 * (1.0 - fy) + c10 * fy;
                let c1 = c01 * (1.0 - fy) + c11 * fy;
                out.push(c0 * (1.0 - fz) + c1 * fz);
            }
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct TopologyMasks {
    pub admissible: Option<Vec<f64>>,
    pub fixed_solid: Option<Vec<f64>>,
    pub fixed_void: Option<Vec<f64>>,
    pub preserve: Option<Vec<f64>>,
    pub initial: Option<Vec<f64>>,
    pub weights: Option<Vec<f64>>,
}

fn indicator(m: Option<&Vec<f64>>, i: usize, default: f64) -> f64 {
    m.map_or(default, |v| if v[i] > 0.5 { 1.0 } else { 0.0 })
}


pub fn project_logits_to_fraction<S: Scalar>(
    logits: &[S],
    target_fraction: f64,
    masks: &TopologyMasks,
    iterations: usize,
) -> GResult<Vec<S>> {
    if !(0.0..=1.0).contains(&target_fraction) {
        return Err(verr("topology target fraction must lie in [0, 1]"));
    }
    let n = logits.len();
    let domain: Vec<f64> = (0..n).map(|i| indicator(masks.admissible.as_ref(), i, 1.0)).collect();
    let solid: Vec<f64> = (0..n).map(|i| indicator(masks.fixed_solid.as_ref(), i, 0.0)).collect();
    let void: Vec<f64> = (0..n).map(|i| indicator(masks.fixed_void.as_ref(), i, 0.0)).collect();
    let keep: Vec<f64> = (0..n).map(|i| indicator(masks.preserve.as_ref(), i, 0.0)).collect();
    let free: Vec<f64> =
        (0..n).map(|i| domain[i] * (1.0 - solid[i]) * (1.0 - void[i]) * (1.0 - keep[i])).collect();
    let w: Vec<f64> = match &masks.weights {
        Some(v) if v.len() == n => v.clone(),
        Some(v) if v.len() == 1 => vec![v[0]; n],
        _ => vec![1.0; n],
    };
    let preserved: Vec<f64> = masks
        .initial
        .as_ref()
        .map_or_else(|| vec![0.0; n], |v| v.iter().map(|x| x.clamp(0.0, 1.0)).collect());
    let reference = crate::numpy::sum(&(0..n).map(|i| w[i] * domain[i]).collect::<Vec<_>>());
    let fixed =
        crate::numpy::sum(&(0..n).map(|i| w[i] * (solid[i] + keep[i] * preserved[i])).collect::<Vec<_>>());
    let desired = target_fraction * reference - fixed;
    let capacity = crate::numpy::sum(&(0..n).map(|i| w[i] * free[i]).collect::<Vec<_>>());
    if desired < -1e-9 || desired > capacity + 1e-9 {
        return Err(verr(format!(
            "topology volume target is infeasible: free target {}, capacity {}",
            crate::pyfmt::g(desired),
            crate::pyfmt::g(capacity)
        )));
    }
    let mut bias = S::cst(0.0);
    for _ in 0..iterations {
        let mut residual = S::cst(-desired);
        let mut slope = S::cst(0.0);
        for i in 0..n {
            let rho = S::cst(1.0) / ((-(logits[i] + bias)).exp() + 1.0);
            let wf = w[i] * free[i];
            residual = residual + rho * wf;
            slope = slope + rho * (-rho + 1.0) * wf;
        }
        let step = residual / slope.max_c(1e-12);
        bias = bias - step.clip_c(-8.0, 8.0);
    }
    Ok((0..n)
        .map(|i| {
            let rho = S::cst(1.0) / ((-(logits[i] + bias)).exp() + 1.0);
            let mut p = rho * free[i] + solid[i] + keep[i] * preserved[i];
            if domain[i] <= 0.5 || void[i] > 0.5 {
                p = S::cst(0.0);
            }
            p.clip_c(0.0, 1.0)
        })
        .collect())
}

#[derive(Clone, Debug, Default)]
pub struct OccupancyOwner {
    pub analysis_shape: Option<Vec<usize>>,
    pub points_mm: Option<Vec<[f64; 3]>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub shape: Vec<usize>,
    pub values: Vec<f64>,
    pub metadata: DirectOccupancyMetadata,
}

pub enum OccupancyModel<'a> {
    Node(&'a NodeRef),
    Json(&'a Value),
}

struct Source {
    shape: Vec<usize>,
    values: Vec<f64>,
    name: String,
    occupancy: bool,
    roots: Vec<Value>,
}

fn node_source(root: &NodeRef, owner: Option<&OccupancyOwner>) -> GResult<Option<Source>> {
    let walk = root.walk();
    let occ = root.op().occupancy_source();
    if !root.children().is_empty() && occ.is_none() {
        return Ok(None);
    }

    let mut marked = false;
    let mut records: Vec<Value> = Vec::new();
    for (_p, n) in &walk {
        if n.op().occupancy_source().is_some() {
            marked = true;
        }
        if let Some(s) = n.op().as_any().downcast_ref::<Sampled>() {
            let rec = json!({
                "source": Value::Object(s.source.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
                "measurement": Value::Object(s.measurement.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
                "visualization": Value::Object(s.visualization.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
            });
            match marker_value_json(&rec) {
                Some(false) => {
                    return Err(verr("topologyAlwaysFree cannot be disabled for a universal topology model"));
                }
                Some(true) => marked = true,
                None => {}
            }
            records.push(rec);
        }
    }
    if !marked {
        return Ok(None);
    }
    if let Some(src) = occ {
        let (shape, values) = if let Some(points) = owner.and_then(|o| o.points_mm.as_ref()) {
            (vec![points.len()], src.occupancy_at(root, points)?)
        } else {
            let f = src.analysis_fields(root)?;
            (f.shape.to_vec(), f.rho)
        };
        return Ok(Some(Source {
            shape,
            values,
            name: format!("{}.implexity_direct_occupancy", kind_class_name(root.kind())),
            occupancy: true,
            roots: records,
        }));
    }
    for (_p, n) in &walk {
        if let Some(s) = n.op().as_any().downcast_ref::<Sampled>() {
            let get = |k: &str| s.source.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_json());
            let sem = get("field_semantics")
                .as_ref()
                .map(crate::document::py_str)
                .unwrap_or_default()
                .to_lowercase();
            let role =
                get("design_role").as_ref().map(crate::document::py_str).unwrap_or_default().to_lowercase();
            if ["occupancy", "density", "material_fraction"].contains(&sem.as_str())
                && role == "topology"
                && let Some(p) = n.param("samples")
            {
                let (shape, values) = p.to_f64_array().map_err(|_| verr("samples are not numeric"))?;
                return Ok(Some(Source {
                    shape,
                    values,
                    name: "Sampled.samples".into(),
                    occupancy: false,
                    roots: records,
                }));
            }
        }
    }
    Err(verr("universal topology model does not expose model:control"))
}

fn kind_class_name(kind: &str) -> &'static str {
    match kind {
        "lattice.controlled" => "ControlledLattice",
        "lattice.controlled_assembly" => "ControlledAssembly",
        _ => "Node",
    }
}


#[allow(clippy::too_many_lines)]
pub fn try_resolve_direct_occupancy(
    model: &OccupancyModel<'_>,
    owner: Option<&OccupancyOwner>,
) -> GResult<Option<Resolved>> {
    let src = match model {
        OccupancyModel::Node(n) => node_source(n, owner)?,
        OccupancyModel::Json(v) => {
            match marker_value_json(v) {
                Some(false) => {
                    return Err(verr("topologyAlwaysFree cannot be disabled for a universal topology model"));
                }
                Some(true) => {}
                None => return Ok(None),
            }
            let (control, name) = extract_control_json(v)
                .ok_or_else(|| verr("universal topology model does not expose model:control"))?;
            let (shape, values) = json_array(control)?;
            Some(Source { shape, values, name, occupancy: false, roots: vec![(*v).clone()] })
        }
    };
    let Some(src) = src else { return Ok(None) };
    let roots: Vec<&Value> = src.roots.iter().collect();
    let source_shape = src.shape.clone();
    let registration = registration(&roots, &source_shape)?;
    let rep = if src.occupancy { "occupancy".to_string() } else { representation(&roots) };
    let logits = ["logit", "logits", "control"].contains(&rep.as_str());
    let mut shape = src.shape.clone();
    let mut values = src.values;
    if let Some(expected) = owner
        .and_then(|o| o.analysis_shape.clone())
        .filter(|s| (s.len() == 2 || s.len() == 3) && s.iter().all(|v| *v > 0))
    {
        let prod: usize = expected.iter().product();
        if shape.len() == 1 && values.len() == prod {
            shape.clone_from(&expected);
        } else if shape != expected {
            if shape.len() == 3 && expected.len() == 3 {
                values = resample_3d(
                    &values,
                    [shape[0], shape[1], shape[2]],
                    [expected[0], expected[1], expected[2]],
                );
                shape.clone_from(&expected);
            } else if values.len() == prod {
                shape.clone_from(&expected);
            } else {
                return Err(verr(format!(
                    "direct occupancy shape {} cannot be mapped to analysis grid {}",
                    shape_repr(&shape),
                    shape_repr(&expected)
                )));
            }
        }
    }
    let mapped = |names: &[&str]| -> GResult<Option<Vec<f64>>> {
        let Some(c) = find_semantic_array(&roots, names) else { return Ok(None) };
        let (cs, cv) = json_array(c)?;
        if cs == shape {
            return Ok(Some(cv));
        }
        if cs.len() == 3 && shape.len() == 3 {
            return Ok(Some(resample_3d(&cv, [cs[0], cs[1], cs[2]], [shape[0], shape[1], shape[2]])));
        }
        if cv.len() == values.len() {
            return Ok(Some(cv));
        }
        Err(verr("topology mask shape does not match direct occupancy"))
    };
    let masks = TopologyMasks {
        admissible: mapped(&["admissible", "admissible_mask", "design_domain", "growth_envelope"])?,
        fixed_solid: mapped(&["fixed_solid", "keep_solid", "dense_wall_mask"])?,
        fixed_void: mapped(&["fixed_void", "keep_void"])?,
        preserve: mapped(&["preserve", "preserve_current", "preserve_mask"])?,
        initial: mapped(&["initial_occupancy", "initial_density", "initial"])?,
        weights: mapped(&["volume_weights", "cell_weights", "quadrature_weights"])?,
    };
    let target = find_scalar(
        &roots,
        &["target_fraction", "target_material_fraction", "volume_fraction", "material_fraction"],
    );
    if logits {
        values = match target {
            Some(t) => project_logits_to_fraction(&values, t, &masks, 32)?,
            None => values.iter().map(|l| 1.0 / (1.0 + (-l).exp())).collect(),
        };
    }
    for v in &mut values {
        *v = v.clamp(0.0, 1.0);
    }
    if let Some(a) = &masks.admissible {
        for (v, m) in values.iter_mut().zip(a) {
            if *m <= 0.5 {
                *v = 0.0;
            }
        }
    }
    if let Some(p) = &masks.preserve {
        let Some(init) = &masks.initial else {
            return Err(verr("preserve-current mask requires initial occupancy"));
        };
        for ((v, m), i) in values.iter_mut().zip(p).zip(init) {
            if *m > 0.5 {
                *v = *i;
            }
        }
    }
    if let Some(fv) = &masks.fixed_void {
        for (v, m) in values.iter_mut().zip(fv) {
            if *m > 0.5 {
                *v = 0.0;
            }
        }
    }
    if let Some(fs) = &masks.fixed_solid {
        for (v, m) in values.iter_mut().zip(fs) {
            if *m > 0.5 {
                *v = 1.0;
            }
        }
    }
    if let (Some(fs), Some(fv)) = (&masks.fixed_solid, &masks.fixed_void)
        && fs.iter().zip(fv).any(|(a, b)| *a > 0.5 && *b > 0.5)
    {
        return Err(verr("fixed-solid and fixed-void topology masks overlap"));
    }
    let metadata = DirectOccupancyMetadata {
        source_shape,
        analysis_shape: shape.clone(),
        representation: "occupancy".into(),
        registration,
        parameter: topology_parameter(&roots, &src.name),
        source: src.name,
    };
    Ok(Some(Resolved { shape, values, metadata }))
}


pub fn ensure_topology_declaration(request: &Value) -> GResult<Value> {
    let Value::Object(m) = request else { return Ok(request.clone()) };
    match marker_value_json(request) {
        Some(false) => return Err(verr("direct-gradient topology cannot be disabled")),
        Some(true) => {}
        None => return Ok(request.clone()),
    }
    let mut result = m.clone();
    for key in ["free", "free_parameters", "freeParameters", "design_variables", "designVariables"] {
        let Some(current) = result.get(key) else { continue };
        let Value::Array(items) = current else { return Ok(Value::Object(result)) };
        let mut items = items.clone();
        let names: Vec<String> =
            items.iter().map(|i| if let Value::String(s) = i { s.clone() } else { name_of(i) }).collect();
        if !names.iter().any(|n| n == "model:control") {
            if let Some(Value::Object(first)) = items.first() {
                let name_key = ["path", "name", "id", "key"]
                    .into_iter()
                    .find(|c| first.contains_key(*c))
                    .unwrap_or("path");
                items.push(json!({ name_key: "model:control" }));
            } else {
                items.push(json!("model:control"));
            }
        }
        result.insert(key.into(), Value::Array(items));
        return Ok(Value::Object(result));
    }
    Ok(Value::Object(result))
}


pub fn augment_payload(payload: &Value, metadata: Option<&DirectOccupancyMetadata>) -> GResult<Value> {
    let Some(meta) = metadata else { return Ok(payload.clone()) };
    let wire = meta.to_wire();
    if wire.get("registration").is_none_or(Value::is_null) {
        return Ok(payload.clone());
    }
    let Value::Object(body) = payload else { return Ok(payload.clone()) };
    let mut body = body.clone();
    let kind = body.get("kind").map(crate::document::py_str).unwrap_or_default().to_lowercase();
    body.entry("direct_occupancy").or_insert_with(|| wire.clone());
    if kind.contains("sensitivity") || kind.contains("derivative") {
        let registration = wire["registration"].clone();
        match body.get("field_registration") {
            None | Some(Value::Null) => {
                body.insert("field_registration".into(), registration);
            }
            Some(current) => {
                if current.get("registration_id") != registration.get("registration_id") {
                    return Err(verr(
                        "direct occupancy sensitivity registration disagrees with its authored source field",
                    ));
                }
            }
        }
    }
    Ok(Value::Object(body))
}
