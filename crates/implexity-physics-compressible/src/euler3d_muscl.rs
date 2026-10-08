// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_physics_solid::structural_dynamics::{conforming_interface_map, transfer_conforming_forces};
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::euler3d::{
    Problem, WallLayout, add_rate, check_all, face_index, faces, initial_state, normalize, outward, storage,
};
use crate::euler3d_ad::{
    AdmittedHistory, exact_int, face_fractions, history_bytes,
    outward_boundary_rates, real_step, schedule_matches, transport_rhs, volume_fraction_rhs,
    wall_force_history,
};
use crate::pyval::nums;

type StageRhs = (Vec<[f64; 5]>, [f64; 5], [f64; 5]);

pub const NAME: &str = "cartesian_euler3d_muscl_topology";


#[allow(clippy::type_complexity)]
pub fn reference_rhs(
    q: &[[f64; 5]],
    p: &Problem,
    phi: Option<&[f64]>,
) -> PResult<(Vec<[f64; 5]>, Vec<f64>, [f64; 5], [f64; 5])> {
    let n = p.cells();
    let mut derivative = vec![[0.0; 5]; n];
    let mut rate = vec![0.0; n];
    let mut out = [0.0; 5];
    let mut source = [0.0; 5];
    let vol = p.volume();
    let pressure: Vec<f64> = q.iter().map(|x| crate::euler3d::primitive(*x, p.gamma)[4]).collect();
    for axis in 0..3 {
        let fc = faces(q, p, axis, true, true)?;
        let h = p.spacing[axis];
        match phi {
            None => {
                crate::euler3d::divergence(p, axis, &fc.flux, &mut derivative);
                add_rate(p, axis, &fc.speed, None, &mut rate);
                let o = outward(p, axis, &fc.flux);
                for c in 0..5 {
                    out[c] += o[c];
                }
            }
            Some(phi) => {
                let fphi = face_fractions(p, phi, axis);
                let weighted: Vec<[f64; 5]> =
                    fc.flux.iter().zip(&fphi).map(|(f, w)| f.map(|x| w * x)).collect();
                crate::euler3d::divergence(p, axis, &weighted, &mut derivative);
                let mut total = 0.0;
                for i in 0..p.shape[0] {
                    for j in 0..p.shape[1] {
                        for k in 0..p.shape[2] {
                            let idx = [i, j, k];
                            let mut next = idx;
                            next[axis] += 1;
                            let c = p.cell(idx);
                            let force = pressure[c]
                                * (fphi[face_index(p, axis, next)] - fphi[face_index(p, axis, idx)])
                                / h;
                            derivative[c][axis + 1] += force;
                            total += force;
                        }
                    }
                }
                source[axis + 1] = total * vol;
                add_rate(p, axis, &fc.speed, Some((&fphi, phi)), &mut rate);
                let m = p.shape[axis];
                let (a1, a2) = crate::euler3d::others(axis);
                let layer = |t: usize| {
                    let mut s = [0.0; 5];
                    for u in 0..p.shape[a1] {
                        for v in 0..p.shape[a2] {
                            let mut idx = [0; 3];
                            idx[axis] = t;
                            idx[a1] = u;
                            idx[a2] = v;
                            let fi = face_index(p, axis, idx);
                            for c in 0..5 {
                                s[c] += weighted[fi][c];
                            }
                        }
                    }
                    s
                };
                let (hi, lo) = (layer(m), layer(0));
                for c in 0..5 {
                    out[c] += (hi[c] - lo[c]) * vol / h;
                }
            }
        }
    }
    if let Some(phi) = phi {
        for (row, f) in derivative.iter_mut().zip(phi) {
            for x in row.iter_mut() {
                *x /= f;
            }
        }
    }
    for (c, row) in derivative.iter_mut().enumerate() {
        if !p.fluid_mask[c] {
            *row = [0.0; 5];
        }
    }
    Ok((derivative, rate, out, source))
}

#[derive(Debug, Clone, PartialEq)]
pub struct MusclLedger {
    pub initial: [f64; 5],
    pub final_: [f64; 5],
    pub outward_integrals: [f64; 5],
    pub maximum_cfl: f64,
    pub scaled_balance_error: [f64; 5],
    pub geometry_pressure_impulse: [f64; 5],
    pub balance_error: [f64; 5],
}

impl MusclLedger {
    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("initial".into(), nums(&self.initial));
        m.insert("final".into(), nums(&self.final_));
        m.insert("outward_integrals".into(), nums(&self.outward_integrals));
        m.insert("maximum_cfl".into(), json!(self.maximum_cfl));
        m.insert("scaled_balance_error".into(), nums(&self.scaled_balance_error));
        m.insert("geometry_pressure_impulse".into(), nums(&self.geometry_pressure_impulse));
        m.insert("balance_error".into(), nums(&self.balance_error));
        m.insert("method".into(), json!(METHOD));
        m
    }
}

pub const METHOD: &str = "primitive_MC_Rusanov_SSPRK2";

fn axpy(q: &[[f64; 5]], dt: f64, d: &[[f64; 5]]) -> Vec<[f64; 5]> {
    q.iter().zip(d).map(|(a, b)| std::array::from_fn(|c| a[c] + dt * b[c])).collect()
}


#[allow(clippy::too_many_lines)]
pub fn checked_history(
    problem: &Value,
    step_s: &Value,
    step_count: &Value,
    budget: &Value,
    fluid_fraction: Option<&[f64]>,
) -> PResult<(Vec<Vec<[f64; 5]>>, MusclLedger)> {
    let p = normalize(problem)?;
    if let Some(f) = fluid_fraction {
        if f.len() != p.cells() || !f.iter().all(|v| v.is_finite()) {
            return Err(ModelError::invalid(format!(
                "fluid_fraction has nonfinite values or incorrect shape; expected {}",
                crate::pyval::shape_repr(&p.shape)
            )));
        }
        if f.iter().any(|v| *v <= 0.0 || *v > 1.0) || !p.all_fluid() {
            return Err(ModelError::invalid(
                "diffuse-volume MUSCL requires 0 < phi <= 1 on an all-fluid base grid",
            ));
        }
    }
    let Some(count) = exact_int(step_count).filter(|c| (1..=p.max_steps).contains(c)) else {
        return Err(ModelError::invalid("invalid MUSCL step count"));
    };
    #[allow(clippy::cast_precision_loss)]
    let Some(dt) = real_step(step_s).filter(|dt| schedule_matches(dt * count as f64, p.end_time)) else {
        return Err(ModelError::invalid("MUSCL fixed schedule must match end time"));
    };
    if exact_int(budget).is_none_or(|b| b < history_bytes(&p, count)) {
        return Err(ModelError::invalid("MUSCL state history exceeds byte budget"));
    }
    let count = usize::try_from(count).unwrap_or(usize::MAX);
    let mut q = initial_state(&p);
    check_all(&q, p.gamma)?;
    let mut states = vec![q.clone()];
    let ones = vec![1.0; p.cells()];
    let weight = fluid_fraction.unwrap_or(&ones);
    let initial = storage(&p, &q, Some(weight));
    let mut exchange = [0.0; 5];
    let mut impulse = [0.0; 5];
    let mut maximum_cfl: f64 = 0.0;
    let mut admitted_rhs = |state: &[[f64; 5]]| -> PResult<StageRhs> {
        check_all(state, p.gamma)?;
        let (derivative, rate, flow, source) = reference_rhs(state, &p, fluid_fraction)?;
        let mut peak = f64::NEG_INFINITY;
        let mut nan = false;
        for (r, m) in rate.iter().zip(&p.fluid_mask) {
            if *m {
                nan |= r.is_nan();
                peak = peak.max(*r);
            }
        }
        let cfl = dt * peak;
        if !nan {
            maximum_cfl = maximum_cfl.max(cfl);
        }
        if !rate.iter().all(|r| r.is_finite()) || nan || cfl > p.cfl * (1.0 + 1e-12) {
            return Err(ModelError::invalid("MUSCL stage exceeds authored CFL; no retry"));
        }
        Ok((derivative, flow, source))
    };
    let mut final_ = initial;
    let mut balance = [0.0; 5];
    let mut scaled = [0.0; 5];
    for _ in 0..count {
        let (rhs, flow, source) = admitted_rhs(&q)?;
        let stage = axpy(&q, dt, &rhs);
        let (rhs_stage, flow_stage, source_stage) = admitted_rhs(&stage)?;
        let forward = axpy(&stage, dt, &rhs_stage);
        check_all(&forward, p.gamma)?;
        q = q.iter().zip(&forward).map(|(a, b)| std::array::from_fn(|c| 0.5 * a[c] + 0.5 * b[c])).collect();
        check_all(&q, p.gamma)?;
        for c in 0..5 {
            exchange[c] += 0.5 * dt * (flow[c] + flow_stage[c]);
            impulse[c] += 0.5 * dt * (source[c] + source_stage[c]);
        }
        states.push(q.clone());
        final_ = storage(&p, &q, Some(weight));
        for c in 0..5 {
            balance[c] = final_[c] - initial[c] + exchange[c] - impulse[c];
            scaled[c] = balance[c]
                / (initial[c].abs() + final_[c].abs() + exchange[c].abs() + impulse[c].abs()).max(1.0);
        }
        if scaled.iter().map(|x| x.abs()).fold(0.0, f64::max) > 1e-10 {
            return Err(ModelError::invalid("MUSCL stage-weighted conservation ledger failed"));
        }
    }
    let ledger = MusclLedger {
        initial,
        final_,
        outward_integrals: exchange,
        maximum_cfl,
        scaled_balance_error: scaled,
        geometry_pressure_impulse: impulse,
        balance_error: balance,
    };
    Ok((states, ledger))
}


pub fn admit_history(flow: &Value, timing: &Value, phi: &[f64]) -> PResult<AdmittedHistory> {
    let (states, ledger) = checked_history(
        flow,
        &timing["step_s"],
        &timing["step_count"],
        &timing["history_byte_budget"],
        Some(phi),
    )?;
    let step_s = timing["step_s"].as_f64().unwrap_or(f64::NAN);
    let step_count = states.len() - 1;
    let mut info = Map::new();
    info.insert("status".into(), json!("completed_admitted_fixed_history"));
    info.insert("step_s".into(), json!(step_s));
    info.insert("step_count".into(), json!(step_count));
    info.insert("maximum_cfl".into(), json!(ledger.maximum_cfl));
    info.insert("scaled_balance_error".into(), nums(&ledger.scaled_balance_error));
    info.insert("method".into(), json!(METHOD));
    info.insert("outward_boundary_integrals".into(), nums(&ledger.outward_integrals));
    info.insert("geometry_pressure_impulse".into(), nums(&ledger.geometry_pressure_impulse[1..4]));
    Ok(AdmittedHistory {
        states,
        fluid_fraction: phi.to_vec(),
        step_s,
        step_count,
        maximum_cfl: ledger.maximum_cfl,
        scaled_balance_error: ledger.scaled_balance_error,
        info,
    })
}


pub fn step<S: implexity_ad::Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: Option<&[S]>,
    dt: f64,
) -> PResult<Vec<[S; 5]>> {
    let rhs = |x: &[[S; 5]]| match phi {
        Some(phi) => volume_fraction_rhs(x, p, phi, true),
        None => transport_rhs(x, p, true),
    };
    let d = rhs(q)?;
    let stage: Vec<[S; 5]> =
        q.iter().zip(&d).map(|(a, b)| std::array::from_fn(|c| a[c] + b[c] * dt)).collect();
    let d2 = rhs(&stage)?;
    Ok(q.iter()
        .zip(stage.iter().zip(&d2))
        .map(|(a, (s, r))| std::array::from_fn(|c| a[c] * 0.5 + (s[c] + r[c] * dt) * 0.5))
        .collect())
}


pub fn fixed_history(
    q0: &[[f64; 5]],
    p: &Problem,
    dt: f64,
    count: usize,
    phi: Option<&[f64]>,
) -> PResult<Vec<Vec<[f64; 5]>>> {
    let mut states = vec![q0.to_vec()];
    for n in 0..count {
        let next = step(&states[n], p, phi, dt)?;
        states.push(next);
    }
    Ok(states)
}


pub fn history_responses(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    phi: &[f64],
    dt: f64,
) -> PResult<crate::euler3d_ad::HistoryResponses> {
    let mut exchanges = [[0.0; 5]; 6];
    for q in &states[..states.len() - 1] {
        let stage = axpy(q, dt, &volume_fraction_rhs(q, p, phi, true)?);
        let (a, b) =
            (outward_boundary_rates(q, p, phi, true)?, outward_boundary_rates(&stage, p, phi, true)?);
        for f in 0..6 {
            for c in 0..5 {
                exchanges[f][c] += 0.5 * dt * (a[f][c] + b[f][c]);
            }
        }
    }
    let weighted = |q: &[[f64; 5]]| {
        let mut out = [0.0; 5];
        for (row, f) in q.iter().zip(phi) {
            for c in 0..5 {
                out[c] += f * row[c];
            }
        }
        out.map(|x| x * p.volume())
    };
    Ok(crate::euler3d_ad::HistoryResponses {
        final_boundary_rates: outward_boundary_rates(&states[states.len() - 1], p, phi, true)?,
        boundary_exchanges: exchanges,
        initial_storage: weighted(&states[0]),
        final_storage: weighted(&states[states.len() - 1]),
        fluid_volume_m3: phi.iter().sum::<f64>() * p.volume(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct MusclSolidLoads {
    pub times_s: Vec<f64>,
    pub interval_nodal_forces_n: Vec<Vec<[f64; 3]>>,
    pub instantaneous_nodal_forces_n: Vec<Vec<[f64; 3]>>,
    pub nodal_impulse_ns: Vec<[f64; 3]>,
    pub source_to_solid_node_indices: Vec<usize>,
}

pub const FORCE_SAMPLING: &str = "SSPRK2_stage_average_held_constant";


pub fn conforming_solid_load_history(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    layout: &WallLayout,
    dt: f64,
    solid_nodes: &[[f64; 3]],
) -> PResult<MusclSolidLoads> {
    let mapping = conforming_interface_map(&layout.node_positions, solid_nodes)?;
    let predictor: Vec<Vec<[f64; 5]>> = states[..states.len() - 1]
        .iter()
        .map(|q| transport_rhs(q, p, true).map(|d| axpy(q, dt, &d)))
        .collect::<PResult<_>>()?;
    let actual = wall_force_history(states, p, layout, dt, true)?;
    let stage = wall_force_history(&predictor, p, layout, dt, true)?;
    let n = solid_nodes.len();
    let interval: Vec<Vec<[f64; 3]>> = actual.nodal_forces_n[..states.len() - 1]
        .iter()
        .zip(&stage.nodal_forces_n)
        .map(|(a, b)| a.iter().zip(b).map(|(x, y)| std::array::from_fn(|c| 0.5 * (x[c] + y[c]))).collect())
        .collect();
    let transferred: Vec<Vec<[f64; 3]>> =
        interval.iter().map(|f| transfer_conforming_forces(f, &mapping, n)).collect();
    let mut impulse = vec![[0.0; 3]; n];
    for sample in &transferred {
        for (o, x) in impulse.iter_mut().zip(sample) {
            for c in 0..3 {
                o[c] += x[c];
            }
        }
    }
    Ok(MusclSolidLoads {
        times_s: actual.times_s,
        instantaneous_nodal_forces_n: actual
            .nodal_forces_n
            .iter()
            .map(|f| transfer_conforming_forces(f, &mapping, n))
            .collect(),
        interval_nodal_forces_n: transferred,
        nodal_impulse_ns: impulse.into_iter().map(|r| r.map(|x| x * dt)).collect(),
        source_to_solid_node_indices: mapping,
    })
}
