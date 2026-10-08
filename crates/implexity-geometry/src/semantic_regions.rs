// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use crate::document::py_str;
use crate::numpy;
use crate::pyfmt::str_repr;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SemanticRegionError(pub String);

type SR<T> = Result<T, SemanticRegionError>;

fn err<T>(m: impl Into<String>) -> SR<T> {
    Err(SemanticRegionError(m.into()))
}

fn truthy(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    })
}

fn flat_floats(v: &Value, out: &mut Vec<f64>) -> bool {
    match v {
        Value::Array(a) => a.iter().all(|x| flat_floats(x, out)),
        Value::Number(n) => {
            out.push(n.as_f64().unwrap_or(f64::NAN));
            true
        }
        Value::Bool(b) => {
            out.push(f64::from(u8::from(*b)));
            true
        }
        Value::String(s) => s.trim().parse::<f64>().map(|x| out.push(x)).is_ok(),
        Value::Null => {
            out.push(f64::NAN);
            true
        }
        Value::Object(_) => false,
    }
}

fn vec3(v: Option<&Value>, name: &str) -> SR<[f64; 3]> {
    let mut out = Vec::new();
    let ok = flat_floats(v.unwrap_or(&Value::Null), &mut out);
    if !ok || out.len() != 3 || !out.iter().all(|x| x.is_finite()) {
        return err(format!("{name} must contain three finite values"));
    }
    Ok([out[0], out[1], out[2]])
}

fn unit(v: Option<&Value>, name: &str) -> SR<[f64; 3]> {
    let a = vec3(v, name)?;
    let n = numpy::norm(&a);
    if !n.is_finite() || n <= 1e-14 {
        return err(format!("{name} must have a finite nonzero norm"));
    }
    Ok(a.map(|x| x / n))
}

fn mapping(v: Option<&Value>, name: &str) -> SR<Map<String, Value>> {
    match v {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m.clone()),
        _ => err(format!("{name} must be an object")),
    }
}

fn adjacency(v: Option<&Value>) -> SR<Vec<String>> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for x in a {
                match x {
                    Value::String(s) if !s.is_empty() => out.push(s.clone()),
                    _ => return err("adjacent_to must be an array of nonempty strings"),
                }
            }
            out.sort();
            Ok(out)
        }
        _ => err("adjacent_to must be an array of nonempty strings"),
    }
}

fn s_or_empty(v: Option<&Value>) -> String {
    truthy(v).map(py_str).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Descriptor {
    pub id: String,
    pub anchor_mm: [f64; 3],
    pub normal: [f64; 3],
    pub radius_mm: f64,
    pub role: String,
    pub phase: String,
    pub adjacent_to: Vec<String>,
    pub source_node: String,
}

fn get_or<'a>(m: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    for k in keys {
        if let Some(v) = m.get(*k) {
            return Some(v);
        }
    }
    None
}


pub fn semantic_descriptor(region: &Value) -> SR<Descriptor> {
    let Some(r) = region.as_object() else { return err("region must be an object") };
    let patch_v = truthy(r.get("selection")).or_else(|| truthy(r.get("surface_patch"))).unwrap_or(region);
    let Some(patch) = patch_v.as_object() else { return err("region selection must be an object") };
    let tracking = mapping(patch.get("tracking"), "tracking")?;
    let sem = mapping(truthy(tracking.get("semantic")).or_else(|| r.get("semantic")), "semantic identity")?;
    let anchor = vec3(get_or(patch, &["anchor_mm", "point", "center"]), "anchor")?;
    let default_normal = json!([0, 0, 1]);
    let normal = unit(get_or(patch, &["normal", "surface_normal"]).or(Some(&default_normal)), "normal")?;
    let radius = match get_or(patch, &["radius_mm", "radius"]) {
        None => 1.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::String(s)) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| SemanticRegionError("region radius must be finite and positive".into()))?,
        Some(_) => return err("region radius must be finite and positive"),
    };
    if !radius.is_finite() || radius <= 0.0 {
        return err("region radius must be finite and positive");
    }
    Ok(Descriptor {
        id: truthy(r.get("id")).or_else(|| truthy(patch.get("id"))).map(py_str).unwrap_or_default(),
        anchor_mm: anchor,
        normal,
        radius_mm: radius,
        role: s_or_empty(sem.get("role")),
        phase: s_or_empty(sem.get("phase")),
        adjacent_to: adjacency(get_or(&sem, &["adjacent_to", "adjacentTo"]))?,
        source_node: truthy(sem.get("source_node"))
            .or_else(|| truthy(tracking.get("node_id")))
            .map(py_str)
            .unwrap_or_default(),
    })
}

fn candidate(c: &Value) -> SR<Descriptor> {
    let Some(m) = c.as_object() else { return err("candidate must be an object") };
    match m.get("id") {
        Some(Value::String(s)) if !s.trim().is_empty() => {}
        _ => return err("candidate requires a nonempty string id"),
    }
    let default_normal = json!([0, 0, 1]);
    Ok(Descriptor {
        id: s_or_empty(m.get("id")),
        anchor_mm: vec3(get_or(m, &["anchor_mm", "point"]), "candidate anchor")?,
        normal: unit(m.get("normal").or(Some(&default_normal)), "candidate normal")?,
        radius_mm: 1.0,
        role: s_or_empty(m.get("role")),
        phase: s_or_empty(m.get("phase")),
        adjacent_to: adjacency(get_or(m, &["adjacent_to", "adjacentTo"]))?,
        source_node: s_or_empty(get_or(m, &["source_node", "sourceNode"])),
    })
}


pub fn rebind_semantic_region(
    region: &Value,
    candidates: &Value,
    maximum_distance_factor: f64,
    ambiguity_ratio: f64,
    minimum_normal_alignment: f64,
) -> SR<Value> {
    let d = semantic_descriptor(region)?;
    let Some(cands) = candidates.as_array() else { return err("candidates must be an array") };
    if cands.is_empty() {
        return err("the semantic region has no surviving candidate surface");
    }
    let tol = [maximum_distance_factor, ambiguity_ratio, minimum_normal_alignment];
    if !tol.iter().all(|x| x.is_finite())
        || maximum_distance_factor <= 0.0
        || ambiguity_ratio < 1.0
        || !(-1.0..=1.0).contains(&minimum_normal_alignment)
    {
        return err("invalid semantic matching tolerances");
    }
    let radius = d.radius_mm;
    let mut scored: Vec<(f64, Descriptor, f64, f64)> = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for raw in cands {
        let c = candidate(raw)?;
        if !ids.insert(c.id.clone()) {
            return err("candidate ids must be unique");
        }
        let incompatible = (!d.role.is_empty() && d.role != c.role)
            || (!d.phase.is_empty() && d.phase != c.phase)
            || (!d.source_node.is_empty() && d.source_node != c.source_node);
        if incompatible || (!d.adjacent_to.is_empty() && d.adjacent_to != c.adjacent_to) {
            continue;
        }
        let dist = numpy::norm(&[
            c.anchor_mm[0] - d.anchor_mm[0],
            c.anchor_mm[1] - d.anchor_mm[1],
            c.anchor_mm[2] - d.anchor_mm[2],
        ]);
        let align =
            c.normal[2].mul_add(d.normal[2], c.normal[1].mul_add(d.normal[1], c.normal[0] * d.normal[0]));
        if dist > maximum_distance_factor * radius || align < minimum_normal_alignment {
            continue;
        }
        let role_bonus = if d.role.is_empty() || d.role == c.role { 0.0 } else { 2.0 };
        let phase_bonus = if d.phase.is_empty() || d.phase == c.phase { 0.0 } else { 2.0 };
        let score = dist / radius + 0.75 * (1.0 - align) + role_bonus + phase_bonus;
        scored.push((score, c, dist, align));
    }
    if scored.is_empty() {
        return err(
            "no surviving surface satisfies the region's semantic role, phase, ancestry and orientation",
        );
    }
    scored.sort_by(|a, b| a.0.total_cmp(&b.0));
    let best = &scored[0];
    if let Some(second) = scored.get(1)
        && second.0 <= (best.0 * ambiguity_ratio).max(best.0 + 0.08)
    {
        return err(format!(
            "semantic region rebinding is ambiguous between {} and {}",
            str_repr(&best.1.id),
            str_repr(&second.1.id)
        ));
    }
    Ok(
        json!({"ok": true, "region_id": d.id, "candidate_id": best.1.id, "distance_mm": best.2, "normal_alignment": best.3,
        "score": best.0, "evidence": {"role": d.role, "phase": d.phase, "adjacent_to": d.adjacent_to, "source_node": d.source_node}}),
    )
}
