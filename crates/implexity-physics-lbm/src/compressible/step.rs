// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::{CaeError, CaeResult};
use rayon::prelude::*;
use serde_json::{Map, Value, json};

use super::equilibrium::{Lattice, macroscopic};
use super::kernels::{
    CollisionDiagnostics, DragTransfer, gas_solid_exchange, gas_solid_exchange_vjp, macroscopic_vjp,
    porous_drag, porous_drag_vjp, prescribed_heat, prescribed_heat_vjp, stabilized_collision,
    stabilized_collision_vjp,
};
use super::solid::{ConductionMap, ReservoirMap, ViscosityLaw, WallFlux};
use super::transport::{
    FaceExchange, Transport, TransportKind, box_face_impulses, stream_pair, stream_pair_vjp,
    wall_event_impulses, wall_event_impulses_vjp,
};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Copy, Debug)]
pub struct StepConfig<'a> {
    pub gamma: f64,
    pub tau: f64,
    pub transport: &'a Transport,
    pub exposure: Option<&'a [f64]>,
    pub gas_heat_fraction: f64,
    pub wall_heat: Option<&'a WallFlux>,
    pub viscosity: Option<&'a ViscosityLaw>,
}

#[derive(Clone, Copy, Debug)]
pub struct SolidConfig<'a> {
    pub capacity: &'a [f64],
    pub conductance_dt: &'a [f64],
    pub conduction: Option<&'a ConductionMap>,
    pub reservoir: Option<&'a ReservoirMap>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepDiagnostics {
    pub mass_error: f64,
    pub energy_error: f64,
    pub wall_heat_energy: [f64; 6],
    pub momentum_error: [f64; 3],
    pub exchange: FaceExchange,
    pub reservoir: bool,
    pub wall_event_impulse: Option<Vec<[f64; 3]>>,
    pub wall_impulse: [f64; 3],
    pub face_wall_normal_impulse: [f64; 6],
    pub solid_drag_impulse: [f64; 3],
    pub solid_dissipation_heat: f64,
    pub gas_dissipation_heat: f64,
    pub cell_solid_drag_impulse: Vec<[f64; 3]>,
    pub cell_solid_dissipation_heat: Vec<f64>,
    pub minimum_mass_population: f64,
    pub minimum_internal_population: f64,
    pub minimum_temperature: f64,
    pub maximum_temperature: f64,
    pub minimum_base_relaxation: f64,
    pub maximum_base_relaxation: f64,
    pub maximum_equilibrium_residual: f64,
    pub minimum_sensor_switch_distance: f64,
    pub positivity_limited_cells: i64,
    pub minimum_positivity_switch_distance: f64,
    pub maximum_kinetic_sensor: f64,
    pub solid: Option<SolidDiagnostics>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SolidDiagnostics {
    pub reservoir_convection_energy: f64,
    pub reservoir_radiation_energy: f64,
    pub reservoir_residual_k: f64,
    pub combined_energy_error: f64,
    pub solid_temperature_start: Vec<f64>,
    pub solid_temperature_end: Vec<f64>,
    pub combined_mass_error: f64,
    pub combined_momentum_error: [f64; 3],
    pub cell_gas_to_solid_heat: Vec<f64>,
    pub thermal_equilibrium_residual: f64,
    pub minimum_exchange_gas_temperature: f64,
    pub maximum_exchange_gas_temperature: f64,
    pub minimum_exchange_population: f64,
    pub minimum_exchange_internal_population: f64,
    pub minimum_exchange_temperature: f64,
}

fn min_of(v: &[f64]) -> f64 {
    v.iter().fold(f64::INFINITY, |a, b| a.min(*b))
}

fn max_of(v: &[f64]) -> f64 {
    v.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b))
}

struct Tape {
    relaxation: Vec<f64>,
    collided: (Vec<f64>, Vec<f64>),
    streamed: (Vec<f64>, Vec<f64>),
    heated: (Vec<f64>, Vec<f64>),
    solid_start: Option<Vec<f64>>,
    solid_heated: Option<Vec<f64>>,
    exchanged_solid: Option<Vec<f64>>,
    conducted: Option<Vec<f64>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct State {
    pub f: Vec<f64>,
    pub g: Vec<f64>,
    pub solid: Option<Vec<f64>>,
}

fn cell_moments(
    f: &[f64],
    g: &[f64],
    q: usize,
    gamma: f64,
    lattice: Lattice,
) -> Vec<(f64, [f64; 3], f64, f64)> {
    let data = lattice.data();
    f.par_chunks(q).zip(g.par_chunks(q)).map(|(a, b)| macroscopic(a, b, gamma, data)).collect()
}

fn forward(
    state: &State,
    cfg: &StepConfig<'_>,
    solid: Option<&SolidConfig<'_>>,
) -> CaeResult<(State, StepDiagnostics, Tape)> {
    let t = cfg.transport;
    let lattice = t.lattice;
    let q = t.q();
    let n = t.shape.iter().product::<usize>();
    let (f, g) = (&state.f, &state.g);
    if f.len() != n * q || g.len() != n * q {
        return Err(err("Compressible state must match the transport map"));
    }
    let moments = cell_moments(f, g, q, cfg.gamma, lattice);
    let relaxation: Vec<f64> = match cfg.viscosity {
        Some(law) => moments.iter().map(|m| law.relaxation(m.2)).collect(),
        None => vec![cfg.tau; n],
    };
    let collided: Vec<(Vec<f64>, Vec<f64>, CollisionDiagnostics)> = (0..n)
        .into_par_iter()
        .map(|c| {
            stabilized_collision(
                &f[c * q..(c + 1) * q],
                &g[c * q..(c + 1) * q],
                cfg.gamma,
                relaxation[c],
                lattice,
            )
        })
        .collect();
    let mut fc: Vec<f64> = collided.iter().flat_map(|c| c.0.iter().copied()).collect();
    let mut gc: Vec<f64> = collided.iter().flat_map(|c| c.1.iter().copied()).collect();
    let local: Vec<CollisionDiagnostics> = collided.iter().map(|c| c.2).collect();
    let minimum_collision_f = min_of(&fc);
    let minimum_collision_g = min_of(&gc);
    let collided_pair = (fc.clone(), gc.clone());
    let mut solid_impulse = [0.0; 3];
    let mut solid_heat = 0.0;
    let mut gas_heat = 0.0;
    let mut drag_residual = 0.0;
    let mut cell_impulse = vec![[0.0; 3]; n];
    let mut cell_heat = vec![0.0; n];
    if let Some(exposure) = cfg.exposure {
        let dragged: Vec<(Vec<f64>, Vec<f64>, DragTransfer)> = (0..n)
            .into_par_iter()
            .map(|c| {
                porous_drag(
                    &fc[c * q..(c + 1) * q],
                    &gc[c * q..(c + 1) * q],
                    cfg.gamma,
                    exposure[c],
                    cfg.gas_heat_fraction,
                    lattice,
                )
            })
            .collect();
        fc = dragged.iter().flat_map(|d| d.0.iter().copied()).collect();
        gc = dragged.iter().flat_map(|d| d.1.iter().copied()).collect();
        for (c, d) in dragged.iter().enumerate() {
            for k in 0..3 {
                solid_impulse[k] += d.2.solid_impulse[k];
            }
            solid_heat += d.2.solid_dissipation_heat;
            gas_heat += d.2.gas_dissipation_heat;
            drag_residual = f64::max(drag_residual, d.2.equilibrium_residual);
            cell_impulse[c] = d.2.solid_impulse;
            cell_heat[c] = d.2.solid_dissipation_heat;
        }
    }
    let streamed = stream_pair(&fc, &gc, t);
    let reservoir = matches!(t.kind, TransportKind::Reservoir(_));
    let face_wall = if reservoir { [0.0; 6] } else { box_face_impulses(&fc, t) };
    let wall = streamed.wall_impulse;
    let exchange = streamed.exchange;
    let wall_event_impulse = t.wall_events.as_ref().map(|events| wall_event_impulses(&fc, events));
    let mut out_f = streamed.f;
    let mut out_g = streamed.g;
    let streamed_pair = (out_f.clone(), out_g.clone());
    let minimum_stream_f = min_of(&out_f);
    let minimum_stream_g = min_of(&out_g);
    let mut wall_heat = [0.0; 6];
    let mut heat_residual = 0.0_f64;
    let mut stream_t: Option<Vec<f64>> = None;
    if let Some(source) = cfg.wall_heat {
        wall_heat = source.face_energy;
        if !source.indices.is_empty() {
            let pre = cell_moments(&out_f, &out_g, q, cfg.gamma, lattice);
            stream_t = Some(pre.iter().map(|m| m.2).collect());
            let mut worst = f64::NEG_INFINITY;
            for (x, e) in source.indices.iter().zip(&source.cell_energy) {
                let (a, b, r) = prescribed_heat(
                    &out_f[x * q..(x + 1) * q],
                    &out_g[x * q..(x + 1) * q],
                    *e,
                    cfg.gamma,
                    lattice,
                );
                out_f[x * q..(x + 1) * q].copy_from_slice(&a);
                out_g[x * q..(x + 1) * q].copy_from_slice(&b);
                worst = worst.max(r);
            }
            heat_residual = worst;
        }
    }
    let before = moments;
    let after = cell_moments(&out_f, &out_g, q, cfg.gamma, lattice);
    let stream_t = stream_t.unwrap_or_else(|| after.iter().map(|m| m.2).collect());
    let sum = |v: &[(f64, [f64; 3], f64, f64)], k: usize| -> f64 {
        v.iter().map(|m| if k == 0 { m.0 } else { m.3 }).sum()
    };
    let mut momentum_error = [0.0; 3];
    for d in 0..3 {
        let mut s = 0.0;
        for (a, b) in after.iter().zip(&before) {
            s += a.0 * a.1[d] - b.0 * b.1[d];
        }
        momentum_error[d] =
            s + wall[d] + solid_impulse[d] - (exchange.momentum[0][d] + exchange.momentum[1][d]);
    }
    let temps: Vec<f64> = before.iter().map(|m| m.2).collect();
    let temps1: Vec<f64> = after.iter().map(|m| m.2).collect();
    let mut diag = StepDiagnostics {
        mass_error: sum(&after, 0) - sum(&before, 0) - (exchange.mass[0] + exchange.mass[1]),
        energy_error: sum(&after, 3) - sum(&before, 3) + solid_heat
            - (exchange.energy[0] + exchange.energy[1])
            - wall_heat.iter().sum::<f64>(),
        wall_heat_energy: wall_heat,
        momentum_error,
        exchange,
        reservoir,
        wall_event_impulse,
        wall_impulse: wall,
        face_wall_normal_impulse: face_wall,
        solid_drag_impulse: solid_impulse,
        solid_dissipation_heat: solid_heat,
        gas_dissipation_heat: gas_heat,
        cell_solid_drag_impulse: cell_impulse,
        cell_solid_dissipation_heat: cell_heat.clone(),
        minimum_mass_population: minimum_stream_f.min(minimum_collision_f.min(min_of(f).min(min_of(&out_f)))),
        minimum_internal_population: minimum_stream_g
            .min(minimum_collision_g.min(min_of(g).min(min_of(&out_g)))),
        minimum_temperature: min_of(&stream_t).min(min_of(&temps).min(min_of(&temps1))),
        maximum_temperature: max_of(&stream_t).max(max_of(&temps).max(max_of(&temps1))),
        minimum_base_relaxation: min_of(&relaxation),
        maximum_base_relaxation: max_of(&relaxation),
        maximum_equilibrium_residual: heat_residual.max(
            drag_residual.max(local.iter().fold(f64::NEG_INFINITY, |a, l| a.max(l.equilibrium_residual))),
        ),
        minimum_sensor_switch_distance: local
            .iter()
            .fold(f64::INFINITY, |a, l| a.min(l.distance_to_sensor_switch)),
        positivity_limited_cells: i64::try_from(local.iter().filter(|l| l.positivity_limited).count())
            .unwrap_or(i64::MAX),
        minimum_positivity_switch_distance: local
            .iter()
            .fold(f64::INFINITY, |a, l| a.min(l.distance_to_positivity_switch)),
        maximum_kinetic_sensor: local.iter().fold(f64::NEG_INFINITY, |a, l| a.max(l.kinetic_sensor)),
        solid: None,
    };
    let heated_pair = (out_f.clone(), out_g.clone());
    let mut tape = Tape {
        relaxation,
        collided: collided_pair,
        streamed: streamed_pair,
        heated: heated_pair,
        solid_start: None,
        solid_heated: None,
        exchanged_solid: None,
        conducted: None,
    };
    let Some(solid) = solid else {
        return Ok((State { f: out_f, g: out_g, solid: None }, diag, tape));
    };
    let ts = state.solid.as_ref().ok_or_else(|| err("Coupled thermal step requires a solid temperature"))?;
    let heated: Vec<f64> = (0..n).map(|x| ts[x] + cell_heat[x] / solid.capacity[x]).collect();
    let exchanged: Vec<_> = (0..n)
        .into_par_iter()
        .map(|x| {
            gas_solid_exchange(
                &out_f[x * q..(x + 1) * q],
                &out_g[x * q..(x + 1) * q],
                heated[x],
                solid.capacity[x],
                solid.conductance_dt[x],
                cfg.gamma,
                lattice,
            )
        })
        .collect();
    let a: Vec<f64> = exchanged.iter().flat_map(|e| e.0.iter().copied()).collect();
    let b: Vec<f64> = exchanged.iter().flat_map(|e| e.1.iter().copied()).collect();
    let exchanged_solid: Vec<f64> = exchanged.iter().map(|e| e.2).collect();
    let mut end = exchanged_solid.clone();
    let mut conducted = None;
    if let Some(c) = solid.conduction {
        end = c.advance(&end, solid.capacity);
        conducted = Some(end.clone());
    }
    let (mut conv, mut rad, mut res) = (0.0, 0.0, 0.0);
    if let Some(r) = solid.reservoir {
        let step = r.advance(&end, solid.capacity);
        end = step.temperature;
        conv = step.convection_energy;
        rad = step.radiation_energy;
        res = step.residual_k;
    }
    let data = lattice.data();
    let mut gas_change = 0.0;
    let mut mass_change = 0.0;
    let mut momentum_change = [0.0; 3];
    for i in 0..a.len() {
        let vq = i % q;
        let df = a[i] - out_f[i];
        gas_change += df * data.speed2[vq] + b[i] - out_g[i];
        mass_change += df;
        for d in 0..3 {
            momentum_change[d] += df * f64::from(data.velocities[vq][d]);
        }
    }
    let gas_change = 0.5 * gas_change;
    let solid_change: f64 = (0..n).map(|x| solid.capacity[x] * (end[x] - ts[x])).sum();
    let gas_temps: Vec<f64> = exchanged.iter().map(|e| e.3.gas_temperature).collect();
    diag.solid = Some(SolidDiagnostics {
        reservoir_convection_energy: conv,
        reservoir_radiation_energy: rad,
        reservoir_residual_k: res,
        combined_energy_error: diag.energy_error + gas_change + solid_change
            - cell_heat.iter().sum::<f64>()
            - conv
            - rad,
        solid_temperature_start: ts.clone(),
        solid_temperature_end: end.clone(),
        combined_mass_error: diag.mass_error + mass_change,
        combined_momentum_error: std::array::from_fn(|d| diag.momentum_error[d] + momentum_change[d]),
        cell_gas_to_solid_heat: exchanged.iter().map(|e| e.3.heat_to_solid).collect(),
        thermal_equilibrium_residual: exchanged
            .iter()
            .fold(f64::NEG_INFINITY, |m, e| m.max(e.3.equilibrium_residual)),
        minimum_exchange_gas_temperature: min_of(&gas_temps),
        maximum_exchange_gas_temperature: max_of(&gas_temps),
        minimum_exchange_population: min_of(&a),
        minimum_exchange_internal_population: min_of(&b),
        minimum_exchange_temperature: min_of(&gas_temps).min(min_of(&end)),
    });
    tape.solid_start = Some(ts.clone());
    tape.solid_heated = Some(heated);
    tape.exchanged_solid = Some(exchanged_solid);
    tape.conducted = conducted;
    Ok((State { f: a, g: b, solid: Some(end) }, diag, tape))
}


pub fn step(
    state: &State,
    cfg: &StepConfig<'_>,
    solid: Option<&SolidConfig<'_>>,
) -> CaeResult<(State, StepDiagnostics)> {
    let (s, d, _) = forward(state, cfg, solid)?;
    Ok((s, d))
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StageBar {
    pub exchange: Option<FaceExchange>,
    pub cell_impulse: Option<Vec<[f64; 3]>>,
    pub wall_event_impulse: Option<Vec<[f64; 3]>>,
    pub reservoir_energy: [f64; 2],
}


pub fn step_vjp(
    state: &State,
    cfg: &StepConfig<'_>,
    solid: Option<&SolidConfig<'_>>,
    out_bar: &State,
    stage: &StageBar,
) -> CaeResult<(State, Vec<f64>)> {
    let (_, _, tape) = forward(state, cfg, solid)?;
    let t = cfg.transport;
    let lattice = t.lattice;
    let q = t.q();
    let n = t.shape.iter().product::<usize>();
    let mut f_bar = out_bar.f.clone();
    let mut g_bar = out_bar.g.clone();
    let mut ts_bar: Option<Vec<f64>> = None;
    let mut cell_heat_bar = vec![0.0; n];
    if let Some(solid) = solid {
        let (ts, heated, exchanged) = (
            tape.solid_start.as_ref().ok_or_else(|| err("missing solid tape"))?,
            tape.solid_heated.as_ref().ok_or_else(|| err("missing solid tape"))?,
            tape.exchanged_solid.as_ref().ok_or_else(|| err("missing solid tape"))?,
        );
        let mut end_bar = out_bar.solid.clone().unwrap_or_else(|| vec![0.0; n]);
        if let Some(r) = solid.reservoir {
            let start = tape.conducted.clone().unwrap_or_else(|| exchanged.clone());
            let end = r.advance(&start, solid.capacity).temperature;
            end_bar = r.advance_vjp(&start, solid.capacity, &end, &end_bar, stage.reservoir_energy);
        }
        if let Some(c) = solid.conduction {
            end_bar = c.advance_vjp(solid.capacity, &end_bar);
        }
        let (hf, hg) = &tape.heated;
        let results: Vec<(Vec<f64>, Vec<f64>, f64)> = (0..n)
            .into_par_iter()
            .map(|x| {
                gas_solid_exchange_vjp(
                    &hf[x * q..(x + 1) * q],
                    &hg[x * q..(x + 1) * q],
                    heated[x],
                    solid.capacity[x],
                    solid.conductance_dt[x],
                    cfg.gamma,
                    lattice,
                    &f_bar[x * q..(x + 1) * q],
                    &g_bar[x * q..(x + 1) * q],
                    end_bar[x],
                )
            })
            .collect();
        let mut tsb = vec![0.0; n];
        for (x, (fb, gb, hb)) in results.into_iter().enumerate() {
            f_bar[x * q..(x + 1) * q].copy_from_slice(&fb);
            g_bar[x * q..(x + 1) * q].copy_from_slice(&gb);
            tsb[x] = hb;
            cell_heat_bar[x] = hb / solid.capacity[x];
        }
        debug_assert_eq!(ts.len(), n);
        ts_bar = Some(tsb);
    }

    if let Some(source) = cfg.wall_heat {
        let (sf, sg) = &tape.streamed;
        for (x, e) in source.indices.iter().zip(&source.cell_energy) {
            let (fb, gb) = prescribed_heat_vjp(
                &sf[x * q..(x + 1) * q],
                &sg[x * q..(x + 1) * q],
                *e,
                cfg.gamma,
                lattice,
                &f_bar[x * q..(x + 1) * q],
                &g_bar[x * q..(x + 1) * q],
            );
            f_bar[x * q..(x + 1) * q].copy_from_slice(&fb);
            g_bar[x * q..(x + 1) * q].copy_from_slice(&gb);
        }
    }

    let (mut fb, mut gb) = stream_pair_vjp(t, &f_bar, &g_bar, stage.exchange.as_ref());
    if let (Some(events), Some(bar)) = (t.wall_events.as_ref(), stage.wall_event_impulse.as_ref()) {
        wall_event_impulses_vjp(events, bar, &mut fb);
    }

    let mut exposure_bar = vec![0.0; n];
    if let Some(exposure) = cfg.exposure {
        let (cf, cg) = &tape.collided;
        let impulse_bar = stage.cell_impulse.clone().unwrap_or_else(|| vec![[0.0; 3]; n]);
        let results: Vec<(Vec<f64>, Vec<f64>, f64)> = (0..n)
            .into_par_iter()
            .map(|x| {
                porous_drag_vjp(
                    &cf[x * q..(x + 1) * q],
                    &cg[x * q..(x + 1) * q],
                    cfg.gamma,
                    exposure[x],
                    cfg.gas_heat_fraction,
                    lattice,
                    &fb[x * q..(x + 1) * q],
                    &gb[x * q..(x + 1) * q],
                    impulse_bar[x],
                    cell_heat_bar[x],
                )
            })
            .collect();
        for (x, (a, b, e)) in results.into_iter().enumerate() {
            fb[x * q..(x + 1) * q].copy_from_slice(&a);
            gb[x * q..(x + 1) * q].copy_from_slice(&b);
            exposure_bar[x] = e;
        }
    }

    let (f, g) = (&state.f, &state.g);
    let results: Vec<(Vec<f64>, Vec<f64>, f64)> = (0..n)
        .into_par_iter()
        .map(|x| {
            stabilized_collision_vjp(
                &f[x * q..(x + 1) * q],
                &g[x * q..(x + 1) * q],
                cfg.gamma,
                tape.relaxation[x],
                lattice,
                &fb[x * q..(x + 1) * q],
                &gb[x * q..(x + 1) * q],
            )
        })
        .collect();
    let data = lattice.data();
    let mut f_in = vec![0.0; n * q];
    let mut g_in = vec![0.0; n * q];
    for (x, (a, b, tau_bar)) in results.into_iter().enumerate() {
        f_in[x * q..(x + 1) * q].copy_from_slice(&a);
        g_in[x * q..(x + 1) * q].copy_from_slice(&b);
        if let Some(law) = cfg.viscosity
            && tau_bar != 0.0
        {
            let (_, _, temperature, _) =
                macroscopic(&f[x * q..(x + 1) * q], &g[x * q..(x + 1) * q], cfg.gamma, data);
            let d = law.relaxation(implexity_ad::Dual::<1>::variable(temperature, 0)).eps[0];
            macroscopic_vjp(
                &f[x * q..(x + 1) * q],
                &g[x * q..(x + 1) * q],
                cfg.gamma,
                data,
                (0.0, [0.0; 3], tau_bar * d, 0.0),
                &mut f_in[x * q..(x + 1) * q],
                &mut g_in[x * q..(x + 1) * q],
            );
        }
    }
    Ok((State { f: f_in, g: g_in, solid: ts_bar }, exposure_bar))
}

fn nested(values: &[f64], shape: &[usize]) -> Value {
    crate::nparray::to_nested(values, shape)
}

impl StepDiagnostics {
    #[must_use]
    pub fn to_map(&self, shape: [usize; 3]) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("mass_error".into(), json!(self.mass_error));
        m.insert("energy_error".into(), json!(self.energy_error));
        m.insert("wall_heat_energy".into(), json!(self.wall_heat_energy));
        m.insert("momentum_error".into(), json!(self.momentum_error));
        m.insert("face_mass_exchange".into(), json!(self.exchange.mass));
        m.insert("face_momentum_exchange".into(), json!(self.exchange.momentum));
        m.insert("face_energy_exchange".into(), json!(self.exchange.energy));
        if let Some(events) = &self.wall_event_impulse {
            m.insert("wall_event_impulse".into(), json!(events));
        }
        m.insert("wall_impulse".into(), json!(self.wall_impulse));
        m.insert("face_wall_normal_impulse".into(), json!(self.face_wall_normal_impulse));
        m.insert("solid_drag_impulse".into(), json!(self.solid_drag_impulse));
        m.insert("solid_dissipation_heat".into(), json!(self.solid_dissipation_heat));
        m.insert("gas_dissipation_heat".into(), json!(self.gas_dissipation_heat));
        let flat: Vec<f64> = self.cell_solid_drag_impulse.iter().flatten().copied().collect();
        m.insert("cell_solid_drag_impulse".into(), nested(&flat, &[shape[0], shape[1], shape[2], 3]));
        m.insert("cell_solid_dissipation_heat".into(), nested(&self.cell_solid_dissipation_heat, &shape));
        m.insert("minimum_mass_population".into(), json!(self.minimum_mass_population));
        m.insert("minimum_internal_population".into(), json!(self.minimum_internal_population));
        m.insert("minimum_temperature".into(), json!(self.minimum_temperature));
        m.insert("maximum_temperature".into(), json!(self.maximum_temperature));
        m.insert("minimum_base_relaxation".into(), json!(self.minimum_base_relaxation));
        m.insert("maximum_base_relaxation".into(), json!(self.maximum_base_relaxation));
        m.insert("maximum_equilibrium_residual".into(), json!(self.maximum_equilibrium_residual));
        m.insert("minimum_sensor_switch_distance".into(), json!(self.minimum_sensor_switch_distance));
        m.insert("positivity_limited_cells".into(), json!(self.positivity_limited_cells));
        m.insert("minimum_positivity_switch_distance".into(), json!(self.minimum_positivity_switch_distance));
        m.insert("maximum_kinetic_sensor".into(), json!(self.maximum_kinetic_sensor));
        if let Some(s) = &self.solid {
            m.insert("solid_reservoir_convection_energy".into(), json!(s.reservoir_convection_energy));
            m.insert("solid_reservoir_radiation_energy".into(), json!(s.reservoir_radiation_energy));
            m.insert("solid_reservoir_residual_K".into(), json!(s.reservoir_residual_k));
            m.insert("combined_energy_error".into(), json!(s.combined_energy_error));
            m.insert("solid_temperature_start".into(), nested(&s.solid_temperature_start, &shape));
            m.insert("solid_temperature_end".into(), nested(&s.solid_temperature_end, &shape));
            m.insert("combined_mass_error".into(), json!(s.combined_mass_error));
            m.insert("combined_momentum_error".into(), json!(s.combined_momentum_error));
            m.insert("cell_gas_to_solid_heat".into(), nested(&s.cell_gas_to_solid_heat, &shape));
            m.insert("thermal_equilibrium_residual".into(), json!(s.thermal_equilibrium_residual));
            m.insert("minimum_exchange_gas_temperature".into(), json!(s.minimum_exchange_gas_temperature));
            m.insert("maximum_exchange_gas_temperature".into(), json!(s.maximum_exchange_gas_temperature));
            m.insert("minimum_exchange_population".into(), json!(s.minimum_exchange_population));
            m.insert(
                "minimum_exchange_internal_population".into(),
                json!(s.minimum_exchange_internal_population),
            );
            m.insert("minimum_exchange_temperature".into(), json!(s.minimum_exchange_temperature));
        }
        m
    }
}
