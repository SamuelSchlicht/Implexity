// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use implexity_geometry::lattice::assembly::ControlledAssembly;
use implexity_geometry::lattice::freeze::{FROZEN_SCHEMA, FrozenGeometry};
use implexity_geometry::lattice::node::ControlledLattice;
use implexity_geometry::node::{Attr, Node};
use implexity_geometry::value::ParamValue;

use crate::error::{AResult, AuthoringError};
use crate::geometry_holds::{decode_runs, encode_runs, runs_json};
use crate::geometry_sculpt::{SculptOp, construct, influence, native_owner};
use crate::py::{Arr, jfs, obj_mut, py_str, setdefault_obj};

pub const SCHEMA: &str = FROZEN_SCHEMA;

fn verr(m: &str) -> AuthoringError {
    AuthoringError::value("ValueError", m)
}


pub fn normalise(raw: &Value) -> AResult<Option<Value>> {
    if raw.is_null() {
        return Ok(None);
    }
    Ok(FrozenGeometry::normalise(Some(&Attr::from_json(raw)))?.map(|f| f.to_json()))
}

fn halo(mask: &[bool], shape: [usize; 3]) -> Vec<bool> {
    let mut out = vec![false; mask.len()];
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                let mut any = false;
                'n: for di in -1i64..=1 {
                    for dj in -1i64..=1 {
                        for dk in -1i64..=1 {
                            let (a, b, c) = (i as i64 + di, j as i64 + dj, k as i64 + dk);
                            if a < 0
                                || b < 0
                                || c < 0
                                || a >= shape[0] as i64
                                || b >= shape[1] as i64
                                || c >= shape[2] as i64
                            {
                                continue;
                            }
                            if mask[(a as usize * shape[1] + b as usize) * shape[2] + c as usize] {
                                any = true;
                                break 'n;
                            }
                        }
                    }
                }
                out[(i * shape[1] + j) * shape[2] + k] = any;
            }
        }
    }
    out
}

fn full_node(spec: &Value, tensor: &implexity_geometry::value::NdArray) -> AResult<Node> {
    let kind = spec.get("kind").and_then(Value::as_str).unwrap_or("lattice.controlled");
    let mut params = BTreeMap::new();
    params.insert("control".to_string(), ParamValue::Array(Arc::new(tensor.clone())));
    construct(kind, spec.get("attrs").unwrap_or(&json!({})), params)
}


pub fn edit_frozen_geometry(document: &Value, field_id: &str, op: &SculptOp) -> AResult<(Value, Value)> {
    let owner = native_owner(document, field_id)?;
    let spec = document
        .get("nodes")
        .and_then(|n| n.get(&owner.node_id))
        .cloned()
        .ok_or_else(|| AuthoringError::Key(crate::py::repr(&Value::from(owner.node_id.clone()))))?;
    let node = full_node(&spec, &owner.tensor)?;
    let assembly = node.op().as_any().downcast_ref::<ControlledAssembly>();
    let lattice = node.op().as_any().downcast_ref::<ControlledLattice>();
    let frozen = match (assembly, lattice) {
        (Some(a), _) => a.frozen_geometry.clone(),
        (None, Some(l)) => l.spec.frozen_geometry.clone(),
        _ => return Err(verr("frozen geometry requires a controlled lattice owner")),
    };
    let mut raw: Option<Value> = frozen.map(|f| f.to_json());
    let shape: [usize; 3] = match &raw {
        Some(r) => {
            let s: Vec<usize> = r["shape"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_u64().map(|u| u as usize)).collect())
                .unwrap_or_default();
            [s[0], s[1], s[2]]
        }
        None => match (assembly, lattice) {
            (Some(a), _) => a.analysis_grid,
            (None, Some(l)) => l.spec.geometry_shape(),
            _ => [2, 2, 2],
        },
    };
    let count: usize = shape.iter().product();
    if count > 2_000_000 {
        return Err(verr(
            "freeze capture exceeds two million samples; author a resolved smaller reference grid explicitly",
        ));
    }
    let vec3 = |v: &Value| -> [f64; 3] {
        let a = v.as_array().cloned().unwrap_or_default();
        std::array::from_fn(|i| a.get(i).and_then(Value::as_f64).unwrap_or(f64::NAN))
    };
    let (node_origin, node_domain) = match (assembly, lattice) {
        (Some(a), _) => (a.origin_mm, a.domain_mm),
        (None, Some(l)) => (l.spec.origin_mm, l.spec.domain_mm),
        _ => ([0.0; 3], [1.0; 3]),
    };
    let origin = raw.as_ref().map_or(node_origin, |r| vec3(&r["origin_mm"]));
    let domain = raw.as_ref().map_or(node_domain, |r| vec3(&r["domain_mm"]));
    let axes: Vec<Vec<f64>> = (0..3)
        .map(|i| (0..shape[i]).map(|k| origin[i] + (k as f64 + 0.5) * domain[i] / shape[i] as f64).collect())
        .collect();
    let mut xyz = Vec::with_capacity(count);
    for x in &axes[0] {
        for y in &axes[1] {
            for z in &axes[2] {
                xyz.push([*x, *y, *z]);
            }
        }
    }
    let (rho, phase): (Vec<f64>, Vec<f64>) = if let Some(a) = assembly {
        let vf = a.volume_fields(&node)?;
        let rhos: Vec<Vec<f64>> = vf.iter().map(|f| f.rho.clone()).collect();
        let phases: Vec<Vec<f64>> = vf.iter().map(|f| f.phase_fraction.clone()).collect();
        xyz.iter().map(|p| a.fields_at::<f64>(&rhos, &phases, *p)).unzip()
    } else if let Some(l) = lattice {
        let gf = l.geometry_fields(&node)?;
        if shape == l.spec.geometry_shape() && origin == l.spec.origin_mm && domain == l.spec.domain_mm {
            (gf.rho.clone(), gf.phase_fraction.clone())
        } else {
            xyz.iter().map(|p| l.spec.sample_const::<f64>(&gf.rho, &gf.phase_fraction, *p)).unzip()
        }
    } else {
        return Err(verr("frozen geometry requires a controlled lattice owner"));
    };
    let mut raw_map = if let Some(Value::Object(m)) = raw.take() {
        m
    } else {
        let r = json!({"schema": SCHEMA, "shape": shape, "origin_mm": jfs(&origin), "domain_mm": jfs(&domain),
            "occupancy_runs": [], "phase_runs": [], "occupancy": jfs(&rho), "phase_fraction": jfs(&phase)});
        r.as_object().cloned().unwrap_or_default()
    };
    let mut masks: BTreeMap<&str, Vec<bool>> = BTreeMap::new();
    for key in ["occupancy_runs", "phase_runs"] {
        masks.insert(key, decode_runs(raw_map.get(key).unwrap_or(&Value::Null), &shape)?);
    }
    let infl = influence(&xyz, op, true)?;
    let selected = halo(&infl.iter().map(|v| *v > 0.0).collect::<Vec<_>>(), shape);
    let release = op.tool == "release";
    for (mkey, rkey, field) in
        [("occupancy_runs", "occupancy", &rho), ("phase_runs", "phase_fraction", &phase)]
    {
        if mkey == "phase_runs" && !op.protect_phase && !release {
            continue;
        }
        let mut reference = Arr::from_opt(raw_map.get(rkey))?.data;
        let mask = masks.get_mut(mkey).ok_or_else(|| verr("missing mask"))?;
        if release {
            for (m, s) in mask.iter_mut().zip(&selected) {
                if *s {
                    *m = false;
                }
            }
        } else {
            for i in 0..mask.len() {
                if selected[i] && !mask[i] {
                    reference[i] = field[i];
                }
            }
            for (m, s) in mask.iter_mut().zip(&selected) {
                if *s {
                    *m = true;
                }
            }
        }
        raw_map.insert(rkey.into(), jfs(&reference));
        raw_map.insert(mkey.into(), runs_json(&encode_runs(mask)));
    }
    let mut out = document.clone();
    let attrs = setdefault_obj(
        obj_mut(&mut out)?
            .get_mut("nodes")
            .and_then(Value::as_object_mut)
            .and_then(|n| n.get_mut(&owner.node_id))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| AuthoringError::Key("'nodes'".into()))?,
        "attrs",
    )?;
    let any = masks.values().any(|m| m.iter().any(|v| *v));
    if any {
        let normalised = normalise(&Value::Object(raw_map))?.unwrap_or(Value::Null);
        attrs.insert("frozen_geometry".into(), normalised);
    } else {
        attrs.shift_remove("frozen_geometry");
    }
    let c = |k: &str| masks.get(k).map_or(0, |m| m.iter().filter(|v| **v).count());
    Ok((
        out,
        json!({"protection": "sampled_geometry", "frozen_occupancy_samples": c("occupancy_runs"),
            "frozen_phase_samples": c("phase_runs"), "reference_grid": shape,
            "interpolation_halo_cells": 1, "scope": "root controlled volume after composition",
            "constraint": "same frozen occupancy field in viewer and geometry-design-map; zero local shape sensitivity on the mask plateau"}),
    ))
}

fn doc_attrs(kind: &str, attrs: &Value) -> AResult<Value> {
    let node = construct(kind, attrs, BTreeMap::new())?;
    Ok(Value::Object(node.op().doc_attrs().into_iter().map(|(k, v)| (k, v.to_json())).collect()))
}


pub fn rebind_matching_maps(problem: &Value, before: &Value, after: &Value) -> AResult<(Value, usize)> {
    const KINDS: [&str; 2] = ["lattice.controlled", "lattice.controlled_assembly"];
    let mut pairs: Vec<(String, Value, Value)> = Vec::new();
    if let Some(nodes) = before.get("nodes").and_then(Value::as_object) {
        for (nid, spec) in nodes {
            let kind = spec.get("kind").and_then(Value::as_str).unwrap_or_default();
            let Some(new) = after.get("nodes").and_then(|n| n.get(nid)) else { continue };
            if !KINDS.contains(&kind) || new.get("kind").and_then(Value::as_str) != Some(kind) {
                continue;
            }
            let old_attrs = doc_attrs(kind, spec.get("attrs").unwrap_or(&json!({})))?;
            let new_attrs = doc_attrs(kind, new.get("attrs").unwrap_or(&json!({})))?;
            if old_attrs != new_attrs {
                pairs.push((kind.to_string(), old_attrs, new_attrs));
            }
        }
    }
    let mut out = problem.clone();
    let mut changed = 0;
    fn walk(value: &mut Value, pairs: &[(String, Value, Value)], changed: &mut usize) {
        match value {
            Value::Object(m) => {
                let schema = m.get("schema").and_then(Value::as_str);
                if matches!(
                    schema,
                    Some("implexity-geometry-design-map/1" | "implexity-geometry-design-map/2")
                ) {
                    let kind = m.get("kind").map_or_else(|| "lattice.controlled".to_string(), py_str);
                    if KINDS.contains(&kind.as_str()) {
                        let attrs = doc_attrs(&kind, m.get("geometry").unwrap_or(&json!({}))).ok();
                        let matches: Vec<&Value> = pairs
                            .iter()
                            .filter(|(k, old, _)| *k == kind && attrs.as_ref() == Some(old))
                            .map(|(_, _, new)| new)
                            .collect();
                        if matches.len() == 1 {
                            m.insert("geometry".into(), matches[0].clone());
                            *changed += 1;
                        }
                    }
                    return;
                }
                for v in m.values_mut() {
                    walk(v, pairs, changed);
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v, pairs, changed);
                }
            }
            _ => {}
        }
    }
    walk(&mut out, &pairs, &mut changed);
    Ok((out, changed))
}
