// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use crate::error::{GResult, GeometryError};
use crate::numpy;

pub const FIELD_VIEWS_SCHEMA: &str = "implexity-field-views/1";

#[derive(Clone, Debug, PartialEq)]
pub struct FieldArray {
    pub shape: Vec<usize>,
    pub data: Vec<f64>,
    pub complex: bool,
}

fn verr(m: &str) -> GeometryError {
    GeometryError::Value(m.into())
}

#[must_use]
pub fn robust_range(
    values: &[f64],
    symmetric: bool,
    low_percentile: f64,
    high_percentile: f64,
    sample_limit: usize,
) -> (f64, f64) {
    let mut v: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return if symmetric { (-1.0, 1.0) } else { (0.0, 1.0) };
    }
    if v.len() > sample_limit {
        let stride = (v.len() / sample_limit).max(1);
        v = v.into_iter().step_by(stride).take(sample_limit).collect();
    }
    let lo = numpy::percentile(&v, low_percentile).unwrap_or(0.0);
    let hi = numpy::percentile(&v, high_percentile).unwrap_or(1.0);
    if symmetric {
        let bound = lo.abs().max(hi.abs()).max(f64::EPSILON);
        return (-bound, bound);
    }
    if !(hi > lo) {
        let eps = (lo.abs() * 1e-6).max(1e-9);
        return (lo - eps, hi + eps);
    }
    (lo, hi)
}

#[must_use]
pub fn labels(rank: &str, components: usize, supplied: Option<&[String]>) -> Vec<String> {
    if let Some(s) = supplied
        && s.len() == components
    {
        return s.to_vec();
    }
    let name = rank.to_lowercase();
    let v: &[&str] = match (components, name.as_str()) {
        (3, "vector" | "tensor_or_axis" | "axis") => &["x", "y", "z"],
        (6, "tensor_sym" | "symmetric_tensor" | "stress") => &["xx", "yy", "zz", "xy", "yz", "zx"],
        (9, "tensor" | "tensor_full" | "matrix") => &["xx", "xy", "xz", "yx", "yy", "yz", "zx", "zy", "zz"],
        _ => return (0..components).map(|i| i.to_string()).collect(),
    };
    v.iter().map(|s| (*s).to_string()).collect()
}

fn range(values: &[f64], signed_hint: bool) -> Vec<f64> {
    let finite: Vec<f64> = values.iter().copied().filter(|x| x.is_finite()).collect();
    if finite.is_empty() {
        return if signed_hint { vec![-1.0, 1.0] } else { vec![0.0, 1.0] };
    }
    let mn = finite.iter().copied().fold(f64::INFINITY, f64::min);
    let mx = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let signed = signed_hint || (mn < 0.0 && 0.0 < mx);
    let (lo, hi) = robust_range(&finite, signed, 1.0, 99.0, 500_000);
    vec![lo, hi]
}


pub fn field_views(
    array: &FieldArray,
    rank: &str,
    component_labels: Option<&[String]>,
    signed: bool,
    sample_limit: usize,
) -> GResult<Value> {
    if array.shape.len() < 3 {
        return Err(verr("a spatial result field must have at least three axes"));
    }
    if array.complex {
        return Err(verr("complex display fields require explicit real/imaginary transport channels"));
    }
    if array.shape[..3].contains(&0) {
        return Err(verr("spatial result-field axes must be non-empty"));
    }
    let spatial: usize = array.shape[..3].iter().product();
    let components: usize = array.shape[3..].iter().product();
    let limit = sample_limit.max(1);
    let stride = if spatial > limit { (spatial / limit).max(1) } else { 1 };
    let rows: Vec<&[f64]> = array
        .data
        .chunks(components.max(1))
        .step_by(stride)
        .take(if spatial > limit { limit } else { spatial })
        .collect();
    let mut views = Vec::new();
    let default = if components == 1 {
        let col: Vec<f64> = rows.iter().map(|r| r[0]).collect();
        views.push(json!({"id": "value", "label": "Value", "component": 0, "range": range(&col, signed)}));
        "value"
    } else {
        let magnitude: Vec<f64> =
            rows.iter().map(|r| r.iter().skip(1).fold(r[0].abs(), |acc, v| acc.hypot(*v))).collect();
        if rows.iter().zip(&magnitude).any(|(r, m)| r.iter().all(|v| v.is_finite()) && !m.is_finite()) {
            return Err(verr("field magnitude exceeds the representable display range"));
        }
        let rl = rank.to_lowercase();
        views.push(json!({"id": "magnitude", "label": if rl == "scalar" || rl == "vector" { "Magnitude" } else { "Frobenius magnitude" },
            "component": "magnitude", "range": range(&magnitude, false)}));
        for (i, label) in labels(rank, components, component_labels).into_iter().enumerate() {
            let col: Vec<f64> = rows.iter().map(|r| r[i]).collect();
            views.push(json!({"id": format!("component:{i}"), "label": label, "component": i, "range": range(&col, signed)}));
        }
        "magnitude"
    };
    Ok(
        json!({"schema": FIELD_VIEWS_SCHEMA, "rank": if rank.is_empty() { "scalar" } else { rank }, "components": components,
        "default_view": default, "views": views}),
    )
}


pub fn prepare_stream_array(array: &FieldArray) -> GResult<(Vec<usize>, Vec<f64>)> {
    if array.shape.len() < 3 {
        return Err(verr("a streamable field must have at least three axes"));
    }
    let mut shape = array.shape.clone();
    if array.complex {
        shape.push(2);
    }
    if shape.len() == 3 {
        return Ok((shape, array.data.clone()));
    }
    let components: usize = shape[3..].iter().product();
    Ok((vec![shape[0], shape[1], shape[2], components], array.data.clone()))
}

#[must_use]
pub fn component_labels_from_metadata(
    metadata: Option<&Map<String, Value>>,
    array: Option<&FieldArray>,
) -> Option<Vec<String>> {
    if let Some(a) = array.filter(|a| a.complex) {
        let empty = Map::new();
        let md = metadata.unwrap_or(&empty);
        let count: usize = if a.shape.len() > 3 { a.shape[3..].iter().product() } else { 1 };
        let rank = md.get("rank").map_or_else(|| "scalar".to_string(), crate::document::py_str);
        let supplied = component_labels_from_metadata(Some(md), None);
        let ls = labels(&rank, count, supplied.as_deref());
        let mut out = Vec::new();
        for label in ls {
            for part in ["Real", "Imaginary"] {
                out.push(if count > 1 { format!("{part} {label}") } else { part.to_string() });
            }
        }
        return Some(out);
    }
    let md = metadata.filter(|m| !m.is_empty())?;
    for key in ["component_labels", "components", "component_names"] {
        if let Some(Value::Array(items)) = md.get(key)
            && items.iter().all(|v| v.is_string() || v.is_i64() || v.is_u64())
        {
            return Some(items.iter().map(crate::document::py_str).collect());
        }
    }
    None
}
