// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::error::{CaeError, CaeResult};
use serde_json::{Value, json};

use crate::pyfmt::{fmt_e, fmt_e0};

pub const EVENT_SWITCH_CERTIFICATE: f64 = crate::nonsmooth::SWITCH_CERTIFICATE;

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct IrreversibleState {
    pub state: Vec<f64>,
    pub active: Vec<bool>,
    pub event_margin: f64,
    pub switch_distance: Vec<f64>,
}

impl IrreversibleState {
    #[must_use]
    pub fn min_switch_distance(&self) -> f64 {
        self.switch_distance.iter().copied().fold(f64::INFINITY, f64::min)
    }

    #[must_use]
    pub fn within_margin(&self) -> bool {
        self.min_switch_distance() <= self.event_margin
    }

    #[must_use]
    pub fn diagnostics(&self) -> Value {
        json!({
            "distance_to_event_switch": self.min_switch_distance(),
            "switch_certificate": self.event_margin,
            "points_within_margin": self.switch_distance.iter().filter(|d| **d <= self.event_margin).count(),
            "sensitivity_admissible": !self.within_margin(),
            "derivative_scope": "piecewise branch-local; no derivative across the event switch",
        })
    }



    pub fn certify_sensitivity(&self, threshold: Option<f64>) -> CaeResult<f64> {
        certify_switch_distance(&self.switch_distance, 0.0, threshold.unwrap_or(self.event_margin))
    }
}

#[must_use]
pub fn switch_distance(driving: &[f64], threshold: f64) -> Vec<f64> {
    driving.iter().map(|d| (d - threshold).abs()).collect()
}



pub fn certify_switch_distance(driving: &[f64], threshold: f64, event_margin: f64) -> CaeResult<f64> {
    if !threshold.is_finite() || !event_margin.is_finite() || event_margin < 0.0 {
        return Err(err("event threshold and margin must be finite; margin must be nonnegative"));
    }
    let distance = switch_distance(driving, threshold);
    if !distance.iter().all(|d| d.is_finite()) {
        return Err(err("event switch distance is not finite; sensitivity admission refused"));
    }
    let minimum = distance.iter().copied().fold(f64::INFINITY, f64::min);
    if minimum <= event_margin {
        let count = distance.iter().filter(|d| **d <= event_margin).count();
        return Err(err(format!(
            "nucleation/active-set boundary encountered; generalized derivative selection and relinearization required: minimum |driving - threshold| = {} <= {} at {count} of {} point(s)",
            fmt_e(minimum, 3),
            fmt_e0(event_margin),
            distance.len()
        )));
    }
    Ok(minimum)
}



pub fn irreversible_update(
    old_state: &[f64],
    driving: &[f64],
    threshold: f64,
    increment_scale: f64,
    event_margin: f64,
) -> CaeResult<IrreversibleState> {
    if old_state.len() != driving.len() {
        return Err(err("state/driving shape mismatch"));
    }
    if !threshold.is_finite() || !increment_scale.is_finite() || increment_scale < 0.0
        || !event_margin.is_finite() || event_margin < 0.0
        || old_state.iter().any(|o| !o.is_finite() || !(0.0..=1.0).contains(o))
        || driving.iter().any(|d| !d.is_finite())
    {
        return Err(err("irreversible update requires finite inputs, state in [0,1], and nonnegative scale and margin"));
    }
    let mut state = Vec::with_capacity(old_state.len());
    let mut active = Vec::with_capacity(old_state.len());
    let mut distance = Vec::with_capacity(old_state.len());
    for (&old, &d) in old_state.iter().zip(driving) {
        let excess = d - threshold;
        if !excess.is_finite() {
            return Err(err("irreversible update arithmetic is not finite"));
        }
        active.push(excess > event_margin);
        if increment_scale == 0.0 || old == 1.0 {
            state.push(old);
            distance.push(f64::MAX);
            continue;
        }
        let increment = increment_scale * excess.max(0.0);
        let candidate = old + increment;
        if !excess.is_finite() || !candidate.is_finite() {
            return Err(err("irreversible update arithmetic is not finite"));
        }
        let saturation_excess = (1.0 - old) / increment_scale;
        let saturation_distance = if saturation_excess.is_finite() {
            (excess - saturation_excess).abs()
        } else {
            f64::MAX
        };
        state.push(candidate.min(1.0));
        distance.push(excess.abs().min(saturation_distance));
    }
    Ok(IrreversibleState { state, active, event_margin, switch_distance: distance })
}

pub fn semismooth_driving_derivative(
    old_state: &[f64],
    driving: &[f64],
    threshold: f64,
    increment_scale: f64,
    event_margin: f64,
) -> CaeResult<Vec<f64>> {
    let update = irreversible_update(old_state, driving, threshold, increment_scale, event_margin)?;
    update.certify_sensitivity(None)?;
    Ok(update.state.iter().zip(driving).map(|(state, d)| {
        if *state < 1.0 && *d > threshold { increment_scale } else { 0.0 }
    }).collect())
}

#[must_use]
pub fn topology_event_gate(
    reference_active: &[bool],
    candidate_driving: &[f64],
    threshold: f64,
    event_margin: f64,
) -> Value {
    let x: Vec<f64> = candidate_driving.iter().map(|d| d - threshold).collect();
    let candidate: Vec<bool> = x.iter().map(|v| *v > event_margin).collect();
    let unresolved = x.iter().any(|v| v.abs() <= event_margin);
    let changed = candidate.len() != reference_active.len()
        || candidate.iter().zip(reference_active).any(|(a, b)| a != b)
        || unresolved;
    json!({
        "admissible": !changed, "relinearize": changed,
        "active_count": candidate.iter().filter(|a| **a).count(),
        "min_switch_distance": x.iter().map(|v| v.abs()).fold(f64::INFINITY, f64::min),
        "reason": if changed { json!("irreversible topology active set changed; rebuild state/adjoint partition") } else { Value::Null },
    })
}

