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

