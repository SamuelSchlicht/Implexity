// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use implexity_core::contracts::{CaeProvider, ProviderCapabilities, ProviderProblem, ResponseSpec};
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::{list_repr, py_str, truthy};
use implexity_geometry::document::{Binding, Model};
use implexity_geometry::{NodeRef, ParamRef};
use implexity_optim::coordinate_bounds::{Bound, array_bounds, coordinate_bounds, same_bounds};
use implexity_optim::numeric::{array_to_value, bool_array_to_value, float_value};
use implexity_optim::provider_ops::design_operations;
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use super::job::{DerivedHook, JobMeta, num_array};
use super::{ModelOptimizeManager, param_to_array};
use crate::error::{JobError, JobResult};
use crate::private::{canonical_text, sha256_hex};

pub(crate) fn int_list(items: &[usize]) -> String {
    format!("[{}]", items.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))
}

pub(crate) fn int_tuple(items: &[usize]) -> String {
    match items {
        [one] => format!("({one},)"),
        _ => format!("({})", items.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}

#[derive(Debug, Clone)]
pub struct Declaration {
    pub js: Map<String, Value>,
    pub meta: JobMeta,
}

fn opt<T>(problems: Vec<String>) -> JobResult<T> {
    Err(JobError::optimize(problems))
}

fn opt1<T>(problem: impl Into<String>) -> JobResult<T> {
    Err(JobError::optimize1(problem))
}

fn obj(v: Option<&Value>) -> Map<String, Value> {
    v.and_then(Value::as_object).cloned().unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Owner {
    Array(String),
    Node(usize, String),
}

#[derive(Clone)]
struct DesignRow {
    coordinate: String,
    reference: String,
    lower: Bound,
    upper: Bound,
    step_scale: f64,
    value: ArrayD<f64>,
    node: NodeRef,
    paramref: ParamRef,
    binding: Option<Binding>,
}

fn binding_array_key(binding: Option<&Binding>) -> Option<String> {
    match binding {
        Some(Binding::Array { key }) => Some(key.clone()),
        _ => None,
    }
}

fn owner_of(node: &NodeRef, param: &str, binding: Option<&Binding>) -> Owner {
    match binding_array_key(binding) {
        Some(k) => Owner::Array(k),
        None => Owner::Node(Arc::as_ptr(node) as usize, param.to_string()),
    }
}

fn eq_node(a: &NodeRef, b: &NodeRef) -> bool {
    Arc::ptr_eq(a, b)
}

#[must_use]
pub fn provider_name(req: &Value) -> Option<String> {
    let r = req.as_object()?;
    let cae = obj(r.get("cae"));
    let physics = obj(r.get("physics"));
    let cphysics = obj(cae.get("physics"));
    let name = r
        .get("provider")
        .filter(|v| truthy(v))
        .or_else(|| physics.get("provider").filter(|v| truthy(v)))
        .or_else(|| cphysics.get("provider").filter(|v| truthy(v)))?;
    Some(py_str(name).trim().to_string())
}


pub fn reject_misplaced_computation_effort(req: &Value) -> JobResult<()> {
    let Some(r) = req.as_object() else { return Ok(()) };
    let internal = [
        "effective_computation_effort",
        "computation_effort_binding",
        "requested_policy_digest",
        "effective_effort_digest",
        "normalized_profile_digest",
        "provider_profile",
        "provider_profile_id",
        "provider_registry_generation",
        "provider_registry_fingerprint",
        "operation_context",
        "authority_handle",
        "truth_envelope",
        "truth_status",
    ];
    let misplaced = [
        "hard_budgets",
        "wall_time_budget_s",
        "memory_budget_bytes",
        "target_update_rate_hz",
        "error_limits",
        "max_response_error",
        "max_state_error",
        "max_gradient_error",
        "trust_radius",
        "exact_correction",
        "exact_correction_cadence",
        "exact_correction_deadline_s",
        "ood_policy",
    ];
    let mut bad: Vec<String> =
        internal.iter().filter(|k| r.contains_key(**k)).map(|k| (*k).to_string()).collect();
    bad.sort();
    let mut top: Vec<String> =
        misplaced.iter().filter(|k| r.contains_key(**k)).map(|k| (*k).to_string()).collect();
    top.sort();
    bad.extend(top);
    fn walk(value: &Value, path: &str, internal: &[&str], bad: &mut Vec<String>) {
        match value {
            Value::Object(m) => {
                for (key, child) in m {
                    if internal.contains(&key.as_str()) || key == "computation_effort" {
                        bad.push(format!("{path}.{key}"));
                    }
                    walk(child, &format!("{path}.{key}"), internal, bad);
                }
            }
            Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{i}]"), internal, bad);
                }
            }
            _ => {}
        }
    }
    for root in ["settings", "problem", "physics", "cae", "schedule"] {
        if let Some(v) = r.get(root) {
            walk(v, root, &internal, &mut bad);
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    let unique: BTreeSet<String> = bad.into_iter().collect();
    let sorted: Vec<String> = unique.into_iter().collect();
    opt1(format!(
        "computation effort fields are misplaced or server-owned: {}; user controls belong only in top-level computation_effort",
        list_repr(&sorted)
    ))
}

#[must_use]
pub fn matching_time_job_declaration(internal: Option<&Value>) -> Option<Value> {
    let internal = internal?.as_object()?;
    let mut out = Map::new();
    if let Some(consume) = internal.get("consume").and_then(Value::as_object) {
        out.insert(
            "consume".into(),
            json!({
                "capsule_id": consume.get("capsule_id").cloned().unwrap_or(Value::Null),
                "required": consume.get("required").cloned().unwrap_or(Value::Bool(true)),
            }),
        );
    }
    if let Some(produce) = internal.get("produce").and_then(Value::as_object) {
        out.insert(
            "produce".into(),
            json!({"require_accepted": produce.get("require_accepted").cloned().unwrap_or(Value::Bool(true))}),
        );
    }
    if out.is_empty() { None } else { Some(Value::Object(out)) }
}


pub fn provider_ref_value(child: &NodeRef, reference: &str) -> JobResult<(ArrayD<f64>, NodeRef, ParamRef)> {
    let pr = ParamRef::parse(reference)
        .map_err(|e| JobError::optimize1(format!("topology binding {}: {e}", repr_str(reference))))?;
    if pr.path.first().map(String::as_str) != Some("model") {
        return opt1(format!(
            "topology binding {} must be rooted at model/, for example model/topology:samples",
            repr_str(reference)
        ));
    }
    let node = child.at(&pr.path[1..]).map_err(|e| {
        JobError::optimize1(format!("topology binding {} does not resolve: {e}", repr_str(reference)))
    })?;
    if node.info().param(&pr.name).is_none() {
        return opt1(format!(
            "topology binding {} names no parameter {} on {}",
            repr_str(reference),
            repr_str(&pr.name),
            node.kind()
        ));
    }
    let value = node.param(&pr.name).and_then(param_to_array).ok_or_else(|| {
        JobError::value(format!("topology binding {} is not a numeric array", repr_str(reference)))
    })?;
    Ok((value, node, pr))
}


pub fn validated_provider_derived_values(
    plan: &[Value],
    raw: &BTreeMap<String, ArrayD<f64>>,
    location: &str,
) -> JobResult<BTreeMap<String, ArrayD<f64>>> {
    let expected: Vec<String> = plan.iter().map(|r| r.get("ref").map(py_str).unwrap_or_default()).collect();
    let got: BTreeSet<&String> = raw.keys().collect();
    let want: BTreeSet<&String> = expected.iter().collect();
    if got != want || raw.len() != expected.len() {
        let mut keys: Vec<&String> = raw.keys().collect();
        keys.sort();
        return Err(JobError::value(format!(
            "{location} returned refs {}; expected exactly {}",
            list_repr(&keys),
            list_repr(&expected)
        )));
    }
    let mut out = BTreeMap::new();
    for row in plan {
        let reference = row.get("ref").map(py_str).unwrap_or_default();
        let value = &raw[&reference];
        let shape: Vec<usize> = row
            .get("shape")
            .and_then(Value::as_array)
            .map(|s| {
                s.iter().filter_map(Value::as_u64).map(|v| usize::try_from(v).unwrap_or(usize::MAX)).collect()
            })
            .unwrap_or_default();
        if value.shape() != shape.as_slice() || value.iter().any(|v| !v.is_finite()) {
            return Err(JobError::value(format!(
                "{location} output {} must be finite, real, and retain shape {}",
                repr_str(&reference),
                int_list(&shape)
            )));
        }
        out.insert(reference, value.clone());
    }
    Ok(out)
}

fn same_32eps(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            let scale = x.abs().max(y.abs()).max(f64::MIN_POSITIVE);
            (x - y).abs() <= 32.0 * f64::EPSILON * scale
        })
}

impl ModelOptimizeManager {

    #[allow(clippy::too_many_lines)]
    pub(crate) fn resolve_spatial_design_mask(
        &self,
        model: &Model,
        raw: &Map<String, Value>,
        coordinate: &str,
    ) -> JobResult<Option<(ArrayD<bool>, Map<String, Value>)>> {
        let singular = raw.get("designable_selection_id").filter(|v| !v.is_null());
        let plural = raw.get("designable_selection_ids").filter(|v| !v.is_null());
        if singular.is_some() && plural.is_some() {
            return opt1(format!(
                "{coordinate} coordinate mask supplies both singular and plural saved-region IDs"
            ));
        }
        let ids: Value = match (plural, singular) {
            (Some(p), _) => p.clone(),
            (None, Some(s)) => Value::Array(vec![s.clone()]),
            (None, None) => return Ok(None),
        };
        if raw.contains_key("designable") || raw.get("designable_ref").is_some_and(truthy) {
            return opt1(format!(
                "{coordinate} coordinate mask cannot combine inline/ref and saved-region authorities"
            ));
        }
        let Some(ids) = ids
            .as_array()
            .filter(|a| !a.is_empty() && a.iter().all(|v| v.as_str().is_some_and(|s| !s.trim().is_empty())))
        else {
            return opt1(format!("{coordinate} designable_selection_ids must be a nonempty list of IDs"));
        };
        let ids: Vec<String> = ids.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).collect();
        if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return opt1(format!("{coordinate} designable_selection_ids must be unique"));
        }
        let combine = raw.get("combine").map_or_else(|| "union".to_string(), py_str).trim().to_lowercase();
        if combine != "union" && combine != "intersection" {
            return opt1(format!("{coordinate} saved regions combine must be union or intersection"));
        }
        let document = model.to_doc()?;
        let mut field_payloads: BTreeMap<String, Value> = BTreeMap::new();
        let mut masks: Vec<(Vec<i64>, Vec<bool>)> = Vec::new();
        let mut registration_ids = Vec::new();
        let mut field_ids = Vec::new();
        for selection_id in &ids {
            let Some(persisted) =
                implexity_authoring::spatial_selection::find_spatial_selection(&document, Some(selection_id))
            else {
                return opt1(format!(
                    "{coordinate} references unknown saved region {}",
                    repr_str(selection_id)
                ));
            };
            let field_id = persisted.get("field_id").filter(|v| truthy(v)).map(py_str).unwrap_or_default();
            if field_id.is_empty() {
                return opt1(format!(
                    "{coordinate} saved region {} has no authoritative field",
                    repr_str(selection_id)
                ));
            }
            if !field_payloads.contains_key(&field_id) {
                let field =
                    self.inner.host.interaction_field(&json!({"field_id": field_id})).map_err(|e| {
                        JobError::optimize1(format!(
                            "{coordinate} saved region {} field validation: {e}",
                            repr_str(selection_id)
                        ))
                    })?;
                field_payloads.insert(field_id.clone(), field);
            }
            let field = &field_payloads[&field_id];
            let current = field.get("selections").and_then(Value::as_array).and_then(|s| {
                s.iter().find(|item| item.get("id").map(py_str).as_deref() == Some(selection_id.as_str()))
            });
            let Some(current) = current else {
                let stale = field
                    .get("stale_selection_ids")
                    .and_then(Value::as_array)
                    .is_some_and(|s| s.iter().any(|v| v.as_str() == Some(selection_id.as_str())));
                return opt1(format!(
                    "{coordinate} saved region {} is {} for the current model field",
                    repr_str(selection_id),
                    if stale { "stale" } else { "invalid" }
                ));
            };
            let shape: Vec<i64> = current
                .get("shape")
                .and_then(Value::as_array)
                .map(|s| s.iter().filter_map(Value::as_i64).collect())
                .unwrap_or_default();
            let mask =
                implexity_authoring::spatial_selection::decode_runs(current.get("selected_runs"), &shape)
                    .map_err(|e| {
                        JobError::optimize1(format!(
                            "{coordinate} saved region {} cannot be decoded: {e}",
                            repr_str(selection_id)
                        ))
                    })?;
            let identity = current.get("field_identity").cloned().unwrap_or(Value::Null);
            registration_ids
                .push(identity.get("registration_id").filter(|v| truthy(v)).map(py_str).unwrap_or_default());
            field_ids.push(field_id);
            masks.push((shape, mask));
        }
        let shapes: BTreeSet<&Vec<i64>> = masks.iter().map(|(s, _)| s).collect();
        let registrations: BTreeSet<&String> = registration_ids.iter().collect();
        if shapes.len() != 1 || registrations.len() != 1 {
            return opt1(format!("{coordinate} saved regions do not share one exact grid registration"));
        }
        let shape: Vec<usize> = masks[0].0.iter().map(|v| usize::try_from(*v).unwrap_or(0)).collect();
        let n = masks[0].1.len();
        let resolved: Vec<bool> = (0..n)
            .map(|i| {
                if combine == "union" {
                    masks.iter().any(|(_, m)| m[i])
                } else {
                    masks.iter().all(|(_, m)| m[i])
                }
            })
            .collect();
        let selected = resolved.iter().filter(|v| **v).count();
        let resolved =
            ArrayD::from_shape_vec(IxDyn(&shape), resolved).map_err(|e| JobError::value(e.to_string()))?;
        let mut unique_fields: Vec<String> =
            field_ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        unique_fields.sort();
        let mut source = Map::new();
        source.insert("kind".into(), json!("exact_saved_spatial_regions"));
        source.insert("selection_ids".into(), json!(ids));
        source.insert("combine".into(), json!(combine));
        source.insert("field_ids".into(), json!(unique_fields));
        source.insert("registration_id".into(), json!(registration_ids[0]));
        source.insert("selected_entries".into(), json!(selected));
        Ok(Some((resolved, source)))
    }


    #[allow(clippy::too_many_arguments)]
    pub(crate) fn validate_provider_topology_registration(
        provider: &dyn CaeProvider,
        problem: &ProviderProblem,
        model: &Model,
        child: &NodeRef,
        node: &NodeRef,
        parameter: &str,
        topology: &ArrayD<f64>,
    ) -> JobResult<Option<Value>> {
        let Some(contract) =
            crate::provider_hooks::with_hooks(provider, |h| h.required_topology_registration(problem))
        else {
            return Ok(None);
        };
        let reject = |m: &str| JobError::optimize1(format!("provider topology registration: {m}"));
        let contract = contract?;
        if contract.get("units").and_then(Value::as_str) != Some("mm") {
            return Err(reject("provider must declare its required model-world registration in mm"));
        }
        let required = implexity_geometry::field_registration::GridRegistration::from_wire(
            contract.get("registration").unwrap_or(&Value::Null),
        )
        .map_err(|e| reject(&format!("invalid provider registration: {e}")))?;
        if model.doc.get("units").map_or("mm".to_string(), py_str) != "mm" {
            return Err(reject("source model must explicitly use the supported mm coordinate convention"));
        }
        let root = model.root();
        if !root.is_some_and(|r| eq_node(&r, child)) || !eq_node(node, child) {
            return Err(reject(
                "nested or transformed topology ownership is unsupported; use an explicitly aligned root cell field",
            ));
        }
        if node.kind() != "cell_grid_field" || parameter != "samples" {
            return Err(reject(
                "direct topology requires cell-centred root cell_grid_field samples, not a node-centred or unregistered array",
            ));
        }
        let vec3 =
            |name: &str| node.param(name).and_then(param_to_array).filter(|a| a.len() == 3 && a.ndim() == 1);
        let (Some(origin), Some(spacing)) = (vec3("origin"), vec3("spacing")) else {
            return Err(reject("source origin and spacing must be finite mm vectors with positive spacing"));
        };
        if origin.iter().chain(spacing.iter()).any(|v| !v.is_finite()) || spacing.iter().any(|v| *v <= 0.0) {
            return Err(reject("source origin and spacing must be finite mm vectors with positive spacing"));
        }
        let shape = topology.shape();
        if shape.len() != 3 {
            return Err(reject(
                "source and analysis grids must share cell centering, shape, axis order and frame",
            ));
        }
        let o = [origin[0], origin[1], origin[2]];
        #[allow(clippy::cast_precision_loss)]
        let hi = [
            o[0] + spacing[0] * shape[0] as f64,
            o[1] + spacing[1] * shape[1] as f64,
            o[2] + spacing[2] * shape[2] as f64,
        ];
        let source = implexity_geometry::field_registration::axis_aligned_registration(
            [shape[0], shape[1], shape[2]],
            o,
            hi,
            "cell",
        )
        .map_err(|e| reject(&e.to_string()))?;
        if required.shape != source.shape
            || required.centering != source.centering
            || required.axis_order != source.axis_order
            || required.frame != source.frame
        {
            return Err(reject(
                "source and analysis grids must share cell centering, shape, axis order and frame",
            ));
        }
        let flat = |m: [[f64; 3]; 3]| m.iter().flatten().copied().collect::<Vec<f64>>();
        if !same_32eps(&source.origin, &required.origin)
            || !same_32eps(&flat(source.matrix()), &flat(required.matrix()))
        {
            return Err(reject(
                "source origin/spacing differs from the authored analysis domain; align the grids explicitly (no automatic resampling)",
            ));
        }
        Ok(Some(json!({
            "units": "mm", "source": source.to_wire(), "required": required.to_wire(),
            "mapping": "identical_cell_indices",
        })))
    }


    pub(crate) fn validate_provider_topology_semantics(
        provider: &dyn CaeProvider,
        problem: &ProviderProblem,
        model: &Model,
        child: &NodeRef,
        node: &NodeRef,
        parameter: &str,
    ) -> JobResult<Option<Value>> {
        let Some(contract) =
            crate::provider_hooks::with_hooks(provider, |h| h.required_topology_semantics(problem))
        else {
            return Ok(None);
        };
        let reject = |m: &str| JobError::optimize1(format!("provider topology semantics: {m}"));
        let contract = contract?;
        let isovalue = contract.get("isovalue").filter(|v| v.is_number()).and_then(Value::as_f64);
        let ok = contract.get("coordinate").and_then(Value::as_str) == Some("model:control")
            && matches!(contract.get("inside").and_then(Value::as_str), Some("greater" | "less"))
            && isovalue.is_some_and(f64::is_finite);
        if !ok {
            return Err(reject("provider must declare a finite scalar isovalue and inside polarity"));
        }
        let root = model.root();
        if !root.is_some_and(|r| eq_node(&r, child))
            || !eq_node(node, child)
            || node.kind() != "cell_grid_field"
            || parameter != "samples"
        {
            return Err(reject("only an untransformed root cell_grid_field samples mapping is supported"));
        }
        let scalar = |name: &str| node.param(name).and_then(ParamValueExt::scalar);
        let (Some(scale), Some(offset)) = (scalar("scale"), scalar("offset")) else {
            return Err(reject("source scalar-field scale and offset must be finite"));
        };
        #[allow(clippy::float_cmp)]
        if !scale.is_finite() || !offset.is_finite() || scale == 0.0 {
            return Err(reject(
                "source scalar-field scale must be finite and nonzero; offset must be finite",
            ));
        }
        let source_inside = if scale < 0.0 { "greater" } else { "less" };
        let threshold = offset / scale;
        let expected = isovalue.unwrap_or(f64::NAN);
        let tolerance = 32.0 * f64::EPSILON * threshold.abs().max(expected.abs()).max(f64::MIN_POSITIVE);
        if Some(source_inside) != contract.get("inside").and_then(Value::as_str)
            || !threshold.is_finite()
            || (threshold - expected).abs() > tolerance
        {
            return Err(reject(
                "rendered inside polarity/isovalue disagrees with the provider's direct sample semantics; no automatic inversion or threshold change",
            ));
        }
        Ok(Some(contract))
    }


    pub(crate) fn infer_topology_binding(model: &Model, child: &NodeRef) -> JobResult<Map<String, Value>> {
        let mut candidates = Vec::new();
        for (path, node) in child.walk() {
            let nid = model.id_of(&node);
            for pname in ["samples", "control", "occupancy", "density"] {
                if node.info().param(pname).is_none() {
                    continue;
                }
                let bound = nid.as_ref().and_then(|id| model.bindings().get(id)).and_then(|b| b.get(pname));
                if bound.is_some_and(|b| !matches!(b, Binding::Array { .. })) {
                    continue;
                }
                let Some(arr) = node.param(pname).and_then(param_to_array) else { continue };
                if arr.ndim() != 3 || arr.shape().iter().any(|n| *n < 2) || arr.iter().any(|v| !v.is_finite())
                {
                    continue;
                }
                let lo = arr.iter().copied().fold(f64::INFINITY, f64::min);
                let hi = arr.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                if lo < -1e-9 || hi > 1.0 + 1e-9 {
                    continue;
                }
                let mut p = vec!["model".to_string()];
                p.extend(path.iter().cloned());
                let mut row = Map::new();
                row.insert("ref".into(), json!(ParamRef::new(p, pname).as_str()));
                row.insert("node".into(), nid.clone().map_or(Value::Null, Value::String));
                row.insert("param".into(), json!(pname));
                row.insert("shape".into(), json!(arr.shape()));
                row.insert("kind".into(), json!(node.kind()));
                candidates.push(row);
            }
        }
        match candidates.len() {
            1 => Ok(candidates.remove(0)),
            0 => opt1(
                "provider topology optimisation requires an authoritative model:control binding, but this model has no unambiguous three-dimensional occupancy field. Author one in the GUI or set meta.implexity.topology.ref to the field parameter that manual brushes/cages edit.",
            ),
            _ => opt1(format!(
                "provider topology optimisation found several possible occupancy fields ({}); choosing one silently would detach optimisation from manual modelling. Select the authoritative model:control field in the GUI.",
                candidates
                    .iter()
                    .map(|c| c.get("ref").map(py_str).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }


    pub(crate) fn provider_current_design(&self, meta: &JobMeta) -> JobResult<BTreeMap<String, ArrayD<f64>>> {
        let model = self.inner.models.require()?;
        let child = model.node(&meta.s("node"))?;
        let mut design = BTreeMap::new();
        let mut order = Vec::new();
        for row in meta.get("plan").as_array().cloned().unwrap_or_default() {
            let name = row.get("ref").map(py_str).unwrap_or_default();
            let reference = row.get("source_ref").filter(|v| truthy(v)).map(py_str);
            let Some(reference) = reference.filter(|_| !name.is_empty()) else {
                return opt1("native provider design binding lacks coordinate/source_ref");
            };
            if design.contains_key(&name) {
                return opt1(format!("duplicate authoritative design coordinate {name}"));
            }
            let (arr, _node, _pr) = provider_ref_value(&child, &reference)?;
            order.push(name.clone());
            design.insert(name, arr);
        }
        let check = || -> Result<(), String> {
            if !design.contains_key("model:control") {
                return Err("mandatory model:control is missing".into());
            }
            let free = meta.get("free").as_array().cloned().unwrap_or_default();
            let expected: BTreeMap<String, Vec<u64>> = free
                .iter()
                .map(|r| {
                    (
                        r.get("ref").map(py_str).unwrap_or_default(),
                        r.get("shape")
                            .and_then(Value::as_array)
                            .map(|s| s.iter().filter_map(Value::as_u64).collect())
                            .unwrap_or_default(),
                    )
                })
                .collect();
            if expected.keys().collect::<BTreeSet<_>>() != design.keys().collect::<BTreeSet<_>>() {
                return Err("live coordinate set differs from the authored job".into());
            }
            for name in &order {
                let shape: Vec<u64> = design[name].shape().iter().map(|v| *v as u64).collect();
                if shape != expected[name] {
                    return Err(format!("live coordinate changed its authored shape: {name}"));
                }
            }
            Ok(())
        };
        check().map_err(|e| JobError::optimize1(format!("authoritative design state: {e}")))?;
        Ok(design)
    }


    #[allow(clippy::too_many_lines)]
    pub(crate) fn declare_provider_job(
        &self,
        req: &Map<String, Value>,
        provider_name: &str,
    ) -> JobResult<Option<Declaration>> {
        let cae_request = obj(req.get("cae"));
        let removed = || -> implexity_core::CaeResult<()> {
            implexity_core::contracts::reject_removed_constraint_keys(
                &Value::Object(req.clone()),
                "optimization request",
            )?;
            implexity_core::contracts::reject_removed_constraint_keys(
                &Value::Object(cae_request.clone()),
                "optimization request 'cae' block",
            )?;
            if let Some(Value::Object(s)) = req.get("settings") {
                implexity_core::contracts::reject_removed_constraint_keys(
                    &Value::Object(s.clone()),
                    "optimization request settings",
                )?;
            }
            Ok(())
        };
        removed().map_err(|e| JobError::optimize1(e.message().to_string()))?;
        let raw_schedule =
            req.get("schedule").filter(|v| truthy(v)).or_else(|| cae_request.get("schedule")).cloned();
        implexity_runtime::provider_job_authority::require_single_provider_schedule(
            &Value::String(provider_name.into()),
            raw_schedule.as_ref(),
        )
        .map_err(|e| JobError::optimize1(format!("provider schedule authority: {}", e.message())))?;
        let epoch_fields = crate::epoch_capture::normalise_selection(req.get("epoch_fields"))?;
        let m = self.inner.models.require()?;
        let node_name = req
            .get("node")
            .filter(|v| truthy(v))
            .map_or_else(|| m.doc.get("root").map(py_str).unwrap_or_default(), py_str);
        let child = m.node(&node_name)?;
        let child_name = m.id_of(&child);
        let provider = implexity_core::registries::global().providers.get(provider_name)?;
        let caps = provider.capabilities()?;
        let descriptor = match &caps {
            ProviderCapabilities::Descriptor(d) => d.as_ref().clone(),
            ProviderCapabilities::Legacy(l) => {
                if l.execution != "array" {
                    return Ok(None);
                }
                l.base.clone()
            }
            ProviderCapabilities::Mapping(_) => {
                return opt1(format!(
                    "provider {} must return a typed ProviderDescriptor; got dict",
                    repr_str(provider_name)
                ));
            }
        };
        let cae = cae_request.clone();
        let physics = obj(req.get("physics"));
        let cphysics = obj(cae.get("physics"));
        let problem_raw = physics
            .get("problem")
            .filter(|v| !v.is_null())
            .or_else(|| cphysics.get("problem").filter(|v| !v.is_null()))
            .or_else(|| req.get("problem").filter(|v| !v.is_null()))
            .cloned();
        let Some(problem_raw) = problem_raw else {
            return opt1(format!("provider {} needs a physics.problem declaration", repr_str(provider_name)));
        };
        let problem_norm = provider.normalise_problem(&problem_raw).map_err(|e| {
            JobError::optimize1(format!("provider {} problem: {}", repr_str(provider_name), e.message()))
        })?;
        let problem_wire = implexity_core::wire::to_wire(&crate::provider_worker::problem_json(
            provider.as_ref(),
            &problem_norm,
        )?)?;

        let admitted =
            implexity_optim::admitted_responses(provider.as_ref(), &problem_norm, &descriptor.responses)
                .map_err(|e| JobError::optimize1(e.message().to_string()))?;

        let mut binding: Map<String, Value> =
            match req.get("topology").filter(|v| !v.is_null()).cloned().or_else(|| {
                m.doc
                    .get("meta")
                    .and_then(|x| x.get("implexity"))
                    .and_then(|x| x.get("topology"))
                    .filter(|v| !v.is_null())
                    .cloned()
            }) {
                Some(Value::String(s)) => {
                    let mut b = Map::new();
                    b.insert("ref".into(), Value::String(s));
                    b
                }
                Some(Value::Object(o)) => o,
                _ => Map::new(),
            };
        if !binding.get("ref").is_some_and(truthy) {
            binding = Self::infer_topology_binding(&m, &child)?;
        }
        let source_ref = binding.get("ref").map(py_str).unwrap_or_default();
        let (topology, topo_node, topo_pr) = provider_ref_value(&child, &source_ref)?;
        let pick =
            |a: &str, b: &str, d: Value| binding.get(a).or_else(|| binding.get(b)).cloned().unwrap_or(d);
        let (lo, hi) = coordinate_bounds(
            &topology,
            &pick("lower", "lo", json!(0.0)),
            &pick("upper", "hi", json!(1.0)),
            "model:control",
        )?;
        let declared_shape = crate::provider_hooks::with_hooks(provider.as_ref(), |h| {
            h.design_coordinate_shape(&problem_norm, "model:control")
        })
        .transpose()?
        .flatten();
        let valid_shape = match &declared_shape {
            Some(s) => topology.shape() == s.as_slice(),
            None => topology.ndim() == 3,
        };
        if !valid_shape || topology.iter().any(|v| !v.is_finite()) {
            return opt1(format!(
                "model:control {source_ref} must match its declared finite spatial layout {}, got {}",
                declared_shape
                    .as_ref()
                    .map_or_else(|| "three-dimensional occupancy".to_string(), |s| int_tuple(s)),
                int_list(topology.shape())
            ));
        }
        let has_model_hook = crate::provider_hooks::with_hooks(provider.as_ref(), |h| {
            h.validate_design_model(&problem_norm, &topo_node, &topo_pr.name).map(|r| (r,))
        });
        if let Some((result,)) = has_model_hook {
            if declared_shape.is_some() && !eq_node(&topo_node, &child) {
                return opt1(
                    "mapped topology must own the authored root volume; nested transforms require an explicit geometry map",
                );
            }
            result?;
        }
        let (lo_b, hi_b) = (lo.broadcast(topology.shape()), hi.broadcast(topology.shape()));
        if topology
            .iter()
            .zip(lo_b.iter().zip(hi_b.iter()))
            .any(|(v, (l, h))| *v < l - 1e-12 || *v > h + 1e-12)
        {
            return opt1(format!("model:control {source_ref} contains values outside its declared bounds"));
        }
        let topo_nid = m.id_of(&topo_node);
        let topo_binding =
            topo_nid.as_ref().and_then(|id| m.bindings().get(id)).and_then(|b| b.get(&topo_pr.name)).cloned();
        if topo_binding.as_ref().is_some_and(|b| !matches!(b, Binding::Array { .. })) {
            return opt1(format!(
                "model:control {source_ref} is bound/derived; direct topology updates must write an authoritative spatial field"
            ));
        }
        Self::validate_provider_topology_registration(
            provider.as_ref(),
            &problem_norm,
            &m,
            &child,
            &topo_node,
            &topo_pr.name,
            &topology,
        )?;
        Self::validate_provider_topology_semantics(
            provider.as_ref(),
            &problem_norm,
            &m,
            &child,
            &topo_node,
            &topo_pr.name,
        )?;

        let mut design_rows = vec![DesignRow {
            coordinate: "model:control".into(),
            reference: source_ref.clone(),
            lower: lo.clone(),
            upper: hi.clone(),
            step_scale: 1.0,
            value: topology.clone(),
            node: Arc::clone(&topo_node),
            paramref: topo_pr.clone(),
            binding: topo_binding.clone(),
        }];
        let mut cap_coords: Vec<String> = if descriptor.design_coordinates.is_empty() {
            vec!["model:control".into()]
        } else {
            descriptor.design_coordinates.clone()
        };
        if let Some(source) =
            design_operations(provider.as_ref()).and_then(|o| o.authoring_design_coordinates(&problem_norm))
        {
            let source = source?;
            let unique: BTreeSet<&String> = source.iter().collect();
            if source.is_empty()
                || unique.len() != source.len()
                || source.iter().any(|c| !cap_coords.contains(c))
            {
                return opt1("provider returned invalid source-coordinate contract");
            }
            cap_coords = source;
        }
        if !cap_coords.iter().any(|c| c == "model:control") {
            design_rows.clear();
        }
        let mut raw_coords = req.get("design_coordinates").filter(|v| !v.is_null()).cloned();
        let mut explicit = raw_coords.is_some();
        if raw_coords.is_none() {
            raw_coords = cae.get("design_coordinates").filter(|v| !v.is_null()).cloned();
            explicit = raw_coords.is_some();
        }
        if raw_coords.is_none() && cap_coords.len() > 1 {
            let Some(defaults) =
                design_operations(provider.as_ref()).and_then(|o| o.default_design_bindings(&problem_norm))
            else {
                return opt1(format!(
                    "provider {} exposes {} design-coordinate families but has no default bindings; supply design_coordinates explicitly to limit the variable set instead of silently optimizing only model:control",
                    repr_str(provider_name),
                    cap_coords.len()
                ));
            };
            raw_coords = Some(defaults.map_err(|e| {
                JobError::optimize1(format!(
                    "provider {} default design bindings: {}",
                    repr_str(provider_name),
                    e.message()
                ))
            })?);
        }
        let raw_coord_rows: Vec<Value> = match &raw_coords {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) | None => Vec::new(),
            Some(other) => vec![other.clone()],
        };
        for raw in &raw_coord_rows {
            let row: Map<String, Value> = match raw {
                Value::Object(o) => o.clone(),
                other => {
                    let mut o = Map::new();
                    o.insert("coordinate".into(), Value::String(py_str(other)));
                    o
                }
            };
            let coord = row
                .get("coordinate")
                .filter(|v| truthy(v))
                .or_else(|| row.get("name").filter(|v| truthy(v)))
                .map(py_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if coord.is_empty() {
                continue;
            }
            let raw_step = row.get("step_scale").cloned().unwrap_or(json!(1.0));
            let Some(step_scale) =
                raw_step.as_f64().filter(|v| raw_step.is_number() && v.is_finite() && *v > 0.0)
            else {
                return opt1(format!(
                    "design coordinate {} step_scale must be positive and finite",
                    repr_str(&coord)
                ));
            };
            if coord == "model:control" {
                if !cap_coords.iter().any(|c| c == "model:control") {
                    return opt1("provider does not declare the implicit model:control coordinate");
                }
                let declared_ref = row
                    .get("ref")
                    .filter(|v| truthy(v))
                    .map_or_else(|| source_ref.clone(), py_str)
                    .trim()
                    .to_string();
                let low = row.get("lower").or_else(|| row.get("lo")).cloned().unwrap_or_else(|| lo.to_wire());
                let high =
                    row.get("upper").or_else(|| row.get("hi")).cloned().unwrap_or_else(|| hi.to_wire());
                let (dlo, dhi) = coordinate_bounds(&topology, &low, &high, &coord)?;
                if declared_ref != source_ref
                    || !same_bounds(topology.shape(), &dlo, &lo)
                    || !same_bounds(topology.shape(), &dhi, &hi)
                {
                    return opt1(
                        "model:control default binding must match the authoritative topology binding",
                    );
                }
                design_rows[0].step_scale = step_scale;
                continue;
            }
            let reference = row.get("ref").map(py_str).unwrap_or_default().trim().to_string();
            if reference.is_empty() {
                return opt1(format!(
                    "design coordinate {} requires an authoritative model parameter ref",
                    repr_str(&coord)
                ));
            }
            let (arr, dn, dpr) = provider_ref_value(&child, &reference)?;
            if arr.ndim() == 0 || arr.iter().any(|v| !v.is_finite()) {
                return opt1(format!(
                    "design coordinate {} ({reference}) must be a finite spatial/scalar array",
                    repr_str(&coord)
                ));
            }
            let low = row.get("lower").or_else(|| row.get("lo")).cloned().unwrap_or(json!(0.0));
            let high = row.get("upper").or_else(|| row.get("hi")).cloned().unwrap_or(json!(1.0));
            let (dlo, dhi) = coordinate_bounds(&arr, &low, &high, &coord)?;
            let (lb, hb) = (dlo.broadcast(arr.shape()), dhi.broadcast(arr.shape()));
            if arr.iter().zip(lb.iter().zip(hb.iter())).any(|(v, (l, h))| *v < l - 1e-12 || *v > h + 1e-12) {
                return opt1(format!(
                    "design coordinate {} contains values outside its declared bounds",
                    repr_str(&coord)
                ));
            }
            let dnid = m.id_of(&dn);
            let dbinding =
                dnid.as_ref().and_then(|id| m.bindings().get(id)).and_then(|b| b.get(&dpr.name)).cloned();
            if dbinding.as_ref().is_some_and(|b| !matches!(b, Binding::Array { .. })) {
                return opt1(format!(
                    "design coordinate {} is derived/bound and cannot be written directly",
                    repr_str(&coord)
                ));
            }
            design_rows.push(DesignRow {
                coordinate: coord,
                reference,
                lower: dlo,
                upper: dhi,
                step_scale,
                value: arr,
                node: dn,
                paramref: dpr,
                binding: dbinding,
            });
        }
        let mut owners: BTreeMap<Owner, String> = BTreeMap::new();
        for row in &design_rows {
            let owner = owner_of(&row.node, &row.paramref.name, row.binding.as_ref());
            if let Some(previous) = owners.get(&owner) {
                return opt1(format!(
                    "design coordinates {} and {} alias the same authoritative storage",
                    repr_str(previous),
                    repr_str(&row.coordinate)
                ));
            }
            owners.insert(owner, row.coordinate.clone());
        }
        let coord_names: Vec<String> = design_rows.iter().map(|r| r.coordinate.clone()).collect();
        if coord_names.iter().collect::<BTreeSet<_>>().len() != coord_names.len() {
            return opt1("design coordinate names must be unique");
        }
        let unsupported: Vec<&String> = coord_names.iter().filter(|c| !cap_coords.contains(c)).collect();
        if !unsupported.is_empty() {
            return opt1(format!(
                "provider {} does not advertise derivatives for design coordinate(s) {}",
                repr_str(provider_name),
                list_repr(&unsupported)
            ));
        }
        let inactive: Vec<String> = cap_coords.iter().filter(|c| !coord_names.contains(c)).cloned().collect();
        if !inactive.is_empty() && !explicit {
            return opt1(format!(
                "provider {} default design binding omitted coordinate(s) {}",
                repr_str(provider_name),
                list_repr(&inactive)
            ));
        }
        let mut variable_scope_warnings = Vec::new();
        if !inactive.is_empty() {
            variable_scope_warnings.push(Value::String(format!(
                "Explicit design-variable limitation: active {}/{} provider coordinate families; inactive {}",
                coord_names.len(),
                cap_coords.len(),
                inactive.join(", ")
            )));
        }
        let design_coordinate_selection = json!({
            "source": if explicit { "explicit_request" } else { "all_provider_declared" },
            "explicit": explicit,
            "provider_declared": cap_coords,
            "active": coord_names,
            "inactive": inactive,
        });

        let derived_enabled = crate::provider_hooks::with_hooks(provider.as_ref(), |h| Some(h.derived_model_hooks_enabled(&problem_norm))).unwrap_or(true);
        let has_refs = derived_enabled &&
            crate::provider_hooks::with_hooks(provider.as_ref(), |h| Some(h.has_derived_model_output_refs()))
                .unwrap_or(false);
        let has_updates = derived_enabled &&
            crate::provider_hooks::with_hooks(provider.as_ref(), |h| Some(h.has_derive_model_updates()))
                .unwrap_or(false);
        if has_refs != has_updates {
            return opt1(format!(
                "provider {} must implement both derived_model_output_refs and derive_model_updates, or neither",
                repr_str(provider_name)
            ));
        }
        let mut derived_plan: Vec<Value> = Vec::new();
        let mut derived_owners: Vec<Owner> = Vec::new();
        let mut derived_before_values = BTreeMap::new();
        let mut derived_design_base = BTreeMap::new();
        let mut derived_hook: Option<DerivedHook> = None;
        if has_updates {
            let refs_result = crate::provider_hooks::with_hooks(provider.as_ref(), |h| {
                h.derived_model_output_refs(&problem_norm)
            })
            .unwrap_or_else(|| {
                Err(implexity_core::CaeError::contract("provider derived_model_output_refs is unavailable"))
            });
            let mut complete: BTreeMap<String, ArrayD<f64>> =
                design_rows.iter().map(|r| (r.coordinate.clone(), r.value.clone())).collect();
            let missing: Vec<String> =
                cap_coords.iter().filter(|c| !complete.contains_key(*c)).cloned().collect();
            if !missing.is_empty() {
                let Some(defaults) = design_operations(provider.as_ref())
                    .and_then(|o| o.default_design_bindings(&problem_norm))
                else {
                    return opt1(format!(
                        "provider {} derives model outputs from a complete design but has no default bindings for inactive coordinate(s) {}",
                        repr_str(provider_name),
                        list_repr(&missing)
                    ));
                };
                let defaults = defaults.map_err(|e| {
                    JobError::optimize1(format!(
                        "provider {} derived-model input bindings: {}",
                        repr_str(provider_name),
                        e.message()
                    ))
                })?;
                let mut default_rows: BTreeMap<String, String> = BTreeMap::new();
                for raw in defaults.as_array().cloned().unwrap_or_default() {
                    let Some(r) = raw.as_object() else {
                        return opt1(format!(
                            "provider {} default design bindings must be objects",
                            repr_str(provider_name)
                        ));
                    };
                    let coordinate = r
                        .get("coordinate")
                        .filter(|v| truthy(v))
                        .or_else(|| r.get("name").filter(|v| truthy(v)))
                        .map(py_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let reference = r.get("ref").map(py_str).unwrap_or_default().trim().to_string();
                    if coordinate.is_empty() || reference.is_empty() || default_rows.contains_key(&coordinate)
                    {
                        return opt1(format!(
                            "provider {} has an invalid or duplicate default design binding for {}",
                            repr_str(provider_name),
                            repr_str(&coordinate)
                        ));
                    }
                    default_rows.insert(coordinate, reference);
                }
                for coordinate in &missing {
                    let Some(reference) = default_rows.get(coordinate) else {
                        return opt1(format!(
                            "provider {} cannot refresh derived model outputs: inactive coordinate {} has no model binding",
                            repr_str(provider_name),
                            repr_str(coordinate)
                        ));
                    };
                    let (value, _n, _p) = provider_ref_value(&child, reference).map_err(|e| {
                        JobError::optimize1(format!(
                            "provider {} derived-model input {}: {}",
                            repr_str(provider_name),
                            repr_str(coordinate),
                            e.message()
                        ))
                    })?;
                    complete.insert(coordinate.clone(), value);
                }
            }
            derived_design_base =
                cap_coords.iter().filter_map(|c| complete.get(c).map(|v| (c.clone(), v.clone()))).collect();
            let output_refs = refs_result.map_err(|e| {
                JobError::optimize1(format!(
                    "provider {} derived model output declaration: {}",
                    repr_str(provider_name),
                    e.message()
                ))
            })?;
            if output_refs.is_empty() {
                return opt1(format!(
                    "provider {} derived_model_output_refs must return a nonempty tuple/list",
                    repr_str(provider_name)
                ));
            }
            let output_refs: Vec<String> = output_refs.iter().map(|r| r.trim().to_string()).collect();
            if output_refs.iter().any(String::is_empty)
                || output_refs.iter().collect::<BTreeSet<_>>().len() != output_refs.len()
            {
                return opt1(format!(
                    "provider {} derived model output refs must be unique, nonempty strings",
                    repr_str(provider_name)
                ));
            }
            for reference in &output_refs {
                let (value, output_node, output_pr) = provider_ref_value(&child, reference).map_err(|e| {
                    JobError::optimize1(format!(
                        "provider {} derived model output {}: {}",
                        repr_str(provider_name),
                        repr_str(reference),
                        e.message()
                    ))
                })?;
                let output_nid = m.id_of(&output_node);
                let output_binding = output_nid
                    .as_ref()
                    .and_then(|id| m.bindings().get(id))
                    .and_then(|b| b.get(&output_pr.name))
                    .cloned();
                if output_binding.as_ref().is_some_and(|b| !matches!(b, Binding::Array { .. })) {
                    return opt1(format!(
                        "provider {} derived model output {} is bound to a non-array authority",
                        repr_str(provider_name),
                        repr_str(reference)
                    ));
                }
                let owner = owner_of(&output_node, &output_pr.name, output_binding.as_ref());
                if let Some(coordinate) = owners.get(&owner) {
                    return opt1(format!(
                        "provider {} derived model output {} aliases design coordinate {}",
                        repr_str(provider_name),
                        repr_str(reference),
                        repr_str(coordinate)
                    ));
                }
                if derived_owners.contains(&owner) {
                    return opt1(format!(
                        "provider {} derived model outputs alias the same authoritative storage",
                        repr_str(provider_name)
                    ));
                }
                derived_owners.push(owner);
                let output_units =
                    output_node.info().param(&output_pr.name).map(|p| p.units.clone()).unwrap_or_default();
                let storage_key = binding_array_key(output_binding.as_ref());
                let storage_file = storage_key.as_ref().and_then(|k| {
                    m.doc.get("arrays").and_then(|a| a.get(k)).and_then(|e| e.get("file")).cloned()
                });
                derived_plan.push(json!({
                    "kind": if output_binding.is_some() { "spatial_array" } else { "node_param" },
                    "ref": reference, "source_ref": reference,
                    "node": output_nid.map_or(Value::Null, Value::String),
                    "param": output_pr.name, "units": output_units, "node_units": output_units,
                    "parameter": Value::Null,
                    "array_key": storage_key.map_or(Value::Null, Value::String),
                    "array_file": storage_file.unwrap_or(Value::Null),
                    "shape": value.shape(),
                    "provider_derived": true,
                }));
                derived_before_values.insert(reference.clone(), value);
            }
            let provider_for_hook = Arc::clone(&provider);
            let problem_for_hook = problem_norm.clone();
            let hook = DerivedHook(Arc::new(move |design: &BTreeMap<String, ArrayD<f64>>| {
                crate::provider_hooks::with_hooks(provider_for_hook.as_ref(), |h| {
                    h.derive_model_updates(&problem_for_hook, design)
                })
                .unwrap_or_else(|| {
                    Err(implexity_core::CaeError::contract("provider derive_model_updates is unavailable"))
                })
                .map_err(JobError::from)
            }));
            let initial = (hook.0)(&derived_design_base).and_then(|updates| {
                validated_provider_derived_values(
                    &derived_plan,
                    &updates,
                    &format!("provider {} derive_model_updates", repr_str(provider_name)),
                )
            });
            if let Err(e) = initial {
                return opt1(format!(
                    "provider {} cannot derive model outputs: {}",
                    repr_str(provider_name),
                    e.message()
                ));
            }
            derived_hook = Some(hook);
        }

        let mut extra_names = Vec::new();
        for raw in req.get("free").and_then(Value::as_array).cloned().unwrap_or_default() {
            let name = match &raw {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => ["coordinate", "name", "parameter", "ref"]
                    .iter()
                    .find_map(|k| o.get(*k).filter(|v| truthy(v)))
                    .map(py_str),
                other => Some(py_str(other)),
            };
            if let Some(name) = name.filter(|n| !n.is_empty())
                && name != "model:control"
                && name != source_ref
                && !coord_names.contains(&name)
            {
                extra_names.push(name);
            }
        }
        if !extra_names.is_empty() {
            return opt1(format!(
                "provider free coordinates {} are not bound through design_coordinates",
                list_repr(&extra_names)
            ));
        }

        let mut response_raw = req.get("responses").filter(|v| !v.is_null()).cloned();
        if response_raw.is_none() {
            response_raw = cae.get("responses").filter(|v| !v.is_null()).cloned();
        }
        if response_raw.is_none() {
            response_raw = req.get("objective").filter(|v| !v.is_null()).cloned();
        }
        if let Some(Value::Object(o)) = &response_raw {
            response_raw = Some(
                o.get("terms")
                    .filter(|v| truthy(v))
                    .or_else(|| o.get("objectives").filter(|v| truthy(v)))
                    .cloned()
                    .unwrap_or_else(|| Value::Array(vec![Value::Object(o.clone())])),
            );
        }
        let normalization_raw = req.get("response_normalization").filter(|v| !v.is_null()).cloned();
        let mut responses: Vec<ResponseSpec> = Vec::new();
        let mut problems: Vec<String> = Vec::new();
        let rows: Vec<Value> = match &response_raw {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) | None => Vec::new(),
            Some(other) => vec![other.clone()],
        };
        for raw in rows {
            let mut row: Map<String, Value> = match raw {
                Value::Object(o) => o,
                other => {
                    let mut o = Map::new();
                    o.insert("name".into(), other);
                    o
                }
            };
            if normalization_raw.is_some() && row.contains_key("scale") {
                problems
                    .push("response_normalization cannot be combined with explicit response scales".into());
            }
            if row.contains_key("term")
                && !["name", "response", "response_id"].iter().any(|k| row.contains_key(*k))
                && let Some(t) = row.shift_remove("term")
            {
                row.insert("name".into(), t);
            }
            match ResponseSpec::from_dict(&Value::Object(row)) {
                Ok(spec) => {
                    if !admitted.contains(&spec.name) {
                        problems.push(format!(
                            "provider {} does not supply response {}; available: {}",
                            repr_str(provider_name),
                            repr_str(&spec.name),
                            list_repr(&admitted)
                        ));
                    }
                    responses.push(spec);
                }
                Err(e) => problems.push(e.message().to_string()),
            }
        }
        if responses.is_empty() {
            problems.push("provider topology optimisation requires at least one differentiable objective/constraint response".into());
        }
        let mut response_normalization = Value::Null;
        if problems.is_empty() {
            match implexity_optim::bounds::normalise_response_normalization(
                normalization_raw.as_ref(),
                &responses,
            ) {
                Ok(policy) => response_normalization = policy.map_or(Value::Null, |p| p.to_value()),
                Err(e) => problems.push(e.message().to_string()),
            }
        }
        if !problems.is_empty() {
            return opt(problems);
        }

        let mut masks = Map::new();
        for (role, key) in [
            ("fixed_solid", "fixed_solid_ref"),
            ("fixed_void", "fixed_void_ref"),
            ("preserve", "preserve_ref"),
            ("designable", "designable_ref"),
        ] {
            if let Some(reference) = binding.get(key).filter(|v| truthy(v)).map(py_str) {
                let (arr, _n, _p) = provider_ref_value(&child, &reference)?;
                if arr.shape() != topology.shape() {
                    return opt1(format!(
                        "{role} {reference} shape {} differs from model:control {}",
                        int_list(arr.shape()),
                        int_list(topology.shape())
                    ));
                }
                masks.insert(role.into(), array_to_value(&arr));
            }
        }

        let raw_coordinate_masks = req
            .get("coordinate_masks")
            .filter(|v| !v.is_null())
            .or_else(|| cae.get("coordinate_masks").filter(|v| !v.is_null()))
            .cloned();
        if raw_coordinate_masks.as_ref().is_some_and(|v| !v.is_object()) {
            return opt1("coordinate_masks must map design-coordinate IDs to mask declarations");
        }
        let mut coordinate_mask_sources = Map::new();
        let row_binding: BTreeMap<String, Option<Binding>> =
            design_rows.iter().map(|r| (r.coordinate.clone(), r.binding.clone())).collect();
        let mut resolve = |coord: &str, declaration: &Value| -> JobResult<Value> {
            let spec: Map<String, Value> = match declaration {
                Value::Object(o) => o.clone(),
                other => {
                    let mut o = Map::new();
                    o.insert("designable".into(), other.clone());
                    o
                }
            };
            if let Some((resolved, mut source)) = self.resolve_spatial_design_mask(&m, &spec, coord)? {
                let key = binding_array_key(row_binding.get(coord).and_then(Option::as_ref));
                let Some(key) = key else {
                    return opt1(format!(
                        "{coord} saved-region mask requires an authoritative registered spatial-array binding"
                    ));
                };
                let target =
                    self.inner.host.interaction_field(&json!({"field_id": key})).map_err(|e| {
                        JobError::optimize1(format!("{coord} coordinate-grid validation: {e}"))
                    })?;
                let target_registration = target
                    .get("identity")
                    .and_then(|i| i.get("registration_id"))
                    .filter(|v| truthy(v))
                    .map(py_str)
                    .unwrap_or_default();
                if target_registration.is_empty()
                    || Some(&Value::String(target_registration)) != source.get("registration_id")
                {
                    return opt1(format!(
                        "{coord} saved region is not registered on the coordinate's exact grid"
                    ));
                }
                source.insert("coordinate_field_id".into(), Value::String(key));
                coordinate_mask_sources.insert(coord.to_string(), Value::Object(source));
                return Ok(json!({"designable": bool_array_to_value(&resolved)}));
            }
            if let Some(reference) = spec.get("designable_ref").filter(|v| truthy(v)).map(py_str) {
                let (mask, _n, _p) = provider_ref_value(&child, &reference)?;
                coordinate_mask_sources.insert(
                    coord.to_string(),
                    json!({"kind": "authoritative_model_parameter", "ref": reference}),
                );
                return Ok(json!({"designable": array_to_value(&mask)}));
            }
            if let Some(d) = spec.get("designable") {
                coordinate_mask_sources.insert(coord.to_string(), json!({"kind": "inline_exact_mask"}));
                return Ok(json!({"designable": d}));
            }
            opt1(format!("{coord} coordinate mask has no designable mask, ref, or saved-region ID"))
        };
        let mut coordinate_masks = Map::new();
        for (coord, declaration) in
            raw_coordinate_masks.as_ref().and_then(Value::as_object).cloned().unwrap_or_default()
        {
            let resolved = resolve(&coord, &declaration)?;
            coordinate_masks.insert(coord, resolved);
        }
        let mask_keys =
            ["designable", "designable_ref", "designable_selection_id", "designable_selection_ids"];
        for raw in &raw_coord_rows {
            let Some(r) = raw.as_object() else { continue };
            let coord = r
                .get("coordinate")
                .filter(|v| truthy(v))
                .or_else(|| r.get("name").filter(|v| truthy(v)))
                .map(py_str)
                .unwrap_or_default();
            if !mask_keys.iter().any(|k| r.contains_key(*k)) {
                continue;
            }
            if coordinate_masks.contains_key(&coord) {
                return opt1(format!("{coord} has duplicate coordinate-mask declarations"));
            }
            let resolved = resolve(&coord, raw)?;
            coordinate_masks.insert(coord, resolved);
        }
        let mask_result = (|| -> JobResult<Map<String, Value>> {
            let designs = implexity_optim::NamedArrays::from_pairs(
                design_rows.iter().map(|r| (r.coordinate.clone(), r.value.clone())),
            );
            let validated = crate::hierarchical_job::coordinate_designable_masks(
                Some(&Value::Object(coordinate_masks.clone())),
                &designs,
            )?;
            let mut masks_out = Map::new();
            for (k, v) in &validated {
                masks_out.insert(k.clone(), json!({"designable": bool_array_to_value(v)}));
            }
            let rows: Vec<(String, Option<(String, String)>, Vec<usize>)> = design_rows
                .iter()
                .map(|r| {
                    (
                        r.coordinate.clone(),
                        binding_array_key(r.binding.as_ref()).map(|k| ("array".to_string(), k)),
                        r.value.shape().to_vec(),
                    )
                })
                .collect();
            implexity_authoring::geometry_holds::intersect_coordinate_masks(
                &m.doc,
                &rows,
                &mut masks_out,
                &mut coordinate_mask_sources,
            )?;
            Ok(masks_out)
        })();
        let coordinate_masks =
            mask_result.map_err(|e| JobError::optimize1(format!("native design masks: {}", e.message())))?;
        let raw_update_metric =
            req.get("update_metric").filter(|v| !v.is_null()).or_else(|| cae.get("update_metric")).cloned();
        let update_metric = crate::stage_search::normalise_update_metric(raw_update_metric.as_ref())
            .map_err(|e| JobError::optimize1(format!("native optimizer update metric: {}", e.message())))?;

        let preflight = (|| -> JobResult<Map<String, Value>> {
            if let Some(result) = crate::provider_hooks::with_hooks(provider.as_ref(), |h| {
                h.preflight_declaration(&problem_norm)
            }) {
                return Ok(result?);
            }
            Ok(provider.preflight(&problem_norm, Some(&topology))?)
        })();
        let mut pre = match preflight {
            Ok(p) => p,
            Err(e @ JobError::Problems { .. }) => return Err(e),
            Err(e) => {
                return opt1(format!("provider {} preflight: {}", repr_str(provider_name), e.message()));
            }
        };
        if !pre.get("ok").is_none_or(truthy) {
            let issues = pre
                .get("issues")
                .filter(|v| truthy(v))
                .or_else(|| pre.get("errors").filter(|v| truthy(v)))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let msgs: Vec<String> = issues
                .iter()
                .map(|i| match i {
                    Value::Object(o) => {
                        o.get("message").filter(|v| truthy(v)).map_or_else(|| py_str(i), py_str)
                    }
                    other => py_str(other),
                })
                .collect();
            return opt(if msgs.is_empty() {
                vec!["provider preflight refused model:control".into()]
            } else {
                msgs
            });
        }
        let coupling = implexity_core::coupling_graph::validate_provider_couplings(
            provider.as_ref(),
            Some(&problem_norm),
            &implexity_core::registries::global().extensions,
            true,
        );
        if !coupling.get("ok").is_some_and(truthy) {
            let msgs: Vec<String> = coupling
                .get("errors")
                .and_then(Value::as_array)
                .map(|e| {
                    e.iter()
                        .map(|i| i.get("message").filter(|v| truthy(v)).map_or_else(|| py_str(i), py_str))
                        .collect()
                })
                .unwrap_or_default();
            return opt(if msgs.is_empty() {
                vec!["automatic multiphysics coupling preflight refused optimization".into()]
            } else {
                msgs
            });
        }
        pre.insert("couplingReport".into(), coupling);
        if let Some(candidates) =
            pre.get("semanticRegionCandidates").filter(|v| truthy(v)).and_then(Value::as_object).cloned()
        {
            let region_rows: BTreeMap<String, Value> = problem_wire
                .get("regions")
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter(|x| x.get("id").is_some_and(truthy))
                        .map(|x| (py_str(&x["id"]), x.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let mut bindings = Map::new();
            for (rid, cands) in candidates {
                let Some(region) = region_rows.get(&rid) else { continue };
                let bound = implexity_geometry::semantic_regions::rebind_semantic_region(
                    region, &cands, 3.0, 1.15, 0.35,
                )
                .map_err(|e| JobError::optimize1(format!("semantic boundary/region tracking: {e}")))?;
                bindings.insert(rid, bound);
            }
            pre.insert("semanticRegionBindings".into(), Value::Object(bindings));
        }

        let nested = match req.get("settings") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(o)) => o.clone(),
            Some(_) => return opt1("provider optimization settings must be an object"),
        };
        let mut provider_settings_raw = nested.clone();
        let live_every = provider_settings_raw
            .shift_remove("live_every")
            .or_else(|| req.get("live_every").cloned())
            .unwrap_or(json!(1));
        if req.contains_key("live_every") && nested.contains_key("live_every") {
            return opt1("provider optimization request cannot declare live_every twice");
        }
        let provider_keys = [
            "iterations",
            "step_fraction",
            "minimum_step_fraction",
            "backtracking",
            "armijo",
            "move_limit",
            "topology_lower",
            "topology_upper",
            "step_growth",
            "stationarity_tolerance",
            "bound_tolerance",
            "penalty_growth",
            "penalty_limit",
            "violation_reduction",
            "multiplier_update_interval",
            "gradient_clip_norm",
            "iters",
            "lr",
        ];
        let mut sorted_keys = provider_keys.to_vec();
        sorted_keys.sort_unstable();
        for key in sorted_keys {
            if let Some(v) = req.get(key) {
                if provider_settings_raw.contains_key(key) {
                    return opt1(format!(
                        "provider optimization setting {} is declared twice",
                        repr_str(key)
                    ));
                }
                provider_settings_raw.insert(key.into(), v.clone());
            }
        }
        let parsed = crate::provider_job::settings(Some(&Value::Object(provider_settings_raw.clone())))
            .map_err(|e| JobError::optimize1(format!("provider optimization settings: {}", e.message())))?;
        let mut provider_settings = implexity_optim::optimizer::legacy_settings_value(&parsed)
            .as_object()
            .cloned()
            .unwrap_or_default();
        if array_bounds(&lo, &hi) {
            if ["topology_lower", "topology_upper"].iter().any(|k| provider_settings_raw.contains_key(*k)) {
                return opt1("array coordinate bounds cannot use legacy scalar topology bounds");
            }
        } else {
            for (key, expected) in [("topology_lower", &lo), ("topology_upper", &hi)] {
                let expected_value = expected.to_wire();
                if let Some(supplied) = provider_settings_raw.get(key).filter(|v| !v.is_null()) {
                    #[allow(clippy::float_cmp)]
                    if supplied.as_f64() != expected_value.as_f64() {
                        return opt1(format!("{key} must match the authoritative model:control binding"));
                    }
                }
                provider_settings.insert(key.into(), expected_value);
            }
        }
        let iters = parsed.settings.iterations;
        let lr = parsed.settings.step_fraction.as_f64();
        let Some(live_every) =
            live_every.as_i64().filter(|n| (live_every.is_i64() || live_every.is_u64()) && *n >= 1)
        else {
            return opt1("live_every must be an integer >= 1");
        };
        if req.get("steerable").is_some_and(|v| !v.is_null() && *v != Value::Bool(false)) {
            return opt1(
                "native provider jobs do not support in-place fixed-parameter steering; use pause, intervene, and branch_after_intervention with fresh preflight",
            );
        }
        let settings = json!({
            "iters": iters, "lr": float_value(lr), "band_h": 0.0, "eval_mode": "provider",
            "smooth_r": 0.0, "scaling": "unit_range", "steerable": false,
            "live_every": live_every, "momentum": 0.0, "model_units": "-",
            "provider_settings": provider_settings,
        });

        let mut plan = Vec::new();
        let mut free_desc = Vec::new();
        let mut coordinate_initial = BTreeMap::new();
        let mut coordinate_files = Map::new();
        for (idx, dr) in design_rows.iter().enumerate() {
            let dnid = m.id_of(&dr.node);
            let key = binding_array_key(dr.binding.as_ref());
            let file = key.as_ref().and_then(|k| {
                m.doc.get("arrays").and_then(|a| a.get(k)).and_then(|e| e.get("file")).cloned()
            });
            plan.push(json!({
                "kind": if dr.binding.is_some() { "spatial_array" } else { "node_param" },
                "ref": dr.coordinate, "source_ref": dr.reference,
                "node": dnid.map_or(Value::Null, Value::String), "param": dr.paramref.name,
                "units": "-", "node_units": "-", "parameter": Value::Null,
                "lo": dr.lower.to_wire(), "hi": dr.upper.to_wire(), "start": num_array(&dr.value),
                "step_scale": float_value(dr.step_scale),
                "start_document": Value::Null,
                "array_key": key.map_or(Value::Null, Value::String),
                "array_file": file.unwrap_or(Value::Null),
            }));
            free_desc.push(json!({
                "ref": dr.coordinate, "source_ref": dr.reference, "units": "-",
                "lo": dr.lower.to_wire(), "hi": dr.upper.to_wire(), "size": dr.value.len(),
                "shape": dr.value.shape(), "step_scale": float_value(dr.step_scale),
                "role": if dr.coordinate == "model:control" { "mandatory_topology_coordinate" } else { "provider_design_coordinate" },
            }));
            coordinate_initial.insert(dr.coordinate.clone(), dr.value.clone());
            coordinate_files.insert(
                dr.coordinate.clone(),
                json!(if dr.coordinate == "model:control" {
                    "topology_initial.npz".to_string()
                } else {
                    format!("design_{idx:02}.npz")
                }),
            );
        }
        let response_wire: Vec<Value> = responses.iter().map(ResponseSpec::to_value).collect();
        pre.insert(
            "native_runtime_contract".into(),
            json!({
                "schema": "implexity-native-runtime-contract/1",
                "pause_resume_replay": "exact_checkpoint_numerical_response_verification_v1",
                "in_place_problem_mutation": false,
                "problem_intervention": "public_checkpoint_branch_repreflight_v1",
            }),
        );
        pre.insert(
            "constraint_semantics".into(),
            json!({
                "search": "projected gradient with an augmented Lagrangian for every bounded response (per-bound multipliers and penalties)",
                "response_bounds": "soft_augmented_lagrangian_response_bounds",
                "convergence": "projected stationarity and every bound within bound_tolerance",
                "promotion": "best sealed update of the terminal stage that passes the authored final engineering screens: bound-feasible updates first by their unbounded objective; response bounds are reported, not gated",
                "local_limits_inferred_from_means": false,
                "default_intermediate_engineering_action": "warn",
                "engineering_acceptance_stage": "final_promotion",
                "solver_tolerances": "selected_provider_and_computation_effort_unchanged",
            }),
        );
        pre.insert(
            "epoch_field_capture".into(),
            json!({
                "requested": epoch_fields.clone().map_or(Value::Null, Value::Object),
                "source": "same_design_provider_cache_only",
                "extra_evaluations": false,
                "unavailable_capture_stops_optimization": false,
            }),
        );
        let initial_named = implexity_optim::NamedArrays::from_pairs(
            coordinate_initial.iter().map(|(k, v)| (k.clone(), v.clone())),
        );
        let mut digest_payload = Map::new();
        digest_payload.insert("provider".into(), json!(provider_name));
        digest_payload.insert("problem".into(), problem_wire.clone());
        digest_payload.insert("responses".into(), Value::Array(response_wire.clone()));
        digest_payload.insert("topology_ref".into(), json!(source_ref));
        digest_payload.insert("response_normalization".into(), response_normalization.clone());
        digest_payload.insert("structure_id".into(), json!(child.structure_id()));
        digest_payload.insert("content_id".into(), json!(child.content_id()));
        digest_payload.insert(
            "initial_design_state_id".into(),
            json!(implexity_optim::design_identity(&initial_named)?),
        );
        digest_payload.insert(
            "design_bindings".into(),
            Value::Array(
                design_rows
                    .iter()
                    .map(|r| {
                        json!({"coordinate": r.coordinate, "ref": r.reference, "lower": r.lower.to_wire(),
                               "upper": r.upper.to_wire(), "step_scale": float_value(r.step_scale)})
                    })
                    .collect(),
            ),
        );
        digest_payload.insert("coordinate_masks".into(), Value::Object(coordinate_masks.clone()));
        digest_payload
            .insert("coordinate_mask_sources".into(), Value::Object(coordinate_mask_sources.clone()));
        digest_payload.insert("update_metric".into(), Value::Object(update_metric.clone()));
        digest_payload.insert("topology_masks".into(), Value::Object(masks.clone()));
        if let Some(e) = &epoch_fields {
            digest_payload.insert("epoch_fields".into(), Value::Object(e.clone()));
        }
        let physics_snapshot = crate::effort::physics_snapshot()?;
        let scope_digest = sha256_hex(json_sorted(&Value::Object(digest_payload.clone())).as_bytes());
        let mut context = Map::new();
        context.insert("operation".into(), json!("provider_lifecycle"));
        context.insert("scope_digest".into(), json!(scope_digest));
        let effort_binding = implexity_runtime::provider_job_authority::make_server_effort_binding(
            req.get("computation_effort"),
            provider_name,
            &caps,
            &physics_snapshot,
            Some(&context),
            None,
        )
        .map_err(|e| JobError::optimize1(format!("provider computation effort: {}", e.message())))?;
        for key in [
            "requested_policy_digest",
            "effective_effort_digest",
            "coupling_approximation_digest",
            "operation_context_digest",
        ] {
            digest_payload.insert(key.into(), effort_binding.get(key).cloned().unwrap_or(Value::Null));
        }
        let solve_id = sha256_hex(json_sorted(&Value::Object(digest_payload)).as_bytes());
        let before_doc = m.to_doc()?;
        let hierarchy = if cae.is_empty() {
            req.get("design_freedom").cloned()
        } else {
            req.get("design_freedom").filter(|v| truthy(v)).or_else(|| cae.get("design_freedom")).cloned()
        };
        let schedule = req
            .get("schedule")
            .filter(|v| truthy(v))
            .or_else(|| cae.get("schedule"))
            .cloned()
            .unwrap_or(Value::Null);
        let js_coords: Vec<Value> = design_rows
            .iter()
            .map(|dr| {
                json!({"coordinate": dr.coordinate, "file": coordinate_files[&dr.coordinate],
                       "key": if dr.coordinate == "model:control" { "topology" } else { "value" },
                       "lower": dr.lower.to_wire(), "upper": dr.upper.to_wire(),
                       "step_scale": float_value(dr.step_scale)})
            })
            .collect();
        let mut js = Map::new();
        js.insert("schema".into(), json!("implexity-provider-job/1"));
        js.insert("provider".into(), json!(provider_name));
        js.insert("runtime_packages".into(), json!(implexity_core::packages::global().selected()));
        js.insert("physics_snapshot".into(), physics_snapshot.clone());
        js.insert("problem".into(), problem_wire.clone());
        js.insert("responses".into(), Value::Array(response_wire.clone()));
        js.insert("response_normalization".into(), response_normalization.clone());
        js.insert("settings".into(), settings["provider_settings"].clone());
        js.insert("masks".into(), Value::Object(masks));
        js.insert("coordinate_masks".into(), Value::Object(coordinate_masks));
        js.insert("coordinate_mask_sources".into(), Value::Object(coordinate_mask_sources.clone()));
        js.insert("update_metric".into(), Value::Object(update_metric.clone()));
        js.insert("solve_id".into(), json!(solve_id));
        js.insert("topology_coordinate".into(), json!("model:control"));
        js.insert("topology_source_ref".into(), json!(source_ref));
        js.insert("design_coordinate_selection".into(), design_coordinate_selection.clone());
        js.insert("design_coordinates".into(), Value::Array(js_coords));
        js.insert(
            "design_blocks".into(),
            hierarchy.as_ref().and_then(|h| h.get("blocks")).cloned().unwrap_or(Value::Null),
        );
        js.insert("schedule".into(), schedule);
        js.insert("document_content_id".into(), json!(child.content_id()));
        js.insert("computation_effort".into(), Value::Object(effort_binding.clone()));
        if let Some(e) = &epoch_fields {
            js.insert("epoch_fields".into(), Value::Object(e.clone()));
        }
        let mut warnings = variable_scope_warnings;
        for issue in pre.get("issues").and_then(Value::as_array).cloned().unwrap_or_default() {
            if issue.get("severity").and_then(Value::as_str) == Some("warning") {
                warnings.push(Value::String(issue.get("message").map_or_else(|| "None".into(), py_str)));
            }
        }
        let before_sha256 = sha256_hex(&implexity_geometry::document::canonical_bytes(&before_doc));
        let mut fields = Map::new();
        for (k, v) in [
            ("settings", settings),
            ("grid", json!(topology.shape())),
            ("node", child_name.map_or(Value::Null, Value::String)),
            ("model_kind", json!(child.kind())),
            ("case_name", json!(provider_name)),
            ("case_norm", json!({})),
            ("free", Value::Array(free_desc)),
            ("plan", Value::Array(plan)),
            ("drive", json!({})),
            ("solve_id", json!(solve_id)),
            ("objective_terms", Value::Array(responses.iter().map(|r| json!(r.name)).collect())),
            (
                "objective_block",
                json!({"terms": response_wire, "response_normalization": response_normalization}),
            ),
            ("spec", Value::Null),
            ("warnings", Value::Array(warnings)),
            ("ignored", json!({})),
            ("declared_by", json!(format!("Implexity modular CAE provider {}", repr_str(provider_name)))),
            ("structure_id", json!(child.structure_id())),
            ("content_id", json!(child.content_id())),
            ("before_doc", before_doc),
            ("before_sha256", json!(before_sha256)),
            ("case_source", json!("the modular CAE provider declaration")),
            ("physics_provider", json!(provider_name)),
            ("provider_execution", json!("array")),
            ("provider_problem", problem_wire),
            ("provider_responses", Value::Array(response_wire)),
            ("native_runtime_contract", pre.get("native_runtime_contract").cloned().unwrap_or(Value::Null)),
            ("constraint_semantics", pre.get("constraint_semantics").cloned().unwrap_or(Value::Null)),
            ("design_coordinate_selection", design_coordinate_selection),
            ("coordinate_mask_sources", Value::Object(coordinate_mask_sources)),
            ("update_metric", Value::Object(update_metric)),
            ("provider_derived_plan", Value::Array(derived_plan)),
            ("response_normalization", response_normalization_clone(&js)),
            ("physics_snapshot", physics_snapshot),
            ("computation_effort", Value::Object(effort_binding)),
            ("topology_source_ref", json!(source_ref)),
            ("coordinate_files", Value::Object(coordinate_files)),
        ] {
            fields.insert(k.into(), v);
        }
        let meta = JobMeta {
            fields,
            before_values: coordinate_initial.clone(),
            coordinate_initial,
            topology_initial: Some(topology),
            provider_derived_design_base: derived_design_base,
            provider_derived_before_values: derived_before_values,
            provider_derived_hook: derived_hook,
            spec: None,
        };
        Ok(Some(Declaration { js, meta }))
    }
}

fn response_normalization_clone(js: &Map<String, Value>) -> Value {
    js.get("response_normalization").cloned().unwrap_or(Value::Null)
}

fn json_sorted(value: &Value) -> String {
    canonical_text(value)
}

trait ParamValueExt {
    fn scalar(&self) -> Option<f64>;
}

impl ParamValueExt for implexity_geometry::ParamValue {
    fn scalar(&self) -> Option<f64> {
        self.scalar_f64()
    }
}

impl ModelOptimizeManager {

    pub fn declare(&self, req: &Value) -> JobResult<Declaration> {
        let mut out: Option<Declaration> = None;
        self.inner.host.physics_runtime_guard(req, &mut || {
            out = Some(self.declare_guarded(req)?);
            Ok(())
        })?;
        let decl = out.ok_or_else(|| JobError::runtime("declaration produced no result"))?;
        Self::apply_engineering_problem(decl)
    }

    fn apply_engineering_problem(decl: Declaration) -> JobResult<Declaration> {
        if decl.meta.get("provider_execution").as_str() == Some("array") {
            return Ok(decl);
        }
        let Some(spec) = decl.meta.spec.clone() else {
            return Err(JobError::of(
                "TypeError",
                "ModelOptimizeManager.declare did not expose the validated case; typed engineering entities cannot be \
                 applied safely",
            ));
        };
        let model_key =
            json!({"content_id": spec.model.content_id(), "structure_id": spec.model.structure_id()});
        let effective = implexity_authoring::problem::effective_case_for(Some(&model_key), &spec.case)?;
        if effective == spec.case {
            return Ok(decl);
        }
        let Declaration { mut js, mut meta } = decl;
        let list = |k: &str| js.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        let settings = js
            .get("settings")
            .and_then(Value::as_object)
            .filter(|m| !m.is_empty())
            .cloned()
            .unwrap_or_else(|| spec.settings.clone());
        let rebuilt = Arc::new(crate::optimize::OptimizeSpec::new(
            &spec.model,
            &list("free"),
            &list("objective"),
            &list("constraints"),
            &effective,
            &settings,
        )?);
        js.insert("case".into(), rebuilt.case.clone());
        js.insert("settings".into(), Value::Object(rebuilt.settings.clone()));
        let grid: Vec<i64> = rebuilt
            .bbox
            .get("grid")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| crate::optimize::spec::py_int(v).ok()).collect())
            .unwrap_or_default();
        let steerable = rebuilt.settings.get("steerable").is_some_and(truthy);
        let drive = if steerable {
            crate::optimize::spec::drive_value(&crate::optimize::spec::model_drive_of(&rebuilt)?)
        } else {
            json!({})
        };
        let mut warnings: Vec<Value> = meta.get("warnings").as_array().cloned().unwrap_or_default();
        for w in &rebuilt.case_warnings {
            let w = json!(w);
            if !warnings.contains(&w) {
                warnings.push(w);
            }
        }
        for (k, v) in [
            ("settings", Value::Object(rebuilt.settings.clone())),
            ("grid", json!(grid)),
            ("case_name", rebuilt.norm.get("name").cloned().unwrap_or(Value::Null)),
            ("case_norm", rebuilt.norm.clone()),
            ("solve_id", json!(rebuilt.digest())),
            ("objective_terms", json!(rebuilt.term_names())),
            ("objective_block", rebuilt.objective.clone()),
            ("drive", drive),
            ("case_source", json!("the active differentiable engineering problem")),
            ("warnings", Value::Array(warnings)),
        ] {
            meta.fields.insert(k.into(), v);
        }
        meta.spec = Some(rebuilt);
        Ok(Declaration { js, meta })
    }

    #[allow(clippy::too_many_lines)]
    fn declare_guarded(&self, req0: &Value) -> JobResult<Declaration> {
        let Some(r) = req0.as_object() else {
            return Err(JobError::model_doc(vec!["the request body must be a JSON object".into()]));
        };
        if r.contains_key("applied_setup") {
            self.inner.host.validate_guided_launch(r)?;
        }
        reject_misplaced_computation_effort(req0)?;
        let effort_explicit = r.contains_key("computation_effort");
        let mut req = r.clone();
        let effort = self
            .inner
            .host
            .normalise_computation_effort_request(req.get("computation_effort"))
            .map_err(|e| JobError::optimize1(format!("computation_effort: {e}")))?;
        req.insert("computation_effort".into(), effort);
        let reg = &implexity_core::registries::global().contributions;
        let pname = provider_name(&Value::Object(req.clone()));
        let mut binding =
            pname.as_deref().and_then(|p| implexity_authoring::physics_binding::for_provider(reg, p));
        if let (Some(p), None) = (pname.as_deref(), binding.as_ref())
            && let Some(decl) = self.declare_provider_job(&req, p)?
        {
            return Ok(decl);
        }
        if binding.is_none() {
            let name = req.get("physics").and_then(Value::as_str);
            binding = Some(implexity_authoring::physics_binding::resolve(reg, name).map_err(|e| {
                if e.class() == "BindingError" { JobError::optimize(e.problem_list()) } else { e.into() }
            })?);
        }
        let binding = binding.ok_or_else(|| JobError::runtime("no physics binding"))?;
        if effort_explicit {
            return opt1(
                "computation_effort is currently available only through a registered modular provider whose exact \
                 worker binds and reports computation truth; the legacy implicit-job path cannot silently ignore it",
            );
        }
        req.remove("computation_effort");
        let Value::Object(req) =
            implexity_geometry::direct_occupancy::ensure_topology_declaration(&Value::Object(req))?
        else {
            return Err(JobError::model_doc(vec!["the request body must be a JSON object".into()]));
        };
        crate::optimize::node::register_kind();
        let m = self.inner.models.require()?;
        let node_name = req
            .get("node")
            .filter(|v| truthy(v))
            .or_else(|| m.doc.get("root"))
            .map(py_str)
            .unwrap_or_default();
        let node = m.node(&node_name)?;
        let mut declared_by = "the request".to_string();
        let mut base_free: Vec<Value> = Vec::new();
        let mut base_objective: Vec<Value> = Vec::new();
        let mut base_constraints: Vec<Value> = Vec::new();
        let mut base_case = Value::Null;
        let mut settings: Map<String, Value> = Map::new();
        let (child, child_name) = if let Some(op) = crate::optimize::node::op_of(&node) {
            let d = op.declaration();
            base_free.clone_from(&d.free);
            base_objective.clone_from(&d.objective);
            base_constraints.clone_from(&d.constraints);
            base_case = d.case.clone();
            settings.clone_from(&d.settings);
            declared_by = format!("the document's {} node", repr_str(&node_name));
            let child = node
                .children()
                .first()
                .cloned()
                .ok_or_else(|| JobError::optimize1("an Optimize node has one child"))?;
            let name = m.id_of(&child);
            (child, name)
        } else {
            let name = m.id_of(&node);
            (Arc::clone(&node), name)
        };
        let mut problems: Vec<String> = Vec::new();
        let mut ignored = Map::new();
        let defaults = crate::optimize::spec::defaults();
        let mut keys: Vec<&String> = req.keys().collect();
        keys.sort();
        for k in keys {
            if ["node", "free", "objective", "constraints", "case", "channel", "seq", "provider"]
                .contains(&k.as_str())
            {
                continue;
            }
            if defaults.contains_key(k) {
                settings.insert(k.clone(), req[k].clone());
            } else if let Some((_, why)) = super::IGNORED_KEYS.iter().find(|(n, _)| *n == k) {
                ignored.insert(k.clone(), json!(why));
            } else {
                let mut names: Vec<&String> = defaults.keys().collect();
                names.sort();
                problems.push(format!(
                    "unknown request key {}; this endpoint takes node, free, objective, constraints, case, channel, seq \
                     and the optimiser settings {}",
                    repr_str(k),
                    list_repr(&names)
                ));
            }
        }
        if let Some(stages) = req.get("stages")
            && !stages.is_null()
            && !implexity_core::pyobj::py_eq(stages, &json!(1))
        {
            let why = super::IGNORED_KEYS.iter().find(|(n, _)| *n == "stages").map_or("", |(_, w)| w);
            problems.push(format!(
                "stages = {}: {why}.  Ask for the iterations you want with 'iters'.",
                implexity_core::pyobj::repr(stages)
            ));
        }
        let req_case = req.get("case").filter(|v| truthy(v)).cloned();
        let case_source = if req_case.is_some() {
            "the request".to_string()
        } else if truthy(&base_case) {
            format!("the {declared_by}")
        } else {
            "the service's stored case".to_string()
        };
        let mut case = req_case.unwrap_or(base_case);
        if case.is_null() {
            match self.inner.host.current_case(binding.name()) {
                Some(c) => case = c,
                None => problems.push(format!(
                    "no case document: the objective is about the loads, boundary conditions and materials of a {} \
                     case, so a run needs one.  Store one with the physics package's case endpoint, or send it as \
                     \"case\" in this request.",
                    if binding.label().is_empty() { binding.name() } else { binding.label() }
                )),
            }
        }
        let entries: Vec<Value> = match req.get("free") {
            None | Some(Value::Null) => base_free,
            Some(Value::Object(o)) => vec![Value::Object(o.clone())],
            Some(Value::Array(a)) => a.clone(),
            Some(other) => vec![other.clone()],
        };
        let (free, plan, fprobs) = super::job::plan_free(&m, &child, &entries);
        problems.extend(fprobs.iter().cloned());
        let obj_raw: Vec<Value> = match req.get("objective") {
            None | Some(Value::Null) => base_objective,
            Some(Value::Object(o)) => {
                o.get("terms").filter(|v| truthy(v)).and_then(Value::as_array).cloned().unwrap_or_default()
            }
            Some(Value::Array(a)) => a.clone(),
            Some(other) => vec![other.clone()],
        };
        let objective: Vec<Value> =
            obj_raw.into_iter().map(|t| if t.is_object() { t } else { json!({"term": t}) }).collect();
        let constraints: Vec<Value> = match req.get("constraints") {
            None | Some(Value::Null) => base_constraints,
            Some(Value::Object(o)) => vec![Value::Object(o.clone())],
            Some(Value::Array(a)) => a.clone(),
            Some(other) => vec![other.clone()],
        };
        let mut warn: Vec<String> = Vec::new();
        let grid = settings.get("grid").cloned().unwrap_or(Value::Null);
        let g: Option<i64> = if grid.is_null() {
            None
        } else if let Ok(v) = crate::optimize::spec::py_int(&grid) {
            Some(v)
        } else {
            problems.push(format!("grid must be an integer, got {}", implexity_core::pyobj::repr(&grid)));
            None
        };
        let (max_g, safe_g) = (super::max_opt_grid(), super::safe_opt_grid());
        if let Some(g) = g {
            if g > max_g {
                problems.push(format!(
                    "grid {g} exceeds the guard of {max_g} (IMPLEXITY_MAX_OPT_GRID): a coupled gradient step measured \
                     2.2-3.6 GB at grid 6 on this container's 8 GB cgroup, and the cost is worse than cubic"
                ));
            } else if g > safe_g {
                warn.push(format!(
                    "grid {g} is above the {safe_g} this driver has been measured at (2.2-3.6 GB peak RSS; \
                     docs/IMPLICIT_OPTIMISATION.md s9).  It is allowed, and it may be killed by the cgroup."
                ));
            }
        }
        settings.insert("grid".into(), g.map_or(Value::Null, Value::from));
        if let Some(p) = settings.get("physics").filter(|v| !v.is_null())
            && p.as_str() != Some(binding.name())
        {
            problems.push(format!(
                "physics {} does not match the binding {} this job was declared for",
                implexity_core::pyobj::repr(p),
                repr_str(binding.name())
            ));
        }
        settings.insert("physics".into(), json!(binding.name()));
        if free.iter().any(|f| f.get("parameter").and_then(Value::as_str).is_some()) {
            settings.insert("document_parameter_context".into(), json!({"schema":"implexity-document-parameter-optimization/1","document":m.doc,"node":m.id_of(&child),"base_dir":self.inner.models.dir().to_string_lossy()}));
        }
        let spec = match crate::optimize::OptimizeSpec::new(
            &child,
            &free,
            &objective,
            &constraints,
            &case,
            &settings,
        ) {
            Ok(s) => Some(Arc::new(s)),
            Err(JobError::Problems { class, problems: p, .. }) if class == "OptimizeError" => {
                problems.extend(
                    p.into_iter().filter(|pr| !(!fprobs.is_empty() && pr.starts_with("free is empty:"))),
                );
                None
            }
            Err(e) if e.python_class() == "ModelError" => {
                problems.push(e.message());
                None
            }
            Err(e) => return Err(e),
        };
        let mut eff: Vec<i64> = Vec::new();
        if let Some(spec) = &spec {
            eff = spec
                .bbox
                .get("grid")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| crate::optimize::spec::py_int(v).ok()).collect())
                .unwrap_or_default();
            let e0 = eff.first().copied().unwrap_or(0);
            let eff_repr = implexity_core::pyobj::repr(&json!(eff));
            if e0 > max_g {
                let norm_grid = spec.norm.get("grid").cloned().unwrap_or(Value::Null);
                let norm_list = match &norm_grid {
                    Value::Array(_) => implexity_core::pyobj::repr(&norm_grid),
                    other => implexity_core::pyobj::repr(other),
                };
                problems.push(format!(
                    "the effective grid {eff_repr} exceeds the guard of {max_g} (IMPLEXITY_MAX_OPT_GRID); the case \
                     declares {norm_list} and the request {}",
                    g.map_or_else(|| "no override".to_string(), |g| format!("grid {g}"))
                ));
            } else if e0 > safe_g && g.is_none() {
                warn.push(format!(
                    "the case's own grid {eff_repr} is above the {safe_g} this driver has been measured at (2.2-3.6 GB \
                     peak RSS); pass a coarser \"grid\" if this container is shared"
                ));
            }
        }
        if !problems.is_empty() {
            return opt(problems);
        }
        let spec = spec.ok_or_else(|| JobError::runtime("the optimisation spec was not built"))?;
        warn.extend(spec.case_warnings.iter().cloned());
        let document = m.to_doc()?;
        let mut js = Map::new();
        js.insert("schema".into(), json!(crate::optimize::JOB_SPEC_SCHEMA));
        js.insert("document".into(), document.clone());
        js.insert("base_dir".into(), json!(self.inner.models.dir().to_string_lossy()));
        js.insert("node".into(), child_name.clone().map_or(Value::Null, Value::from));
        js.insert("free".into(), Value::Array(free.clone()));
        js.insert("objective".into(), Value::Array(objective.clone()));
        js.insert("constraints".into(), Value::Array(constraints.clone()));
        js.insert("case".into(), spec.case.clone());
        js.insert("settings".into(), Value::Object(spec.settings.clone()));
        let starts: BTreeMap<String, ArrayD<f64>> =
            spec.free.iter().map(|f| (f.ref_str(), f.start.clone())).collect();
        let mut before: BTreeMap<String, ArrayD<f64>> = BTreeMap::new();
        for e in &plan {
            let r = e.get("ref").map(py_str).unwrap_or_default();
            let v = if e.get("kind").and_then(Value::as_str) == Some("spatial_array") {
                let n = m.node(&e.get("node").map(py_str).unwrap_or_default())?;
                n.param(&e.get("param").map(py_str).unwrap_or_default())
                    .and_then(param_to_array)
                    .ok_or_else(|| JobError::of("KeyError", repr_str(&r)))?
            } else {
                starts.get(&r).cloned().ok_or_else(|| JobError::of("KeyError", repr_str(&r)))?
            };
            before.insert(r, v);
        }
        let steerable = spec.settings.get("steerable").is_some_and(truthy);
        let drive = if steerable {
            crate::optimize::spec::drive_value(&crate::optimize::spec::model_drive_of(&spec)?)
        } else {
            json!({})
        };
        let before_sha256 = sha256_hex(&implexity_geometry::document::canonical_bytes(&document));
        let mut fields = Map::new();
        for (k, v) in [
            ("settings", Value::Object(spec.settings.clone())),
            ("grid", json!(eff)),
            ("node", child_name.map_or(Value::Null, Value::from)),
            ("model_kind", json!(child.kind())),
            ("case_name", spec.norm.get("name").cloned().unwrap_or(Value::Null)),
            ("case_norm", spec.norm.clone()),
            ("free", Value::Array(spec.free.iter().map(crate::optimize::spec::Free::describe).collect())),
            ("plan", Value::Array(plan.clone())),
            ("drive", drive),
            ("solve_id", json!(spec.digest())),
            ("objective_terms", json!(spec.term_names())),
            ("objective_block", spec.objective.clone()),
            ("warnings", json!(warn)),
            ("ignored", Value::Object(ignored)),
            ("declared_by", json!(declared_by)),
            ("structure_id", json!(child.structure_id())),
            ("content_id", json!(child.content_id())),
            ("before_doc", document),
            ("before_sha256", json!(before_sha256)),
            ("case_source", json!(case_source)),
            ("physics_provider", json!(binding.provider())),
            ("provider_execution", json!("implicit_job")),
            ("provider_problem", Value::Null),
            ("provider_responses", json!([])),
        ] {
            fields.insert(k.into(), v);
        }
        let meta = JobMeta {
            fields,
            before_values: before,
            coordinate_initial: BTreeMap::new(),
            topology_initial: None,
            provider_derived_design_base: BTreeMap::new(),
            provider_derived_before_values: BTreeMap::new(),
            provider_derived_hook: None,
            spec: Some(spec),
        };
        Ok(Declaration { js, meta })
    }
}
