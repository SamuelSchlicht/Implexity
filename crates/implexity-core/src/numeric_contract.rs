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



use ndarray::{ArrayD, IxDyn};
use serde_json::Value;

use crate::error::{CaeError, CaeResult};
use crate::orchestration::py_float;


pub fn require_finite(values: &[f64], label: &str) -> CaeResult<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(CaeError::contract(format!("{label} must contain only finite values")))
    }
}


pub fn require_finite_array(values: &ArrayD<f64>, label: &str) -> CaeResult<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(CaeError::contract(format!("{label} must contain only finite values")))
    }
}

fn shape_of(value: &Value, out: &mut Vec<usize>) {
    if let Value::Array(items) = value {
        out.push(items.len());
        if let Some(first) = items.first() {
            shape_of(first, out);
        }
    }
}

fn flatten(value: &Value, shape: &[usize], label: &str, out: &mut Vec<f64>) -> CaeResult<()> {
    match (value, shape.split_first()) {
        (Value::Array(items), Some((&n, rest))) if items.len() == n => {
            for item in items {
                flatten(item, rest, label, out)?;
            }
            Ok(())
        }
        (Value::Array(_), _) | (_, Some(_)) => {
            Err(CaeError::contract(format!("{label} must be a numeric array")))
        }
        (Value::Object(_) | Value::Null, None) => {
            Err(CaeError::contract(format!("{label} must be a real numeric array")))
        }
        (v, None) => {
            out.push(
                py_float(v)
                    .map_err(|_| CaeError::contract(format!("{label} must be a real numeric array")))?,
            );
            Ok(())
        }
    }
}


pub fn real_array(value: &Value, label: &str) -> CaeResult<ArrayD<f64>> {
    let mut shape = Vec::new();
    shape_of(value, &mut shape);
    let mut data = Vec::new();
    flatten(value, &shape, label, &mut data)?;
    require_finite(&data, label)?;
    ArrayD::from_shape_vec(IxDyn(&shape), data)
        .map_err(|_| CaeError::contract(format!("{label} must be a numeric array")))
}


pub fn real_scalar(value: &Value, label: &str) -> CaeResult<f64> {
    match value {
        Value::Number(n) => match n.as_f64() {
            Some(f) if f.is_finite() => Ok(f),
            _ => Err(CaeError::contract(format!("{label} must be a finite real scalar"))),
        },
        _ => Err(CaeError::contract(format!(
            "{label} must be a finite real scalar, not Boolean, text, complex or array data"
        ))),
    }
}


pub fn real_scalar_f64(value: f64, label: &str) -> CaeResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CaeError::contract(format!("{label} must be a finite real scalar")))
    }
}

