// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_core::{CaeError, CaeResult};
use implexity_solve::multirate_coupling::{
    SubcycleCotangent, SubcycleRecord, SubcycleTangent, SubcycledField,
};
use implexity_solve::time_stepper::{StepDiagnostics, StepParameters};
use rayon::prelude::*;

use super::boundary::Topology;
use super::collision::Collision;
use super::field::{MovingLbmConfig, Turbulence};
use super::lattice::{Lattice, equilibrium, moments};

pub const REFUSAL: &str = "interpolated bounce-back is a non-differentiable verification mode";

pub trait RigidShape: Send + Sync {
    fn inside(&self, x: [f64; 3]) -> bool;
    fn crossing(&self, from: [f64; 3], to: [f64; 3]) -> f64 {
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            let p = std::array::from_fn(|d| from[d] + mid * (to[d] - from[d]));
            if self.inside(p) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi.max(1e-12)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cylinder {
    pub centre: [f64; 3],
    pub radius: f64,
    pub axis: usize,
}

impl RigidShape for Cylinder {
    fn inside(&self, x: [f64; 3]) -> bool {
        let mut r2 = 0.0;
        for d in 0..3 {
            if d != self.axis {
                r2 += (x[d] - self.centre[d]).powi(2);
            }
        }
        r2 < self.radius * self.radius
    }

    fn crossing(&self, from: [f64; 3], to: [f64; 3]) -> f64 {
        let (mut a, mut b, mut c) = (0.0, 0.0, -self.radius * self.radius);
        for d in 0..3 {
            if d == self.axis {
                continue;
            }
            let p = from[d] - self.centre[d];
            let v = to[d] - from[d];
            a += v * v;
            b += 2.0 * p * v;
            c += p * p;
        }
        let disc = (b * b - 4.0 * a * c).max(0.0).sqrt();
        ((-b - disc) / (2.0 * a)).clamp(1e-12, 1.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sphere {
    pub centre: [f64; 3],
    pub radius: f64,
}

impl RigidShape for Sphere {
    fn inside(&self, x: [f64; 3]) -> bool {
        (0..3).map(|d| (x[d] - self.centre[d]).powi(2)).sum::<f64>() < self.radius * self.radius
    }

    fn crossing(&self, from: [f64; 3], to: [f64; 3]) -> f64 {
        let (mut a, mut b, mut c) = (0.0, 0.0, -self.radius * self.radius);
        for d in 0..3 {
            let p = from[d] - self.centre[d];
            let v = to[d] - from[d];
            a += v * v;
            b += 2.0 * p * v;
            c += p * p;
        }
        let disc = (b * b - 4.0 * a * c).max(0.0).sqrt();
        ((-b - disc) / (2.0 * a)).clamp(1e-12, 1.0)
    }
}

pub struct InterpolatedBounceBack<const Q: usize, L: Lattice<Q>> {
    config: MovingLbmConfig,
    topology: Topology<Q>,
    collision: Collision<Q, L>,
    shape: Arc<dyn RigidShape>,
    centres: Vec<[f64; 3]>,
    names: Vec<String>,
}

impl<const Q: usize, L: Lattice<Q>> std::fmt::Debug for InterpolatedBounceBack<Q, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterpolatedBounceBack")
            .field("lattice", &L::NAME)
            .field("shape", &self.config.shape)
            .finish_non_exhaustive()
    }
}

impl<const Q: usize, L: Lattice<Q>> InterpolatedBounceBack<Q, L> {

    pub fn new(config: MovingLbmConfig, shape: Arc<dyn RigidShape>) -> CaeResult<Self> {
        if !config.ports.is_empty()
            || !config.sponges.is_empty()
            || !config.observables.is_empty()
            || config.turbulence != Turbulence::Laminar
        {
            return Err(CaeError::contract(
                "interpolated bounce-back verification mode supports walls, body forces and the collision model only \
                 (no ports, sponges, observables or turbulence closure)",
            ));
        }
        if !(config.spacing_m > 0.0
            && config.macro_step_s > 0.0
            && config.substeps > 0
            && config.kinematic_viscosity_m2_s > 0.0)
        {
            return Err(CaeError::contract("interpolated bounce-back: invalid units"));
        }
        let grid = crate::d3q19::Grid::new(config.shape);
        let topology = Topology::new::<L>(grid, config.periodic, config.solid_mask.clone(), &[])?;
        let collision = Collision::new(config.collision.clone())?;
        let centres = (0..grid.cells())
            .map(|x| {
                let c = grid.coords(x);
                std::array::from_fn(|a| config.origin_m[a] + (c[a] as f64 + 0.5) * config.spacing_m)
            })
            .collect();
        Ok(Self { config, topology, collision, shape, centres, names: Vec::new() })
    }

    fn dt_f(&self) -> f64 {
        self.config.macro_step_s / self.config.substeps as f64
    }

    #[must_use]
    pub fn initial_state(&self) -> Vec<f64> {
        let n = self.topology.cells();
        let c = self.config.spacing_m / self.dt_f();
        let eq = equilibrium::<f64, Q, L>(1.0, self.config.initial_velocity_m_s.map(|v| v / c));
        let mut out = vec![0.0; Q * n];
        for i in 0..Q {
            out[i * n..(i + 1) * n].fill(eq[i]);
        }
        out
    }

    fn occupied(&self, offset: [f64; 3]) -> Vec<bool> {
        self.centres
            .par_iter()
            .enumerate()
            .map(|(x, p)| {
                !self.topology.wall[x]
                    && self.shape.inside([p[0] - offset[0], p[1] - offset[1], p[2] - offset[2]])
            })
            .collect()
    }


    pub fn subcycle(
        &self,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
    ) -> CaeResult<SubcycleRecord> {
        let n = self.topology.cells();
        if previous.len() != Q * n || trace_start.len() != 3 || trace_end.len() != 3 {
            return Err(CaeError::contract("interpolated bounce-back: state or trace length mismatch"));
        }
        let cfg = &self.config;
        let m = cfg.substeps;
        let dx = cfg.spacing_m;
        let dt_f = self.dt_f();
        let c = dx / dt_f;
        let uw: [f64; 3] = std::array::from_fn(|d| (trace_end[d] - trace_start[d]) / cfg.macro_step_s / c);
        let accel = cfg.body_acceleration_m_s2.map(|a| a * dt_f * dt_f / dx);
        let tau = 0.5 + 3.0 * cfg.kinematic_viscosity_m2_s * dt_f / (dx * dx);
        let omega = 1.0 / tau;
        let grid = self.topology.grid;
        let offset_at = |k: usize| -> [f64; 3] {
            let t = k as f64 / m as f64;
            std::array::from_fn(|d| trace_start[d] + t * (trace_end[d] - trace_start[d]))
        };
        let mut g = previous.to_vec();
        let mut old = self.occupied(offset_at(0));
        let mut momentum = [0.0; 3];
        for k in 1..=m {
            let offset = offset_at(k);
            let new = self.occupied(offset);

            for x in 0..n {
                if old[x] && !new[x] {
                    let mut rho = 0.0;
                    let mut count = 0.0;
                    for i in 1..Q {
                        let cc = L::C[i];
                        let o = [i64::from(cc[0]), i64::from(cc[1]), i64::from(cc[2])];
                        if grid.inside(x, o, cfg.periodic) {
                            let y = grid.wrap(x, o);
                            if !self.topology.wall[y] && !old[y] && !new[y] {
                                let f: [f64; Q] = std::array::from_fn(|j| g[j * n + y]);
                                rho += moments::<f64, Q, L>(&f).0;
                                count += 1.0;
                            }
                        }
                    }
                    let rho = if count > 0.0 { rho / count } else { 1.0 };
                    let eq = equilibrium::<f64, Q, L>(rho, uw);
                    for i in 0..Q {
                        g[i * n + x] = eq[i];
                        momentum[0] -= eq[i] * L::CF[i][0];
                        momentum[1] -= eq[i] * L::CF[i][1];
                        momentum[2] -= eq[i] * L::CF[i][2];
                    }
                }
                if !old[x] && new[x] && !self.topology.wall[x] {

                    for i in 0..Q {
                        for d in 0..3 {
                            momentum[d] += g[i * n + x] * L::CF[i][d];
                        }
                    }
                }
            }
            let rows: Vec<([f64; Q], [f64; 3])> = (0..n)
                .into_par_iter()
                .map(|x| {
                    if self.topology.wall[x] || new[x] {
                        return (std::array::from_fn(|i| g[i * n + x]), [0.0; 3]);
                    }
                    let mut f = self.topology.pull_cell(&g, x);
                    let mut ex = [0.0; 3];
                    for i in 1..Q {
                        let cc = L::C[i];
                        let back = [-i64::from(cc[0]), -i64::from(cc[1]), -i64::from(cc[2])];
                        if !grid.inside(x, back, cfg.periodic) {
                            continue;
                        }
                        let src = grid.wrap(x, back);
                        if !new[src] {
                            continue;
                        }
                        let o = L::OPPOSITE[i];
                        let p = self.centres[x];
                        let from = [p[0] - offset[0], p[1] - offset[1], p[2] - offset[2]];
                        let to = std::array::from_fn(|d| from[d] - L::CF[i][d] * dx);
                        let q = self.shape.crossing(from, to);
                        let cu = L::CF[i][0] * uw[0] + L::CF[i][1] * uw[1] + L::CF[i][2] * uw[2];
                        let fwd = [i64::from(cc[0]), i64::from(cc[1]), i64::from(cc[2])];
                        let ahead = grid.inside(x, fwd, cfg.periodic).then(|| grid.wrap(x, fwd));
                        let usable = ahead.filter(|&y| !self.topology.wall[y] && !new[y]);
                        let out = g[o * n + x];
                        let value = match usable {
                            Some(y) if q < 0.5 => {
                                2.0 * q * out + (1.0 - 2.0 * q) * g[o * n + y] + 6.0 * L::W[i] * cu
                            }
                            _ if q >= 0.5 => {
                                out / (2.0 * q)
                                    + (2.0 * q - 1.0) / (2.0 * q) * g[i * n + x]
                                    + 3.0 * L::W[i] * cu / q
                            }
                            _ => out + 6.0 * L::W[i] * cu,
                        };
                        f[i] = value;
                        for d in 0..3 {
                            ex[d] += out * (L::CF[o][d] - uw[d]) - value * (L::CF[i][d] - uw[d]);
                        }
                    }
                    let (rho, j) = moments::<f64, Q, L>(&f);
                    let u = std::array::from_fn(|d| j[d] / rho + 0.5 * accel[d]);
                    let force = accel.map(|a| rho * a);
                    (self.collision.collide(&f, rho, u, force, omega), ex)
                })
                .collect();
            let mut next = vec![0.0; Q * n];
            for (x, (post, ex)) in rows.iter().enumerate() {
                for i in 0..Q {
                    next[i * n + x] = post[i];
                }
                for d in 0..3 {
                    momentum[d] += ex[d];
                }
            }
            g = next;
            old = new;
        }
        if g.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("interpolated bounce-back: non-finite population"));
        }
        let scale = cfg.density_kg_m3 * dx.powi(4) / dt_f / cfg.macro_step_s;
        let flux: Vec<f64> = momentum.iter().map(|p| p * scale).collect();
        let work: f64 = (0..3).map(|d| flux[d] * (trace_end[d] - trace_start[d])).sum();
        let mut ledger = BTreeMap::new();
        ledger.insert("interface_work_J".to_string(), work);
        Ok(SubcycleRecord {
            state: g,
            flux,
            samples: Vec::new(),
            pairing: work,
            ledger,
            diagnostics: StepDiagnostics::default(),
        })
    }
}

impl<const Q: usize, L: Lattice<Q>> SubcycledField for InterpolatedBounceBack<Q, L> {
    fn state_size(&self) -> usize {
        Q * self.topology.cells()
    }
    fn trace_size(&self) -> usize {
        3
    }
    fn design_size(&self) -> usize {
        0
    }
    fn sample_names(&self) -> &[String] {
        &self.names
    }
    fn initial_state(&self, _design: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(InterpolatedBounceBack::initial_state(self))
    }
    fn initial_state_vjp(&self, _design: &[f64], _cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        Err(CaeError::contract(REFUSAL))
    }
    fn subcycle(
        &self,
        _n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        if p.time_scale != 1.0 {
            return Err(CaeError::contract("interpolated bounce-back runs at the nominal time scale only"));
        }
        InterpolatedBounceBack::subcycle(self, previous, trace_start, trace_end)
    }
    fn subcycle_tangent(
        &self,
        _n: usize,
        _previous: &[f64],
        _trace_start: &[f64],
        _trace_end: &[f64],
        _p: StepParameters<'_>,
        _d_previous: &[f64],
        _d_trace_start: &[f64],
        _d_trace_end: &[f64],
        _d_design: Option<&[f64]>,
        _d_time_scale: f64,
    ) -> CaeResult<SubcycleTangent> {
        Err(CaeError::contract(REFUSAL))
    }
    fn subcycle_adjoint(
        &self,
        _n: usize,
        _previous: &[f64],
        _trace_start: &[f64],
        _trace_end: &[f64],
        _p: StepParameters<'_>,
        _state_bar: &[f64],
        _flux_bar: &[f64],
        _sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        Err(CaeError::contract(REFUSAL))
    }
}
