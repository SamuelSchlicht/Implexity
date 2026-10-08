// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

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
use super::pushforward::{Frame, Stencils};

pub const REFUSAL: &str =
    "the multi-direct-forcing immersed boundary is a verification mode without design sensitivities";

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceMarkers {
    reference: Vec<[f64; 3]>,
    weights: Vec<f64>,
}

impl SurfaceMarkers {

    pub fn new(reference: Vec<[f64; 3]>, weights: Vec<f64>) -> CaeResult<Self> {
        if reference.is_empty()
            || reference.len() != weights.len()
            || reference.iter().flatten().any(|v| !v.is_finite())
            || weights.iter().any(|w| !(w.is_finite() && *w > 0.0))
        {
            return Err(CaeError::contract(
                "immersed boundary markers need finite positions and positive weights, one per marker",
            ));
        }
        Ok(Self { reference, weights })
    }


    pub fn circle(
        centre: [f64; 3],
        radius: f64,
        axis: usize,
        spacing: f64,
        spacing_m: f64,
        depth_m: f64,
    ) -> CaeResult<Self> {
        if axis > 2 || !(radius > 0.0 && spacing > 0.0 && spacing_m > 0.0 && depth_m > 0.0) {
            return Err(CaeError::contract("immersed boundary circle: invalid parameters"));
        }
        let (a, b) = match axis {
            0 => (1, 2),
            1 => (2, 0),
            _ => (0, 1),
        };
        let perimeter = 2.0 * std::f64::consts::PI * radius;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = (perimeter / spacing).ceil().max(8.0) as usize;
        let ds = perimeter / count as f64;
        let mut reference = Vec::with_capacity(count);
        for j in 0..count {
            let phi = 2.0 * std::f64::consts::PI * (j as f64 + 0.5) / count as f64;
            let mut x = centre;
            x[a] += radius * phi.cos();
            x[b] += radius * phi.sin();
            reference.push(x);
        }
        Self::new(reference, vec![ds * spacing_m * depth_m; count])
    }


    pub fn sphere(centre: [f64; 3], radius: f64, spacing: f64, spacing_m: f64) -> CaeResult<Self> {
        if !(radius > 0.0 && spacing > 0.0 && spacing_m > 0.0) {
            return Err(CaeError::contract("immersed boundary sphere: invalid parameters"));
        }
        let area = 4.0 * std::f64::consts::PI * radius * radius;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = (area / (spacing * spacing)).ceil().max(12.0) as usize;
        let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
        let mut reference = Vec::with_capacity(count);
        for j in 0..count {
            let z = 1.0 - 2.0 * (j as f64 + 0.5) / count as f64;
            let r = (1.0 - z * z).max(0.0).sqrt();
            let phi = golden * j as f64;
            reference.push([
                centre[0] + radius * r * phi.cos(),
                centre[1] + radius * r * phi.sin(),
                centre[2] + radius * z,
            ]);
        }
        Self::new(reference, vec![area / count as f64 * spacing_m; count])
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.reference.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reference.is_empty()
    }
}

pub struct ImmersedBoundary<const Q: usize, L: Lattice<Q>> {
    config: MovingLbmConfig,
    frame: Frame,
    topology: Topology<Q>,
    collision: Collision<Q, L>,
    markers: SurfaceMarkers,
    iterations: usize,
    names: Vec<String>,
}

impl<const Q: usize, L: Lattice<Q>> std::fmt::Debug for ImmersedBoundary<Q, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImmersedBoundary")
            .field("lattice", &L::NAME)
            .field("shape", &self.config.shape)
            .field("markers", &self.markers.len())
            .field("iterations", &self.iterations)
            .finish_non_exhaustive()
    }
}

impl<const Q: usize, L: Lattice<Q>> ImmersedBoundary<Q, L> {

    pub fn new(config: MovingLbmConfig, markers: SurfaceMarkers, iterations: usize) -> CaeResult<Self> {
        if !config.ports.is_empty()
            || !config.sponges.is_empty()
            || !config.observables.is_empty()
            || config.turbulence != Turbulence::Laminar
        {
            return Err(CaeError::contract(
                "immersed boundary verification mode supports walls, body forces and the collision model only \
                 (no ports, sponges, observables or turbulence closure)",
            ));
        }
        if !(1..=50).contains(&iterations) {
            return Err(CaeError::contract("immersed boundary: iterations must lie in 1..=50"));
        }
        if !(config.spacing_m > 0.0
            && config.macro_step_s > 0.0
            && config.substeps > 0
            && config.kinematic_viscosity_m2_s > 0.0)
        {
            return Err(CaeError::contract("immersed boundary: invalid units"));
        }
        let frame = Frame::new(config.shape, config.spacing_m, config.origin_m, config.periodic, 1)?;
        let topology = Topology::new::<L>(frame.grid(), config.periodic, config.solid_mask.clone(), &[])?;
        let collision = Collision::new(config.collision.clone())?;
        Ok(Self { config, frame, topology, collision, markers, iterations, names: Vec::new() })
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

    fn supports(&self, st: &Stencils<f64>) -> Vec<Vec<(usize, f64)>> {
        let grid = self.frame.grid();
        let sup = [self.frame.support(0), self.frame.support(1), self.frame.support(2)];
        let stride = 4 * self.frame.width_cells;
        (0..st.len())
            .into_par_iter()
            .map(|q| {
                let base = st.base[q];
                let mut out = Vec::with_capacity(sup[0] * sup[1] * sup[2]);
                for tx in 0..sup[0] {
                    let i = (base[0] + tx) % self.frame.shape[0];
                    let wx = st.w[q * 3 * stride + tx];
                    for ty in 0..sup[1] {
                        let j = (base[1] + ty) % self.frame.shape[1];
                        let wy = st.w[(q * 3 + 1) * stride + ty];
                        for tz in 0..sup[2] {
                            let k = (base[2] + tz) % self.frame.shape[2];
                            let wz = st.w[(q * 3 + 2) * stride + tz];
                            let x = grid.index(i, j, k);
                            if !self.topology.wall[x] {
                                out.push((x, wx * wy * wz));
                            }
                        }
                    }
                }
                out
            })
            .collect()
    }


    #[allow(clippy::too_many_lines)]
    pub fn subcycle(
        &self,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
    ) -> CaeResult<SubcycleRecord> {
        let n = self.topology.cells();
        if previous.len() != Q * n || trace_start.len() != 3 || trace_end.len() != 3 {
            return Err(CaeError::contract("immersed boundary: state or trace length mismatch"));
        }
        let cfg = &self.config;
        let m = cfg.substeps;
        let dx = cfg.spacing_m;
        let dt_f = self.dt_f();
        let c = dx / dt_f;
        let ub: [f64; 3] = std::array::from_fn(|d| (trace_end[d] - trace_start[d]) / cfg.macro_step_s / c);
        let accel = cfg.body_acceleration_m_s2.map(|a| a * dt_f * dt_f / dx);
        let tau = 0.5 + 3.0 * cfg.kinematic_viscosity_m2_s * dt_f / (dx * dx);
        let omega = 1.0 / tau;
        let vol = dx * dx * dx;
        let wm: Vec<f64> = self.markers.weights.iter().map(|w| w / vol).collect();
        let mut g = previous.to_vec();
        let mut next = vec![0.0; Q * n];
        let mut momentum = [0.0; 3];
        let mut worst_defect: f64 = 0.0;
        for k in 1..=m {
            let theta = (k as f64 - 0.5) / m as f64;
            let xi: Vec<[f64; 3]> = self
                .markers
                .reference
                .iter()
                .map(|x| {
                    let p: [f64; 3] = std::array::from_fn(|d| {
                        x[d] + trace_start[d] + theta * (trace_end[d] - trace_start[d])
                    });
                    self.frame.to_lattice(p)
                })
                .collect();
            let st = Stencils::new(&self.frame, &xi, false)?;
            let support = self.supports(&st);

            let (rho, u0): (Vec<f64>, Vec<[f64; 3]>) = (0..n)
                .into_par_iter()
                .map(|x| {
                    if self.topology.wall[x] {
                        return (1.0, [0.0; 3]);
                    }
                    let f = self.topology.pull_cell(&g, x);
                    let (r, j) = moments::<f64, Q, L>(&f);
                    (r, std::array::from_fn(|d| j[d] / r + 0.5 * accel[d]))
                })
                .unzip();
            let mut u = u0.clone();
            for _ in 0..self.iterations {
                let defects: Vec<[f64; 3]> = support
                    .par_iter()
                    .map(|cells| {
                        let mut um = [0.0; 3];
                        for &(x, w) in cells {
                            for d in 0..3 {
                                um[d] += w * u[x][d];
                            }
                        }
                        std::array::from_fn(|d| ub[d] - um[d])
                    })
                    .collect();
                for (q, cells) in support.iter().enumerate() {
                    for &(x, w) in cells {
                        for d in 0..3 {
                            u[x][d] += w * defects[q][d] * wm[q];
                        }
                    }
                }
            }

            for cells in &support {
                let mut um = [0.0; 3];
                for &(x, w) in cells {
                    for d in 0..3 {
                        um[d] += w * u[x][d];
                    }
                }
                let e: f64 = (0..3).map(|d| (ub[d] - um[d]).powi(2)).sum::<f64>().sqrt();
                worst_defect = worst_defect.max(e);
            }
            let posts: Vec<([f64; Q], [f64; 3])> = (0..n)
                .into_par_iter()
                .map(|x| {
                    if self.topology.wall[x] {
                        return (std::array::from_fn(|i| g[i * n + x]), [0.0; 3]);
                    }
                    let f = self.topology.pull_cell(&g, x);
                    let r = rho[x];
                    let fib: [f64; 3] = std::array::from_fn(|d| 2.0 * r * (u[x][d] - u0[x][d]));
                    let force: [f64; 3] = std::array::from_fn(|d| r * accel[d] + fib[d]);
                    (self.collision.collide(&f, r, u[x], force, omega), fib)
                })
                .collect();
            for (x, (post, fib)) in posts.iter().enumerate() {
                for i in 0..Q {
                    next[i * n + x] = post[i];
                }
                for d in 0..3 {
                    momentum[d] -= fib[d];
                }
            }
            std::mem::swap(&mut g, &mut next);
        }
        if g.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence("immersed boundary: non-finite population"));
        }
        let scale = cfg.density_kg_m3 * dx.powi(4) / dt_f / cfg.macro_step_s;
        let flux: Vec<f64> = momentum.iter().map(|p| p * scale).collect();
        let work: f64 = (0..3).map(|d| flux[d] * (trace_end[d] - trace_start[d])).sum();
        let mut ledger = BTreeMap::new();
        ledger.insert("interface_work_J".to_string(), work);
        ledger.insert("max_marker_velocity_defect".to_string(), worst_defect);
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

impl<const Q: usize, L: Lattice<Q>> SubcycledField for ImmersedBoundary<Q, L> {
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
        Ok(ImmersedBoundary::initial_state(self))
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
            return Err(CaeError::contract("immersed boundary runs at the nominal time scale only"));
        }
        ImmersedBoundary::subcycle(self, previous, trace_start, trace_end)
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
