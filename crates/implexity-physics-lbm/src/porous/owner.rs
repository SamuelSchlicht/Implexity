// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::solid_history::SolidKernel;
use serde_json::{Map, Value, json};

use super::caloric::{CaloricLedger, SharedFluidCaloric};
use super::drag::{Drag, SharedDragAdapter};
use super::geometry::GeometryBinding;
use super::lattice::{
    C, CS2, Grid, PortGeometry, Q, W, equilibrium, interior_rates, pressure_port, quadrature_and_gradient,
    stream_open,
};
use super::sp::{self, Sp};
use super::trace::PressureTrace;
use super::viscous::{ViscousCell, initial_populations, stress_and_heat};
use super::wall::{ReferenceWall, WallExchange};

fn err(msg: impl Into<String>) -> CaeError {
    CaeError::contract(msg.into())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Viscosity {
    Constant(f64),
    Exponential {
        nu: f64,
        reference: f64,
        slope: f64,
    },
}

impl Viscosity {
    #[must_use]
    pub fn nu<S: Scalar>(&self, t: S) -> S {
        match *self {
            Self::Constant(nu) => S::from_f64(nu),
            Self::Exponential { nu, reference, slope } => ((t - reference) * slope).exp() * nu,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Interval<S> {
    pub caloric: CaloricLedger<S>,
    pub drag: Drag<S>,
    pub phase_design: Vec<S>,
    pub pressure_pa: Vec<S>,
    pub tau: Vec<S>,
    pub q: Vec<S>,
    pub g: Vec<[S; 3]>,
    pub faces: [f64; 2],
    pub temperature: Vec<S>,
    pub displacement: Vec<[S; 3]>,
    pub previous_displacement: Vec<[S; 3]>,
    pub boundary: Vec<S>,
    pub streamed: Vec<S>,
    pub escaped: Vec<S>,
    pub after_wall: Vec<S>,
    pub expected: Vec<S>,
    pub port_boundary: Vec<S>,
    pub wall: Option<WallExchange<S>>,
}

pub struct Owner {
    pub s: Arc<SolidKernel>,
    pub grid: Grid,
    pub ns: usize,
    pub nf: usize,
    pub state_size: usize,
    pub design_size: usize,
    pub h: f64,
    pub dt: f64,
    pub rho: f64,
    pub cp: f64,
    pub viscosity: Viscosity,
    pub beta: f64,
    pub face_pressure: Vec<[f64; 2]>,
    pub reservoir: Vec<f64>,
    pub trace: Option<PressureTrace>,
    pub wall: Option<ReferenceWall>,
    pub viscous: Option<Value>,
    pub flow_initialization: Option<Value>,
    pub cal: SharedFluidCaloric,
    pub drag: SharedDragAdapter,
    pub geometry: GeometryBinding,
    pub free: Vec<usize>,
    pub rx: Sp,
    pub cx: Sp,
    pub temperature_interval: (f64, f64),
    pub reference_pressure: f64,
    pub batch: usize,
    pub ports: [PortGeometry; 2],
}

pub struct OwnerSpec {
    pub s: Arc<SolidKernel>,
    pub h: f64,
    pub dt: f64,
    pub rho: f64,
    pub cp: f64,
    pub viscosity: Viscosity,
    pub beta: f64,
    pub face_pressure: Vec<[f64; 2]>,
    pub reservoir: Vec<f64>,
    pub trace: Option<PressureTrace>,
    pub wall: Option<ReferenceWall>,
    pub viscous: Option<Value>,
    pub flow_initialization: Option<Value>,
    pub geometry: GeometryBinding,
    pub temperature_interval: (f64, f64),
    pub reference_pressure: f64,
    pub batch: usize,
}

impl Owner {

    pub fn new(spec: OwnerSpec) -> CaeResult<Self> {
        let s = spec.s;
        if spec.wall.is_none() && spec.trace.is_none() {
            return Err(err("pressure_trace or an explicitly authored reference wall is required"));
        }
        if spec.wall.is_some() && spec.trace.is_some() {
            return Err(err("reference wall must replace, not supplement, a sampled pressure trace"));
        }
        if [spec.h, spec.dt, spec.rho, spec.cp].iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(err("positive finite physical scales required"));
        }
        if s.times.windows(2).any(|w| ((w[1] - w[0]) - spec.dt).abs() > 1e-13 * spec.dt.abs()) {
            return Err(err("solid/fluid schedules differ"));
        }
        if let Some(v) = &spec.viscous
            && spec.wall.is_some()
            && v["trace_traction"] == json!(true)
        {
            return Err(err("separate viscous traction duplicates reference-wall momentum exchange"));
        }
        if spec.reservoir.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(err("finite positive scalar/cell reservoir temperature required"));
        }
        let grid = Grid { n: s.grid };
        let nc = grid.cells();
        let cal = SharedFluidCaloric::new(grid, s.nn, &s.mesh.tets, &s.mesh.owners)?;
        let drag = SharedDragAdapter::new(grid, &s.mesh.ijk)?;
        let free: Vec<usize> = (0..nc).filter(|c| !spec.geometry.mask[*c]).collect();
        let nfree = free.len();
        let nd = 2 * nfree;
        let rx = sp::triplets(nc, nd, &free, &(0..nfree).collect::<Vec<_>>(), &vec![1.0; nfree])?;
        let cx = sp::triplets(nc, nd, &free, &(nfree..nd).collect::<Vec<_>>(), &vec![1.0; nfree])?;
        let ns = s.state_size;
        Ok(Self {
            grid,
            ns,
            nf: Q * nc,
            state_size: ns + Q * nc,
            design_size: nd,
            h: spec.h,
            dt: spec.dt,
            rho: spec.rho,
            cp: spec.cp,
            viscosity: spec.viscosity,
            beta: spec.beta,
            face_pressure: spec.face_pressure,
            reservoir: spec.reservoir,
            trace: spec.trace,
            wall: spec.wall,
            viscous: spec.viscous,
            flow_initialization: spec.flow_initialization,
            cal,
            drag,
            geometry: spec.geometry,
            free,
            rx,
            cx,
            temperature_interval: spec.temperature_interval,
            reference_pressure: spec.reference_pressure,
            batch: spec.batch,
            ports: [PortGeometry::new(0, 1)?, PortGeometry::new(0, -1)?],
            s,
        })
    }

    #[must_use]
    pub fn nc(&self) -> usize {
        self.grid.cells()
    }

    #[must_use]
    pub fn coordinates<S: Scalar>(&self, x: &[S]) -> (Vec<S>, Vec<S>) {
        let nc = self.nc();
        let nfree = self.free.len();
        let mut r = vec![S::zero(); nc];
        let mut c: Vec<S> = self.geometry.ghost.iter().map(|v| S::from_f64(*v)).collect();
        for (k, cell) in self.free.iter().enumerate() {
            r[*cell] = x[k];
            c[*cell] = x[nfree + k];
        }
        (r, c)
    }

    #[must_use]
    pub fn phase<S: Scalar>(&self, x: &[S]) -> (Vec<S>, Vec<S>, Vec<S>) {
        let (r, c) = self.coordinates(x);
        let v = self.geometry.values(&r, &c);
        (v.native_design, v.raw_phi, v.phi_halo)
    }

    #[must_use]
    pub fn drag_beta<S: Scalar>(&self, phi: &[S]) -> Vec<S> {
        phi.iter().map(|p| (S::one() - *p) * self.beta).collect()
    }

    #[must_use]
    pub fn nodal_fields<S: Scalar>(&self, n: usize, z: &[S]) -> (Vec<S>, Vec<[S; 3]>) {
        let s = &self.s;
        let m = &s.model;
        let mut t: Vec<S> = s.fixed_t[n].iter().map(|v| S::from_f64(*v)).collect();
        for (k, node) in s.free_t.iter().enumerate() {
            t[*node] = z[k] * m.ts + m.t0;
        }
        let mut u: Vec<S> = s.fixed_u[n].iter().map(|v| S::from_f64(*v)).collect();
        let nt = s.free_t.len();
        for (k, dof) in s.free_u.iter().enumerate() {
            u[*dof] = z[nt + k] * m.us;
        }
        (t, (0..s.nn).map(|i| [u[3 * i], u[3 * i + 1], u[3 * i + 2]]).collect())
    }

    #[must_use]
    pub fn initial<S: Scalar>(&self, x: &[S]) -> Vec<S> {
        let (_, phi, halo) = self.phase(x);
        let (q, g) = quadrature_and_gradient(self.grid, &halo);
        let native: Vec<S> = self.s.initial_state().iter().map(|v| S::from_f64(*v)).collect();
        let f = if let Some(sel) = &self.flow_initialization {
            let beta = self.drag_beta(&phi);
            initial_populations(&q, &g, &beta, sel, self.rho, self.h, self.dt)
        } else {
            let mut f = Vec::with_capacity(self.nf);
            for c in 0..self.nc() {
                let eq = equilibrium(q[c], &[S::zero(); 3]);
                for i in 0..Q {
                    let e = (0..3).fold(S::zero(), |acc, a| acc + g[c][a] / 3.0 * f64::from(C[i][a]));
                    f.push(eq[i] - e * (0.5 * W[i]) / CS2);
                }
            }
            f
        };
        let mut out = native;
        out.extend(f);
        out
    }

    fn relaxation<S: Scalar>(&self, tc: &[S]) -> Vec<S> {
        tc.iter().map(|t| self.viscosity.nu(*t) * 3.0 * self.dt / (self.h * self.h) + 0.5).collect()
    }

    #[must_use]
    pub fn transport_interval<S: Scalar>(&self, n: usize, z: &[S], old: &[S], x: &[S]) -> Interval<S> {
        let nc = self.nc();
        let (xs, phi, halo) = self.phase(x);
        let (q, g) = quadrature_and_gradient(self.grid, &halo);
        let (t, u) = self.nodal_fields(n, z);
        let (to, uo) = self.nodal_fields(n - 1, old);
        let oldf = &old[self.ns..];
        let newf = &z[self.ns..];
        let tc = self.cal.cell_temperature(&t);
        let tau = self.relaxation(&tc);
        let beta = self.drag_beta(&phi);
        let velocity: Vec<[S; 3]> =
            u.iter().zip(&uo).map(|(a, b)| std::array::from_fn(|k| (a[k] - b[k]) / self.dt)).collect();
        let drag =
            self.drag.coupled(&self.cal, oldf, &q, &g, &tau, &velocity, &beta, self.rho, self.h, self.dt);
        let (streamed, escaped) = stream_open(self.grid, &drag.post);
        let mut expected = streamed.clone();
        let wall = self.wall.as_ref().map(|w| {
            let e = w.exchange(&drag.post, &velocity);
            w.return_populations(&mut expected, &e);
            e
        });
        let after_wall = expected.clone();
        let faces = self.face_pressure[n];
        let pressure_scale = self.rho * (self.h / self.dt).powi(2);
        for (face, (index, side)) in [(0usize, 1i32), (self.grid.n[0] - 1, -1)].into_iter().enumerate() {
            if self.wall.as_ref().is_some_and(|w| w.face == face) {
                continue;
            }
            let geo = &self.ports[face];
            debug_assert_eq!(geo.side, side);
            let p = S::from_f64(1.0 / 3.0 + faces[face] / pressure_scale);
            for cell in self.grid.plane(index) {
                let out = pressure_port(
                    geo,
                    &expected[cell * Q..(cell + 1) * Q],
                    p,
                    q[cell],
                    &g[cell],
                    &[S::zero(); 3],
                    &[S::zero(); 2],
                );
                expected[cell * Q..(cell + 1) * Q].copy_from_slice(&out);
            }
        }
        let scale = self.rho * self.h.powi(3);
        let boundary: Vec<S> = (0..nc)
            .map(|c| {
                let mut acc = S::zero();
                for i in 0..Q {
                    let k = c * Q + i;
                    acc += expected[k] - streamed[k] - escaped[k];
                }
                acc * scale / self.dt
            })
            .collect();
        let rates = interior_rates(self.grid, &drag.post, scale / self.dt);
        let s = &self.s;
        let mut delta: Vec<S> =
            s.fixed_t[n].iter().zip(&s.fixed_t[n - 1]).map(|(a, b)| S::from_f64(a - b)).collect();
        for (k, node) in s.free_t.iter().enumerate() {
            delta[*node] = (z[k] - old[k]) * s.model.ts;
        }
        let port_boundary: Vec<S> = match &self.wall {
            None => boundary.clone(),
            Some(w) => boundary.iter().zip(&w.port_mask).map(|(b, m)| *b * *m).collect(),
        };
        let mass = |f: &[S]| -> Vec<S> {
            (0..nc).map(|c| f[c * Q..(c + 1) * Q].iter().fold(S::zero(), |acc, v| acc + *v) * scale).collect()
        };
        let mut caloric = self.cal.interval(
            &t,
            &to,
            &mass(newf),
            &mass(oldf),
            &rates,
            &port_boundary,
            &self.reservoir,
            self.cp,
            self.dt,
            &delta,
        );
        if let (Some(w), Some(e)) = (&self.wall, &wall) {
            caloric = w.caloric(&self.cal, caloric, e, &to, self.cp);
        }
        let pressure: Vec<S> = (0..nc)
            .map(|c| {
                let m = newf[c * Q..(c + 1) * Q].iter().fold(S::zero(), |acc, v| acc + *v);
                (m / q[c] - 1.0) * CS2 * pressure_scale
            })
            .collect();
        Interval {
            caloric,
            drag,
            phase_design: xs,
            pressure_pa: pressure,
            tau,
            q,
            g,
            faces,
            temperature: t,
            displacement: u,
            previous_displacement: uo,
            boundary,
            streamed,
            escaped,
            after_wall,
            expected,
            port_boundary,
            wall,
        }
    }

    #[must_use]
    pub fn interface_load<S: Scalar>(&self, ledger: &Interval<S>) -> Vec<[S; 3]> {
        match (&self.trace, &ledger.wall) {
            (Some(tr), _) => tr.loads(&ledger.pressure_pa).solid_load_n,
            (None, Some(w)) => w.solid_nodal_force_n.clone(),
            (None, None) => vec![[S::zero(); 3]; self.s.nn],
        }
    }

    #[must_use]
    pub fn viscous_cells<S: Scalar>(&self, old: &[S], ledger: &Interval<S>) -> Vec<ViscousCell<S>> {
        let oldf = &old[self.ns..];
        (0..self.nc())
            .map(|c| {
                let d = &ledger.drag.cells[c].drag;
                stress_and_heat(
                    &oldf[c * Q..(c + 1) * Q],
                    ledger.q[c],
                    ledger.tau[c],
                    &d.intrinsic_velocity,
                    &d.total_fluid_force_density,
                    self.rho,
                    self.h,
                    self.dt,
                )
            })
            .collect()
    }

    #[must_use]
    pub fn viscous_trace<S: Scalar>(&self, cells: &[ViscousCell<S>]) -> Vec<[S; 3]> {
        let stress: Vec<[S; 9]> = cells.iter().map(|c| c.intrinsic_stress).collect();
        self.trace.as_ref().map_or_else(|| vec![[S::zero(); 3]; self.s.nn], |tr| tr.viscous_loads(&stress).0)
    }

    #[must_use]
    pub fn viscous_flag(&self, key: &str) -> bool {
        self.viscous.as_ref().is_some_and(|v| v[key] == json!(true))
    }


    pub fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let ledger = self.transport_interval(n, z, old, x);
        let s = &self.s;
        let mut solid = s.assembled_residual(n, &z[..self.ns], &old[..self.ns], &ledger.phase_design)?;
        let load = self.interface_load(&ledger);
        let force: Vec<[f64; 3]> = ledger
            .drag
            .solid_nodal_force_n
            .iter()
            .zip(&load)
            .map(|(a, b)| std::array::from_fn(|k| a[k] + b[k]))
            .collect();
        for (r, v) in solid.iter_mut().zip(s.external_nodal_load_residual(&force)?) {
            *r += v;
        }
        let heat: Vec<f64> = ledger
            .drag
            .shared_nodal_heat_w
            .iter()
            .zip(&ledger.caloric.nodal_residual_w)
            .map(|(a, b)| a - b)
            .collect();
        for (r, v) in solid.iter_mut().zip(s.external_nodal_heat_residual(&heat)?) {
            *r += v;
        }
        if self.viscous.is_some() {
            let cells = self.viscous_cells(old, &ledger);
            if self.viscous_flag("heating") {
                let w: Vec<f64> = cells.iter().map(|c| c.heat).collect();
                let heat = self.cal.lmap().apply_t(&w);
                for (r, v) in solid.iter_mut().zip(s.external_nodal_heat_residual(&heat)?) {
                    *r += v;
                }
            }
            if self.viscous_flag("trace_traction") {
                let force = self.viscous_trace(&cells);
                for (r, v) in solid.iter_mut().zip(s.external_nodal_load_residual(&force)?) {
                    *r += v;
                }
            }
        }
        solid.extend(z[self.ns..].iter().zip(&ledger.expected).map(|(a, b)| a - b));
        Ok(solid)
    }


    pub fn validate_design(&self, x: &[f64]) -> CaeResult<Map<String, Value>> {
        let nc = self.nc();
        if x.len() != self.design_size {
            return Err(err("exact free design shape required"));
        }
        if x.iter().any(|v| !v.is_finite()) {
            return Err(err("nonfinite geometry"));
        }
        let (rho, c) = self.coordinates(x);
        self.geometry.partials(&rho, &c, &self.rx, &self.cx)?;
        let (xs, phi, halo) = self.phase(x);
        let (q, g) = quadrature_and_gradient(self.grid, &halo);
        if phi.iter().chain(&halo).any(|v| *v <= 0.0 || *v > 1.0) {
            return Err(err("strictly positive fluid porosity required"));
        }
        let hg = self.grid.halo();
        for cell in 0..nc {
            let [i, j, k] = self.grid.ijk(cell);
            if halo[hg.id(i + 1, j + 1, k + 1)] != phi[cell] {
                return Err(err("halo interior mismatch"));
            }
        }
        if (0..nc).any(|c| (xs[c] + q[c] - 1.0).abs() > 4.0 * f64::EPSILON) {
            return Err(err("effective phase complementarity mismatch"));
        }
        if (0..3).any(|a| (xs[nc + a] * 1e-3 - self.h).abs() > 1e-14 * self.h.abs()) {
            return Err(err("physical grid sizing must remain fixed"));
        }
        for index in [0, self.grid.n[0] - 1] {
            for cell in self.grid.plane(index) {
                if (q[cell] - 1.0).abs() > 1e-15 || g[cell].iter().any(|v| *v != 0.0) {
                    return Err(err("pressure faces require clear quadrature collars"));
                }
            }
        }
        let mut m = Map::new();
        m.insert("source_profile".into(), json!(super::geometry::SOURCE_PROFILE));
        m.insert("public_native_void_admission_established".into(), json!(false));
        m.insert("exact_zero_solid_cells".into(), json!(xs[..nc].iter().filter(|v| **v == 0.0).count()));
        Ok(m)
    }


    pub fn validate_state(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<()> {
        self.validate_design(x)?;
        for v in [z, old] {
            if v.len() != self.state_size || v.iter().any(|a| !a.is_finite()) {
                return Err(err("finite exact-shape coupled state required"));
            }
        }
        let (t, _) = self.nodal_fields(n, z);
        let (_, phi, _) = self.phase(x);
        let tc = self.cal.cell_temperature(&t);
        let nu: Vec<f64> = tc.iter().map(|v| self.viscosity.nu(*v)).collect();
        let beta = self.drag_beta(&phi);
        if t.iter().any(|v| *v <= 0.0) || nu.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(err("positive finite absolute temperature and viscosity required"));
        }
        if beta.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err(err("finite nonnegative exact-grid drag required"));
        }
        for index in [0, self.grid.n[0] - 1] {
            if self.grid.plane(index).iter().any(|c| beta[*c] != 0.0) {
                return Err(err("explicit-force pressure collars cannot carry implicit drag"));
            }
        }
        for v in [z, old] {
            if (0..self.nc()).any(|c| v[self.ns + c * Q..self.ns + (c + 1) * Q].iter().sum::<f64>() <= 0.0) {
                return Err(err("positive population mass required"));
            }
        }
        let faces = self.face_pressure[n];
        let lattice = self.rho * (self.h / self.dt).powi(2);
        if faces.iter().any(|v| !v.is_finite() || 1.0 / 3.0 + v / lattice <= 0.0) {
            return Err(err("two finite pressure faces with positive lattice EOS pressure required"));
        }
        let (lo, hi) = self.temperature_interval;
        for (step, state) in [(n, z), (n - 1, old)] {
            let (t, _) = self.nodal_fields(step, state);
            if t.iter().any(|v| *v < lo || *v > hi) {
                return Err(err("shared temperature outside authored fluid evaluation interval"));
            }
            let (_, _, halo) = self.phase(x);
            let (q, _) = quadrature_and_gradient(self.grid, &halo);
            let gauge: Vec<f64> = (0..self.nc())
                .map(|c| {
                    let mass: f64 = state[self.ns + c * Q..self.ns + (c + 1) * Q].iter().sum();
                    (mass / q[c] - 1.0) * self.rho * (self.h / self.dt).powi(2) / 3.0
                })
                .collect();
            if let Some(tr) = &self.trace {
                tr.admit(&gauge)?;
            } else if gauge.iter().any(|g| self.reference_pressure + g <= 0.0) {
                return Err(err("nonpositive physical absolute pressure"));
            }
        }
        Ok(())
    }


    pub fn native_state(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<()> {
        let s = &self.s;
        let (xs, _, _) = self.phase(x);
        s.check(n, &z[..self.ns], &old[..self.ns], &xs)?;
        let m = &s.model;
        let base = s.n_t() + s.n_u();
        if let Some(h) = &m.history {
            let mut aux = Vec::with_capacity(s.ne * h.size);
            for e in 0..s.ne {
                let start = base + e * s.internal_size + m.layout.material_start();
                aux.extend(z[start..start + h.size].iter().zip(&h.scales).map(|(v, s)| v * s));
            }
            h.check_state(&aux)?;
        }
        if let Some(v) = &m.viscoelastic {
            let fields = s.fields(n, &z[..self.ns], &xs);
            let range = m.layout.viscoelastic();
            let state: Vec<f64> = fields.iter().flat_map(|f| f.state[range.clone()].to_vec()).collect();
            let temperature: Vec<f64> = fields.iter().map(|f| f.prop.temperature).collect();
            v.check_state(&state, &temperature)?;
        }
        Ok(())
    }
}
