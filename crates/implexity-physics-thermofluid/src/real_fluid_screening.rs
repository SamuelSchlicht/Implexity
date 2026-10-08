// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_physics_base::model_errors::{PhysicsError, PhysicsResult};

pub const RU: f64 = 8.314_462_618_153_24;

pub const PROPERTY_NAMES: [&str; 7] = [
    "densityKgPerM3",
    "enthalpyJPerKg",
    "internalEnergyJPerKg",
    "soundSpeedMPerS",
    "dynamicViscosityPaS",
    "thermalConductivityWPerMK",
    "compressibilityFactor",
];

fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::String(s) => {
            let t = s.trim().to_ascii_lowercase();
            match t.as_str() {
                "inf" | "+inf" | "infinity" | "+infinity" => Some(f64::INFINITY),
                "-inf" | "-infinity" => Some(f64::NEG_INFINITY),
                "nan" | "+nan" | "-nan" => Some(f64::NAN),
                other => other.parse().ok(),
            }
        }
        _ => None,
    }
}

fn float_or_raise(v: &Value) -> PhysicsResult<f64> {
    py_float(v).ok_or_else(|| match v {
        Value::String(s) => PhysicsError::value(format!(
            "could not convert string to float: {}",
            implexity_core::py_repr::repr_str(s)
        )),
        Value::Null => {
            PhysicsError::Type("float() argument must be a string or a real number, not 'NoneType'".into())
        }
        Value::Array(_) => {
            PhysicsError::Type("float() argument must be a string or a real number, not 'list'".into())
        }
        _ => PhysicsError::Type("float() argument must be a string or a real number, not 'dict'".into()),
    })
}

fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => implexity_core::py_repr::PyValue::from_json(other).repr(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}


#[allow(clippy::too_many_lines)]
pub fn normalize_real_fluid(spec: Option<&Value>, path: &str) -> PhysicsResult<Option<Map<String, Value>>> {
    let Some(spec) = spec.filter(|v| !v.is_null()) else { return Ok(None) };
    let Some(src) = spec.as_object() else {
        return Err(PhysicsError::validation("real-fluid screening declaration must be an object", path));
    };
    let mut out = src.clone();
    let model = match out.get("model") {
        Some(v) if truthy(v) => py_str(v).trim().to_string(),
        _ => String::new(),
    };
    if model != "peng_robinson" && model != "compressed_liquid" {
        return Err(PhysicsError::validation(
            "real-fluid screening model must be Peng-Robinson or compressed liquid",
            format!("{path}.model"),
        ));
    }
    out.insert("model".into(), json!(model));
    let pos = |out: &mut Map<String, Value>, k: &str| -> PhysicsResult<()> {
        let v = out.get(k).and_then(py_float).ok_or_else(|| {
            PhysicsError::validation(format!("{k} is required and must be numeric"), format!("{path}.{k}"))
        })?;
        if !v.is_finite() || v <= 0.0 {
            return Err(PhysicsError::validation(
                format!("{k} must be finite and positive"),
                format!("{path}.{k}"),
            ));
        }
        out.insert(k.into(), json!(v));
        Ok(())
    };
    if model == "peng_robinson" {
        for k in ["criticalTemperatureK", "criticalPressurePa", "molarMassKgPerMol", "heatCapacityJPerKgK"] {
            pos(&mut out, k)?;
        }
        let omega = float_or_raise(out.get("acentricFactor").unwrap_or(&json!(0.0)))?;
        if !omega.is_finite() {
            return Err(PhysicsError::validation(
                "acentric factor must be finite",
                format!("{path}.acentricFactor"),
            ));
        }
        out.insert("acentricFactor".into(), json!(omega));
        let hint = match out.get("phaseHint") {
            Some(v) if truthy(v) => py_str(v),
            _ => "vapor".into(),
        };
        out.insert("phaseHint".into(), json!(hint));
        if !["vapor", "supercritical", "dense_single_phase"].contains(&hint.as_str()) {
            return Err(PhysicsError::validation(
                "Peng-Robinson screening supports vapor, dense-single-phase, or supercritical states only",
                format!("{path}.phaseHint"),
            ));
        }
        let gamma = float_or_raise(out.get("gammaEstimate").unwrap_or(&json!(1.2)))?;
        out.insert("gammaEstimate".into(), json!(gamma));
        if !gamma.is_finite() || gamma <= 1.0 {
            return Err(PhysicsError::validation(
                "gammaEstimate must be finite and greater than one",
                format!("{path}.gammaEstimate"),
            ));
        }
    } else {
        for k in [
            "referenceDensityKgPerM3",
            "referencePressurePa",
            "referenceTemperatureK",
            "bulkModulusPa",
            "heatCapacityJPerKgK",
        ] {
            pos(&mut out, k)?;
        }
        let alpha = float_or_raise(out.get("thermalExpansionPerK").unwrap_or(&json!(0.0)))?;
        if !alpha.is_finite() || alpha < 0.0 {
            return Err(PhysicsError::validation(
                "thermal expansion must be finite and nonnegative",
                format!("{path}.thermalExpansionPerK"),
            ));
        }
        out.insert("thermalExpansionPerK".into(), json!(alpha));
    }
    if let Some(vr) = out.get("validTemperatureRangeK").filter(|v| !v.is_null()).cloned() {
        let Some(items) = vr.as_array().filter(|a| a.len() == 2) else {
            return Err(PhysicsError::validation(
                "validTemperatureRangeK must be [Tmin,Tmax]",
                format!("{path}.validTemperatureRangeK"),
            ));
        };
        let lo = float_or_raise(&items[0])?;
        let hi = float_or_raise(&items[1])?;
        if !(lo.is_finite() && hi.is_finite() && 0.0 < lo && lo < hi) {
            return Err(PhysicsError::validation(
                "validTemperatureRangeK must contain finite positive increasing bounds",
                format!("{path}.validTemperatureRangeK"),
            ));
        }
        out.insert("validTemperatureRangeK".into(), json!([lo, hi]));
    }
    let mu = float_or_raise(out.get("dynamicViscosityPaS").unwrap_or(&json!(1e-4)))?;
    let k = float_or_raise(out.get("thermalConductivityWPerMK").unwrap_or(&json!(0.1)))?;
    out.insert("dynamicViscosityPaS".into(), json!(mu));
    out.insert("thermalConductivityWPerMK".into(), json!(k));
    if [mu, k].iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(PhysicsError::validation("transport properties must be positive", path));
    }
    Ok(Some(out))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RealFluidModel {
    PengRobinson {
        tc: f64,
        pc: f64,
        molar_mass: f64,
        cp: f64,
        omega: f64,
        gamma: f64,
    },
    CompressedLiquid {
        rho0: f64,
        p0: f64,
        t0: f64,
        bulk: f64,
        cp: f64,
        alpha: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RealFluidSpec {
    pub model: RealFluidModel,
    pub viscosity: f64,
    pub conductivity: f64,
}

impl RealFluidSpec {

    pub fn from_normalized(spec: &Map<String, Value>) -> PhysicsResult<Self> {
        let get = |k: &str| -> PhysicsResult<f64> {
            spec.get(k)
                .and_then(Value::as_f64)
                .ok_or_else(|| PhysicsError::Key(implexity_core::py_repr::repr_str(k)))
        };
        let model = match spec.get("model").and_then(Value::as_str) {
            Some("compressed_liquid") => RealFluidModel::CompressedLiquid {
                rho0: get("referenceDensityKgPerM3")?,
                p0: get("referencePressurePa")?,
                t0: get("referenceTemperatureK")?,
                bulk: get("bulkModulusPa")?,
                cp: get("heatCapacityJPerKgK")?,
                alpha: get("thermalExpansionPerK")?,
            },
            _ => RealFluidModel::PengRobinson {
                tc: get("criticalTemperatureK")?,
                pc: get("criticalPressurePa")?,
                molar_mass: get("molarMassKgPerMol")?,
                cp: get("heatCapacityJPerKgK")?,
                omega: get("acentricFactor")?,
                gamma: spec.get("gammaEstimate").and_then(Value::as_f64).unwrap_or(1.2),
            },
        };
        Ok(Self {
            model,
            viscosity: get("dynamicViscosityPaS")?,
            conductivity: get("thermalConductivityWPerMK")?,
        })
    }
}

fn pr_z<S: Scalar>(p: S, t: S, tc: f64, pc: f64, omega: f64) -> (S, S, f64) {
    let kappa = 0.37464 + 1.54226 * omega - 0.26992 * omega * omega;
    let alpha_root = (S::one() - (t / tc).sqrt()) * kappa + 1.0;
    let alpha = alpha_root * alpha_root;
    let a = 0.45724 * RU * RU * tc * tc / pc;
    let b = 0.07780 * RU * tc / pc;
    let aa = alpha * a;
    let big_a = aa * p / (t * (RU * RU) * t);
    let big_b = p * b / (t * RU);
    let mut z = (big_b + 0.05).max_f64(1.0);
    for _ in 0..16 {
        let c1 = big_a - big_b * big_b * 3.0 - big_b * 2.0;
        let c0 = big_a * big_b - big_b * big_b - big_b * big_b * big_b;
        let f = z * z * z - (S::one() - big_b) * (z * z) + c1 * z - c0;
        let df = z * z * 3.0 - (S::one() - big_b) * 2.0 * z + c1;
        z = (z - f / (df + 1e-14)).maximum(big_b + 1e-8);
    }
    (z, aa, b)
}

#[must_use]
pub fn density<S: Scalar>(p: S, t: S, spec: &RealFluidSpec) -> S {
    match spec.model {
        RealFluidModel::CompressedLiquid { rho0, p0, t0, bulk, alpha, .. } => {
            ((p - p0) / bulk - (t - t0) * alpha).exp() * rho0
        }
        RealFluidModel::PengRobinson { tc, pc, molar_mass, omega, .. } => {
            let (z, _, _) = pr_z(p, t, tc, pc, omega);
            let v = z * RU * t / p;
            S::from_f64(molar_mass) / v
        }
    }
}

#[must_use]
pub fn properties<S: Scalar>(p: S, t: S, spec: &RealFluidSpec) -> [S; 7] {
    let (rho, h, u, a, z) = match spec.model {
        RealFluidModel::CompressedLiquid { rho0, p0, t0, bulk, cp, alpha } => {
            let rho = ((p - p0) / bulk - (t - t0) * alpha).exp() * rho0;
            let h = (t - t0) * cp + (p - p0) / rho;
            let u = h - p / rho;
            let a = (S::from_f64(bulk) / rho).max_f64(1e-12).sqrt();
            (rho, h, u, a, S::zero())
        }
        RealFluidModel::PengRobinson { tc, pc, molar_mass, cp, omega, gamma } => {
            let (z, aa, b) = pr_z(p, t, tc, pc, omega);
            let v = z * RU * t / p;
            let rho = S::from_f64(molar_mass) / v;
            let den = v * v + v * (2.0 * b) - b * b;
            let vb = v - b;
            let dpdv = -(t * RU) / (vb * vb) + aa * (v * 2.0 + 2.0 * b) / (den * den);
            let dpdrho = -(v * v) / molar_mass * dpdv;
            let a = (dpdrho * gamma).max_f64(1e-12).sqrt();
            let t_ref = 298.15;
            let h = (t - t_ref) * cp + p / rho;
            let u = h - p / rho;
            (rho, h, u, a, z)
        }
    };
    [rho, h, u, a, S::from_f64(spec.viscosity), S::from_f64(spec.conductivity), z]
}

#[must_use]
pub fn evaluate(p: f64, t: f64, spec: &RealFluidSpec) -> Map<String, Value> {
    PROPERTY_NAMES.iter().zip(properties(p, t, spec)).map(|(k, v)| ((*k).to_string(), json!(v))).collect()
}

#[must_use]
pub fn jacobian(p: f64, t: f64, spec: &RealFluidSpec) -> [[f64; 2]; 7] {
    let out = properties(Dual::<2>::new(p, [1.0, 0.0]), Dual::<2>::new(t, [0.0, 1.0]), spec);
    out.map(|d| d.eps)
}
