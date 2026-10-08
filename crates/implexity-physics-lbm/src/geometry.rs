// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::{CaeError, CaeResult};
use implexity_geometry::document;
use implexity_geometry::eval::{EvalOptions, eval_points};
use implexity_geometry::field_registration::{GridRegistration, axis_aligned_registration};
use serde_json::{Map, Value, json};

use crate::d3q19::Grid;
use crate::nparray::asarray;


pub fn origin(value: &Value) -> CaeResult<[f64; 3]> {
    let a = asarray(value);
    if a.shape != [3] || !a.is_real() || !a.all_finite() {
        return Err(CaeError::contract("origin_m requires three finite real coordinates"));
    }
    Ok([a.data[0], a.data[1], a.data[2]])
}


pub fn registration(shape: [usize; 3], spacing_m: f64, origin_m: [f64; 3]) -> CaeResult<Value> {
    let lo = origin_m;
    let hi: [f64; 3] = std::array::from_fn(|a| lo[a] + shape[a] as f64 * spacing_m);
    if (0..3).any(|a| hi[a] <= lo[a]) || !hi.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract("Grid bounds are not representable at the requested origin/spacing"));
    }
    let registration =
        axis_aligned_registration(shape, lo.map(|v| v * 1000.0), hi.map(|v| v * 1000.0), "cell")
            .map_err(|e| CaeError::contract(e.to_string()))?;
    Ok(registration.to_wire())
}


pub fn check_registration(
    problem: &Map<String, Value>,
    shape: [usize; 3],
    spacing_m: f64,
    origin_m: [f64; 3],
) -> CaeResult<Value> {
    let expected = registration(shape, spacing_m, origin_m)?;
    if let Some(authored_raw) = problem.get("field_registration") {
        let authored = GridRegistration::from_wire(authored_raw)
            .map_err(|e| CaeError::contract(e.to_string()))?
            .to_wire();
        if authored != expected {
            return Err(CaeError::contract("LBM field registration must match origin, spacing and shape"));
        }
        if let Some(id) = authored_raw.get("registration_id")
            && Some(id) != expected.get("registration_id")
        {
            return Err(CaeError::contract("LBM field registration identity is stale"));
        }
    }
    Ok(expected)
}


pub fn registered_problem(
    problem: &Map<String, Value>,
    shape: [usize; 3],
    spacing_m: f64,
    origin_m: [f64; 3],
) -> CaeResult<Value> {
    let registration = check_registration(problem, shape, spacing_m, origin_m)?;
    let mut out = problem.clone();
    out.insert("field_registration".into(), registration);
    Ok(Value::Object(out))
}

#[must_use]
pub fn cell_centres_m(shape: [usize; 3], spacing_m: f64, origin_m: [f64; 3]) -> Vec<[f64; 3]> {
    let grid = Grid::new(shape);
    (0..grid.cells())
        .map(|cell| {
            let x = grid.coords(cell);
            std::array::from_fn(|a| origin_m[a] + (x[a] as f64 + 0.5) * spacing_m)
        })
        .collect()
}


pub fn field_metadata(shape: [usize; 3], spacing_m: f64, origin_m: [f64; 3]) -> CaeResult<Value> {
    let reg = registration(shape, spacing_m, origin_m)?;
    let mut result = Map::new();
    for (name, unit, rank) in [
        ("fluid_fraction", "1", "scalar"),
        ("density_kg_m3", "kg/m^3", "scalar"),
        ("velocity_m_s", "m/s", "vector"),
        ("gauge_pressure_Pa", "Pa", "scalar"),
        ("temperature_K", "K", "scalar"),
    ] {
        result.insert(
            name.into(),
            json!({"units": unit, "rank": rank, "association": "cell", "registration": reg.clone()}),
        );
    }
    for (name, unit, rank, association) in [
        ("temperature_history_K", "K", "scalar", "cell_history"),
        ("time_s", "s", "scalar", "time"),
        ("wall_link_positions_m", "m", "vector", "wall_link"),
        ("wall_link_gauge_force_N", "N", "vector", "wall_link"),
        ("wall_link_force_N", "N", "vector", "wall_link"),
        ("selected_wall_force_history_N", "N", "vector", "wall_link_history"),
        ("solid_displacement_history_m", "m", "vector", "node_history"),
        ("solid_stress_history_Pa", "Pa", "tensor", "element_history"),
        ("fatigue_usage_per_element", "1", "scalar", "element"),
    ] {
        result.insert(name.into(), json!({"units": unit, "rank": rank, "association": association}));
    }
    if let Some(Value::Object(m)) = result.get_mut("velocity_m_s") {
        m.insert("components".into(), json!(["x", "y", "z"]));
    }
    if let Some(Value::Object(m)) = result.get_mut("solid_stress_history_Pa") {
        m.insert("components".into(), json!(["xx", "yy", "zz", "xy", "yz", "xz"]));
    }
    Ok(Value::Object(result))
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampledMasks {
    pub solid: Vec<bool>,
    pub design: Vec<bool>,
    pub fixed: Vec<f64>,
    pub sampling: Value,
}

pub const ROLES: [&str; 3] = ["solid_nodes", "design_nodes", "protected_fluid_nodes"];


pub fn sample_roles(
    source: &Map<String, Value>,
    roles: &[&str],
    centres_m: &[[f64; 3]],
) -> CaeResult<Vec<Vec<bool>>> {
    let doc = source.get("document").cloned().unwrap_or(Value::Null);
    let model = document::build(&doc, None, None)
        .map_err(|e| CaeError::contract(format!("Invalid inline geometry document: {e}")))?;
    let points: Vec<[f64; 3]> = centres_m.iter().map(|p| p.map(|v| v * 1000.0)).collect();
    let options = EvalOptions { validate: false, ..EvalOptions::exact() };
    let mut masks = Vec::with_capacity(roles.len());
    for role in roles {
        let ids = source.get(*role).and_then(Value::as_array);
        let valid = ids.is_some_and(|ids| {
            let mut seen = std::collections::BTreeSet::new();
            ids.iter()
                .all(|n| n.as_str().is_some_and(|s| model.node_table().contains_key(s) && seen.insert(s)))
        });
        let Some(ids) = ids.filter(|_| valid) else {
            return Err(CaeError::contract(format!("{role} requires unique existing document node ids")));
        };
        let mut mask = vec![false; points.len()];
        for id in ids {
            let node =
                model.node(id.as_str().unwrap_or_default()).map_err(|e| CaeError::contract(e.to_string()))?;
            for start in (0..points.len()).step_by(65536) {
                let end = (start + 65536).min(points.len());
                let values = eval_points(&node, &points[start..end], &options)
                    .map_err(|e| CaeError::contract(e.to_string()))?;
                if values.len() != end - start || !values.iter().all(|v| v.is_finite()) {
                    return Err(CaeError::contract(
                        "Geometry evaluation requires one finite signed field value per cell",
                    ));
                }
                for (m, v) in mask[start..end].iter_mut().zip(&values) {
                    *m |= *v <= 0.0;
                }
            }
        }
        masks.push(mask);
    }
    Ok(masks)
}


pub fn masks_from_document(
    shape: [usize; 3],
    spacing_m: f64,
    origin_m: [f64; 3],
    source: &Value,
    masks_are_null: bool,
) -> CaeResult<SampledMasks> {
    let Some(source) = source
        .as_object()
        .filter(|m| m.len() == 4 && m.contains_key("document") && ROLES.iter().all(|r| m.contains_key(*r)))
    else {
        return Err(CaeError::contract(
            "geometry_source requires an inline native document and three node-id role lists",
        ));
    };
    if !masks_are_null {
        return Err(CaeError::contract(
            "geometry_source owns masks: set solid_mask, design_region and fixed_design to null",
        ));
    }
    let centres = cell_centres_m(shape, spacing_m, origin_m);
    let masks = sample_roles(source, &ROLES, &centres)?;
    let (solid, design_raw, protected) = (&masks[0], &masks[1], &masks[2]);
    if solid.iter().zip(protected).any(|(s, p)| *s && *p) {
        return Err(CaeError::contract("Sampled fixed solid overlaps protected fluid"));
    }
    let design: Vec<bool> = (0..solid.len()).map(|i| design_raw[i] && !solid[i] && !protected[i]).collect();
    if !design.iter().any(|v| *v) {
        return Err(CaeError::contract("Geometry source selects no editable fluid cells at this resolution"));
    }
    let fixed = solid.iter().map(|s| if *s { 0.0 } else { 1.0 }).collect();
    let mut counts = Map::new();
    for (role, mask) in ROLES.iter().zip(&masks) {
        counts.insert((*role).into(), json!(mask.iter().filter(|v| **v).count()));
    }
    let doc = source.get("document").cloned().unwrap_or(Value::Null);
    let sampling = json!({
        "document_sha256": document::sha256_of(&doc),
        "sampling": "cell_centres; native millimetres; field <= 0 is inside; no subcell reconstruction",
        "role_counts": counts,
        "editable_cells": design.iter().filter(|v| **v).count(),
    });
    Ok(SampledMasks { solid: solid.clone(), design, fixed, sampling })
}
