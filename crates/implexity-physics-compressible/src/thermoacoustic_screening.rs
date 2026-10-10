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


use implexity_ad::Scalar;
use implexity_physics_base::PhysicsError;
use serde_json::{Map, Value};
use crate::errors::{PResult, ModelError};
use crate::pyval::{num, py_float};

pub const DEFAULTS: [(&str, f64); 5] = [
    ("heatReleaseDelayS", 1e-3),
    ("dampingRatio", 0.03),
    ("couplingGain", 0.05),
    ("minimumOpenFraction", 0.1),
    ("transitionWidth", 0.05),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermoacousticSpec {
    pub delay: f64,
    pub damping: f64,
    pub gain: f64,
    pub minimum_open: f64,
    pub width: f64,
}

impl ThermoacousticSpec {
    #[must_use]
    pub fn from_map(m: &Map<String, Value>) -> Self {
        let g = |i: usize| m.get(DEFAULTS[i].0).and_then(Value::as_f64).unwrap_or(DEFAULTS[i].1);
        Self { delay: g(0), damping: g(1), gain: g(2), minimum_open: g(3), width: g(4) }
    }
}


pub fn normalize_thermoacoustic(spec: Option<&Value>, path: &str) -> PResult<Option<Map<String, Value>>> {
    let Some(spec) = spec.filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(map) = spec.as_object() else {
        return Err(ModelError::validation(
            "thermoacoustic screening declaration must be an object",
            path,
        ));
    };
    let mut out = map.clone();
    for (k, d) in DEFAULTS {
        let v = match out.get(k) {
            Some(v) => py_float(v).ok_or_else(|| {
                ModelError::from(PhysicsError::value(format!(
                    "could not convert string to float: {}",
                    implexity_core::pyobj::repr(v)
                )))
            })?,
            None => d,
        };
        if !v.is_finite() || v < 0.0 {
            return Err(ModelError::validation(
                format!("{k} must be finite and nonnegative"),
                format!("{path}.{k}"),
            ));
        }
        out.insert(k.to_string(), num(v));
    }
    let f = |k: &str| out.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
    if f("transitionWidth") <= 0.0 {
        return Err(ModelError::validation(
            "transition width must be positive",
            format!("{path}.transitionWidth"),
        ));
    }
    if f("minimumOpenFraction") > 1.0 {
        return Err(ModelError::validation(
            "minimumOpenFraction must lie in [0,1]",
            format!("{path}.minimumOpenFraction"),
        ));
    }
    Ok(Some(out))
}

#[must_use]
pub fn metrics<S: Scalar>(
    axial_open: &[S],
    dx: f64,
    sound_speed: S,
    efficiency: S,
    spec: &ThermoacousticSpec,
) -> [S; 4] {
    let total: S = axial_open.iter().map(|a| ((*a - spec.minimum_open) / spec.width).sigmoid()).sum();
    let length = (total * dx).max_f64(dx);
    let frequency = sound_speed / (length * 2.0);
    let phase = (frequency * (2.0 * std::f64::consts::PI) * spec.delay).cos();
    let destabilizing = efficiency * spec.gain * phase.max_f64(0.0);
    let margin = S::from_f64(spec.damping) - destabilizing;
    [frequency, margin, phase, length]
}
