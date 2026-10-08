// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::dense::{DenseLu, DenseMatrix};

use super::kinematics::{Mat3, TetMesh, det, green_maxwell_step, green_strain};
use super::poro::{PoroParams, PoroProblem, PoroStep, element_residual, local_u, nominal_stress};
use crate::util::{bool_array, contract, f64_shaped, int_array, real_array};

pub const R_GAS: f64 = 8.314_462_618_153_24;

pub const MECHANICAL: [&str; 7] = [
    "shear_Pa",
    "lame_Pa",
    "branch_moduli_Pa",
    "relaxation_times_s",
    "biot_coefficient",
    "biot_modulus_Pa",
    "reference_mobility_m2_Pa_s",
];
pub const THERMAL: [&str; 5] = [
    "heat_capacity_J_m3_K",
    "reference_conductivity_W_m_K",
    "relaxation_activation_J_mol",
    "mobility_activation_J_mol",
    "nodal_convection_W_K",
];
pub const AGEING_KEYS: [&str; 4] =
    ["rate_ref_s_inv", "activation_J_mol", "residual_stiffness", "chemical_energy_J_m3"];
pub const DAMAGE_KEYS: [&str; 4] = ["rate_s_inv", "threshold_J_m3", "residual_stiffness", "heat_fraction"];
pub const RESPONSES: [&str; 5] = [
    "final_mean_temperature_K",
    "generated_heat_J",
    "final_displacement_squared_m2",
    "final_mean_ageing_extent",
    "final_mean_damage",
];

const THERMAL_REQUIRED: [&str; 13] = [
    "heat_capacity_J_m3_K",
    "reference_conductivity_W_m_K",
    "initial_temperature_K",
    "fixed_temperature_nodes",
    "temperature_history_K",
    "heat_source_history_W",
    "nodal_convection_W_K",
    "ambient_temperature_history_K",
    "reference_temperature_K",
    "relaxation_activation_J_mol",
    "mobility_activation_J_mol",
    "temperature_min_K",
    "temperature_max_K",
];

#[derive(Debug, Clone)]
pub struct Ageing {
    pub initial_extent: Vec<f64>,
    pub rate: Vec<f64>,
    pub activation: Vec<f64>,
    pub residual: Vec<f64>,
    pub chemical: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct DamageValues {
    pub initial_damage: Vec<f64>,
    pub rate: Vec<f64>,
    pub threshold: Vec<f64>,
    pub residual: Vec<f64>,
    pub heat_fraction: Vec<f64>,
}

fn elementwise(v: &Value, ne: usize) -> Option<Vec<f64>> {
    f64_shaped(v, &[ne]).filter(|a| a.iter().all(Scalar::is_finite))
}


pub fn damage_operators(
    settings: &Value,
    mesh_points: &[[f64; 3]],
    elements: &[[usize; 4]],
) -> Result<(DamageValues, Vec<Vec<f64>>), CaeError> {
    const KEYS: [&str; 7] = [
        "initial_damage",
        "rate_s_inv",
        "threshold_J_m3",
        "residual_stiffness",
        "length_scale_m",
        "material_region",
        "provenance",
    ];
    let Some(map) = settings.as_object() else {
        return contract("Complete calibrated damage settings and provenance required");
    };
    let complete = KEYS.iter().all(|k| map.contains_key(*k))
        && map.keys().all(|k| KEYS.contains(&k.as_str()) || k == "heat_fraction")
        && map["provenance"].as_str().is_some_and(|s| !s.trim().is_empty());
    if !complete {
        return contract("Complete calibrated damage settings and provenance required");
    }
    let ne = elements.len();
    let arrays = ["initial_damage", "rate_s_inv", "threshold_J_m3", "residual_stiffness"]
        .map(|k| elementwise(&map[k], ne));
    let heat_fraction = match map.get("heat_fraction") {
        None => Some(vec![1.0; ne]),
        Some(v) => elementwise(v, ne),
    };
    let ([Some(d), Some(rate), Some(threshold), Some(residual)], Some(heat_fraction)) =
        (arrays, heat_fraction)
    else {
        return contract("Finite elementwise damage arrays required");
    };
    let length = match &map["length_scale_m"] {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        _ => f64::NAN,
    };
    let regions = int_array(&map["material_region"]).filter(|(s, _)| s == &[ne]).map(|x| x.1);
    let Some(regions) = regions.filter(|_| length.is_finite() && length > 0.0) else {
        return contract("Positive physical damage length and integer material-region labels required");
    };
    if d.iter().any(|v| !(0.0..=1.0).contains(v))
        || rate.iter().any(|v| *v < 0.0)
        || threshold.iter().any(|v| *v <= 0.0)
        || residual.iter().any(|v| *v <= 0.0 || *v > 1.0)
    {
        return contract("Invalid damage coefficient bounds");
    }
    if heat_fraction.iter().any(|v| !(0.0..=1.0).contains(v)) {
        return contract("Damage heat fraction must be in [0,1]");
    }
    let mut centers = Vec::with_capacity(ne);
    let mut volume = Vec::with_capacity(ne);
    for t in elements {
        let v: [[f64; 3]; 4] = t.map(|i| mesh_points[i]);
        centers.push(std::array::from_fn::<f64, 3, _>(|c| (v[0][c] + v[1][c] + v[2][c] + v[3][c]) / 4.0));
        let m: Mat3<f64> = std::array::from_fn(|r| std::array::from_fn(|c| v[c + 1][r] - v[0][r]));
        volume.push(det(&m) / 6.0);
    }
    if volume.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return contract("Positive oriented reference elements required for damage");
    }
    let weights: Vec<Vec<f64>> = (0..ne)
        .map(|i| {
            let row: Vec<f64> = (0..ne)
                .map(|j| {
                    let dist = (0..3).map(|c| (centers[i][c] - centers[j][c]).powi(2)).sum::<f64>().sqrt();
                    let same = if regions[i] == regions[j] { 1.0 } else { 0.0 };
                    (-(dist / length).powi(2)).exp() * volume[j] * same
                })
                .collect();
            let total: f64 = row.iter().sum();
            row.iter().map(|w| w / total).collect()
        })
        .collect();
    Ok((DamageValues { initial_damage: d, rate, threshold, residual, heat_fraction }, weights))
}


pub fn damage_update(
    values: &DamageValues,
    weights: &[Vec<f64>],
    previous: &[f64],
    energy: &[f64],
    dt: f64,
) -> Result<(Vec<f64>, Vec<f64>, Vec<f64>), CaeError> {
    let ne = values.initial_damage.len();
    if previous.len() != ne
        || energy.len() != ne
        || previous.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0)
        || energy.iter().any(|v| !v.is_finite() || *v < -1e-10)
        || !dt.is_finite()
        || dt <= 0.0
    {
        return contract("Damage update requires bounded state, nonnegative energy and positive time");
    }
    let positive: Vec<f64> = energy.iter().map(|e| e.max(0.0)).collect();
    let mut updated = Vec::with_capacity(ne);
    let mut factor = Vec::with_capacity(ne);
    let mut released = Vec::with_capacity(ne);
    for i in 0..ne {
        let drive: f64 = weights[i].iter().zip(&positive).map(|(w, e)| w * e).sum();
        let rate = values.rate[i] * (drive / values.threshold[i] - 1.0).max(0.0);
        let u = previous[i] + (1.0 - previous[i]) * (-(-dt * rate).exp_m1());
        factor.push(1.0 - (1.0 - values.residual[i]) * u);
        released.push((u - previous[i]) * (1.0 - values.residual[i]) * positive[i]);
        updated.push(u);
    }
    Ok((updated, factor, released))
}

#[derive(Debug, Clone)]
pub struct ThermoProblem {
    pub poro: PoroProblem,
    pub capacity: Vec<f64>,
    pub conductivity: Vec<f64>,
    pub t0: Vec<f64>,
    pub fixed_t: Vec<bool>,
    pub temperature_history: Vec<Vec<f64>>,
    pub heat_source: Vec<Vec<f64>>,
    pub convection: Vec<f64>,
    pub ambient: Vec<Vec<f64>>,
    pub reference: Vec<f64>,
    pub relaxation_activation: Vec<Vec<f64>>,
    pub mobility_activation: Vec<f64>,
    pub t_min: f64,
    pub t_max: f64,
    pub ageing: Option<Ageing>,
    pub damage: Option<(DamageValues, Vec<Vec<f64>>)>,
}

fn rows(v: &[f64], width: usize) -> Vec<Vec<f64>> {
    v.chunks(width.max(1)).map(<[f64]>::to_vec).collect()
}

impl ThermoProblem {

    #[allow(clippy::too_many_lines)]
    pub fn parse(problem: &Value) -> Result<Self, CaeError> {
        let Some(map) = problem
            .as_object()
            .filter(|m| m.len() == 2 && m.contains_key("poromechanics") && m.contains_key("thermal"))
        else {
            return contract("Poromechanics and thermal settings required");
        };
        let t = &map["thermal"];
        let complete = t.as_object().is_some_and(|m| {
            THERMAL_REQUIRED.iter().all(|k| m.contains_key(*k))
                && m.keys().all(|k| THERMAL_REQUIRED.contains(&k.as_str()) || k == "ageing" || k == "damage")
        });
        if !complete {
            return contract("Complete thermal/rate-coupling settings required");
        }
        let poro = PoroProblem::parse(&map["poromechanics"])?;
        let n = poro.mesh.node_count();
        let ne = poro.mesh.elements.len();
        let nt = poro.steps.len();
        let nb = poro.moduli.first().map_or(0, Vec::len);
        let bound = |k: &str| t[k].as_f64().unwrap_or(f64::NAN);
        let (low, high) = (bound("temperature_min_K"), bound("temperature_max_K"));
        if !low.is_finite() || !high.is_finite() || !(0.0 < low && low < high) {
            return contract("Explicit positive calibrated temperature interval required");
        }
        let times_shape =
            real_array(&map["poromechanics"]["relaxation_times_s"]).map(|x| x.0).unwrap_or_default();
        let shaped = |k: &str, shape: &[usize]| f64_shaped(&t[k], shape);
        let fixed = bool_array(&t["fixed_temperature_nodes"]).filter(|(s, _)| s == &[n]).map(|x| x.1);
        let parts = (
            shaped("heat_capacity_J_m3_K", &[ne]),
            shaped("reference_conductivity_W_m_K", &[ne]),
            shaped("initial_temperature_K", &[n]),
            fixed,
            shaped("temperature_history_K", &[nt, n]),
            shaped("heat_source_history_W", &[nt, n]),
            shaped("nodal_convection_W_K", &[n]),
            shaped("ambient_temperature_history_K", &[nt, n]),
            shaped("reference_temperature_K", &[ne]),
            shaped("relaxation_activation_J_mol", &times_shape),
            shaped("mobility_activation_J_mol", &[ne]),
        );
        let (
            Some(cvol),
            Some(conduct),
            Some(t0),
            Some(fixed),
            Some(th),
            Some(qh),
            Some(h),
            Some(ambient),
            Some(reference),
            Some(ea),
            Some(em),
        ) = parts
        else {
            return contract("Invalid thermal material/history shapes");
        };
        let all = [&cvol, &conduct, &t0, &th, &qh, &h, &ambient, &reference, &ea, &em];
        if all.iter().any(|a| a.iter().any(|v| !v.is_finite()))
            || cvol.iter().any(|v| *v <= 0.0)
            || conduct.iter().any(|v| *v < 0.0)
            || h.iter().any(|v| *v < 0.0)
            || ea.iter().any(|v| *v < 0.0)
            || em.iter().any(|v| *v < 0.0)
        {
            return contract(
                "Finite thermal data, positive capacity and nonnegative conductivity, convection and activation energies required",
            );
        }
        if [&t0, &th, &ambient, &reference].iter().any(|a| a.iter().any(|v| *v < low || *v > high)) {
            return contract("Authored temperature outside calibrated interval");
        }
        let ageing = match t.get("ageing") {
            None => None,
            Some(a) => {
                const KEYS: [&str; 6] = [
                    "initial_extent",
                    "rate_ref_s_inv",
                    "activation_J_mol",
                    "residual_stiffness",
                    "chemical_energy_J_m3",
                    "provenance",
                ];
                let ok = a
                    .as_object()
                    .is_some_and(|m| m.len() == KEYS.len() && KEYS.iter().all(|k| m.contains_key(*k)))
                    && a["provenance"].as_str().is_some_and(|s| !s.trim().is_empty());
                if !ok {
                    return contract("Complete calibrated ageing law and provenance required");
                }
                let values = [KEYS[0], KEYS[1], KEYS[2], KEYS[3], KEYS[4]].map(|k| elementwise(&a[k], ne));
                let [Some(initial_extent), Some(rate), Some(activation), Some(residual), Some(chemical)] =
                    values
                else {
                    return contract("Elementwise finite ageing coefficients required");
                };
                if initial_extent.iter().any(|v| !(0.0..=1.0).contains(v))
                    || rate.iter().any(|v| *v < 0.0)
                    || activation.iter().any(|v| *v < 0.0)
                    || residual.iter().any(|v| *v <= 0.0 || *v > 1.0)
                    || chemical.iter().any(|v| *v < 0.0)
                {
                    return contract("Invalid irreversible ageing bounds");
                }
                Some(Ageing { initial_extent, rate, activation, residual, chemical })
            }
        };
        let damage = match t.get("damage") {
            None => None,
            Some(d) => Some(damage_operators(d, &poro.mesh.points, &poro.mesh.elements)?),
        };
        Ok(Self {
            poro,
            capacity: cvol,
            conductivity: conduct,
            t0,
            fixed_t: fixed,
            temperature_history: rows(&th, n),
            heat_source: rows(&qh, n),
            convection: h,
            ambient: rows(&ambient, n),
            reference,
            relaxation_activation: rows(&ea, nb),
            mobility_activation: em,
            t_min: low,
            t_max: high,
            ageing,
            damage,
        })
    }

    fn heat_operators(&self) -> (DenseMatrix, DenseMatrix) {
        let mesh = &self.poro.mesh;
        let n = mesh.node_count();
        let mut c = DenseMatrix::zeros(n, n);
        let mut l = DenseMatrix::zeros(n, n);
        for (e, t) in mesh.elements.iter().enumerate() {
            let (s, k) = mesh.hydraulic_local(e, 1.0 / self.capacity[e], self.conductivity[e]);
            for a in 0..4 {
                for b in 0..4 {
                    c.data[t[a] * n + t[b]] += s[a][b];
                    l.data[t[a] * n + t[b]] += k[a][b];
                }
            }
        }
        (c, l)
    }
}

fn mechanical_energy<S: Scalar>(f: &Mat3<S>, mu: S, lam: S, g: &[S], branch: &[Mat3<S>]) -> S {
    let j = det(f);
    let mut sq = S::zero();
    for r in f {
        for v in r {
            sq += *v * *v;
        }
    }
    let lj = j.ln();
    let mut out = mu * (sq - 3.0) * 0.5 - mu * lj + lam * lj * lj * 0.5;
    let e = green_strain(f);
    for (b, m) in branch.iter().enumerate() {
        let mut acc = S::zero();
        for r in 0..3 {
            for c in 0..3 {
                let d = e[r][c] - m[r][c];
                acc += d * d;
            }
        }
        out += g[b] * acc * 0.5;
    }
    out
}

#[derive(Debug, Clone)]
pub struct ThermoStep {
    pub poro: PoroStep,
    pub temperature: Vec<f64>,
    pub generated_heat: f64,
    pub thermal_energy_change: f64,
    pub thermostat_exchange: Vec<f64>,
    pub convection_heat_out: f64,
    pub heat_balance_error: f64,
    pub free_heat_balance_error: f64,
    pub coupling_temperature_error: f64,
    pub coupling_iterations: usize,
    pub actual_relaxation_times: Vec<Vec<f64>>,
    pub actual_reference_mobility: Vec<f64>,
    pub ageing_extent: Vec<f64>,
    pub ageing_heat: f64,
    pub damage: Vec<f64>,
    pub damage_heat: f64,
    pub damage_released_energy: f64,
    pub damage_heat_power: f64,
    pub nonthermal_damage_energy: f64,
    pub damage_coupling_error: f64,
    pub remaining_chemical_energy: f64,
}

impl ThermoStep {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut v = super::poro::step_json(&self.poro);
        if let Some(m) = v.as_object_mut() {
            for (k, x) in [
                ("generated_heat_J", self.generated_heat),
                ("thermal_energy_change_J", self.thermal_energy_change),
                ("convection_heat_out_J", self.convection_heat_out),
                ("heat_balance_error_J", self.heat_balance_error),
                ("free_heat_balance_error_J", self.free_heat_balance_error),
                ("coupling_temperature_error_K", self.coupling_temperature_error),
                ("ageing_heat_J", self.ageing_heat),
                ("damage_heat_J", self.damage_heat),
                ("damage_released_energy_J", self.damage_released_energy),
                ("damage_heat_power_W", self.damage_heat_power),
                ("nonthermal_damage_energy_increment_cumulative_J", self.nonthermal_damage_energy),
                ("damage_coupling_error", self.damage_coupling_error),
                ("remaining_chemical_energy_J", self.remaining_chemical_energy),
            ] {
                m.insert(k.to_string(), json!(x));
            }
            m.insert("coupling_iterations".into(), json!(self.coupling_iterations));
            m.insert("temperature_K".into(), json!(self.temperature));
            m.insert("ageing_extent".into(), json!(self.ageing_extent));
            m.insert("damage".into(), json!(self.damage));
        }
        v
    }
}

#[derive(Debug, Clone)]
pub struct ThermoHistory {
    pub steps: Vec<ThermoStep>,
    pub times: Vec<f64>,
}

pub const THERMAL_SCOPE: &str = "Dissipation-to-heat, Arrhenius kinetics, optional ageing and nonlocal energy-driven scalar damage; no reversible thermoelastic expansion or mixing chemistry";
pub const DAMAGE_SCOPE: &str = "Calibrated isotropic rate law and optional heat fraction (default 1); nonthermal released energy tracked since initial state, not a calibrated crack-surface energy or recoverable-energy model; endpoint quadrature, no fatigue-life or mesh-objectivity qualification";

fn max_abs(v: impl Iterator<Item = f64>) -> f64 {
    v.fold(0.0, |m, x| if x.abs() > m || x.is_nan() { x.abs() } else { m })
}

impl ThermoProblem {

    #[allow(clippy::too_many_lines)]
    pub fn solve_history(&self) -> Result<ThermoHistory, CaeError> {
        let p = &self.poro;
        let mesh = &p.mesh;
        let (n, ne) = (mesh.node_count(), mesh.elements.len());
        let (c, l) = self.heat_operators();
        let free: Vec<usize> = (0..n).filter(|i| !self.fixed_t[*i]).collect();
        let fixed_idx: Vec<usize> = (0..n).filter(|i| self.fixed_t[*i]).collect();
        let volumes = &mesh.volumes;
        let mut temperature = self.t0.clone();
        let mut extent = self.ageing.as_ref().map_or_else(|| vec![0.0; ne], |a| a.initial_extent.clone());
        let mut damage_state =
            self.damage.as_ref().map_or_else(|| vec![0.0; ne], |d| d.0.initial_damage.clone());
        let fgrad = |u: &[f64]| -> Vec<Mat3<f64>> {
            (0..ne).map(|e| mesh.deformation_gradient(e, &local_u(u, &mesh.elements[e]))).collect()
        };
        let initial_f = fgrad(&p.u0);
        let mut old_f = initial_f.clone();
        let mut factor0 = self.ageing.as_ref().map_or_else(
            || vec![1.0; ne],
            |a| (0..ne).map(|e| 1.0 - (1.0 - a.residual[e]) * extent[e]).collect(),
        );
        if let Some((d, _)) = &self.damage {
            for e in 0..ne {
                factor0[e] *= 1.0 - (1.0 - d.residual[e]) * damage_state[e];
            }
        }
        let mut old_p: Vec<Mat3<f64>> = (0..ne)
            .map(|e| {
                let t = &mesh.elements[e];
                let pbar = (p.p0[t[0]] + p.p0[t[1]] + p.p0[t[2]] + p.p0[t[3]]) / 4.0;
                let params = PoroParams {
                    mu: p.mu[e] * factor0[e],
                    lam: p.lam[e] * factor0[e],
                    g: p.moduli[e].iter().map(|g| g * factor0[e]).collect(),
                    tau: p.times[e].clone(),
                    alpha: p.alpha[e],
                    biot: p.biot[e],
                    mobility: p.mobility[e],
                };
                nominal_stress(&old_f[e], pbar, &params, &p.memory[e])
            })
            .collect();
        let mut work = 0.0;
        let mut nonthermal = 0.0;
        let mut current = p.clone();
        let mut records = Vec::with_capacity(p.steps.len());
        for (i, dt) in p.steps.iter().copied().enumerate() {
            let old_t = temperature.clone();
            let mut guess = temperature.clone();
            for k in &fixed_idx {
                guess[*k] = self.temperature_history[i][*k];
            }
            let mut damage_guess = damage_state.clone();
            let mut previous_damage_error = f64::INFINITY;
            let mut damage_relaxation = 1.0;
            let mut step = current.clone();
            step.steps = vec![dt];
            step.prescribed = vec![p.prescribed[i].clone()];
            step.force = vec![p.force[i].clone()];
            step.pressure_history = vec![p.pressure_history[i].clone()];
            step.source = vec![p.source[i].clone()];
            let mut converged = None;
            for iteration in 0..40 {
                let mean: Vec<f64> = mesh
                    .elements
                    .iter()
                    .map(|t| (guess[t[0]] + guess[t[1]] + guess[t[2]] + guess[t[3]]) / 4.0)
                    .collect();
                let shift: Vec<Vec<f64>> = (0..ne)
                    .map(|e| {
                        self.relaxation_activation[e]
                            .iter()
                            .map(|a| a / R_GAS * (1.0 / mean[e] - 1.0 / self.reference[e]))
                            .collect()
                    })
                    .collect();
                let flow_shift: Vec<f64> = (0..ne)
                    .map(|e| -self.mobility_activation[e] / R_GAS * (1.0 / mean[e] - 1.0 / self.reference[e]))
                    .collect();
                if max_abs(shift.iter().flatten().copied()).max(max_abs(flow_shift.iter().copied())) > 600.0 {
                    return contract("Temperature shift exceeds finite numerical range");
                }
                step.times = (0..ne)
                    .map(|e| p.times[e].iter().zip(&shift[e]).map(|(t, s)| t * s.exp()).collect())
                    .collect();
                step.mobility = (0..ne).map(|e| p.mobility[e] * flow_shift[e].exp()).collect();
                let mut new_extent = extent.clone();
                let mut factor = vec![1.0; ne];
                if let Some(a) = &self.ageing {
                    let ageing_shift: Vec<f64> = (0..ne)
                        .map(|e| -a.activation[e] / R_GAS * (1.0 / mean[e] - 1.0 / self.reference[e]))
                        .collect();
                    if max_abs(ageing_shift.iter().copied()) > 600.0 {
                        return contract("Ageing temperature shift exceeds finite range");
                    }
                    let rate: Vec<f64> = (0..ne).map(|e| a.rate[e] * ageing_shift[e].exp()).collect();
                    if rate.iter().any(|r| !r.is_finite()) {
                        return contract("Nonfinite ageing rate");
                    }
                    for e in 0..ne {
                        new_extent[e] = extent[e] + (1.0 - extent[e]) * (-(-dt * rate[e]).exp_m1());
                        factor[e] = 1.0 - (1.0 - a.residual[e]) * new_extent[e];
                    }
                }
                let damage_factor: Vec<f64> = match &self.damage {
                    None => vec![1.0; ne],
                    Some((d, _)) => (0..ne).map(|e| 1.0 - (1.0 - d.residual[e]) * damage_guess[e]).collect(),
                };
                let combined: Vec<f64> = factor.iter().zip(&damage_factor).map(|(a, b)| a * b).collect();
                step.mu = (0..ne).map(|e| p.mu[e] * combined[e]).collect();
                step.lam = (0..ne).map(|e| p.lam[e] * combined[e]).collect();
                step.moduli =
                    (0..ne).map(|e| p.moduli[e].iter().map(|g| g * combined[e]).collect()).collect();
                let result = step.solve_history()?.steps.remove(0);
                let mut ageing_heat = vec![0.0; ne];
                let mut damage_heat = vec![0.0; ne];
                let mut damage_release = vec![0.0; ne];
                let mut new_damage = damage_state.clone();
                if self.ageing.is_some() || self.damage.is_some() {
                    let f = fgrad(&result.displacement);
                    let mechanical: Vec<f64> = (0..ne)
                        .map(|e| {
                            mechanical_energy(
                                &f[e],
                                p.mu[e],
                                p.lam[e],
                                &p.moduli[e],
                                &result.branch_strain[e],
                            )
                        })
                        .collect();
                    if let Some(a) = &self.ageing {
                        for e in 0..ne {
                            let old_damage_factor = self
                                .damage
                                .as_ref()
                                .map_or(1.0, |(d, _)| 1.0 - (1.0 - d.residual[e]) * damage_state[e]);
                            ageing_heat[e] = (new_extent[e] - extent[e])
                                * ((1.0 - a.residual[e]) * old_damage_factor * mechanical[e] + a.chemical[e]);
                        }
                    }
                    if let Some((d, w)) = &self.damage {
                        let energy: Vec<f64> = (0..ne).map(|e| factor[e] * mechanical[e]).collect();
                        let (updated, _, released) = damage_update(d, w, &damage_state, &energy, dt)?;
                        new_damage = updated;
                        damage_release = released;
                        damage_heat = (0..ne).map(|e| d.heat_fraction[e] * damage_release[e]).collect();
                    }
                }
                let mut generated = vec![0.0; n];
                for (e, t) in mesh.elements.iter().enumerate() {
                    let share =
                        volumes[e] * (result.dissipation_density[e] + ageing_heat[e] + damage_heat[e]) / 4.0;
                    for a in t {
                        generated[*a] += share;
                    }
                }
                let mut a_mat = DenseMatrix::zeros(n, n);
                for r in 0..n {
                    for k in 0..n {
                        a_mat.data[r * n + k] = c.data[r * n + k]
                            + dt * (l.data[r * n + k] + if r == k { self.convection[r] } else { 0.0 });
                    }
                }
                let shifted: Vec<f64> = old_t.iter().map(|v| v - old_t[0]).collect();
                let l_old: Vec<f64> =
                    (0..n).map(|r| (0..n).map(|k| l.data[r * n + k] * shifted[k]).sum()).collect();
                let rhs: Vec<f64> = (0..n)
                    .map(|r| {
                        generated[r]
                            + dt * (self.heat_source[i][r]
                                + self.convection[r] * (self.ambient[i][r] - old_t[r])
                                - l_old[r])
                    })
                    .collect();
                let mut increment: Vec<f64> =
                    (0..n).map(|r| self.temperature_history[i][r] - old_t[r]).collect();
                if !free.is_empty() {
                    let aff = DenseMatrix {
                        nrows: free.len(),
                        ncols: free.len(),
                        data: free
                            .iter()
                            .flat_map(|r| free.iter().map(|k| a_mat.data[r * n + k]).collect::<Vec<_>>())
                            .collect(),
                    };
                    let b: Vec<f64> = free
                        .iter()
                        .map(|r| {
                            rhs[*r]
                                - fixed_idx.iter().map(|k| a_mat.data[r * n + k] * increment[*k]).sum::<f64>()
                        })
                        .collect();
                    let x = implexity_linalg::dense::solve(&aff, &b, 1)
                        .map_err(|_| CaeError::contract("Singular heat operator"))?;
                    for (r, v) in free.iter().zip(x) {
                        increment[*r] = v;
                    }
                }
                let new_t: Vec<f64> = old_t.iter().zip(&increment).map(|(a, b)| a + b).collect();
                if new_t.iter().any(|v| !v.is_finite() || *v < self.t_min || *v > self.t_max) {
                    return contract("Coupled temperature outside calibrated interval");
                }
                let discrepancy = max_abs(new_t.iter().zip(&guess).map(|(a, b)| a - b));
                let damage_error = max_abs(new_damage.iter().zip(&damage_guess).map(|(a, b)| a - b));
                if discrepancy <= 1e-8 && damage_error <= 1e-10 {
                    temperature = new_t;
                    converged = Some((
                        iteration,
                        result,
                        a_mat,
                        increment,
                        rhs,
                        generated,
                        discrepancy,
                        damage_error,
                        new_extent,
                        ageing_heat,
                        damage_heat,
                        damage_release,
                        new_damage,
                        step.times.clone(),
                        step.mobility.clone(),
                    ));
                    break;
                }
                guess = new_t;
                for k in &fixed_idx {
                    guess[*k] = self.temperature_history[i][*k];
                }
                if damage_error > previous_damage_error {
                    damage_relaxation = 0.5;
                }
                for e in 0..ne {
                    damage_guess[e] += damage_relaxation * (new_damage[e] - damage_guess[e]);
                }
                previous_damage_error = damage_error;
            }
            let Some((
                iteration,
                mut result,
                a_mat,
                increment,
                rhs,
                generated,
                discrepancy,
                damage_error,
                new_extent,
                ageing_heat,
                damage_heat,
                damage_release,
                new_damage,
                times,
                mobility,
            )) = converged
            else {
                return contract("Thermal/poromechanical/damage fixed-point iteration limit");
            };
            let balance: Vec<f64> = (0..n)
                .map(|r| (0..n).map(|k| a_mat.data[r * n + k] * increment[k]).sum::<f64>() - rhs[r])
                .collect();
            let thermostat: Vec<f64> =
                (0..n).map(|r| if self.fixed_t[r] { balance[r] } else { 0.0 }).collect();
            let column: Vec<f64> = (0..n).map(|k| (0..n).map(|r| c.data[r * n + k]).sum()).collect();
            let thermal_change: f64 = column.iter().zip(&increment).map(|(a, b)| a * b).sum();
            let convection = dt
                * (0..n)
                    .map(|r| self.convection[r] * ((old_t[r] - self.ambient[i][r]) + increment[r]))
                    .sum::<f64>();
            let generated_sum: f64 = generated.iter().sum();
            let heat_balance_error = thermal_change + convection
                - dt * self.heat_source[i].iter().sum::<f64>()
                - generated_sum
                - thermostat.iter().sum::<f64>();
            let free_heat_balance_error =
                if free.is_empty() { 0.0 } else { max_abs(free.iter().map(|r| balance[*r])) };
            let f = result.deformation_gradient.clone();
            let piola = result.first_piola.clone();
            for e in 0..ne {
                let mut acc = 0.0;
                for r in 0..3 {
                    for k in 0..3 {
                        acc += (old_p[e][r][k] + piola[e][r][k]) * (f[e][r][k] - old_f[e][r][k]) / 2.0;
                    }
                }
                work += volumes[e] * acc;
            }
            result.mechanical_work = work;
            result.kinematic_cycle_closure = f
                .iter()
                .zip(&initial_f)
                .flat_map(|(a, b)| (0..9).map(move |k| (a[k / 3][k % 3] - b[k / 3][k % 3]).abs()))
                .fold(0.0, f64::max);
            old_f = f;
            old_p = piola;
            let dotv = |a: &[f64]| volumes.iter().zip(a).map(|(v, x)| v * x).sum::<f64>();
            let damage_heat_j = dotv(&damage_heat);
            let released_minus_heat: Vec<f64> =
                damage_release.iter().zip(&damage_heat).map(|(a, b)| a - b).collect();
            nonthermal += dotv(&released_minus_heat);
            let remaining = self.ageing.as_ref().map_or(0.0, |a| {
                dotv(&(0..ne).map(|e| a.chemical[e] * (1.0 - new_extent[e])).collect::<Vec<_>>())
            });
            current.u0.clone_from(&result.displacement);
            current.p0.clone_from(&result.pressure);
            current.memory.clone_from(&result.branch_strain);
            records.push(ThermoStep {
                temperature: temperature.clone(),
                generated_heat: generated_sum,
                thermal_energy_change: thermal_change,
                thermostat_exchange: thermostat,
                convection_heat_out: convection,
                heat_balance_error,
                free_heat_balance_error,
                coupling_temperature_error: discrepancy,
                coupling_iterations: iteration + 1,
                actual_relaxation_times: times,
                actual_reference_mobility: mobility,
                ageing_extent: new_extent.clone(),
                ageing_heat: dotv(&ageing_heat),
                damage: new_damage.clone(),
                damage_heat: damage_heat_j,
                damage_released_energy: dotv(&damage_release),
                damage_heat_power: damage_heat_j / dt,
                nonthermal_damage_energy: nonthermal,
                damage_coupling_error: damage_error,
                remaining_chemical_energy: remaining,
                poro: result,
            });
            extent = new_extent;
            damage_state = new_damage;
        }
        let mut times = Vec::with_capacity(p.steps.len());
        let mut acc = 0.0;
        for s in &p.steps {
            acc += s;
            times.push(acc);
        }
        Ok(ThermoHistory { steps: records, times })
    }
}

#[derive(Clone)]
struct Coeffs<S> {
    mu: Vec<S>,
    lam: Vec<S>,
    g: Vec<Vec<S>>,
    tau0: Vec<Vec<S>>,
    alpha: Vec<S>,
    biot: Vec<S>,
    mob0: Vec<S>,
    cvol: Vec<S>,
    conduct: Vec<S>,
    ea: Vec<Vec<S>>,
    em: Vec<S>,
    h: Vec<S>,
    ageing: Option<[Vec<S>; 4]>,
    damage: Option<[Vec<S>; 4]>,
}

#[derive(Clone)]
struct Memory<S> {
    branch: Vec<Vec<Mat3<S>>>,
    zold: Vec<S>,
    extent: Vec<S>,
    damage: Vec<S>,
    old_t: Vec<S>,
    total_heat: S,
}

struct Fields<S> {
    anew: Vec<S>,
    dnew: Vec<S>,
    branch: Vec<Vec<Mat3<S>>>,
    generated: Vec<S>,
    params: Vec<PoroParams<S>>,
}

fn lift<S: Scalar>(v: &[f64]) -> Vec<S> {
    v.iter().map(|x| S::from_f64(*x)).collect()
}

fn lift2<S: Scalar>(v: &[Vec<f64>]) -> Vec<Vec<S>> {
    v.iter().map(|r| lift(r)).collect()
}

fn lift_mats<S: Scalar>(v: &[Vec<Mat3<f64>>]) -> Vec<Vec<Mat3<S>>> {
    v.iter().map(|r| r.iter().map(|m| m.map(|row| row.map(S::from_f64))).collect()).collect()
}

struct Sensitivity<'a> {
    problem: &'a ThermoProblem,
    material: Vec<&'a str>,
    free: Vec<usize>,
    scale: Vec<f64>,
    pref: f64,
    tref: f64,
    vsum: f64,
}

impl Sensitivity<'_> {
    fn original(&self) -> Result<(Vec<f64>, Vec<usize>), CaeError> {
        let t = self.problem;
        let p = &t.poro;
        let flat = |v: &[Vec<f64>]| v.iter().flatten().copied().collect::<Vec<f64>>();
        let ne = p.mesh.elements.len();
        let nb = p.moduli.first().map_or(0, Vec::len);
        let path = &self.material;
        let key = path[path.len() - 1];
        let value = match (path[0], path.len()) {
            ("poromechanics", _) => match key {
                "shear_Pa" => (p.mu.clone(), vec![ne]),
                "lame_Pa" => (p.lam.clone(), vec![ne]),
                "branch_moduli_Pa" => (flat(&p.moduli), vec![ne, nb]),
                "relaxation_times_s" => (flat(&p.times), vec![ne, nb]),
                "biot_coefficient" => (p.alpha.clone(), vec![ne]),
                "biot_modulus_Pa" => (p.biot.clone(), vec![ne]),
                _ => (p.mobility.clone(), vec![ne]),
            },
            (_, 2) => match key {
                "heat_capacity_J_m3_K" => (t.capacity.clone(), vec![ne]),
                "reference_conductivity_W_m_K" => (t.conductivity.clone(), vec![ne]),
                "relaxation_activation_J_mol" => (flat(&t.relaxation_activation), vec![ne, nb]),
                "mobility_activation_J_mol" => (t.mobility_activation.clone(), vec![ne]),
                _ => (t.convection.clone(), vec![p.mesh.node_count()]),
            },
            (_, _) if path[1] == "ageing" => {
                let a = t
                    .ageing
                    .as_ref()
                    .ok_or_else(|| CaeError::contract("Unsupported thermal-history derivative"))?;
                let v = match key {
                    "rate_ref_s_inv" => a.rate.clone(),
                    "activation_J_mol" => a.activation.clone(),
                    "residual_stiffness" => a.residual.clone(),
                    _ => a.chemical.clone(),
                };
                (v, vec![ne])
            }
            _ => {
                let (d, _) = t
                    .damage
                    .as_ref()
                    .ok_or_else(|| CaeError::contract("Unsupported thermal-history derivative"))?;
                let v = match key {
                    "rate_s_inv" => d.rate.clone(),
                    "threshold_J_m3" => d.threshold.clone(),
                    "residual_stiffness" => d.residual.clone(),
                    _ => d.heat_fraction.clone(),
                };
                (v, vec![ne])
            }
        };
        Ok(value)
    }

    fn coeffs<S: Scalar>(&self, theta: &[S]) -> Coeffs<S> {
        let t = self.problem;
        let p = &t.poro;
        let mut k = Coeffs {
            mu: lift(&p.mu),
            lam: lift(&p.lam),
            g: lift2(&p.moduli),
            tau0: lift2(&p.times),
            alpha: lift(&p.alpha),
            biot: lift(&p.biot),
            mob0: lift(&p.mobility),
            cvol: lift(&t.capacity),
            conduct: lift(&t.conductivity),
            ea: lift2(&t.relaxation_activation),
            em: lift(&t.mobility_activation),
            h: lift(&t.convection),
            ageing: t
                .ageing
                .as_ref()
                .map(|a| [lift(&a.rate), lift(&a.activation), lift(&a.residual), lift(&a.chemical)]),
            damage: t
                .damage
                .as_ref()
                .map(|(d, _)| [lift(&d.rate), lift(&d.threshold), lift(&d.residual), lift(&d.heat_fraction)]),
        };
        let nb = p.moduli.first().map_or(0, Vec::len);
        let two = |v: &mut Vec<Vec<S>>| {
            for (e, row) in v.iter_mut().enumerate() {
                row.copy_from_slice(&theta[e * nb..(e + 1) * nb]);
            }
        };
        let path = &self.material;
        let key = path[path.len() - 1];
        let theta_v = theta.to_vec();
        match (path[0], path.len()) {
            ("poromechanics", _) => match key {
                "shear_Pa" => k.mu = theta_v,
                "lame_Pa" => k.lam = theta_v,
                "branch_moduli_Pa" => two(&mut k.g),
                "relaxation_times_s" => two(&mut k.tau0),
                "biot_coefficient" => k.alpha = theta_v,
                "biot_modulus_Pa" => k.biot = theta_v,
                _ => k.mob0 = theta_v,
            },
            (_, 2) => match key {
                "heat_capacity_J_m3_K" => k.cvol = theta_v,
                "reference_conductivity_W_m_K" => k.conduct = theta_v,
                "relaxation_activation_J_mol" => two(&mut k.ea),
                "mobility_activation_J_mol" => k.em = theta_v,
                _ => k.h = theta_v,
            },
            (_, _) if path[1] == "ageing" => {
                if let Some(a) = k.ageing.as_mut() {
                    let slot = AGEING_KEYS.iter().position(|x| *x == key).unwrap_or(0);
                    a[slot] = theta_v;
                }
            }
            _ => {
                if let Some(d) = k.damage.as_mut() {
                    let slot = DAMAGE_KEYS.iter().position(|x| *x == key).unwrap_or(0);
                    d[slot] = theta_v;
                }
            }
        }
        k
    }

    fn content<S: Scalar>(&self, k: &Coeffs<S>, u: &[S], pressure: &[S]) -> Vec<S> {
        let mesh = &self.problem.poro.mesh;
        let mut z = vec![S::zero(); mesh.node_count()];
        for (e, t) in mesh.elements.iter().enumerate() {
            let j = det(&mesh.deformation_gradient(e, &local_u(u, t)));
            let (s, _) = mesh.hydraulic_local(e, k.biot[e], S::zero());
            let v = mesh.volumes[e];
            for a in 0..4 {
                let mut acc = S::zero();
                for b in 0..4 {
                    acc += s[a][b] * pressure[t[b]];
                }
                z[t[a]] += acc;
            }
            for a in 0..4 {
                z[t[a]] += k.alpha[e] * (j - 1.0) * v / 4.0;
            }
        }
        z
    }

    fn initial<S: Scalar>(&self, k: &Coeffs<S>) -> Memory<S> {
        let t = self.problem;
        let p = &t.poro;
        let ne = p.mesh.elements.len();
        Memory {
            branch: lift_mats(&p.memory),
            zold: self.content(k, &lift(&p.u0), &lift(&p.p0)),
            extent: t.ageing.as_ref().map_or_else(|| vec![S::zero(); ne], |a| lift(&a.initial_extent)),
            damage: t.damage.as_ref().map_or_else(|| vec![S::zero(); ne], |(d, _)| lift(&d.initial_damage)),
            old_t: lift(&t.t0),
            total_heat: S::zero(),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn fields<S: Scalar>(&self, k: &Coeffs<S>, m: &Memory<S>, dt: f64, y: &[S]) -> Fields<S> {
        let t = self.problem;
        let mesh = &t.poro.mesh;
        let (n, ne) = (mesh.node_count(), mesh.elements.len());
        let u = &y[..3 * n];
        let pressure = &y[3 * n..4 * n];
        let temp = &y[4 * n..];
        let mut anew = m.extent.clone();
        let mut factor = vec![S::one(); ne];
        let mut params = Vec::with_capacity(ne);
        let mut base = vec![S::zero(); ne];
        let mut f_all = Vec::with_capacity(ne);
        let mut tau_all = Vec::with_capacity(ne);
        let mut mob_all = Vec::with_capacity(ne);
        for (e, tet) in mesh.elements.iter().enumerate() {
            let mean = (temp[tet[0]] + temp[tet[1]] + temp[tet[2]] + temp[tet[3]]) / 4.0;
            let inv = mean.recip() - 1.0 / t.reference[e];
            let tau: Vec<S> =
                k.tau0[e].iter().zip(&k.ea[e]).map(|(t0, a)| *t0 * (*a / R_GAS * inv).exp()).collect();
            let mob = k.mob0[e] * (-(k.em[e] / R_GAS) * inv).exp();
            if let Some([rate, activation, residual, _]) = &k.ageing {
                let r = rate[e] * (-(activation[e] / R_GAS) * inv).exp();
                anew[e] = m.extent[e] + (-m.extent[e] + 1.0) * (-(r * (-dt)).exp_m1());
                factor[e] = -((-residual[e] + 1.0) * anew[e]) + 1.0;
            }
            let f = mesh.deformation_gradient(e, &local_u(u, tet));
            if k.ageing.is_some() || k.damage.is_some() {
                let bare = green_maxwell_step(&f, &m.branch[e], &k.g[e], &tau, S::from_f64(dt));
                base[e] = mechanical_energy(&f, k.mu[e], k.lam[e], &k.g[e], &bare.branch_strain);
            }
            f_all.push(f);
            tau_all.push(tau);
            mob_all.push(mob);
        }
        let mut age_heat = vec![S::zero(); ne];
        let mut damage_heat = vec![S::zero(); ne];
        let mut dnew = m.damage.clone();
        let mut dfactor = vec![S::one(); ne];
        if k.ageing.is_some() || k.damage.is_some() {
            let old_dfactor: Vec<S> = match &k.damage {
                None => vec![S::one(); ne],
                Some([_, _, residual, _]) => {
                    (0..ne).map(|e| -((-residual[e] + 1.0) * m.damage[e]) + 1.0).collect()
                }
            };
            if let Some([_, _, residual, chemical]) = &k.ageing {
                for e in 0..ne {
                    age_heat[e] = (anew[e] - m.extent[e])
                        * ((-residual[e] + 1.0) * old_dfactor[e] * base[e] + chemical[e]);
                }
            }
            if let (Some([rate, threshold, residual, fraction]), Some((_, weights))) = (&k.damage, &t.damage)
            {
                let energy: Vec<S> = (0..ne).map(|e| (factor[e] * base[e]).max_f64(0.0)).collect();
                for e in 0..ne {
                    let mut drive = S::zero();
                    for (w, x) in weights[e].iter().zip(&energy) {
                        drive += *x * *w;
                    }
                    let r = rate[e] * (drive / threshold[e] - 1.0).max_f64(0.0);
                    dnew[e] = m.damage[e] + (-m.damage[e] + 1.0) * (-(r * (-dt)).exp_m1());
                    dfactor[e] = -((-residual[e] + 1.0) * dnew[e]) + 1.0;
                    damage_heat[e] = fraction[e] * (dnew[e] - m.damage[e]) * (-residual[e] + 1.0) * energy[e];
                }
            }
        }
        let mut branch = Vec::with_capacity(ne);
        let mut generated = vec![S::zero(); n];
        for (e, tet) in mesh.elements.iter().enumerate() {
            let combined = factor[e] * dfactor[e];
            let g: Vec<S> = k.g[e].iter().map(|x| *x * combined).collect();
            let vis = green_maxwell_step(&f_all[e], &m.branch[e], &g, &tau_all[e], S::from_f64(dt));
            let grad = &mesh.gradients[e];
            let mut gp2 = S::zero();
            for c in 0..3 {
                let mut acc = S::zero();
                for a in 0..4 {
                    acc += pressure[tet[a]] * grad[a][c];
                }
                gp2 += acc * acc;
            }
            let density = vis.dissipated + mob_all[e] * gp2 * dt + age_heat[e] + damage_heat[e];
            let share = density * mesh.volumes[e] / 4.0;
            for a in tet {
                generated[*a] += share;
            }
            params.push(PoroParams {
                mu: k.mu[e] * combined,
                lam: k.lam[e] * combined,
                g,
                tau: tau_all[e].clone(),
                alpha: k.alpha[e],
                biot: k.biot[e],
                mobility: mob_all[e],
            });
            branch.push(vis.branch_strain);
        }
        Fields { anew, dnew, branch, generated, params }
    }

    fn equations<S: Scalar>(&self, k: &Coeffs<S>, m: &Memory<S>, i: usize, y: &[S]) -> Vec<S> {
        let t = self.problem;
        let p = &t.poro;
        let mesh = &p.mesh;
        let n = mesh.node_count();
        let dt = p.steps[i];
        let fields = self.fields(k, m, dt, y);
        let mut r = vec![S::zero(); 5 * n];
        for (e, tet) in mesh.elements.iter().enumerate() {
            let u = local_u(y, tet);
            let pl: [S; 4] = std::array::from_fn(|a| y[3 * n + tet[a]]);
            let out = element_residual(mesh, e, &u, &pl, &fields.params[e], &m.branch[e], dt);
            for a in 0..4 {
                for c in 0..3 {
                    r[3 * tet[a] + c] += out[3 * a + c];
                }
                r[3 * n + tet[a]] += out[12 + a];
            }

            let (s, kk) = mesh.hydraulic_local(e, k.cvol[e].recip(), k.conduct[e]);
            for a in 0..4 {
                let mut acc = S::zero();
                for b in 0..4 {
                    let (ta, tb) = (y[4 * n + tet[b]], m.old_t[tet[b]]);
                    acc += s[a][b] * (ta - tb) + kk[a][b] * ta * dt;
                }
                r[4 * n + tet[a]] += acc;
            }
        }
        let mut cmean = S::zero();
        for c in &k.cvol {
            cmean += *c;
        }
        cmean = cmean / k.cvol.len() as f64;
        let heat_scale = cmean * (self.vsum * self.tref);
        let mech_scale = self.vsum * self.pref;
        for a in 0..n {
            for c in 0..3 {
                r[3 * a + c] = (r[3 * a + c] - p.force[i][3 * a + c]) * (self.scale[3 * a + c] / mech_scale);
            }
            r[3 * n + a] =
                (r[3 * n + a] + m.zold[a] + p.source[i][a] * dt) * (self.scale[3 * n + a] / mech_scale);
            let temp = y[4 * n + a];
            r[4 * n + a] = (r[4 * n + a] + k.h[a] * (temp - t.ambient[i][a]) * dt
                - t.heat_source[i][a] * dt
                - fields.generated[a])
                / heat_scale;
        }
        r
    }

    fn advance<S: Scalar>(&self, k: &Coeffs<S>, m: &Memory<S>, i: usize, y: &[S]) -> Memory<S> {
        let p = &self.problem.poro;
        let n = p.mesh.node_count();
        let fields = self.fields(k, m, p.steps[i], y);
        let mut total = m.total_heat;
        for g in &fields.generated {
            total += *g;
        }
        Memory {
            zold: self.content(k, &y[..3 * n], &y[3 * n..4 * n]),
            branch: fields.branch,
            extent: fields.anew,
            damage: fields.dnew,
            old_t: y[4 * n..].to_vec(),
            total_heat: total,
        }
    }

    fn response<S: Scalar>(&self, k: &Coeffs<S>, m: &Memory<S>, y: &[S], response: &str) -> S {
        let mesh = &self.problem.poro.mesh;
        let n = mesh.node_count();
        match response {
            "final_mean_temperature_K" => {
                let (mut num, mut den) = (S::zero(), S::zero());
                for (e, tet) in mesh.elements.iter().enumerate() {
                    let (s, _) = mesh.hydraulic_local(e, k.cvol[e].recip(), S::zero());
                    for a in 0..4 {
                        for b in 0..4 {
                            num += s[a][b] * y[4 * n + tet[b]];
                            den += s[a][b];
                        }
                    }
                }
                num / den
            }
            "generated_heat_J" => m.total_heat,
            "final_displacement_squared_m2" => {
                let mut acc = S::zero();
                for v in &y[..3 * n] {
                    acc += *v * *v;
                }
                acc
            }
            name => {
                let values = if name == "final_mean_ageing_extent" { &m.extent } else { &m.damage };
                let mut acc = S::zero();
                for (v, x) in mesh.volumes.iter().zip(values) {
                    acc += *x * *v;
                }
                acc / self.vsum
            }
        }
    }

    fn jacobian(&self, k: &Coeffs<f64>, m: &Memory<f64>, i: usize, y: &[f64]) -> DenseMatrix {
        const W: usize = 8;
        let nf = self.free.len();
        let kd: Coeffs<Dual<W>> = lift_coeffs(k);
        let md: Memory<Dual<W>> = lift_memory(m);
        let mut jac = DenseMatrix::zeros(nf, nf);
        for start in (0..nf).step_by(W) {
            let mut yd: Vec<Dual<W>> = y.iter().map(|v| Dual::constant(*v)).collect();
            for (slot, col) in (start..(start + W).min(nf)).enumerate() {
                yd[self.free[col]].eps[slot] = 1.0;
            }
            let r = self.equations(&kd, &md, i, &yd);
            for (row, f) in self.free.iter().enumerate() {
                for slot in 0..W.min(nf - start) {
                    jac.data[row * nf + start + slot] = r[*f].eps[slot];
                }
            }
        }
        jac
    }
}

fn lift_coeffs<const W: usize>(k: &Coeffs<f64>) -> Coeffs<Dual<W>> {
    let l = |v: &[f64]| lift::<Dual<W>>(v);
    let l2 = |v: &[Vec<f64>]| lift2::<Dual<W>>(v);
    Coeffs {
        mu: l(&k.mu),
        lam: l(&k.lam),
        g: l2(&k.g),
        tau0: l2(&k.tau0),
        alpha: l(&k.alpha),
        biot: l(&k.biot),
        mob0: l(&k.mob0),
        cvol: l(&k.cvol),
        conduct: l(&k.conduct),
        ea: l2(&k.ea),
        em: l(&k.em),
        h: l(&k.h),
        ageing: k.ageing.as_ref().map(|a| [l(&a[0]), l(&a[1]), l(&a[2]), l(&a[3])]),
        damage: k.damage.as_ref().map(|a| [l(&a[0]), l(&a[1]), l(&a[2]), l(&a[3])]),
    }
}

fn lift_memory<const W: usize>(m: &Memory<f64>) -> Memory<Dual<W>> {
    Memory {
        branch: lift_mats(&m.branch),
        zold: lift(&m.zold),
        extent: lift(&m.extent),
        damage: lift(&m.damage),
        old_t: lift(&m.old_t),
        total_heat: Dual::constant(m.total_heat),
    }
}

#[derive(Debug, Clone)]
pub struct ThermoSensitivity {
    pub value: f64,
    pub gradient: Vec<f64>,
    pub shape: Vec<usize>,
}

pub const SENSITIVITY_SCOPE: &str = "Implicit coupled equilibrium/heat history with ageing, damage and memory; fixed initial state, mesh, nonlocal length and schedule; piecewise derivatives away from damage activation thresholds";



#[allow(clippy::too_many_lines)]
pub fn material_sensitivity(
    problem: &ThermoProblem,
    raw: &Value,
    material: &str,
    response: &str,
) -> Result<ThermoSensitivity, CaeError> {
    let mut allowed: Vec<String> = MECHANICAL.iter().map(|k| format!("poromechanics.{k}")).collect();
    allowed.extend(THERMAL.iter().map(|k| format!("thermal.{k}")));
    if raw["thermal"].get("ageing").is_some() {
        allowed.extend(AGEING_KEYS.iter().map(|k| format!("thermal.ageing.{k}")));
    }
    if let Some(d) = raw["thermal"].get("damage") {
        allowed.extend(
            DAMAGE_KEYS.iter().filter(|k| d.get(**k).is_some()).map(|k| format!("thermal.damage.{k}")),
        );
    }
    if !allowed.iter().any(|a| a == material) || !RESPONSES.contains(&response) {
        return contract("Unsupported thermal-history derivative");
    }
    let admitted = problem.solve_history()?;
    let p = &problem.poro;
    let mesh = &p.mesh;
    let n = mesh.node_count();
    let fixed: Vec<bool> =
        p.fixed_dofs.iter().chain(&p.fixed_pressure).chain(&problem.fixed_t).copied().collect();
    let free: Vec<usize> = (0..5 * n).filter(|i| !fixed[*i]).collect();
    let length = (0..3)
        .map(|a| {
            let (lo, hi) = mesh
                .points
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), q| (l.min(q[a]), h.max(q[a])));
            hi - lo
        })
        .fold(f64::NEG_INFINITY, f64::max);
    let pref = p.mu.iter().chain(&p.lam).chain(&p.biot).copied().fold(f64::NEG_INFINITY, f64::max);
    let tref = problem.reference.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let scale: Vec<f64> = (0..5 * n)
        .map(|i| {
            if i < 3 * n {
                length
            } else if i < 4 * n {
                pref
            } else {
                tref
            }
        })
        .collect();
    let sens = Sensitivity {
        problem,
        material: material.split('.').collect(),
        free,
        scale,
        pref,
        tref,
        vsum: mesh.volumes.iter().sum(),
    };
    let (theta, shape) = sens.original()?;
    let states: Vec<Vec<f64>> = admitted
        .steps
        .iter()
        .map(|r| r.poro.displacement.iter().chain(&r.poro.pressure).chain(&r.temperature).copied().collect())
        .collect();

    let k0 = sens.coeffs::<f64>(&theta);
    let mut m = sens.initial(&k0);
    let mut factors = Vec::with_capacity(states.len());
    for (i, y) in states.iter().enumerate() {
        if sens.free.is_empty() {
            factors.push(None);
        } else {
            let jac = sens.jacobian(&k0, &m, i, y);
            factors.push(Some(
                DenseLu::new(&jac).map_err(|e| {
                    CaeError::contract(format!("Singular thermal-history step Jacobian: {e}"))
                })?,
            ));
        }
        m = sens.advance(&k0, &m, i, y);
    }
    let last = states.last().ok_or_else(|| CaeError::contract("empty thermal history"))?;
    let value = sens.response(&k0, &m, last, response);

    const W: usize = 8;
    let mut gradient = vec![0.0; theta.len()];
    for start in (0..theta.len()).step_by(W) {
        let width = W.min(theta.len() - start);
        let seeded: Vec<Dual<W>> = theta
            .iter()
            .enumerate()
            .map(|(j, v)| {
                let mut d = Dual::constant(*v);
                if (start..start + width).contains(&j) {
                    d.eps[j - start] = 1.0;
                }
                d
            })
            .collect();
        let k = sens.coeffs(&seeded);
        let mut md = sens.initial(&k);
        let mut yd: Vec<Dual<W>> = Vec::new();
        for (i, y) in states.iter().enumerate() {
            let y0: Vec<Dual<W>> = y.iter().map(|v| Dual::constant(*v)).collect();
            yd.clone_from(&y0);
            if let Some(lu) = &factors[i] {
                let r = sens.equations(&k, &md, i, &y0);
                let nf = sens.free.len();
                let mut rhs = vec![0.0; nf * width];
                for (row, f) in sens.free.iter().enumerate() {
                    for slot in 0..width {
                        rhs[row * width + slot] = -r[*f].eps[slot];
                    }
                }
                let dy = lu.solve(&rhs, width, false).map_err(|e| CaeError::contract(e.to_string()))?;
                for (row, f) in sens.free.iter().enumerate() {
                    for slot in 0..width {
                        yd[*f].eps[slot] = dy[row * width + slot];
                    }
                }
            }
            md = sens.advance(&k, &md, i, &yd);
        }
        let out = sens.response(&k, &md, &yd, response);
        gradient[start..start + width].copy_from_slice(&out.eps[..width]);
    }
    if !value.is_finite() || gradient.iter().any(|g| !g.is_finite()) {
        return contract("Nonfinite thermal-history derivative");
    }
    Ok(ThermoSensitivity { value, gradient, shape })
}

#[must_use]
pub fn validate_reply() -> Value {
    json!({"ok": true, "equilibrium_solved": false, "optimization_supported": false})
}

#[must_use]
pub fn mesh_of(problem: &ThermoProblem) -> &TetMesh {
    &problem.poro.mesh
}
