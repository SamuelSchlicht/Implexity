// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::document::arrays::{b64decode_strict, decode_array, encode_array, inline_entry};
use crate::error::{GResult, GeometryError};
use crate::lattice::assembly::ControlledAssembly;
use crate::lattice::controls::{control_components, validate_control};
use crate::node::{Attr, ConstructArgs, Node};
use crate::value::{NdArray, ParamValue};

pub const SEPARATOR: &str = "::component::";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentIndex {
    Single(usize),
    Volume(String, usize),
}

fn verr(m: &str) -> GeometryError {
    GeometryError::Value(m.into())
}

fn valid_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}


pub fn split_component_id(field_id: &str) -> GResult<Option<(String, ComponentIndex)>> {
    let Some((base, name)) = field_id.split_once(SEPARATOR) else { return Ok(None) };
    let names: Vec<String> = control_components().into_iter().map(|c| c.name).collect();
    if base.is_empty() {
        return Err(verr("unknown native control component"));
    }
    if let Some((volume, component)) = name.split_once(':') {
        let Some(i) = names.iter().position(|n| n == component) else {
            return Err(verr("unknown named-volume control component"));
        };
        if !valid_ident(volume) {
            return Err(verr("unknown named-volume control component"));
        }
        return Ok(Some((base.to_string(), ComponentIndex::Volume(volume.to_string(), i))));
    }
    let Some(i) = names.iter().position(|n| n == name) else {
        return Err(verr("unknown native control component"));
    };
    Ok(Some((base.to_string(), ComponentIndex::Single(i))))
}

fn owners<'a>(document: &'a Value, base: &str) -> Vec<&'a Value> {
    document
        .get("nodes")
        .and_then(Value::as_object)
        .map(|m| {
            m.values()
                .filter(|n| {
                    matches!(
                        n.get("kind").and_then(Value::as_str),
                        Some("lattice.controlled" | "lattice.controlled_assembly")
                    ) && n.pointer("/params/control/array").and_then(Value::as_str) == Some(base)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn build_assembly(attrs: &Value, control: Option<NdArray>) -> GResult<Node> {
    let attrs: BTreeMap<String, Attr> = attrs
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), Attr::from_json(v))).collect())
        .unwrap_or_default();
    let mut params = BTreeMap::new();
    if let Some(c) = control {
        params.insert("control".to_string(), ParamValue::Array(std::sync::Arc::new(c)));
    }
    ControlledAssembly::entry().construct.as_ref()(ConstructArgs {
        children: Vec::new(),
        names: None,
        params,
        attrs,
    })
}


pub fn component_source(document: &Value, field_id: &str) -> GResult<(String, usize, NdArray, Value)> {
    let (base, index) =
        split_component_id(field_id)?.ok_or_else(|| verr("an explicit native component is required"))?;
    let own = owners(document, &base);
    if own.len() != 1 {
        return Err(verr("component must have one authoritative controlled-lattice owner"));
    }
    let owner = own[0];
    let attrs = owner.get("attrs").cloned().unwrap_or_else(|| json!({}));
    let entry = document.pointer(&format!("/arrays/{}", base.replace('~', "~0").replace('/', "~1")));
    let Some(entry) = entry.filter(|e| e.get("b64").is_some()) else {
        return Err(verr("component editing requires an inline authoritative tensor"));
    };
    let raw = b64decode_strict(entry["b64"].as_str().unwrap_or_default()).map_err(GeometryError::Value)?;
    let decoded = decode_array(entry, &raw)?;
    if owner["kind"] == "lattice.controlled_assembly" {
        let node = build_assembly(&attrs, Some(decoded))?;
        let asm =
            node.op().as_any().downcast_ref::<ControlledAssembly>().ok_or_else(|| verr("not an assembly"))?;
        let ComponentIndex::Volume(volume, local) = index else {
            return Err(verr("assembly editing requires an explicit volume id"));
        };
        let Some(vi) = asm.volumes.iter().position(|v| v.id == volume) else {
            return Err(verr("unknown assembly volume"));
        };
        let index = 20 * vi + local;
        let layout = asm.control_layout()?;
        let registration = layout["components"][index]["registration"].clone();
        let tensor = match node.param("control") {
            Some(ParamValue::Array(a)) => (**a).clone(),
            _ => return Err(verr("assembly without controls")),
        };
        return Ok((base, index, tensor, registration));
    }
    let ComponentIndex::Single(index) = index else {
        return Err(verr("a single lattice has no named-volume component"));
    };
    let grid = attrs.get("control_grid").and_then(Value::as_array).and_then(|g| {
        let v: Option<Vec<usize>> =
            g.iter().map(|x| x.as_u64().and_then(|u| usize::try_from(u).ok())).collect();
        v.filter(|v| v.len() == 3).map(|v| [v[0], v[1], v[2]])
    });
    let c = validate_control(decoded.shape(), &decoded.to_f64_vec(), decoded.dtype().kind(), grid)?;
    let triple = |k: &str, d: [f64; 3]| -> Option<Vec<f64>> {
        match attrs.get(k) {
            None => Some(d.to_vec()),
            Some(v) => v.as_array().and_then(|a| a.iter().map(Value::as_f64).collect()),
        }
    };
    let origin = triple("origin_mm", [0.0; 3]);
    let domain = attrs
        .get("domain_mm")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().map(Value::as_f64).collect::<Option<Vec<f64>>>());
    let (Some(origin), Some(domain)) = (origin, domain) else {
        return Err(verr("invalid native component domain"));
    };
    if origin.len() != 3
        || domain.len() != 3
        || !origin.iter().chain(&domain).all(|v| v.is_finite())
        || domain.iter().any(|v| *v <= 0.0)
    {
        return Err(verr("invalid native component domain"));
    }
    #[allow(clippy::cast_precision_loss)]
    let basis: Vec<Vec<f64>> = (0..3)
        .map(|i| (0..3).map(|j| if i == j { domain[i] / (c.grid[i] as f64 - 1.0) } else { 0.0 }).collect())
        .collect();
    let registration = json!({"schema": "implexity-grid-registration/1", "shape": c.grid, "origin": origin, "basis": basis,
        "centering": "node", "axis_order": "xyz", "frame": "model"});
    let tensor = NdArray::from_f64(vec![20, c.grid[0], c.grid[1], c.grid[2]], c.data)
        .ok_or_else(|| verr("bad tensor"))?;
    Ok((base, index, tensor, registration))
}


pub fn replace_component(
    document: &Value,
    field_id: &str,
    values: &[f64],
    registration: &Value,
) -> GResult<Value> {
    let (base, index, tensor, expected) = component_source(document, field_id)?;
    for key in ["shape", "origin", "basis", "centering", "axis_order", "frame"] {
        if registration.get(key) != expected.get(key) {
            return Err(verr("native component registration changed"));
        }
    }
    let shape = tensor.shape().to_vec();
    let n: usize = shape[1..].iter().product();
    if values.len() != n || !values.iter().all(|v| v.is_finite()) {
        return Err(verr("invalid native component values"));
    }
    let mut data = tensor.to_f64_vec();
    data[index * n..(index + 1) * n].copy_from_slice(values);
    let updated = NdArray::from_f64(shape, data).ok_or_else(|| verr("bad tensor"))?;
    let mut out = document.clone();
    let (entry, raw) = encode_array(&updated);
    if let Some(arrays) = out.get_mut("arrays").and_then(Value::as_object_mut) {
        arrays.insert(base, inline_entry(&entry, &raw));
    }
    Ok(out)
}


pub fn component_catalogue(document: &Value, base: &str) -> GResult<Option<Vec<Value>>> {
    let own = owners(document, base);
    if own.is_empty() {
        return Ok(None);
    }
    if own.len() != 1 {
        return Err(verr("native component tensor must have one authoritative owner"));
    }
    let owner = own[0];
    let rows: Vec<Value> = if owner["kind"] == "lattice.controlled" {
        control_components()
            .into_iter()
            .map(|c| json!({"name": c.name, "label": c.label, "units": c.units}))
            .collect()
    } else {
        let node = build_assembly(owner.get("attrs").unwrap_or(&json!({})), None)?;
        let asm =
            node.op().as_any().downcast_ref::<ControlledAssembly>().ok_or_else(|| verr("not an assembly"))?;
        asm.control_layout()?["components"].as_array().cloned().unwrap_or_default()
    };
    let first = rows.first().and_then(|r| r["name"].as_str()).unwrap_or_default();
    component_source(document, &format!("{base}{SEPARATOR}{first}"))?;
    Ok(Some(
        rows.iter()
            .map(|row| {
                let mut m = serde_json::Map::new();
                m.insert(
                    "field_id".into(),
                    json!(format!("{base}{SEPARATOR}{}", row["name"].as_str().unwrap_or_default())),
                );
                m.insert("label".into(), row["label"].clone());
                m.insert("units".into(), row["units"].clone());
                if row.get("volume_id").is_some() {
                    m.insert("volume_id".into(), row["volume_id"].clone());
                    m.insert("control_index".into(), row["control_index"].clone());
                }
                Value::Object(m)
            })
            .collect(),
    ))
}
