// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use super::equilibrium::{Lattice, macroscopic};
use super::kernels::macroscopic_vjp;
use super::solid::{ConductionMap, ReservoirMap, ViscosityLaw, WallFlux};
use super::step::{SolidConfig, StageBar, State, StepConfig, StepDiagnostics, step, step_vjp};
use super::transport::{FaceExchange, Transport, TransportKind};

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SolidModel {
    pub temperature: Vec<f64>,
    pub capacity: Vec<f64>,
    pub conductance_dt: Vec<f64>,
    pub gas_heat_fraction: f64,
    pub conduction: Option<ConductionMap>,
    pub reservoir: Option<ReservoirMap>,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub gamma: f64,
    pub tau: f64,
    pub transport: Transport,
    pub wall_heat: WallFlux,
    pub viscosity: Option<ViscosityLaw>,
    pub solid: Option<SolidModel>,
    pub steps: usize,
}

impl Model {
    fn configs<'a>(&'a self, exposure: &'a [f64]) -> (StepConfig<'a>, Option<SolidConfig<'a>>) {
        let fraction = self.solid.as_ref().map_or(1.0, |s| s.gas_heat_fraction);
        let cfg = StepConfig {
            gamma: self.gamma,
            tau: self.tau,
            transport: &self.transport,
            exposure: Some(exposure),
            gas_heat_fraction: fraction,
            wall_heat: Some(&self.wall_heat),
            viscosity: self.viscosity.as_ref(),
        };
        let solid = self.solid.as_ref().map(|s| SolidConfig {
            capacity: &s.capacity,
            conductance_dt: &s.conductance_dt,
            conduction: s.conduction.as_ref(),
            reservoir: s.reservoir.as_ref(),
        });
        (cfg, solid)
    }

    fn check_inputs(&self, state: &State, exposure: &[f64]) -> CaeResult<()> {
        let (gamma, tau) = (self.gamma, self.tau);
        if !gamma.is_finite() || !tau.is_finite() {
            return Err(err("Finite scalar gas and relaxation parameters required"));
        }
        if !(1.0 < gamma && gamma <= 5.0 / 3.0) || !(0.5 < tau && tau <= 1.0 / 1.35) {
            return Err(err("Require 1 < gamma <= 5/3 and 0.5 < tau <= 1/1.35"));
        }
        let t = &self.transport;
        if let TransportKind::Reservoir(r) = &t.kind {
            if r.gamma != gamma {
                return Err(err("Reservoir and interior gas heat-capacity ratios must match"));
            }
            for (name, rows) in [("reservoir_f", &r.reservoir_f), ("reservoir_g", &r.reservoir_g)] {
                if rows
                    .iter()
                    .flatten()
                    .any(|v| !v.is_finite() || *v < 0.0 || (name == "reservoir_f" && *v == 0.0))
                {
                    return Err(err(
                        "Finite positive mass and nonnegative internal reservoir populations required",
                    ));
                }
            }
            if gamma == 5.0 / 3.0 && r.reservoir_g.iter().flatten().any(|v| *v != 0.0) {
                return Err(err("Monatomic reservoir cannot contain internal-mode energy"));
            }
        }
        let n = t.len();
        for (name, a) in [("f", &state.f), ("g", &state.g)] {
            if a.len() != n || a.iter().any(|v| !v.is_finite() || *v < 0.0 || (name == "f" && *v == 0.0)) {
                return Err(err("Finite positive mass and nonnegative internal populations required"));
            }
        }
        if gamma == 5.0 / 3.0 && state.g.iter().any(|v| *v != 0.0) {
            return Err(err("Monatomic gas cannot contain internal-mode energy"));
        }
        let fraction = self.solid.as_ref().map_or(1.0, |s| s.gas_heat_fraction);
        if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
            return Err(err("Gas dissipation heat fraction must lie in [0,1]"));
        }
        if exposure.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err(err("Finite nonnegative porous drag exposure required per cell"));
        }
        Ok(())
    }

    fn check_flow(
        &self,
        state: &State,
        d: &StepDiagnostics,
        next: &State,
        require_sensitivity: bool,
    ) -> CaeResult<()> {
        let values: Vec<f64> = next.f.iter().chain(&next.g).copied().collect();
        let diag_values = diagnostics_values(d);
        if !values.iter().chain(&diag_values).all(|v| f64::is_finite(*v)) {
            return Err(err("Nonfinite compressible step or unconverged equilibrium"));
        }
        if d.minimum_mass_population <= 0.0
            || d.minimum_internal_population < 0.0
            || d.minimum_temperature <= 0.0
            || d.maximum_equilibrium_residual > 1e-10
        {
            return Err(err("Compressible step lost admissibility or equilibrium convergence"));
        }
        if let Some(law) = &self.viscosity {
            law.admit_temperature(d.minimum_temperature, d.maximum_temperature)?;
            if d.minimum_base_relaxation <= 0.5 || d.maximum_base_relaxation > 1.0 / 1.35 {
                return Err(err("Temperature-dependent relaxation is outside its admitted range"));
            }
        }
        let data = self.transport.lattice.data();
        let q = data.q();
        let mass: f64 = state.f.iter().sum();
        let mut energy = 0.0;
        let mut momentum_scale = 0.0;
        for (i, v) in state.f.iter().enumerate() {
            energy += v * data.speed2[i % q] + state.g[i];
            let c = data.velocities[i % q];
            momentum_scale +=
                v * f64::from(c[0].abs()) + v * f64::from(c[1].abs()) + v * f64::from(c[2].abs());
        }
        let energy = 0.5 * energy;
        let worst_momentum = d.momentum_error.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        if d.mass_error.abs() > 1e-9 * mass
            || d.energy_error.abs() > 1e-9 * energy
            || worst_momentum > 1e-9 * momentum_scale.max(mass)
        {
            return Err(err("Compressible step failed its conservation ledger"));
        }
        if require_sensitivity && d.minimum_sensor_switch_distance <= 1e-8 {
            return Err(err(
                "Kinetic sensor or positivity active-set switch prevents branch-local sensitivity admission",
            ));
        }
        Ok(())
    }

    fn check_solid_inputs(&self, state: &State) -> CaeResult<()> {
        let Some(s) = &self.solid else { return Ok(()) };
        let ts = state.solid.as_deref().unwrap_or_default();
        for (name, a) in
            [("temperature", ts), ("capacity", &s.capacity[..]), ("exchange", &s.conductance_dt[..])]
        {
            if a.len() != s.capacity.len()
                || a.iter().any(|v| !v.is_finite() || *v < 0.0 || (name != "exchange" && *v == 0.0))
            {
                return Err(err(
                    "Positive cellwise solid temperature/capacity and nonnegative exchange required",
                ));
            }
        }
        if let Some(c) = &s.conduction {
            super::solid::validate(c, &s.capacity)?;
        }
        Ok(())
    }

    fn check_solid(&self, state: &State, d: &StepDiagnostics, next: &State) -> CaeResult<()> {
        let (Some(s), Some(sd)) = (&self.solid, &d.solid) else { return Ok(()) };
        if sd.reservoir_residual_k > 1e-8 {
            return Err(err("Solid reservoir implicit temperature balance failed"));
        }
        let finite =
            next.f.iter().chain(&next.g).chain(next.solid.iter().flatten()).all(|v| f64::is_finite(*v))
                && diagnostics_values(d).iter().all(|v| f64::is_finite(*v));
        if !finite
            || sd.minimum_exchange_population <= 0.0
            || sd.minimum_exchange_internal_population < 0.0
            || sd.minimum_exchange_temperature <= 0.0
            || sd.thermal_equilibrium_residual > 1e-10
        {
            return Err(err("Coupled thermal step lost admissibility or equilibrium convergence"));
        }
        if let Some(law) = &self.viscosity {
            law.admit_temperature(sd.minimum_exchange_gas_temperature, sd.maximum_exchange_gas_temperature)?;
        }
        let data = self.transport.lattice.data();
        let q = data.q();
        let ts = state.solid.as_deref().unwrap_or_default();
        let mut energy = 0.0;
        let mut momentum_scale = 0.0;
        for (i, v) in state.f.iter().enumerate() {
            energy += v * data.speed2[i % q] + state.g[i];
            let c = data.velocities[i % q];
            momentum_scale +=
                v * f64::from(c[0].abs()) + v * f64::from(c[1].abs()) + v * f64::from(c[2].abs());
        }
        let energy = 0.5 * energy + s.capacity.iter().zip(ts).map(|(c, t)| c * t).sum::<f64>();
        if sd.combined_energy_error.abs() > 1e-9 * energy {
            return Err(err("Combined gas/solid energy ledger failed"));
        }
        let mass: f64 = state.f.iter().sum();
        let scale = mass.max(momentum_scale);
        if sd.combined_mass_error.abs() > 1e-9 * mass
            || sd.combined_momentum_error.iter().fold(0.0_f64, |m, v| m.max(v.abs())) > 1e-9 * scale
        {
            return Err(err("Thermal exchange mass or momentum ledger failed"));
        }
        Ok(())
    }


    pub fn checked_history(
        &self,
        initial: &State,
        exposure: &[f64],
        require_sensitivity: bool,
    ) -> CaeResult<History> {
        if self.steps < 1 {
            return Err(err("A positive fixed step count is required"));
        }
        let (cfg, solid) = self.configs(exposure);
        let mut states = vec![initial.clone()];
        let mut diagnostics = Vec::with_capacity(self.steps);
        for index in 0..self.steps {
            let state = &states[index];
            let run = || -> CaeResult<(State, StepDiagnostics)> {
                self.check_solid_inputs(state)?;
                let inner = || -> CaeResult<(State, StepDiagnostics)> {
                    self.check_inputs(state, exposure)?;
                    let (next, d) = step(state, &cfg, solid.as_ref())?;
                    self.check_flow(state, &d, &next, require_sensitivity)?;
                    Ok((next, d))
                };
                let (next, d) = if self.solid.is_some() {
                    inner()?
                } else {
                    inner().map_err(|e| {
                        err(format!("Compressible history rejected interval {index}: {}", e.message()))
                    })?
                };
                self.check_solid(state, &d, &next)?;
                Ok((next, d))
            };
            let (next, d) = if self.solid.is_some() {
                run().map_err(|e| err(format!("Thermal history interval {index}: {}", e.message())))?
            } else {
                run()?
            };
            states.push(next);
            diagnostics.push(d);
        }
        Ok(History { states, diagnostics })
    }
}

fn diagnostics_values(d: &StepDiagnostics) -> Vec<f64> {
    let mut v = vec![
        d.mass_error,
        d.energy_error,
        d.solid_dissipation_heat,
        d.gas_dissipation_heat,
        d.minimum_mass_population,
        d.minimum_internal_population,
        d.minimum_temperature,
        d.maximum_temperature,
        d.minimum_base_relaxation,
        d.maximum_base_relaxation,
        d.maximum_equilibrium_residual,
        d.minimum_sensor_switch_distance,
        d.minimum_positivity_switch_distance,
        d.maximum_kinetic_sensor,
    ];
    v.extend(d.wall_heat_energy);
    v.extend(d.momentum_error);
    v.extend(d.exchange.mass);
    v.extend(d.exchange.momentum.iter().flatten());
    v.extend(d.exchange.energy);
    v.extend(d.wall_impulse);
    v.extend(d.face_wall_normal_impulse);
    v.extend(d.solid_drag_impulse);
    v.extend(d.cell_solid_drag_impulse.iter().flatten());
    v.extend(&d.cell_solid_dissipation_heat);
    if let Some(e) = &d.wall_event_impulse {
        v.extend(e.iter().flatten());
    }
    if let Some(s) = &d.solid {
        v.extend([
            s.reservoir_convection_energy,
            s.reservoir_radiation_energy,
            s.reservoir_residual_k,
            s.combined_energy_error,
            s.combined_mass_error,
            s.thermal_equilibrium_residual,
            s.minimum_exchange_gas_temperature,
            s.maximum_exchange_gas_temperature,
            s.minimum_exchange_population,
            s.minimum_exchange_internal_population,
            s.minimum_exchange_temperature,
        ]);
        v.extend(s.combined_momentum_error);
        v.extend(&s.solid_temperature_start);
        v.extend(&s.solid_temperature_end);
        v.extend(&s.cell_gas_to_solid_heat);
    }
    v
}

#[derive(Clone, Debug)]
pub struct History {
    pub states: Vec<State>,
    pub diagnostics: Vec<StepDiagnostics>,
}

impl History {
    #[must_use]
    pub fn stacked(&self, shape: [usize; 3]) -> Map<String, Value> {
        let rows: Vec<Map<String, Value>> = self.diagnostics.iter().map(|d| d.to_map(shape)).collect();
        let mut out = Map::new();
        if let Some(first) = rows.first() {
            for key in first.keys() {
                out.insert(key.clone(), Value::Array(rows.iter().map(|r| r[key].clone()).collect()));
            }
        }
        out
    }

    #[must_use]
    pub fn solid_temperature_history(&self, unit: f64) -> Vec<Vec<f64>> {
        let mut out = Vec::new();
        if let Some(s) = self.diagnostics.first().and_then(|d| d.solid.as_ref()) {
            out.push(s.solid_temperature_start.iter().map(|v| v * unit).collect());
        }
        for d in &self.diagnostics {
            if let Some(s) = &d.solid {
                out.push(s.solid_temperature_end.iter().map(|v| v * unit).collect());
            }
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Units {
    pub cell_volume_m3: f64,
    pub velocity_unit_m_s: f64,
    pub density_unit_kg_m3: f64,
    pub temperature_unit_k: f64,
    pub pressure_unit_pa: f64,
    pub energy_unit_j: f64,
}

impl Units {

    pub fn new(spacing: f64, step: f64, density: f64, gas_constant: f64) -> CaeResult<Self> {
        if [spacing, step, density, gas_constant].iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(err("Positive finite lattice scales and specific gas constant required"));
        }
        let velocity = spacing / step;
        Ok(Self {
            cell_volume_m3: spacing.powi(3),
            velocity_unit_m_s: velocity,
            density_unit_kg_m3: density,
            temperature_unit_k: velocity * velocity / gas_constant,
            pressure_unit_pa: density * velocity * velocity,
            energy_unit_j: density * spacing.powi(3) * velocity * velocity,
        })
    }
}

pub const GAS_RESPONSES: [&str; 9] = [
    "gas_mass_kg",
    "kinetic_energy_J",
    "internal_energy_J",
    "mean_velocity_x_m_s",
    "mean_velocity_y_m_s",
    "mean_velocity_z_m_s",
    "mean_temperature_K",
    "mean_pressure_Pa",
    "porous_fluid_volume_m3",
];

fn gas_terms<S: Scalar>(rho: S, u: [S; 3], t: S, e: S, units: &Units, cells: f64) -> [S; 8] {
    let rho_p = rho * units.density_unit_kg_m3;
    let up = u.map(|v| v * units.velocity_unit_m_s);
    let usq = up[0] * up[0] + up[1] * up[1] + up[2] * up[2];
    let kinetic = rho_p * usq;
    [
        rho_p,
        kinetic,
        e * units.pressure_unit_pa,
        up[0] / cells,
        up[1] / cells,
        up[2] / cells,
        t * units.temperature_unit_k / cells,
        rho * t * units.pressure_unit_pa / cells,
    ]
}

#[must_use]
pub fn gas_responses(
    f: &[f64],
    g: &[f64],
    phi: &[f64],
    gamma: f64,
    lattice: Lattice,
    units: &Units,
) -> [f64; 9] {
    let data = lattice.data();
    let q = data.q();
    let n = phi.len();
    let mut s = [0.0; 8];
    for c in 0..n {
        let (rho, u, t, e) = macroscopic(&f[c * q..(c + 1) * q], &g[c * q..(c + 1) * q], gamma, data);
        let terms = gas_terms(rho, u, t, e, units, n as f64);
        for k in 0..8 {
            s[k] += terms[k];
        }
    }
    let volume = units.cell_volume_m3;
    let kinetic = 0.5 * s[1] * volume;
    [
        s[0] * volume,
        kinetic,
        s[2] * volume - kinetic,
        s[3],
        s[4],
        s[5],
        s[6],
        s[7],
        phi.iter().sum::<f64>() * volume,
    ]
}

#[must_use]
pub fn gas_responses_vjp(
    f: &[f64],
    g: &[f64],
    phi: &[f64],
    gamma: f64,
    lattice: Lattice,
    units: &Units,
    w: &[f64; 9],
) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let data = lattice.data();
    let q = data.q();
    let n = phi.len();
    let volume = units.cell_volume_m3;

    let ws = [w[0] * volume, 0.5 * volume * (w[1] - w[2]), w[2] * volume, w[3], w[4], w[5], w[6], w[7]];
    let mut f_bar = vec![0.0; f.len()];
    let mut g_bar = vec![0.0; g.len()];
    for c in 0..n {
        let (fc, gc) = (&f[c * q..(c + 1) * q], &g[c * q..(c + 1) * q]);
        let (rho, u, t, e) = macroscopic(fc, gc, gamma, data);
        let s: [Dual<6>; 6] = std::array::from_fn(|k| Dual::variable([rho, u[0], u[1], u[2], t, e][k], k));
        let terms = gas_terms(s[0], [s[1], s[2], s[3]], s[4], s[5], units, n as f64);
        let mut bar = [0.0; 6];
        for (term, weight) in terms.iter().zip(ws) {
            for k in 0..6 {
                bar[k] += term.eps[k] * weight;
            }
        }
        macroscopic_vjp(
            fc,
            gc,
            gamma,
            data,
            (bar[0], [bar[1], bar[2], bar[3]], bar[4], bar[5]),
            &mut f_bar[c * q..(c + 1) * q],
            &mut g_bar[c * q..(c + 1) * q],
        );
    }
    (f_bar, g_bar, vec![w[8] * volume; n])
}

#[must_use]
pub fn fields(f: &[f64], g: &[f64], gamma: f64, lattice: Lattice, units: &Units) -> [Vec<f64>; 5] {
    let data = lattice.data();
    let q = data.q();
    let n = f.len() / q;
    let mut out: [Vec<f64>; 5] = std::array::from_fn(|_| Vec::with_capacity(n));
    for c in 0..n {
        let (rho, u, t, e) = macroscopic(&f[c * q..(c + 1) * q], &g[c * q..(c + 1) * q], gamma, data);
        out[0].push(rho * units.density_unit_kg_m3);
        out[1].extend(u.map(|v| v * units.velocity_unit_m_s));
        out[2].push(t * units.temperature_unit_k);
        out[3].push(rho * t * units.pressure_unit_pa);
        out[4].push(e * units.pressure_unit_pa);
    }
    out
}

pub const PORT_RESPONSES: [&str; 10] = [
    "xmin_mean_outward_mass_flow_kg_s",
    "xmin_mean_outward_total_energy_W",
    "xmin_mean_outward_momentum_x_N",
    "xmin_mean_outward_momentum_y_N",
    "xmin_mean_outward_momentum_z_N",
    "xmax_mean_outward_mass_flow_kg_s",
    "xmax_mean_outward_total_energy_W",
    "xmax_mean_outward_momentum_x_N",
    "xmax_mean_outward_momentum_y_N",
    "xmax_mean_outward_momentum_z_N",
];

fn port_scales(units: &Units, step: f64) -> (f64, f64, f64) {
    let mass_unit = units.density_unit_kg_m3 * units.cell_volume_m3;
    (-mass_unit / step, -units.energy_unit_j / step, -mass_unit * units.velocity_unit_m_s / step)
}

#[must_use]
pub fn port_responses(history: &History, units: &Units, step: f64) -> [f64; 10] {
    let n = history.diagnostics.len() as f64;
    let mut mean = FaceExchange::default();
    for d in &history.diagnostics {
        for k in 0..2 {
            mean.mass[k] += d.exchange.mass[k];
            mean.energy[k] += d.exchange.energy[k];
            for a in 0..3 {
                mean.momentum[k][a] += d.exchange.momentum[k][a];
            }
        }
    }
    let (ms, es, ps) = port_scales(units, step);
    let mut out = [0.0; 10];
    for k in 0..2 {
        out[5 * k] = mean.mass[k] / n * ms;
        out[5 * k + 1] = mean.energy[k] / n * es;
        for a in 0..3 {
            out[5 * k + 2 + a] = mean.momentum[k][a] / n * ps;
        }
    }
    out
}

#[must_use]
pub fn port_exchange_bar(w: &[f64; 10], units: &Units, step: f64, steps: usize) -> FaceExchange {
    let n = steps as f64;
    let (ms, es, ps) = port_scales(units, step);
    let mut e = FaceExchange::default();
    for k in 0..2 {
        e.mass[k] = w[5 * k] * ms / n;
        e.energy[k] = w[5 * k + 1] * es / n;
        for a in 0..3 {
            e.momentum[k][a] = w[5 * k + 2 + a] * ps / n;
        }
    }
    e
}

#[derive(Clone, Debug)]
pub struct Cotangents {
    pub terminal: State,
    pub phi: Vec<f64>,
    pub stages: Vec<StageBar>,
    pub solid_temperature: Vec<Vec<f64>>,
}

fn add_solid(bar: &mut State, extra: Option<&Vec<f64>>) {
    if let (Some(solid), Some(e)) = (bar.solid.as_mut(), extra.filter(|e| !e.is_empty())) {
        for (a, b) in solid.iter_mut().zip(e) {
            *a += b;
        }
    }
}

impl Model {

    pub fn adjoint(&self, history: &History, exposure: &[f64], cot: &Cotangents) -> CaeResult<Vec<f64>> {
        let (cfg, solid) = self.configs(exposure);
        let mut bar = cot.terminal.clone();
        add_solid(&mut bar, cot.solid_temperature.get(self.steps));
        let mut exposure_bar = vec![0.0; exposure.len()];
        for t in (0..self.steps).rev() {
            let (b, e) = step_vjp(&history.states[t], &cfg, solid.as_ref(), &bar, &cot.stages[t])?;
            for (acc, v) in exposure_bar.iter_mut().zip(&e) {
                *acc += v;
            }
            bar = b;
            add_solid(&mut bar, cot.solid_temperature.get(t));
        }
        Ok(exposure_bar)
    }
}


pub fn initial_state(
    problem: &Map<String, Value>,
    shape: [usize; 3],
    units: &Units,
    gamma: f64,
    lattice: Lattice,
) -> CaeResult<(State, Vec<f64>)> {
    use crate::nparray::asarray;
    let n: usize = shape.iter().product();
    let source = problem.get("initial_fields").cloned().unwrap_or(Value::Null);
    let (source, spatial) = if source.is_null() {
        (
            json!({
                "density_kg_m3": problem.get("initial_density_kg_m3").cloned().unwrap_or(Value::Null),
                "temperature_K": problem.get("initial_temperature_K").cloned().unwrap_or(Value::Null),
                "velocity_m_s": problem.get("initial_velocity_m_s").cloned().unwrap_or(Value::Null),
            }),
            false,
        )
    } else {
        if ["initial_density_kg_m3", "initial_temperature_K", "initial_velocity_m_s"]
            .iter()
            .any(|k| problem.get(*k).is_some_and(|v| !v.is_null()))
        {
            return Err(err("Spatial initial fields require all three uniform initial values to be null"));
        }
        (source, true)
    };
    let keys = ["density_kg_m3", "temperature_K", "velocity_m_s"];
    let Some(m) = source.as_object().filter(|m| m.len() == 3 && keys.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(err("Initial fields require density_kg_m3, temperature_K and velocity_m_s"));
    };
    let mut arrays = Vec::new();
    for key in keys {
        let a = asarray(&m[key]);
        let expected: Vec<usize> = match (spatial, key) {
            (false, "velocity_m_s") => vec![3],
            (false, _) => vec![],
            (true, "velocity_m_s") => vec![shape[0], shape[1], shape[2], 3],
            (true, _) => shape.to_vec(),
        };
        if a.shape != expected || !a.is_real() || !a.all_finite() {
            return Err(err(format!("Invalid initial {key} shape or values")));
        }
        arrays.push(a.data);
    }
    let density_unit = units.density_unit_kg_m3;
    let at = |a: &Vec<f64>, x: usize, width: usize, k: usize| if spatial { a[x * width + k] } else { a[k] };
    let mut rows: Vec<[f64; 5]> = Vec::with_capacity(n);
    for x in 0..n {
        rows.push([
            at(&arrays[0], x, 1, 0) / density_unit,
            at(&arrays[2], x, 3, 0) / units.velocity_unit_m_s,
            at(&arrays[2], x, 3, 1) / units.velocity_unit_m_s,
            at(&arrays[2], x, 3, 2) / units.velocity_unit_m_s,
            at(&arrays[1], x, 1, 0) / units.temperature_unit_k,
        ]);
    }
    let mut unique: Vec<[f64; 5]> = rows.clone();
    unique.sort_by(|a, b| {
        a.iter().zip(b).map(|(x, y)| x.total_cmp(y)).find(|o| o.is_ne()).unwrap_or(std::cmp::Ordering::Equal)
    });
    unique.dedup_by(|a, b| a.iter().zip(b.iter()).all(|(x, y)| x.to_bits() == y.to_bits()));
    let mut solved = Vec::with_capacity(unique.len());
    for row in &unique {
        solved.push(super::equilibrium::checked_equilibrium(
            row[0],
            [row[1], row[2], row[3]],
            row[4],
            gamma,
            lattice,
        )?);
    }
    let q = lattice.data().q();
    let mut f = Vec::with_capacity(n * q);
    let mut g = Vec::with_capacity(n * q);
    for row in &rows {
        let i = unique
            .iter()
            .position(|u| u.iter().zip(row).all(|(x, y)| x.to_bits() == y.to_bits()))
            .unwrap_or(0);
        f.extend_from_slice(&solved[i].f);
        g.extend_from_slice(&solved[i].g);
    }
    let temperature = rows.iter().map(|r| r[4]).collect();
    Ok((State { f, g, solid: None }, temperature))
}
