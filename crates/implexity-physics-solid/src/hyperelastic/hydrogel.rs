// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_ad::tape::{Tape, Var};
use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::dense::DenseMatrix;
use serde_json::Value;

use super::adjoint::{Gather, Kernel, RootSpec, element_node, root_node};
use super::kinematics::{
    Mat3, TetMesh, ddot, det, green_maxwell_step, inverse_transpose, matmul, maxwell_potential_piola,
    transpose,
};
use super::poro::{local_u, parse_mesh};
use super::statics::{StaticOptions, StaticProblem, ViscoelasticState};
use crate::util::{bool_array, contract, f64_shaped, has_exact_keys, real_array};

pub const R_GAS: f64 = 8.314_462_618_153_24;
pub const QUAD_A: f64 = 0.585_410_196_624_968_5;
pub const QUAD_B: f64 = 0.138_196_601_125_010_5;
pub const KEYS: [&str; 21] = [
    "points",
    "elements",
    "network_shear_Pa",
    "volume_penalty_Pa",
    "temperature_K",
    "solvent_molar_volume_m3_mol",
    "chi",
    "fixed_dofs",
    "prescribed_displacement_history_m",
    "nodal_force_history_N",
    "steps_s",
    "branch_moduli_Pa",
    "relaxation_times_s",
    "initial_branch_strain",
    "initial_displacement_m",
    "reference_mobility_m2_Pa_s",
    "initial_solvent_volume",
    "initial_chemical_potential_Pa",
    "fixed_chemical_nodes",
    "chemical_potential_history_Pa",
    "solvent_source_history_m3_s",
];
pub const MATERIALS: [&str; 7] = [
    "network_shear_Pa",
    "volume_penalty_Pa",
    "solvent_molar_volume_m3_mol",
    "chi",
    "reference_mobility_m2_Pa_s",
    "branch_moduli_Pa",
    "relaxation_times_s",
];
pub const RESPONSES: [&str; 4] =
    ["final_solvent_volume_m3", "final_free_energy_J", "dissipation_J", "final_displacement_squared_m2"];

fn quadrature<S: Scalar>(s: &[S; 4]) -> [S; 4] {
    std::array::from_fn(|q| {
        let mut acc = S::zero();
        for (a, sa) in s.iter().enumerate() {
            acc += *sa * if a == q { QUAD_A } else { QUAD_B };
        }
        acc
    })
}

pub fn mixing_energy<S: Scalar>(s: S, a: S, chi: S) -> S {
    a * (s * (s.ln() - s.ln_1p()) + chi * s / (s + 1.0))
}

pub fn mixing_chemical_potential<S: Scalar>(s: S, a: S, chi: S) -> S {
    a * (s.ln() - s.ln_1p() + S::one() / (s + 1.0) + chi / (s + 1.0).powi(2))
}

#[derive(Debug, Clone)]
pub struct GelParams<S> {
    pub shear: S,
    pub penalty: S,
    pub mixing: S,
    pub chi: S,
    pub mobility: S,
    pub g: Vec<S>,
    pub tau: Vec<S>,
}

pub fn constitutive<S: Scalar>(f: &Mat3<S>, sq: &[S; 4], m: &GelParams<S>) -> S {
    let j = det(f);
    let network = m.shear * (ddot(f, f) - 3.0 - j.ln() * 2.0) * 0.5;
    let mut mixing = S::zero();
    let mut penalty = S::zero();
    for s in sq {
        mixing += mixing_energy(*s, m.mixing, m.chi);
        penalty += (j - 1.0 - *s).powi(2);
    }
    network + mixing / 4.0 + m.penalty * 0.5 * (penalty / 4.0)
}

pub fn constitutive_piola<S: Scalar>(f: &Mat3<S>, sq: &[S; 4], m: &GelParams<S>) -> Mat3<S> {
    let j = det(f);
    let inv_t = inverse_transpose(f);
    let mut mean = S::zero();
    for s in sq {
        mean += j - 1.0 - *s;
    }
    let mean = mean / 4.0;
    std::array::from_fn(|r| {
        std::array::from_fn(|c| m.shear * (f[r][c] - inv_t[r][c]) + m.penalty * mean * j * inv_t[r][c])
    })
}

#[allow(clippy::too_many_arguments)]
pub fn element_residual<S: Scalar>(
    mesh: &TetMesh,
    e: usize,
    u: &[[S; 3]; 4],
    s: &[S; 4],
    c: &[S; 4],
    sold: &[S; 4],
    m: &GelParams<S>,
    memory: &[Mat3<S>],
    dt: f64,
) -> [S; 20] {
    let f = mesh.deformation_gradient(e, u);
    let j = det(&f);
    let sq = quadrature(s);
    let mut piola = constitutive_piola(&f, &sq, m);
    let pb = maxwell_potential_piola(&f, memory, &m.g, &m.tau, S::from_f64(dt));
    for r in 0..3 {
        for k in 0..3 {
            piola[r][k] += pb[r][k];
        }
    }
    let forces = mesh.nodal_forces(e, &piola);
    let (cmat, kmat) = mesh.hydraulic_local(e, S::one(), m.mobility);
    let v = mesh.volumes[e];
    let mut out = [S::zero(); 20];
    let dq: [S; 4] = std::array::from_fn(|q| {
        (mixing_chemical_potential(sq[q], m.mixing, m.chi) - m.penalty * (j - 1.0 - sq[q])) / 4.0
    });
    for a in 0..4 {
        for i in 0..3 {
            out[3 * a + i] = forces[a][i];
        }
        let mut ds = S::zero();
        for (q, d) in dq.iter().enumerate() {
            ds += *d * if q == a { QUAD_A } else { QUAD_B };
        }
        let mut acc_s = ds * v;
        let mut acc_c = S::zero();
        for b in 0..4 {
            acc_s -= cmat[b][a] * c[b];
            acc_c = acc_c - cmat[a][b] * (s[b] - sold[b]) - kmat[a][b] * c[b] * dt;
        }
        out[12 + a] = acc_s;
        out[16 + a] = acc_c;
    }
    out
}

#[derive(Debug, Clone)]
pub struct GelProblem {
    pub mesh: TetMesh,
    pub shear: Vec<f64>,
    pub penalty: Vec<f64>,
    pub temperature: Vec<f64>,
    pub molar_volume: Vec<f64>,
    pub chi: Vec<f64>,
    pub mobility: Vec<f64>,
    pub fixed_dofs: Vec<bool>,
    pub prescribed: Vec<Vec<f64>>,
    pub force: Vec<Vec<f64>>,
    pub steps: Vec<f64>,
    pub moduli: Vec<Vec<f64>>,
    pub times: Vec<Vec<f64>>,
    pub memory: Vec<Vec<Mat3<f64>>>,
    pub u0: Vec<f64>,
    pub s0: Vec<f64>,
    pub c0: Vec<f64>,
    pub fixed_chemical: Vec<bool>,
    pub chemical_history: Vec<Vec<f64>>,
    pub source: Vec<Vec<f64>>,
}

fn rows(v: &[f64], width: usize) -> Vec<Vec<f64>> {
    v.chunks(width.max(1)).map(<[f64]>::to_vec).collect()
}

impl GelProblem {

    #[allow(clippy::too_many_lines)]
    pub fn parse(p: &Value) -> Result<Self, CaeError> {
        if !has_exact_keys(p, &KEYS) {
            return contract("Complete explicit neutral-gel problem required");
        }
        let mesh = parse_mesh(p)?;
        let (n, ne) = (mesh.node_count(), mesh.elements.len());
        let steps = match real_array(&p["steps_s"]) {
            Some((s, v))
                if s.len() == 1
                    && (1..=1000).contains(&v.len())
                    && v.iter().all(|x| x.is_finite() && *x > 0.0)
                    && v.iter().sum::<f64>().is_finite() =>
            {
                v
            }
            _ => return contract("Finite positive time increments required"),
        };
        let nt = steps.len();
        let uh = f64_shaped(&p["prescribed_displacement_history_m"], &[nt, n, 3]);
        let fh = f64_shaped(&p["nodal_force_history_N"], &[nt, n, 3]);
        let ch = f64_shaped(&p["chemical_potential_history_Pa"], &[nt, n]);
        let source = f64_shaped(&p["solvent_source_history_m3_s"], &[nt, n]);
        let c = f64_shaped(&p["initial_chemical_potential_Pa"], &[n]);
        let s = f64_shaped(&p["initial_solvent_volume"], &[n]);
        let u = f64_shaped(&p["initial_displacement_m"], &[n, 3]);
        let (Some(uh), Some(fh), Some(ch), Some(source), Some(c), Some(s), Some(u)) =
            (uh, fh, ch, source, c, s, u)
        else {
            return contract("Inconsistent gel histories");
        };
        let Some((_, fixedc)) = bool_array(&p["fixed_chemical_nodes"]).filter(|(sh, _)| sh == &[n]) else {
            return contract("Boolean chemical-reservoir node mask required");
        };
        if uh
            .iter()
            .chain(&fh)
            .chain(&ch)
            .chain(&source)
            .chain(&c)
            .chain(&s)
            .chain(&u)
            .any(|v| !v.is_finite())
            || s.iter().any(|v| *v <= 0.0)
        {
            return contract("Finite histories and strictly positive initial solvent required");
        }
        let material = |k: &str| f64_shaped(&p[k], &[ne]).filter(|v| v.iter().all(Scalar::is_finite));
        let keys = [
            "network_shear_Pa",
            "volume_penalty_Pa",
            "temperature_K",
            "solvent_molar_volume_m3_mol",
            "chi",
            "reference_mobility_m2_Pa_s",
        ];
        let values: Vec<Option<Vec<f64>>> = keys.iter().map(|k| material(k)).collect();
        if values.iter().any(Option::is_none) {
            return contract("One finite material value per tetrahedron required");
        }
        let values: Vec<Vec<f64>> = values.into_iter().flatten().collect();
        let positive = values[..4].iter().any(|v| v.iter().any(|x| *x <= 0.0));
        if positive
            || values[5].iter().any(|x| *x < 0.0)
            || values[4].iter().any(|x| !(0.0..=0.5).contains(x))
        {
            return contract(
                "Positive moduli, absolute temperature and molar volume; mobility>=0 and 0<=chi<=0.5 required",
            );
        }
        #[allow(clippy::float_cmp)]                     
        if values[2].iter().any(|t| *t != values[2][0]) {
            return contract("Current gel transport is isothermal: one uniform temperature required");
        }
        let fixed_dofs =
            bool_array(&p["fixed_dofs"]).filter(|(sh, _)| sh == &[n, 3]).map(|x| x.1).unwrap_or_default();
        let (ms, moduli) = real_array(&p["branch_moduli_Pa"]).unwrap_or_default();
        let (_, times) = real_array(&p["relaxation_times_s"]).unwrap_or_default();
        let (mems, memory) = real_array(&p["initial_branch_strain"]).unwrap_or_default();
        let nb = if ms.len() == 2 { ms[1] } else { 0 };
        let memory_ok = mems.len() == 4 && mems[0] == ne && mems[1] == nb && mems[2] == 3 && mems[3] == 3;
        let mats: Vec<Mat3<f64>> = memory
            .chunks(9)
            .map(|c| std::array::from_fn(|i| std::array::from_fn(|j| c[3 * i + j])))
            .collect();
        let problem = Self {
            shear: values[0].clone(),
            penalty: values[1].clone(),
            temperature: values[2].clone(),
            molar_volume: values[3].clone(),
            chi: values[4].clone(),
            mobility: values[5].clone(),
            fixed_dofs,
            prescribed: rows(&uh, 3 * n),
            force: rows(&fh, 3 * n),
            steps,
            moduli: rows(&moduli, nb),
            times: rows(&times, nb),
            memory: if memory_ok {
                mats.chunks(nb.max(1)).map(<[Mat3<f64>]>::to_vec).collect()
            } else {
                Vec::new()
            },
            u0: u,
            s0: s,
            c0: c,
            fixed_chemical: fixedc,
            chemical_history: rows(&ch, n),
            source: rows(&source, n),
            mesh,
        };
        let vs = ViscoelasticState {
            memory: problem.memory.clone(),
            moduli: problem.moduli.clone(),
            times: problem.times.clone(),
            step: problem.steps[0],
        };
        let zeros = vec![0.0; ne];
        StaticProblem {
            mesh: &problem.mesh,
            mu: &problem.shear,
            lam: &zeros,
            fixed: &problem.fixed_dofs,
            prescribed: &problem.prescribed[0],
            force: &problem.force[0],
            initial: Some(&problem.u0),
            viscoelastic: Some(&vs),
            options: StaticOptions::default(),
        }
        .validate()?;
        let j = problem.jacobians(&problem.u0);
        if j.iter().any(|v| !v.is_finite()) || j.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0 {
            return contract("Initial gel deformation is inverted");
        }
        Ok(problem)
    }

    fn jacobians(&self, u: &[f64]) -> Vec<f64> {
        (0..self.mesh.elements.len())
            .map(|e| det(&self.mesh.deformation_gradient(e, &local_u(u, &self.mesh.elements[e]))))
            .collect()
    }

    #[must_use]
    pub fn params(&self, e: usize) -> GelParams<f64> {
        GelParams {
            shear: self.shear[e],
            penalty: self.penalty[e],
            mixing: R_GAS * self.temperature[e] / self.molar_volume[e],
            chi: self.chi[e],
            mobility: self.mobility[e],
            g: self.moduli[e].clone(),
            tau: self.times[e].clone(),
        }
    }

    fn scales(&self) -> (Vec<f64>, f64, f64) {
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
        let a_max = (0..self.mesh.elements.len())
            .map(|e| R_GAS * self.temperature[e] / self.molar_volume[e])
            .fold(f64::NEG_INFINITY, f64::max);
        let pref = a_max
            .max(self.penalty.iter().copied().fold(f64::NEG_INFINITY, f64::max))
            .max(self.shear.iter().copied().fold(f64::NEG_INFINITY, f64::max));
        let scale: Vec<f64> = (0..5 * n)
            .map(|i| {
                if i < 3 * n {
                    length
                } else if i < 4 * n {
                    1.0
                } else {
                    pref
                }
            })
            .collect();
        (scale, length, self.mesh.volumes.iter().sum::<f64>() * pref)
    }

    fn element_dofs(&self, t: &[usize; 4]) -> Vec<usize> {
        let n = self.mesh.node_count();
        t.iter()
            .flat_map(|node| (0..3).map(move |i| 3 * node + i))
            .chain(t.iter().map(|node| 3 * n + node))
            .chain(t.iter().map(|node| 4 * n + node))
            .collect()
    }

    fn residual(
        &self,
        state: &[f64],
        memory: &[Vec<Mat3<f64>>],
        sold: &[f64],
        dt: f64,
        force: &[f64],
        source: &[f64],
    ) -> Vec<f64> {
        let n = self.mesh.node_count();
        let mut r = vec![0.0; 5 * n];
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let u = local_u(state, t);
            let s: [f64; 4] = std::array::from_fn(|a| state[3 * n + t[a]]);
            let c: [f64; 4] = std::array::from_fn(|a| state[4 * n + t[a]]);
            let so: [f64; 4] = std::array::from_fn(|a| sold[t[a]]);
            let out = element_residual(&self.mesh, e, &u, &s, &c, &so, &self.params(e), &memory[e], dt);
            for (k, d) in self.element_dofs(t).iter().enumerate() {
                r[*d] += out[k];
            }
        }
        for i in 0..3 * n {
            r[i] -= force[i];
        }
        for a in 0..n {
            r[4 * n + a] += dt * source[a];
        }
        r
    }

    fn tangent(
        &self,
        state: &[f64],
        memory: &[Vec<Mat3<f64>>],
        sold: &[f64],
        dt: f64,
    ) -> Result<DenseMatrix, CaeError> {
        let n = self.mesh.node_count();
        let size = 5 * n;
        let mut h = DenseMatrix::zeros(size, size);
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let dofs = self.element_dofs(t);
            let local: Vec<f64> = dofs.iter().map(|d| state[*d]).collect();
            let params = self.params(e);
            let so: [f64; 4] = std::array::from_fn(|a| sold[t[a]]);
            let jac = implexity_ad::forward::jacobian::<20, _>(
                |x: &[Dual<20>]| {
                    let u: [[Dual<20>; 3]; 4] =
                        std::array::from_fn(|a| std::array::from_fn(|i| x[3 * a + i]));
                    let s: [Dual<20>; 4] = std::array::from_fn(|a| x[12 + a]);
                    let c: [Dual<20>; 4] = std::array::from_fn(|a| x[16 + a]);
                    let sold: [Dual<20>; 4] = so.map(Dual::constant);
                    let m = lift(&params);
                    let mem: Vec<Mat3<Dual<20>>> =
                        memory[e].iter().map(|x| x.map(|r| r.map(Dual::constant))).collect();
                    element_residual(&self.mesh, e, &u, &s, &c, &sold, &m, &mem, dt).to_vec()
                },
                &local,
            )
            .map_err(|err| CaeError::contract(err.to_string()))?;
            for (a, ra) in dofs.iter().enumerate() {
                for (b, cb) in dofs.iter().enumerate() {
                    h.data[ra * size + cb] += jac.matrix[a * 20 + b];
                }
            }
        }
        Ok(h)
    }

    #[must_use]
    pub fn solvent_volume(&self, s: &[f64]) -> f64 {
        let mut total = 0.0;
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let (cmat, _) = self.mesh.hydraulic_local(e, 1.0, self.mobility[e]);
            for a in 0..4 {
                for b in 0..4 {
                    total += cmat[a][b] * s[t[b]];
                }
            }
        }
        total
    }

    fn product(&self, v: &[f64], storage: bool) -> Vec<f64> {
        let mut out = vec![0.0; v.len()];
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let (cm, km) = self.mesh.hydraulic_local(e, 1.0, self.mobility[e]);
            let m = if storage { cm } else { km };
            for a in 0..4 {
                for b in 0..4 {
                    out[t[a]] += m[a][b] * v[t[b]];
                }
            }
        }
        out
    }
}

fn lift<S: Scalar>(m: &GelParams<f64>) -> GelParams<S> {
    GelParams {
        shear: S::from_f64(m.shear),
        penalty: S::from_f64(m.penalty),
        mixing: S::from_f64(m.mixing),
        chi: S::from_f64(m.chi),
        mobility: S::from_f64(m.mobility),
        g: m.g.iter().map(|v| S::from_f64(*v)).collect(),
        tau: m.tau.iter().map(|v| S::from_f64(*v)).collect(),
    }
}

#[derive(Debug, Clone)]
pub struct GelStep {
    pub displacement: Vec<f64>,
    pub solvent: Vec<f64>,
    pub chemical: Vec<f64>,
    pub cauchy_stress: Vec<Mat3<f64>>,
    pub jacobian: Vec<f64>,
    pub branch_strain: Vec<Vec<Mat3<f64>>>,
    pub support_reactions: Vec<f64>,
    pub solvent_volume: f64,
    pub reservoir_exchange: Vec<f64>,
    pub free_solvent_balance: f64,
    pub global_solvent_balance: f64,
    pub volume_constraint_error: f64,
    pub flux: Vec<[f64; 3]>,
    pub diffusion_dissipation: f64,
    pub viscoelastic_dissipation: f64,
    pub free_energy: f64,
    pub scaled_free_residual: f64,
    pub iterations: usize,
}

#[derive(Debug, Clone)]
pub struct GelHistory {
    pub steps: Vec<GelStep>,
    pub times: Vec<f64>,
    pub initial_volume: f64,
}

impl GelProblem {
    #[must_use]
    pub fn fixed_state(&self) -> Vec<bool> {
        let n = self.mesh.node_count();
        self.fixed_dofs
            .iter()
            .copied()
            .chain(std::iter::repeat_n(false, n))
            .chain(self.fixed_chemical.iter().copied())
            .collect()
    }


    #[allow(clippy::too_many_lines)]
    pub fn solve_history(&self) -> Result<GelHistory, CaeError> {
        let n = self.mesh.node_count();
        let ne = self.mesh.elements.len();
        let (scale, length, energy_scale) = self.scales();
        let fixed = self.fixed_state();
        let free: Vec<usize> = (0..5 * n).filter(|i| !fixed[*i]).collect();
        let (mut u, mut s, mut c) = (self.u0.clone(), self.s0.clone(), self.c0.clone());
        let mut memory = self.memory.clone();
        let mut old_volume = self.solvent_volume(&s);
        let initial_volume = old_volume;
        let mut records = Vec::with_capacity(self.steps.len());
        for (i, dt) in self.steps.iter().copied().enumerate() {
            let mut state: Vec<f64> = u.iter().chain(&s).chain(&c).copied().collect();
            let target: Vec<f64> =
                self.prescribed[i].iter().chain(&s).chain(&self.chemical_history[i]).copied().collect();
            for k in 0..5 * n {
                if fixed[k] {
                    state[k] = target[k];
                }
            }
            let sold = s.clone();
            let to_state = |y: &[f64]| -> Vec<f64> { y.iter().zip(&scale).map(|(a, b)| a * b).collect() };
            let residual_y = |y: &[f64]| -> Vec<f64> {
                self.residual(&to_state(y), &memory, &sold, dt, &self.force[i], &self.source[i])
                    .iter()
                    .zip(&scale)
                    .map(|(r, c)| r * c / energy_scale)
                    .collect()
            };
            let tangent_y = |y: &[f64]| -> Result<DenseMatrix, CaeError> {
                let h = self.tangent(&to_state(y), &memory, &sold, dt)?;
                let size = 5 * n;
                let mut out = h;
                for a in 0..size {
                    for b in 0..size {
                        out.data[a * size + b] *= scale[a] * scale[b] / energy_scale;
                    }
                }
                Ok(out)
            };
            let admissible = |y: &[f64]| -> bool {
                let st = to_state(y);
                let j = self.jacobians(&st[..3 * n]);
                st.iter().all(Scalar::is_finite)
                    && st[3 * n..4 * n].iter().all(|v| *v > 0.0)
                    && j.iter().all(Scalar::is_finite)
                    && j.iter().copied().fold(f64::INFINITY, f64::min) > 0.0
            };
            let equilibrate = |mut y: Vec<f64>| -> Result<Vec<f64>, CaeError> {
                if !admissible(&y) {
                    return contract("Gel condensation requires positive volume and solvent concentration");
                }
                for _ in 0..20 {
                    let r: Vec<f64> = residual_y(&y)[3 * n..4 * n].to_vec();
                    if r.iter().any(|v| !v.is_finite()) {
                        return contract("Nonfinite gel concentration residual");
                    }
                    if r.iter().map(|v| v.abs()).fold(0.0, f64::max) <= 1e-13 {
                        return Ok(y);
                    }
                    let full = tangent_y(&y)?;
                    let size = 5 * n;
                    let h = DenseMatrix {
                        nrows: n,
                        ncols: n,
                        data: (0..n * n).map(|k| full.data[(3 * n + k / n) * size + 3 * n + k % n]).collect(),
                    };
                    if h.data.iter().any(|v| !v.is_finite()) {
                        return contract("Nonfinite gel concentration tangent");
                    }
                    let rhs: Vec<f64> = r.iter().map(|v| -v).collect();
                    let direction = implexity_linalg::dense::solve(&h, &rhs, 1)
                        .map_err(|_| CaeError::contract("Singular gel concentration tangent"))?;
                    let rn = r.iter().map(|v| v * v).sum::<f64>().sqrt();
                    let mut accepted = false;
                    for k in 0..24 {
                        let mut trial = y.clone();
                        for (a, d) in direction.iter().enumerate() {
                            trial[3 * n + a] += 2.0_f64.powi(-k) * d;
                        }
                        if trial[3 * n..4 * n].iter().copied().fold(f64::INFINITY, f64::min) > 0.0 {
                            let rt = &residual_y(&trial)[3 * n..4 * n];
                            if rt.iter().all(Scalar::is_finite)
                                && rt.iter().map(|v| v * v).sum::<f64>().sqrt() < rn
                            {
                                y = trial;
                                accepted = true;
                                break;
                            }
                        }
                    }
                    if !accepted {
                        return contract("Gel concentration condensation line search failed");
                    }
                }
                contract("Gel concentration condensation iteration limit")
            };
            let mut y: Vec<f64> = state.iter().zip(&scale).map(|(a, b)| a / b).collect();
            y = equilibrate(y)?;
            let mut initial_norm: Option<f64> = None;
            let (mut norm, mut iteration, mut r);
            iteration = 0;
            loop {
                if !admissible(&y) {
                    return contract("Gel state leaves positive volume/concentration domain");
                }
                r = residual_y(&y);
                norm = free.iter().map(|k| r[*k].abs()).fold(0.0, f64::max);
                if r.iter().any(|v| !v.is_finite()) {
                    return contract("Nonfinite gel residual");
                }
                let initial = *initial_norm.get_or_insert(norm);
                if norm <= 1e-12 + 1e-9 * initial {
                    break;
                }
                if iteration == 60 {
                    let st = to_state(&y);
                    return contract(format!(
                        "Gel Newton iteration limit: residual={}, initial={}, J={}",
                        implexity_core::py_repr::repr_float(norm),
                        implexity_core::py_repr::repr_float(initial),
                        implexity_core::pyobj::repr(&serde_json::json!(self.jacobians(&st[..3 * n])))
                    ));
                }
                let full = tangent_y(&y)?;
                let size = 5 * n;
                let k = DenseMatrix {
                    nrows: free.len(),
                    ncols: free.len(),
                    data: free
                        .iter()
                        .flat_map(|a| free.iter().map(|b| full.data[a * size + b]).collect::<Vec<_>>())
                        .collect(),
                };
                let rhs: Vec<f64> = free.iter().map(|a| -r[*a]).collect();
                let direction = implexity_linalg::dense::solve(&k, &rhs, 1)
                    .map_err(|_| CaeError::contract("Singular gel tangent"))?;
                let rfree = free.iter().map(|a| r[*a] * r[*a]).sum::<f64>().sqrt();
                let mut accepted = false;
                for backtrack in 0..28 {
                    let step = 2.0_f64.powi(-backtrack);
                    let mut trial = y.clone();
                    for (a, d) in free.iter().zip(&direction) {
                        trial[*a] += step * d;
                    }
                    if admissible(&trial) {
                        let trial = equilibrate(trial)?;
                        let rt = residual_y(&trial);
                        let rt_free: Vec<f64> = free.iter().map(|a| rt[*a]).collect();
                        if rt_free.iter().all(Scalar::is_finite)
                            && rt_free.iter().map(|v| v * v).sum::<f64>().sqrt() < (1.0 - 1e-4 * step) * rfree
                        {
                            y = trial;
                            accepted = true;
                            break;
                        }
                    }
                }
                if !accepted {
                    return contract("Gel residual line search failed");
                }
                iteration += 1;
            }
            let st = to_state(&y);
            u = st[..3 * n].to_vec();
            let new_s = st[3 * n..4 * n].to_vec();
            c = st[4 * n..].to_vec();
            let (mut free_energy, mut visco) = (0.0, 0.0);
            let mut new_memory = Vec::with_capacity(ne);
            let mut stress = Vec::with_capacity(ne);
            let mut jac = Vec::with_capacity(ne);
            let mut constraint: f64 = 0.0;
            let mut flux = Vec::with_capacity(ne);
            for (e, t) in self.mesh.elements.iter().enumerate() {
                let params = self.params(e);
                let f = self.mesh.deformation_gradient(e, &local_u(&u, t));
                let sq = quadrature(&std::array::from_fn(|a| new_s[t[a]]));
                let elastic = constitutive(&f, &sq, &params);
                let update = green_maxwell_step(&f, &memory[e], &params.g, &params.tau, dt);
                free_energy += self.mesh.volumes[e] * (elastic + update.stored);
                visco += self.mesh.volumes[e] * update.dissipated;
                let j = det(&f);
                let mut first = constitutive_piola(&f, &sq, &params);
                for r in 0..3 {
                    for k in 0..3 {
                        first[r][k] += update.first_piola[r][k];
                    }
                }
                let cauchy = matmul(&first, &transpose(&f));
                stress.push(cauchy.map(|r| r.map(|v| v / j)));
                jac.push(j);
                constraint = constraint.max(sq.iter().map(|s| (j - 1.0 - s).abs()).fold(0.0, f64::max));
                let g = &self.mesh.gradients[e];
                let grad_c: [f64; 3] = std::array::from_fn(|k| (0..4).map(|a| c[t[a]] * g[a][k]).sum());
                flux.push(grad_c.map(|v| -params.mobility * v));
                new_memory.push(update.branch_strain);
            }
            memory = new_memory;
            let cs = self.product(&new_s, true);
            let cso = self.product(&s, true);
            let kc = self.product(&c, false);
            let balance: Vec<f64> =
                (0..n).map(|a| cs[a] - cso[a] + dt * (kc[a] - self.source[i][a])).collect();
            let exchange: Vec<f64> =
                (0..n).map(|a| if self.fixed_chemical[a] { balance[a] } else { 0.0 }).collect();
            let volume = self.solvent_volume(&new_s);
            let force_residual: Vec<f64> = r[..3 * n].iter().map(|v| v * energy_scale / length).collect();
            records.push(GelStep {
                displacement: u.clone(),
                solvent: new_s.clone(),
                chemical: c.clone(),
                cauchy_stress: stress,
                jacobian: jac,
                branch_strain: memory.clone(),
                support_reactions: (0..3 * n)
                    .map(|k| if self.fixed_dofs[k] { force_residual[k] } else { 0.0 })
                    .collect(),
                solvent_volume: volume,
                reservoir_exchange: exchange.clone(),
                free_solvent_balance: (0..n)
                    .filter(|a| !self.fixed_chemical[*a])
                    .map(|a| balance[a].abs())
                    .fold(0.0, f64::max),
                global_solvent_balance: volume
                    - old_volume
                    - dt * self.source[i].iter().sum::<f64>()
                    - exchange.iter().sum::<f64>(),
                volume_constraint_error: constraint,
                flux,
                diffusion_dissipation: dt * c.iter().zip(&kc).map(|(a, b)| a * b).sum::<f64>(),
                viscoelastic_dissipation: visco,
                free_energy,
                scaled_free_residual: norm,
                iterations: iteration,
            });
            s = new_s;
            old_volume = volume;
        }
        let mut times = Vec::with_capacity(self.steps.len());
        let mut acc = 0.0;
        for dt in &self.steps {
            acc += dt;
            times.push(acc);
        }
        Ok(GelHistory { steps: records, times, initial_volume })
    }
}

fn param_width(nb: usize) -> usize {
    5 + 2 * nb
}

fn params_from<S: Scalar>(local: &[S], nb: usize, temperature: f64) -> GelParams<S> {
    GelParams {
        shear: local[0],
        penalty: local[1],
        mixing: S::one() / local[2] * (R_GAS * temperature),
        chi: local[3],
        mobility: local[4],
        g: local[5..5 + nb].to_vec(),
        tau: local[5 + nb..5 + 2 * nb].to_vec(),
    }
}

fn mem_from<S: Scalar>(local: &[S], nb: usize) -> Vec<Mat3<S>> {
    (0..nb).map(|b| std::array::from_fn(|r| std::array::from_fn(|c| local[9 * b + 3 * r + c]))).collect()
}

#[derive(Clone)]
struct GelKernels {
    mesh: Arc<TetMesh>,
    nb: usize,
    dt: f64,
    temperature: Arc<Vec<f64>>,
}

struct RootKernel(GelKernels);

impl Kernel for RootKernel {
    fn n_out(&self) -> usize {
        20
    }
    fn eval<S: Scalar>(&self, e: usize, x: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| x[3 * a + i]));
        let s: [S; 4] = std::array::from_fn(|a| x[12 + a]);
        let c: [S; 4] = std::array::from_fn(|a| x[16 + a]);
        let so: [S; 4] = std::array::from_fn(|a| x[20 + a]);
        let mem = mem_from(&x[24..24 + 9 * k.nb], k.nb);
        let params = params_from(&x[24 + 9 * k.nb..], k.nb, k.temperature[e]);
        out.copy_from_slice(&element_residual(&k.mesh, e, &u, &s, &c, &so, &params, &mem, k.dt));
    }
}

struct MemoryKernel(GelKernels);

impl Kernel for MemoryKernel {
    fn n_out(&self) -> usize {
        9 * self.0.nb
    }
    fn eval<S: Scalar>(&self, e: usize, x: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| x[3 * a + i]));
        let mem = mem_from(&x[12..12 + 9 * k.nb], k.nb);
        let params = params_from(&x[12 + 9 * k.nb..], k.nb, k.temperature[e]);
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

struct EnergyKernel(GelKernels);

impl Kernel for EnergyKernel {
    fn n_out(&self) -> usize {
        2
    }
    fn eval<S: Scalar>(&self, e: usize, x: &[S], out: &mut [S]) {
        let k = &self.0;
        let u: [[S; 3]; 4] = std::array::from_fn(|a| std::array::from_fn(|i| x[3 * a + i]));
        let s: [S; 4] = std::array::from_fn(|a| x[12 + a]);
        let c: [S; 4] = std::array::from_fn(|a| x[16 + a]);
        let mem = mem_from(&x[20..20 + 9 * k.nb], k.nb);
        let params = params_from(&x[20 + 9 * k.nb..], k.nb, k.temperature[e]);
        let f = k.mesh.deformation_gradient(e, &u);
        let update = green_maxwell_step(&f, &mem, &params.g, &params.tau, S::from_f64(k.dt));
        let (_, kmat) = k.mesh.hydraulic_local(e, S::one(), params.mobility);
        let mut ckc = S::zero();
        for a in 0..4 {
            for b in 0..4 {
                ckc += c[a] * kmat[a][b] * c[b];
            }
        }
        let v = k.mesh.volumes[e];
        out[0] = ckc * k.dt + update.dissipated * v;
        out[1] = (constitutive(&f, &quadrature(&s), &params) + update.stored) * v;
    }
}


#[allow(clippy::too_many_lines)]
pub fn material_sensitivity(
    problem: &GelProblem,
    material: &str,
    response: &str,
) -> Result<(f64, Vec<f64>), CaeError> {
    if !MATERIALS.contains(&material) || !RESPONSES.contains(&response) {
        return contract("Unsupported gel material derivative");
    }
    let admitted = problem.solve_history()?;
    let p = problem;
    let (n, ne) = (p.mesh.node_count(), p.mesh.elements.len());
    let nb = p.moduli.first().map_or(0, Vec::len);
    let pw = param_width(nb);
    let mut tape = Tape::new();
    let map_err = |e: implexity_ad::AdError| CaeError::contract(e.to_string());
    let (theta_value, offset): (Vec<f64>, (usize, usize)) = match material {
        "network_shear_Pa" => (p.shear.clone(), (0, 1)),
        "volume_penalty_Pa" => (p.penalty.clone(), (1, 1)),
        "solvent_molar_volume_m3_mol" => (p.molar_volume.clone(), (2, 1)),
        "chi" => (p.chi.clone(), (3, 1)),
        "reference_mobility_m2_Pa_s" => (p.mobility.clone(), (4, 1)),
        "branch_moduli_Pa" => (p.moduli.iter().flatten().copied().collect(), (5, nb)),
        _ => (p.times.iter().flatten().copied().collect(), (5 + nb, nb)),
    };
    let theta = tape.input(theta_value);
    let mut base = Vec::with_capacity(ne * pw);
    for e in 0..ne {
        base.extend([p.shear[e], p.penalty[e], p.molar_volume[e], p.chi[e], p.mobility[e]]);
        base.extend(&p.moduli[e]);
        base.extend(&p.times[e]);
    }
    let base = tape.constant(base);
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
            out[self.start..self.start + self.len].copy_from_slice(&local[self.pw..self.pw + self.len]);
        }
    }
    let range = |w: usize| -> Vec<Vec<usize>> { (0..ne).map(|e| (e * w..(e + 1) * w).collect()).collect() };
    let params = element_node(
        &mut tape,
        Arc::new(Select { pw, start: offset.0, len: offset.1 }),
        &[base, theta],
        Gather::from_blocks(&[
            range(pw),
            (0..ne).map(|e| (e * offset.1..(e + 1) * offset.1).collect()).collect(),
        ]),
    )?;
    let mut memory =
        tape.constant(p.memory.iter().flatten().flat_map(|m| m.iter().flatten().copied()).collect());
    let mut sold = tape.constant(p.s0.clone());
    let fixed = p.fixed_state();
    let free: Vec<usize> = (0..5 * n).filter(|i| !fixed[*i]).collect();
    let mesh = Arc::new(p.mesh.clone());
    let temperature = Arc::new(p.temperature.clone());
    let dofs: Vec<Vec<usize>> = p.mesh.elements.iter().map(|t| p.element_dofs(t)).collect();
    let mut dissipation: Option<Var> = None;
    let mut last_state = None;
    let mut last_energy = None;
    for (i, record) in admitted.steps.iter().enumerate() {
        let k =
            GelKernels { mesh: Arc::clone(&mesh), nb, dt: p.steps[i], temperature: Arc::clone(&temperature) };
        let y: Vec<f64> =
            record.displacement.iter().chain(&record.solvent).chain(&record.chemical).copied().collect();
        let mut gather = Vec::with_capacity(ne);
        let mut rows = Vec::with_capacity(ne);
        for (e, t) in p.mesh.elements.iter().enumerate() {
            let mut local: Vec<(usize, usize)> = dofs[e].iter().map(|d| (0, *d)).collect();
            local.extend(t.iter().map(|node| (2, *node)));
            local.extend((0..9 * nb).map(|j| (1, e * 9 * nb + j)));
            local.extend((0..pw).map(|j| (3, e * pw + j)));
            gather.push(local);
            rows.push(dofs[e].iter().map(|d| Some(*d)).collect());
        }
        let spec = RootSpec {
            kernel: Arc::new(RootKernel(k.clone())),
            gather: Gather { map: gather },
            rows,
            free: free.clone(),
            size: 5 * n,
        };
        let state = root_node(&mut tape, spec, y, &[memory, sold, params])?;
        let energy_blocks = {
            let state_blocks: Vec<Vec<usize>> = dofs.clone();
            vec![state_blocks, range(9 * nb), range(pw)]
        };
        let energies = element_node(
            &mut tape,
            Arc::new(EnergyKernel(k.clone())),
            &[state, memory, params],
            Gather::from_blocks(&energy_blocks),
        )?;
        let diss_idx: Vec<usize> = (0..ne).map(|e| 2 * e).collect();
        let d = tape.gather(energies, diss_idx).map_err(map_err)?;
        let d = tape.sum(d).map_err(map_err)?;
        dissipation = Some(match dissipation {
            None => d,
            Some(prev) => tape.add(prev, d).map_err(map_err)?,
        });
        let u_blocks: Vec<Vec<usize>> = dofs.iter().map(|d| d[..12].to_vec()).collect();
        memory = element_node(
            &mut tape,
            Arc::new(MemoryKernel(k.clone())),
            &[state, memory, params],
            Gather::from_blocks(&[u_blocks, range(9 * nb), range(pw)]),
        )?;
        sold = tape.slice(state, 3 * n, n).map_err(map_err)?;
        last_state = Some(state);
        last_energy = Some(energies);
    }
    let (Some(state), Some(energies), Some(dissipation)) = (last_state, last_energy, dissipation) else {
        return contract("empty gel history");
    };
    let output = match response {
        "dissipation_J" => dissipation,
        "final_free_energy_J" => {
            let idx: Vec<usize> = (0..ne).map(|e| 2 * e + 1).collect();
            let g = tape.gather(energies, idx).map_err(map_err)?;
            tape.sum(g).map_err(map_err)?
        }
        "final_displacement_squared_m2" => {
            let u = tape.slice(state, 0, 3 * n).map_err(map_err)?;
            tape.dot(u, u).map_err(map_err)?
        }
        _ => {

            let weights: Vec<f64> = {
                let mut w = vec![0.0; n];
                for (e, t) in p.mesh.elements.iter().enumerate() {
                    let (cm, _) = p.mesh.hydraulic_local(e, 1.0, 1.0);
                    for a in 0..4 {
                        for b in 0..4 {
                            w[t[b]] += cm[a][b];
                        }
                    }
                }
                w
            };
            let s = tape.slice(state, 3 * n, n).map_err(map_err)?;
            let w = tape.constant(weights);
            tape.dot(w, s).map_err(map_err)?
        }
    };
    let value = tape.value(output).map_err(map_err)?[0];
    let grads = tape.vjp(output, &[1.0]).map_err(map_err)?;
    let gradient = grads.wrt(theta).map_err(map_err)?;
    if !value.is_finite() || gradient.iter().any(|g| !g.is_finite()) {
        return contract("Nonfinite gel derivative; tangent may be singular");
    }
    Ok((value, gradient))
}
