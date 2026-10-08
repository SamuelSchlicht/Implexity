// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RankineHugoniotResult {
    pub speed: f64,
    pub residual: f64,
    pub admissible: bool,
}



pub fn rankine_hugoniot_speed(
    ul: f64,
    ur: f64,
    fl: f64,
    fr: f64,
    tol: f64,
) -> CaeResult<RankineHugoniotResult> {
    let jump = ur - ul;
    if jump.abs() <= tol {
        return Err(CaeError::contract(
            "state jump vanished; shock position is not a differentiable coordinate",
        ));
    }
    let s = (fr - fl) / jump;
    Ok(RankineHugoniotResult { speed: s, residual: (fr - fl) - s * jump, admissible: true })
}



#[allow(clippy::too_many_arguments)]
pub fn shock_speed_gradient(
    ul: f64,
    ur: f64,
    fl: f64,
    fr: f64,
    dul: &[f64],
    dur: &[f64],
    dfl: &[f64],
    dfr: &[f64],
    tol: f64,
) -> CaeResult<Vec<f64>> {
    let (jump, flux) = (ur - ul, fr - fl);
    if jump.abs() <= tol {
        return Err(CaeError::contract("shock bifurcation/coalescence requires relinearization"));
    }
    let n = dul.len();
    if dur.len() != n || dfl.len() != n || dfr.len() != n {
        return Err(CaeError::contract("derivative directions must agree"));
    }
    Ok((0..n).map(|k| ((dfr[k] - dfl[k]) * jump - flux * (dur[k] - dul[k])) / (jump * jump)).collect())
}

#[must_use]
pub fn track_front(x0: f64, dt: f64, speed: f64, domain: Option<(f64, f64)>) -> Value {
    let mut x = x0 + dt * speed;
    let mut event = "none";
    if let Some((a, b)) = domain {
        if x <= a {
            x = a;
            event = "left_boundary";
        } else if x >= b {
            x = b;
            event = "right_boundary";
        }
    }
    json!({"position": x, "event": event, "relinearize": event != "none"})
}

#[must_use]
pub fn discontinuity_event_gate(reference_jump: f64, candidate_jump: f64, min_jump: f64) -> Value {
    let crossed = reference_jump * candidate_jump < 0.0 || candidate_jump.abs() < min_jump;
    json!({"admissible": !crossed, "relinearize": crossed,
        "reason": if crossed { json!("shock/contact discontinuity bifurcated or vanished") } else { Value::Null }})
}

