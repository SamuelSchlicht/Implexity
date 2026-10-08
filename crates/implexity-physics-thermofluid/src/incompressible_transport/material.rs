// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_core::pyobj::list_repr;
use implexity_physics_base::core_bridge::PhysicsComponent;
use implexity_physics_base::material_domains::{evaluation_interval, interval_status};

pub const INACTIVE_PHASE_MATERIAL_SCHEMA: &str = "implexity-inactive-fluid-numerical-material/1";
pub const INACTIVE_PHASE_MATERIAL_METHOD: &str = "positive_c2_endpoint_tangent_tanh";
pub const INACTIVE_PHASE_MATERIAL_SCOPE: &str = "exact_zero_physical_fluid_fraction_only";

pub const LIMITATIONS: [&str; 1] = [
    "Constant density; linear viscosity/conductivity/capacity laws in an explicit temperature interval. No phase change.",
];

const NUMERIC_KEYS: [&str; 10] = [
    "density_kg_m3",
    "mu_Pa_s",
    "k_W_mK",
    "cp_J_kgK",
    "T_ref_K",
    "T_min_K",
    "T_max_K",
    "mu_slope",
    "k_slope",
    "cp_slope",
];

fn contract<T>(message: impl Into<String>) -> Result<T, CaeError> {
    Err(CaeError::contract(message))
}


pub fn normalise_inactive_phase_numerical_material(value: &Value) -> Result<Value, CaeError> {
    let keys = ["method", "provenance", "schema", "scope"];
    let Some(m) = value.as_object().filter(|m| m.len() == 4 && keys.iter().all(|k| m.contains_key(*k)))
    else {
        return contract(format!("inactive-phase numerical material requires exactly {}", list_repr(&keys)));
    };
    if m["schema"].as_str() != Some(INACTIVE_PHASE_MATERIAL_SCHEMA) {
        return contract(format!(
            "inactive-phase numerical material schema must be {INACTIVE_PHASE_MATERIAL_SCHEMA}"
        ));
    }
    if m["method"].as_str() != Some(INACTIVE_PHASE_MATERIAL_METHOD) {
        return contract(format!(
            "inactive-phase numerical material method must be {INACTIVE_PHASE_MATERIAL_METHOD}"
        ));
    }
    if m["scope"].as_str() != Some(INACTIVE_PHASE_MATERIAL_SCOPE) {
        return contract(format!(
            "inactive-phase numerical material scope must be {INACTIVE_PHASE_MATERIAL_SCOPE}"
        ));
    }
    if m["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return contract("inactive-phase numerical material provenance required");
    }
    let mut out = m.clone();
    out.insert("schema".into(), json!(INACTIVE_PHASE_MATERIAL_SCHEMA));
    Ok(Value::Object(out))
}

#[derive(Debug, Clone, PartialEq)]
pub struct FluidCard {
    pub density: f64,
    pub mu: f64,
    pub k: f64,
    pub cp: f64,
    pub t_ref: f64,
    pub t_min: f64,
    pub t_max: f64,
    pub mu_slope: f64,
    pub k_slope: f64,
    pub cp_slope: f64,
    pub lower: f64,
    pub upper: f64,
    pub raw: Value,
}

impl FluidCard {

    pub fn from_value(p: &Value) -> Result<Self, CaeError> {
        let get = |k: &str| -> Result<f64, CaeError> {
            p.get(k)
                .and_then(Value::as_f64)
                .ok_or_else(|| CaeError::contract("finite fluid coefficients required"))
        };
        let (t_min, t_max) = (get("T_min_K")?, get("T_max_K")?);
        let (lower, upper) = evaluation_bounds_of(p, t_min, t_max)?;
        Ok(Self {
            density: get("density_kg_m3")?,
            mu: get("mu_Pa_s")?,
            k: get("k_W_mK")?,
            cp: get("cp_J_kgK")?,
            t_ref: get("T_ref_K")?,
            t_min,
            t_max,
            mu_slope: get("mu_slope")?,
            k_slope: get("k_slope")?,
            cp_slope: get("cp_slope")?,
            lower,
            upper,
            raw: p.clone(),
        })
    }

    pub fn properties<S: Scalar>(&self, t: S) -> (S, S, S) {
        let d = t - self.t_ref;
        (d * self.mu_slope + self.mu, d * self.k_slope + self.k, d * self.cp_slope + self.cp)
    }

    pub fn enthalpy<S: Scalar>(&self, t: S) -> S {
        let d = t - self.t_ref;
        d * self.cp + d * d * (0.5 * self.cp_slope)
    }

    pub fn enthalpy_increment_from_delta<S: Scalar>(&self, previous: S, delta: S) -> S {
        ((previous - self.t_ref + delta * 0.5) * self.cp_slope + self.cp) * delta
    }

    fn endpoint_increment<S: Scalar>(
        distance: S,
        delta: S,
        endpoint_value: f64,
        outward_derivative: f64,
    ) -> S {
        if outward_derivative == 0.0 {
            return delta * endpoint_value;
        }
        let l = endpoint_value / outward_derivative.abs();
        let u = distance / l;
        let d = delta / l;
        let e = (u * -2.0).exp();
        let correction = (e / (e + 1.0) * (d * -2.0).exp_m1()).ln_1p();
        if outward_derivative > 0.0 {
            return (d * 2.0 + correction) * (endpoint_value * l);
        }
        let t = u.tanh();
        let td = d.tanh();
        let change = (-t + 1.0) * (t + 1.0) * td / (t * td + 1.0);
        let square_change = change * (t * 2.0 + change);
        let floor = f64::EPSILON * endpoint_value;
        correction * (-endpoint_value * l) + (d + correction - square_change * 0.5) * (floor * l)
    }

    pub fn numerical_enthalpy_increment_from_delta<S: Scalar>(&self, previous: S, delta: S) -> S {
        let (lo, hi) = (self.lower, self.upper);
        let (slope, cp) = (self.cp_slope, self.cp);
        let forward = delta.value() >= 0.0;
        let start = if forward { previous } else { previous + delta };
        let length = delta.abs();
        let lower = if start.value() < lo { (-start + lo).minimum(length) } else { S::zero() };
        let middle =
            if start.value() < hi { (-start.max_f64(lo) + hi).minimum(length - lower) } else { S::zero() };
        let upper = length - lower - middle;
        let low = Self::endpoint_increment(
            (-start - lower + lo).max_f64(0.0),
            lower,
            cp + slope * (lo - self.t_ref),
            -slope,
        );
        let mid = self.enthalpy_increment_from_delta(start.max_f64(lo), middle);
        let high =
            Self::endpoint_increment((start - hi).max_f64(0.0), upper, cp + slope * (hi - self.t_ref), slope);
        let total = low + mid + high;
        if forward { total } else { -total }
    }

    fn endpoint_continuation<S: Scalar>(s: S, qb: f64, outward_derivative: f64) -> (S, S) {
        if outward_derivative == 0.0 {
            return (S::from_f64(qb), s * qb);
        }
        let l = qb / outward_derivative.abs();
        let u = s / l;
        let log2 = 2.0f64.ln();
        if outward_derivative > 0.0 {
            let value = ((u * -2.0).exp() + 1.0).recip() * (2.0 * qb);
            let integral = (S::zero().logaddexp(u * 2.0) - log2) * (qb * l);
            return (value, integral);
        }
        let raw = ((u * 2.0).exp() + 1.0).recip() * (2.0 * qb);
        let floor = f64::EPSILON * qb;
        let tanh_u = u.tanh();
        let value = raw + tanh_u.powi(3) * floor;
        let log_cosh = u.logaddexp(-u) - log2;
        let integral = (-S::zero().logaddexp(u * -2.0) + log2) * (qb * l)
            + (log_cosh - tanh_u.powi(2) * 0.5) * (floor * l);
        (value, integral)
    }

    fn continued_linear<S: Scalar>(&self, t: S, reference: f64, slope: f64) -> S {
        let (tmin, tmax, tref) = (self.lower, self.upper, self.t_ref);
        if t.value() < tmin {
            Self::endpoint_continuation(-t + tmin, reference + slope * (tmin - tref), -slope).0
        } else if t.value() > tmax {
            Self::endpoint_continuation(t - tmax, reference + slope * (tmax - tref), slope).0
        } else {
            (t - tref) * slope + reference
        }
    }

    pub fn numerical_properties<S: Scalar>(&self, t: S) -> (S, S, S) {
        (
            self.continued_linear(t, self.mu, self.mu_slope),
            self.continued_linear(t, self.k, self.k_slope),
            self.continued_linear(t, self.cp, self.cp_slope),
        )
    }

    pub fn numerical_enthalpy<S: Scalar>(&self, t: S) -> S {
        let (tmin, tmax, tref) = (self.lower, self.upper, self.t_ref);
        let (cp0, slope) = (self.cp, self.cp_slope);
        let physical = |value: f64| {
            let d = value - tref;
            cp0 * d + 0.5 * slope * d * d
        };
        if t.value() < tmin {
            -Self::endpoint_continuation(-t + tmin, cp0 + slope * (tmin - tref), -slope).1 + physical(tmin)
        } else if t.value() > tmax {
            Self::endpoint_continuation(t - tmax, cp0 + slope * (tmax - tref), slope).1 + physical(tmax)
        } else {
            let d = t - tref;
            d * cp0 + d * d * (0.5 * slope)
        }
    }
}

fn evaluation_bounds_of(p: &Value, t_min: f64, t_max: f64) -> Result<(f64, f64), CaeError> {
    let bounds =
        evaluation_interval(p.get("evaluation_domain"), "temperature_K", t_min, t_max, &["authored_law"])?;
    if bounds.0 <= 0.0 {
        return contract("fluid evaluation requires positive absolute temperature");
    }
    Ok(bounds)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NewtonianFluid;

impl NewtonianFluid {
    pub const IMPLEMENTATION: &'static str =
        "implexity.physics_library.incompressible_transport.NewtonianFluid";
    pub const COMPONENT_KIND: &'static str = "incompressible_fluid_material";


    pub fn validate(&self, p: &Value) -> Result<Value, CaeError> {
        let mut keys: Vec<&str> = NUMERIC_KEYS.to_vec();
        keys.push("provenance");
        keys.sort_unstable();
        let Some(m) = p.as_object() else {
            return contract(format!(
                "fluid material requires {}; optional evaluation_domain",
                list_repr(&keys)
            ));
        };
        if keys.iter().any(|k| !m.contains_key(*k))
            || m.keys().any(|k| !keys.contains(&k.as_str()) && k != "evaluation_domain")
        {
            return contract(format!(
                "fluid material requires {}; optional evaluation_domain",
                list_repr(&keys)
            ));
        }
        if m["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
            return contract("fluid material provenance required");
        }
        let mut values = BTreeMap::new();
        for key in NUMERIC_KEYS {
            match m[key].as_f64() {
                Some(v) if v.is_finite() && !m[key].is_boolean() => {
                    values.insert(key, v);
                }
                _ => return contract("finite fluid coefficients required"),
            }
        }
        let v = |k: &str| values[k];
        if !(0.0 < v("T_min_K") && v("T_min_K") <= v("T_ref_K") && v("T_ref_K") <= v("T_max_K"))
            || v("T_min_K") == v("T_max_K")
            || v("density_kg_m3") <= 0.0
        {
            return contract("invalid fluid density/temperature interval");
        }
        let (lower, upper) = evaluation_bounds_of(p, v("T_min_K"), v("T_max_K"))?;
        for (a, b) in [("mu_Pa_s", "mu_slope"), ("k_W_mK", "k_slope"), ("cp_J_kgK", "cp_slope")] {
            let endpoints = [lower, upper].map(|t| v(a) + v(b) * (t - v("T_ref_K")));
            if endpoints.iter().any(|e| !e.is_finite()) || endpoints[0].min(endpoints[1]) <= 0.0 {
                return contract(format!("{a} must stay finite positive throughout the evaluation interval"));
            }
        }
        for t in [lower, upper] {
            let d = t - v("T_ref_K");
            let h = v("cp_J_kgK") * d + 0.5 * v("cp_slope") * d * d;
            if !h.is_finite() {
                return contract("fluid enthalpy must remain finite throughout the evaluation interval");
            }
        }
        Ok(p.clone())
    }


    pub fn evaluation_bounds(&self, p: &Value) -> Result<(f64, f64), CaeError> {
        let get = |k: &str| p.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
        evaluation_bounds_of(p, get("T_min_K"), get("T_max_K"))
    }


    pub fn domain_status(&self, p: &Value, t: &[f64], support: Option<&[bool]>) -> Result<Value, CaeError> {
        let card = FluidCard::from_value(p)?;
        interval_status(t, (card.t_min, card.t_max), (card.lower, card.upper), support)
    }

    #[must_use]
    pub fn runtime_support() -> Map<String, Value> {
        let v = json!({"status": "field_component", "data": "user_required", "history": false,
            "limitations": LIMITATIONS});
        v.as_object().cloned().unwrap_or_default()
    }
}

impl PhysicsComponent for NewtonianFluid {
    fn implementation(&self) -> String {
        Self::IMPLEMENTATION.into()
    }
    fn component_kind(&self) -> Option<String> {
        Some(Self::COMPONENT_KIND.into())
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        Some(Self::runtime_support())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FluidLaw {
    pub card: FluidCard,
    pub policy: Option<Value>,
    pub fraction_floor: f64,
}

impl FluidLaw {
    pub fn properties<S: Scalar>(&self, t: S) -> (S, S, S) {
        if self.policy.is_some() { self.card.numerical_properties(t) } else { self.card.properties(t) }
    }

    pub fn enthalpy<S: Scalar>(&self, t: S) -> S {
        if self.policy.is_some() { self.card.numerical_enthalpy(t) } else { self.card.enthalpy(t) }
    }

    pub fn enthalpy_increment<S: Scalar>(&self, previous: S, delta: S) -> S {
        if self.policy.is_some() {
            self.card.numerical_enthalpy_increment_from_delta(previous, delta)
        } else {
            self.card.enthalpy_increment_from_delta(previous, delta)
        }
    }

    pub fn fraction<S: Scalar>(&self, theta: S) -> S {
        let f = self.fraction_floor;
        (-theta + 1.0) * (1.0 - f) + f
    }
}
