// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};

use implexity_core::error::{CaeError, CaeResult};
use implexity_core::wire::fingerprint_value;
use implexity_geometry::field_registration::GridRegistration;
use ndarray::{ArrayD, Axis};
use serde_json::{Map, Value, json};

fn err(message: &str) -> CaeError {
    CaeError::contract(message)
}

type Layout = (Vec<Map<String, Value>>, Vec<Value>, Vec<GridRegistration>);

fn component_layout(shape: &[usize], layout: &Value, registration: Option<&Value>) -> CaeResult<Layout> {
    let layout = layout
        .as_object()
        .filter(|l| {
            matches!(
                l.get("schema").and_then(Value::as_str),
                Some("implexity-component-field-layout/1" | "implexity-component-field-layout/2")
            )
        })
        .ok_or_else(|| err("unsupported component-gradient layout"))?;
    let declared: Option<Vec<usize>> =
        layout.get("shape").and_then(|s| serde_json::from_value(s.clone()).ok());
    if shape.len() != 4
        || shape.contains(&0)
        || declared.as_deref() != Some(shape)
        || !layout.get("component_axis").is_some_and(|v| v.is_i64() && v.as_i64() == Some(0))
        || layout.get("spatial_axes") != Some(&json!([1, 2, 3]))
    {
        return Err(err("component-gradient shape/axes disagree with the declared layout"));
    }
    let separate = layout["schema"] == "implexity-component-field-layout/2";
    if separate
        && (registration.is_some_and(|r| !r.is_null())
            || layout.get("spatial_registration").and_then(Value::as_str) != Some("per_component"))
    {
        return Err(err("per-component layout must not be assigned a fictitious common grid"));
    }
    let components = layout
        .get("components")
        .and_then(Value::as_array)
        .filter(|c| c.len() == shape[0])
        .ok_or_else(|| err("one description per gradient component required"))?;
    let mut rows = Vec::with_capacity(components.len());
    for (index, row) in components.iter().enumerate() {
        let r = row
            .as_object()
            .filter(|r| {
                r.get("index").is_some_and(|v| v.is_i64() && v.as_i64() == i64::try_from(index).ok())
                    && ["name", "label", "units"]
                        .iter()
                        .all(|k| r.get(*k).and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty()))
            })
            .ok_or_else(|| err("ordered named and unit-declared gradient components required"))?;
        rows.push(r.clone());
    }
    let names: BTreeSet<&str> = rows.iter().filter_map(|r| r["name"].as_str()).collect();
    if names.len() != rows.len() {
        return Err(err("component-gradient names must be unique"));
    }
    let registrations: Vec<Value> = rows
        .iter()
        .map(|r| {
            if separate {
                r.get("registration").cloned().unwrap_or(Value::Null)
            } else {
                registration.cloned().unwrap_or(Value::Null)
            }
        })
        .collect();
    let mut grids = Vec::with_capacity(rows.len());
    for raw in &registrations {
        if !raw.is_object() {
            return Err(err("component-gradient requires an explicit spatial registration"));
        }
        let grid = GridRegistration::from_wire(raw)
            .map_err(|_| err("component-gradient requires an explicit spatial registration"))?;
        if let Some(id) = raw.get("registration_id").filter(|v| !v.is_null()) {
            let wire: Map<String, Value> = raw
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, _)| *k != "registration_id")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if *id != Value::String(fingerprint_value(&Value::Object(wire))) {
                return Err(err("stale component-gradient registration identity"));
            }
        }
        if grid.shape[..] != shape[1..] {
            return Err(err("component-gradient registration does not match its spatial grid"));
        }
        grids.push(grid);
    }
    Ok((rows, registrations, grids))
}



pub fn add_registered_gradient_components(
    arrays: &mut BTreeMap<String, ArrayD<f64>>,
    metadata: &mut Map<String, Value>,
    field_name: &str,
    gradient: &ArrayD<f64>,
    layout: &Value,
    registration: Option<&Value>,
) -> CaeResult<Vec<String>> {
    if gradient.is_empty() || !gradient.iter().all(|v| v.is_finite()) {
        return Err(err("finite real component gradient required"));
    }
    let shape = gradient.shape().to_vec();
    let (components, registrations, grids) = component_layout(&shape, layout, registration)?;
    let mut base = metadata
        .get(field_name)
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| err("missing gradient field metadata"))?;
    base.shift_remove("registration");
    let mut full = base.clone();
    full.insert("registration".into(), Value::Null);
    full.insert("association".into(), json!("component_array"));
    full.insert("shape".into(), json!(shape));
    full.insert("axes".into(), json!(["component", "x", "y", "z"]));
    full.insert("component_layout".into(), layout.clone());
    full.insert(
        "display_contract".into(),
        json!("select_a_registered_signed_component; not_a_scalar_volume"),
    );
    metadata.insert(field_name.to_string(), Value::Object(full));
    let mut names = Vec::with_capacity(components.len());
    for (i, row) in components.iter().enumerate() {
        let key = format!("{field_name}__component_{i:03}");
        if arrays.contains_key(&key) || metadata.contains_key(&key) {
            return Err(err("gradient component artifact name collision"));
        }
        arrays.insert(key.clone(), gradient.index_axis(Axis(0), i).to_owned());
        let mut m = base.clone();
        let units = row["units"].as_str().unwrap_or_default();
        m.insert("shape".into(), json!(shape[1..]));
        m.insert("rank".into(), json!("scalar"));
        m.insert("association".into(), json!(grids[i].centering));
        m.insert("registration".into(), registrations[i].clone());
        m.insert("coordinate_units".into(), json!("mm"));
        m.insert("component_index".into(), json!(i));
        m.insert("component_name".into(), row["name"].clone());
        m.insert("component_label".into(), row["label"].clone());
        m.insert("parameter_units".into(), json!(units));
        m.insert("units".into(), json!(format!("response/{units}")));
        m.insert("full_tensor_field".into(), json!(field_name));
        metadata.insert(key.clone(), Value::Object(m));
        names.push(key);
    }
    Ok(names)
}



pub fn validate_registered_gradient_components(
    field_name: &str,
    metadata: &Map<String, Value>,
    names: &[String],
    fields: &Map<String, Value>,
    shape: &[usize],
) -> CaeResult<BTreeMap<String, usize>> {
    let expected_names: Vec<String> =
        (0..shape.first().copied().unwrap_or(0)).map(|i| format!("{field_name}__component_{i:03}")).collect();
    if names != expected_names.as_slice() || names.iter().any(|n| !fields.contains_key(n)) {
        return Err(err("ordered component-gradient fields disagree with the full tensor"));
    }
    if metadata.get("association").and_then(Value::as_str) != Some("component_array")
        || !metadata.get("registration").is_none_or(Value::is_null)
        || metadata.get("axes") != Some(&json!(["component", "x", "y", "z"]))
        || metadata.get("shape") != Some(&json!(shape))
    {
        return Err(err("full component-gradient metadata is inconsistent"));
    }
    let layout = metadata.get("component_layout").cloned().unwrap_or(Value::Null);
    let common = if layout.get("schema").and_then(Value::as_str) == Some("implexity-component-field-layout/1")
    {
        fields.get(&names[0]).and_then(|r| r.get("registration")).cloned()
    } else {
        None
    };
    let (components, registrations, grids) = component_layout(shape, &layout, common.as_ref())?;
    let get = |k: &str| metadata.get(k).cloned().unwrap_or(Value::Null);
    for (i, name) in names.iter().enumerate() {
        let row = fields
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| err("registered component-gradient metadata drifted"))?;
        let c = &components[i];
        let units = c["units"].as_str().unwrap_or_default();
        let expected = [
            ("shape", json!(shape[1..])),
            ("rank", json!("scalar")),
            ("association", json!(grids[i].centering)),
            ("registration", registrations[i].clone()),
            ("coordinate_units", json!("mm")),
            ("component_index", json!(i)),
            ("component_name", c["name"].clone()),
            ("component_label", c["label"].clone()),
            ("parameter_units", json!(units)),
            ("units", json!(format!("response/{units}"))),
            ("full_tensor_field", json!(field_name)),
            ("response", get("response")),
            ("parameter", get("parameter")),
            ("signed", get("signed")),
            ("source", get("source")),
        ];
        if expected.iter().any(|(k, v)| row.get(*k).unwrap_or(&Value::Null) != v) {
            return Err(err("registered component-gradient metadata drifted"));
        }
    }
    Ok(names.iter().enumerate().map(|(i, n)| (n.clone(), i)).collect())
}

