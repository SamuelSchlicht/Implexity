// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
