// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::dense::DenseMatrix;

use super::kinematics::{
    GreenMaxwell, Mat3, TetMesh, det, green_maxwell_step, matmul, maxwell_potential_piola,
    neo_hookean_cauchy, neo_hookean_energy, neo_hookean_piola, transpose,
};
use crate::util::contract;

#[derive(Debug, Clone, PartialEq)]
pub struct ViscoelasticState {
    pub memory: Vec<Vec<Mat3<f64>>>,
    pub moduli: Vec<Vec<f64>>,
    pub times: Vec<Vec<f64>>,
    pub step: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct StaticOptions {
    pub max_iterations: usize,
    pub relative_tolerance: f64,
    pub absolute_tolerance: f64,
}

impl Default for StaticOptions {
    fn default() -> Self {
        Self { max_iterations: 40, relative_tolerance: 1e-9, absolute_tolerance: 1e-10 }
    }
}

#[derive(Debug, Clone)]
pub struct StaticResult {
    pub displacement: Vec<[f64; 3]>,
    pub cauchy_stress: Vec<Mat3<f64>>,
    pub jacobian: Vec<f64>,
    pub stored_energy: f64,
    pub support_reactions: Vec<[f64; 3]>,
    pub free_residual: f64,
    pub iterations: Vec<Value>,
    pub branch_strain: Option<Vec<Vec<Mat3<f64>>>>,
    pub dissipation_increment: Option<f64>,
    pub incremental_potential: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct Validated {
    pub minimum_initial_j: f64,
}

pub struct StaticProblem<'a> {
    pub mesh: &'a TetMesh,
    pub mu: &'a [f64],
    pub lam: &'a [f64],
    pub fixed: &'a [bool],
    pub prescribed: &'a [f64],
    pub force: &'a [f64],
    pub initial: Option<&'a [f64]>,
    pub viscoelastic: Option<&'a ViscoelasticState>,
    pub options: StaticOptions,
}

fn local_u<S: Scalar>(state: &[S], t: &[usize; 4]) -> [[S; 3]; 4] {
    std::array::from_fn(|a| std::array::from_fn(|i| state[3 * t[a] + i]))
}

impl StaticProblem<'_> {

    pub fn validate(&self) -> Result<Validated, CaeError> {
        let n = self.mesh.node_count();
        let ne = self.mesh.elements.len();
        if !(4..=256).contains(&n) {
            return contract("dense hyperelastic path requires 4..256 XYZ nodes");
        }
        if self.mu.len() != ne
            || self.lam.len() != ne
            || self.mu.iter().chain(self.lam).any(|v| !v.is_finite())
            || self.mu.iter().any(|v| *v <= 0.0)
            || self.lam.iter().any(|v| *v < 0.0)
        {
            return contract("one positive shear modulus and nonnegative Lame lambda per element required");
        }
        let young: Vec<f64> =
            self.mu.iter().zip(self.lam).map(|(m, l)| m * (3.0 * l + 2.0 * m) / (l + m)).collect();
        let nu: Vec<f64> = self.mu.iter().zip(self.lam).map(|(m, l)| l / (2.0 * (l + m))).collect();
        crate::structural_dynamics::assemble_linear_tetrahedra(
            &self.mesh.points,
            &self.mesh.elements,
            &vec![1.0; ne],
            &young,
            &nu,
        )?;
        if self.fixed.len() != 3 * n || !self.fixed.iter().any(|f| *f) {
            return contract("explicit fixed node-by-XYZ mask required");
        }
        if self.prescribed.len() != 3 * n
            || self.force.len() != 3 * n
            || self.prescribed.iter().chain(self.force).any(|v| !v.is_finite())
        {
            return contract("finite node-by-XYZ displacement and force arrays required");
        }
        let o = &self.options;
        if o.max_iterations < 1
            || !o.relative_tolerance.is_finite()
            || !(0.0 < o.relative_tolerance && o.relative_tolerance < 1.0)
            || !o.absolute_tolerance.is_finite()
            || o.absolute_tolerance <= 0.0
        {
            return contract("invalid Newton iteration limits");
        }
        let mut u = self.initial.map_or_else(|| vec![0.0; 3 * n], <[f64]>::to_vec);
        if u.len() != 3 * n || u.iter().any(|v| !v.is_finite()) {
            return contract("invalid initial displacement");
        }
        for (i, f) in self.fixed.iter().enumerate() {
            if *f {
                u[i] = self.prescribed[i];
            }
        }
        let j: Vec<f64> = (0..ne)
            .map(|e| det(&self.mesh.deformation_gradient(e, &local_u(&u, &self.mesh.elements[e]))))
            .collect();
        let jmin = j.iter().copied().fold(f64::INFINITY, f64::min);
        if j.iter().any(|v| !v.is_finite()) || jmin <= 0.0 {
            return contract("nonpositive initial deformed Jacobian");
        }
        if let Some(vs) = self.viscoelastic {
            validate_viscoelastic(vs, ne)?;
        }
        Ok(Validated { minimum_initial_j: jmin })
    }

    fn branch<S: Scalar>(&self, e: usize, f: &Mat3<S>) -> Option<GreenMaxwell<S>> {
        let vs = self.viscoelastic?;
        let prev: Vec<Mat3<S>> = vs.memory[e].iter().map(|m| m.map(|r| r.map(S::from_f64))).collect();
        let g: Vec<S> = vs.moduli[e].iter().map(|v| S::from_f64(*v)).collect();
        let t: Vec<S> = vs.times[e].iter().map(|v| S::from_f64(*v)).collect();
        Some(green_maxwell_step(f, &prev, &g, &t, S::from_f64(vs.step)))
    }

    fn element_residual<S: Scalar>(&self, e: usize, u: &[[S; 3]; 4]) -> [[S; 3]; 4] {
        let f = self.mesh.deformation_gradient(e, u);
        let mut p = neo_hookean_piola(&f, S::from_f64(self.mu[e]), S::from_f64(self.lam[e]));
        if let Some(vs) = self.viscoelastic {
            let prev: Vec<Mat3<S>> = vs.memory[e].iter().map(|m| m.map(|r| r.map(S::from_f64))).collect();
            let g: Vec<S> = vs.moduli[e].iter().map(|v| S::from_f64(*v)).collect();
            let t: Vec<S> = vs.times[e].iter().map(|v| S::from_f64(*v)).collect();
            let pb = maxwell_potential_piola(&f, &prev, &g, &t, S::from_f64(vs.step));
            for i in 0..3 {
                for j in 0..3 {
                    p[i][j] += pb[i][j];
                }
            }
        }
        self.mesh.nodal_forces(e, &p)
    }

    fn elastic_energy(&self, state: &[f64]) -> f64 {
        (0..self.mesh.elements.len())
            .map(|e| {
                let f = self.mesh.deformation_gradient(e, &local_u(state, &self.mesh.elements[e]));
                self.mesh.volumes[e] * neo_hookean_energy(&f, self.mu[e], self.lam[e])
            })
            .sum()
    }

    fn energy(&self, state: &[f64]) -> f64 {
        let mut w = self.elastic_energy(state);
        if self.viscoelastic.is_some() {
            for e in 0..self.mesh.elements.len() {
                let f = self.mesh.deformation_gradient(e, &local_u(state, &self.mesh.elements[e]));
                if let Some(b) = self.branch(e, &f) {
                    w += self.mesh.volumes[e] * b.potential;
                }
            }
        }
        w
    }

    fn gradient(&self, state: &[f64]) -> Vec<f64> {
        let mut g = vec![0.0; state.len()];
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let r = self.element_residual(e, &local_u(state, t));
            for a in 0..4 {
                for i in 0..3 {
                    g[3 * t[a] + i] += r[a][i];
                }
            }
        }
        g
    }

    fn tangent(&self, state: &[f64]) -> Result<DenseMatrix, CaeError> {
        let n = state.len();
        let mut h = DenseMatrix::zeros(n, n);
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let local: Vec<f64> =
                t.iter().flat_map(|node| (0..3).map(move |i| state[3 * node + i])).collect();
            let jac = implexity_ad::forward::jacobian::<12, _>(
                |s: &[Dual<12>]| {
                    let u: [[Dual<12>; 3]; 4] =
                        std::array::from_fn(|a| std::array::from_fn(|i| s[3 * a + i]));
                    self.element_residual(e, &u).iter().flatten().copied().collect()
                },
                &local,
            )
            .map_err(|err| CaeError::contract(err.to_string()))?;
            let dofs: Vec<usize> = t.iter().flat_map(|node| (0..3).map(move |i| 3 * node + i)).collect();
            for (a, ra) in dofs.iter().enumerate() {
                for (b, cb) in dofs.iter().enumerate() {
                    h.data[ra * n + cb] += jac.matrix[a * 12 + b];
                }
            }
        }
        Ok(h)
    }

    fn jacobians(&self, state: &[f64]) -> Vec<f64> {
        (0..self.mesh.elements.len())
            .map(|e| det(&self.mesh.deformation_gradient(e, &local_u(state, &self.mesh.elements[e]))))
            .collect()
    }


    #[allow(clippy::too_many_lines)]
    pub fn solve(&self) -> Result<StaticResult, CaeError> {
        self.validate()?;
        let n = self.mesh.node_count();
        let o = self.options;
        let mut state = self.initial.map_or_else(|| vec![0.0; 3 * n], <[f64]>::to_vec);
        for (i, f) in self.fixed.iter().enumerate() {
            if *f {
                state[i] = self.prescribed[i];
            }
        }
        let free: Vec<usize> = (0..3 * n).filter(|i| !self.fixed[*i]).collect();
        let external = self.force;
        let mut scale: Option<f64> = None;
        let mut history: Vec<Value> = Vec::new();
        let mut residual;
        let mut norm;
        let mut iteration = 0;
        loop {
            let j = self.jacobians(&state);
            let jmin = j.iter().copied().fold(f64::INFINITY, f64::min);
            if j.iter().any(|v| !v.is_finite()) || jmin <= 0.0 {
                return contract("nonpositive deformed Jacobian");
            }
            residual = self.gradient(&state).iter().zip(external).map(|(a, b)| a - b).collect::<Vec<f64>>();
            norm = free.iter().map(|i| residual[*i].abs()).fold(0.0, f64::max);
            let s = *scale
                .get_or_insert_with(|| norm.max(free.iter().map(|i| external[*i].abs()).fold(0.0, f64::max)));
            history.push(json!({"iteration": iteration, "free_residual_N": norm, "minimum_J": jmin}));
            if norm <= o.absolute_tolerance + o.relative_tolerance * s {
                break;
            }
            if iteration == o.max_iterations {
                return contract("hyperelastic Newton iteration limit reached");
            }
            let h = self.tangent(&state)?;
            let k = DenseMatrix {
                nrows: free.len(),
                ncols: free.len(),
                data: free
                    .iter()
                    .flat_map(|r| free.iter().map(|c| h.data[r * 3 * n + c]).collect::<Vec<_>>())
                    .collect(),
            };
            if k.data.iter().any(|v| !v.is_finite()) {
                return contract("nonfinite hyperelastic tangent");
            }
            let rhs: Vec<f64> = free.iter().map(|i| -residual[*i]).collect();
            let direction = implexity_linalg::dense::solve(&k, &rhs, 1)
                .map_err(|_| CaeError::contract("singular constrained hyperelastic tangent"))?;
            let slope: f64 = free.iter().zip(&direction).map(|(i, d)| residual[*i] * d).sum();
            if !slope.is_finite() || slope >= 0.0 {
                return contract("Newton direction is not an energy descent direction");
            }
            let dot = |x: &[f64]| x.iter().zip(external).map(|(a, b)| a * b).sum::<f64>();
            let potential = self.energy(&state) - dot(&state);
            let mut accepted = false;
            for backtrack in 0..24 {
                let alpha = 2.0_f64.powi(-backtrack);
                let mut trial = state.clone();
                for (i, d) in free.iter().zip(&direction) {
                    trial[*i] += alpha * d;
                }
                let tj = self.jacobians(&trial);
                if tj.iter().any(|v| !v.is_finite())
                    || tj.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0
                {
                    continue;
                }
                let trial_potential = self.energy(&trial) - dot(&trial);
                let armijo =
                    trial_potential.is_finite() && trial_potential <= potential + 1e-4 * alpha * slope;
                let rounding =
                    32.0 * f64::EPSILON * potential.abs().max(trial_potential.abs()).max(f64::MIN_POSITIVE);
                let near = trial_potential.is_finite()
                    && (alpha * slope).abs() <= rounding
                    && (trial_potential - potential).abs() <= rounding;
                let descent = near && {
                    let g = self.gradient(&trial);
                    free.iter().map(|i| (g[*i] - external[*i]).abs()).fold(0.0, f64::max) < norm
                };
                if armijo || descent {
                    state = trial;
                    if let Some(last) = history.last_mut() {
                        last["step_fraction"] = json!(alpha);
                    }
                    accepted = true;
                    break;
                }
            }
            if !accepted {
                return contract(format!(
                    "hyperelastic line search failed at iteration {iteration}, free residual {} N",
                    implexity_core::py_repr::repr_float(norm)
                ));
            }
            iteration += 1;
        }
        let mut reaction = residual.clone();
        for i in &free {
            reaction[*i] = 0.0;
        }
        let ne = self.mesh.elements.len();
        let mut stress = Vec::with_capacity(ne);
        let mut jac = Vec::with_capacity(ne);
        let mut physical = self.elastic_energy(&state);
        let mut branch_strain = Vec::new();
        let (mut dissipation, mut stored_branch) = (0.0, 0.0);
        for (e, t) in self.mesh.elements.iter().enumerate() {
            let f = self.mesh.deformation_gradient(e, &local_u(&state, t));
            let j = det(&f);
            let mut s = neo_hookean_cauchy(&f, self.mu[e], self.lam[e]);
            if let Some(b) = self.branch(e, &f) {
                let extra = matmul(&b.first_piola, &transpose(&f));
                for r in 0..3 {
                    for c in 0..3 {
                        s[r][c] += extra[r][c] / j;
                    }
                }
                stored_branch += self.mesh.volumes[e] * b.stored;
                dissipation += self.mesh.volumes[e] * b.dissipated;
                branch_strain.push(b.branch_strain);
            }
            stress.push(s);
            jac.push(j);
        }
        let mut result = StaticResult {
            displacement: state.chunks(3).map(|c| [c[0], c[1], c[2]]).collect(),
            cauchy_stress: stress,
            jacobian: jac,
            stored_energy: 0.0,
            support_reactions: reaction.chunks(3).map(|c| [c[0], c[1], c[2]]).collect(),
            free_residual: norm,
            iterations: history,
            branch_strain: None,
            dissipation_increment: None,
            incremental_potential: None,
        };
        if self.viscoelastic.is_some() {
            physical += stored_branch;
            result.branch_strain = Some(branch_strain);
            result.dissipation_increment = Some(dissipation);
            result.incremental_potential = Some(self.energy(&state));
        }
        result.stored_energy = physical;
        Ok(result)
    }
}


pub fn validate_viscoelastic(vs: &ViscoelasticState, ne: usize) -> Result<(), CaeError> {
    let nb = vs.moduli.first().map_or(0, Vec::len);
    if vs.moduli.len() != ne
        || nb < 1
        || vs.moduli.iter().any(|r| r.len() != nb)
        || vs.times.len() != ne
        || vs.times.iter().any(|r| r.len() != nb)
        || vs.memory.len() != ne
        || vs.memory.iter().any(|r| r.len() != nb)
    {
        return contract(
            "branch arrays require element-by-branch materials and element-by-branch-by-3-by-3 memory",
        );
    }
    let finite = vs
        .memory
        .iter()
        .flatten()
        .flatten()
        .flatten()
        .chain(vs.moduli.iter().flatten())
        .chain(vs.times.iter().flatten())
        .all(Scalar::is_finite)
        && vs.step.is_finite();
    if !finite
        || vs.moduli.iter().flatten().any(|v| *v < 0.0)
        || vs.times.iter().flatten().any(|v| *v <= 0.0)
        || vs.step <= 0.0
    {
        return contract("finite memory, nonnegative moduli and positive times required");
    }
    let symmetric = vs
        .memory
        .iter()
        .flatten()
        .all(|m| (0..3).all(|i| (0..3).all(|j| (m[i][j] - m[j][i]).abs() <= 1e-12)));
    if !symmetric {
        return contract("material memory must be symmetric");
    }
    Ok(())
}

pub type HistoryRecord = StaticResult;


#[allow(clippy::too_many_arguments)]
pub fn solve_viscoelastic_history(
    mesh: &TetMesh,
    mu: &[f64],
    lam: &[f64],
    fixed: &[bool],
    prescribed_history: &[Vec<f64>],
    force_history: &[Vec<f64>],
    moduli: &[Vec<f64>],
    times: &[Vec<f64>],
    steps: &[f64],
    initial_branch_strain: Vec<Vec<Mat3<f64>>>,
    initial_displacement: Option<Vec<f64>>,
) -> Result<Vec<HistoryRecord>, CaeError> {
    let n = mesh.node_count();
    if !(1..=1000).contains(&steps.len()) || steps.iter().any(|s| !s.is_finite() || *s <= 0.0) {
        return contract("history requires 1..1000 finite positive steps");
    }
    let ok = |h: &[Vec<f64>]| {
        h.len() == steps.len() && h.iter().all(|r| r.len() == 3 * n && r.iter().all(Scalar::is_finite))
    };
    if !ok(prescribed_history) || !ok(force_history) {
        return contract("finite step-by-node-by-XYZ histories required");
    }
    let mut u = initial_displacement;
    let mut memory = initial_branch_strain;
    let mut records = Vec::with_capacity(steps.len());
    for ((dt, displacement), force) in steps.iter().zip(prescribed_history).zip(force_history) {
        let vs = ViscoelasticState {
            memory: memory.clone(),
            moduli: moduli.to_vec(),
            times: times.to_vec(),
            step: *dt,
        };
        let problem = StaticProblem {
            mesh,
            mu,
            lam,
            fixed,
            prescribed: displacement,
            force,
            initial: u.as_deref(),
            viscoelastic: Some(&vs),
            options: StaticOptions::default(),
        };
        let result = problem.solve()?;
        u = Some(result.displacement.iter().flatten().copied().collect());
        memory = result.branch_strain.clone().unwrap_or_default();
        records.push(result);
    }
    Ok(records)
}
