// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
