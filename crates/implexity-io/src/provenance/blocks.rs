// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::path::Path;

use ndarray::ArrayD;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use implexity_core::extensions::format_g;
use implexity_core::json::{DumpOptions, dumps};
use implexity_core::pyobj::{py_eq, py_str, truthy};

use super::{
    ProvResult, canonical, get, invalid, jsonable_f64, list, need_float, need_int, obj, or, pyfloat, pyint,
};

pub const FINGERPRINT_RECIPE: &str = "sha256 over, in this order: the literal b'implexity-design-fingerprint/1\\n'; then for each control channel in ASCII-sorted name order, the line '<name> <dtype.str> <shape tuple>\\n' followed by the raw little-endian float64 bytes of the C-contiguous array; then for each of interface_w, beta_mask, beta_mat, beta_topo, t_offset in that order, the line '<name> %.17g\\n'.  Depends on nothing but the design itself -- not on the file it came from, its path, or its compression.";

pub const CONT_KEYS: [&str; 5] = ["interface_w", "beta_mask", "beta_mat", "beta_topo", "t_offset"];

fn cont_source(raw: &Value) -> String {
    match raw {
        Value::Null => "unknown".into(),
        Value::String(s) => match s.as_str() {
            "run" | "file" => "read".into(),
            "assumed" | "ASSUMED" => "assumed".into(),
            other => other.to_string(),
        },
        other => py_str(other),
    }
}

fn shape_tuple(shape: &[usize]) -> String {
    match shape {
        [] => "()".into(),
        [one] => format!("({one},)"),
        many => format!("({})", many.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}

fn meta_float(meta: &Value, key: &str) -> ProvResult<f64> {
    match meta.get(key) {
        None => Ok(0.0),
        Some(v) => need_float(v, key),
    }
}


pub fn design_fingerprint(params: &BTreeMap<String, ArrayD<f64>>, meta: &Value) -> ProvResult<String> {
    let mut h = Sha256::new();
    h.update(b"implexity-design-fingerprint/1\n");
    for (k, a) in params {
        h.update(format!("{k} <f8 {}\n", shape_tuple(a.shape())).as_bytes());
        for x in a {
            h.update(x.to_le_bytes());
        }
    }
    for k in CONT_KEYS {
        h.update(format!("{k} {}\n", format_g(meta_float(meta, k)?, 17)).as_bytes());
    }
    Ok(hex::encode(h.finalize()))
}

fn iter_values<'a>(meta: &'a Value, key: &str) -> ProvResult<Vec<&'a Value>> {
    match meta.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(a)) => Ok(a.iter().collect()),
        Some(other) => Err(invalid(format!("{key}: {} is not iterable", py_str(other)))),
    }
}

fn round9(x: f64) -> f64 {
    format!("{x:.9}").parse().unwrap_or(x)
}


pub fn design_block(
    params: &BTreeMap<String, ArrayD<f64>>,
    meta: &Value,
    version: i64,
    h_design_mm: Option<f64>,
) -> ProvResult<Value> {
    let prov = obj(get(meta, "continuation_provenance"));
    let mut cont = Map::new();
    let mut assumed: Vec<String> = Vec::new();
    for k in CONT_KEYS {
        let raw = prov.get(k).cloned().unwrap_or(Value::Null);
        let src = cont_source(&raw);
        if src == "assumed" {
            assumed.push(k.to_string());
        }
        let value = meta_float(meta, k)?;
        let mut entry = json!({"value": jsonable_f64(value), "source": src, "source_token": raw});
        if k == "t_offset"
            && let Value::Object(m) = &mut entry
        {
            m.insert("value_m".into(), jsonable_f64(value));
            m.insert("value_mm".into(), jsonable_f64(value * 1e3));
        }
        cont.insert(k.into(), entry);
    }
    let src = get(meta, "source");
    let fileinfo = match src {
        Value::String(p) if Path::new(p).is_file() => {
            let bytes = std::fs::metadata(p).map(|m| m.len()).map_err(|e| invalid(format!("{p}: {e}")))?;
            let sha = crate::digest::sha256_file(Path::new(p)).map_err(|e| invalid(format!("{p}: {e}")))?;
            json!({"path": p, "path_is_host_specific": true, "bytes": bytes, "sha256": sha})
        }
        _ => Value::Null,
    };
    let dom: Vec<f64> = iter_values(meta, "domain_mm")?
        .into_iter()
        .map(|v| need_float(v, "domain_mm"))
        .collect::<ProvResult<_>>()?;
    let cs: Vec<i64> = iter_values(meta, "control_shape")?
        .into_iter()
        .map(|v| need_int(v, "control_shape"))
        .collect::<ProvResult<_>>()?;
    let grid: Vec<i64> = iter_values(meta, "design_grid")?
        .into_iter()
        .map(|v| need_int(v, "design_grid"))
        .collect::<ProvResult<_>>()?;
    let spacing = if dom.len() == 3 && cs.len() == 3 && cs.iter().min().is_some_and(|&m| m > 1) {
        #[allow(clippy::cast_precision_loss)]
        let s: Vec<Value> = (0..3).map(|i| jsonable_f64(round9(dom[i] / (cs[i] - 1) as f64))).collect();
        Value::Array(s)
    } else {
        Value::Null
    };
    let mut channels: Vec<&String> = params.keys().collect();
    channels.sort();
    let warning = if assumed.is_empty() {
        Value::Null
    } else {
        json!(format!(
            "continuation parameter(s) {} were NOT in the design file and are ASSUMED at this service's defaults. The same control fields give a different solid at a different continuation stage: CAD_INTEGRATION.md 5.3 measures 6.24 % of volume fraction between interface_w 0.5 and 0.35.",
            assumed.join(", ")
        ))
    };
    Ok(json!({
        "source": py_str(src),
        "source_kind": if fileinfo.is_null() { "live" } else { "file" },
        "version": version,
        "fingerprint": format!("sha256:{}", design_fingerprint(params, meta)?),
        "fingerprint_recipe": FINGERPRINT_RECIPE,
        "file": fileinfo,
        "channels": channels,
        "control_shape": cs,
        "control_spacing_mm": spacing,
        "domain_mm": dom.iter().map(|&v| jsonable_f64(v)).collect::<Vec<_>>(),
        "design_grid": grid,
        "h_design_mm": h_design_mm.map(jsonable_f64),
        "period_mm": get(meta, "period_mm"),
        "continuation": cont,
        "continuation_assumed": assumed,
        "continuation_warning": warning,
    }))
}

#[must_use]
pub fn case_hashes(case_doc: &Value) -> Value {
    let canon = canonical(case_doc);
    let legacy = dumps(case_doc, &DumpOptions::default().sorted(true));
    let steer = crate::digest::sha256_hex(dumps(case_doc, &DumpOptions::canonical()).as_bytes());
    json!({
        "canonical_sha256": crate::digest::sha256_hex(canon.as_bytes()),
        "canonical_bytes": canon.len(),
        "bodyexport_blake2b_8": crate::digest::blake2b_hex(legacy.as_bytes(), 8),
        "steer_sha256_16": &steer[..16],
    })
}


pub fn case_block(case_doc: &Value, domain: &Value, include_document: bool) -> ProvResult<Value> {
    let doc = if truthy(case_doc) { case_doc.clone() } else { json!({}) };
    let mut loads = Vec::new();
    for l in list(get(&doc, "loads")) {
        if !l.is_object() {
            return Err(invalid("case load entries must be objects"));
        }
        loads.push(json!({
            "name": get(&l, "name"), "kind": get(&l, "kind"), "magnitude": get(&l, "magnitude"),
            "vector_MPa": get(&l, "vector_MPa"), "region": get(&l, "region"),
        }));
    }
    let mut out = json!({
        "name": get(&doc, "name"),
        "schema": get(&doc, "schema"),
        "grid": get(&doc, "grid"),
        "h_mm": get(&doc, "h_mm"),
        "domain_mm": get(&doc, "domain_mm"),
        "volfrac": get(&doc, "volfrac"),
        "volfrac_of_domain": get(&doc, "volfrac_of_domain"),
        "loads": loads,
        "hashes": case_hashes(&doc),
        "document": if include_document { doc.clone() } else { Value::Null },
        "document_note": if include_document {
            "the whole normalised case document, so this record answers 'from what' without reference to any other file"
        } else {
            "omitted from this copy; the sidecar carries it"
        },
    });
    let dom = or(get(&doc, "domain"), &Value::Null).clone();
    let mut mesh = Value::Null;
    if truthy(get(&dom, "mesh")) {
        mesh = json!({
            "name": get(&dom, "mesh"), "units_mm": get(&dom, "units_mm"), "sha256_16": null, "bytes": null,
            "source": "named by the case document",
        });
    }
    if truthy(domain) {
        mesh = json!({
            "name": get(domain, "name"), "units_mm": get(domain, "units_mm"), "sha256_16": get(domain, "sha"),
            "bytes": get(domain, "bytes"), "source": "uploaded through PUT /v1/domain and hashed there",
        });
    }
    if let Value::Object(m) = &mut out {
        m.insert("domain_mesh".into(), mesh);
    }
    Ok(out)
}

pub type Declaration<'a> = &'a dyn Fn(&str) -> Option<Value>;

pub type DefaultTerms<'a> = &'a dyn Fn(&Map<String, Value>) -> (Value, Vec<Value>, Map<String, Value>);

fn registry_declaration(name: &str) -> Value {
    let reg = &implexity_core::registries::global().contributions;
    let Some(term) = implexity_core::objective_terms::get(reg, name) else { return json!({}) };
    let mut knobs = Map::new();
    for (k, knob) in &term.knobs {
        knobs.insert(k.clone(), json!([knob.default, knob.unit, knob.doc]));
    }
    json!({
        "kind": term.kind, "units": term.units, "direction": term.direction,
        "transcribed": term.provenance, "calibrated": term.calibrated,
        "reference_key": term.reference_key, "reads": term.reads, "doc": term.doc,
        "knobs": knobs, "family": term.family,
    })
}

fn term_declaration(name: &Value, declaration: Option<Declaration<'_>>) -> (Value, Value) {
    let key = py_str(name);
    let spec = match declaration {
        Some(f) => f(&key).filter(truthy).unwrap_or_else(|| json!({})),
        None if name.is_string() => registry_declaration(&key),
        None => json!({}),
    };
    let transcribed = get(&spec, "transcribed").clone();
    let declared = json!({
        "kind": get(&spec, "kind"), "units": get(&spec, "units"), "family": get(&spec, "family"),
        "direction": get(&spec, "direction"),
        "transcribed_from": transcribed,
        "transcription_note": if truthy(&transcribed) {
            "the declaring package states this term reproduces that line of its reference implementation at the catalogue defaults"
        } else {
            "the declaring package states no reference transcription for this term"
        },
        "declares_calibrated": spec.get("calibrated").is_some_and(truthy),
        "reference_key": get(&spec, "reference_key"),
        "reads": list(get(&spec, "reads")),
        "doc": get(&spec, "doc"),
        "in_catalogue": truthy(&spec),
    });
    (declared, spec)
}

fn merge(entry: &mut Value, extra: &Value) {
    if let (Value::Object(m), Value::Object(x)) = (entry, extra) {
        for (k, v) in x {
            m.insert(k.clone(), v.clone());
        }
    }
}

fn set(entry: &mut Value, key: &str, value: Value) {
    if let Value::Object(m) = entry {
        m.insert(key.into(), value);
    }
}

#[derive(Default)]
pub struct ObjectiveInputs<'a> {
    pub objective_meta: Value,
    pub last_row: Value,
    pub refs: Value,
    pub l0: Value,
    pub declaration: Option<Declaration<'a>>,
    pub penalties: Option<Value>,
    pub default_terms: Option<DefaultTerms<'a>>,
}

impl std::fmt::Debug for ObjectiveInputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectiveInputs")
            .field("objective_meta", &self.objective_meta)
            .field("last_row", &self.last_row)
            .field("refs", &self.refs)
            .field("l0", &self.l0)
            .field("penalties", &self.penalties)
            .finish_non_exhaustive()
    }
}

fn lookup_last<'v>(rows: &'v [(Value, Value)], key: &Value) -> Option<&'v Value> {
    rows.iter().rev().find(|(k, _)| py_eq(k, key)).map(|(_, v)| v)
}


#[allow(clippy::too_many_lines)]
pub fn objective_block(case_doc: &Value, inputs: &ObjectiveInputs<'_>) -> ProvResult<Value> {
    let doc = if truthy(case_doc) { case_doc.clone() } else { json!({}) };
    let block = get(&doc, "objective").clone();
    let refs = obj(&inputs.refs);
    let mut meta_terms: Vec<(Value, Value)> = Vec::new();
    for t in list(get(or(&inputs.objective_meta, &Value::Null), "terms")) {
        meta_terms.push((get(&t, "term").clone(), t));
    }
    let last_row = if truthy(&inputs.last_row) { inputs.last_row.clone() } else { json!({}) };
    let raw_rows = or(get(&last_row, "terms"), &Value::Null).clone();
    if let Value::Array(items) = &raw_rows
        && !items.is_empty()
        && items.iter().all(|r| r.is_object() && r.get("response").is_some())
    {
        let measured = items.clone();
        let final_l = last_row.get("L").or_else(|| last_row.get("objective")).cloned().unwrap_or(Value::Null);
        return Ok(json!({
            "source": "modular provider response history",
            "terms": measured, "term_count": items.len(), "final_values": measured,
            "final_L": final_l, "final_L_physical": get(&last_row, "L_physical"),
            "normalisation_L0": null,
            "normalisation_note": "Response scales and senses are declared in the provider job specification; no implicit start-loss normalization.",
            "penalties": null,
            "penalties_note": "Only declared provider objective and constraint contributions are reported.",
            "references": null,
        }));
    }
    let rows: Map<String, Value> = match &raw_rows {
        Value::Object(m) => m.clone(),
        Value::Null => Map::new(),
        Value::Array(a) if a.is_empty() => Map::new(),
        _ => return Err(invalid("history row terms must be a mapping")),
    };

    let mut terms: Vec<Value> = Vec::new();
    let mut row_names = Map::new();
    let source: Value;
    if truthy(&block) && truthy(get(&block, "terms")) {
        source = json!("authored");
        for t in list(get(&block, "terms")) {
            let name = get(&t, "term").clone();
            let (declared, spec) = term_declaration(&name, inputs.declaration);
            let meta_entry = lookup_last(&meta_terms, &name);
            let spec_knobs = obj(get(&spec, "knobs"));
            let mut knobs = Vec::new();
            for (kn, decl) in &spec_knobs {
                let tuple = decl.as_array();
                let default = tuple.and_then(|d| d.first()).cloned().unwrap_or(Value::Null);
                let units = tuple.and_then(|d| d.get(1)).cloned().unwrap_or(Value::Null);
                let kdoc = tuple.and_then(|d| d.get(2)).cloned().unwrap_or(Value::Null);
                let given = t.get(kn.as_str()).is_some();
                let value = t.get(kn.as_str()).cloned().unwrap_or_else(|| default.clone());
                let resolved =
                    meta_entry.map_or(Value::Null, |m| get(or(get(m, "knobs"), &Value::Null), kn).clone());
                knobs.push(json!({
                    "name": kn, "value": value,
                    "source": if given { "from_case" } else { "catalogue_default" },
                    "assumed": !given, "catalogue_default": default, "units": units, "doc": kdoc,
                    "resolved_at_build": resolved,
                }));
            }
            let weight_default = spec_knobs
                .get("weight")
                .and_then(|w| w.as_array().and_then(|a| a.first()).cloned())
                .unwrap_or(Value::Null);
            let weight_value = match t.get("weight") {
                Some(w) => w.clone(),
                None => or(&weight_default, &json!(0.0)).clone(),
            };
            let weight = need_float(&weight_value, "weight")?;
            let mut entry = json!({
                "term": name,
                "weight": jsonable_f64(weight),
                "weight_source": if t.get("weight").is_some() { "from_case" } else { "catalogue_default" },
                "knobs": knobs,
                "resolved": meta_entry.map_or(Value::Null, |m| Value::Object(obj(m))),
                "row_key": name,
            });
            merge(&mut entry, &declared);
            terms.push(entry);
        }
    } else if let Some(hook) = inputs.default_terms {
        let (s, t, names) = hook(&rows);
        source = s;
        terms = t;
        row_names = names;
    } else {
        source =
            json!("not authored: the terms the run's own history rows recorded, with their MEASURED weights");
        let mut keys: Vec<&String> = rows.keys().collect();
        keys.sort();
        for rk in keys {
            let (declared, _spec) = term_declaration(&json!(rk), inputs.declaration);
            let mut entry = json!({
                "term": rk, "row_key": rk, "weight": get(&rows[rk], "w"),
                "weight_source": "MEASURED from the run's own history rows",
                "knobs": [], "resolved": null,
            });
            merge(&mut entry, &declared);
            terms.push(entry);
        }
    }

    for entry in &mut terms {
        let key = get(entry, "reference_key").clone();
        if !truthy(get(entry, "declares_calibrated")) {
            set(entry, "calibrated", json!(false));
            set(entry, "reference_value", Value::Null);
            set(
                entry,
                "calibration_note",
                json!("not a calibrated term: its value is in the units the catalogue declares for it"),
            );
        } else if let Some(v) = key.as_str().filter(|k| !k.is_empty()).and_then(|k| refs.get(k)) {
            set(entry, "calibrated", json!(true));
            set(entry, "reference_value", v.clone());
            set(
                entry,
                "calibration_note",
                json!(
                    "normalised on the start design; this is the reference the run measured and carried, read back from the run's own checkpoint"
                ),
            );
        } else {
            set(entry, "calibrated", Value::Null);
            set(entry, "reference_value", Value::Null);
            set(
                entry,
                "calibration_note",
                json!(
                    "the term DECLARES itself calibrated and the runner does calibrate on the start design, but no reference value for it was recorded anywhere this record could read. Treat the term as normalised on the start design and the reference itself as UNRECORDED -- not as uncalibrated."
                ),
            );
        }
    }

    let mut final_values = Map::new();
    for (k, v) in &rows {
        let mut row = Value::Object(obj(v));
        set(&mut row, "term", row_names.get(k).cloned().unwrap_or_else(|| json!(k)));
        final_values.insert(k.clone(), row);
    }
    let count = terms.len();
    let penalties_given = inputs.penalties.is_some();
    Ok(json!({
        "source": source,
        "terms": terms,
        "term_count": count,
        "penalties": inputs.penalties.as_ref().map(|p| Value::Object(obj(p))),
        "penalties_note": if penalties_given {
            "admissibility penalties declared by the physics package ride on EVERY objective and are not authored"
        } else {
            "the physics package declares no fixed penalties"
        },
        "final_values": if final_values.is_empty() { Value::Null } else { Value::Object(final_values) },
        "final_L": get(&last_row, "L"),
        "final_L_physical": get(&last_row, "L_physical"),
        "normalisation_L0": inputs.l0,
        "normalisation_note": "L is the weighted sum divided by its value on the start design, so L = 1 at iteration 0 by construction. L is comparable only WITHIN one regime: across a steer it is a value of a different objective, and across a re-calibration it is a value of a different normalisation as well.",
        "references": if refs.is_empty() { Value::Null } else { Value::Object(refs) },
    }))
}

fn changed(before: &Map<String, Value>, after: &Map<String, Value>) -> Value {
    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Map::new();
    for k in keys {
        let (b, a) = (before.get(k).unwrap_or(&Value::Null), after.get(k).unwrap_or(&Value::Null));
        if !py_eq(b, a) {
            out.insert(k.clone(), json!({"before": b, "after": a}));
        }
    }
    if out.is_empty() { Value::Null } else { Value::Object(out) }
}


#[allow(clippy::too_many_lines)]
pub fn steer_events(timeline: &[Value], rows: &[Value]) -> ProvResult<Vec<Value>> {
    let mut out = Vec::new();
    for ent in timeline {
        let e = Value::Object(obj(ent));
        let applied = truthy(get(&e, "applied"));
        let g = |k: &str| get(&e, k).clone();
        let mut rec = json!({
            "seq": g("seq"), "at_iteration": g("at_iteration"), "stage": g("stage"), "applied": applied,
            "refused": g("refused"), "replayed": truthy(get(&e, "replayed")),
            "delta_class": g("delta_class"), "classes": g("classes"), "hot": g("hot"), "changes": g("changes"),
            "rebuild": g("rebuild"), "rebuilt": g("rebuilt"), "grid_changed": g("grid_changed"),
            "momentum": g("momentum"), "recalibrate": g("recalibrate"),
            "rebuild_ms": g("rebuild_ms"), "steer_ms": g("steer_ms"), "recalibrate_ms": g("recalibrate_ms"),
            "case_hash_before": g("hash_before"), "case_hash_after": g("hash_after"),
            "drive_before": g("drive_before"), "drive_after": g("drive_after"),
            "regime": g("regime"), "L_before": g("L_before"), "warnings": g("warnings"),
        });
        if !applied {
            set(&mut rec, "calibration_discontinuity", Value::Null);
            set(
                &mut rec,
                "note",
                json!(
                    "this steer was NOT applied, so it moved nothing; it is recorded because a refused steer is part of what happened"
                ),
            );
            out.push(rec);
            continue;
        }
        let (l0b, l0a) = (g("l0_before"), g("l0_after"));
        let refs_changed = changed(&obj(get(&e, "refs_before")), &obj(get(&e, "refs_after")));
        let drive_changed = changed(&obj(get(&e, "drive_before")), &obj(get(&e, "drive_after")));
        let mut l_after = Value::Null;
        if !rows.is_empty() {
            let at = match e.get("at_iteration") {
                None => 0,
                Some(v) => need_int(v, "at_iteration")?,
            };
            for r in rows {
                let i = match r.get("i") {
                    None => -1,
                    Some(v) => need_int(v, "i")?,
                };
                if i >= at {
                    l_after = get(r, "L").clone();
                    break;
                }
            }
        }
        set(&mut rec, "L_after_first_row", l_after);
        let recal = truthy(get(&e, "recalibrate"));
        let reset = get(&e, "momentum").as_str() == Some("reset");
        set(
            &mut rec,
            "calibration_discontinuity",
            json!({
                "objective_changed": true,
                "loss_comparable_across": false,
                "normalisation_changed": recal,
                "l0_before": l0b, "l0_after": l0a,
                "l0_changed": !py_eq(&l0b, &l0a),
                "references_changed": refs_changed,
                "drive_changed": drive_changed,
                "incumbent_retired": g("best_before"),
                "momentum": g("momentum"),
                "momentum_note": if reset {
                    "the optimiser's momentum state was RESET at this boundary"
                } else {
                    "the optimiser's momentum state was KEPT across this boundary, so the first steps after it carry velocity accumulated under the previous objective"
                },
                "severity": if recal {
                    "normalisation re-measured on the current iterate: L before and after this steer are values of different functions AND of different normalisations, and are not comparable at all"
                } else {
                    "calibration kept from the start design: the terms stay in the units they started in, but L after this steer is still a value of a DIFFERENT objective"
                },
                "statement": format!(
                    "The problem being solved changed at iteration {}. Anything read across this boundary -- a loss curve, a 'best' iterate, a convergence claim -- spans two different problems.",
                    py_str(get(&e, "at_iteration"))
                ),
            }),
        );
        out.push(rec);
    }
    Ok(out)
}

fn l_of(r: &Value) -> ProvResult<f64> {
    pyfloat(get(r, "L")).ok_or_else(|| invalid("history row L is not a number"))
}

pub(crate) fn best_by_l(rows: &[Value]) -> ProvResult<Option<&Value>> {
    let mut best: Option<(&Value, f64)> = None;
    for r in rows {
        let l = l_of(r)?;
        if best.is_none_or(|(_, b)| l < b) {
            best = Some((r, l));
        }
    }
    Ok(best.map(|(r, _)| r))
}

fn row_i(r: &Value) -> ProvResult<i64> {
    match r.get("i") {
        None => Ok(0),
        Some(v) => pyint(v).ok_or_else(|| invalid("history row i is not an integer")),
    }
}


pub fn loss_trajectory(rows: &[Value], events: &[Value]) -> ProvResult<Value> {
    let rows: Vec<&Value> = rows.iter().filter(|r| r.get("L").is_some()).collect();
    if rows.is_empty() {
        return Ok(json!({
            "iterations": 0, "first": null, "last": null, "best": null, "regimes": [], "comparable": true,
            "note": "no iteration completed",
        }));
    }
    let mut cuts: Vec<i64> = Vec::new();
    for e in events {
        if truthy(get(e, "applied")) && !get(e, "at_iteration").is_null() {
            cuts.push(need_int(get(e, "at_iteration"), "at_iteration")?);
        }
    }
    cuts.sort_unstable();
    let mut bounds: Vec<Option<i64>> = cuts.into_iter().map(Some).collect();
    bounds.push(None);
    let mut segs = Vec::new();
    let mut start = 0i64;
    for (n, stop) in bounds.iter().enumerate() {
        let mut sel: Vec<Value> = Vec::new();
        for r in &rows {
            let i = row_i(r)?;
            if i >= start && stop.is_none_or(|s| i < s) {
                sel.push((*r).clone());
            }
        }
        if let (Some(first), Some(last), Some(best)) = (sel.first(), sel.last(), best_by_l(&sel)?) {
            segs.push(json!({
                "regime": n, "i_first": row_i(first)?, "i_last": row_i(last)?, "iterations": sel.len(),
                "L_first": get(first, "L"), "L_last": get(last, "L"), "L_best": get(best, "L"),
                "best_at": {"i": get(best, "i"), "stage": get(best, "stage"), "it": get(best, "it")},
            }));
        }
        if let Some(s) = stop {
            start = *s;
        }
    }
    let owned: Vec<Value> = rows.iter().map(|r| (*r).clone()).collect();
    let best = best_by_l(&owned)?.map_or(Value::Null, |r| get(r, "L").clone());
    let comparable = segs.len() <= 1;
    let note = if comparable {
        "one regime: first, last and best are values of the same function and are comparable".to_string()
    } else {
        format!(
            "MORE THAN ONE REGIME: the whole-run first/last/best above span {} different objectives and are NOT comparable. Read the per-regime endpoints instead.",
            segs.len()
        )
    };
    let (first, last) = (rows[0], rows[rows.len() - 1]);
    Ok(json!({
        "iterations": rows.len(),
        "first": get(first, "L"), "last": get(last, "L"), "best": best,
        "i_first": row_i(first)?, "i_last": row_i(last)?,
        "regimes": segs,
        "comparable": comparable,
        "note": note,
    }))
}

