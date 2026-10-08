// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use serde_json::{Value, json};

use implexity_ad::tape::{Tape, Var};
use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::dense::DenseMatrix;

use super::adjoint::{Gather, Kernel, RootSpec, element_node, root_node};
use super::kinematics::{
    Mat3, TetMesh, det, green_maxwell_step, inverse_transpose, matmul, maxwell_potential_piola,
    neo_hookean_cauchy, neo_hookean_energy, neo_hookean_piola, transpose,
};
use super::statics::{StaticOptions, StaticProblem, ViscoelasticState};
use crate::util::{bool_array, contract, f64_shaped, has_exact_keys, int_array, real_array};

pub const KEYS: [&str; 19] = [
    "points",
    "elements",
    "shear_Pa",
    "lame_Pa",
    "fixed_dofs",
    "prescribed_displacement_history_m",
    "nodal_force_history_N",
    "steps_s",
    "branch_moduli_Pa",
    "relaxation_times_s",
    "initial_branch_strain",
    "initial_displacement_m",
    "biot_coefficient",
    "biot_modulus_Pa",
    "reference_mobility_m2_Pa_s",
    "initial_pressure_Pa",
    "fixed_pressure_nodes",
    "pressure_history_Pa",
    "fluid_source_history_m3_s",
];
pub const MATERIALS: [&str; 7] = [
    "shear_Pa",
    "lame_Pa",
    "branch_moduli_Pa",
    "relaxation_times_s",
    "biot_coefficient",
    "biot_modulus_Pa",
    "reference_mobility_m2_Pa_s",
];
pub const RESPONSES: [&str; 5] = [
    "dissipation_J",
    "final_stored_energy_J",
    "final_displacement_squared_m2",
    "final_fluid_content_increment_m3",
    "mechanical_work_J",
];

#[derive(Debug, Clone)]
pub struct PoroProblem {
    pub mesh: TetMesh,
    pub mu: Vec<f64>,
    pub lam: Vec<f64>,
    pub fixed_dofs: Vec<bool>,
    pub prescribed: Vec<Vec<f64>>,
    pub force: Vec<Vec<f64>>,
    pub steps: Vec<f64>,
    pub moduli: Vec<Vec<f64>>,
    pub times: Vec<Vec<f64>>,
    pub memory: Vec<Vec<Mat3<f64>>>,
    pub u0: Vec<f64>,
    pub alpha: Vec<f64>,
    pub biot: Vec<f64>,
    pub mobility: Vec<f64>,
    pub p0: Vec<f64>,
    pub fixed_pressure: Vec<bool>,
    pub pressure_history: Vec<Vec<f64>>,
    pub source: Vec<Vec<f64>>,
}

fn rows(v: &[f64], width: usize) -> Vec<Vec<f64>> {
    v.chunks(width.max(1)).map(<[f64]>::to_vec).collect()
}

fn mats(v: &[f64]) -> Vec<Mat3<f64>> {
    v.chunks(9).map(|c| std::array::from_fn(|i| std::array::from_fn(|j| c[3 * i + j]))).collect()
}


pub fn parse_mesh(p: &Value) -> Result<TetMesh, CaeError> {
    let Some((ps, points)) = real_array(&p["points"]).filter(|(s, _)| s.len() == 2 && s[1] == 3) else {
        return contract("points require finite numerical [node,3] metre coordinates");
    };
    let Some((es, elements)) = int_array(&p["elements"]).filter(|(s, _)| s.len() == 2 && s[1] == 4) else {
        return contract("elements require integer [element,4] node indices");
    };
    let n = ps[0];
    if elements.iter().any(|v| *v < 0 || usize::try_from(*v).unwrap_or(usize::MAX) >= n) {
        return contract("invalid connectivity or unused mesh nodes");
    }
    let pts = points.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    let tets =
        elements.chunks(4).map(|c| std::array::from_fn(|i| usize::try_from(c[i]).unwrap_or(0))).collect();
    let _ = es;
    TetMesh::new(pts, tets)
}

impl PoroProblem {

    #[allow(clippy::too_many_lines)]
    pub fn parse(p: &Value) -> Result<Self, CaeError> {
        if !has_exact_keys(p, &KEYS) {
            return contract("Complete explicit poroviscoelastic problem required");
        }
        let mesh = parse_mesh(p)?;
        let (n, ne) = (mesh.node_count(), mesh.elements.len());
        let steps = match real_array(&p["steps_s"]) {
            Some((s, v))
                if s.len() == 1
                    && (1..=1000).contains(&v.len())
                    && v.iter().all(|x| x.is_finite() && *x > 0.0) =>
            {
                v
            }
            _ => return contract("Positive finite time increments required"),
        };
        let nt = steps.len();
        let uh = f64_shaped(&p["prescribed_displacement_history_m"], &[nt, n, 3]);
        let fh = f64_shaped(&p["nodal_force_history_N"], &[nt, n, 3]);
        let ph = f64_shaped(&p["pressure_history_Pa"], &[nt, n]);
        let sources = f64_shaped(&p["fluid_source_history_m3_s"], &[nt, n]);
        let pressure = f64_shaped(&p["initial_pressure_Pa"], &[n]);
        let u = f64_shaped(&p["initial_displacement_m"], &[n, 3]);
        let (Some(uh), Some(fh), Some(ph), Some(sources), Some(pressure), Some(u)) =
            (uh, fh, ph, sources, pressure, u)
        else {
            return contract("Inconsistent nodal histories or initial states");
        };
        let Some((_, fixedp)) = bool_array(&p["fixed_pressure_nodes"]).filter(|(s, _)| s == &[n]) else {
            return contract("Explicit boolean pressure-reservoir node mask required");
        };
        if uh.iter().chain(&fh).chain(&ph).chain(&sources).chain(&pressure).chain(&u).any(|v| !v.is_finite())
        {
            return contract("All history arrays must be finite");
        }
        let elementwise = |k: &str| f64_shaped(&p[k], &[ne]).filter(|v| v.iter().all(Scalar::is_finite));
        let (Some(alpha), Some(biot), Some(mobility)) = (
            elementwise("biot_coefficient"),
            elementwise("biot_modulus_Pa"),
            elementwise("reference_mobility_m2_Pa_s"),
        ) else {
            return contract(
                "Elementwise alpha in [0,1], positive Biot modulus and nonnegative reference mobility required",
            );
        };
        if alpha.iter().any(|a| !(0.0..=1.0).contains(a))
            || biot.iter().any(|m| *m <= 0.0)
            || mobility.iter().any(|m| *m < 0.0)
        {
            return contract(
                "Elementwise alpha in [0,1], positive Biot modulus and nonnegative reference mobility required",
            );
        }
        let (mu, lam) = (
            real_array(&p["shear_Pa"]).map(|x| x.1).unwrap_or_default(),
            real_array(&p["lame_Pa"]).map(|x| x.1).unwrap_or_default(),
        );
        let fixed_dofs =
            bool_array(&p["fixed_dofs"]).filter(|(s, _)| s == &[n, 3]).map(|x| x.1).unwrap_or_default();
        let (ms, moduli) = real_array(&p["branch_moduli_Pa"]).unwrap_or_default();
        let (_, times) = real_array(&p["relaxation_times_s"]).unwrap_or_default();
        let (mems, memory) = real_array(&p["initial_branch_strain"]).unwrap_or_default();
        let nb = if ms.len() == 2 { ms[1] } else { 0 };
        let memory_ok = mems.len() == 4 && mems[0] == ne && mems[1] == nb && mems[2] == 3 && mems[3] == 3;
        let problem = Self {
            mu,
            lam,
            fixed_dofs,
            prescribed: rows(&uh, 3 * n),
            force: rows(&fh, 3 * n),
            steps,
            moduli: rows(&moduli, nb),
            times: rows(&times, nb),
            memory: if memory_ok {
                mats(&memory).chunks(nb.max(1)).map(<[Mat3<f64>]>::to_vec).collect()
            } else {
                Vec::new()
            },
            u0: u,
            alpha,
            biot,
            mobility,
            p0: pressure,
            fixed_pressure: fixedp,
            pressure_history: rows(&ph, n),
            source: rows(&sources, n),
            mesh,
        };
        let vs = ViscoelasticState {
            memory: problem.memory.clone(),
            moduli: problem.moduli.clone(),
            times: problem.times.clone(),
            step: problem.steps[0],
        };
        StaticProblem {
            mesh: &problem.mesh,
            mu: &problem.mu,
            lam: &problem.lam,
            fixed: &problem.fixed_dofs,
            prescribed: &problem.prescribed[0],
            force: &problem.force[0],
            initial: Some(&problem.u0),
            viscoelastic: Some(&vs),
            options: StaticOptions::default(),
        }
        .validate()?;
        if !problem.steps.iter().sum::<f64>().is_finite() {
            return contract("Time grid overflows");
        }
        let j = problem.jacobians(&problem.u0);
        if j.iter().any(|v| !v.is_finite()) || j.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0 {
            return contract("Initial deformation is inverted");
        }
        Ok(problem)
    }

    fn jacobians(&self, u: &[f64]) -> Vec<f64> {
        (0..self.mesh.elements.len())
            .map(|e| det(&self.mesh.deformation_gradient(e, &local_u(u, &self.mesh.elements[e]))))
            .collect()
    }

    fn scales(&self) -> (f64, f64, f64) {
        let n = self.mesh.node_count();
        let length = (0..3)
            .map(|a| {
                let (lo, hi) = self
                    .mesh
                    .points
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), p| (l.min(p[a]), h.max(p[a])));
                hi - lo
            })
            .fold(f64::NEG_INFINITY, f64::max);
        let pref =
            self.mu.iter().chain(&self.lam).chain(&self.biot).copied().fold(f64::NEG_INFINITY, f64::max);
        let _ = n;
        (length, pref, self.mesh.volumes.iter().sum::<f64>() * pref)
    }
}

pub(crate) fn local_u<S: Scalar>(state: &[S], t: &[usize; 4]) -> [[S; 3]; 4] {
    std::array::from_fn(|a| std::array::from_fn(|i| state[3 * t[a] + i]))
}

#[derive(Debug, Clone)]
pub struct PoroParams<S> {
    pub mu: S,
    pub lam: S,
    pub g: Vec<S>,
    pub tau: Vec<S>,
    pub alpha: S,
    pub biot: S,
    pub mobility: S,
}

pub fn element_residual<S: Scalar>(
    mesh: &TetMesh,
    e: usize,
    u: &[[S; 3]; 4],
    p: &[S; 4],
    m: &PoroParams<S>,
    memory: &[Mat3<S>],
    dt: f64,
) -> [S; 16] {
    let f = mesh.deformation_gradient(e, u);
    let j = det(&f);
    let inv_t = inverse_transpose(&f);
    let mut piola = neo_hookean_piola(&f, m.mu, m.lam);
    let pb = maxwell_potential_piola(&f, memory, &m.g, &m.tau, S::from_f64(dt));
    let pbar = (p[0] + p[1] + p[2] + p[3]) / 4.0;
    for r in 0..3 {
        for c in 0..3 {
            piola[r][c] += pb[r][c] - m.alpha * pbar * j * inv_t[r][c];
        }
    }
    let forces = mesh.nodal_forces(e, &piola);
    let (s, k) = mesh.hydraulic_local(e, m.biot, m.mobility);
    let v = mesh.volumes[e];
    let mut out = [S::zero(); 16];
    for a in 0..4 {
        for i in 0..3 {
            out[3 * a + i] = forces[a][i];
        }
        let mut acc = -(m.alpha * (j - 1.0) * v / 4.0);
        for b in 0..4 {
            acc = acc - s[a][b] * p[b] - k[a][b] * p[b] * dt;
        }
        out[12 + a] = acc;
    }
    out
}

pub fn nominal_stress<S: Scalar>(f: &Mat3<S>, pbar: S, m: &PoroParams<S>, memory: &[Mat3<S>]) -> Mat3<S> {
    let j = det(f);
    let inv_t = inverse_transpose(f);
    let e = super::kinematics::green_strain(f);
    let mut second = [[S::zero(); 3]; 3];
    for (b, mem) in memory.iter().enumerate() {
        for r in 0..3 {
            for c in 0..3 {
                second[r][c] += m.g[b] * (e[r][c] - mem[r][c]);
            }
        }
    }
    let branch = matmul(f, &second);
    let lj = j.ln();
    std::array::from_fn(|r| {
        std::array::from_fn(|c| {
            m.mu * (f[r][c] - inv_t[r][c]) + m.lam * lj * inv_t[r][c] + branch[r][c]
                - m.alpha * pbar * j * inv_t[r][c]
        })
    })
}

impl PoroProblem {
    #[must_use]
    pub fn params(&self, e: usize) -> PoroParams<f64> {
        PoroParams {
            mu: self.mu[e],
            lam: self.lam[e],
            g: self.moduli[e].clone(),
            tau: self.times[e].clone(),
            alpha: self.alpha[e],
            biot: self.biot[e],
            mobility: self.mobility[e],
        }
    }

    #[must_use]
    pub fn content(&self, u: &[f64], p: &[f64]) -> Vec<f64> {
        let mut z = vec![0.0; p.len()];
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let f = self.mesh.deformation_gradient(e, &local_u(u, t));
            let j = det(&f);
            let (s, _) = self.mesh.hydraulic_local(e, self.biot[e], self.mobility[e]);
            for a in 0..4 {
                let mut acc = 0.0;
                for b in 0..4 {
                    acc += s[a][b] * p[t[b]];
                }
                z[t[a]] += acc + self.mesh.volumes[e] * self.alpha[e] * (j - 1.0) / 4.0;
            }
        }
        z
    }

    fn residual(
        &self,
        state: &[f64],
        memory: &[Vec<Mat3<f64>>],
        zold: &[f64],
        dt: f64,
        force: &[f64],
        source: &[f64],
    ) -> Vec<f64> {
        let n = self.mesh.node_count();
        let mut r = vec![0.0; 4 * n];
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let u = local_u(state, t);
            let p: [f64; 4] = std::array::from_fn(|a| state[3 * n + t[a]]);
            let out = element_residual(&self.mesh, e, &u, &p, &self.params(e), &memory[e], dt);
            for a in 0..4 {
                for i in 0..3 {
                    r[3 * t[a] + i] += out[3 * a + i];
                }
                r[3 * n + t[a]] += out[12 + a];
            }
        }
        for i in 0..3 * n {
            r[i] -= force[i];
        }
        for a in 0..n {
            r[3 * n + a] += zold[a] + dt * source[a];
        }
        r
    }

    fn tangent(&self, state: &[f64], memory: &[Vec<Mat3<f64>>], dt: f64) -> Result<DenseMatrix, CaeError> {
        let n = self.mesh.node_count();
        let size = 4 * n;
        let mut h = DenseMatrix::zeros(size, size);
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let dofs: Vec<usize> = t
                .iter()
                .flat_map(|node| (0..3).map(move |i| 3 * node + i))
                .chain(t.iter().map(|node| 3 * n + node))
                .collect();
            let local: Vec<f64> = dofs.iter().map(|d| state[*d]).collect();
            let params = self.params(e);
            let jac = implexity_ad::forward::jacobian::<16, _>(
                |s: &[Dual<16>]| {
                    let u: [[Dual<16>; 3]; 4] =
                        std::array::from_fn(|a| std::array::from_fn(|i| s[3 * a + i]));
                    let p: [Dual<16>; 4] = std::array::from_fn(|a| s[12 + a]);
                    let m = lift_params(&params);
                    let mem: Vec<Mat3<Dual<16>>> =
                        memory[e].iter().map(|x| x.map(|r| r.map(Dual::constant))).collect();
                    element_residual(&self.mesh, e, &u, &p, &m, &mem, dt).to_vec()
                },
                &local,
            )
            .map_err(|err| CaeError::contract(err.to_string()))?;
            for (a, ra) in dofs.iter().enumerate() {
                for (b, cb) in dofs.iter().enumerate() {
                    h.data[ra * size + cb] += jac.matrix[a * 16 + b];
                }
            }
        }
        Ok(h)
    }
}

fn lift_params<S: Scalar>(m: &PoroParams<f64>) -> PoroParams<S> {
    PoroParams {
        mu: S::from_f64(m.mu),
        lam: S::from_f64(m.lam),
        g: m.g.iter().map(|v| S::from_f64(*v)).collect(),
        tau: m.tau.iter().map(|v| S::from_f64(*v)).collect(),
        alpha: S::from_f64(m.alpha),
        biot: S::from_f64(m.biot),
        mobility: S::from_f64(m.mobility),
    }
}

#[derive(Debug, Clone)]
pub struct PoroStep {
    pub displacement: Vec<f64>,
    pub pressure: Vec<f64>,
    pub cauchy_stress: Vec<Mat3<f64>>,
    pub jacobian: Vec<f64>,
    pub branch_strain: Vec<Vec<Mat3<f64>>>,
    pub support_reactions: Vec<f64>,
    pub fluid_content: f64,
    pub reservoir_exchange: Vec<f64>,
    pub free_fluid_balance: f64,
    pub global_fluid_balance: f64,
    pub darcy_flux: Vec<[f64; 3]>,
    pub darcy_dissipation: f64,
    pub viscoelastic_dissipation: f64,
    pub dissipation_density: Vec<f64>,
    pub stored_energy: f64,
    pub mechanical_work: f64,
    pub first_piola: Vec<Mat3<f64>>,
    pub deformation_gradient: Vec<Mat3<f64>>,
    pub kinematic_cycle_closure: f64,
    pub scaled_free_residual: f64,
    pub iterations: usize,
}

#[derive(Debug, Clone)]
pub struct PoroHistory {
    pub steps: Vec<PoroStep>,
    pub times: Vec<f64>,
    pub initial_content: f64,
}

impl PoroProblem {
    #[must_use]
    pub fn fixed_state(&self) -> Vec<bool> {
        self.fixed_dofs.iter().chain(&self.fixed_pressure).copied().collect()
    }


    #[allow(clippy::too_many_lines)]
    pub fn solve_history(&self) -> Result<PoroHistory, CaeError> {
        let n = self.mesh.node_count();
        let ne = self.mesh.elements.len();
        let (length, pref, energy_scale) = self.scales();
        let scale: Vec<f64> = (0..4 * n).map(|i| if i < 3 * n { length } else { pref }).collect();
        let fixed = self.fixed_state();
        let free: Vec<usize> = (0..4 * n).filter(|i| !fixed[*i]).collect();
        let mut u = self.u0.clone();
        let mut pressure = self.p0.clone();
        let mut memory = self.memory.clone();
        let mut zold = self.content(&u, &pressure);
        let initial_content: f64 = zold.iter().sum();
        let fgrad = |u: &[f64]| -> Vec<Mat3<f64>> {
            (0..ne).map(|e| self.mesh.deformation_gradient(e, &local_u(u, &self.mesh.elements[e]))).collect()
        };
        let initial_f = fgrad(&u);
        let mut old_f = initial_f.clone();
        let pbar = |p: &[f64], t: &[usize; 4]| (p[t[0]] + p[t[1]] + p[t[2]] + p[t[3]]) / 4.0;
        let mut old_p: Vec<Mat3<f64>> = (0..ne)
            .map(|e| {
                nominal_stress(
                    &old_f[e],
                    pbar(&pressure, &self.mesh.elements[e]),
                    &self.params(e),
                    &memory[e],
                )
            })
            .collect();
        let mut work = 0.0;
        let mut records = Vec::with_capacity(self.steps.len());
        for (i, dt) in self.steps.iter().copied().enumerate() {
            let mut state: Vec<f64> = u.iter().chain(&pressure).copied().collect();
            let target: Vec<f64> =
                self.prescribed[i].iter().chain(&self.pressure_history[i]).copied().collect();
            for k in 0..4 * n {
                if fixed[k] {
                    state[k] = target[k];
                }
            }
            let mut y: Vec<f64> = state.iter().zip(&scale).map(|(s, c)| s / c).collect();
            let residual_y = |y: &[f64]| -> Vec<f64> {
                let st: Vec<f64> = y.iter().zip(&scale).map(|(a, c)| a * c).collect();
                self.residual(&st, &memory, &zold, dt, &self.force[i], &self.source[i])
                    .iter()
                    .zip(&scale)
                    .map(|(r, c)| r * c / energy_scale)
                    .collect()
            };
            let mut initial_norm: Option<f64> = None;
            let mut norm;
            let mut iteration = 0;
            let mut r;
            loop {
                let st: Vec<f64> = y.iter().zip(&scale).map(|(a, c)| a * c).collect();
                let j = self.jacobians(&st[..3 * n]);
                if j.iter().any(|v| !v.is_finite()) || j.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0
                {
                    return contract("Nonpositive deformed Jacobian in coupled history");
                }
                r = residual_y(&y);
                norm = free.iter().map(|k| r[*k].abs()).fold(0.0, f64::max);
                if r.iter().any(|v| !v.is_finite()) {
                    return contract("Nonfinite coupled residual");
                }
                let initial = *initial_norm.get_or_insert(norm);
                if norm <= 1e-12 + 1e-9 * initial {
                    break;
                }
                if iteration == 40 {
                    return contract("Coupled Newton iteration limit");
                }
                let h = self.tangent(&st, &memory, dt)?;
                let size = 4 * n;
                let k = DenseMatrix {
                    nrows: free.len(),
                    ncols: free.len(),
                    data: free
                        .iter()
                        .flat_map(|a| {
                            free.iter()
                                .map(|b| h.data[a * size + b] * scale[*a] * scale[*b] / energy_scale)
                                .collect::<Vec<_>>()
                        })
                        .collect(),
                };
                let rhs: Vec<f64> = free.iter().map(|a| -r[*a]).collect();
                let direction = implexity_linalg::dense::solve(&k, &rhs, 1)
                    .map_err(|_| CaeError::contract("Singular coupled tangent"))?;
                let mut accepted = false;
                for backtrack in 0..24 {
                    let step = 2.0_f64.powi(-backtrack);
                    let mut trial = y.clone();
                    for (a, d) in free.iter().zip(&direction) {
                        trial[*a] += step * d;
                    }
                    let v: Vec<f64> = trial[..3 * n].iter().zip(&scale).map(|(a, c)| a * c).collect();
                    let jt = self.jacobians(&v);
                    if jt.iter().all(Scalar::is_finite)
                        && jt.iter().copied().fold(f64::INFINITY, f64::min) > 0.0
                    {
                        let rt = residual_y(&trial);
                        let rt_free: Vec<f64> = free.iter().map(|a| rt[*a]).collect();
                        if rt_free.iter().all(Scalar::is_finite)
                            && rt_free.iter().map(|x| x.abs()).fold(0.0, f64::max)
                                < (1.0 - 1e-4 * step) * norm
                        {
                            y = trial;
                            accepted = true;
                            break;
                        }
                    }
                }
                if !accepted {
                    return contract("Coupled residual line search failed");
                }
                iteration += 1;
            }
            let st: Vec<f64> = y.iter().zip(&scale).map(|(a, c)| a * c).collect();
            u = st[..3 * n].to_vec();
            pressure = st[3 * n..].to_vec();
            let f = fgrad(&u);
            let mut new_memory = Vec::with_capacity(ne);
            let mut stress = Vec::with_capacity(ne);
            let mut jac = Vec::with_capacity(ne);
            let mut piola = Vec::with_capacity(ne);
            let (mut visco_diss, mut branch_stored) = (0.0, 0.0);
            let mut density = Vec::with_capacity(ne);
            let mut flux = Vec::with_capacity(ne);
            for e in 0..ne {
                let t = &self.mesh.elements[e];
                let params = self.params(e);
                let update = green_maxwell_step(&f[e], &memory[e], &params.g, &params.tau, dt);
                let j = det(&f[e]);
                let mut s = neo_hookean_cauchy(&f[e], params.mu, params.lam);
                let extra = matmul(&update.first_piola, &transpose(&f[e]));
                let pb = pbar(&pressure, t);
                for r in 0..3 {
                    for c in 0..3 {
                        s[r][c] += extra[r][c] / j - if r == c { params.alpha * pb } else { 0.0 };
                    }
                }
                stress.push(s);
                jac.push(j);
                let g = &self.mesh.gradients[e];
                let grad_p: [f64; 3] =
                    std::array::from_fn(|c| (0..4).map(|a| pressure[t[a]] * g[a][c]).sum());
                flux.push(grad_p.map(|v| -params.mobility * v));
                density.push(
                    update.dissipated + dt * params.mobility * grad_p.iter().map(|v| v * v).sum::<f64>(),
                );
                visco_diss += self.mesh.volumes[e] * update.dissipated;
                branch_stored += self.mesh.volumes[e] * update.stored;
                new_memory.push(update.branch_strain);
            }
            memory = new_memory;
            let znew = self.content(&u, &pressure);
            let kp = self.stiffness_product(&pressure, false);
            let balance: Vec<f64> =
                (0..n).map(|a| znew[a] - zold[a] + dt * (kp[a] - self.source[i][a])).collect();
            let exchange: Vec<f64> =
                (0..n).map(|a| if self.fixed_pressure[a] { balance[a] } else { 0.0 }).collect();
            for e in 0..ne {
                let t = &self.mesh.elements[e];
                piola.push(nominal_stress(&f[e], pbar(&pressure, t), &self.params(e), &memory[e]));
            }
            for e in 0..ne {
                let mut acc = 0.0;
                for r in 0..3 {
                    for c in 0..3 {
                        acc += (old_p[e][r][c] + piola[e][r][c]) * (f[e][r][c] - old_f[e][r][c]) / 2.0;
                    }
                }
                work += self.mesh.volumes[e] * acc;
            }
            old_f.clone_from(&f);
            old_p.clone_from(&piola);
            let force_residual: Vec<f64> = r[..3 * n].iter().map(|v| v * energy_scale / length).collect();
            let reaction: Vec<f64> =
                (0..3 * n).map(|k| if self.fixed_dofs[k] { force_residual[k] } else { 0.0 }).collect();
            let sp = self.stiffness_product(&pressure, true);
            let elastic: f64 = (0..ne)
                .map(|e| self.mesh.volumes[e] * neo_hookean_energy(&f[e], self.mu[e], self.lam[e]))
                .sum();
            let pk: f64 = pressure.iter().zip(&kp).map(|(a, b)| a * b).sum();
            let psp: f64 = pressure.iter().zip(&sp).map(|(a, b)| a * b).sum();
            let zsum: f64 = znew.iter().sum();
            let free_balance =
                (0..n).filter(|a| !self.fixed_pressure[*a]).map(|a| balance[a].abs()).fold(0.0, f64::max);
            let closure = f
                .iter()
                .zip(&initial_f)
                .flat_map(|(a, b)| (0..9).map(move |k| (a[k / 3][k % 3] - b[k / 3][k % 3]).abs()))
                .fold(0.0, f64::max);
            records.push(PoroStep {
                displacement: u.clone(),
                pressure: pressure.clone(),
                cauchy_stress: stress,
                jacobian: jac,
                branch_strain: memory.clone(),
                support_reactions: reaction,
                fluid_content: zsum,
                reservoir_exchange: exchange.clone(),
                free_fluid_balance: free_balance,
                global_fluid_balance: zsum
                    - zold.iter().sum::<f64>()
                    - dt * self.source[i].iter().sum::<f64>()
                    - exchange.iter().sum::<f64>(),
                darcy_flux: flux,
                darcy_dissipation: dt * pk,
                viscoelastic_dissipation: visco_diss,
                dissipation_density: density,
                stored_energy: elastic + branch_stored + 0.5 * psp,
                mechanical_work: work,
                first_piola: piola,
                deformation_gradient: f,
                kinematic_cycle_closure: closure,
                scaled_free_residual: norm,
                iterations: iteration,
            });
            zold = znew;
        }
        let mut times = Vec::with_capacity(self.steps.len());
        let mut acc = 0.0;
        for s in &self.steps {
            acc += s;
            times.push(acc);
        }
        Ok(PoroHistory { steps: records, times, initial_content })
    }

    #[must_use]
    pub fn stiffness_product(&self, p: &[f64], storage: bool) -> Vec<f64> {
        let mut out = vec![0.0; p.len()];
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let (s, k) = self.mesh.hydraulic_local(e, self.biot[e], self.mobility[e]);
            let m = if storage { s } else { k };
            for a in 0..4 {
                for b in 0..4 {
                    out[t[a]] += m[a][b] * p[t[b]];
                }
            }
        }
        out
    }
}


pub fn material_sensitivity(
    problem: &PoroProblem,
    material: &str,
    response: &str,
) -> Result<(f64, Vec<f64>), CaeError> {
    if !MATERIALS.contains(&material) || !RESPONSES.contains(&response) {
        return contract("Unsupported coupled material derivative");
    }
    let admitted = problem.solve_history()?;
    let (value, gradient) = PoroTape::new(problem, material).run(&admitted, response)?;
    if !value.is_finite() || gradient.iter().any(|g| !g.is_finite()) {
        return contract("Nonfinite coupled material derivative; tangent may be singular");
    }
    Ok((value, gradient))
}

struct PoroTape<'a> {
    problem: &'a PoroProblem,
    material: &'a str,
    nb: usize,
}

fn param_width(nb: usize) -> usize {
    5 + 2 * nb
}

fn params_from<S: Scalar>(local: &[S], nb: usize) -> PoroParams<S> {
    PoroParams {
        mu: local[0],
        lam: local[1],
        g: local[2..2 + nb].to_vec(),
        tau: local[2 + nb..2 + 2 * nb].to_vec(),
        alpha: local[2 + 2 * nb],
        biot: local[3 + 2 * nb],
        mobility: local[4 + 2 * nb],
    }
}

fn mem_from<S: Scalar>(local: &[S], nb: usize) -> Vec<Mat3<S>> {
    (0..nb).map(|b| std::array::from_fn(|r| std::array::from_fn(|c| local[9 * b + 3 * r + c]))).collect()
}

#[derive(Clone)]
struct PoroKernels {
    mesh: Arc<TetMesh>,
    nb: usize,
    dt: f64,
    ne: usize,
}

struct RootKernel(PoroKernels);

impl Kernel for RootKernel {
    fn n_out(&self) -> usize {
        16
    }
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]) {
        let k = &self.0;
        if e >= k.ne {
            out.fill(S::zero());
            out[0] = local[0];
            return;
        }
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[3 * a + i]));
        let p: [S; 4] = std::array::from_fn(|a| local[12 + a]);
        let mem = mem_from(&local[16..16 + 9 * k.nb], k.nb);
        let params = params_from(&local[16 + 9 * k.nb..], k.nb);
        out.copy_from_slice(&element_residual(&k.mesh, e, &u, &p, &params, &mem, k.dt));
    }
}

struct MemoryKernel(PoroKernels);

impl Kernel for MemoryKernel {
    fn n_out(&self) -> usize {
        9 * self.0.nb
    }
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[3 * a + i]));
        let mem = mem_from(&local[12..12 + 9 * k.nb], k.nb);
        let params = params_from(&local[12 + 9 * k.nb..], k.nb);
        let f = k.mesh.deformation_gradient(e, &u);
        let update = green_maxwell_step(&f, &mem, &params.g, &params.tau, S::from_f64(k.dt));
        for (b, m) in update.branch_strain.iter().enumerate() {
            for r in 0..3 {
                for c in 0..3 {
                    out[9 * b + 3 * r + c] = m[r][c];
                }
            }
        }
    }
}

struct ContentKernel(PoroKernels);

impl Kernel for ContentKernel {
    fn n_out(&self) -> usize {
        4
    }
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[3 * a + i]));
        let p: [S; 4] = std::array::from_fn(|a| local[12 + a]);
        let params = params_from(&local[16..], k.nb);
        let j = det(&k.mesh.deformation_gradient(e, &u));
        let (s, _) = k.mesh.hydraulic_local(e, params.biot, params.mobility);
        let v = k.mesh.volumes[e];
        for a in 0..4 {
            let mut acc = params.alpha * (j - 1.0) * v / 4.0;
            for b in 0..4 {
                acc += s[a][b] * p[b];
            }
            out[a] = acc;
        }
    }
}

struct PiolaKernel(PoroKernels);

impl Kernel for PiolaKernel {
    fn n_out(&self) -> usize {
        9
    }
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[3 * a + i]));
        let pbar = (local[12] + local[13] + local[14] + local[15]) / 4.0;
        let mem = mem_from(&local[16..16 + 9 * k.nb], k.nb);
        let params = params_from(&local[16 + 9 * k.nb..], k.nb);
        let f = k.mesh.deformation_gradient(e, &u);
        let p = nominal_stress(&f, pbar, &params, &mem);
        for r in 0..3 {
            for c in 0..3 {
                out[3 * r + c] = p[r][c];
            }
        }
    }
}

struct WorkKernel(PoroKernels);

impl Kernel for WorkKernel {
    fn n_out(&self) -> usize {
        1
    }
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]) {
        let k = &self.0;
        let un: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[3 * a + i]));
        let uo: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[12 + 3 * a + i]));
        let (fnew, fold) = (k.mesh.deformation_gradient(e, &un), k.mesh.deformation_gradient(e, &uo));
        let mut acc = S::zero();
        for r in 0..3 {
            for c in 0..3 {
                acc += (local[24 + 3 * r + c] + local[33 + 3 * r + c]) * (fnew[r][c] - fold[r][c]) / 2.0;
            }
        }
        out[0] = acc * k.mesh.volumes[e];
    }
}

struct DissipationKernel(PoroKernels);

impl Kernel for DissipationKernel {
    fn n_out(&self) -> usize {
        1
    }
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[3 * a + i]));
        let p: [S; 4] = std::array::from_fn(|a| local[12 + a]);
        let mem = mem_from(&local[16..16 + 9 * k.nb], k.nb);
        let params = params_from(&local[16 + 9 * k.nb..], k.nb);
        let f = k.mesh.deformation_gradient(e, &u);
        let update = green_maxwell_step(&f, &mem, &params.g, &params.tau, S::from_f64(k.dt));
        let (_, kk) = k.mesh.hydraulic_local(e, params.biot, params.mobility);
        let mut pkp = S::zero();
        for a in 0..4 {
            for b in 0..4 {
                pkp += p[a] * kk[a][b] * p[b];
            }
        }
        out[0] = pkp * k.dt + update.dissipated * k.mesh.volumes[e];
    }
}

struct StoredKernel(PoroKernels);

impl Kernel for StoredKernel {
    fn n_out(&self) -> usize {
        1
    }
    fn eval<S: Scalar>(&self, e: usize, local: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| local[3 * a + i]));
        let p: [S; 4] = std::array::from_fn(|a| local[12 + a]);
        let mem = mem_from(&local[16..16 + 9 * k.nb], k.nb);
        let params = params_from(&local[16 + 9 * k.nb..], k.nb);
        let f = k.mesh.deformation_gradient(e, &u);
        let update = green_maxwell_step(&f, &mem, &params.g, &params.tau, S::from_f64(k.dt));
        let (s, _) = k.mesh.hydraulic_local(e, params.biot, params.mobility);
        let mut psp = S::zero();
        for a in 0..4 {
            for b in 0..4 {
                psp += p[a] * s[a][b] * p[b];
            }
        }
        let v = k.mesh.volumes[e];
        out[0] = neo_hookean_energy(&f, params.mu, params.lam) * v + update.stored * v + psp * 0.5;
    }
}

impl<'a> PoroTape<'a> {
    fn new(problem: &'a PoroProblem, material: &'a str) -> Self {
        let nb = problem.moduli.first().map_or(0, Vec::len);
        Self { problem, material, nb }
    }

    fn kernels(&self, dt: f64) -> PoroKernels {
        PoroKernels {
            mesh: Arc::new(self.problem.mesh.clone()),
            nb: self.nb,
            dt,
            ne: self.problem.mesh.elements.len(),
        }
    }

    fn state_blocks(&self, with_pressure: bool) -> Vec<Vec<usize>> {
        let n = self.problem.mesh.node_count();
        self.problem
            .mesh
            .elements
            .iter()
            .map(|t| {
                let mut idx: Vec<usize> =
                    t.iter().flat_map(|node| (0..3).map(move |i| 3 * node + i)).collect();
                if with_pressure {
                    idx.extend(t.iter().map(|node| 3 * n + node));
                }
                idx
            })
            .collect()
    }

    fn range_blocks(&self, width: usize) -> Vec<Vec<usize>> {
        (0..self.problem.mesh.elements.len()).map(|e| (e * width..(e + 1) * width).collect()).collect()
    }

    fn run(&self, admitted: &PoroHistory, response: &str) -> Result<(f64, Vec<f64>), CaeError> {
        let p = self.problem;
        let (n, ne, nb) = (p.mesh.node_count(), p.mesh.elements.len(), self.nb);
        let pw = param_width(nb);
        let mut tape = Tape::new();

        let theta_value: Vec<f64> = match self.material {
            "shear_Pa" => p.mu.clone(),
            "lame_Pa" => p.lam.clone(),
            "branch_moduli_Pa" => p.moduli.iter().flatten().copied().collect(),
            "relaxation_times_s" => p.times.iter().flatten().copied().collect(),
            "biot_coefficient" => p.alpha.clone(),
            "biot_modulus_Pa" => p.biot.clone(),
            _ => p.mobility.clone(),
        };
        let theta = tape.input(theta_value);
        let mut base = Vec::with_capacity(ne * pw);
        for e in 0..ne {
            base.push(p.mu[e]);
            base.push(p.lam[e]);
            base.extend(&p.moduli[e]);
            base.extend(&p.times[e]);
            base.push(p.alpha[e]);
            base.push(p.biot[e]);
            base.push(p.mobility[e]);
        }
        let base = tape.constant(base);

        let offset: (usize, usize) = match self.material {
            "shear_Pa" => (0, 1),
            "lame_Pa" => (1, 1),
            "branch_moduli_Pa" => (2, nb),
            "relaxation_times_s" => (2 + nb, nb),
            "biot_coefficient" => (2 + 2 * nb, 1),
            "biot_modulus_Pa" => (3 + 2 * nb, 1),
            _ => (4 + 2 * nb, 1),
        };
        let params = {
            struct Select {
                pw: usize,
                start: usize,
                len: usize,
            }
            impl Kernel for Select {
                fn n_out(&self) -> usize {
                    self.pw
                }
                fn eval<S: Scalar>(&self, _e: usize, local: &[S], out: &mut [S]) {
                    out.copy_from_slice(&local[..self.pw]);
                    out[self.start..self.start + self.len]
                        .copy_from_slice(&local[self.pw..self.pw + self.len]);
                }
            }
            let blocks = vec![
                self.range_blocks(pw),
                (0..ne).map(|e| (e * offset.1..(e + 1) * offset.1).collect()).collect(),
            ];
            element_node(
                &mut tape,
                Arc::new(Select { pw, start: offset.0, len: offset.1 }),
                &[base, theta],
                Gather::from_blocks(&blocks),
            )?
        };
        let memory0 =
            tape.constant(p.memory.iter().flatten().flat_map(|m| m.iter().flatten().copied()).collect());
        let state0 = tape.constant(p.u0.iter().chain(&p.p0).copied().collect());
        let k0 = self.kernels(p.steps.first().copied().unwrap_or(1.0));
        let content = |tape: &mut Tape, state: Var, k: &PoroKernels| -> Result<Var, CaeError> {
            let blocks = vec![self.state_blocks(true), self.range_blocks(pw)];
            let local = element_node(
                tape,
                Arc::new(ContentKernel(k.clone())),
                &[state, params],
                Gather::from_blocks(&blocks),
            )?;
            let idx: Vec<usize> = p.mesh.elements.iter().flat_map(|t| t.iter().copied()).collect();
            tape.scatter_add(local, idx, n).map_err(|e| CaeError::contract(e.to_string()))
        };
        let piola = |tape: &mut Tape, state: Var, memory: Var, k: &PoroKernels| -> Result<Var, CaeError> {
            let blocks = vec![self.state_blocks(true), self.range_blocks(9 * nb), self.range_blocks(pw)];
            element_node(
                tape,
                Arc::new(PiolaKernel(k.clone())),
                &[state, memory, params],
                Gather::from_blocks(&blocks),
            )
        };
        let mut zold = content(&mut tape, state0, &k0)?;
        let mut memory = memory0;
        let mut old_state = state0;
        let mut old_p = piola(&mut tape, state0, memory0, &k0)?;
        let mut dissipation: Option<Var> = None;
        let mut work: Option<Var> = None;
        let fixed = p.fixed_state();
        let free: Vec<usize> = (0..4 * n).filter(|i| !fixed[*i]).collect();
        let mut last = (state0, memory0, k0.clone());
        for (i, record) in admitted.steps.iter().enumerate() {
            let k = self.kernels(p.steps[i]);
            let y: Vec<f64> = record.displacement.iter().chain(&record.pressure).copied().collect();
            let mut gather_map = Vec::with_capacity(ne + n);
            let mut rows = Vec::with_capacity(ne + n);
            for (e, t) in p.mesh.elements.iter().enumerate() {
                let mut local: Vec<(usize, usize)> =
                    t.iter().flat_map(|node| (0..3).map(move |c| (0, 3 * node + c))).collect();
                local.extend(t.iter().map(|node| (0, 3 * n + node)));
                local.extend((0..9 * nb).map(|j| (1, e * 9 * nb + j)));
                local.extend((0..pw).map(|j| (3, e * pw + j)));
                gather_map.push(local);
                let mut r: Vec<Option<usize>> =
                    t.iter().flat_map(|node| (0..3).map(move |c| Some(3 * node + c))).collect();
                r.extend(t.iter().map(|node| Some(3 * n + node)));
                rows.push(r);
            }
            for a in 0..n {
                gather_map.push(vec![(2, a)]);
                let mut r = vec![None; 16];
                r[0] = Some(3 * n + a);
                rows.push(r);
            }
            let spec = RootSpec {
                kernel: Arc::new(RootKernel(k.clone())),
                gather: Gather { map: gather_map },
                rows,
                free: free.clone(),
                size: 4 * n,
            };
            let state = root_node(&mut tape, spec, y, &[memory, zold, params])?;
            let diss = {
                let blocks = vec![self.state_blocks(true), self.range_blocks(9 * nb), self.range_blocks(pw)];
                let d = element_node(
                    &mut tape,
                    Arc::new(DissipationKernel(k.clone())),
                    &[state, memory, params],
                    Gather::from_blocks(&blocks),
                )?;
                tape.sum(d).map_err(|e| CaeError::contract(e.to_string()))?
            };
            dissipation = Some(match dissipation {
                None => diss,
                Some(prev) => tape.add(prev, diss).map_err(|e| CaeError::contract(e.to_string()))?,
            });
            let new_memory = {
                let blocks = vec![self.state_blocks(false), self.range_blocks(9 * nb), self.range_blocks(pw)];
                element_node(
                    &mut tape,
                    Arc::new(MemoryKernel(k.clone())),
                    &[state, memory, params],
                    Gather::from_blocks(&blocks),
                )?
            };
            last = (state, memory, k.clone());
            memory = new_memory;
            zold = content(&mut tape, state, &k)?;
            let pnew = piola(&mut tape, state, memory, &k)?;
            let w = {
                let blocks = vec![
                    self.state_blocks(false),
                    self.state_blocks(false),
                    self.range_blocks(9),
                    self.range_blocks(9),
                ];
                let d = element_node(
                    &mut tape,
                    Arc::new(WorkKernel(k.clone())),
                    &[state, old_state, pnew, old_p],
                    Gather::from_blocks(&blocks),
                )?;
                tape.sum(d).map_err(|e| CaeError::contract(e.to_string()))?
            };
            work = Some(match work {
                None => w,
                Some(prev) => tape.add(prev, w).map_err(|e| CaeError::contract(e.to_string()))?,
            });
            old_state = state;
            old_p = pnew;
        }
        let map_err = |e: implexity_ad::AdError| CaeError::contract(e.to_string());
        let (final_state, final_memory, final_k) = last;
        let output = match response {
            "dissipation_J" => dissipation.ok_or_else(|| CaeError::contract("empty history"))?,
            "mechanical_work_J" => work.ok_or_else(|| CaeError::contract("empty history"))?,
            "final_fluid_content_increment_m3" => tape.sum(zold).map_err(map_err)?,
            "final_displacement_squared_m2" => {
                let u = tape.slice(final_state, 0, 3 * n).map_err(map_err)?;
                tape.dot(u, u).map_err(map_err)?
            }
            _ => {
                let blocks = vec![self.state_blocks(true), self.range_blocks(9 * nb), self.range_blocks(pw)];
                let s = element_node(
                    &mut tape,
                    Arc::new(StoredKernel(final_k)),
                    &[final_state, final_memory, params],
                    Gather::from_blocks(&blocks),
                )?;
                tape.sum(s).map_err(map_err)?
            }
        };
        let value = tape.value(output).map_err(map_err)?[0];
        let grads = tape.vjp(output, &[1.0]).map_err(map_err)?;
        Ok((value, grads.wrt(theta).map_err(map_err)?))
    }
}

#[must_use]
pub fn step_json(s: &PoroStep) -> Value {
    json!({"fluid_content_increment_m3": s.fluid_content, "darcy_dissipation_J": s.darcy_dissipation,
        "viscoelastic_dissipation_J": s.viscoelastic_dissipation, "stored_energy_J": s.stored_energy,
        "mechanical_work_J": s.mechanical_work, "kinematic_cycle_closure": s.kinematic_cycle_closure,
        "free_fluid_balance_m3": s.free_fluid_balance, "global_fluid_balance_m3": s.global_fluid_balance,
        "scaled_free_residual": s.scaled_free_residual, "iterations": s.iterations})
}
