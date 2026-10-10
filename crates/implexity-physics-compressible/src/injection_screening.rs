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

pub const REQUIRED: [&str; 4] =
    ["surfaceTensionNPerM", "liquidViscosityPaS", "initialSauterMeanDiameterM", "evaporationConstantM2PerS"];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InjectionSpec {
    pub surface_tension: f64,
    pub viscosity: f64,
    pub d0: f64,
    pub evaporation: f64,
    pub critical_weber: f64,
    pub weber_width: f64,
}

impl InjectionSpec {
    #[must_use]
    pub fn from_map(m: &Map<String, Value>) -> Self {
        let g = |k: &str| m.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
        Self {
            surface_tension: g(REQUIRED[0]),
            viscosity: g(REQUIRED[1]),
            d0: g(REQUIRED[2]),
            evaporation: g(REQUIRED[3]),
            critical_weber: g("criticalWeberNumber"),
            weber_width: g("weberTransitionWidth"),
        }
    }
}

fn float_or_value_error(value: &Value) -> PResult<f64> {
    py_float(value).ok_or_else(|| {
        ModelError::from(PhysicsError::value(format!(
            "could not convert string to float: {}",
            implexity_core::pyobj::repr(value)
        )))
    })
}


pub fn normalize_injection(spec: Option<&Value>, path: &str) -> PResult<Option<Map<String, Value>>> {
    let Some(spec) = spec.filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(map) = spec.as_object() else {
        return Err(ModelError::validation(
            "multiphase injection screening declaration must be an object",
            path,
        ));
    };
    let mut out = map.clone();
    for k in REQUIRED {
        let v = out.get(k).and_then(py_float).ok_or_else(|| {
            ModelError::validation(format!("{k} is required and must be numeric"), format!("{path}.{k}"))
        })?;
        if !v.is_finite() || v <= 0.0 {
            return Err(ModelError::validation(
                format!("{k} must be finite and positive"),
                format!("{path}.{k}"),
            ));
        }
        out.insert(k.to_string(), num(v));
    }
    for (k, d) in [("criticalWeberNumber", 12.0), ("weberTransitionWidth", 3.0)] {
        let v = match out.get(k) {
            Some(v) => float_or_value_error(v)?,
            None => d,
        };
        out.insert(k.to_string(), num(v));
        if !v.is_finite() || v <= 0.0 {
            return Err(ModelError::validation(
                "Weber transition parameters must be finite and positive",
                path,
            ));
        }
    }
    Ok(Some(out))
}

#[must_use]
pub fn metrics<S: Scalar>(rho_l: S, rho_g: S, u: S, dh: S, residence: S, spec: &InjectionSpec) -> [S; 5] {
    let d = dh.max_f64(1e-9);
    let t = residence.max_f64(0.0);
    let we = rho_g * u * u * d / spec.surface_tension;
    let oh = S::from_f64(spec.viscosity) / (rho_l * spec.surface_tension * d).sqrt();
    let atom = ((we - spec.critical_weber) / spec.weber_width).sigmoid();
    let d0 = spec.d0;
    let d2 = (S::from_f64(d0 * d0) - t * spec.evaporation).max_f64(0.0);
    let remaining = (d2 / (d0 * d0)).powf(1.5);
    let evap = (S::one() - remaining).clip(0.0, 1.0);
    let available = (atom * evap).clip(0.0, 1.0);
    [we, oh, atom, evap, available]
}

#[must_use]
pub fn evaluate(
    rho_l: f64,
    rho_g: f64,
    u: f64,
    dh: f64,
    residence: f64,
    spec: &InjectionSpec,
) -> Map<String, Value> {
    let vals = metrics(rho_l, rho_g, u, dh, residence, spec);
    let names = [
        "WeberNumber",
        "OhnesorgeNumber",
        "atomizationRegimeFraction",
        "evaporatedMassFraction",
        "vaporizedReactantFraction",
    ];
    names.iter().zip(vals).map(|(n, v)| ((*n).to_string(), num(v))).collect()
}
