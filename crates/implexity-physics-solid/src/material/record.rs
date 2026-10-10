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

use std::sync::Arc;

use serde_json::{Map, Value};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_core::pyobj::list_repr;

use crate::pchip::PropertyCurve;

pub const MATERIAL_KEYS: [&str; 15] = [
    "E",
    "nu",
    "yield_stress",
    "H_iso",
    "H_kin",
    "alpha",
    "k",
    "cp",
    "density",
    "creep_rate_ref",
    "creep_stress_ref",
    "creep_exponent",
    "creep_activation_J_mol",
    "creep_T_ref",
    "taylor_quinney",
];

pub const TEMPERATURE_KEYS: [&str; 5] = ["E", "yield_stress", "alpha", "k", "cp"];

#[must_use]
pub const fn key_index(key: &str) -> usize {
    let bytes = key.as_bytes();
    let mut i = 0;
    while i < MATERIAL_KEYS.len() {
        let k = MATERIAL_KEYS[i].as_bytes();
        if k.len() == bytes.len() {
            let mut j = 0;
            let mut same = true;
            while j < k.len() {
                if k[j] != bytes[j] {
                    same = false;
                }
                j += 1;
            }
            if same {
                return i;
            }
        }
        i += 1;
    }
    usize::MAX
}

pub mod idx {
    use super::key_index;
    pub const E: usize = key_index("E");
    pub const NU: usize = key_index("nu");
    pub const YIELD: usize = key_index("yield_stress");
    pub const H_ISO: usize = key_index("H_iso");
    pub const H_KIN: usize = key_index("H_kin");
    pub const ALPHA: usize = key_index("alpha");
    pub const K: usize = key_index("k");
    pub const CP: usize = key_index("cp");
    pub const DENSITY: usize = key_index("density");
    pub const CREEP_RATE: usize = key_index("creep_rate_ref");
    pub const CREEP_STRESS: usize = key_index("creep_stress_ref");
    pub const CREEP_EXPONENT: usize = key_index("creep_exponent");
    pub const CREEP_ACTIVATION: usize = key_index("creep_activation_J_mol");
    pub const CREEP_T_REF: usize = key_index("creep_T_ref");
    pub const TAYLOR_QUINNEY: usize = key_index("taylor_quinney");
}

#[must_use]
pub fn temperature_slot(key_index: usize) -> Option<usize> {
    TEMPERATURE_KEYS
        .iter()
        .position(|k| MATERIAL_KEYS[key_index] == *k)
}

fn contract<T>(message: impl Into<String>) -> Result<T, CaeError> {
    Err(CaeError::contract(message))
}

#[must_use]
pub fn finite_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(Scalar::is_finite),
        _ => None,
    }
}

fn sorted_keys<'a>(keys: impl Iterator<Item = &'a String>) -> String {
    let mut v: Vec<&String> = keys.collect();
    v.sort();
    list_repr(&v)
}

pub fn validate_material(raw: &Value) -> Result<Map<String, Value>, CaeError> {
    let Some(m) = raw.as_object() else {
        return contract("material data must be an object");
    };
    let mut required: Vec<&str> = MATERIAL_KEYS.to_vec();
    required.extend([
        "name",
        "provenance",
        "T_ref",
        "T_min",
        "T_max",
        "temperature_slopes",
    ]);
    let missing: Vec<String> = required
        .iter()
        .filter(|k| !m.contains_key(**k))
        .map(|k| (*k).to_string())
        .collect();
    if !missing.is_empty() {
        return contract(format!(
            "material missing explicit data: {}",
            sorted_keys(missing.iter())
        ));
    }
    let extra: Vec<&String> = m
        .keys()
        .filter(|k| !required.contains(&k.as_str()))
        .collect();
    if !extra.is_empty() {
        return contract(format!(
            "unknown material keys: {}",
            sorted_keys(extra.into_iter())
        ));
    }
    if implexity_core::pyobj::py_str(&m["provenance"])
        .trim()
        .is_empty()
    {
        return contract("material provenance required");
    }
    let keys: Vec<&str> = MATERIAL_KEYS
        .iter()
        .copied()
        .chain(["T_ref", "T_min", "T_max"])
        .collect();
    if !keys.iter().all(|k| finite_number(&m[*k]).is_some()) {
        return contract("material parameters must be finite scalars");
    }
    let f = |k: &str| finite_number(&m[k]).unwrap_or(f64::NAN);
    if !(0.0 < f("T_min") && f("T_min") <= f("T_ref") && f("T_ref") < f("T_max")) {
        return contract("invalid material temperature validity interval");
    }
    if !(-1.0 < f("nu") && f("nu") < 0.5) {
        return contract("invalid Poisson ratio");
    }
    if [
        "E",
        "yield_stress",
        "k",
        "cp",
        "density",
        "creep_stress_ref",
        "creep_T_ref",
    ]
    .iter()
    .any(|k| f(k) <= 0.0)
    {
        return contract("elastic, thermal and reference coefficients must be positive");
    }
    if ["H_iso", "H_kin", "creep_rate_ref", "creep_activation_J_mol"]
        .iter()
        .any(|k| f(k) < 0.0)
    {
        return contract("hardening/creep coefficients cannot be negative");
    }
    if f("creep_exponent") < 1.0 {
        return contract("creep exponent must be at least one");
    }
    if !(0.0..=1.0).contains(&f("taylor_quinney")) {
        return contract("invalid heat conversion fraction");
    }
    let slopes = m["temperature_slopes"].as_object();
    let slopes_ok = slopes.is_some_and(|s| {
        s.len() == TEMPERATURE_KEYS.len()
            && TEMPERATURE_KEYS.iter().all(|k| s.contains_key(*k))
            && s.values().all(|v| finite_number(v).is_some())
    });
    let Some(slopes) = slopes.filter(|_| slopes_ok) else {
        return contract(
            "explicit finite temperature slopes required for E, yield_stress, alpha, k, cp",
        );
    };
    for key in TEMPERATURE_KEYS {
        let slope = finite_number(&slopes[key]).unwrap_or(f64::NAN);
        let endpoints: Vec<f64> = [f("T_min"), f("T_max")]
            .iter()
            .map(|t| f(key) + slope * (t - f("T_ref")))
            .collect();
        if !endpoints.iter().all(Scalar::is_finite) {
            return contract(format!(
                "{key} becomes nonfinite within the authored temperature range"
            ));
        }
        if key != "alpha" && endpoints.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0 {
            return contract(format!(
                "{key} becomes nonpositive within the authored temperature range"
            ));
        }
    }
    Ok(m.clone())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseTransition {
    pub temperature: f64,
    pub width: f64,
    pub latent_heat: f64,
}

pub type TableCurves = [PropertyCurve; 5];

#[derive(Debug, Clone, PartialEq)]
pub struct SolidMaterial {
    pub raw: Map<String, Value>,
    pub values: [f64; 15],
    pub creep_curve: Option<[f64; 11]>,
    pub creep_validity: Option<[f64; 5]>,
    pub slopes: [f64; 5],
    pub t_ref: f64,
    pub t_min: f64,
    pub t_max: f64,
    pub phase: Option<PhaseTransition>,
    pub constant_strain_capacity: Option<f64>,
    pub tables: Option<Arc<TableCurves>>,
    pub kinematic: Option<Arc<Vec<(PropertyCurve, PropertyCurve)>>>,
}

impl SolidMaterial {
    pub(crate) fn from_validated(raw: Map<String, Value>) -> Self {
        let num = |k: &str| raw.get(k).and_then(finite_number).unwrap_or(f64::NAN);
        let values = MATERIAL_KEYS.map(num);
        let slopes = TEMPERATURE_KEYS.map(|k| {
            raw.get("temperature_slopes")
                .and_then(|s| s.get(k))
                .and_then(finite_number)
                .unwrap_or(0.0)
        });
        Self {
            values,
            creep_curve: None,
            creep_validity: None,
            slopes,
            t_ref: num("T_ref"),
            t_min: num("T_min"),
            t_max: num("T_max"),
            raw,
            phase: None,
            constant_strain_capacity: None,
            tables: None,
            kinematic: None,
        }
    }

    #[must_use]
    pub fn get(&self, key_index: usize) -> f64 {
        self.values[key_index]
    }

    #[must_use]
    pub fn density(&self) -> f64 {
        self.values[idx::DENSITY]
    }

    #[must_use]
    pub fn name(&self) -> String {
        self.raw
            .get("name")
            .map(implexity_core::pyobj::py_str)
            .unwrap_or_default()
    }

    #[must_use]
    pub fn provenance(&self) -> Value {
        self.raw.get("provenance").cloned().unwrap_or(Value::Null)
    }

    #[must_use]
    pub fn minimum_yield(&self) -> f64 {
        if let Some(t) = self.raw.get("temperature_table") {
            let values = t
                .get("yield_stress")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            return values
                .iter()
                .filter_map(Value::as_f64)
                .fold(f64::INFINITY, f64::min);
        }
        let slope = self.slopes[1];
        [self.t_min, self.t_max]
            .iter()
            .map(|t| self.values[idx::YIELD] + slope * (t - self.t_ref))
            .fold(f64::INFINITY, f64::min)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Props<S> {
    pub values: [S; 15],
    pub creep_curve: Option<[S; 11]>,
    pub creep_validity: Option<[f64; 5]>,
    pub creep_multiplier: S,
    pub temperature: S,
    pub kin_c: Vec<S>,
    pub kin_gamma: Vec<S>,
}

impl<S: Scalar> Props<S> {
    pub fn new(values: [S; 15], temperature: S) -> Self {
        Self {
            values,
            creep_curve: None,
            creep_validity: None,
            creep_multiplier: S::one(),
            temperature,
            kin_c: Vec::new(),
            kin_gamma: Vec::new(),
        }
    }

    #[must_use]
    pub fn get(&self, key_index: usize) -> S {
        self.values[key_index]
    }

    #[must_use]
    pub fn mix(a: &Self, b: &Self, c: S, temperature: S) -> Self {
        let w = -c + 1.0;
        let m = |x: S, y: S| w * x + c * y;
        Self {
            values: std::array::from_fn(|i| m(a.values[i], b.values[i])),
            creep_multiplier: m(a.creep_multiplier, b.creep_multiplier),
            creep_validity: match (a.creep_validity, b.creep_validity) {
                (Some(x), Some(y)) => Some([
                    x[0].max(y[0]),
                    x[1].min(y[1]),
                    x[2].min(y[2]),
                    x[3].max(y[3]),
                    x[4].min(y[4]),
                ]),
                _ => None,
            },
            creep_curve: match (a.creep_curve, b.creep_curve) {
                (Some(x), Some(y)) => Some(std::array::from_fn(|i| m(x[i], y[i]))),
                _ => None,
            },
            temperature,
            kin_c: a
                .kin_c
                .iter()
                .zip(&b.kin_c)
                .map(|(x, y)| m(*x, *y))
                .collect(),
            kin_gamma: a
                .kin_gamma
                .iter()
                .zip(&b.kin_gamma)
                .map(|(x, y)| m(*x, *y))
                .collect(),
        }
    }
}
