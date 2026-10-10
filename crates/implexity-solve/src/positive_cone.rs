// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{
    dense::{DenseLu, DenseMatrix},
    lu::SparseCholesky,
    CscMatrix,
};
use std::sync::Arc;
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    from = "ConeFactorizationDocument"
)]
pub enum ConeFactorization {
    DenseReference { maximum_unknowns: usize },
    Sparse,
}
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ConeFactorizationDocument {
    DenseReference { maximum_unknowns: usize },
    Sparse {},
}
impl From<ConeFactorizationDocument> for ConeFactorization {
    fn from(value: ConeFactorizationDocument) -> Self {
        match value {
            ConeFactorizationDocument::DenseReference { maximum_unknowns } => {
                Self::DenseReference { maximum_unknowns }
            }
            ConeFactorizationDocument::Sparse {} => Self::Sparse,
        }
    }
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConeProjectionPolicy {
    pub factorization: ConeFactorization,
    pub maximum_iterations: usize,
    pub tolerance: f64,
    pub maximum_schur_condition: f64,
    pub derivative_relative_dual_tolerance: f64,
}
#[derive(Clone)]
enum Factor {
    Dense(DenseLu),
    Sparse(SparseCholesky),
}
impl Factor {
    fn solve(&self, b: &[f64]) -> CaeResult<Vec<f64>> {
        let x = match self {
            Self::Dense(f) => f.solve(b, 1, false).map_err(|e| fail(e.to_string()))?,
            Self::Sparse(f) => {
                let mut x = b.to_vec();
                f.solve_in_place(&mut x).map_err(|e| fail(e.to_string()))?;
                x
            }
        };
        if x.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite positive-cone factor solve"));
        }
        Ok(x)
    }
}
#[derive(Clone)]
struct FreeFactor {
    free: Vec<usize>,
    inverse: Factor,
    lift: Vec<Vec<f64>>,
    schur: DenseLu,
}
#[derive(Clone)]
pub struct ConeProjectionProblem {
    metric: Arc<Vec<Vec<(usize, f64)>>>,
    equality: Arc<Vec<Vec<f64>>>,
    policy: ConeProjectionPolicy,
}
#[derive(Clone)]
pub struct ConeProjection {
    pub state: Vec<f64>,
    pub equality_multipliers: Vec<f64>,
    pub cone_multipliers: Vec<f64>,
    pub iterations: usize,
    problem: ConeProjectionProblem,
    factor: Arc<FreeFactor>,
    stationarity_scales: Vec<f64>,
}
impl ConeProjectionProblem {
    pub fn new(
        metric: Vec<Vec<(usize, f64)>>,
        equality: Vec<Vec<f64>>,
        policy: ConeProjectionPolicy,
    ) -> CaeResult<Self> {
        let n = metric.len();
        if n == 0
            || equality.is_empty()
            || equality.len() > n
            || equality
                .iter()
                .any(|r| r.len() != n || r.iter().any(|x| !x.is_finite()))
            || policy.maximum_iterations == 0
            || !policy.tolerance.is_finite()
            || policy.tolerance <= 0.
            || !policy.maximum_schur_condition.is_finite()
            || policy.maximum_schur_condition <= 1.
            || !policy.derivative_relative_dual_tolerance.is_finite()
            || policy.derivative_relative_dual_tolerance < 0.
            || policy.derivative_relative_dual_tolerance >= 1.
        {
            return Err(fail("invalid positive-cone problem or policy"));
        }
        for (i, row) in metric.iter().enumerate() {
            let mut last = None;
            for &(j, v) in row {
                if j >= n || !v.is_finite() || last.is_some_and(|k| k >= j) {
                    return Err(fail("invalid positive-cone sparse metric"));
                }
                let other = metric[j]
                    .binary_search_by_key(&i, |p| p.0)
                    .ok()
                    .map_or(0., |k| metric[j][k].1);
                if v != other {
                    return Err(fail("positive-cone metric must be symmetric"));
                }
                last = Some(j);
            }
        }
        if let ConeFactorization::DenseReference { maximum_unknowns } = policy.factorization {
            if n > maximum_unknowns {
                return Err(fail(
                    "dense positive-cone numerical reference size exceeded",
                ));
            }
        }
        let p = Self {
            metric: Arc::new(metric),
            equality: Arc::new(equality),
            policy,
        };
        let all = (0..n).collect::<Vec<_>>();
        let matrix = p.free_matrix(&all)?;
        SparseCholesky::new(&matrix)
            .map_err(|e| fail(format!("positive-cone metric is not SPD: {e}")))?;
        p.factor(&all)?;
        Ok(p)
    }
    pub fn dimension(&self) -> usize {
        self.metric.len()
    }
    pub fn equality_action(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        self.check(x)?;
        let out = self.equality.iter().map(|r| dot(r, x)).collect::<Vec<_>>();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite positive-cone equality action"));
        }
        Ok(out)
    }
    pub fn metric_action(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        self.check(x)?;
        let out = self
            .metric
            .iter()
            .map(|r| r.iter().map(|(j, v)| v * x[*j]).sum::<f64>())
            .collect::<Vec<_>>();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite positive-cone metric action"));
        }
        Ok(out)
    }
    fn metric_absolute_action(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        self.check(x)?;
        let out = self
            .metric
            .iter()
            .map(|r| r.iter().map(|(j, v)| (v * x[*j]).abs()).sum::<f64>())
            .collect::<Vec<_>>();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite positive-cone backward-error scale"));
        }
        Ok(out)
    }
    fn equality_valid(&self, x: &[f64], target: &[f64]) -> CaeResult<bool> {
        let got = self.equality_action(x)?;
        Ok(self
            .equality
            .iter()
            .zip(got)
            .zip(target)
            .all(|((r, v), t)| {
                let scale = t.abs() + r.iter().zip(x).map(|(a, b)| (a * b).abs()).sum::<f64>();
                scale.is_finite() && (v - t).abs() <= self.policy.tolerance * scale
            }))
    }
    fn check(&self, x: &[f64]) -> CaeResult<()> {
        if x.len() != self.dimension() || x.iter().any(|v| !v.is_finite()) {
            Err(fail("positive-cone vector shape or value invalid"))
        } else {
            Ok(())
        }
    }
    fn free_matrix(&self, free: &[usize]) -> CaeResult<CscMatrix> {
        let mut map = vec![usize::MAX; self.dimension()];
        for (i, &j) in free.iter().enumerate() {
            map[j] = i;
        }
        let (mut rows, mut cols, mut vals) = (vec![], vec![], vec![]);
        for (i, &j) in free.iter().enumerate() {
            for &(k, v) in &self.metric[j] {
                if map[k] != usize::MAX {
                    rows.push(i);
                    cols.push(map[k]);
                    vals.push(v);
                }
            }
        }
        CscMatrix::from_triplets(free.len(), free.len(), &rows, &cols, &vals)
            .map_err(|e| fail(e.to_string()))
    }
    fn factor(&self, free: &[usize]) -> CaeResult<FreeFactor> {
        self.factor_with_lift(free, &self.equality)
    }
    fn factor_with_lift(&self, free: &[usize], lifting: &[Vec<f64>]) -> CaeResult<FreeFactor> {
        if free.len() < self.equality.len() {
            return Err(fail("positive-cone free moment rank is insufficient"));
        }
        let a = self.free_matrix(free)?;
        let inverse = match self.policy.factorization {
            ConeFactorization::Sparse => {
                Factor::Sparse(SparseCholesky::new(&a).map_err(|e| fail(e.to_string()))?)
            }
            ConeFactorization::DenseReference { .. } => {
                let dense = (0..free.len())
                    .flat_map(|i| (0..free.len()).map(move |j| (i, j)))
                    .map(|(i, j)| a.get(i, j))
                    .collect();
                Factor::Dense(
                    DenseLu::new(
                        &DenseMatrix::new(free.len(), free.len(), dense)
                            .map_err(|e| fail(e.to_string()))?,
                    )
                    .map_err(|e| fail(e.to_string()))?,
                )
            }
        };
        let lift = lifting
            .iter()
            .map(|r| inverse.solve(&free.iter().map(|j| r[*j]).collect::<Vec<_>>()))
            .collect::<CaeResult<Vec<_>>>()?;
        let m = self.equality.len();
        let data = (0..m)
            .flat_map(|i| (0..m).map(move |j| (i, j)))
            .map(|(i, j)| {
                free.iter()
                    .enumerate()
                    .map(|(k, v)| self.equality[i][*v] * lift[j][k])
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let norm = (0..m)
            .map(|i| {
                data[i * m..(i + 1) * m]
                    .iter()
                    .map(|x| x.abs())
                    .sum::<f64>()
            })
            .fold(0., f64::max);
        let schur = DenseLu::new(&DenseMatrix::new(m, m, data).map_err(|e| fail(e.to_string()))?)
            .map_err(|e| fail(e.to_string()))?;
        let mut inverse_norm = vec![0.; m];
        for j in 0..m {
            let mut e = vec![0.; m];
            e[j] = 1.;
            let col = schur.solve(&e, 1, false).map_err(|e| fail(e.to_string()))?;
            for i in 0..m {
                inverse_norm[i] += col[i].abs();
            }
        }
        if norm * inverse_norm.into_iter().fold(0., f64::max) > self.policy.maximum_schur_condition
        {
            return Err(fail(
                "positive-cone equality Schur complement is ill conditioned",
            ));
        }
        Ok(FreeFactor {
            free: free.to_vec(),
            inverse,
            lift,
            schur,
        })
    }
    pub fn try_project_cached(
        &self,
        predictor: &[f64],
        target: &[f64],
        previous: &ConeProjection,
    ) -> CaeResult<Option<ConeProjection>> {
        self.check(predictor)?;
        let m = self.equality.len();
        if target.len() != m || target.iter().any(|v| !v.is_finite()) {
            return Err(fail("positive-cone cached equality target invalid"));
        }
        if !Arc::ptr_eq(&self.metric, &previous.problem.metric)
            || !Arc::ptr_eq(&self.equality, &previous.problem.equality)
        {
            return Ok(None);
        }
        self.certified_factor(predictor, target, Arc::clone(&previous.factor))
    }
    fn factor_candidate(
        &self,
        predictor: &[f64],
        target: &[f64],
        f: &FreeFactor,
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>)> {
        let m = self.equality.len();
        let free = &f.free;
        let mut active_predictor = predictor.to_vec();
        for &j in free {
            active_predictor[j] = 0.;
        }
        let active_metric = self.metric_action(&active_predictor)?;
        let z = f
            .inverse
            .solve(&free.iter().map(|j| active_metric[*j]).collect::<Vec<_>>())?;
        let predictor_equality = self.equality_action(predictor)?;
        let active_equality = self.equality_action(&active_predictor)?;
        let rhs = (0..m)
            .map(|i| {
                free.iter()
                    .enumerate()
                    .map(|(k, j)| self.equality[i][*j] * z[k])
                    .sum::<f64>()
                    - active_equality[i]
                    + (predictor_equality[i] - target[i])
            })
            .collect::<Vec<_>>();
        let lambda = f
            .schur
            .solve(&rhs, 1, false)
            .map_err(|e| fail(e.to_string()))?;
        let mut x = vec![0.; self.dimension()];
        for (k, &j) in free.iter().enumerate() {
            x[j] = predictor[j] + (z[k] - (0..m).map(|i| f.lift[i][k] * lambda[i]).sum::<f64>());
        }
        let difference = x
            .iter()
            .zip(predictor)
            .map(|(x, y)| x - y)
            .collect::<Vec<_>>();
        let metric_difference = self.metric_action(&difference)?;
        let eta = (0..x.len())
            .map(|j| {
                metric_difference[j] + (0..m).map(|i| self.equality[i][j] * lambda[i]).sum::<f64>()
            })
            .collect::<Vec<_>>();
        let ax = self.metric_absolute_action(&x)?;
        let ay = self.metric_absolute_action(predictor)?;
        let scales = (0..x.len())
            .map(|j| {
                ax[j]
                    + ay[j]
                    + (0..m)
                        .map(|i| (self.equality[i][j] * lambda[i]).abs())
                        .sum::<f64>()
            })
            .collect::<Vec<_>>();
        Ok((x, lambda, eta, scales))
    }
    fn certified_factor(
        &self,
        predictor: &[f64],
        target: &[f64],
        f: Arc<FreeFactor>,
    ) -> CaeResult<Option<ConeProjection>> {
        let free = &f.free;
        let (x, lambda, eta, scales) = self.factor_candidate(predictor, target, &f)?;
        let mut is_free = vec![false; self.dimension()];
        for &j in free {
            is_free[j] = true;
        }
        if x.iter().any(|v| !v.is_finite() || *v < 0.)
            || !self.equality_valid(&x, target)?
            || eta
                .iter()
                .zip(&scales)
                .any(|(v, scale)| !v.is_finite() || !scale.is_finite())
            || free
                .iter()
                .any(|j| eta[*j].abs() > self.policy.tolerance * scales[*j])
            || (0..x.len()).any(|j| !is_free[j] && eta[j] < -self.policy.tolerance * scales[j])
            || (0..x.len()).any(|j| x[j] != 0. && eta[j].abs() > self.policy.tolerance * scales[j])
        {
            return Ok(None);
        }
        Ok(Some(ConeProjection {
            state: x,
            equality_multipliers: lambda,
            cone_multipliers: eta,
            iterations: 1,
            problem: self.clone(),
            factor: f,
            stationarity_scales: scales,
        }))
    }
    pub fn project(
        &self,
        predictor: &[f64],
        target: &[f64],
        feasible: &[f64],
    ) -> CaeResult<ConeProjection> {
        self.check(predictor)?;
        self.check(feasible)?;
        let m = self.equality.len();
        if target.len() != m
            || target.iter().any(|v| !v.is_finite())
            || feasible.iter().any(|v| *v < 0.)
        {
            return Err(fail("invalid positive-cone feasible initializer"));
        }
        if !self.equality_valid(feasible, target)? {
            return Err(fail(
                "positive-cone initializer does not satisfy the equality target",
            ));
        }
        if predictor.iter().all(|x| *x >= 0.)
            && self
                .equality_action(predictor)?
                .iter()
                .zip(target)
                .all(|(x, y)| x == y)
        {
            let free = (0..self.dimension()).collect::<Vec<_>>();
            return Ok(ConeProjection {
                state: predictor.to_vec(),
                equality_multipliers: vec![0.; m],
                cone_multipliers: vec![0.; self.dimension()],
                iterations: 0,
                problem: self.clone(),
                factor: Arc::new(self.factor(&free)?),
                stationarity_scales: self
                    .metric_absolute_action(predictor)?
                    .into_iter()
                    .map(|v| 2. * v)
                    .collect(),
            });
        }
        let mut attempts = 0;
        let predictive_budget = (self.policy.maximum_iterations / 4).min(8);
        let mut proposed = vec![false; self.dimension()];
        let mut seen = std::collections::BTreeSet::new();
        while attempts < predictive_budget {
            let free = (0..self.dimension())
                .filter(|j| !proposed[*j])
                .collect::<Vec<_>>();
            if !seen.insert(free.clone()) {
                break;
            }
            attempts += 1;
            let Ok(factor) = self.factor(&free) else {
                break;
            };
            let factor = Arc::new(factor);
            let Ok(candidate) = self.factor_candidate(predictor, target, &factor) else {
                break;
            };
            if let Some(mut accepted) =
                self.certified_factor(predictor, target, Arc::clone(&factor))?
            {
                accepted.iterations = attempts;
                return Ok(accepted);
            }
            let (state, _, dual, scales) = candidate;
            let mut changed = false;
            for j in 0..self.dimension() {
                if !proposed[j] && state[j] < 0. {
                    proposed[j] = true;
                    changed = true;
                } else if proposed[j] && dual[j] < -self.policy.tolerance * scales[j] {
                    proposed[j] = false;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut x = feasible.to_vec();
        let mut active = vec![false; self.dimension()];
        for iteration in attempts..self.policy.maximum_iterations {
            let free = (0..active.len())
                .filter(|j| !active[*j])
                .collect::<Vec<_>>();
            let f = self.factor(&free)?;
            let (candidate, lambda, eta, scales) = self.factor_candidate(predictor, target, &f)?;
            if candidate.iter().any(|x| !x.is_finite()) {
                return Err(fail("nonfinite positive-cone candidate"));
            }
            let mut block = None;
            let mut alpha = 1.;
            for &j in &free {
                if candidate[j] < 0. {
                    let a = x[j] / (x[j] - candidate[j]);
                    if a <= alpha {
                        alpha = a;
                        block = Some(j);
                    }
                }
            }
            if let Some(j) = block {
                for k in 0..x.len() {
                    x[k] += alpha * (candidate[k] - x[k]);
                }
                x[j] = 0.;
                active[j] = true;
                continue;
            }
            x = candidate;
            let release = (0..x.len())
                .filter(|j| active[*j] && eta[*j] < -self.policy.tolerance * scales[*j])
                .min_by(|a, b| eta[*a].total_cmp(&eta[*b]));
            if let Some(j) = release {
                active[j] = false;
                continue;
            }
            if !self.equality_valid(&x, target)?
                || eta
                    .iter()
                    .zip(&scales)
                    .any(|(v, scale)| !v.is_finite() || !scale.is_finite())
                || x.iter().any(|v| *v < 0.)
                || free
                    .iter()
                    .any(|j| eta[*j].abs() > self.policy.tolerance * scales[*j])
                || (0..x.len()).any(|j| active[j] && eta[j] < -self.policy.tolerance * scales[j])
                || (0..x.len())
                    .any(|j| x[j] != 0. && eta[j].abs() > self.policy.tolerance * scales[j])
            {
                return Err(fail("positive-cone KKT residual exceeds tolerance"));
            }
            return Ok(ConeProjection {
                state: x,
                equality_multipliers: lambda,
                cone_multipliers: eta,
                iterations: iteration + 1,
                problem: self.clone(),
                factor: Arc::new(f),
                stationarity_scales: scales,
            });
        }
        Err(fail("positive-cone active-set iteration budget exhausted"))
    }
}
impl ConeProjection {
    fn derivative_domain(&self) -> CaeResult<()> {
        let p = &self.problem;
        let mut free = vec![false; p.dimension()];
        for &j in &self.factor.free {
            free[j] = true;
        }
        for j in 0..free.len() {
            if (free[j] && self.state[j] <= 0.)
                || (!free[j]
                    && self.cone_multipliers[j]
                        <= p.policy.derivative_relative_dual_tolerance
                            * self.stationarity_scales[j])
            {
                return Err(fail(&format!(
                    "positive-cone derivative requires a strict fixed active stratum at coordinate {j}, free {}, state {:.17e}, dual {:.17e}, stationarity scale {:.17e}, relative dual tolerance {:.17e}",
                    free[j],
                    self.state[j],
                    self.cone_multipliers[j],
                    self.stationarity_scales[j],
                    p.policy.derivative_relative_dual_tolerance
                )));
            }
        }
        Ok(())
    }
    pub fn strict_active_indices(&self) -> CaeResult<Vec<usize>> {
        self.derivative_domain()?;
        Ok(self.selected_kkt_active_indices())
    }
    pub fn selected_kkt_active_indices(&self) -> Vec<usize> {
        let mut free = vec![false; self.problem.dimension()];
        for &j in &self.factor.free {
            free[j] = true;
        }
        free.iter()
            .enumerate()
            .filter_map(|(j, value)| (!*value).then_some(j))
            .collect()
    }
    pub fn jvp(
        &self,
        predictor_direction: &[f64],
        target_direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.derivative_domain()?;
        self.selected_kkt_jvp(predictor_direction, target_direction)
    }
    pub fn selected_kkt_jvp(
        &self,
        predictor_direction: &[f64],
        target_direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        let p = &self.problem;
        p.check(predictor_direction)?;
        let m = p.equality.len();
        if target_direction.len() != m || target_direction.iter().any(|v| !v.is_finite()) {
            return Err(fail("positive-cone target direction invalid"));
        }
        let rhs = p.metric_action(predictor_direction)?;
        let z = self
            .factor
            .inverse
            .solve(&self.factor.free.iter().map(|j| rhs[*j]).collect::<Vec<_>>())?;
        let rhs = (0..m)
            .map(|i| {
                self.factor
                    .free
                    .iter()
                    .enumerate()
                    .map(|(k, j)| p.equality[i][*j] * z[k])
                    .sum::<f64>()
                    - target_direction[i]
            })
            .collect::<Vec<_>>();
        let lambda = self
            .factor
            .schur
            .solve(&rhs, 1, false)
            .map_err(|e| fail(e.to_string()))?;
        let mut out = vec![0.; p.dimension()];
        for (k, &j) in self.factor.free.iter().enumerate() {
            out[j] = z[k]
                - (0..m)
                    .map(|i| self.factor.lift[i][k] * lambda[i])
                    .sum::<f64>();
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite positive-cone direction"));
        }
        Ok(out)
    }
    pub fn vjp(&self, bar: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.derivative_domain()?;
        self.selected_kkt_vjp(bar)
    }
    pub fn selected_kkt_vjp(&self, bar: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let p = &self.problem;
        p.check(bar)?;
        let z = self
            .factor
            .inverse
            .solve(&self.factor.free.iter().map(|j| bar[*j]).collect::<Vec<_>>())?;
        let m = p.equality.len();
        let rhs = (0..m)
            .map(|i| {
                self.factor
                    .free
                    .iter()
                    .enumerate()
                    .map(|(k, j)| p.equality[i][*j] * z[k])
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let target = self
            .factor
            .schur
            .solve(&rhs, 1, true)
            .map_err(|e| fail(e.to_string()))?;
        let mut lift = vec![0.; p.dimension()];
        for (k, &j) in self.factor.free.iter().enumerate() {
            lift[j] = z[k]
                - (0..m)
                    .map(|i| self.factor.lift[i][k] * target[i])
                    .sum::<f64>();
        }
        if target.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite positive-cone target cotangent"));
        }
        Ok((p.metric_action(&lift)?, target))
    }
}
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[derive(Clone)]
pub struct LinearConeResidualProblem {
    operator: ConeProjectionProblem,
    lifting: Arc<Vec<Vec<f64>>>,
    target: Arc<Vec<f64>>,
}
#[derive(Clone)]
pub struct LinearConeResidual {
    pub state: Vec<f64>,
    pub equality_multipliers: Vec<f64>,
    pub cone_multipliers: Vec<f64>,
    pub iterations: usize,
    problem: LinearConeResidualProblem,
    factor: Arc<FreeFactor>,
    stationarity_scales: Vec<f64>,
}
impl LinearConeResidualProblem {
    pub fn new(
        operator_rows: Vec<Vec<(usize, f64)>>,
        multiplier_lifting_rows: Vec<Vec<f64>>,
        constraint_rows: Vec<Vec<f64>>,
        target: Vec<f64>,
        policy: ConeProjectionPolicy,
    ) -> CaeResult<Self> {
        let operator = ConeProjectionProblem::new(operator_rows, constraint_rows, policy)?;
        if multiplier_lifting_rows.len() != operator.equality.len()
            || multiplier_lifting_rows
                .iter()
                .any(|r| r.len() != operator.dimension() || r.iter().any(|v| !v.is_finite()))
            || target.len() != operator.equality.len()
            || target.iter().any(|v| !v.is_finite())
        {
            return Err(fail("invalid linear cone residual lifting or target"));
        }
        operator.factor_with_lift(
            &(0..operator.dimension()).collect::<Vec<_>>(),
            &multiplier_lifting_rows,
        )?;
        Ok(Self {
            operator,
            lifting: Arc::new(multiplier_lifting_rows),
            target: Arc::new(target),
        })
    }
    pub fn solve(&self, rhs: &[f64], initial_nonnegative: &[f64]) -> CaeResult<LinearConeResidual> {
        let p = &self.operator;
        p.check(rhs)?;
        p.check(initial_nonnegative)?;
        if initial_nonnegative.iter().any(|v| *v < 0.) {
            return Err(fail(
                "linear cone residual initial iterate must be nonnegative",
            ));
        }
        let n = p.dimension();
        let m = p.equality.len();
        let mut x = initial_nonnegative.to_vec();
        let mut active = vec![false; n];
        for it in 0..p.policy.maximum_iterations {
            let free = (0..n).filter(|j| !active[*j]).collect::<Vec<_>>();
            let f = p.factor_with_lift(&free, &self.lifting)?;
            let z = f
                .inverse
                .solve(&free.iter().map(|j| rhs[*j]).collect::<Vec<_>>())?;
            let residual = (0..m)
                .map(|i| {
                    free.iter()
                        .enumerate()
                        .map(|(k, j)| p.equality[i][*j] * z[k])
                        .sum::<f64>()
                        - self.target[i]
                })
                .collect::<Vec<_>>();
            let lambda = f
                .schur
                .solve(&residual, 1, false)
                .map_err(|e| fail(e.to_string()))?;
            let mut candidate = vec![0.; n];
            for (k, &j) in free.iter().enumerate() {
                candidate[j] = z[k] - (0..m).map(|i| f.lift[i][k] * lambda[i]).sum::<f64>();
            }
            if candidate.iter().any(|v| !v.is_finite()) {
                return Err(fail("nonfinite linear cone residual candidate"));
            }
            let mut block = None;
            let mut alpha = 1.;
            for &j in &free {
                if candidate[j] < 0. {
                    let a = x[j] / (x[j] - candidate[j]);
                    if a <= alpha {
                        alpha = a;
                        block = Some(j);
                    }
                }
            }
            if let Some(j) = block {
                for k in 0..n {
                    x[k] += alpha * (candidate[k] - x[k]);
                }
                x[j] = 0.;
                active[j] = true;
                continue;
            }
            x = candidate;
            let ax = p.metric_action(&x)?;
            let eta = (0..n)
                .map(|j| {
                    ax[j] - rhs[j] + (0..m).map(|i| self.lifting[i][j] * lambda[i]).sum::<f64>()
                })
                .collect::<Vec<_>>();
            let absolute = p.metric_absolute_action(&x)?;
            let scales = (0..n)
                .map(|j| {
                    absolute[j]
                        + rhs[j].abs()
                        + (0..m)
                            .map(|i| (self.lifting[i][j] * lambda[i]).abs())
                            .sum::<f64>()
                })
                .collect::<Vec<_>>();
            let release = (0..n)
                .filter(|j| active[*j] && eta[*j] < -p.policy.tolerance * scales[*j])
                .min_by(|a, b| eta[*a].total_cmp(&eta[*b]));
            if let Some(j) = release {
                active[j] = false;
                continue;
            }
            if !p.equality_valid(&x, &self.target)?
                || eta
                    .iter()
                    .zip(&scales)
                    .any(|(v, scale)| !v.is_finite() || !scale.is_finite())
                || x.iter().any(|v| *v < 0.)
                || free
                    .iter()
                    .any(|j| eta[*j].abs() > p.policy.tolerance * scales[*j])
                || (0..n).any(|j| active[j] && eta[j] < -p.policy.tolerance * scales[j])
                || (0..n).any(|j| x[j] != 0. && eta[j].abs() > p.policy.tolerance * scales[j])
            {
                return Err(fail("linear cone residual KKT error exceeds tolerance"));
            }
            return Ok(LinearConeResidual {
                state: x,
                equality_multipliers: lambda,
                cone_multipliers: eta,
                iterations: it + 1,
                problem: self.clone(),
                factor: Arc::new(f),
                stationarity_scales: scales,
            });
        }
        Err(fail("linear cone residual active-set budget exhausted"))
    }
}
impl LinearConeResidual {
    fn derivative_domain(&self) -> CaeResult<()> {
        let p = &self.problem.operator;
        let mut free = vec![false; p.dimension()];
        for &j in &self.factor.free {
            free[j] = true;
        }
        for j in 0..free.len() {
            if (free[j] && self.state[j] <= 0.)
                || (!free[j]
                    && self.cone_multipliers[j]
                        <= p.policy.derivative_relative_dual_tolerance
                            * self.stationarity_scales[j])
            {
                return Err(fail(
                    "linear cone residual derivative requires a strict active stratum",
                ));
            }
        }
        Ok(())
    }
    pub fn jvp_rhs(
        &self,
        stationarity_rhs_direction: &[f64],
        constraint_rhs_direction: &[f64],
    ) -> CaeResult<Vec<f64>> {
        self.derivative_domain()?;
        let p = &self.problem.operator;
        p.check(stationarity_rhs_direction)?;
        let m = p.equality.len();
        if constraint_rhs_direction.len() != m
            || constraint_rhs_direction.iter().any(|v| !v.is_finite())
        {
            return Err(fail("linear cone constraint direction invalid"));
        }
        let z = self.factor.inverse.solve(
            &self
                .factor
                .free
                .iter()
                .map(|j| stationarity_rhs_direction[*j])
                .collect::<Vec<_>>(),
        )?;
        let residual = (0..m)
            .map(|i| {
                self.factor
                    .free
                    .iter()
                    .enumerate()
                    .map(|(k, j)| p.equality[i][*j] * z[k])
                    .sum::<f64>()
                    - constraint_rhs_direction[i]
            })
            .collect::<Vec<_>>();
        let lambda = self
            .factor
            .schur
            .solve(&residual, 1, false)
            .map_err(|e| fail(e.to_string()))?;
        let mut out = vec![0.; p.dimension()];
        for (k, &j) in self.factor.free.iter().enumerate() {
            out[j] = z[k]
                - (0..m)
                    .map(|i| self.factor.lift[i][k] * lambda[i])
                    .sum::<f64>();
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("nonfinite linear cone residual direction"));
        }
        Ok(out)
    }
    pub fn vjp_rhs(&self, bar: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        self.derivative_domain()?;
        let p = &self.problem.operator;
        p.check(bar)?;
        let m = p.equality.len();
        let z = self
            .factor
            .inverse
            .solve(&self.factor.free.iter().map(|j| bar[*j]).collect::<Vec<_>>())?;
        let residual = (0..m)
            .map(|i| {
                self.factor
                    .free
                    .iter()
                    .enumerate()
                    .map(|(k, j)| self.problem.lifting[i][*j] * z[k])
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let target = self
            .factor
            .schur
            .solve(&residual, 1, true)
            .map_err(|e| fail(e.to_string()))?;
        let adjustment = self.factor.inverse.solve(
            &self
                .factor
                .free
                .iter()
                .map(|j| (0..m).map(|i| p.equality[i][*j] * target[i]).sum::<f64>())
                .collect::<Vec<_>>(),
        )?;
        let mut out = vec![0.; p.dimension()];
        for (k, &j) in self.factor.free.iter().enumerate() {
            out[j] = z[k] - adjustment[k];
        }
        if out.iter().chain(&target).any(|v| !v.is_finite()) {
            return Err(fail("nonfinite linear cone residual cotangent"));
        }
        Ok((out, target))
    }
}
