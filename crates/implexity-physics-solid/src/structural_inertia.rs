// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_solve::local_assembly::LocalResidual;

use crate::mandel::{self, IDENTITY, Mandel};
use crate::material::idx;
use crate::solid_elements::SolidModel;
use crate::util::{contract, num};

pub const SCHEMA_KEYS: [&str; 5] =
    ["acceleration_scale_m_s2", "mass_interpolation", "method", "rayleigh_damping", "velocity_scale_m_s"];
pub const OPTIONAL_KEYS: [&str; 1] = ["compliance_window_s"];
pub const METHODS: [&str; 1] = ["newmark_average_acceleration"];
pub const MASS_INTERPOLATIONS: [&str; 2] = ["linear", "polynomial_void_suppression"];
pub const LIMITATIONS: [&str; 4] = [
    "Newmark average acceleration (beta=1/4, gamma=1/2) with consistent linear-tetrahedron mass; no algorithmic high-frequency damping, so unresolved modes persist rather than decay.",
    "Supports are fixed in time; no prescribed support motion, contact, impact or geometric nonlinearity.",
    "Linear tetrahedra are stiff in bending; resolve the frequencies that matter and check refinement.",
    "Rayleigh damping is an authored numerical-physical assumption, not a calibrated dissipation law.",
];

fn positive(value: &Value, label: &str) -> Result<f64, CaeError> {
    let Some(v) = value.as_f64().filter(|_| value.is_number()) else {
        return contract(format!("structural_dynamics.{label} must be a finite real number"));
    };
    if !v.is_finite() || v <= 0.0 {
        return contract(format!("structural_dynamics.{label} must be positive and finite"));
    }
    Ok(v)
}

fn nonnegative(value: &Value, label: &str) -> Result<f64, CaeError> {
    let Some(v) = value.as_f64().filter(|_| value.is_number()) else {
        return contract(format!("structural_dynamics.{label} must be a finite real number"));
    };
    if !v.is_finite() || v < 0.0 {
        return contract(format!("structural_dynamics.{label} must be nonnegative and finite"));
    }
    Ok(v)
}


pub fn validate(settings: &Value, problem: &Value) -> Result<Value, CaeError> {
    let keys_ok = settings.as_object().is_some_and(|m| {
        SCHEMA_KEYS.iter().all(|k| m.contains_key(*k))
            && m.keys().all(|k| SCHEMA_KEYS.contains(&k.as_str()) || OPTIONAL_KEYS.contains(&k.as_str()))
    });
    if !keys_ok {
        return contract(format!(
            "structural_dynamics requires exactly: {} (optional: {})",
            SCHEMA_KEYS.join(", "),
            OPTIONAL_KEYS.join(", ")
        ));
    }
    if !settings["method"].as_str().is_some_and(|m| METHODS.contains(&m)) {
        return contract(format!(
            "structural_dynamics.method must be one of {}",
            implexity_core::pyobj::list_repr(&METHODS)
        ));
    }
    let mass = &settings["mass_interpolation"];
    let kind = mass.get("kind").and_then(Value::as_str).filter(|k| MASS_INTERPOLATIONS.contains(k));
    let Some(kind) = kind.filter(|_| mass.is_object()) else {
        return contract(format!(
            "structural_dynamics.mass_interpolation.kind must be one of {}",
            implexity_core::pyobj::list_repr(&MASS_INTERPOLATIONS)
        ));
    };
    let mass = if kind == "linear" {
        if mass.as_object().map_or(0, Map::len) != 1 {
            return contract("linear mass interpolation takes no parameters");
        }
        json!({"kind": "linear"})
    } else {
        if !crate::util::has_exact_keys(mass, &["kind", "threshold"]) {
            return contract("polynomial_void_suppression requires exactly kind and threshold");
        }
        let threshold = positive(&mass["threshold"], "mass_interpolation.threshold")?;
        if threshold > 0.5 {
            return contract("mass-interpolation threshold must not exceed 0.5");
        }
        json!({"kind": kind, "threshold": threshold})
    };
    let damping = &settings["rayleigh_damping"];
    if !crate::util::has_exact_keys(damping, &["mass_s_inv", "stiffness_s"]) {
        return contract("rayleigh_damping requires exactly mass_s_inv and stiffness_s");
    }
    let damping = json!({
        "mass_s_inv": nonnegative(&damping["mass_s_inv"], "rayleigh_damping.mass_s_inv")?,
        "stiffness_s": nonnegative(&damping["stiffness_s"], "rayleigh_damping.stiffness_s")?});
    for bc in problem["displacement_bcs"].as_array().into_iter().flatten() {
        let moving = bc["values"].as_array().into_iter().flatten().any(|v| v.as_f64() != Some(0.0));
        if moving {
            return contract(
                "structural_dynamics requires fixed supports: every prescribed displacement must be zero at all times",
            );
        }
    }
    let window = match settings.get("compliance_window_s") {
        None | Some(Value::Null) => None,
        Some(w) => {
            let times = crate::util::times(problem);
            let ok = w.as_array().is_some_and(|a| {
                a.len() == 2
                    && a.iter().all(Value::is_number)
                    && a.iter().all(|v| times.iter().any(|t| Some(*t) == v.as_f64()))
                    && a[0].as_f64() < a[1].as_f64()
            });
            if !ok {
                return contract("compliance_window_s must be two increasing entries of times_s");
            }
            Some(json!([w[0].as_f64(), w[1].as_f64()]))
        }
    };
    let mut result = json!({"method": settings["method"], "mass_interpolation": mass, "rayleigh_damping": damping,
        "velocity_scale_m_s": positive(&settings["velocity_scale_m_s"], "velocity_scale_m_s")?,
        "acceleration_scale_m_s2": positive(&settings["acceleration_scale_m_s2"], "acceleration_scale_m_s2")?});
    if let Some(w) = window {
        result["compliance_window_s"] = w;
    }
    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dynamics {
    pub threshold: Option<f64>,
    pub alpha: f64,
    pub beta: f64,
    pub vs: f64,
    pub acs: f64,
}

impl Dynamics {
    #[must_use]
    pub fn from_json(d: &Value) -> Self {
        Self {
            threshold: d["mass_interpolation"].get("threshold").and_then(num),
            alpha: d["rayleigh_damping"]["mass_s_inv"].as_f64().unwrap_or(0.0),
            beta: d["rayleigh_damping"]["stiffness_s"].as_f64().unwrap_or(0.0),
            vs: d["velocity_scale_m_s"].as_f64().unwrap_or(1.0),
            acs: d["acceleration_scale_m_s2"].as_f64().unwrap_or(1.0),
        }
    }

    pub fn mass_factor<S: Scalar>(&self, density: S) -> S {
        match self.threshold {
            None => density,
            Some(t) => {
                if density.value() >= t {
                    density
                } else {
                    density.powi(6) * (6.0 / t.powi(5)) - density.powi(7) * (5.0 / t.powi(6))
                }
            }
        }
    }
}

pub fn consistent_mass_action<S: Scalar>(values: &[[S; 3]; 4], factor: S) -> [[S; 3]; 4] {
    let total: [S; 3] = std::array::from_fn(|a| values[0][a] + values[1][a] + values[2][a] + values[3][a]);
    std::array::from_fn(|i| std::array::from_fn(|a| factor * (total[a] + values[i][a])))
}

#[must_use]
pub fn kinematic_row_scales(dt: f64, us: f64, vs: f64, acs: f64) -> (f64, f64) {
    (vs.max(0.5 * dt * acs), us.max(dt * vs).max(0.25 * dt * dt * acs))
}


#[derive(Debug, Clone)]
pub struct InertiaElement {
    pub model: std::sync::Arc<SolidModel>,
    pub dynamics: Dynamics,
    pub grad0: std::sync::Arc<Vec<[[f64; 3]; 4]>>,
}

impl InertiaElement {
    pub fn force<S: Scalar>(&self, grad0: &[[f64; 3]; 4], current: &[S], design: &[S]) -> [[S; 3]; 4] {
        let m = &self.model;
        let d = &self.dynamics;
        let density = design[0];
        let h = [design[1] * 1e-3, design[2] * 1e-3, design[3] * 1e-3];
        let c = design[4];
        let grad: [[S; 3]; 4] =
            std::array::from_fn(|i| std::array::from_fn(|a| S::from_f64(grad0[i][a]) / h[a]));
        let volume = h[0] * h[1] * h[2] / 6.0;
        let v: [[S; 3]; 4] = std::array::from_fn(|i| std::array::from_fn(|a| current[16 + 3 * i + a] * d.vs));
        let acc: [[S; 3]; 4] =
            std::array::from_fn(|i| std::array::from_fn(|a| current[28 + 3 * i + a] * d.acs));
        let [ma, mb] = &m.materials;
        let rho_material = (-c + 1.0) * ma.density() + c * mb.density();
        let factor = d.mass_factor(density) * rho_material * volume / 20.0;
        let combined: [[S; 3]; 4] =
            std::array::from_fn(|i| std::array::from_fn(|a| acc[i][a] + v[i][a] * d.alpha));
        let mut force = consistent_mass_action(&combined, factor);
        if d.beta > 0.0 {
            let te = (0..4).fold(S::zero(), |s, i| s + (current[i] * m.ts + m.t0)) / 4.0;
            let prop = m.properties(te, c, None);
            let rate = mandel::strain(&v, &grad);
            let stiffness = m.stiffness(density);
            let (e, nu) = (prop.get(idx::E), prop.get(idx::NU));
            let g = e / ((nu + 1.0) * 2.0);
            let k = e / ((-(nu * 2.0) + 1.0) * 3.0);
            let dv = mandel::dev(&rate);
            let tr = mandel::trace(&rate);
            let s: Mandel<S> =
                std::array::from_fn(|i| stiffness * d.beta * (g * 2.0 * dv[i] + k * tr * IDENTITY[i]));
            let damping = mandel::nodal_forces(&s, &grad, volume);
            for i in 0..4 {
                for a in 0..3 {
                    force[i][a] += damping[i][a];
                }
            }
        }
        force
    }
}

impl LocalResidual for InertiaElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let force = self.force(&self.grad0[item], current, design);
        let scale = self.model.ss * self.model.ls * self.model.ls;
        for i in 0..4 {
            for a in 0..3 {
                out[3 * i + a] = force[i][a] / scale;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KinematicElement {
    pub us: f64,
    pub dynamics: Dynamics,
}

impl LocalResidual for KinematicElement {
    fn residual<S: Scalar>(&self, _item: usize, current: &[S], previous: &[S], _design: &[S], out: &mut [S]) {
        let (vs, acs) = (self.dynamics.vs, self.dynamics.acs);
        let dt = current[3].value();
        let (u, v, a) = (current[0] * self.us, current[1] * vs, current[2] * acs);
        let (u0, v0, a0) = (previous[0] * self.us, previous[1] * vs, previous[2] * acs);
        let (velocity_scale, displacement_scale) = kinematic_row_scales(dt, self.us, vs, acs);
        out[0] = (v - v0 - (a + a0) * (0.5 * dt)) / velocity_scale;
        out[1] = (u - u0 - v0 * dt - (a + a0) * (0.25 * dt * dt)) / displacement_scale;
    }
}

pub fn kinetic_energy<S: Scalar>(
    model: &SolidModel,
    dynamics: &Dynamics,
    velocity: &[[S; 3]; 4],
    density: S,
    c: S,
    h: [S; 3],
) -> S {
    let volume = h[0] * h[1] * h[2] / 6.0;
    let [ma, mb] = &model.materials;
    let rho_material = (-c + 1.0) * ma.density() + c * mb.density();
    let factor = dynamics.mass_factor(density) * rho_material * volume / 20.0;
    let mv = consistent_mass_action(velocity, factor);
    let mut acc = S::zero();
    for i in 0..4 {
        for a in 0..3 {
            acc += velocity[i][a] * mv[i][a];
        }
    }
    acc * 0.5
}

#[must_use]
pub fn editor_schema() -> Value {
    json!({"title": "Structural dynamics (optional)",
        "description": "Adds inertia: Newmark average acceleration with consistent mass. Omit for quasistatic equilibrium. Supports must be fixed; resolve the relevant periods with the history times.",
        "type": "object",
        "properties": {
            "method": {"title": "Time integration", "enum": METHODS},
            "mass_interpolation": {"title": "Occupancy-to-mass interpolation", "type": "object",
                "properties": {"kind": {"enum": MASS_INTERPOLATIONS},
                    "threshold": {"type": "number", "exclusiveMinimum": 0, "maximum": 0.5}}},
            "rayleigh_damping": {"title": "Rayleigh damping C = a M + b K", "type": "object",
                "properties": {"mass_s_inv": {"type": "number", "minimum": 0, "unit": "1/s"},
                    "stiffness_s": {"type": "number", "minimum": 0, "unit": "s"}}},
            "velocity_scale_m_s": {"title": "Velocity scale", "type": "number", "exclusiveMinimum": 0, "unit": "m/s"},
            "acceleration_scale_m_s2": {"title": "Acceleration scale", "type": "number", "exclusiveMinimum": 0, "unit": "m/s²"},
            "compliance_window_s": {"title": "Time-mean compliance window (optional)", "type": "array",
                "minItems": 2, "maxItems": 2, "items": {"type": "number", "unit": "s"},
                "description": "Start and end history times over which solid_time_mean_compliance_J is averaged."}},
        "default": {"method": "newmark_average_acceleration",
            "mass_interpolation": {"kind": "polynomial_void_suppression", "threshold": 0.1},
            "rayleigh_damping": {"mass_s_inv": 0.0, "stiffness_s": 0.0},
            "velocity_scale_m_s": 1e-2, "acceleration_scale_m_s2": 1e2}})
}
