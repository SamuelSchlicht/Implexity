// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::pyval::{is_real_number, num};

pub const DEFAULTS: [(&str, f64); 6] = [
    ("lineLossCoefficientPaPerKg2S2", 0.0),
    ("valveLossCoefficientPaPerKg2S2", 0.0),
    ("pumpPressureRisePa", 0.0),
    ("pumpEfficiency", 0.7),
    ("injectorDischargeCoefficient", 0.8),
    ("minimumPressureMarginPa", 0.0),
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeedSpec {
    pub line_loss: f64,
    pub valve_loss: f64,
    pub pump_rise: f64,
    pub pump_efficiency: f64,
    pub discharge: f64,
    pub minimum_margin: f64,
}

impl FeedSpec {
    #[must_use]
    pub fn from_map(m: &Map<String, Value>) -> Self {
        let g = |k: &str, d: f64| m.get(k).and_then(Value::as_f64).unwrap_or(d);
        Self {
            line_loss: g(DEFAULTS[0].0, DEFAULTS[0].1),
            valve_loss: g(DEFAULTS[1].0, DEFAULTS[1].1),
            pump_rise: g(DEFAULTS[2].0, DEFAULTS[2].1),
            pump_efficiency: g(DEFAULTS[3].0, DEFAULTS[3].1),
            discharge: g(DEFAULTS[4].0, DEFAULTS[4].1),
            minimum_margin: g(DEFAULTS[5].0, DEFAULTS[5].1),
        }
    }
}


pub fn normalize_feed(spec: Option<&Value>, path: &str) -> PResult<Option<Map<String, Value>>> {
    let Some(spec) = spec.filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(map) = spec.as_object() else {
        return Err(ModelError::validation("feed-system screening declaration must be an object", path));
    };
    let mut out = map.clone();
    let mut unknown: Vec<String> =
        out.keys().filter(|k| !DEFAULTS.iter().any(|(d, _)| d == k)).cloned().collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(
            ModelError::validation("unknown feed-system fields", path).detail("unknown", json!(unknown))
        );
    }
    for (k, v) in DEFAULTS {
        let value = out.get(k).cloned().unwrap_or_else(|| num(v));
        if !is_real_number(&value) {
            return Err(ModelError::validation(
                format!("{k} must be a finite real number"),
                format!("{path}.{k}"),
            ));
        }
        out.insert(k.to_string(), num(value.as_f64().unwrap_or(f64::NAN)));
    }
    let f = |k: &str| out.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
    for k in [
        "lineLossCoefficientPaPerKg2S2",
        "valveLossCoefficientPaPerKg2S2",
        "pumpPressureRisePa",
        "minimumPressureMarginPa",
    ] {
        if !f(k).is_finite() || f(k) < 0.0 {
            return Err(ModelError::validation(
                format!("{k} must be finite and nonnegative"),
                format!("{path}.{k}"),
            ));
        }
    }
    let eff = f("pumpEfficiency");
    if !(eff > 0.0 && eff <= 1.0) {
        return Err(ModelError::validation(
            "pump efficiency must lie in (0,1]",
            format!("{path}.pumpEfficiency"),
        ));
    }
    let cd = f("injectorDischargeCoefficient");
    if !(cd > 0.0 && cd <= 1.0) {
        return Err(ModelError::validation(
            "injector discharge coefficient must lie in (0,1]",
            format!("{path}.injectorDischargeCoefficient"),
        ));
    }
    Ok(Some(out))
}

#[must_use]
pub fn metrics<S: Scalar>(mdot: S, rho: S, area: S, upstream: S, spec: &FeedSpec) -> [S; 6] {
    let line_dp = mdot * mdot * (spec.line_loss + spec.valve_loss);
    let q = mdot / (area * spec.discharge);
    let injector_dp = S::from_f64(0.5) / rho * (q * q);
    let gross = upstream + spec.pump_rise;
    let downstream = gross - line_dp - injector_dp;
    let pump_power = S::from_f64(spec.pump_rise) * mdot / (rho * spec.pump_efficiency);
    let velocity = mdot / (rho * area);
    [line_dp, injector_dp, downstream, pump_power, S::from_f64(spec.minimum_margin), velocity]
}


pub fn evaluate(
    mdot: f64,
    rho: f64,
    area: f64,
    upstream: f64,
    spec: Option<&Value>,
) -> PResult<Map<String, Value>> {
    let spec = normalize_feed(spec, "feedSystem")?
        .ok_or_else(|| ModelError::invalid("feedSystem declaration required"))?;
    for (name, value) in [
        ("mass flow", mdot),
        ("density", rho),
        ("inlet area", area),
        ("upstream absolute pressure", upstream),
    ] {
        if !value.is_finite() || value < 0.0 || (name != "mass flow" && value == 0.0) {
            return Err(ModelError::invalid(format!(
                "{name} must be a finite real scalar; mass flow must be nonnegative and other values positive"
            )));
        }
    }
    let vals = metrics(mdot, rho, area, upstream, &FeedSpec::from_map(&spec));
    if !vals.iter().all(Scalar::is_finite) {
        return Err(ModelError::invalid("feed-system metrics overflowed; no clipping applied"));
    }
    let names = [
        "linePressureDropPa",
        "injectorPressureDropPa",
        "availableChamberPressurePa",
        "pumpPowerW",
        "requiredPressureMarginPa",
        "injectionVelocityMPerS",
    ];
    Ok(names.iter().zip(vals).map(|(n, v)| ((*n).to_string(), num(v))).collect())
}
