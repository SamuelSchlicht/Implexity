// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::error::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use crate::pyfmt::{fmt_e, fmt_e0};

pub const SWITCH_CERTIFICATE: f64 = 1e-8;

fn check_eps(eps: f64) -> CaeResult<()> {
    if eps > 0.0 { Ok(()) } else { Err(CaeError::contract("eps must be positive")) }
}



pub fn smooth_positive<S: Scalar>(x: S, eps: f64) -> CaeResult<S> {
    check_eps(eps)?;
    Ok((x + (x * x + eps * eps).sqrt()) * 0.5)
}



pub fn smooth_heaviside<S: Scalar>(x: S, eps: f64) -> CaeResult<S> {
    check_eps(eps)?;
    Ok((x / (x * x + eps * eps).sqrt() + 1.0) * 0.5)
}

#[must_use]
pub fn fischer_burmeister_switch_distance(a: f64, b: f64) -> f64 {
    a.hypot(b)
}




pub fn fischer_burmeister<S: Scalar>(a: &[S], b: &[S], eps: f64, corner_tolerance: f64) -> CaeResult<Vec<S>> {
    if eps < 0.0 {
        return Err(CaeError::contract("eps must be non-negative"));
    }
    if a.len() != b.len() {
        return Err(CaeError::contract("Fischer-Burmeister arguments must have equal length"));
    }
    if eps == 0.0 {
        let at_corner: Vec<usize> = (0..a.len())
            .filter(|&i| fischer_burmeister_switch_distance(a[i].value(), b[i].value()) <= corner_tolerance)
            .collect();
        if let Some(&first) = at_corner.first() {
            return Err(CaeError::contract(format!(
                "Fischer-Burmeister corner: eps=0 at (a, b) = ({}, {}) is within sqrt(a^2+b^2) <= {} of the non-differentiable switch (0, 0) at {} of {} point(s); the complementarity gradient is undefined there. Pass eps > 0 to regularize the corner or move the state off the switch.",
                fmt_e(a[first].value(), 3),
                fmt_e(b[first].value(), 3),
                fmt_e0(corner_tolerance),
                at_corner.len(),
                a.len()
            )));
        }
    }
    Ok(a.iter().zip(b).map(|(&x, &y)| (x * x + y * y + eps * eps).sqrt() - x - y).collect())
}



pub fn penalty_contact_pressure<S: Scalar>(gap: S, stiffness: f64, eps: f64) -> CaeResult<S> {
    if stiffness < 0.0 {
        return Err(CaeError::contract("stiffness must be non-negative"));
    }
    Ok(smooth_positive(-gap, eps)? * stiffness)
}



pub fn smooth_irreversible_update<S: Scalar>(history: S, driving: S, eps: f64) -> CaeResult<S> {
    Ok(history + smooth_positive(driving - history, eps)?)
}



pub fn transition_diagnostics(
    values: &[f64],
    eps: f64,
    resolution_ratio: f64,
) -> CaeResult<Map<String, Value>> {
    check_eps(eps)?;
    let near = values.iter().filter(|v| v.abs() <= resolution_ratio * eps).count();
    let mut out = Map::new();
    out.insert(
        "near_transition_fraction".into(),
        json!(if values.is_empty() { 0.0 } else { near as f64 / values.len() as f64 }),
    );
    let minimum = values.iter().map(|v| v.abs()).fold(f64::INFINITY, f64::min);

    out.insert(
        "minimum_abs_switch_argument".into(),
        if minimum.is_finite() { json!(minimum) } else { Value::Null },
    );
    out.insert("smoothing_width".into(), json!(eps));
    Ok(out)
}

#[must_use]
pub fn switch_distance(values: &[f64]) -> Vec<f64> {
    values.iter().map(|v| v.abs()).collect()
}



pub fn certify_switch_distance(
    values: &[f64],
    eps: f64,
    threshold: f64,
    resolution_ratio: f64,
) -> CaeResult<Map<String, Value>> {
    if values.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract("switch argument is not finite; sensitivity admission refused"));
    }
    let mut report = transition_diagnostics(values, eps, resolution_ratio)?;
    let minimum = values.iter().map(|v| v.abs()).fold(f64::INFINITY, f64::min);
    if minimum <= threshold {
        let count = values.iter().filter(|v| v.abs() <= threshold).count();
        return Err(CaeError::contract(format!(
            "non-smooth switch prevents branch-local sensitivity admission: minimum |switch argument| = {} <= {} at {count} of {} point(s) (smoothing width eps = {}); the derivative there is set by the regularization, not by the physics",
            fmt_e(minimum, 3),
            fmt_e0(threshold),
            values.len(),
            fmt_e(eps, 3)
        )));
    }
    report.insert("switch_certificate".into(), json!(threshold));
    report
        .insert("min_switch_distance".into(), if minimum.is_finite() { json!(minimum) } else { Value::Null });
    report.insert("sensitivity_admissible".into(), json!(true));
    report
        .insert("derivative_scope".into(), json!("piecewise branch-local; no derivative across the switch"));
    Ok(report)
}

