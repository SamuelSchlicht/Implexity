// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_geometry::lattice::component_field::{component_source, split_component_id};

use crate::error::{AResult, AuthoringError};
use crate::field_interaction::{flatten_json, read_spatial_field};
use crate::py::{obj_mut, path_obj, py_str, setdefault_obj};

pub const SCHEMA: &str = "implexity-design-holds/1";

fn verr(m: &str) -> AuthoringError {
    AuthoringError::value("ValueError", m)
}

#[must_use]
pub fn encode_runs(mask: &[bool]) -> Vec<[usize; 2]> {
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for (i, v) in mask.iter().enumerate() {
        match (v, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                runs.push([s, i - s]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push([s, mask.len() - s]);
    }
    runs
}

#[must_use]
pub fn runs_json(runs: &[[usize; 2]]) -> Value {
    Value::Array(runs.iter().map(|r| json!([r[0], r[1]])).collect())
}


pub fn decode_runs(runs: &Value, shape: &[usize]) -> AResult<Vec<bool>> {
    let count: usize = shape.iter().product();
    let mut out = vec![false; count];
    let Some(items) = runs.as_array() else { return Err(verr("held runs must be a sequence")) };
    let mut last = 0i64;
    for item in items {
        let Some(pair) = item.as_array().filter(|p| p.len() == 2) else {
            return Err(verr("held run requires start and count"));
        };
        let (Some(a), Some(n)) = (pair[0].as_i64(), pair[1].as_i64()) else {
            return Err(verr("invalid or overlapping held runs"));
        };
        if a < last || n < 1 || a + n > count as i64 {
            return Err(verr("invalid or overlapping held runs"));
        }
        for v in &mut out[a as usize..(a + n) as usize] {
            *v = true;
        }
        last = a + n;
    }
    Ok(out)
}

fn records(document: &Value) -> Map<String, Value> {
    path_obj(document, &["meta", "implexity", "geometry_edit", "holds"]).cloned().unwrap_or_default()
}

fn shape_list(shape: &[usize]) -> Value {
    json!(shape)
}

fn binary_mask(raw: &Value, size: usize) -> Option<Vec<bool>> {
    let flat = flatten_json(raw)?;
    if flat.len() != size {
        return None;
    }
    flat.iter()
        .map(|v| match v {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => match n.as_f64() {
                Some(0.0) => Some(false),
                Some(1.0) => Some(true),
                _ => None,
            },
            _ => None,
        })
        .collect()
}


pub fn held_mask(document: &Value, base: &str, shape: &[usize], include_legacy: bool) -> AResult<Vec<bool>> {
    let count: usize = shape.iter().product();
    let recs = records(document);
    let mut held = match recs.get(base) {
        None => vec![false; count],
        Some(record) => {
            let rec_shape: Vec<usize> = record
                .get("shape")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_u64().map(|u| u as usize)).collect())
                .unwrap_or_default();
            let schema_ok = record.get("schema").and_then(Value::as_str) == Some(SCHEMA);
            let shape_len_ok =
                record.get("shape").and_then(Value::as_array).is_some_and(|a| a.len() == rec_shape.len());
            if !schema_ok || !shape_len_ok || rec_shape != shape {
                return Err(verr(
                    "saved control hold no longer matches the authoritative array; explicitly release or migrate it",
                ));
            }
            decode_runs(record.get("runs").unwrap_or(&Value::Null), shape)?
        }
    };
    if include_legacy {
        let fields =
            path_obj(document, &["meta", "implexity", "spatial_fields"]).cloned().unwrap_or_default();
        for (field_id, meta) in &fields {
            if !meta.is_object() {
                continue;
            }
            let selection = split_component_id(field_id)?;
            let matches_base = match &selection {
                None => field_id == base,
                Some((b, _)) => field_id == base || b == base,
            };
            if !matches_base {
                continue;
            }
            let index =
                if selection.is_some() { Some(component_source(document, field_id)?.1) } else { None };
            let target_size: usize = if index.is_none() { count } else { shape[1..].iter().product() };
            let masks = meta.get("protected_masks").and_then(Value::as_object).cloned().unwrap_or_default();
            for raw in masks.values() {
                let raw = if raw.is_object() { raw.get("values").unwrap_or(&Value::Null) } else { raw };
                let Some(mask) = binary_mask(raw, target_size) else {
                    return Err(verr("invalid protected design mask"));
                };
                match index {
                    None => held.iter_mut().zip(&mask).for_each(|(h, m)| *h |= *m),
                    Some(i) => held[i * target_size..(i + 1) * target_size]
                        .iter_mut()
                        .zip(&mask)
                        .for_each(|(h, m)| *h |= *m),
                }
            }
        }
    }
    Ok(held)
}


pub fn field_held_mask(document: &Value, field_id: &str, shape: &[usize]) -> AResult<Vec<bool>> {
    if split_component_id(field_id)?.is_some() {
        let (base, index, tensor, _) = component_source(document, field_id)?;
        let tshape = tensor.shape().to_vec();
        let n: usize = tshape[1..].iter().product();
        let all = held_mask(document, &base, &tshape, true)?;
        return Ok(all[index * n..(index + 1) * n].to_vec());
    }
    held_mask(document, field_id, shape, true)
}


pub fn edit_holds(
    document: &Value,
    field_id: &str,
    selected: &[bool],
    release: bool,
    whole_volume: bool,
) -> AResult<(Value, Value)> {
    let field = read_spatial_field(document, field_id)?;
    if selected.len() != field.values.len() {
        return Err(verr("hold selection shape mismatch"));
    }
    let (base, shape, index) = if split_component_id(field_id)?.is_some() {
        let (base, index, tensor, _) = component_source(document, field_id)?;
        (base, tensor.shape().to_vec(), Some(index))
    } else {
        (field_id.to_string(), field.grid.shape.to_vec(), None)
    };
    let mut held = held_mask(document, &base, &shape, false)?;
    let value = !release;
    match index {
        None => {
            for (h, s) in held.iter_mut().zip(selected) {
                if *s {
                    *h = value;
                }
            }
        }
        Some(i) => {
            let n: usize = shape[1..].iter().product();
            let channels: Vec<usize> =
                if whole_volume { (20 * (i / 20)..20 * (i / 20 + 1)).collect() } else { vec![i] };
            for c in channels {
                for (k, s) in selected.iter().enumerate() {
                    if *s && let Some(h) = held.get_mut(c * n + k) {
                        *h = value;
                    }
                }
            }
        }
    }
    let mut out = document.clone();
    let root = obj_mut(&mut out)?;
    let records = setdefault_obj(
        setdefault_obj(setdefault_obj(setdefault_obj(root, "meta")?, "implexity")?, "geometry_edit")?,
        "holds",
    )?;
    let count = held.iter().filter(|h| **h).count();
    if count > 0 {
        records.insert(
            base.clone(),
            json!({"schema": SCHEMA, "shape": shape_list(&shape), "runs": runs_json(&encode_runs(&held))}),
        );
    } else {
        records.shift_remove(&base);
    }
    Ok((
        out,
        json!({"protection": "design_controls", "held_control_values": count,
            "constraint_scope": "manual writes and native optimizer coordinates; not a surface-position constraint"}),
    ))
}



pub fn intersect_coordinate_masks(
    document: &Value,
    design_rows: &[(String, Option<(String, String)>, Vec<usize>)],
    masks: &mut Map<String, Value>,
    sources: &mut Map<String, Value>,
) -> AResult<()> {
    for (coordinate, binding, shape) in design_rows {
        let Some((kind, key)) = binding else { continue };
        if kind != "array" {
            continue;
        }
        let held = held_mask(document, key, shape, true)?;
        let n_held = held.iter().filter(|h| **h).count();
        if n_held == 0 {
            continue;
        }
        let prior = match masks.get(coordinate) {
            None => vec![true; held.len()],
            Some(existing) => {
                let (s, m) = crate::py::bool_array(existing.get("designable").unwrap_or(&Value::Null))?;
                if s != *shape {
                    return Err(verr("validated coordinate mask has incompatible shape"));
                }
                m
            }
        };
        let designable: Vec<bool> = prior.iter().zip(&held).map(|(p, h)| *p && !*h).collect();
        masks.insert(coordinate.clone(), json!({"designable": crate::py::nested_bool(shape, &designable)}));
        let prior_source = sources.get(coordinate).cloned().unwrap_or(Value::Null);
        sources.insert(
            coordinate.clone(),
            json!({"kind": "intersection_with_document_design_holds", "held_values": n_held,
                "array": key, "prior": prior_source}),
        );
    }
    Ok(())
}

#[must_use]
pub fn key_str(v: &Value) -> String {
    py_str(v)
}
