// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_geometry::document::arrays::{encode_array, inline_entry};
use implexity_geometry::lattice::component_field::{component_source, split_component_id};
use implexity_geometry::lattice::controls::{Controls, control_components};
use implexity_geometry::lattice::node::{ControlledLattice, LatticeSpec};
use implexity_geometry::linalg3::inv;
use implexity_geometry::node::{Attr, ConstructArgs, Node};
use implexity_geometry::value::{NdArray, ParamValue};

use crate::error::{AResult, AuthoringError};
use crate::field_interaction::{
    FieldBrushGesture, GridGeometry, matvec_row, np_clip, read_spatial_field, write_spatial_field,
};
use crate::py::{
    Arr, canonical_ascii, get, jf, jfs, norm3, obj_mut, py_float, py_str, row_norm3, sha256_hex, truthy,
};

pub const SCHEMA: &str = "implexity-geometry-sculpt/1";
pub const DEFORM_TOOLS: [&str; 6] = ["grab", "stretch", "twist", "scale", "bend", "taper"];
pub const TOOLS: [&str; 15] = [
    "grab", "stretch", "twist", "scale", "bend", "taper", "inflate", "deflate", "smooth", "add", "subtract",
    "flatten", "protect", "release", "select",
];

#[must_use]
pub fn native_tools() -> Vec<&'static str> {
    TOOLS.iter().copied().filter(|t| *t != "add" && *t != "flatten").collect()
}

pub(crate) fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("SculptError", message)
}

fn vector(value: Option<&Value>, name: &str) -> AResult<[f64; 3]> {
    let bad = || err(format!("{name} must be a finite model-coordinate triple"));
    let a = Arr::from_opt(value).map_err(|_| bad())?;
    match a.vec3() {
        Some(v) if v.iter().all(|x| x.is_finite()) => Ok(v),
        _ => Err(bad()),
    }
}

fn scalar(value: &Value, name: &str, lo: Option<f64>, hi: Option<f64>) -> AResult<f64> {
    if value.is_boolean() {
        return Err(err(format!("{name} must be numeric, not Boolean")));
    }
    let v = py_float(value)?;
    if !v.is_finite() || lo.is_some_and(|l| v < l) || hi.is_some_and(|h| v > h) {
        return Err(err(format!("{name} is nonfinite or outside its declared range")));
    }
    Ok(v)
}

#[derive(Clone, Debug)]
pub struct SculptOp {
    pub tool: String,
    pub center: [f64; 3],
    pub radius: f64,
    pub depth: f64,
    pub pivot: [f64; 3],
    pub bend_direction: [f64; 3],
    pub axial_length: f64,
    pub hardness: f64,
    pub selection_id: String,
    pub selection_action: String,
    pub normal: [f64; 3],
    pub axis: [f64; 3],
    pub delta: [f64; 3],
    pub amount: f64,
    pub factor: f64,
    pub angle: f64,
    pub strength: f64,
    pub scope: String,
    pub bounds: Option<([f64; 3], [f64; 3])>,
    pub points: Vec<[f64; 3]>,
    pub symmetry: Vec<String>,
    pub symmetry_origin: [f64; 3],
    pub falloff: String,
    pub protection: String,
    pub carry_phase: bool,
    pub protect_phase: bool,
    pub use_selection: bool,
    pub selection: Option<(Vec<f64>, GridGeometry)>,
}

const ALLOWED: [&str; 27] = [
    "tool",
    "center_mm",
    "radius_mm",
    "depth_mm",
    "normal",
    "direction",
    "delta_mm",
    "amount_mm",
    "factor",
    "angle_rad",
    "strength",
    "scope",
    "bounds_mm",
    "points_mm",
    "symmetry_axes",
    "symmetry_origin_mm",
    "carry_phase",
    "protect_phase",
    "falloff",
    "protection",
    "hardness",
    "pivot_mm",
    "bend_direction",
    "axial_length_mm",
    "use_selection",
    "selection_id",
    "selection_action",
];


pub fn normalise_operation(raw: &Value) -> AResult<SculptOp> {
    let Some(m) = raw.as_object() else { return Err(err("sculpt operation must be an object")) };
    let mut unknown: Vec<&String> = m.keys().filter(|k| !ALLOWED.contains(&k.as_str())).collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(err(format!(
            "unknown sculpt arguments: {}",
            unknown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }
    let tool = get(raw, "tool").map_or_else(|| "grab".to_string(), py_str);
    if !TOOLS.contains(&tool.as_str()) {
        return Err(err("unknown geometry sculpt tool"));
    }
    let center = vector(get(raw, "center_mm"), "center_mm")?;
    let radius = scalar(get(raw, "radius_mm").unwrap_or(&json!(1.0)), "radius_mm", Some(1e-12), None)?;
    let depth = scalar(get(raw, "depth_mm").unwrap_or(&jf(radius)), "depth_mm", Some(1e-12), None)?;
    let normal = vector(Some(get(raw, "normal").unwrap_or(&json!([0, 0, 1]))), "normal")?;
    let axis = match get(raw, "direction") {
        Some(v) => vector(Some(v), "direction")?,
        None => normal,
    };
    for (name, v) in [("normal", normal), ("direction", axis)] {
        if norm3(v) < 1e-12 {
            return Err(err(format!("{name} must be nonzero")));
        }
    }
    let scope = get(raw, "scope").map_or_else(|| "local".to_string(), py_str);
    if !["local", "box", "whole"].contains(&scope.as_str()) {
        return Err(err("scope must be local, box or whole"));
    }
    let mut bounds = None;
    if scope == "box" {
        let Some(b) = get(raw, "bounds_mm").filter(|b| b.is_object()) else {
            return Err(err("box scope requires bounds_mm"));
        };
        let lo = vector(get(b, "min_mm"), "box minimum")?;
        let hi = vector(get(b, "max_mm"), "box maximum")?;
        if (0..3).any(|a| hi[a] <= lo[a]) {
            return Err(err("box bounds must be strictly ordered"));
        }
        bounds = Some((lo, hi));
    }
    let points_raw = get(raw, "points_mm").cloned().unwrap_or_else(|| json!([jfs(&center)]));
    let pts = Arr::from_json(&points_raw).map_err(|_| err("points_mm requires 1..512 finite triples"))?;
    if pts.ndim() != 2 || pts.shape[1] != 3 || !(1..=512).contains(&pts.shape[0]) || !pts.all_finite() {
        return Err(err("points_mm requires 1..512 finite triples"));
    }
    let points: Vec<[f64; 3]> = pts.data.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    let syms: Vec<Value> = match get(raw, "symmetry_axes") {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => s.chars().map(|c| Value::from(c.to_string())).collect(),
        Some(Value::Object(o)) => o.keys().map(|k| Value::from(k.clone())).collect(),
        Some(_) => return Err(crate::py::type_error("object is not iterable")),
    };
    let mut symmetry = Vec::new();
    for s in &syms {
        match s.as_str() {
            Some(a @ ("x" | "y" | "z")) if !symmetry.iter().any(|x: &String| x == a) => {
                symmetry.push(a.to_string());
            }
            _ => return Err(err("symmetry_axes must contain distinct x, y or z")),
        }
    }
    let mut flags = [false; 3];
    for (i, name) in ["carry_phase", "protect_phase", "use_selection"].iter().enumerate() {
        match get(raw, name) {
            None => {}
            Some(Value::Bool(b)) => flags[i] = *b,
            Some(_) => return Err(err(format!("{name} must be Boolean"))),
        }
    }
    let falloff = get(raw, "falloff").map_or_else(|| "smoothstep".to_string(), py_str);
    if !["smoothstep", "linear", "constant"].contains(&falloff.as_str()) {
        return Err(err("sculpt falloff must be smoothstep, linear or constant"));
    }
    if DEFORM_TOOLS.contains(&tool.as_str()) && falloff != "smoothstep" && scope != "whole" {
        return Err(err("local coordinate flows require smoothstep falloff"));
    }
    let protection = get(raw, "protection").map_or_else(|| "geometry".to_string(), py_str);
    if protection != "geometry" && protection != "controls" {
        return Err(err("protection must be geometry or controls"));
    }
    let name = crate::geometry_selection::selection_id(get(raw, "selection_id").unwrap_or(&json!("main")))?;
    let action = get(raw, "selection_action").map_or_else(|| "replace".to_string(), py_str);
    if !crate::geometry_selection::ACTIONS.contains(&action.as_str()) {
        return Err(err("invalid selection action"));
    }
    if tool == "select" && flags[2] {
        return Err(err("selection painting must not filter itself through the old selection"));
    }
    let an = norm3(axis);
    let axis_unit = axis.map(|v| v / an);
    let abs: [f64; 3] = axis_unit.map(f64::abs);
    let mut amin = 0;
    for i in 1..3 {
        if abs[i] < abs[amin] {
            amin = i;
        }
    }
    let mut fallback = [0.0; 3];
    fallback[amin] = 1.0;
    let bend_raw = match get(raw, "bend_direction") {
        Some(v) => vector(Some(v), "bend_direction")?,
        None => fallback,
    };
    let dot = crate::py::dot3(bend_raw, axis_unit);
    let bend: [f64; 3] = std::array::from_fn(|a| bend_raw[a] - dot * axis_unit[a]);
    if tool == "bend" && norm3(bend) < 1e-12 {
        return Err(err("bend_direction must have a component perpendicular to direction"));
    }
    let bn = norm3(bend).max(1e-12);
    let bend = bend.map(|v| v / bn);
    let nn = norm3(normal);
    Ok(SculptOp {
        center,
        radius,
        depth,
        pivot: match get(raw, "pivot_mm") {
            Some(v) => vector(Some(v), "pivot_mm")?,
            None => center,
        },
        bend_direction: bend,
        axial_length: scalar(
            get(raw, "axial_length_mm").unwrap_or(&jf(radius)),
            "axial_length_mm",
            Some(1e-12),
            None,
        )?,
        hardness: scalar(get(raw, "hardness").unwrap_or(&json!(0.0)), "hardness", Some(0.0), Some(0.95))?,
        selection_id: name,
        selection_action: action,
        normal: normal.map(|v| v / nn),
        axis: axis_unit,
        delta: vector(Some(get(raw, "delta_mm").unwrap_or(&json!([0, 0, 0]))), "delta_mm")?,
        amount: scalar(get(raw, "amount_mm").unwrap_or(&json!(0.2)), "amount_mm", Some(0.0), None)?,
        factor: scalar(get(raw, "factor").unwrap_or(&json!(1.0)), "factor", Some(0.02), Some(50.0))?,
        angle: scalar(
            get(raw, "angle_rad").unwrap_or(&json!(0.0)),
            "angle_rad",
            Some(-4.0 * std::f64::consts::PI),
            Some(4.0 * std::f64::consts::PI),
        )?,
        strength: scalar(get(raw, "strength").unwrap_or(&json!(0.25)), "strength", Some(0.0), Some(1.0))?,
        scope,
        bounds,
        points,
        symmetry,
        symmetry_origin: vector(
            Some(get(raw, "symmetry_origin_mm").unwrap_or(&json!([0, 0, 0]))),
            "symmetry_origin_mm",
        )?,
        falloff,
        protection,
        carry_phase: flags[0],
        protect_phase: flags[1],
        use_selection: flags[2],
        selection: None,
        tool,
    })
}

fn np_round13(x: f64) -> f64 {
    let y = x * 1e13;
    let r = y.round_ties_even();
    r / 1e13
}

#[must_use]
pub fn symmetry_operations(op: &SculptOp) -> Vec<SculptOp> {
    let mut out: Vec<(Vec<f64>, SculptOp)> = Vec::new();
    for bits in 0..(1usize << op.symmetry.len()) {
        let mut signs = [1.0; 3];
        for (i, a) in op.symmetry.iter().enumerate() {
            if bits & (1 << i) != 0 {
                signs["xyz".find(a.as_str()).unwrap_or(0)] = -1.0;
            }
        }
        let o = op.symmetry_origin;
        let refl = |p: [f64; 3]| -> [f64; 3] { std::array::from_fn(|a| o[a] + signs[a] * (p[a] - o[a])) };
        let mul = |p: [f64; 3]| -> [f64; 3] { std::array::from_fn(|a| signs[a] * p[a]) };
        let mut item = op.clone();
        item.center = refl(op.center);
        item.points = op.points.iter().map(|p| refl(*p)).collect();
        item.pivot = refl(op.pivot);
        item.bend_direction = mul(op.bend_direction);
        item.normal = mul(op.normal);
        item.delta = mul(op.delta);
        let twist = if op.tool == "twist" { signs[0] * signs[1] * signs[2] } else { 1.0 };
        item.axis = std::array::from_fn(|a| signs[a] * op.axis[a] * twist);
        if let Some((lo, hi)) = op.bounds {
            let (a, b) = (refl(lo), refl(hi));
            item.bounds =
                Some((std::array::from_fn(|i| a[i].min(b[i])), std::array::from_fn(|i| a[i].max(b[i]))));
        }
        let mut key: Vec<f64> = Vec::new();
        for v in [item.center, item.pivot, item.normal, item.delta, item.axis] {
            key.extend(v);
        }
        if op.tool == "bend" {
            key.extend(item.bend_direction);
        }
        let key: Vec<f64> = key.into_iter().map(np_round13).collect();
        if !out.iter().any(|(k, _)| *k == key) {
            out.push((key, item));
        }
    }
    out.into_iter().map(|(_, i)| i).collect()
}

fn distance(p: [f64; 3], center: [f64; 3], op: &SculptOp) -> f64 {
    let d: [f64; 3] = std::array::from_fn(|a| p[a] - center[a]);
    let h = crate::py::row_dot3(d, op.normal);
    let planar2 = (crate::py::row_dot3(d, d) - h * h).max(0.0);
    (planar2 / (op.radius * op.radius) + h * h / (op.depth * op.depth)).sqrt()
}

fn base_weights(points: &[[f64; 3]], op: &SculptOp, stroke: bool) -> Vec<f64> {
    if op.scope == "whole" {
        return vec![1.0; points.len()];
    }
    if op.scope == "box" {
        let (lo, hi) = op.bounds.unwrap_or(([0.0; 3], [1.0; 3]));
        if DEFORM_TOOLS.contains(&op.tool.as_str()) {
            return points
                .iter()
                .map(|p| {
                    let mut prod = 1.0;
                    for a in 0..3 {
                        let ext = hi[a] - lo[a];
                        let u = ((p[a] - lo[a]) / ext).min((hi[a] - p[a]) / ext) * 2.0;
                        let u = np_clip(u / (1.0 - op.hardness), 0.0, 1.0);
                        prod *= u * u * (3.0 - 2.0 * u);
                    }
                    prod
                })
                .collect();
        }
        return points
            .iter()
            .map(|p| if (0..3).all(|a| p[a] >= lo[a] && p[a] <= hi[a]) { 1.0 } else { 0.0 })
            .collect();
    }
    let q: Vec<f64> = if stroke && op.points.len() > 1 {
        let n = op.normal;
        let r2 = op.radius * op.radius;
        let d2c = 1.0 / (op.depth * op.depth) - 1.0 / r2;
        let metric: [[f64; 3]; 3] = std::array::from_fn(|i| {
            std::array::from_fn(|j| (if i == j { 1.0 / r2 } else { 0.0 }) + n[i] * n[j] * d2c)
        });
        let mv = |v: [f64; 3]| -> [f64; 3] { std::array::from_fn(|i| matvec_row(&metric[i], v)) };
        let mut d2 = vec![f64::INFINITY; points.len()];
        for seg in op.points.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let ab: [f64; 3] = std::array::from_fn(|i| b[i] - a[i]);
            let mab = mv(ab);
            let den = crate::py::row_dot3(ab, mab);
            for (k, p) in points.iter().enumerate() {
                let pa: [f64; 3] = std::array::from_fn(|i| p[i] - a[i]);
                let t = if den == 0.0 { 0.0 } else { np_clip(crate::py::row_dot3(pa, mab) / den, 0.0, 1.0) };
                let d: [f64; 3] = std::array::from_fn(|i| p[i] - (a[i] + t * ab[i]));
                let md = mv(d);
                let v = crate::py::row_dot3(d, md);
                d2[k] = crate::geometry_selection::np_min(d2[k], v);
            }
        }
        d2.iter().map(|v| v.max(0.0).sqrt()).collect()
    } else {
        let c = if stroke { op.points[0] } else { op.center };
        points.iter().map(|p| distance(*p, c, op)).collect()
    };
    q.iter()
        .map(|q| {
            let u = np_clip((1.0 - q) / (1.0 - op.hardness), 0.0, 1.0);
            match op.falloff.as_str() {
                "constant" => {
                    if *q < 1.0 {
                        1.0
                    } else {
                        0.0
                    }
                }
                "linear" => u,
                _ => u * u * (3.0 - 2.0 * u),
            }
        })
        .collect()
}


pub fn weights(points: &[[f64; 3]], op: &SculptOp, stroke: bool) -> AResult<Vec<f64>> {
    let mut result = base_weights(points, op, stroke);
    if let Some((values, grid)) = &op.selection {
        let s = sample(values, grid, points, false, Some(0.0))?;
        result.iter_mut().zip(&s).for_each(|(r, s)| *r *= np_clip(*s, 0.0, 1.0));
    }
    Ok(result)
}


pub fn influence(points: &[[f64; 3]], op: &SculptOp, stroke: bool) -> AResult<Vec<f64>> {
    let mut out: Option<Vec<f64>> = None;
    for item in symmetry_operations(op) {
        let w = weights(points, &item, stroke)?;
        out = Some(match out {
            None => w,
            Some(prev) => {
                prev.iter().zip(&w).map(|(a, b)| crate::geometry_selection::np_max(*a, *b)).collect()
            }
        });
    }
    Ok(out.unwrap_or_else(|| vec![0.0; points.len()]))
}


pub fn inverse_flow(points: &[[f64; 3]], op: &SculptOp) -> AResult<(Vec<[f64; 3]>, usize)> {
    if !DEFORM_TOOLS.contains(&op.tool.as_str()) {
        return Ok((points.to_vec(), 0));
    }
    let ops = symmetry_operations(op);
    let mut gradient_scale = match op.tool.as_str() {
        "grab" => norm3(op.delta) / op.radius.min(op.depth),
        "stretch" | "scale" | "taper" => op.factor.ln().abs(),
        _ => op.angle.abs(),
    };
    if op.scope != "whole" {
        gradient_scale /= 1.0 - op.hardness;
    }
    if op.tool == "bend" {
        let extent = points
            .iter()
            .map(|p| row_norm3(std::array::from_fn(|a| p[a] - op.pivot[a])))
            .fold(f64::NEG_INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) });
        gradient_scale *= 1.0f64.max(extent / op.axial_length);
    }
    let steps = 8usize.max((12.0 * gradient_scale).ceil() as usize);
    if steps > 128 || !(12.0 * gradient_scale).is_finite() {
        return Err(err("deformation exceeds one resolved gesture; use smaller successive gestures"));
    }
    let velocity = |x: &[[f64; 3]]| -> AResult<Vec<[f64; 3]>> {
        let mut total = vec![[0.0; 3]; x.len()];
        let mut mass = vec![0.0; x.len()];
        for item in &ops {
            let w = weights(x, item, false)?;
            let ln = item.factor.ln();
            for (k, p) in x.iter().enumerate() {
                let d: [f64; 3] = std::array::from_fn(|a| p[a] - item.pivot[a]);
                let v: [f64; 3] = match item.tool.as_str() {
                    "grab" => item.delta,
                    "stretch" => {
                        let along = crate::py::row_dot3(d, item.axis);
                        std::array::from_fn(|a| ln * along * item.axis[a])
                    }
                    "scale" => d.map(|v| ln * v),
                    "taper" => {
                        let along = crate::py::row_dot3(d, item.axis);
                        let t = np_clip(along / item.axial_length, 0.0, 1.0);
                        let profile = t * t * (3.0 - 2.0 * t);
                        std::array::from_fn(|a| ln * profile * (d[a] - along * item.axis[a]))
                    }
                    "bend" => {
                        let along = crate::py::row_dot3(d, item.axis);
                        let lateral = crate::py::row_dot3(d, item.bend_direction);
                        let c = item.angle / item.axial_length;
                        std::array::from_fn(|a| {
                            c * (0.5 * (along * along) * item.bend_direction[a]
                                - (along * lateral) * item.axis[a])
                        })
                    }
                    _ => crate::py::cross3(item.axis, d).map(|v| item.angle * v),
                };
                for a in 0..3 {
                    total[k][a] += w[k] * v[a];
                }
                mass[k] += w[k];
            }
        }
        Ok(total.iter().zip(&mass).map(|(t, m)| t.map(|v| v / m.max(1.0))).collect())
    };
    let mut x = points.to_vec();
    let h = -1.0 / steps as f64;
    let shift = |x: &[[f64; 3]], v: &[[f64; 3]], s: f64| -> Vec<[f64; 3]> {
        x.iter().zip(v).map(|(p, d)| std::array::from_fn(|a| p[a] + s * d[a])).collect()
    };
    for _ in 0..steps {
        let a = velocity(&x)?;
        let b = velocity(&shift(&x, &a, 0.5 * h))?;
        let c = velocity(&shift(&x, &b, 0.5 * h))?;
        let d = velocity(&shift(&x, &c, h))?;
        for k in 0..x.len() {
            for i in 0..3 {
                x[k][i] += h * (a[k][i] + 2.0 * b[k][i] + 2.0 * c[k][i] + d[k][i]) / 6.0;
            }
        }
    }
    if !x.iter().flatten().all(|v| v.is_finite()) {
        return Err(err("nonfinite inverse coordinate flow"));
    }
    Ok((x, steps))
}


pub fn sample(
    values: &[f64],
    grid: &GridGeometry,
    points: &[[f64; 3]],
    extrapolate: bool,
    outside: Option<f64>,
) -> AResult<Vec<f64>> {
    let m = grid.registration.matrix();
    let inverse = inv(&m).ok_or_else(|| crate::py::value_error("Singular matrix"))?;
    let offset = grid.registration.offset();
    let origin = grid.registration.origin;
    let shape = grid.shape;
    let to_index = |p: [f64; 3], base: [f64; 3]| -> [f64; 3] {
        let d: [f64; 3] = std::array::from_fn(|a| p[a] - base[a]);
        std::array::from_fn(|c| matvec_row(&inverse[c], d))
    };
    let mut out = Vec::with_capacity(points.len());
    for p in points {
        let mut ijk = to_index(*p, offset);
        if !extrapolate {
            for a in 0..3 {
                ijk[a] = np_clip(ijk[a], 0.0, shape[a] as f64 - 1.0);
            }
        }
        let lower: [i64; 3] = std::array::from_fn(|a| (ijk[a].floor() as i64).clamp(0, shape[a] as i64 - 2));
        let f: [f64; 3] = std::array::from_fn(|a| ijk[a] - lower[a] as f64);
        let mut acc = 0.0;
        for i in 0..2i64 {
            for j in 0..2i64 {
                for k in 0..2i64 {
                    let bits = [i, j, k];
                    let ix: [usize; 3] = std::array::from_fn(|a| (lower[a] + bits[a]) as usize);
                    let w: [f64; 3] = std::array::from_fn(|a| if bits[a] == 1 { f[a] } else { 1.0 - f[a] });
                    acc += w[0] * w[1] * w[2] * values[grid.flat(ix[0], ix[1], ix[2])];
                }
            }
        }
        if let Some(o) = outside {
            let original = to_index(*p, origin);
            let node = usize::from(grid.registration.centering != "cell");
            let inside = (0..3).all(|a| original[a] >= 0.0 && original[a] <= (shape[a] - node) as f64);
            if !inside {
                acc = o;
            }
        }
        out.push(acc);
    }
    Ok(out)
}

#[must_use]
pub fn local_mean(values: &[f64], shape: [usize; 3]) -> Vec<f64> {
    FieldBrushGesture::smooth(values, shape)
}

pub struct NativeOwner {
    pub base: String,
    pub index: usize,
    pub tensor: NdArray,
    pub registration: Value,
    pub node_id: String,
    pub node: Node,
    pub spec: LatticeSpec,
    pub transform: [[f64; 4]; 4],
}


pub fn construct(kind: &str, attrs: &Value, params: BTreeMap<String, ParamValue>) -> AResult<Node> {
    let attrs: BTreeMap<String, Attr> = attrs
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), Attr::from_json(v))).collect())
        .unwrap_or_default();
    let reg = implexity_geometry::node::registry();
    Ok(reg.construct(kind, ConstructArgs { children: Vec::new(), names: None, params, attrs })?)
}


pub fn native_owner(document: &Value, field_id: &str) -> AResult<NativeOwner> {
    let (base, index, tensor, registration) = component_source(document, field_id)?;
    let nodes = document.get("nodes").and_then(Value::as_object).cloned().unwrap_or_default();
    let (nid, spec) = nodes
        .iter()
        .find(|(_, v)| {
            v.pointer("/params/control/array").and_then(Value::as_str) == Some(base.as_str())
                && matches!(
                    v.get("kind").and_then(Value::as_str),
                    Some("lattice.controlled" | "lattice.controlled_assembly")
                )
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .ok_or_else(|| AuthoringError::runtime("StopIteration", ""))?;
    let attrs = spec.get("attrs").cloned().unwrap_or_else(|| json!({}));
    let volume_index = index / 20;
    let (geometry, transform) =
        if spec.get("kind").and_then(Value::as_str) == Some("lattice.controlled_assembly") {
            let row = attrs
                .get("volumes")
                .and_then(|v| v.get(volume_index))
                .cloned()
                .ok_or_else(|| AuthoringError::Key(volume_index.to_string()))?;
            let geometry =
                row.get("geometry").cloned().ok_or_else(|| AuthoringError::Key("'geometry'".into()))?;
            let t = Arr::from_opt(row.get("local_to_model"))?;
            if t.shape != [4, 4] {
                return Err(crate::py::value_error("local_to_model must be a 4x4 matrix"));
            }
            (geometry, std::array::from_fn(|i| std::array::from_fn(|j| t.data[4 * i + j])))
        } else {
            (attrs, std::array::from_fn(|i| std::array::from_fn(|j| if i == j { 1.0 } else { 0.0 })))
        };
    let n: usize = tensor.shape()[1..].iter().product();
    let all = tensor.to_f64_vec();
    let block = all[20 * volume_index * n..20 * (volume_index + 1) * n].to_vec();
    let mut shape = tensor.shape().to_vec();
    shape[0] = 20;
    let control = NdArray::from_f64(shape, block).ok_or_else(|| err("malformed control tensor"))?;
    let mut params = BTreeMap::new();
    params.insert("control".to_string(), ParamValue::Array(Arc::new(control)));
    let node = construct("lattice.controlled", &geometry, params)?;
    let lattice = node
        .op()
        .as_any()
        .downcast_ref::<ControlledLattice>()
        .ok_or_else(|| err("native owner is not a controlled lattice"))?;
    let spec = lattice.spec.clone();
    Ok(NativeOwner { base, index, tensor, registration, node_id: nid, node, spec, transform })
}


pub fn verify_scalar_semantics(document: &Value, field_id: &str) -> AResult<()> {
    let nodes = document.get("nodes").and_then(Value::as_object).cloned().unwrap_or_default();
    let owners: Vec<&Value> = nodes
        .values()
        .filter(|n| {
            n.get("kind").and_then(Value::as_str) == Some("cell_grid_field")
                && n.pointer("/params/samples/array").and_then(Value::as_str) == Some(field_id)
        })
        .collect();
    if owners.len() != 1 {
        return Err(err(
            "geometry sculpt needs an occupancy cell field or a native controlled volume; use the explicit editable-field conversion for other graphs",
        ));
    }
    let node = owners[0];
    let p = node.get("params").cloned().unwrap_or_else(|| json!({}));
    let attrs = node.get("attrs").cloned().unwrap_or_else(|| json!({}));
    let scale = p.get("scale").cloned().unwrap_or_else(|| attrs.get("scale").cloned().unwrap_or(json!(1.0)));
    let offset =
        p.get("offset").cloned().unwrap_or_else(|| attrs.get("offset").cloned().unwrap_or(json!(0.0)));
    let bad = || err("geometry sculpt requires the declared occupancy convention f = 0.5 - occupancy");
    if scale.is_object() || offset.is_object() || py_float(&scale)? != -1.0 || py_float(&offset)? != -0.5 {
        return Err(bad());
    }
    Ok(())
}

#[must_use]
pub fn capabilities() -> Value {
    json!({"schema": SCHEMA, "coordinate": "model:control", "tools": TOOLS,
        "native_tools": native_tools(), "scopes": ["local", "box", "whole"],
        "directions": ["view_plane", "surface_normal", "x", "y", "z", "custom_model_vector"],
        "enhancements_schema": "implexity-sculpt-enhancements/1",
        "pivot": "picked point, explicit model point, or selection centroid resolved by the client",
        "hardness_range": [0.0, 0.95],
        "selection": {"actions": ["replace", "add", "subtract", "intersect", "invert", "all", "clear"],
                      "slots_per_volume": 16, "representation": "persistent model-space soft grid weights",
                      "use": "use_selection true and selection_id; missing/stale masks refuse, never select all",
                      "optimizer": "authoring influence only; use Protect to constrain optimization"},
        "bend": "stationary flexural coordinate flow about pivot, angle_rad/axial_length_mm is curvature scale, not a prescribed exact tip angle",
        "taper": "smooth one-sided transverse scaling from pivot to axial_length_mm along direction",
        "coordinate_units": "mm", "history": "atomic_preview_commit_cancel_undo_redo",
        "native_contract": "unchanged twenty fields per volume; warp is a control-grid projection",
        "native_excluded_tools": {"add": "no arbitrary material-volume addition in the bounded lattice basis",
                                  "flatten": "requires a scalar occupancy representation"},
        "domain": "no implicit envelope enlargement or remeshing",
        "protection": "controls or frozen sampled geometry; explicit release",
        "physics_independent": true, "requires_new_preflight_after_commit": true})
}


pub fn verify_topology_target(document: &Value, field_id: &str) -> AResult<String> {
    let selection = split_component_id(field_id)?;
    let base = selection.as_ref().map_or_else(|| field_id.to_string(), |(b, _)| b.clone());
    let parameter = if selection.is_some() { "control" } else { "samples" };
    let root_id = document.get("root").and_then(Value::as_str).unwrap_or_default();
    let binding = document
        .get("nodes")
        .and_then(|n| n.get(root_id))
        .and_then(|r| r.get("params"))
        .and_then(|p| p.get(parameter));
    let topology =
        crate::py::path_obj(document, &["meta", "implexity", "topology"]).cloned().unwrap_or_default();
    let coordinate_ok = topology.get("coordinate").is_none_or(|c| c.as_str() == Some("model:control"));
    let ok = binding
        .is_some_and(|b| b.is_object() && b.get("array").and_then(Value::as_str) == Some(base.as_str()))
        && coordinate_ok
        && topology.get("ref").and_then(Value::as_str) == Some(format!("model:{parameter}").as_str());
    if !ok {
        return Err(err(
            "sculpt requires the active root's authoritative model:control topology binding; a detached field or nested display object cannot be silently edited as the optimized design",
        ));
    }
    Ok(base)
}

fn evidence_insert(e: &mut Map<String, Value>, detail: &Value) {
    if let Some(d) = detail.as_object() {
        for (k, v) in d {
            e.insert(k.clone(), v.clone());
        }
    }
}

fn gradient1(values: &[f64], shape: [usize; 3], axis: usize) -> Vec<f64> {
    let mut out = vec![0.0; values.len()];
    let n = shape[axis];
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                let c = [i, j, k];
                let f = (i * shape[1] + j) * shape[2] + k;
                let at = |v: usize| -> f64 {
                    let mut q = c;
                    q[axis] = v;
                    values[(q[0] * shape[1] + q[1]) * shape[2] + q[2]]
                };
                let x = c[axis];
                out[f] = if n < 2 {
                    0.0
                } else if x == 0 {
                    at(1) - at(0)
                } else if x == n - 1 {
                    at(n - 1) - at(n - 2)
                } else {
                    (at(x + 1) - at(x - 1)) / 2.0
                };
            }
        }
    }
    out
}


pub fn apply_sculpt(document: &Value, field_id: &str, raw: &Value) -> AResult<(Value, Value)> {
    verify_topology_target(document, field_id)?;
    let mut op = normalise_operation(raw)?;
    let native = split_component_id(field_id)?.is_some();
    if native && !native_tools().contains(&op.tool.as_str()) {
        let caps = capabilities();
        return Err(err(caps["native_excluded_tools"][op.tool.as_str()].as_str().unwrap_or_default()));
    }
    let field = read_spatial_field(document, field_id)?;
    let grid = field.grid.clone();
    if !native {
        verify_scalar_semantics(document, field_id)?;
    }
    if op.use_selection {
        let values =
            crate::geometry_selection::read_selection(document, field_id, &grid, &op.selection_id, true)?
                .unwrap_or_default();
        op.selection = Some((values, grid.clone()));
    }
    let xyz = grid.coordinates();
    let w = influence(&xyz, &op, true)?;
    let mut evidence = Map::new();
    let mut ev = |k: &str, v: Value| {
        evidence.insert(k.into(), v);
    };
    ev("schema", json!(SCHEMA));
    ev("tool", json!(op.tool));
    ev("coordinate", json!("model:control"));
    ev("representation", json!(if native { "native_twenty_field" } else { "cell_occupancy" }));
    ev("authoritative_field_id", json!(field_id));
    ev("scope", json!(op.scope));
    ev("influenced_samples", json!(w.iter().filter(|v| **v != 0.0).count()));
    ev("neutral_phase_carried", json!(op.carry_phase));
    ev("new_preflight_required", json!(true));
    ev("pivot_mm", jfs(&op.pivot));
    ev("hardness", jf(op.hardness));
    ev("selection_filtered", json!(op.use_selection));
    ev("selection_id", if op.use_selection { json!(op.selection_id) } else { Value::Null });
    if op.tool == "select" {
        let (changed, detail) = crate::geometry_selection::edit_selection(document, field_id, &grid, &op)?;
        evidence_insert(&mut evidence, &detail);
        evidence.insert("new_preflight_required".into(), json!(false));
        return Ok((changed, Value::Object(evidence)));
    }
    if op.tool == "protect" || op.tool == "release" {
        let (changed, detail) = if native && op.protection == "geometry" {
            crate::geometry_freeze::edit_frozen_geometry(document, field_id, &op)?
        } else {
            let selected: Vec<bool> = w.iter().map(|v| *v > 0.0).collect();
            crate::geometry_holds::edit_holds(document, field_id, &selected, op.tool == "release", native)?
        };
        evidence_insert(&mut evidence, &detail);
        return Ok((changed, Value::Object(evidence)));
    }
    let (src, nsteps) = inverse_flow(&xyz, &op)?;
    evidence.insert("coordinate_flow_steps".into(), json!(nsteps));
    let caps = capabilities();
    if op.tool == "bend" {
        evidence.insert("bend_interpretation".into(), caps["bend"].clone());
    }
    if op.tool == "taper" {
        evidence.insert("taper_interpretation".into(), caps["taper"].clone());
    }
    let n = grid.size();
    if native {
        let owner = native_owner(document, field_id)?;
        let block_index = owner.index / 20;
        let tensor_all = owner.tensor.to_f64_vec();
        let tshape = owner.tensor.shape().to_vec();
        let block = 20 * n;
        let old: Vec<f64> = tensor_all[block_index * block..(block_index + 1) * block].to_vec();
        let topology =
            crate::py::path_obj(document, &["meta", "implexity", "topology"]).cloned().unwrap_or_default();
        let comps = control_components();
        let mut bounds: Vec<Vec<f64>> = Vec::new();
        for key in ["lower", "upper"] {
            let arr: Vec<f64> = match topology.get(key).filter(|v| !v.is_null()) {
                None => (0..20)
                    .flat_map(|c| {
                        let v = if key == "lower" { comps[c].lower } else { comps[c].upper };
                        std::iter::repeat_n(v, n)
                    })
                    .collect(),
                Some(raw) => {
                    let a = Arr::from_json(raw)?;
                    if a.ndim() == 0 {
                        vec![a.data[0]; block]
                    } else if a.shape == tshape {
                        a.data[block_index * block..(block_index + 1) * block].to_vec()
                    } else {
                        return Err(err(
                            "authored native bounds must be scalar or match the complete control tensor",
                        ));
                    }
                }
            };
            if !arr.iter().all(|v| v.is_finite()) {
                return Err(err("native bounds must be finite"));
            }
            bounds.push(arr);
        }
        if bounds[0].iter().zip(&bounds[1]).any(|(l, u)| l >= u) {
            return Err(err("native bounds must be strictly ordered"));
        }
        let mut new = old.clone();
        let chan = |c: usize| c * n..(c + 1) * n;
        if nsteps > 0 {
            let controls =
                Controls { grid: [grid.shape[0], grid.shape[1], grid.shape[2]], data: old.clone() };
            let fields = owner.spec.geometry_fields(&controls)?;
            let phase = &fields.state().fields.phi;
            let t = owner.transform;
            let t3: [[f64; 3]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| t[i][j]));
            let inv3 = inv(&t3).ok_or_else(|| crate::py::value_error("Singular matrix"))?;

            let trans: [f64; 3] =
                std::array::from_fn(|i| -(matvec_row(&inv3[i], [t[0][3], t[1][3], t[2][3]])));
            let local = |p: &[[f64; 3]]| -> Vec<[f64; 3]> {
                p.iter().map(|x| std::array::from_fn(|r| matvec_row(&inv3[r], *x) + trans[r])).collect()
            };
            let gs = owner.spec.geometry_shape();
            let step: [f64; 3] = std::array::from_fn(|a| owner.spec.domain_mm[a] / gs[a] as f64);
            let pgrid = GridGeometry::from_shape(
                &gs,
                &json!({"shape": gs, "origin": jfs(&owner.spec.origin_mm),
                    "basis": [[step[0], 0.0, 0.0], [0.0, step[1], 0.0], [0.0, 0.0, step[2]]],
                    "centering": "cell", "frame": "model", "axis_order": "xyz"}),
            )?;
            let (ls, lx) = (local(&src), local(&xyz));
            for i in 0..3 {
                let a = sample(&phase[i], &pgrid, &ls, true, None)?;
                let b = sample(&phase[i], &pgrid, &lx, true, None)?;
                for (k, idx) in chan(2 + i).enumerate() {
                    new[idx] += a[k] - b[k];
                }
            }
            let mut channels: Vec<usize> = vec![0, 1];
            channels.extend(8..19);
            if op.carry_phase {
                channels.push(19);
            }
            for c in channels {
                let s = sample(&old[chan(c)], &grid, &src, false, None)?;
                new[chan(c)].copy_from_slice(&s);
            }
        } else if op.tool == "inflate" || op.tool == "deflate" {
            let [lo, hi] = owner.spec.thickness_range_mm;
            let eps = f64::EPSILON;
            let sign = if op.tool == "inflate" { 1.0 } else { -1.0 };
            for (k, idx) in chan(0).enumerate() {
                let t = lo + (hi - lo) / (1.0 + (-old[idx]).exp());
                let target = t + sign * op.amount * w[k];
                let z = np_clip((target - lo) / (hi - lo), eps, 1.0 - eps);
                new[idx] = (z / (1.0 - z)).ln();
            }
            evidence.insert(
                "thickness_interpretation".into(),
                json!("change of native thickness threshold in mm, not an exact surface offset"),
            );
        } else if op.tool == "subtract" {
            for (k, idx) in chan(1).enumerate() {
                new[idx] = old[idx] + op.strength * w[k] * (-8.0 - old[idx]);
            }
        } else if op.tool == "smooth" {
            let upto = if op.carry_phase { 20 } else { 19 };
            for c in 0..upto {
                let mean = local_mean(&old[chan(c)], grid.shape);
                for (k, idx) in chan(c).enumerate() {
                    new[idx] = old[idx] + op.strength * w[k] * (mean[k] - old[idx]);
                }
            }
        } else {
            return Err(err("unsupported native operation"));
        }
        let mut clipped = 0usize;
        for c in 0..20 {
            for (k, idx) in chan(c).enumerate() {
                if !(w[k] > 0.0) {
                    new[idx] = old[idx];
                }
                let bounded = np_clip(new[idx], bounds[0][idx], bounds[1][idx]);
                if new[idx] != old[idx] && bounded != new[idx] {
                    clipped += 1;
                }
                if new[idx] != old[idx] {
                    new[idx] = bounded;
                }
            }
        }
        let mut full = tensor_all.clone();
        full[block_index * block..(block_index + 1) * block].copy_from_slice(&new);
        let held = crate::geometry_holds::held_mask(document, &owner.base, &tshape, true)?;
        for (i, h) in held.iter().enumerate() {
            if *h {
                full[i] = tensor_all[i];
            }
        }
        let nd = NdArray::from_f64(tshape, full.clone()).ok_or_else(|| err("malformed control tensor"))?;
        let (entry, raw_array) = encode_array(&nd);
        let mut changed = document.clone();
        obj_mut(&mut changed)?
            .get_mut("arrays")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| AuthoringError::Key("'arrays'".into()))?
            .insert(owner.base.clone(), inline_entry(&entry, &raw_array));
        evidence.insert(
            "changed_control_values".into(),
            json!(full.iter().zip(&tensor_all).filter(|(a, b)| a != b).count()),
        );
        evidence.insert("clipped_control_values".into(), json!(clipped));
        evidence.insert("held_control_values".into(), json!(held.iter().filter(|h| **h).count()));
        evidence.insert("phase_projection".into(), json!("trilinear existing control grid"));
        evidence.insert("unchanged_control_schema".into(), json!(true));
        return Ok((changed, Value::Object(evidence)));
    }
    let values = &field.values;
    let new: Vec<f64> = if nsteps > 0 {
        sample(values, &grid, &src, false, Some(0.0))?
    } else if op.tool == "inflate" || op.tool == "deflate" {
        let m = grid.registration.matrix();
        let minv = inv(&m).ok_or_else(|| crate::py::value_error("Singular matrix"))?;
        let g: Vec<Vec<f64>> = (0..3).map(|a| gradient1(values, grid.shape, a)).collect();
        let sign = if op.tool == "inflate" { 1.0 } else { -1.0 };
        let moved: Vec<[f64; 3]> = (0..n)
            .map(|k| {
                let gk = [g[0][k], g[1][k], g[2][k]];
                let world: [f64; 3] =
                    std::array::from_fn(|c| gk[0] * minv[0][c] + gk[1] * minv[1][c] + gk[2] * minv[2][c]);
                let nrm = row_norm3(world);
                let unit = if nrm > 1e-14 { world.map(|v| v / nrm) } else { [0.0; 3] };
                std::array::from_fn(|a| xyz[k][a] + sign * op.amount * w[k] * unit[a])
            })
            .collect();
        sample(values, &grid, &moved, false, Some(0.0))?
    } else if op.tool == "smooth" {
        let mean = local_mean(values, grid.shape);
        (0..n).map(|k| values[k] + op.strength * w[k] * (mean[k] - values[k])).collect()
    } else if op.tool == "add" || op.tool == "subtract" {
        let target = if op.tool == "add" { 1.0 } else { 0.0 };
        (0..n).map(|k| values[k] + op.strength * w[k] * (target - values[k])).collect()
    } else if op.tool == "flatten" {
        let spacing = grid.spacing_mm();
        let width = spacing[0].min(spacing[1]).min(spacing[2]);
        (0..n)
            .map(|k| {
                let signed = crate::py::row_dot3(std::array::from_fn(|a| xyz[k][a] - op.center[a]), op.axis);
                let target = np_clip(0.5 - signed / width, 0.0, 1.0);
                values[k] + op.strength * w[k] * (target - values[k])
            })
            .collect()
    } else {
        return Err(err("unsupported occupancy operation"));
    };
    let mut held = crate::geometry_holds::held_mask(document, field_id, &grid.shape, true)?;
    let mut source_meta = field.entry.clone().and_then(|e| e.as_object().cloned()).unwrap_or_default();
    if let Some(Value::Object(m)) = &field.metadata {
        for (k, v) in m {
            source_meta.insert(k.clone(), v.clone());
        }
    }
    if let Some(Value::Object(masks)) = source_meta.get("protected_masks") {
        for mask in masks.values() {
            let raw = if mask.is_object() {
                mask.get("values").cloned().unwrap_or(Value::Null)
            } else {
                mask.clone()
            };
            let (_, m) = crate::py::bool_array(&raw)?;
            if m.len() != n {
                return Err(crate::py::value_error(format!(
                    "cannot reshape array of size {} into shape {}",
                    m.len(),
                    crate::field_interaction::shape_repr(&grid.shape)
                )));
            }
            held.iter_mut().zip(&m).for_each(|(h, v)| *h |= *v);
        }
    }
    let final_values: Vec<f64> =
        (0..n).map(|k| if held[k] || w[k] == 0.0 { values[k] } else { np_clip(new[k], 0.0, 1.0) }).collect();
    let changed = write_spatial_field(document, field_id, &final_values, &grid, Some(0.0), Some(1.0), None)?;
    evidence.insert(
        "changed_control_values".into(),
        json!(final_values.iter().zip(values).filter(|(a, b)| a != b).count()),
    );
    evidence.insert("held_control_values".into(), json!(held.iter().filter(|h| **h).count()));
    let max_change = final_values
        .iter()
        .zip(values)
        .map(|(a, b)| (a - b).abs())
        .fold(f64::NEG_INFINITY, |a, b| if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) });
    evidence.insert("maximum_sample_change".into(), jf(max_change));
    Ok((changed, Value::Object(evidence)))
}


pub fn preview_field(document: &Value, count: &Value) -> AResult<Value> {
    let count = match count.as_i64() {
        Some(c) if (8..=48).contains(&c) && !count.is_boolean() => c as usize,
        _ => return Err(err("preview count must be an integer in 8..48")),
    };
    let model = implexity_geometry::document::build(document, None, None)?;
    let root = document.get("root").map_or_else(String::new, py_str);
    let node = model.node(&root)?;
    let bounds = node.op().aabb(&node).ok().flatten();
    let Some((lo, hi)) = bounds else {
        return Err(err("sculpt preview requires a bounded implicit root"));
    };
    let denom = 16usize.max(count - 1) as f64;
    let pad: [f64; 3] = std::array::from_fn(|a| (hi[a] - lo[a]) / denom);
    let lo: [f64; 3] = std::array::from_fn(|a| lo[a] - pad[a]);
    let hi: [f64; 3] = std::array::from_fn(|a| hi[a] + pad[a]);
    let axes: Vec<Vec<f64>> =
        (0..3).map(|a| implexity_mesh::numeric::linspace(lo[a], hi[a], count)).collect();
    let mut pts = Vec::with_capacity(count * count * count);
    for x in &axes[0] {
        for y in &axes[1] {
            for z in &axes[2] {
                pts.push([*x, *y, *z]);
            }
        }
    }
    let values =
        implexity_geometry::eval::eval_points(&node, &pts, &implexity_geometry::eval::EvalOptions::exact())?;
    if !values.iter().all(|v| v.is_finite()) {
        return Err(err("uncommitted geometry contains nonfinite preview samples"));
    }
    Ok(json!({
        "schema": "implexity-sculpt-preview/1",
        "shape": [count, count, count],
        "bbox_mm": [jfs(&lo), jfs(&hi)],
        "values": jfs(&values),
        "node": document.get("root").cloned().unwrap_or(Value::Null),
        "authority": "uncommitted_transaction",
        "document_sha256": sha256_hex(canonical_ascii(document).as_bytes()),
        "field_class": {"kind": "IMPLICIT"},
        "sampling_note": "display sample of the actual uncommitted implicit field; not physics evidence",
    }))
}

#[must_use]
pub fn is_truthy(v: &Value) -> bool {
    truthy(v)
}
