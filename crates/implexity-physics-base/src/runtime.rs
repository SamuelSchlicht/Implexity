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


use serde_json::Value;

use implexity_ad::Scalar;
use implexity_core::extensions::format_g6;
use implexity_core::py_repr::repr_float;

use crate::model_errors::{PhysicsError, PhysicsResult};
use crate::models::{ModelOutputs, ModelValue};

const MARKERS: [&str; 2] = ["margin", "validity"];

fn is_margin(key: &str) -> bool {
    let lk = key.to_lowercase();
    MARKERS.iter().any(|m| lk.contains(m))
}

fn min_or_nan(values: &[f64]) -> f64 {
    if values.is_empty() || !values.iter().all(Scalar::is_finite) {
        return f64::NAN;
    }
    values.iter().copied().fold(f64::INFINITY, f64::min)
}

fn json_margin(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Bool(b) => f64::from(u8::from(*b)),
        other => crate::array::Tensor::from_json(other).map_or(f64::NAN, |t| min_or_nan(t.data())),
    }
}

#[must_use]
pub fn json_margins(payload: &serde_json::Map<String, Value>) -> Vec<(String, f64)> {
    payload.iter().filter(|(k, _)| is_margin(k)).map(|(k, v)| (k.clone(), json_margin(v))).collect()
}

#[must_use]
pub fn output_margins<S: Scalar>(payload: &ModelOutputs<S>) -> Vec<(String, f64)> {
    payload
        .iter()
        .filter(|(k, _)| is_margin(k))
        .map(|(k, v)| {
            let m = match v {
                ModelValue::Array(t) if t.ndim() == 0 => t.at(0).value(),
                ModelValue::Array(t) => min_or_nan(&t.values().into_data()),
                ModelValue::Text(_) => f64::NAN,
            };
            (k.to_string(), m)
        })
        .collect()
}


pub fn enforce_runtime_validity(
    margins: &[(String, f64)],
    addin_id: &str,
    tolerance: f64,
    nonnegative_margins: &[&str],
) -> PhysicsResult<()> {
    if !tolerance.is_finite() || tolerance < 0.0 {
        return Err(PhysicsError::value("validity tolerance must be finite and nonnegative"));
    }
    for (key, value) in margins {
        if !value.is_finite() {
            return Err(PhysicsError::value(format!(
                "{addin_id}: non-finite validity margin {key}={}",
                repr_float(*value)
            )));
        }
        let crossed = if nonnegative_margins.contains(&key.as_str()) {
            *value < tolerance
        } else {
            *value <= tolerance
        };
        if crossed {
            return Err(PhysicsError::value(format!(
                "{addin_id}: validity boundary crossed for {key}: {}",
                format_g6(*value)
            )));
        }
    }
    Ok(())
}

