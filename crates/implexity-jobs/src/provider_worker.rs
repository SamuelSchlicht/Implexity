// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, MatchingTimeNewtonGuess, ProviderProblem,
};
use implexity_core::{CaeError, CaeResult};
use implexity_io::npy::NpyArray;
use implexity_optim::provider_ops::{DesignOp, design_operations};
use implexity_optim::{DesignLayout, NamedArrays, design_identity};
use implexity_runtime::intent_orchestrated::IntentOrchestratedProvider;
use implexity_runtime::provider_job_authority::{
    ProviderFacts, attach_exact_effort_evidence, private_provider_effort_validation, validate_effort_binding,
};
use implexity_solve::matching_time_guess::{MatchingTimeGuessStore, execution_identity, public_descriptor};
use ndarray::{ArrayD, Axis, IxDyn, Slice};
use serde_json::{Map, Value};

use crate::artifacts::ResultArtifactStore;
use crate::effort::{physics_snapshot, provider_computation_effort_scope};
use crate::error::{JobError, JobResult};
use crate::private::{compact_text, perf_counter_ns};

pub const INTERNAL_REQUEST_KEYS: [&str; 15] = [
    "runtime_packages",
    "provider",
    "problem",
    "design_file",
    "topology_file",
    "design_coordinates",
    "design_state_id",
    "artifact_root",
    "solve_id",
    "model_content_id",
    "response",
    "responses",
    "matching_time_guess",
    "physics_snapshot",
    "computation_effort",
];

const WORKER_TIMING_SCHEMA: &str = "implexity-provider-worker-timing/1";

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}


pub fn checked_preflight_effects(value: Option<&Value>) -> CaeResult<Value> {
    implexity_runtime::intent_orchestrated::checked_preflight_effects(value)
}


pub fn provider_admission_report(
    elapsed_ns: i64,
    effects: Option<&Value>,
    worker_setup_subset: bool,
) -> CaeResult<Value> {
    let mut m = Map::new();
    m.insert("schema".into(), Value::String("implexity-provider-admission/1".into()));
    m.insert("scope".into(), Value::String("inclusive_provider_preparation_and_admission".into()));
    m.insert(
        "timing_relation".into(),
        Value::String(
            if worker_setup_subset {
                "subset_of_provider_setup_wall_ns"
            } else {
                "standalone_admission_interval"
            }
            .into(),
        ),
    );
    m.insert("admission_wall_ns".into(), Value::from(elapsed_ns));
    m.insert("effects".into(), checked_preflight_effects(effects)?);
    m.insert(
        "observed_numerical_effects".into(),
        serde_json::json!({"status": "not_instrumented", "solve_count": null}),
    );
    Ok(Value::Object(m))
}


pub fn validate_provider_admission(value: Option<&Value>, execution_timing: Option<&Value>) -> CaeResult<()> {
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(()) };
    let keys =
        ["schema", "scope", "timing_relation", "admission_wall_ns", "effects", "observed_numerical_effects"];
    let ok = value.as_object().is_some_and(|m| {
        m.len() == keys.len()
            && keys.iter().all(|k| m.contains_key(*k))
            && m["schema"] == "implexity-provider-admission/1"
            && m["scope"] == "inclusive_provider_preparation_and_admission"
            && matches!(
                m["timing_relation"].as_str(),
                Some("subset_of_provider_setup_wall_ns" | "standalone_admission_interval")
            )
            && m["admission_wall_ns"].as_i64().is_some_and(|n| n >= 0 && m["admission_wall_ns"].is_i64())
            && m["effects"].is_object()
            && m["observed_numerical_effects"]
                == serde_json::json!({"status": "not_instrumented", "solve_count": null})
    });
    if !ok {
        return contract("invalid provider admission evidence");
    }
    checked_preflight_effects(value.get("effects"))?;
    if value["timing_relation"] == "subset_of_provider_setup_wall_ns" {
        let setup = execution_timing
            .and_then(|t| t.get("nanoseconds"))
            .and_then(|n| n.get("provider_setup_wall_ns"))
            .and_then(Value::as_i64);
        if setup.is_none_or(|s| value["admission_wall_ns"].as_i64().unwrap_or(i64::MAX) > s) {
            return contract("provider admission interval exceeds worker setup");
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct WorkerExecutionTiming {
    operation: String,
    started_ns: i64,
    started: Instant,
    cursor_ns: i64,
    setup_ns: i64,
    provider_ns: i64,
    provider_started: bool,
    provider_finished: bool,
    closed: bool,
}

impl WorkerExecutionTiming {

    pub fn new(operation: &str) -> CaeResult<Self> {
        if !matches!(operation, "preflight" | "evaluate" | "sensitivity") {
            return contract("worker timing operation is unsupported");
        }
        let now = perf_counter_ns();
        Ok(Self {
            operation: operation.into(),
            started_ns: now,
            started: Instant::now(),
            cursor_ns: now,
            setup_ns: 0,
            provider_ns: 0,
            provider_started: false,
            provider_finished: false,
            closed: false,
        })
    }

    #[must_use]
    pub fn started(&self) -> Instant {
        self.started
    }


    pub fn begin_provider_numerical(&mut self) -> CaeResult<()> {
        if self.closed || self.provider_started || self.provider_finished || self.operation == "preflight" {
            return contract("worker provider numerical timing lifecycle is invalid");
        }
        let now = perf_counter_ns();
        if now < self.cursor_ns {
            return contract("worker monotonic timing moved backwards");
        }
        self.setup_ns += now - self.cursor_ns;
        self.cursor_ns = now;
        self.provider_started = true;
        Ok(())
    }


    pub fn end_provider_numerical(&mut self) -> CaeResult<()> {
        if self.closed || !self.provider_started || self.provider_finished {
            return contract("worker provider numerical timing lifecycle is invalid");
        }
        let now = perf_counter_ns();
        if now < self.cursor_ns {
            return contract("worker monotonic timing moved backwards");
        }
        self.provider_ns += now - self.cursor_ns;
        self.cursor_ns = now;
        self.provider_finished = true;
        Ok(())
    }


    pub fn finish(&mut self) -> CaeResult<Value> {
        if self.closed {
            return contract("worker timing lifecycle is already closed");
        }
        if (self.operation == "evaluate" || self.operation == "sensitivity") && !self.provider_finished {
            return contract("worker timing lacks the provider numerical interval");
        }
        let now = perf_counter_ns();
        if now < self.cursor_ns {
            return contract("worker monotonic timing moved backwards");
        }
        let artifact_ns = if self.operation == "preflight" {
            self.setup_ns += now - self.cursor_ns;
            0
        } else {
            now - self.cursor_ns
        };
        let wall_ns = now - self.started_ns;
        if self.setup_ns + self.provider_ns + artifact_ns != wall_ns {
            return contract("worker timing accounting identity failed");
        }
        self.closed = true;
        let mut ns = Map::new();
        ns.insert("provider_setup_wall_ns".into(), Value::from(self.setup_ns));
        ns.insert("provider_numerical_wall_ns".into(), Value::from(self.provider_ns));
        ns.insert("transport_artifact_wall_ns".into(), Value::from(artifact_ns));
        ns.insert("wall_time_ns".into(), Value::from(wall_ns));
        let measurements: Map<String, Value> = ns
            .iter()
            .map(|(k, v)| {
                #[allow(clippy::cast_precision_loss)]
                let seconds = v.as_i64().unwrap_or(0) as f64 / 1_000_000_000.0;
                (format!("{}_s", k.trim_end_matches("_ns")), Value::from(seconds))
            })
            .collect();
        let mut m = Map::new();
        m.insert("schema".into(), Value::String(WORKER_TIMING_SCHEMA.into()));
        m.insert("operation".into(), Value::String(self.operation.clone()));
        m.insert("clock".into(), Value::String("time.perf_counter_ns".into()));
        m.insert(
            "accounting".into(),
            Value::String(
                "wall_time_ns=provider_setup_wall_ns+provider_numerical_wall_ns+transport_artifact_wall_ns"
                    .into(),
            ),
        );
        m.insert("nanoseconds".into(), Value::Object(ns));
        m.insert("measurements".into(), Value::Object(measurements));
        Ok(Value::Object(m))
    }
}


pub fn load_topology(path: &Path) -> CaeResult<ArrayD<f64>> {
    let npz = implexity_io::npz::load_file(path).map_err(|e| CaeError::contract(e.to_string()))?;
    let Some(raw) = npz.get("topology").or_else(|| npz.get("p_topology")) else {
        return contract("topology NPZ contains neither topology nor p_topology");
    };
    match raw.to_f64() {
        Some(x) if x.ndim() == 3 && x.iter().all(|v| v.is_finite()) => Ok(x),
        _ => contract("model:control must be a finite three-dimensional array"),
    }
}

#[must_use]
pub fn field_array(value: &FieldValue) -> Option<ArrayD<f64>> {
    match value {
        FieldValue::Array(a) => Some(a.clone()),
        FieldValue::Json(v) => json_array(v),
    }
}

#[must_use]
pub fn json_array(value: &Value) -> Option<ArrayD<f64>> {
    fn walk(value: &Value, depth: usize, shape: &mut Vec<usize>, out: &mut Vec<f64>) -> Option<()> {
        match value {
            Value::Array(items) => {
                if shape.len() == depth {
                    shape.push(items.len());
                } else if shape.get(depth) != Some(&items.len()) {
                    return None;
                }
                items.iter().try_for_each(|v| walk(v, depth + 1, shape, out))
            }
            Value::Number(n) if shape.len() == depth => {
                out.push(n.as_f64()?);
                Some(())
            }
            Value::Bool(b) if shape.len() == depth => {
                out.push(f64::from(u8::from(*b)));
                Some(())
            }
            _ => None,
        }
    }
    let mut shape = Vec::new();
    let mut out = Vec::new();
    walk(value, 0, &mut shape, &mut out)?;
    ArrayD::from_shape_vec(IxDyn(&shape), out).ok()
}

fn problem_value(problem: &ProviderProblem) -> Option<&Value> {
    problem.downcast_ref::<Value>()
}

fn map_node_box(spec: &Map<String, Value>) -> CaeResult<([f64; 3], [f64; 3])> {
    let geometry = spec.get("geometry").and_then(Value::as_object);
    let triple = |key: &str, default: Option<[f64; 3]>| -> CaeResult<[f64; 3]> {
        match geometry.and_then(|g| g.get(key)).and_then(Value::as_array) {
            Some(a) if a.len() == 3 && a.iter().all(Value::is_number) => {
                Ok([a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0), a[2].as_f64().unwrap_or(0.0)])
            }
            _ => default.ok_or_else(|| CaeError::contract(format!("geometry design map node lacks {key}"))),
        }
    };
    Ok((triple("origin_mm", Some([0.0; 3]))?, triple("domain_mm", None)?))
}

fn registration_wire(shape: &[usize], lo: [f64; 3], hi: [f64; 3]) -> CaeResult<Value> {
    let Ok(shape3) = <[usize; 3]>::try_from(shape) else {
        return contract("field registration requires three dimensions");
    };
    implexity_geometry::field_registration::axis_aligned_registration(shape3, lo, hi, "cell")
        .map(|r| r.to_wire())
        .map_err(|e| CaeError::contract(e.to_string()))
}

fn registration_from_wire(raw: &Value) -> CaeResult<Value> {
    implexity_geometry::field_registration::GridRegistration::from_wire(raw)
        .map(|r| r.to_wire())
        .map_err(|e| CaeError::contract(e.to_string()))
}


pub fn problem_registration(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    shape: &[usize],
) -> CaeResult<Option<Value>> {
    if let Some(p) = problem_value(problem).and_then(Value::as_object) {
        let context = p.get("context").and_then(Value::as_object).cloned().unwrap_or_default();
        let raw = p
            .get("field_registration")
            .filter(|v| implexity_core::pyobj::truthy(v))
            .or_else(|| context.get("field_registration"))
            .filter(|v| !v.is_null())
            .cloned();
        if context.get("geometry_design_map").is_some_and(|v| !v.is_null()) {
            let Some(mapping) = implexity_solve::geometry_design_map::from_context(&context)? else {
                return Ok(None);
            };
            if shape != mapping.analysis_shape() {
                return contract("result-worker analysis shape disagrees with the authoritative volume");
            }
            let (origin, domain) = map_node_box(mapping.spec())?;
            let expected = registration_wire(shape, origin, std::array::from_fn(|a| origin[a] + domain[a]))?;
            if let Some(raw) = raw
                && registration_from_wire(&raw)? != expected
            {
                return contract("authored result registration conflicts with the authoritative volume");
            }
            return Ok(Some(expected));
        }
        if let Some(raw) = raw {
            let reg = registration_from_wire(&raw)?;
            let reg_shape: Vec<usize> = reg["shape"]
                .as_array()
                .map(|a| {
                    a.iter().filter_map(Value::as_u64).map(|v| usize::try_from(v).unwrap_or(0)).collect()
                })
                .unwrap_or_default();
            if reg_shape != shape {
                return contract("authored analysis registration disagrees with the topology grid");
            }
            if raw.get("registration_id").is_some_and(implexity_core::pyobj::truthy)
                && raw.get("registration_id") != reg.get("registration_id")
            {
                return contract("authored field registration identity is stale");
            }
            return Ok(Some(reg));
        }
        let raw_shape = p.get("grid_shape").filter(|v| !v.is_null());
        let raw_extent = p.get("extent_mm").filter(|v| !v.is_null());
        if raw_shape.is_some() || raw_extent.is_some() {
            let (Some(Value::Array(s)), Some(Value::Array(e))) = (raw_shape, raw_extent) else {
                return contract("grid_shape and extent_mm must jointly define a cell registration");
            };
            if s.len() != 3 || e.len() != 3 {
                return contract("grid_shape and extent_mm must jointly define a cell registration");
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let declared: Vec<usize> = s.iter().map(|v| v.as_f64().unwrap_or(-1.0) as usize).collect();
            if declared != shape {
                return Ok(None);
            }
            let hi: [f64; 3] = std::array::from_fn(|a| e[a].as_f64().unwrap_or(f64::NAN));
            return Ok(Some(registration_wire(shape, [0.0; 3], hi)?));
        }
        if p.contains_key("intent") {
            let children =
                context.get("provider_problems").and_then(Value::as_object).cloned().unwrap_or_default();
            let mut registrations: Vec<Value> = Vec::new();
            for (aid, raw) in &children {
                let child = implexity_core::registries::global().providers.get(aid)?;
                let normalised = child.normalise_problem(raw)?;
                if let Some(reg) = problem_registration(child.as_ref(), &normalised, shape)? {
                    registrations.push(reg);
                }
            }
            if let Some(first) = registrations.first() {
                if registrations[1..].iter().any(|r| r != first) {
                    return contract(
                        "Different field registrations require separate result artifacts; refusing ambiguous common registration",
                    );
                }
                return Ok(Some(first.clone()));
            }
        }
        return Ok(None);
    }
    let document = design_operations(provider).and_then(|ops| ops.problem_document(problem)).transpose()?;
    let domain = document.as_ref().and_then(|d| d.get("domain")).filter(|d| !d.is_null());
    let (Some(origin), Some(extent)) = (
        domain.and_then(|d| d.get("origin_m")).and_then(Value::as_array),
        domain.and_then(|d| d.get("extent_m")).and_then(Value::as_array),
    ) else {
        return Ok(None);
    };
    let o: Vec<f64> = origin.iter().filter_map(Value::as_f64).collect();
    let e: Vec<f64> = extent.iter().filter_map(Value::as_f64).collect();
    if o.len() != 3 || e.len() != 3 {
        return Ok(None);
    }
    let lo: [f64; 3] = std::array::from_fn(|a| o[a] * 1000.0);
    let hi: [f64; 3] = std::array::from_fn(|a| (o[a] + e[a]) * 1000.0);
    Ok(Some(registration_wire(shape, lo, hi)?))
}


#[allow(clippy::type_complexity)]
pub fn cell_fields(
    fields: &BTreeMap<String, FieldValue>,
    shape: &[usize],
    declared: Option<&Value>,
) -> CaeResult<(BTreeMap<String, ArrayD<f64>>, Map<String, Value>)> {
    let mut arrays: BTreeMap<String, ArrayD<f64>> = BTreeMap::new();
    let mut metadata = Map::new();
    let declared = declared.and_then(Value::as_object).cloned().unwrap_or_default();
    for (name, value) in fields {
        let Some(arr) = field_array(value).filter(|a| !a.is_empty() && a.iter().all(|v| v.is_finite()))
        else {
            return contract(format!(
                "provider field {} is empty, nonnumeric or nonfinite",
                implexity_core::py_repr::repr_str(name)
            ));
        };
        let mut meta = declared.get(name).and_then(Value::as_object).cloned().unwrap_or_default();
        meta.entry("units").or_insert_with(|| Value::String("unspecified".into()));
        meta.entry("source").or_insert_with(|| Value::String("provider_exact".into()));
        if !meta.contains_key("rank") {
            let rank = if arr.ndim() == 5 && arr.shape()[3..] == [3, 3] {
                "tensor"
            } else if arr.ndim() == 4 {
                "vector"
            } else {
                "scalar"
            };
            meta.insert("rank".into(), Value::String(rank.into()));
        }
        meta.insert("shape".into(), Value::Array(arr.shape().iter().map(|d| Value::from(*d)).collect()));
        metadata.insert(name.clone(), Value::Object(meta));
        arrays.insert(name.clone(), arr);
    }
    let mut groups: BTreeMap<String, BTreeMap<usize, String>> = BTreeMap::new();
    for (name, meta) in &metadata {
        let vector = meta.get("vector").and_then(Value::as_str).filter(|s| !s.is_empty());
        let axis = match meta.get("association").and_then(Value::as_str) {
            Some("face_x") => Some(0),
            Some("face_y") => Some(1),
            Some("face_z") => Some(2),
            _ => None,
        };
        let (Some(vector), Some(axis)) = (vector, axis) else { continue };
        let namespace = name.rsplit_once("::").map_or(String::new(), |(ns, _)| format!("{ns}::"));
        groups.entry(format!("{namespace}{vector}")).or_default().insert(axis, name.clone());
    }
    if shape.len() == 3 {
        let (nx, ny, nz) = (shape[0], shape[1], shape[2]);
        for (view, members) in groups {
            if members.len() != 3 {
                continue;
            }
            let sources = [members[&0].clone(), members[&1].clone(), members[&2].clone()];
            let units: std::collections::BTreeSet<String> =
                sources.iter().map(|n| implexity_core::pyobj::py_str(&metadata[n]["units"])).collect();
            if units.len() != 1 {
                continue;
            }
            let (u, v, w) = (&arrays[&sources[0]], &arrays[&sources[1]], &arrays[&sources[2]]);
            if u.shape() != [nx + 1, ny, nz] || v.shape() != [nx, ny + 1, nz] || w.shape() != [nx, ny, nz + 1]
            {
                continue;
            }
            if arrays.contains_key(&view) {
                return contract("derived field view name collides with an exact field");
            }
            let average = |a: &ArrayD<f64>, axis: usize, n: usize| -> ArrayD<f64> {
                let lo = a.slice_axis(Axis(axis), Slice::from(0..n));
                let hi = a.slice_axis(Axis(axis), Slice::from(1..=n));
                (&lo + &hi) * 0.5
            };
            let cu = average(u, 0, nx);
            let cv = average(v, 1, ny);
            let cw = average(w, 2, nz);
            let mut stacked = ArrayD::<f64>::zeros(IxDyn(&[nx, ny, nz, 3]));
            for i in 0..nx {
                for j in 0..ny {
                    for k in 0..nz {
                        stacked[[i, j, k, 0]] = cu[[i, j, k]];
                        stacked[[i, j, k, 1]] = cv[[i, j, k]];
                        stacked[[i, j, k, 2]] = cw[[i, j, k]];
                    }
                }
            }
            let mut meta = Map::new();
            meta.insert("rank".into(), Value::String("vector".into()));
            meta.insert("units".into(), Value::String(units.into_iter().next().unwrap_or_default()));
            meta.insert("association".into(), Value::String("cell".into()));
            meta.insert("components".into(), serde_json::json!(["x", "y", "z"]));
            meta.insert(
                "source".into(),
                Value::String("derived_cell_center_view_from_exact_face_fields".into()),
            );
            meta.insert("source_fields".into(), serde_json::json!(sources));
            metadata.insert(view.clone(), Value::Object(meta));
            arrays.insert(view, stacked);
        }
    }
    Ok((arrays, metadata))
}


pub fn request_design(request: &Map<String, Value>) -> CaeResult<(NamedArrays, String)> {
    let design = if let Some(f) = request.get("design_file").filter(|v| implexity_core::pyobj::truthy(v)) {
        implexity_optim::design::load_design(Path::new(&implexity_core::pyobj::py_str(f)))?
    } else {
        let path = implexity_core::pyobj::py_str(request.get("topology_file").unwrap_or(&Value::Null));
        NamedArrays::single("model:control", load_topology(Path::new(&path))?)
    };
    if let Some(expected) = request.get("design_coordinates").filter(|v| !v.is_null()) {
        let expected: Vec<String> = expected
            .as_array()
            .map(|a| a.iter().map(implexity_core::pyobj::py_str).collect())
            .unwrap_or_default();
        let actual: Vec<String> = design.names();
        if expected != actual {
            return contract("worker snapshot coordinate set/order disagrees with its native declaration");
        }
    }
    let identity = design_identity(&design)?;
    if let Some(claimed) = request.get("design_state_id").filter(|v| implexity_core::pyobj::truthy(v))
        && claimed.as_str() != Some(identity.as_str())
    {
        return contract("worker design snapshot is stale or corrupted");
    }
    Ok((design, identity))
}

pub struct Prepared {
    pub provider: Arc<dyn CaeProvider>,
    pub design: NamedArrays,
    pub identity: String,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("provider", &self.provider.name())
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}


pub fn prepare_request_identity(request: &mut Map<String, Value>) -> CaeResult<Prepared> {
    if let Some(snapshot) = request.get("runtime_packages") {
        implexity_core::packages::global().activate_snapshot_value(snapshot)?;
    }
    if let Some(imports) = request.get("physics_snapshot").and_then(|s|s.get("imported_providers")) { implexity_runtime::provider_import::restore_snapshot(imports)?; }
    let name = implexity_core::pyobj::py_str(request.get("provider").unwrap_or(&Value::Null));
    let provider = implexity_core::registries::global().providers.get(&name)?;
    let caps = provider.capabilities()?;
    let current = physics_snapshot()?;
    if let Some(declared) = request.get("physics_snapshot").filter(|v| !v.is_null()) {
        let Some(declared) = declared.as_object() else {
            return contract("worker physics snapshot must be an object");
        };
        for key in ["registry_fingerprint", "load_order_fingerprint", "loaded"] {
            if declared.get(key) != current.get(key) {
                return contract("worker physics snapshot is stale; provider registry changed");
            }
        }
    }
    if let Some(binding) = request.get("computation_effort").filter(|v| !v.is_null()).cloned() {
        let (binding, candidates) =
            match private_provider_effort_validation(&name, provider.as_ref(), &caps, &current, &binding)? {
                Some((rebound, candidates)) => {
                    let rebound = Value::Object(rebound);
                    request.insert("computation_effort".into(), rebound.clone());
                    (rebound, Some(candidates))
                }
                None => (binding, None),
            };
        let facts = ProviderFacts {
            provider_name: &name,
            capabilities: &caps,
            physics: &current,
            candidates: candidates.as_ref(),
        };
        validate_effort_binding(&binding, Some(&facts))?;
    }
    let (design, identity) = request_design(request)?;
    let supported = caps.design_coordinates();
    if design.names().iter().any(|n| !supported.contains(n)) {
        return contract("worker provider lacks declared derivative support for the complete design");
    }
    Ok(Prepared { provider, design, identity })
}

pub struct ProviderRequest {
    pub problem: ProviderProblem,
    pub preflight: Map<String, Value>,
    pub coupling: Value,
}

impl std::fmt::Debug for ProviderRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRequest").field("preflight", &self.preflight).finish_non_exhaustive()
    }
}


pub fn prepare_provider_request(
    request: &Map<String, Value>,
    prepared: &Prepared,
    for_optimization: bool,
) -> CaeResult<ProviderRequest> {
    let provider = prepared.provider.as_ref();
    let raw_problem = request
        .get("problem")
        .filter(|v| implexity_core::pyobj::truthy(v))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let problem = provider.normalise_problem(&raw_problem)?;
    let ops = design_operations(provider);
    let control = prepared.design.get("model:control").cloned();
    let pre = if let Some(o) = ops.filter(|o| o.provides(DesignOp::PreflightDesign)) {
        o.preflight_design(&problem, &prepared.design)?
    } else {
        let Some(control) = control.as_ref() else {
            return Err(CaeError::contract("'model:control'"));
        };
        provider.preflight(&problem, Some(control))?
    };
    if !pre.get("ok").is_some_and(implexity_core::pyobj::truthy) {
        let issues = pre
            .get("issues")
            .filter(|v| implexity_core::pyobj::truthy(v))
            .or_else(|| pre.get("errors"))
            .map_or_else(|| "None".to_string(), implexity_core::pyobj::repr);
        return contract(format!("provider preflight refused complete-state evaluation: {issues}"));
    }
    if let Some(o) = ops.filter(|o| o.provides(DesignOp::ProjectTopology)) {
        let Some(control) = control.as_ref() else {
            return Err(CaeError::contract("'model:control'"));
        };
        let projected = o.project_topology(&problem, control, control)?;
        if projected != *control {
            return contract(
                "authoritative design violates provider invariants; re-evaluation cannot silently project it",
            );
        }
    }
    let coupling = implexity_core::coupling_graph::validate_provider_couplings(
        provider,
        Some(&problem),
        &implexity_core::registries::global().extensions,
        for_optimization,
    );
    if !coupling.get("ok").is_some_and(implexity_core::pyobj::truthy) {
        return contract(format!(
            "automatic coupling preflight refused evaluation: {}",
            implexity_core::pyobj::repr(coupling.get("errors").unwrap_or(&Value::Null))
        ));
    }
    Ok(ProviderRequest { problem, preflight: pre, coupling })
}

fn handoff_config(request: &Map<String, Value>) -> CaeResult<Map<String, Value>> {
    match request.get("matching_time_guess") {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) if m.keys().all(|k| k == "consume" || k == "produce") => Ok(m.clone()),
        Some(_) => contract("matching_time_guess worker transport is malformed"),
    }
}

fn nonempty_text(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|s| !s.is_empty())
}

pub(crate) fn problem_json(provider: &dyn CaeProvider, problem: &ProviderProblem) -> CaeResult<Value> {
    match design_operations(provider).and_then(|o| o.problem_document(problem)) {
        Some(v) => v,
        None => Ok(problem_value(problem).cloned().unwrap_or(Value::Null)),
    }
}

pub(crate) fn provider_lifecycle_verifier(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    consume: bool,
    produce: bool,
    require_accepted: bool,
) -> CaeResult<Option<Value>> {
    let Some(intent) = provider.as_any().downcast_ref::<IntentOrchestratedProvider>() else {
        return Ok(None);
    };
    let document = problem_json(provider, problem)?;
    let _ = require_accepted;
    intent.validate_matching_time_guess_lifecycle(&document, consume, produce).map(|m| Some(Value::Object(m)))
}


pub fn validate_handoff_lifecycle(
    request: &Map<String, Value>,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
) -> CaeResult<()> {
    let config = handoff_config(request)?;
    if config.is_empty() {
        return Ok(());
    }
    let consume = config.get("consume").filter(|v| !v.is_null());
    let produce = config.get("produce").filter(|v| !v.is_null());
    if let Some(c) = consume
        && !c
            .as_object()
            .is_some_and(|m| m.keys().all(|k| matches!(k.as_str(), "capsule_id" | "store_root" | "required")))
    {
        return contract("matching-time guess consumption transport is malformed");
    }
    if let Some(p) = produce
        && *p != Value::Bool(false)
        && !p
            .as_object()
            .is_some_and(|m| m.keys().all(|k| matches!(k.as_str(), "store_root" | "require_accepted")))
    {
        return contract("matching-time guess production transport is malformed");
    }
    if let Some(c) = consume.and_then(Value::as_object) {
        let required = match c.get("required") {
            None => true,
            Some(Value::Bool(b)) => *b,
            Some(_) => return contract("matching-time guess required policy must be boolean"),
        };
        let capsule = c.get("capsule_id").filter(|v| !v.is_null());
        let root = c.get("store_root").filter(|v| !v.is_null());
        if required && (!nonempty_text(capsule) || !nonempty_text(root)) {
            return contract("required matching-time guess reference is missing");
        }
        if capsule.is_some() && !nonempty_text(capsule) {
            return contract("matching-time guess capsule reference is malformed");
        }
        if root.is_some() && !nonempty_text(root) {
            return contract("matching-time guess store reference is malformed");
        }
    }
    if let Some(p) = produce.and_then(Value::as_object) {
        if p.get("require_accepted").is_some_and(|v| !v.is_boolean()) {
            return contract("matching-time guess accepted-state policy must be boolean");
        }
        if !nonempty_text(p.get("store_root")) {
            return contract("matching-time guess production store is missing");
        }
    }
    let ops = design_operations(provider);
    let has = |op: DesignOp| ops.is_some_and(|o| o.provides(op));
    if consume.is_some() && !has(DesignOp::InstallMatchingTimeGuess) {
        return contract("selected provider has no matching-time guess install lifecycle");
    }
    let producing = produce.is_some_and(|p| *p != Value::Bool(false));
    if producing && !has(DesignOp::ExportMatchingTimeGuess) {
        return contract("selected provider has no matching-time guess export lifecycle");
    }
    let accepted = produce
        .and_then(Value::as_object)
        .and_then(|p| p.get("require_accepted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(result) =
        provider_lifecycle_verifier(provider, problem, consume.is_some(), producing, accepted)?
        && result.get("mutation_performed") != Some(&Value::Bool(false))
    {
        return contract("matching-time lifecycle validation returned unsafe evidence");
    }
    Ok(())
}

pub(crate) fn install_acknowledgement_ok(ack: &Map<String, Value>, identity: &str) -> bool {
    ack.get("design_state_id").and_then(Value::as_str) == Some(identity)
        && ack.get("canonical_cache_admission") == Some(&Value::Bool(false))
}


pub fn consume_matching_time_guess(
    request: &Map<String, Value>,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
    identity: &str,
) -> JobResult<Option<Map<String, Value>>> {
    let config = handoff_config(request)?;
    let Some(raw) = config.get("consume").filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(raw) = raw
        .as_object()
        .filter(|m| m.keys().all(|k| matches!(k.as_str(), "capsule_id" | "store_root" | "required")))
    else {
        return Err(JobError::contract("matching-time guess consumption transport is malformed"));
    };
    let required = match raw.get("required") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(JobError::contract("matching-time guess required policy must be boolean")),
    };
    let capsule = raw.get("capsule_id").filter(|v| implexity_core::pyobj::truthy(v));
    let root = raw.get("store_root").filter(|v| implexity_core::pyobj::truthy(v));
    let (Some(capsule), Some(root)) = (capsule, root) else {
        if required {
            return Err(JobError::contract("required matching-time guess reference is missing"));
        }
        return Ok(None);
    };
    let provider_name = implexity_core::pyobj::py_str(request.get("provider").unwrap_or(&Value::Null));
    let execution =
        execution_identity(&provider_name, &problem_json(provider, problem)?, design, None, None, None)?;
    let store = MatchingTimeGuessStore::new(implexity_core::pyobj::py_str(root))?;
    let (guess, descriptor) = match store.load(&implexity_core::pyobj::py_str(capsule), &execution) {
        Ok(v) => v,
        Err(implexity_solve::matching_time_guess::GuessError::Missing(_)) if !required => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::InstallMatchingTimeGuess))
    else {
        return Err(JobError::contract("selected provider has no matching-time guess lifecycle"));
    };
    let ack = ops.install_matching_time_guess(problem, design, &guess)?;
    if !install_acknowledgement_ok(&ack, identity) {
        return Err(JobError::contract(
            "matching-time guess installation acknowledgement is stale or unsafe",
        ));
    }
    let mut merged = descriptor;
    merged.insert("consumed".into(), Value::Bool(true));
    merged.insert("installation".into(), Value::Object(ack));
    Ok(Some(public_descriptor(&Value::Object(merged))?))
}


pub fn produce_matching_time_guess(
    request: &Map<String, Value>,
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
    identity: &str,
) -> JobResult<Option<Map<String, Value>>> {
    let config = handoff_config(request)?;
    let raw = match config.get("produce") {
        None | Some(Value::Null | Value::Bool(false)) => return Ok(None),
        Some(Value::Bool(true)) => {
            return Err(JobError::contract(
                "worker matching-time guess production requires an internal store root",
            ));
        }
        Some(v) => v,
    };
    let Some(raw) =
        raw.as_object().filter(|m| m.keys().all(|k| k == "store_root" || k == "require_accepted"))
    else {
        return Err(JobError::contract("matching-time guess production transport is malformed"));
    };
    let Some(root) = raw.get("store_root").filter(|v| implexity_core::pyobj::truthy(v)) else {
        return Err(JobError::contract("matching-time guess production store is missing"));
    };
    let accepted = match raw.get("require_accepted") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => {
            return Err(JobError::contract("matching-time guess accepted-state policy must be boolean"));
        }
    };
    let Some(ops) = design_operations(provider).filter(|o| o.provides(DesignOp::ExportMatchingTimeGuess))
    else {
        return Err(JobError::contract("selected provider has no matching-time guess lifecycle"));
    };
    let guess: MatchingTimeNewtonGuess = ops.export_matching_time_guess(problem, design, accepted)?;
    let provider_name = implexity_core::pyobj::py_str(request.get("provider").unwrap_or(&Value::Null));
    let execution =
        execution_identity(&provider_name, &problem_json(provider, problem)?, design, None, None, None)?;
    if execution.get("design_state_id").and_then(Value::as_str) != Some(identity) {
        return Err(JobError::contract("matching-time guess execution identity is stale"));
    }
    let store = MatchingTimeGuessStore::new(implexity_core::pyobj::py_str(root))?;
    let descriptor = store.create(&guess, &execution)?;
    Ok(Some(public_descriptor(&Value::Object(descriptor))?))
}


pub fn store_design(
    request: &Map<String, Value>,
    design: &NamedArrays,
    identity: &str,
) -> JobResult<Option<Map<String, Value>>> {
    let Some(root) = request.get("artifact_root").filter(|v| implexity_core::pyobj::truthy(v)) else {
        return Ok(None);
    };
    let root = PathBuf::from(implexity_core::pyobj::py_str(root)).join("design_states");
    let mut fields = BTreeMap::new();
    let mut coordinates = Map::new();
    for (i, (name, value)) in design.iter().enumerate() {
        let slot = format!("coordinate_{i:03}");
        fields.insert(slot.clone(), NpyArray::from_f64(value));
        coordinates.insert(name.to_string(), Value::String(slot));
    }
    let mut identities = Map::new();
    identities.insert("design_state_id".into(), Value::String(identity.into()));
    identities
        .insert("model_content_id".into(), Value::String(text_or_empty(request.get("model_content_id"))));
    let mut metadata = Map::new();
    metadata.insert("coordinates".into(), Value::Object(coordinates));
    Ok(Some(ResultArtifactStore::new(&root)?.create(&fields, &identities, &metadata)?))
}

fn text_or_empty(value: Option<&Value>) -> String {
    value.filter(|v| implexity_core::pyobj::truthy(v)).map(implexity_core::pyobj::py_str).unwrap_or_default()
}

fn analysis_shape(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    design: &NamedArrays,
) -> CaeResult<Vec<usize>> {
    if let Some(intent) = provider.as_any().downcast_ref::<IntentOrchestratedProvider>() {
        let document = problem_json(provider, problem)?;
        return intent.analysis_shape(&document, design);
    }
    match design.get("model:control") {
        Some(c) => Ok(c.shape().to_vec()),
        None => Err(CaeError::contract("'model:control'")),
    }
}

fn arrays_to_npy(arrays: &BTreeMap<String, ArrayD<f64>>) -> BTreeMap<String, NpyArray> {
    arrays.iter().map(|(k, v)| (k.clone(), NpyArray::from_f64(v))).collect()
}

fn responses_map(values: &BTreeMap<String, f64>) -> Value {
    Value::Object(values.iter().map(|(k, v)| (k.clone(), Value::from(*v))).collect())
}

fn evaluate_in_effort_scope(
    request: &Map<String, Value>,
    prepared: &Prepared,
    problem: &ProviderProblem,
    timing: Option<&mut WorkerExecutionTiming>,
) -> JobResult<Map<String, Value>> {
    let provider = prepared.provider.as_ref();
    let design = &prepared.design;
    let identity = &prepared.identity;
    validate_handoff_lifecycle(request, provider, problem)?;
    let consumed = consume_matching_time_guess(request, provider, problem, design, identity)?;
    let ops = design_operations(provider);
    let mut timing = timing;
    if let Some(t) = timing.as_deref_mut() {
        t.begin_provider_numerical()?;
    }

    let capture_identity = serde_json::json!({"design_state_id": identity});
    let result: Evaluation = implexity_runtime::dynamic_frames::capture::with_identity(
        capture_identity,
        || -> JobResult<Evaluation> {
            Ok(if let Some(o) = ops.filter(|o| o.provides(DesignOp::EvaluateResultsDesign)) {
                crate::solver_recovery::once("provider_evaluation", || o.evaluate_results_design(problem, design))?
            } else if let Some(o) = ops.filter(|o| o.provides(DesignOp::EvaluateDesign)) {
                crate::solver_recovery::once("provider_evaluation", || o.evaluate_design(problem, design, 0))?
            } else if design.names() == ["model:control"] {
                let control = design.get("model:control").cloned().unwrap_or_default();
                crate::solver_recovery::once("provider_evaluation", || provider.evaluate(problem, &control))?
            } else {
                return Err(JobError::contract(
                    "provider has no evaluate_design for the full authoritative state",
                ));
            })
        },
    )?;
    if let Some(t) = timing {
        t.end_provider_numerical()?;
    }
    if result.responses.values().any(|v| !v.is_finite()) {
        return Err(JobError::contract("provider returned nonfinite responses"));
    }

    if let Err(e) = implexity_runtime::dynamic_frames::capture::with_identity(
        serde_json::json!({"design_state_id": identity}),
        || crate::dynamic_capture::import_result_frames(&result, "evaluation"),
    ) {
        eprintln!("dynamic result capture skipped: {e}");
    }
    let shape = analysis_shape(provider, problem, design)?;
    let (arrays, field_meta) = cell_fields(&result.fields, &shape, result.diagnostics.get("field_metadata"))?;
    let mut registration = problem_registration(provider, problem, &shape)?;
    if registration.is_none() {
        let declared: Vec<&Value> = field_meta
            .values()
            .filter_map(|m| m.get("registration"))
            .filter(|r| implexity_core::pyobj::truthy(r))
            .collect();
        if let Some(first) = declared.first()
            && declared[1..].iter().all(|r| r == first)
        {
            registration = Some(registration_from_wire(first)?);
        }
    }
    let design_artifact = store_design(request, design, identity)?;
    let mut artifact = Value::Null;
    if !arrays.is_empty()
        && let Some(root) = request.get("artifact_root").filter(|v| implexity_core::pyobj::truthy(v))
    {
        let mut identities = Map::new();
        identities.insert("provider".into(), Value::String(text_or_empty(request.get("provider"))));
        identities
            .insert("model_content_id".into(), Value::String(text_or_empty(request.get("model_content_id"))));
        identities.insert("solve_id".into(), Value::String(text_or_empty(request.get("solve_id"))));
        identities.insert("design_state_id".into(), Value::String(identity.clone()));
        let mut metadata = Map::new();
        metadata.insert("field_registration".into(), registration.clone().unwrap_or(Value::Null));
        metadata.insert("fields".into(), Value::Object(field_meta.clone()));
        metadata.insert(
            "design_artifact_id".into(),
            design_artifact.as_ref().and_then(|d| d.get("artifact_id")).cloned().unwrap_or(Value::Null),
        );
        let store = ResultArtifactStore::new(Path::new(&implexity_core::pyobj::py_str(root)))?;
        artifact = Value::Object(store.create(&arrays_to_npy(&arrays), &identities, &metadata)?);
    }
    let mut reply = Map::new();
    reply.insert("kind".into(), Value::String("implicit_results".into()));
    reply.insert("schema".into(), Value::String("implexity-provider-results/2".into()));
    reply.insert("provider".into(), Value::String(text_or_empty(request.get("provider"))));
    reply.insert("responses".into(), responses_map(&result.responses));
    reply.insert("diagnostics".into(), Value::Object(result.diagnostics.clone()));
    reply.insert("fields".into(), Value::Object(field_meta));
    reply.insert("field_registration".into(), registration.unwrap_or(Value::Null));
    reply.insert("artifact".into(), artifact);
    reply.insert("design_state_id".into(), Value::String(identity.clone()));
    reply.insert("design_coordinates".into(), serde_json::json!(design.names()));
    reply.insert("design_artifact".into(), design_artifact.map_or(Value::Null, Value::Object));
    if let Some(c) = consumed {
        reply.insert("matching_time_guess_consumed".into(), Value::Object(c));
    }
    if let Some(p) = produce_matching_time_guess(request, provider, problem, design, identity)? {
        reply.insert("matching_time_guess".into(), Value::Object(p));
    }
    Ok(reply)
}


pub fn evaluate(
    request: &mut Map<String, Value>,
    timing: Option<&mut WorkerExecutionTiming>,
) -> JobResult<Map<String, Value>> {
    let prepared = prepare_request_identity(request)?;
    let _scope =
        provider_computation_effort_scope(prepared.provider.as_ref(), request.get("computation_effort"))?;
    let provider_request = prepare_provider_request(request, &prepared, false)?;
    evaluate_in_effort_scope(request, &prepared, &provider_request.problem, timing)
}


pub fn preflight(request: &mut Map<String, Value>) -> JobResult<Map<String, Value>> {
    let prepared = prepare_request_identity(request)?;
    let _scope =
        provider_computation_effort_scope(prepared.provider.as_ref(), request.get("computation_effort"))?;
    let admission_started = perf_counter_ns();
    let raw_problem = request
        .get("problem")
        .filter(|v| implexity_core::pyobj::truthy(v))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let effects =
        match design_operations(prepared.provider.as_ref()).and_then(|o| o.preflight_effects(&raw_problem)) {
            Some(v) => Some(v?),
            None => None,
        };
    let effects = checked_preflight_effects(effects.as_ref())?;
    let provider_request = prepare_provider_request(request, &prepared, true)?;
    let mut reply = provider_request.preflight.clone();
    reply.insert(
        "provider_admission".into(),
        provider_admission_report(perf_counter_ns() - admission_started, Some(&effects), true)?,
    );
    reply.insert("kind".into(), Value::String("implicit_provider_preflight".into()));
    reply.insert("schema".into(), Value::String("implexity-provider-preflight/1".into()));
    reply.insert("provider".into(), Value::String(text_or_empty(request.get("provider"))));
    reply.insert("design_state_id".into(), Value::String(prepared.identity.clone()));
    reply.insert("design_coordinates".into(), serde_json::json!(prepared.design.names()));
    reply.insert("couplingReport".into(), provider_request.coupling);
    Ok(reply)
}


pub fn sensitivity_response_names(request: &Map<String, Value>) -> CaeResult<(Vec<String>, bool)> {
    if !request.contains_key("responses") {
        return match request.get("response").and_then(Value::as_str) {
            Some(r) if !r.trim().is_empty() => Ok((vec![r.to_string()], false)),
            _ => contract("provider sensitivity needs a nonempty response name"),
        };
    }
    if request.get("response").is_some_and(|v| !v.is_null()) {
        return contract("provider sensitivity accepts response or responses, not both");
    }
    let Some(raw) = request.get("responses").and_then(Value::as_array).filter(|a| !a.is_empty()) else {
        return contract("provider sensitivity responses must be a nonempty array");
    };
    let names: Vec<String> = raw.iter().filter_map(Value::as_str).map(str::to_string).collect();
    if names.len() != raw.len() || names.iter().any(|n| n.trim().is_empty()) {
        return contract("provider sensitivity response names must be nonempty strings");
    }
    let unique: std::collections::BTreeSet<&String> = names.iter().collect();
    if unique.len() != names.len() {
        return contract("provider sensitivity response names must be unique");
    }
    Ok((names, true))
}

fn l2(a: &ArrayD<f64>) -> f64 {
    let s: f64 = a.iter().map(|v| v * v).sum();
    s.sqrt()
}

#[allow(clippy::too_many_lines)]
fn sensitivity_in_effort_scope(
    request: &Map<String, Value>,
    prepared: &Prepared,
    problem: &ProviderProblem,
    timing: Option<&mut WorkerExecutionTiming>,
) -> JobResult<Map<String, Value>> {
    let provider = prepared.provider.as_ref();
    let design = &prepared.design;
    let identity = &prepared.identity;
    validate_handoff_lifecycle(request, provider, problem)?;
    let consumed = consume_matching_time_guess(request, provider, problem, design, identity)?;
    let (responses, batched) = sensitivity_response_names(request)?;
    let mut timing = timing;
    if let Some(t) = timing.as_deref_mut() {
        t.begin_provider_numerical()?;
    }
    let ops = design_operations(provider);
    let named = ops.is_some_and(|o| {
        o.provides(DesignOp::SensitivityDesign) || o.provides(DesignOp::SensitivitiesDesign)
    });
    let (values, gradients_by_response, diagnostics): (
        BTreeMap<String, f64>,
        BTreeMap<String, NamedArrays>,
        Map<String, Value>,
    ) = if named {
        let out =
            crate::solver_recovery::once("provider_sensitivity", || implexity_optim::native_design::batch_sensitivities(provider, problem, design, &responses, 0))?;
        (out.responses, out.gradients, out.diagnostics)
    } else if design.names() == ["model:control"] && !batched {
        let response = &responses[0];
        let control = design.get("model:control").cloned().unwrap_or_default();
        let raw = crate::solver_recovery::once("provider_sensitivity", || provider.sensitivity(problem, &control, response))?;
        let mut values = BTreeMap::new();
        values.insert(response.clone(), raw.value);
        let mut gradients = BTreeMap::new();
        gradients.insert(response.clone(), NamedArrays::single("model:control", raw.gradient));
        (values, gradients, raw.diagnostics)
    } else {
        return Err(JobError::contract(if batched {
            "provider has no batched named-design sensitivity for the complete authoritative state"
        } else {
            "provider has no named-design sensitivity for the complete authoritative state"
        }));
    };
    if let Some(t) = timing {
        t.end_provider_numerical()?;
    }
    let layout = DesignLayout::from_values(design)?;
    let analysis = analysis_shape(provider, problem, design)?;
    let registration = problem_registration(provider, problem, &analysis)?;
    let coordinate_registrations =
        diagnostics.get("design_field_registrations").and_then(Value::as_object).cloned().unwrap_or_default();
    let component_layouts =
        diagnostics.get("design_field_layouts").and_then(Value::as_object).cloned().unwrap_or_default();
    let design_artifact = store_design(request, design, identity)?;
    let mut members = Map::new();
    for response in &responses {
        let value = values
            .get(response)
            .copied()
            .ok_or_else(|| CaeError::contract(implexity_core::py_repr::repr_str(response)))?;
        let gradients = gradients_by_response
            .get(response)
            .ok_or_else(|| CaeError::contract(implexity_core::py_repr::repr_str(response)))?;
        layout.pack(gradients, &format!("result-worker sensitivity {response}"))?;
        if !value.is_finite() {
            return Err(JobError::contract(format!(
                "provider returned nonfinite response {}",
                implexity_core::py_repr::repr_str(response)
            )));
        }
        let mut arrays: BTreeMap<String, ArrayD<f64>> = BTreeMap::new();
        let mut field_meta = Map::new();
        let mut summaries = Map::new();
        for (i, coordinate) in layout.names.iter().enumerate() {
            let gradient = gradients.get(coordinate).cloned().unwrap_or_default();
            let field_name = if coordinate == "model:control" {
                format!("{response}__dR_dmodel_control")
            } else {
                format!("{response}__dR_dcoordinate_{i:03}")
            };
            let reg = coordinate_registrations.get(coordinate).cloned().unwrap_or_else(|| {
                if coordinate == "model:control"
                    && gradient.ndim() == 3
                    && gradient.shape() == analysis.as_slice()
                {
                    registration.clone().unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            });
            let mut meta = Map::new();
            meta.insert("shape".into(), serde_json::json!(gradient.shape()));
            meta.insert("rank".into(), Value::String("scalar".into()));
            meta.insert("units".into(), Value::String(format!("response/{coordinate}")));
            meta.insert("signed".into(), Value::Bool(true));
            meta.insert("response".into(), Value::String(response.clone()));
            meta.insert("parameter".into(), Value::String(coordinate.clone()));
            meta.insert("source".into(), Value::String(if provider.capabilities()?.to_map()["traits"]["approximate"]==true { "model_derivative" } else { "exact_provider_adjoint" }.into()));
            meta.insert("registration".into(), reg);
            field_meta.insert(field_name.clone(), Value::Object(meta));
            let max_abs = gradient.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
            let mut summary = Map::new();
            summary.insert("shape".into(), serde_json::json!(gradient.shape()));
            summary.insert("l2".into(), Value::from(l2(&gradient)));
            summary.insert("max_abs".into(), Value::from(max_abs));
            summary.insert("field".into(), Value::String(field_name.clone()));
            arrays.insert(field_name.clone(), gradient.clone());
            if let Some(component_layout) = component_layouts.get(coordinate) {
                let names = implexity_solve::gradient_fields::add_registered_gradient_components(
                    &mut arrays,
                    &mut field_meta,
                    &field_name,
                    &gradient,
                    component_layout,
                    coordinate_registrations.get(coordinate),
                )?;
                summary.insert("component_fields".into(), serde_json::json!(names));
            }
            summaries.insert(coordinate.clone(), Value::Object(summary));
        }
        let mut artifact = Value::Null;
        if let Some(root) = request.get("artifact_root").filter(|v| implexity_core::pyobj::truthy(v)) {
            let mut identities = Map::new();
            identities.insert("provider".into(), Value::String(text_or_empty(request.get("provider"))));
            identities.insert(
                "model_content_id".into(),
                Value::String(text_or_empty(request.get("model_content_id"))),
            );
            identities.insert("solve_id".into(), Value::String(text_or_empty(request.get("solve_id"))));
            identities.insert("response".into(), Value::String(response.clone()));
            identities.insert("design_state_id".into(), Value::String(identity.clone()));
            let mut metadata = Map::new();
            metadata.insert("field_registration".into(), registration.clone().unwrap_or(Value::Null));
            metadata.insert("fields".into(), Value::Object(field_meta.clone()));
            metadata.insert(
                "design_artifact_id".into(),
                design_artifact.as_ref().and_then(|d| d.get("artifact_id")).cloned().unwrap_or(Value::Null),
            );
            let store = ResultArtifactStore::new(Path::new(&implexity_core::pyobj::py_str(root)))?;
            artifact = Value::Object(store.create(&arrays_to_npy(&arrays), &identities, &metadata)?);
        }
        let mut member = Map::new();
        member.insert("kind".into(), Value::String("implicit_sensitivity".into()));
        member.insert("schema".into(), Value::String("implexity-provider-sensitivity/2".into()));
        member.insert("provider".into(), Value::String(text_or_empty(request.get("provider"))));
        member.insert("topology_coordinate".into(), Value::String("model:control".into()));
        member.insert("response".into(), Value::String(response.clone()));
        member.insert("value".into(), Value::from(value));
        member.insert("diagnostics".into(), Value::Object(diagnostics.clone()));
        member.insert("responses".into(), serde_json::json!({response.clone(): {"value": value}}));
        member.insert("field_registration".into(), registration.clone().unwrap_or(Value::Null));
        member.insert("artifact".into(), artifact);
        member.insert("fields".into(), Value::Object(field_meta));
        member.insert("gradient_coordinates".into(), Value::Object(summaries));
        member.insert("design_state_id".into(), Value::String(identity.clone()));
        member.insert("design_coordinates".into(), serde_json::json!(design.names()));
        member.insert("design_artifact".into(), design_artifact.clone().map_or(Value::Null, Value::Object));
        if let Some(c) = &consumed {
            member.insert("matching_time_guess_consumed".into(), Value::Object(c.clone()));
        }
        members.insert(response.clone(), Value::Object(member));
    }
    if !batched {
        let Some(Value::Object(mut reply)) = members.shift_remove(&responses[0]) else {
            return Err(JobError::contract("provider sensitivity reply is missing"));
        };
        if let Some(p) = produce_matching_time_guess(request, provider, problem, design, identity)? {
            reply.insert("matching_time_guess".into(), Value::Object(p));
        }
        return Ok(reply);
    }
    let entrypoint = if ops.is_some_and(|o| o.provides(DesignOp::SensitivitiesDesign)) {
        "sensitivities_design"
    } else {
        "sensitivity_design_fallback"
    };
    let mut reply = Map::new();
    reply.insert("kind".into(), Value::String("implicit_sensitivities".into()));
    reply.insert("schema".into(), Value::String("implexity-provider-sensitivities/1".into()));
    reply.insert("provider".into(), Value::String(text_or_empty(request.get("provider"))));
    reply.insert("topology_coordinate".into(), Value::String("model:control".into()));
    reply.insert("response_order".into(), serde_json::json!(responses));
    reply.insert(
        "responses".into(),
        Value::Object(
            responses
                .iter()
                .map(|n| (n.clone(), Value::from(values.get(n).copied().unwrap_or(f64::NAN))))
                .collect(),
        ),
    );
    reply.insert("sensitivities".into(), Value::Object(members));
    reply.insert("diagnostics".into(), Value::Object(diagnostics));
    reply.insert("field_registration".into(), registration.unwrap_or(Value::Null));
    reply.insert("design_state_id".into(), Value::String(identity.clone()));
    reply.insert("design_coordinates".into(), serde_json::json!(design.names()));
    reply.insert("design_artifact".into(), design_artifact.map_or(Value::Null, Value::Object));
    reply.insert(
        "batch_execution".into(),
        serde_json::json!({"worker_invocations": 1, "provider_batch_entrypoint": entrypoint, "response_count": responses.len()}),
    );
    if let Some(c) = consumed {
        reply.insert("matching_time_guess_consumed".into(), Value::Object(c));
    }
    if let Some(p) = produce_matching_time_guess(request, provider, problem, design, identity)? {
        reply.insert("matching_time_guess".into(), Value::Object(p));
    }
    Ok(reply)
}


pub fn sensitivity(
    request: &mut Map<String, Value>,
    timing: Option<&mut WorkerExecutionTiming>,
) -> JobResult<Map<String, Value>> {
    let prepared = prepare_request_identity(request)?;
    let _scope =
        provider_computation_effort_scope(prepared.provider.as_ref(), request.get("computation_effort"))?;
    let provider_request = prepare_provider_request(request, &prepared, true)?;
    sensitivity_in_effort_scope(request, &prepared, &provider_request.problem, timing)
}


pub fn execute(request: &Value, mode: &str) -> JobResult<Map<String, Value>> {
    let Some(request) = request.as_object() else {
        return Err(JobError::contract("worker request must be an object"));
    };
    let mut request = request.clone();
    let mut unknown: Vec<&String> =
        request.keys().filter(|k| !INTERNAL_REQUEST_KEYS.contains(&k.as_str())).collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(JobError::contract(format!(
            "worker request has unknown fields {}",
            implexity_core::pyobj::list_repr(&unknown)
        )));
    }
    if request.get("physics_snapshot").is_none_or(Value::is_null)
        || request.get("computation_effort").is_none_or(Value::is_null)
    {
        return Err(JobError::contract("worker request lacks server-bound physics/computation identity"));
    }
    if !matches!(mode, "preflight" | "evaluate" | "sensitivity") {
        return Err(JobError::contract("worker operation is unsupported"));
    }
    crate::solver_recovery::clear_notice();
    let mut timing = WorkerExecutionTiming::new(mode)?;
    let reply = match mode {
        "preflight" => preflight(&mut request)?,
        "evaluate" => evaluate(&mut request, Some(&mut timing))?,
        _ => sensitivity(&mut request, Some(&mut timing))?,
    };
    if reply.contains_key("execution_timing") {
        return Err(JobError::contract("worker result timing field is reserved"));
    }
    let mut reply = reply;
    if let Some(report) = crate::solver_recovery::latest_notice() {
        let report = crate::solver_recovery::validate(&report)?;
        if report.get("status").and_then(Value::as_str) == Some("recovered") { reply.insert("solver_recovery".into(), report); }
    }
    let execution_timing = timing.finish()?;
    reply.insert("execution_timing".into(), execution_timing.clone());
    validate_provider_admission(reply.get("provider_admission"), Some(&execution_timing))?;
    let binding = request.get("computation_effort").cloned().unwrap_or(Value::Null);
    Ok(attach_exact_effort_evidence(&reply, &binding, timing.started(), 1)?)
}

pub fn emit_line(prefix: &str, payload: &Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{prefix}{}", compact_text(payload));
    let _ = out.flush();
}

#[must_use]
pub fn error_payload(error: &JobError) -> Value {
    let mut payload = serde_json::json!({"error": error.describe(), "problems": [error.message()]});
    if let Some(report) = error.solver_recovery() { payload["solver_recovery"] = report.clone(); }
    payload
}

#[must_use]
pub fn main(args: &[String]) -> i32 {
    let mut request_file = None;
    let mut mode = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--request-file" => request_file = it.next().cloned(),
            "--mode" => mode = it.next().cloned(),
            other => {
                emit_line(
                    "ERROR ",
                    &error_payload(&JobError::value(format!("unrecognized arguments: {other}"))),
                );
                return 2;
            }
        }
    }
    let (Some(request_file), Some(mode)) =
        (request_file, mode.filter(|m| matches!(m.as_str(), "preflight" | "evaluate" | "sensitivity")))
    else {
        emit_line(
            "ERROR ",
            &error_payload(&JobError::value("the following arguments are required: --request-file, --mode")),
        );
        return 2;
    };
    let outcome = (|| -> JobResult<Map<String, Value>> {
        let text = std::fs::read_to_string(&request_file)?;
        let request: Value =
            serde_json::from_str(&text).map_err(|e| JobError::of("JSONDecodeError", e.to_string()))?;
        execute(&request, &mode)
    })();
    match outcome {
        Ok(reply) => {
            let prefix = match mode.as_str() {
                "preflight" => "PREFLIGHT ",
                "evaluate" => "RESULTS ",
                _ => "SENSITIVITY ",
            };
            emit_line(prefix, &Value::Object(reply));
            0
        }
        Err(e) => {
            emit_line("ERROR ", &error_payload(&e));
            2
        }
    }
}

