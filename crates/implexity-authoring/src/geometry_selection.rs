// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_geometry::lattice::component_field::{ComponentIndex, split_component_id};

use crate::error::{AResult, AuthoringError};
use crate::field_interaction::{GridGeometry, flatten_json};
use crate::geometry_sculpt::{SculptOp, influence};
use crate::py::{canonical_ascii, jf, jfs, np_sum, obj_mut, path_obj, setdefault_obj, sha256_hex};

pub const SCHEMA: &str = "implexity-sculpt-selection/1";
pub const ACTIONS: [&str; 7] = ["replace", "add", "subtract", "intersect", "invert", "all", "clear"];
pub const MAX_SAMPLES: usize = 2_000_000;
pub const MAX_SLOTS: usize = 16;

fn verr(m: &str) -> AuthoringError {
    AuthoringError::value("ValueError", m)
}


pub fn selection_id(value: &Value) -> AResult<String> {
    let bad = || verr("selection_id must be 1..64 letters, digits, spaces, dots, underscores or hyphens");
    let Some(s) = value.as_str() else { return Err(bad()) };
    let b = s.as_bytes();
    let ok = !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b' ' | b'-'));
    if ok { Ok(s.to_string()) } else { Err(bad()) }
}


pub fn target_key(field_id: &str) -> AResult<String> {
    match split_component_id(field_id)? {
        None => Ok(field_id.to_string()),
        Some((base, index)) => {
            let volume = match index {
                ComponentIndex::Volume(v, _) => v,
                ComponentIndex::Single(_) => "single".to_string(),
            };
            Ok(format!("{base}::volume::{volume}"))
        }
    }
}

#[must_use]
pub fn registration_id(grid: &GridGeometry) -> String {
    sha256_hex(canonical_ascii(&grid.serialise()).as_bytes())
}

fn slots(document: &Value, key: &str) -> AResult<Map<String, Value>> {
    let interaction = path_obj(document, &["meta", "implexity", "interaction"]).cloned().unwrap_or_default();
    let bad = || verr("invalid persisted sculpt selection catalogue");
    match interaction.get("sculpt_selections") {
        None => Ok(Map::new()),
        Some(Value::Object(records)) => match records.get(key) {
            None => Ok(Map::new()),
            Some(Value::Object(m)) => Ok(m.clone()),
            Some(_) => Err(bad()),
        },
        Some(_) => Err(bad()),
    }
}


pub fn read_selection(
    document: &Value,
    field_id: &str,
    grid: &GridGeometry,
    name: &str,
    required: bool,
) -> AResult<Option<Vec<f64>>> {
    let name = selection_id(&Value::from(name))?;
    let key = target_key(field_id)?;
    let Some(record) = slots(document, &key)?.get(&name).cloned() else {
        if required {
            return Err(verr(
                "the requested sculpt selection does not exist; paint it or explicitly disable selection filtering",
            ));
        }
        return Ok(None);
    };
    if !record.is_object() || record.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(verr("invalid persisted sculpt selection"));
    }
    if record.get("target").and_then(Value::as_str) != Some(key.as_str())
        || record.get("registration_id").and_then(Value::as_str) != Some(registration_id(grid).as_str())
    {
        return Err(verr(
            "sculpt selection registration changed; recreate the selection on the current grid",
        ));
    }
    let count = grid.size();
    let raw = record.get("weights").cloned().unwrap_or(Value::Null);
    let shape_ok = raw.as_array().is_some_and(|a| a.len() == count && a.iter().all(Value::is_number));
    if count > MAX_SAMPLES || !shape_ok {
        return Err(verr("sculpt selection weights must match the registered grid"));
    }
    let values: Vec<f64> =
        flatten_json(&raw).unwrap_or_default().iter().map(|v| v.as_f64().unwrap_or(f64::NAN)).collect();
    if values.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
        return Err(verr("sculpt selection weights must be finite values in [0,1]"));
    }
    Ok(Some(values))
}

#[must_use]
pub fn selection_summary(values: &[f64], grid: &GridGeometry, name: &str, max_points: usize) -> Value {
    let indices: Vec<usize> = values.iter().enumerate().filter(|(_, v)| **v > 0.0).map(|(i, _)| i).collect();
    let xyz = grid.coordinates();
    let (centroid, bounds) = if indices.is_empty() {
        (Value::Null, Value::Null)
    } else {
        let mut num = [0.0; 3];
        let mut den = 0.0;
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &i in &indices {
            let w = values[i];
            den += w;
            for a in 0..3 {
                num[a] += xyz[i][a] * w;
                lo[a] = lo[a].min(xyz[i][a]);
                hi[a] = hi[a].max(xyz[i][a]);
            }
        }
        (jfs(&num.map(|v| v / den)), json!({"min_mm": jfs(&lo), "max_mm": jfs(&hi)}))
    };
    let shown: Vec<usize> = if indices.is_empty() {
        Vec::new()
    } else {
        let m = max_points.min(indices.len());
        implexity_mesh::numeric::linspace(0.0, (indices.len() - 1) as f64, m)
            .into_iter()
            .map(|p| indices[p.trunc() as usize])
            .collect()
    };
    json!({
        "schema": SCHEMA,
        "selection_id": name,
        "selected_samples": indices.len(),
        "fully_selected_samples": values.iter().filter(|v| **v == 1.0).count(),
        "weight_sum": jf(np_sum(values)),
        "centroid_mm": centroid,
        "bounds_mm": bounds,
        "points_mm": shown.iter().map(|i| jfs(&xyz[*i])).collect::<Vec<_>>(),
        "weights": shown.iter().map(|i| jf(values[*i])).collect::<Vec<_>>(),
        "overlay_sampled": shown.len() < indices.len(),
        "overlay_space": "model_mm_xray_control_samples",
        "role": "authoring influence only; not an optimization hold",
    })
}


pub fn catalogue(document: &Value, field_id: &str, grid: &GridGeometry) -> AResult<Vec<Value>> {
    let mut names: Vec<String> = slots(document, &target_key(field_id)?)?.keys().cloned().collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        match read_selection(document, field_id, grid, &name, true) {
            Ok(Some(values)) => {
                let mut s = selection_summary(&values, grid, &name, 1200);
                if let Some(m) = s.as_object_mut() {
                    m.insert("valid".into(), Value::Bool(true));
                }
                out.push(s);
            }
            Ok(None) => {}
            Err(e) if matches!(e.class(), "ValueError" | "TypeError") => {
                out.push(json!({"selection_id": name, "valid": false, "reason": e.to_string()}));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}


pub fn edit_selection(
    document: &Value,
    field_id: &str,
    grid: &GridGeometry,
    op: &SculptOp,
) -> AResult<(Value, Value)> {
    let name = op.selection_id.clone();
    let action = op.selection_action.as_str();
    if grid.size() > MAX_SAMPLES {
        return Err(verr("sculpt selection exceeds two million samples"));
    }
    let key = target_key(field_id)?;
    let existing = slots(document, &key)?;
    if !existing.contains_key(&name) && existing.len() >= MAX_SLOTS {
        return Err(verr("at most sixteen sculpt selection slots per volume are supported"));
    }
    let n = grid.size();
    let old = if matches!(action, "replace" | "all" | "clear") {
        vec![0.0; n]
    } else {
        read_selection(document, field_id, grid, &name, false)?.unwrap_or_else(|| vec![0.0; n])
    };
    let values: Vec<f64> = match action {
        "all" => vec![1.0; n],
        "clear" => vec![0.0; n],
        "invert" => old.iter().map(|v| 1.0 - v).collect(),
        _ => {
            let brush = influence(&grid.coordinates(), op, true)?;
            match action {
                "replace" => brush,
                "add" => old.iter().zip(&brush).map(|(a, b)| np_max(*a, *b)).collect(),
                "subtract" => old.iter().zip(&brush).map(|(a, b)| np_min(*a, 1.0 - b)).collect(),
                _ => old.iter().zip(&brush).map(|(a, b)| np_min(*a, *b)).collect(),
            }
        }
    };
    let values: Vec<f64> = values.iter().map(|v| crate::field_interaction::np_clip(*v, 0.0, 1.0)).collect();
    let mut changed = document.clone();
    let records = setdefault_obj(
        setdefault_obj(
            setdefault_obj(setdefault_obj(obj_mut(&mut changed)?, "meta")?, "implexity")?,
            "interaction",
        )?,
        "sculpt_selections",
    )?;
    setdefault_obj(records, &key)?.insert(
        name.clone(),
        json!({"schema": SCHEMA, "target": key, "registration_id": registration_id(grid), "weights": jfs(&values)}),
    );
    Ok((
        changed,
        json!({"changed_control_values": 0, "selection_only": true, "selection_action": action,
            "selection": selection_summary(&values, grid, &name, 1200)}),
    ))
}

#[must_use]
pub fn np_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

#[must_use]
pub fn np_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}
