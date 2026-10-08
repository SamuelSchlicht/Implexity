// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use implexity_core::pyobj::py_str;
use implexity_io::npy::NpyArray;
use implexity_optim::design::NamedArrays;
use implexity_optim::numeric::{float_value, np_sum};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use super::problem::{Design, Problem};
use super::spec::{Free, OptimizeSpec, json_array, model_drive_of, py_int, select_optimizer};
use crate::artifacts::ResultArtifactStore;
use crate::error::{JobError, JobResult};

fn contributions() -> &'static implexity_core::contributions::ContributionRegistry {
    &implexity_core::registries::global().contributions
}

fn opt1<T>(m: impl Into<String>) -> JobResult<T> {
    Err(JobError::optimize1(m))
}

fn amax(it: impl Iterator<Item = f64>) -> f64 {
    it.fold(f64::NEG_INFINITY, |x, y| if y.is_nan() || x.is_nan() { f64::NAN } else { x.max(y) })
}

fn amin(it: impl Iterator<Item = f64>) -> f64 {
    it.fold(f64::INFINITY, |x, y| if y.is_nan() || x.is_nan() { f64::NAN } else { x.min(y) })
}

fn mean(a: &ArrayD<f64>) -> f64 {
    implexity_optim::numeric::array_mean(a)
}

fn abs_mean(a: &ArrayD<f64>) -> f64 {
    mean(&a.mapv(f64::abs))
}

fn abs_max(a: &ArrayD<f64>) -> f64 {
    amax(a.iter().map(|v| v.abs()))
}

fn scalar_or_null(a: &ArrayD<f64>) -> Value {
    if a.ndim() == 0 { a.iter().next().map_or(Value::Null, |v| float_value(*v)) } else { Value::Null }
}

fn flat_list(a: &ArrayD<f64>) -> Value {
    Value::Array(a.iter().map(|v| float_value(*v)).collect())
}


pub fn analysis_field_registration(prob: &Problem) -> JobResult<Value> {
    let b = &prob.bridge;
    let max: [f64; 3] = std::array::from_fn(|i| b.origin[i] + b.extent[i]);
    Ok(implexity_geometry::field_registration::axis_aligned_registration(b.shape, b.origin, max, "cell")?
        .to_wire())
}

fn representation(prob: &Problem) -> Value {
    prob.classification().report()
}

fn drive_for(spec: &OptimizeSpec) -> JobResult<Option<super::spec::Drive>> {
    if spec.settings.get("steerable").is_some_and(implexity_core::pyobj::truthy) {
        Ok(Some(model_drive_of(spec)?))
    } else {
        Ok(None)
    }
}

fn term_units(name: &str) -> Value {
    implexity_core::objective_terms::get(contributions(), name).map_or(Value::Null, |t| json!(t.units))
}


pub fn survey(spec: &Arc<OptimizeSpec>, log: Option<super::problem::Log<'_>>) -> JobResult<Value> {
    let t0 = Instant::now();
    let prob = Problem::new(spec, log, true)?;
    let prepare_s = t0.elapsed().as_secs_f64();
    let reg = contributions();
    let requested = spec.term_names();
    let mut weights: Map<String, Value> = Map::new();
    if let Some(ts) = spec.objective.get("terms").and_then(Value::as_array) {
        for t in ts {
            weights.insert(
                t.get("term").map(py_str).unwrap_or_default(),
                t.get("weight").cloned().unwrap_or(json!(1.0)),
            );
        }
    }
    let probe = Value::Object(prob.probe.clone());
    let binding = spec.physics.name();
    let mut all = implexity_core::objective_terms::terms(reg, Some(binding));
    all.sort_by(|a, b| a.name.cmp(&b.name));
    let mut terms = Vec::new();
    for term in &all {
        let (ok, refused) = implexity_core::objective_terms::applicability(
            reg,
            std::slice::from_ref(&term.name),
            &probe,
            Some(binding),
        );
        terms.push(json!({
            "term": term.name, "family": term.family, "applicable": !ok.is_empty(),
            "refusal": refused.first(), "requested": weights.contains_key(&term.name),
            "weight": weights.get(&term.name).cloned().unwrap_or(Value::Null),
            "reads": term.reads, "units": term.units, "direction": term.direction, "doc": term.doc,
        }));
    }
    let (ok_req, refused_req) =
        implexity_core::objective_terms::applicability(reg, &requested, &probe, Some(binding));
    let peak = implexity_optim::optjob::peak_rss_mb();
    let rep = representation(&prob);
    let note = match rep["geometry_representation"].get("note") {
        Some(n) => n.clone(),
        None => json!(
            "the occupancy band is measured in millimetres against this promise: d = f * safe_step_factor"
        ),
    };
    let mut probe_out = prob.probe.clone();
    probe_out.insert(
        "note".into(),
        json!("every refusal above quotes one of these numbers; V_model is the fraction of the analysis box the model fills, the rest are the physics binding's own measurements"),
    );
    let weights_f: Map<String, Value> = weights
        .iter()
        .map(|(k, v)| (k.clone(), float_value(super::spec::py_float(v).unwrap_or(f64::NAN))))
        .collect();
    Ok(json!({
        "kind": "implicit_optimize_preflight", "units": "mm",
        "would_run": refused_req.is_empty(),
        "solve_id": spec.digest(),
        "physics": binding,
        "model": {
            "kind": spec.model.kind(), "structure_id": spec.model.structure_id(), "content_id": spec.model.content_id(),
            "field_class": rep["field_class"], "step_factor": rep["step_factor"],
            "geometry_representation": rep["geometry_representation"], "field_class_note": note,
        },
        "case": spec.physics.describe_case(&spec.norm)?,
        "free": spec.free.iter().map(Free::describe).collect::<Vec<_>>(),
        "settings": spec.settings,
        "occupancy": spec.occupancy,
        "driver": select_optimizer(spec, &prob.constraints)?,
        "constraints": prob.constraints.iter().map(super::spec::Constraint::describe).collect::<Vec<_>>(),
        "objective": {"requested": requested, "applicable": ok_req, "refused": refused_req, "weights": weights_f},
        "terms": terms,
        "applicable": terms.iter().filter(|t| t["applicable"] == json!(true)).map(|t| t["term"].clone()).collect::<Vec<_>>(),
        "refused": terms.iter().filter(|t| t["applicable"] != json!(true))
            .map(|t| json!({"term": t["term"], "reads": t["reads"], "refusal": t["refusal"]})).collect::<Vec<_>>(),
        "probe": probe_out,
        "thresholds": {"min_occupancy": super::spec::MIN_OCCUPANCY},
        "lattice": prob.lattice(),
        "prepare_seconds": float_value(implexity_mesh::numeric::py_round_digits(prepare_s, 2)),
        "peak_rss_mb": if peak.is_finite() { float_value(peak) } else { Value::Null },
        "cost": {
            "iterations": spec.int_setting("iters"),
            "note": "a run is one XLA compilation (tens of seconds) plus one gradient evaluation per iteration",
        },
    }))
}

fn safe_field_name(value: &str) -> String {
    value.chars().map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect()
}

fn spatial_gradient(
    value: &ArrayD<f64>,
    analysis_shape: [usize; 3],
    registration: &Value,
) -> Option<(ArrayD<f64>, Value)> {
    if value.ndim() == 3 && value.shape() == analysis_shape {
        return Some((value.as_standard_layout().to_owned(), registration.clone()));
    }
    if value.len() == analysis_shape.iter().product::<usize>() {
        let flat: Vec<f64> = value.iter().copied().collect();
        return ArrayD::from_shape_vec(IxDyn(&analysis_shape), flat).ok().map(|a| (a, registration.clone()));
    }
    None
}

fn artifact_create(
    root: &str,
    fields: &BTreeMap<String, ArrayD<f64>>,
    identities: &Map<String, Value>,
    metadata: &Map<String, Value>,
) -> JobResult<Value> {
    let store = ResultArtifactStore::new(Path::new(root))?;
    let arrays: BTreeMap<String, NpyArray> =
        fields.iter().map(|(k, a)| (k.clone(), NpyArray::from_f64(a))).collect();
    Ok(Value::Object(store.create(&arrays, identities, metadata)?))
}

fn object(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}


#[allow(clippy::too_many_lines)]
pub fn sensitivity(
    spec: &Arc<OptimizeSpec>,
    request: &Value,
    log: Option<super::problem::Log<'_>>,
) -> JobResult<Value> {
    let mut prob = Problem::new(spec, log, false)?;
    prob.calibrate()?;
    let design =
        Design::new(&spec.free, spec.settings.get("scaling").and_then(Value::as_str).unwrap_or("unit_range"));
    let p = design.start();
    let drive = drive_for(spec)?;
    let warm0 = prob.warm0.clone();
    let ev = prob.evaluate(&p, warm0.as_ref(), drive.as_ref(), true, true, &[])?;
    let gp = ev.grad.clone().unwrap_or_default();
    if !(ev.total.is_finite()
        && ev.aux.values().all(|v| v.as_f64().is_none_or(f64::is_finite))
        && implexity_optim::optjob::all_finite(&gp))
    {
        return opt1(
            "the physics evaluation at the current design is not finite; no sensitivity can be reported",
        );
    }
    let gz = design.grad_z(&gp);
    let response_names = spec.term_names();
    let (response_values, jac_p) =
        prob.response_jacobian(&p, warm0.as_ref(), drive.as_ref(), &response_names)?;
    let jac_z: Vec<NamedArrays> = jac_p.iter().map(|r| design.grad_z(r)).collect();
    let analysis_shape = prob.bridge.shape;
    let registration = analysis_field_registration(&prob)?;
    let mut free = Map::new();
    let mut artifact_fields: BTreeMap<String, ArrayD<f64>> = BTreeMap::new();
    let mut artifact_meta = Map::new();
    let zero = ArrayD::zeros(IxDyn(&[]));
    for fr in &spec.free {
        let pv = p.get(&fr.slot).unwrap_or(&zero);
        let a = gp.get(&fr.slot).unwrap_or(&zero);
        let z = gz.get(&fr.slot).unwrap_or(&zero);
        let mut row = object(vec![
            ("units", json!(fr.units)),
            ("size", json!(pv.len())),
            ("value", scalar_or_null(pv)),
            ("value_mean", float_value(mean(pv))),
            ("value_min", float_value(amin(pv.iter().copied()))),
            ("value_max", float_value(amax(pv.iter().copied()))),
            ("dL_dp", scalar_or_null(a)),
            ("dL_dp_absmax", float_value(abs_max(a))),
            ("dL_dp_absmean", float_value(abs_mean(a))),
            ("dL_dz", scalar_or_null(z)),
            ("dL_dz_absmax", float_value(abs_max(z))),
            ("dL_dz_absmean", float_value(abs_mean(z))),
            ("zeros", json!(a.iter().filter(|v| **v == 0.0).count())),
            ("span", fr.span().map_or(Value::Null, float_value)),
            ("lo", fr.lo.map_or(Value::Null, float_value)),
            ("hi", fr.hi.map_or(Value::Null, float_value)),
        ]);
        if a.len() <= 64 {
            row.insert("dL_dp_values".into(), flat_list(a));
            row.insert("dL_dz_values".into(), flat_list(z));
        }
        if let Some((spatial, reg)) = spatial_gradient(z, analysis_shape, &registration) {
            let name = format!("objective__dL_dz__{}", safe_field_name(&fr.ref_str()));
            artifact_fields.insert(name.clone(), spatial);
            artifact_meta.insert(
                name.clone(),
                json!({"kind": "objective_gradient", "objective": "weighted_total", "parameter": fr.ref_str(),
                    "coordinate": "normalised", "units": "1", "signed": true, "registration": reg}),
            );
            row.insert("dL_dz_artifact_field".into(), json!(name));
        }
        free.insert(fr.ref_str(), Value::Object(row));
    }
    let mut responses = Map::new();
    for (ri, name) in response_names.iter().enumerate() {
        let t = implexity_core::objective_terms::get(contributions(), name);
        let mut row = object(vec![
            ("units", t.as_ref().map_or(Value::Null, |t| json!(t.units))),
            ("kind", t.as_ref().map_or(Value::Null, |t| json!(t.kind))),
            ("family", t.as_ref().map_or(Value::Null, |t| json!(t.family))),
            ("direction", t.as_ref().map_or(Value::Null, |t| json!(t.direction))),
            ("value", float_value(response_values.get(ri).copied().unwrap_or(f64::NAN))),
        ]);
        let mut params = Map::new();
        for fr in &spec.free {
            let a = jac_p.get(ri).and_then(|r| r.get(&fr.slot)).unwrap_or(&zero);
            let z = jac_z.get(ri).and_then(|r| r.get(&fr.slot)).unwrap_or(&zero);
            let mut ent = object(vec![
                ("units", json!(fr.units)),
                ("size", json!(a.len())),
                ("dR_dp", scalar_or_null(a)),
                ("dR_dp_absmax", float_value(abs_max(a))),
                ("dR_dp_absmean", float_value(abs_mean(a))),
                ("dR_dz", scalar_or_null(z)),
                ("dR_dz_absmax", float_value(abs_max(z))),
                ("dR_dz_absmean", float_value(abs_mean(z))),
                ("zeros", json!(a.iter().filter(|v| **v == 0.0).count())),
            ]);
            if a.len() <= 64 {
                ent.insert("dR_dp_values".into(), flat_list(a));
                ent.insert("dR_dz_values".into(), flat_list(z));
            }
            if let Some((spatial, reg)) = spatial_gradient(z, analysis_shape, &registration) {
                let an =
                    format!("response__{}__dR_dz__{}", safe_field_name(name), safe_field_name(&fr.ref_str()));
                artifact_fields.insert(an.clone(), spatial);
                artifact_meta.insert(
                    an.clone(),
                    json!({"kind": "response_gradient", "response": name, "parameter": fr.ref_str(),
                        "coordinate": "normalised", "units": t.as_ref().map_or(Value::Null, |t| json!(t.units)),
                        "signed": true, "registration": reg}),
                );
                ent.insert("dR_dz_artifact_field".into(), json!(an));
            }
            params.insert(fr.ref_str(), Value::Object(ent));
        }
        row.insert("parameters".into(), Value::Object(params));
        responses.insert(name.clone(), Value::Object(row));
    }
    let mut diag = Map::new();
    for k in ["L_physical", "V_model", "constraint_penalty"] {
        if let Some(v) = ev.aux.get(k) {
            diag.insert(k.into(), v.clone());
        }
    }
    for (k, v) in prob.row_quantities(&ev.aux) {
        diag.insert(k, v);
    }
    let mut artifact = None;
    if let Some(root) = request.get("artifact_root").filter(|v| implexity_core::pyobj::truthy(v)).map(py_str)
        && !artifact_fields.is_empty()
    {
        artifact = Some(artifact_create(
            &root,
            &artifact_fields,
            &object(vec![
                ("model_structure_id", json!(spec.model.structure_id())),
                ("model_content_id", json!(spec.model.content_id())),
                ("solve_id", json!(spec.digest())),
                ("kind", json!("sensitivity")),
            ]),
            &object(vec![
                ("field_registration", registration.clone()),
                ("fields", Value::Object(artifact_meta)),
                ("grid", prob.spec.bbox.get("grid").cloned().unwrap_or(Value::Null)),
                ("h_mm", float_value(super::spec::py_float(spec.bbox.get("h_mm").unwrap_or(&Value::Null))?)),
            ]),
        )?);
    }
    let rep = representation(&prob);
    let mut result = object(vec![
        ("kind", json!("implicit_sensitivity")),
        ("schema", json!("implexity-sensitivity/2")),
        (
            "model",
            json!({"kind": spec.model.kind(), "structure_id": spec.model.structure_id(),
                "content_id": spec.model.content_id(), "field_class": rep["field_class"],
                "geometry_representation": rep["geometry_representation"]}),
        ),
        ("case", case_block(&prob)?),
        ("physics", json!(spec.physics.name())),
        ("objective", spec.objective.clone()),
        (
            "constraints",
            Value::Array(prob.constraints.iter().map(super::spec::Constraint::describe).collect()),
        ),
        ("L", float_value(ev.total)),
        ("L0", prob.l0.map_or(Value::Null, float_value)),
        ("start_state", prob.start_state.clone()),
        ("diagnostics", Value::Object(diag)),
        ("free", Value::Object(free)),
        ("responses", Value::Object(responses)),
        ("scaling", spec.settings.get("scaling").cloned().unwrap_or(Value::Null)),
        (
            "derivative",
            json!(
                "exact reverse-mode differentiation through the same implicit occupancy and coupled CAE chain used by the optimiser"
            ),
        ),
        ("step_factor", rep["step_factor"].clone()),
        ("probe", Value::Object(prob.probe.clone())),
        ("field_registration", registration),
    ]);
    if let Some(a) = artifact {
        result.insert("artifact".into(), a);
    }
    Ok(Value::Object(result))
}

fn case_block(prob: &Problem) -> JobResult<Value> {
    Ok(json!({
        "name": prob.spec.bbox.get("name").cloned().unwrap_or(Value::Null),
        "grid": prob.spec.bbox.get("grid").cloned().unwrap_or(Value::Null),
        "h_mm": float_value(super::spec::py_float(prob.spec.bbox.get("h_mm").unwrap_or(&Value::Null))?),
    }))
}


#[allow(clippy::too_many_lines)]
pub fn result_snapshot(
    spec: &Arc<OptimizeSpec>,
    request: &Value,
    log: Option<super::problem::Log<'_>>,
) -> JobResult<Value> {
    let mut prob = Problem::new(spec, log, false)?;
    prob.calibrate()?;
    let design =
        Design::new(&spec.free, spec.settings.get("scaling").and_then(Value::as_str).unwrap_or("unit_range"));
    let p = design.start();
    let drive = drive_for(spec)?;
    let dm = prob.occupancy(&p, drive.as_ref())?;
    let warm0 = prob.warm0.clone();
    let state = prob
        .physics
        .state(
            &implexity_authoring::physics_binding::Occupancy { shape: prob.bridge.shape, values: dm },
            warm0.as_ref(),
        )
        .map_err(
            |e| if e.class() == "BindingError" { JobError::optimize(e.problem_list()) } else { e.into() },
        )?;
    let catalogue = implexity_geometry::result_fields::catalogue(contributions(), Some(spec.physics.name()));
    let cat: indexmap::IndexMap<String, Value> = catalogue
        .get("fields")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(|r| (r.get("id").map(py_str).unwrap_or_default(), r.clone())).collect())
        .unwrap_or_default();
    let wanted: Vec<String> = match request.get("fields").filter(|v| implexity_core::pyobj::truthy(v)) {
        None => cat.keys().cloned().collect(),
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => a.iter().map(py_str).collect(),
        Some(_) => return opt1("results fields must be a list of field ids"),
    };
    let mut unknown: Vec<String> = wanted.iter().filter(|w| !cat.contains_key(*w)).cloned().collect();
    unknown.sort();
    unknown.dedup();
    if !unknown.is_empty() {
        let mut avail: Vec<&String> = cat.keys().collect();
        avail.sort();
        return opt1(format!(
            "unknown result field(s) {}; available: {}",
            implexity_core::pyobj::list_repr(&unknown),
            implexity_core::pyobj::list_repr(&avail)
        ));
    }
    let max_inline = match request.get("max_inline_values") {
        None => 4096,
        Some(v) => py_int(v)?,
    }
    .clamp(0, 65536);
    let include = request.get("include_values").is_some_and(implexity_core::pyobj::truthy);
    let grid = prob.bridge.shape;
    let cells: usize = grid.iter().product();
    let mut out = Map::new();
    let mut artifact_fields: BTreeMap<String, ArrayD<f64>> = BTreeMap::new();
    let mut field_metadata = Map::new();
    for ident in &wanted {
        let meta = &cat[ident];
        let source = meta.get("source").map(py_str).unwrap_or_default();
        let key = source.split_once('.').map_or(String::new(), |(_, k)| k.to_string());
        let Some(raw) = state.get(&key) else {
            return opt1(format!(
                "backend declared result field {} from state.{key} but the coupled state did not return that key",
                implexity_core::py_repr::repr_str(ident)
            ));
        };
        let mut a =
            json_array(raw).ok_or_else(|| JobError::value(format!("state.{key} is not a numeric array")))?;
        let native_shape = a.shape().to_vec();
        if a.ndim() == 4 && a.shape()[1..4] == grid {

            let mut perm: Vec<usize> = (1..a.ndim()).collect();
            perm.push(0);
            a = a.permuted_axes(IxDyn(&perm)).as_standard_layout().to_owned();
        } else if (a.ndim() < 3 || a.shape()[..3] != grid) && cells > 0 && a.len() % cells == 0 {
            let components = a.len() / cells;
            let mut target = grid.to_vec();
            if components != 1 {
                target.push(components);
            }
            let flat: Vec<f64> = a.iter().copied().collect();
            a = ArrayD::from_shape_vec(IxDyn(&target), flat).map_err(|e| JobError::value(e.to_string()))?;
        }
        let a = a.as_standard_layout().to_owned();
        let mut rec = object(vec![
            ("id", json!(ident)),
            ("source", json!(source)),
            ("rank", meta.get("rank").cloned().unwrap_or(Value::Null)),
            ("location", meta.get("location").cloned().unwrap_or(Value::Null)),
            ("units", meta.get("units").cloned().unwrap_or(Value::Null)),
            ("shape", json!(a.shape())),
            ("native_shape", json!(native_shape)),
            ("size", json!(a.len())),
            ("dtype", json!("float64")),
        ]);
        if !a.is_empty() {
            let finite: Vec<f64> = a.iter().copied().filter(|v| v.is_finite()).collect();
            rec.insert("finite".into(), json!(finite.len()));
            if !finite.is_empty() {
                #[allow(clippy::cast_precision_loss)]
                let n = finite.len() as f64;
                let sq: Vec<f64> = finite.iter().map(|v| v * v).collect();
                rec.insert("min".into(), float_value(amin(finite.iter().copied())));
                rec.insert("max".into(), float_value(amax(finite.iter().copied())));
                rec.insert("mean".into(), float_value(np_sum(&finite) / n));
                rec.insert("rms".into(), float_value((np_sum(&sq) / n).sqrt()));
            }
        }
        if include && i64::try_from(a.len()).unwrap_or(i64::MAX) <= max_inline {
            rec.insert("values".into(), implexity_optim::numeric::array_to_value(&a));
        } else {
            rec.insert("values".into(), Value::Null);
            rec.insert(
                "transport".into(),
                json!("summary; request include_values=true and keep size <= max_inline_values, or use a future result artifact for large arrays"),
            );
        }
        out.insert(ident.clone(), Value::Object(rec));
        field_metadata.insert(
            ident.clone(),
            json!({"rank": meta.get("rank"), "location": meta.get("location"), "units": meta.get("units"), "source": source}),
        );
        artifact_fields.insert(ident.clone(), a);
    }
    let registration = analysis_field_registration(&prob)?;
    let mut artifact = None;
    if let Some(root) = request.get("artifact_root").filter(|v| implexity_core::pyobj::truthy(v)).map(py_str)
    {
        artifact = Some(artifact_create(
            &root,
            &artifact_fields,
            &object(vec![
                ("model_structure_id", json!(spec.model.structure_id())),
                ("model_content_id", json!(spec.model.content_id())),
                ("solve_id", json!(spec.digest())),
            ]),
            &object(vec![
                ("field_registration", registration.clone()),
                ("fields", Value::Object(field_metadata)),
                ("grid", spec.bbox.get("grid").cloned().unwrap_or(Value::Null)),
                ("h_mm", float_value(super::spec::py_float(spec.bbox.get("h_mm").unwrap_or(&Value::Null))?)),
            ]),
        )?);
    }
    let rep = representation(&prob);
    let mut result = object(vec![
        ("kind", json!("implicit_results")),
        ("schema", json!("implexity-results/2")),
        (
            "model",
            json!({"structure_id": spec.model.structure_id(), "content_id": spec.model.content_id(),
                "field_class": rep["field_class"], "geometry_representation": rep["geometry_representation"]}),
        ),
        ("case", case_block(&prob)?),
        ("physics", json!(spec.physics.name())),
        ("fields", Value::Object(out)),
        ("requested", json!(wanted)),
        ("step_factor", rep["step_factor"].clone()),
        ("field_registration", registration),
        (
            "note",
            json!(
                "One coupled state at the current design; no optimiser update. Every field is read directly from the backend state declared by GET /v1/implicit/results."
            ),
        ),
    ]);
    if let Some(a) = artifact {
        result.insert("artifact".into(), a);
    }
    Ok(Value::Object(result))
}


#[allow(clippy::too_many_lines)]
pub fn derivative_operator(
    spec: &Arc<OptimizeSpec>,
    request: &Value,
    log: Option<super::problem::Log<'_>>,
) -> JobResult<Value> {
    let op = request
        .get("operator")
        .filter(|v| implexity_core::pyobj::truthy(v))
        .map_or_else(|| "jacobian".to_string(), |v| py_str(v).to_lowercase());
    if op == "jacobian" {
        let mut rep = sensitivity(spec, &json!({}), log)?;
        if let Some(m) = rep.as_object_mut() {
            m.insert("kind".into(), json!("implicit_derivative"));
            m.insert("operator".into(), json!("jacobian"));
        }
        return Ok(rep);
    }
    if op != "jvp" && op != "vjp" {
        return opt1("derivative operator must be jacobian, jvp or vjp");
    }
    let mut prob = Problem::new(spec, log, false)?;
    prob.calibrate()?;
    let design =
        Design::new(&spec.free, spec.settings.get("scaling").and_then(Value::as_str).unwrap_or("unit_range"));
    let p = design.start();
    let drive = drive_for(spec)?;
    let warm0 = prob.warm0.clone();
    let response_names = spec.term_names();
    let refs: BTreeMap<String, &Free> = spec.free.iter().map(|f| (f.ref_str(), f)).collect();
    let base = prob.response_values(&p, warm0.as_ref(), drive.as_ref(), &response_names)?;
    let rep = representation(&prob);
    let registration = analysis_field_registration(&prob)?;
    let mut common = object(vec![
        ("kind", json!("implicit_derivative")),
        ("operator", json!(op)),
        (
            "model",
            json!({"kind": spec.model.kind(), "structure_id": spec.model.structure_id(),
                "content_id": spec.model.content_id(), "field_class": rep["field_class"],
                "geometry_representation": rep["geometry_representation"]}),
        ),
        ("case", case_block(&prob)?),
        ("physics", json!(spec.physics.name())),
        (
            "responses",
            Value::Array(
                response_names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| json!({"name": n, "value": float_value(base[i]), "units": term_units(n)}))
                    .collect(),
            ),
        ),
        (
            "free",
            Value::Array(
                spec.free
                    .iter()
                    .map(|f| {
                        json!({"ref": f.ref_str(), "slot": f.slot, "units": f.units,
                            "size": p.get(&f.slot).map_or(0, ArrayD::len),
                            "lo": f.lo.map_or(Value::Null, float_value), "hi": f.hi.map_or(Value::Null, float_value)})
                    })
                    .collect(),
            ),
        ),
        ("step_factor", rep["step_factor"].clone()),
        ("field_registration", registration),
        ("scaling", spec.settings.get("scaling").cloned().unwrap_or(Value::Null)),
        (
            "derivative",
            json!("exact automatic differentiation through the same implicit occupancy and coupled CAE response map used by optimisation"),
        ),
    ]);
    if op == "jvp" {
        let supplied = request.get("tangent").and_then(Value::as_object).cloned().unwrap_or_default();
        let mut unknown: Vec<&String> = supplied.keys().filter(|k| !refs.contains_key(*k)).collect();
        unknown.sort();
        if !unknown.is_empty() {
            return opt1(format!(
                "jvp tangent names unknown free parameter(s): {}",
                implexity_core::pyobj::list_repr(&unknown)
            ));
        }
        let mut tangent =
            NamedArrays::from_pairs(p.iter().map(|(k, v)| (k.to_string(), ArrayD::zeros(v.raw_dim()))));
        let mut used = Vec::new();
        for (r, val) in &supplied {
            let fr = refs[r];
            let a =
                json_array(val).ok_or_else(|| JobError::value(format!("jvp tangent {r} is not numeric")))?;
            let shape = p.get(&fr.slot).map(|x| x.shape().to_vec()).unwrap_or_default();
            let a = if shape.is_empty() {
                if a.len() != 1 {
                    return opt1(format!("jvp tangent {r} needs one number"));
                }
                ArrayD::from_elem(IxDyn(&[]), a.iter().next().copied().unwrap_or(0.0))
            } else if a.shape() != shape.as_slice() {
                if a.len() == shape.iter().product::<usize>() {
                    ArrayD::from_shape_vec(IxDyn(&shape), a.iter().copied().collect())
                        .map_err(|e| JobError::value(e.to_string()))?
                } else {
                    return opt1(format!(
                        "jvp tangent {r} has shape {}; expected {}",
                        super::spec::shape_tuple(a.shape()),
                        super::spec::shape_tuple(&shape)
                    ));
                }
            } else {
                a
            };
            if let Some(t) = tangent.get_mut(&fr.slot) {
                *t = &*t + &a;
            }
            used.push(r.clone());
        }
        let dy = prob.response_jvp(&p, warm0.as_ref(), drive.as_ref(), &response_names, &tangent)?;
        common.insert(
            "tangent".into(),
            Value::Object(used.iter().map(|r| (r.clone(), supplied[r].clone())).collect()),
        );
        common.insert(
            "directional_responses".into(),
            Value::Object(
                response_names.iter().zip(&dy).map(|(n, v)| (n.clone(), float_value(*v))).collect(),
            ),
        );
        common.insert("mathematical_form".into(), json!("J v"));
        return Ok(Value::Object(common));
    }
    let supplied = request.get("cotangent").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut unknown: Vec<&String> = supplied.keys().filter(|k| !response_names.contains(*k)).collect();
    unknown.sort();
    if !unknown.is_empty() {
        return opt1(format!(
            "vjp cotangent names unknown response(s): {}",
            implexity_core::pyobj::list_repr(&unknown)
        ));
    }
    let w: Vec<f64> = response_names
        .iter()
        .map(|n| supplied.get(n).map_or(Ok(0.0), super::spec::py_float))
        .collect::<JobResult<_>>()?;
    let gp = prob.response_vjp(&p, warm0.as_ref(), drive.as_ref(), &response_names, &w)?;
    let mut out = Map::new();
    let zero = ArrayD::zeros(IxDyn(&[]));
    for fr in &spec.free {
        let a = gp.get(&fr.slot).unwrap_or(&zero);
        let mut ent = object(vec![
            ("units", json!(fr.units)),
            ("size", json!(a.len())),
            ("value", scalar_or_null(a)),
            ("absmax", float_value(abs_max(a))),
            ("absmean", float_value(abs_mean(a))),
        ]);
        if a.len() <= 64 {
            ent.insert("values".into(), flat_list(a));
        }
        out.insert(fr.ref_str(), Value::Object(ent));
    }
    common.insert(
        "cotangent".into(),
        Value::Object(response_names.iter().zip(&w).map(|(n, v)| (n.clone(), float_value(*v))).collect()),
    );
    common.insert("design_cotangent".into(), Value::Object(out));
    common.insert("mathematical_form".into(), json!("J^T w"));
    Ok(Value::Object(common))
}
