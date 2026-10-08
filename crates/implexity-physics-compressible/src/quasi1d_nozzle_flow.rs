// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use serde_json::{Value, json};
use crate::{errors::{PResult,ModelError},quasi1d_euler::linspace};
pub const G0_M_S2: f64 = 9.80665;





const CF_A: f64 = 0.026;

const CF_EXP: f64 = 0.2;


#[must_use]
#[allow(clippy::float_cmp)]
pub fn isentropic_mach_from_area_ratio(ar: f64, gamma: f64, supersonic: bool) -> f64 {
    let expo = (gamma + 1.0) / (2.0 * (gamma - 1.0));
    let f = |m: f64| {
        let t = (2.0 / (gamma + 1.0)) * (1.0 + 0.5 * (gamma - 1.0) * m * m);
        (1.0 / m) * t.powf(expo) - ar
    };
    let (mut lo, mut hi) = if supersonic { (1.001, 50.0) } else { (1.0e-4, 0.9999) };
    let mut f_lo = f(lo);
    let f_hi = f(hi);
    if f_lo * f_hi > 0.0 {
        return if f_hi.abs() < f_lo.abs() { hi } else { lo };
    }
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        let f_mid = f(mid);
        if f_mid == 0.0 || (hi - lo) < 1.0e-12 {
            break;
        }
        if f_mid * f_lo < 0.0 {
            hi = mid;
        } else {
            lo = mid;
            f_lo = f_mid;
        }
    }
    let mut m = 0.5 * (lo + hi);
    for _ in 0..3 {
        let t = (2.0 / (gamma + 1.0)) * (1.0 + 0.5 * (gamma - 1.0) * m * m);
        let dt = (2.0 / (gamma + 1.0)) * (gamma - 1.0) * m;
        let val = (1.0 / m) * t.powf(expo) - ar;
        let deriv = -(1.0 / (m * m)) * t.powf(expo) + (1.0 / m) * expo * t.powf(expo - 1.0) * dt;
        if deriv == 0.0 {
            break;
        }
        let mut next = m - val / deriv;
        if supersonic && next < 1.0 {
            next = 1.001;
        }
        if !supersonic && !(0.0..=1.0).contains(&next) {
            next = 0.5 * (m + 1.0);
        }
        m = next;
    }
    m
}

pub fn argmin(v: &[f64]) -> usize {
    let mut k = 0;
    for (i, x) in v.iter().enumerate() {
        if *x < v[k] {
            k = i;
        }
    }
    k
}

#[must_use]
pub fn isentropic_warmstart(
    z_faces: &[f64],
    r_faces: &[f64],
    p_c: f64,
    t_c: f64,
    gamma: f64,
    r: f64,
) -> (Vec<[f64; 3]>, Vec<f64>) {
    let area_faces: Vec<f64> = r_faces.iter().map(|x| std::f64::consts::PI * x * x).collect();
    let r_c: Vec<f64> = r_faces.windows(2).map(|w| 0.5 * (w[0] + w[1])).collect();
    let area_c: Vec<f64> = r_c.iter().map(|x| std::f64::consts::PI * x * x).collect();
    let n = z_faces.len() - 1;
    let throat = argmin(&area_c);
    let prim = (0..n)
        .map(|i| {
            let ar = area_c[i] / area_c[throat];
            let m = isentropic_mach_from_area_ratio(ar.max(1.0 + 1e-12), gamma, i > throat);
            let t = t_c / (1.0 + 0.5 * (gamma - 1.0) * m * m);
            let p = p_c * (t / t_c).powf(gamma / (gamma - 1.0));
            [p / (r * t), m * (gamma * r * t).sqrt(), p]
        })
        .collect();
    (prim, area_faces)
}

#[must_use]
pub fn wall_friction_drag_n(
    z_faces: &[f64],
    r_faces: &[f64],
    density: &[f64],
    velocity: &[f64],
    mu: f64,
) -> f64 {
    let n = z_faces.len() - 1;
    let terms: Vec<f64> = (0..n)
        .map(|i| {
            let r_c = 0.5 * (r_faces[i] + r_faces[i + 1]);
            let dz = z_faces[i + 1] - z_faces[i];
            let d_local = 2.0 * r_c.max(1.0e-6);
            let re = (density[i] * velocity[i].abs() * d_local / mu).max(1.0e3);
            let cf = CF_A / re.powf(CF_EXP);
            let tau = 0.5 * density[i] * velocity[i] * velocity[i] * cf;
            tau * 2.0 * std::f64::consts::PI * r_c * dz
        })
        .collect();
    implexity_mesh::numeric::pairwise_sum(&terms)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContourFlow {
    pub n_cells: usize,
    pub end_time_s: f64,
    pub cfl: f64,
    pub max_steps: i64,
    pub include_friction: bool,
    pub mu_gas_pa_s: f64,
}
impl ContourFlow {
    #[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
    pub fn evaluate(
        &self,
        r: &[f64],
        z: &[f64],
        p_c: f64,
        t_c: f64,
        gamma: f64,
        rs: f64,
        p_amb: f64,
        provenance: &str,
    ) -> PResult<Value> {
        if self.n_cells < 20 { return Err(ModelError::validation("n_cells must be an integer >= 20", "quasi1d_nozzle_flow.n_cells")); }
        if self.end_time_s.is_nan() || self.end_time_s <= 0.0 { return Err(ModelError::validation("end_time_s must be a positive float", "quasi1d_nozzle_flow.end_time_s")); }
        if r.len() != z.len() || r.len() < 3 {
            return Err(ModelError::validation(
                "contour_r and contour_z must be 1-D arrays of the same length >= 3",
                "quasi1d_nozzle_flow.contour",
            ));
        }
        let bad = |x: f64| x.is_nan();
        if bad(p_c)
            || bad(t_c)
            || bad(gamma)
            || bad(rs)
            || bad(p_amb)
            || p_c <= 0.0
            || t_c <= 0.0
            || gamma <= 1.0
            || gamma > 2.0
            || rs <= 0.0
            || p_amb < 0.0
        {
            return Err(ModelError::validation(
                "require positive p_c, T_c, R, non-negative p_amb, and gamma in (1, 2]",
                "quasi1d_nozzle_flow.chamber_state",
            ));
        }
        if !z.windows(2).all(|w| w[1] - w[0] > 0.0) {
            return Err(ModelError::validation(
                "contour_z must be strictly monotone increasing",
                "quasi1d_nozzle_flow.contour_z",
            ));
        }
        let z_faces = linspace(z[0], z[z.len() - 1], self.n_cells + 1);
        let r_faces: Vec<f64> =
            z_faces.iter().map(|x| implexity_mesh::numeric::interp(*x, z, r).max(1.0e-6)).collect();
        let (prim, area_faces) = isentropic_warmstart(&z_faces, &r_faces, p_c, t_c, gamma, rs);
        let problem = json!({
            "gamma": gamma, "gas_constant_J_kgK": rs, "x_faces_m": z_faces, "area_faces_m2": area_faces,
            "initial_primitive": prim,
            "boundaries": {"left": {"kind": "subsonic_reservoir", "total_pressure_Pa": p_c, "total_temperature_K": t_c},
                           "right": {"kind": "transmissive"}},
            "end_time_s": self.end_time_s, "cfl": self.cfl, "max_steps": self.max_steps,
            "provenance": provenance,
        });
        let result = crate::quasi1d_euler::solve(&problem)?;
        let density: Vec<f64> = result.w.iter().map(|w| w[0]).collect();
        let velocity: Vec<f64> = result.w.iter().map(|w| w[1]).collect();
        let pressure: Vec<f64> = result.w.iter().map(|w| w[2]).collect();
        let area_c: Vec<f64> = area_faces.windows(2).map(|w| 0.5 * (w[0] + w[1])).collect();
        let throat = argmin(&area_c);
        let throat_area = area_c[throat];
        let exit_area = area_faces[area_faces.len() - 1];
        let last = density.len() - 1;
        let (rho_e, u_e, p_e) = (density[last], velocity[last], pressure[last]);
        let mdot = rho_e * u_e * exit_area;
        let thrust_inviscid = mdot * u_e + (p_e - p_amb) * exit_area;
        let friction = wall_friction_drag_n(&z_faces, &r_faces, &density, &velocity, self.mu_gas_pa_s);
        let thrust = if self.include_friction { thrust_inviscid - friction } else { thrust_inviscid };
        let ledger = result.relative.iter().map(|x| x.abs()).fold(f64::NEG_INFINITY, f64::max);
        Ok(json!({
            "thrust_N": thrust, "thrust_inviscid_N": thrust_inviscid, "friction_drag_N": friction,
            "specific_impulse_s": thrust / (mdot * G0_M_S2).max(1.0e-12), "mass_flow_kg_s": mdot,
            "exit_mach": result.mach[last], "exit_pressure_Pa": p_e, "exit_temperature_K": result.temperature[last],
            "exit_velocity_m_s": u_e, "exit_density_kg_m3": rho_e, "exit_area_m2": exit_area, "throat_area_m2": throat_area,
            "throat_index": throat, "area_ratio_exit": exit_area / throat_area.max(1.0e-12),
            "z_cell_centers_m": z_faces.windows(2).map(|w| 0.5 * (w[0] + w[1])).collect::<Vec<_>>(),
            "r_cell_centers_m": r_faces.windows(2).map(|w| 0.5 * (w[0] + w[1])).collect::<Vec<_>>(),
            "mach": result.mach, "pressure_Pa": pressure, "temperature_K": result.temperature, "density_kg_m3": density,
            "velocity_m_s": velocity, "area_m2": area_c, "n_cells": self.n_cells, "end_time_s_actual": result.time,
            "steps": result.steps, "ledger_relative_max": ledger,
        }))
    }
}
