// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_core::{CaeError, CaeResult};

use crate::incompressible_transport::{FluidKernel, GroupSet, flat, ndindex};

pub type MechanicalTerms = Vec<(&'static str, f64)>;


#[allow(clippy::too_many_lines)]
pub fn mechanical_step_terms(
    f: &FluidKernel,
    n: usize,
    current: &[f64],
    previous: &[f64],
    design: &[f64],
) -> CaeResult<MechanicalTerms> {
    if n < 1 || n >= f.nt {
        return Err(CaeError::contract("MAC energy requires a positive authored history interval"));
    }
    if current.len() != f.state_size || previous.len() != f.state_size || design.len() != f.design_size {
        return Err(CaeError::contract(
            "MAC energy requires real current/previous/design vectors of the native shapes",
        ));
    }
    let (z, x) = (current, design);
    let fields = f.fields(z);
    let old = f.fields(previous);
    let h: [f64; 3] = std::array::from_fn(|a| x[f.nc + a] * 1e-3);
    let v = h[0] * h[1] * h[2];
    let times: Vec<f64> =
        f.p["times_s"].as_array().map(|t| t.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let dt = times[n] - times[n - 1];
    let (mut kinetic, mut oldkinetic, mut delta, mut time_defect) = (0.0, 0.0, 0.0, 0.0);
    let mut divergence = vec![0.0; f.nc];
    let (mut normal_viscous, mut drag) = (0.0, 0.0);
    let mu: Vec<f64> = fields.temperature.iter().map(|t| f.law.properties(*t).0).collect();
    for axis in 0..3 {
        let shape = f.map_shapes[axis];
        let (mut k, mut ok, mut d, mut td, mut nv, mut dr) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for cell in ndindex(f.grid) {
            let mut hi = cell;
            hi[axis] += 1;
            let (lo, up) = (fields.faces[axis][flat(shape, cell)], fields.faces[axis][flat(shape, hi)]);
            let (olo, oup) = (old.faces[axis][flat(shape, cell)], old.faces[axis][flat(shape, hi)]);
            let i = f.cell_index(cell);
            k += lo * lo + up * up;
            ok += olo * olo + oup * oup;
            d += (lo - olo) * (lo + olo) + (up - oup) * (up + oup);
            td += (lo - olo).powi(2) + (up - oup).powi(2);
            divergence[i] += (up - lo) * v / h[axis];
            nv += 2.0 * f.cell_viscosity(mu[i], x[i], &fields, cell, h) * v * ((up - lo) / h[axis]).powi(2);
            dr += f.params.alpha(x[i]) * v / 2.0 * (lo * lo + up * up);
        }
        kinetic += f.rho * v / 4.0 * k;
        oldkinetic += f.rho * v / 4.0 * ok;
        delta += f.rho * v / 4.0 * d;
        time_defect += f.rho * v / 4.0 * td;
        normal_viscous += dt * nv;
        drag += dt * dr;
    }
    let pressure_constraint = -dt * fields.pressure.iter().zip(&divergence).map(|(p, d)| p * d).sum::<f64>();
    let velocity = |id: i64| usize::try_from(id).map_or(0.0, |i| f.us * z[i]);
    let mut shear_energy = 0.0;
    for r in &f.shear_records {
        let vel: Vec<f64> = r.iv.iter().map(|id| velocity(*id)).collect();
        let shear = r.factors[0] * (vel[1] - vel[0]) / h[r.b] + r.factors[1] * (vel[3] - vel[2]) / h[r.a];
        let mut mu_edge: f64 = r.w.iter().zip(&r.ci).map(|(w, c)| w * mu[*c]).sum();
        if let Some(ml) = &f.params.turbulence {
            let phi: f64 = r.w.iter().zip(&r.ci).map(|(w, c)| w * (1.0 - x[*c])).sum();
            mu_edge += ml.eddy_viscosity(f.params.rho, phi, shear * shear);
        }
        shear_energy += dt * mu_edge * v * r.vf * shear * shear;
    }
    let mut body_work = 0.0;
    if let Some((g, beta, tref)) = f.params.body {
        let mut total = 0.0;
        for (t, u) in fields.temperature.iter().zip(&fields.velocity) {
            let factor = f.rho * (1.0 - beta * (t - tref));
            for a in 0..3 {
                total += factor * g[a] * u[a];
            }
        }
        body_work = dt * v * total;
    }
    let (mut pressure_in, mut absolute_in, mut net_open_volume) = (0.0, 0.0, 0.0);
    for b in &f.boundaries {
        if !b.pressure {
            continue;
        }
        let lo = b.side == "lo";
        let sign = if lo { -1.0 } else { 1.0 };
        let pressure = b.raw["pressure_absolute_Pa"][n].as_f64().unwrap_or(f64::NAN);
        for cell in f.opening_cells(b.axis, &b.side)? {
            let mut ix = cell;
            if !lo {
                ix[b.axis] += 1;
            }
            let swept = sign * fields.faces[b.axis][flat(f.map_shapes[b.axis], ix)] * v / h[b.axis] * dt;
            pressure_in -= (pressure - f.pref) * swept;
            absolute_in -= pressure * swept;
            net_open_volume += swept;
        }
    }
    let (mut advection_work, mut upwind, mut boundary_transport) = (0.0, 0.0, 0.0);
    let mut dual_mass_out = vec![0.0; f.nv];
    if f.p["momentum_advection"].as_bool() == Some(true) {
        if let Some((ids, meta)) = &f.advection_energy_interior {
            for (row, (axis, factor)) in ids.iter().zip(meta) {
                let u: Vec<f64> = row.iter().map(|id| velocity(*id)).collect();
                let m = f.rho * (v / h[*axis]) * factor * 0.5 * (u[2] + u[3]);
                let (flux, split_magnitude) = match f.params.advection_smoothing {
                    None => (m.max(0.0) * u[0] + m.min(0.0) * u[1], m.abs()),
                    Some(delta) => {
                        let velocity = 0.5 * (u[2] + u[3]);
                        let magnitude = (velocity * velocity + delta * delta).sqrt();
                        let area = (v / h[*axis]) * factor;
                        let flux = area * f.params.rho
                            * (0.5 * (velocity + magnitude) * u[0]
                                + 0.5 * (velocity - magnitude) * u[1]);
                        (flux, area * f.params.rho * magnitude)
                    }
                };
                advection_work += dt * flux * (u[0] - u[1]);
                upwind += dt * 0.5 * split_magnitude * (u[0] - u[1]).powi(2);
                for (column, sign) in [(0usize, 1.0), (1, -1.0)] {
                    if let Ok(i) = usize::try_from(row[column]) {
                        dual_mass_out[i] += sign * m;
                    }
                }
            }
        }
        for (axis, sign, ids, factors) in &f.advection_energy_boundary {
            for (row, factor) in ids.iter().zip(factors) {
                let u: Vec<f64> = row.iter().map(|id| velocity(*id)).collect();
                let outward = sign * f.rho * (v / h[*axis]) * factor * 0.5 * (u[1] + u[2]);
                advection_work += dt * outward * u[0] * u[0];
                boundary_transport += dt * 0.5 * outward * u[0] * u[0];
                if let Ok(i) = usize::try_from(row[0]) {
                    dual_mass_out[i] += outward;
                }
            }
        }
    }
    let dual_continuity_work =
        dt * 0.5 * z[..f.nv].iter().zip(&dual_mass_out).map(|(u, m)| (f.us * u).powi(2) * m).sum::<f64>();
    let advection_identity = advection_work - upwind - boundary_transport - dual_continuity_work;
    let dissipation = normal_viscous + shear_energy + drag;
    let balance =
        delta + time_defect + dissipation + advection_work + pressure_constraint - pressure_in - body_work;
    Ok(vec![
        ("kinetic_energy_J", kinetic),
        ("previous_kinetic_energy_J", oldkinetic),
        ("kinetic_energy_increment_J", delta),
        ("backward_euler_defect_J", time_defect),
        ("normal_viscous_dissipation_J", normal_viscous),
        ("shear_viscous_dissipation_J", shear_energy),
        ("brinkman_dissipation_J", drag),
        ("mechanical_dissipation_J", dissipation),
        ("pressure_boundary_work_inward_J", pressure_in),
        ("absolute_pressure_boundary_work_inward_J", absolute_in),
        ("pressure_reference_volume_work_J", f.pref * net_open_volume),
        ("body_work_inward_J", body_work),
        ("continuity_constraint_work_J", pressure_constraint),
        ("advection_work_J", advection_work),
        ("upwind_kinetic_defect_J", upwind),
        ("advective_boundary_kinetic_transport_outward_J", boundary_transport),
        ("dual_continuity_work_J", dual_continuity_work),
        ("advection_identity_defect_J", advection_identity),
        ("momentum_energy_balance_J", balance),
    ])
}


pub fn observe_mechanical_energy(
    f: &FluidKernel,
    n: usize,
    current: &[f64],
    previous: &[f64],
    design: &[f64],
) -> CaeResult<Value> {
    let mut terms: Map<String, Value> = Map::new();
    let raw = mechanical_step_terms(f, n, current, previous, design)?;
    let get = |k: &str| raw.iter().find(|(name, _)| *name == k).map_or(f64::NAN, |(_, v)| *v);
    let times: Vec<f64> =
        f.p["times_s"].as_array().map(|t| t.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let dt = times[n] - times[n - 1];
    let residual = f.residual(GroupSet::All, n, current, previous, design)?;
    let work =
        dt * residual[..f.nv].iter().zip(&current[..f.nv]).map(|(r, u)| (f.fs * r) * (f.us * u)).sum::<f64>();
    let actual_heat = dt * f.dissipation_w(current, design);
    let mut values: Vec<(&str, f64)> = raw.clone();
    values.push(("actual_momentum_residual_work_J", work));
    values.push(("momentum_identity_defect_J", get("momentum_energy_balance_J") - work));
    values.push(("assembled_dissipative_heat_J", actual_heat));
    values.push(("dissipation_heat_transfer_defect_J", get("mechanical_dissipation_J") - actual_heat));
    if values.iter().any(|(_, v)| !v.is_finite()) {
        return Err(CaeError::contract("nonfinite MAC discrete-energy observation"));
    }
    for (k, v) in values {
        terms.insert(k.to_string(), json!(v));
    }
    Ok(json!({"available": true, "schema": "implexity-mac-discrete-work/1", "terms": terms,
        "kinetic_mass": "full_staggered_dual_mass_of_actual_momentum_residual",
        "caloric_capacity": "authored_effective_fluid_fraction",
        "upwind_and_backward_euler_defects_added_as_heat": false,
        "continuity_work_is_retained": true, "moving_wall_work_implemented": false,
        "changes_solve_admission": false}))
}
