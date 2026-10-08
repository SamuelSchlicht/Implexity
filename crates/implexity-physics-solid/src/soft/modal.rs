// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::{Jet3, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::lu::SparseLu;
use implexity_linalg::sparse::{CscMatrix, CsrMatrix};
use implexity_solve::spectral::generalized_eigh;

use super::model::SoftModel;
use super::stepper::{Loading, NewtonOptions, Scheme, SoftHistory};
use crate::util::contract;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalOptions {
    pub modes: usize,
    pub guard_vectors: usize,
    pub tolerance: f64,
    pub max_iterations: usize,
}

impl Default for ModalOptions {
    fn default() -> Self {
        Self { modes: 4, guard_vectors: 6, tolerance: 1e-8, max_iterations: 300 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Modes {
    pub eigenvalues: Vec<f64>,
    pub frequencies_hz: Vec<f64>,
    pub vectors: Vec<Vec<f64>>,
    pub residuals: Vec<f64>,
    pub iterations: usize,
}

pub struct ModalAnalysis<'m> {
    model: &'m SoftModel,
    params: Vec<f64>,
    u: Vec<f64>,
    p: Vec<f64>,
    jacobian: CscMatrix,
    lu: SparseLu,
    mass: CsrMatrix,
    loaded: bool,
}

impl std::fmt::Debug for ModalAnalysis<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModalAnalysis")
            .field("unknowns", &self.model.n_unknowns)
            .field("loaded", &self.loaded)
            .finish()
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn check_model(model: &SoftModel, params: &[f64]) -> CaeResult<()> {
    if model.materials.iter().any(|m| m.prony.is_some()) {
        return contract(
            "modal analysis takes elastic laws: evaluate a viscoelastic law at its storage modulus \
             (Prony::storage_ratio) instead of its incremental step stiffness",
        );
    }
    if params.len() < 2 * model.ne() || params.iter().any(|v| !v.is_finite()) {
        return contract("modal analysis: (rho, theta) are required per element");
    }
    Ok(())
}

impl<'m> ModalAnalysis<'m> {

    pub fn at_rest(model: &'m SoftModel, params: &[f64]) -> CaeResult<Self> {
        check_model(model, params)?;
        let n3 = 3 * model.n();
        let loading = Loading {
            times: vec![0.0, 1.0],
            prescribed: vec![0.0; n3],
            displacement_amplitude: vec![0.0],
            force: vec![0.0; n3],
            force_amplitude: vec![0.0],
            pressure: 0.0,
            pressure_amplitude: vec![0.0],
            initial_velocity: vec![0.0; n3],
        };
        let history = SoftHistory::new(
            model,
            Scheme::Quasistatic,
            loading,
            (0.0, 0.0),
            NewtonOptions::default(),
            params.to_vec(),
        )?;
        let x0 = history.initial_state()?;
        let y0 = vec![0.0; model.n_unknowns];
        let (_, _, jacobian) = history.step_system(0, &x0, &y0, None, true)?;
        let np = if model.formulation.mixed() { model.n() } else { 0 };
        Self::finish(model, params, vec![0.0; n3], vec![0.0; np], jacobian, false)
    }


    pub fn prestressed(
        model: &'m SoftModel,
        params: &[f64],
        loading: Loading,
        newton: NewtonOptions,
    ) -> CaeResult<Self> {
        check_model(model, params)?;
        if loading.pressure != 0.0 && loading.pressure_amplitude.iter().any(|a| *a != 0.0) {
            return contract(
                "modal analysis of a prestressed state takes dead loads and prescribed displacements; a follower \
                 pressure has a non-symmetric load stiffness",
            );
        }
        let steps = loading.steps();
        let history =
            SoftHistory::new(model, Scheme::Quasistatic, loading, (0.0, 0.0), newton, params.to_vec())?;
        let mut x = history.initial_state()?;
        let mut prev = x.clone();
        let mut y = vec![0.0; model.n_unknowns];
        for t in 0..steps {
            let s = history.solve_step(t, &x, None, Some(&y))?;
            prev.clone_from(&x);
            x.clone_from(&s.state);
            y.clone_from(&s.unknowns);
        }
        let (_, _, jacobian) = history.step_system(steps - 1, &prev, &y, None, true)?;
        let n3 = 3 * model.n();
        let np = if model.formulation.mixed() { model.n() } else { 0 };
        let u = x[..n3].to_vec();
        let p = x[4 * n3..4 * n3 + np].to_vec();
        Self::finish(model, params, u, p, jacobian, true)
    }

    fn finish(
        model: &'m SoftModel,
        params: &[f64],
        u: Vec<f64>,
        p: Vec<f64>,
        jacobian: Option<CscMatrix>,
        loaded: bool,
    ) -> CaeResult<Self> {
        let Some(jacobian) = jacobian else {
            return contract("internal: the step system returned no Jacobian");
        };
        let lu = model.factor(&jacobian)?;
        let ne = model.ne();
        let mass_factor: Vec<f64> = params[..ne].iter().map(|r| model.interpolation.mass(*r)).collect();
        let stiffness_factor: Vec<f64> =
            params[..ne].iter().map(|r| model.interpolation.stiffness(*r)).collect();
        let (mass, _) = model.global_mass_and_reference(&mass_factor, &stiffness_factor)?;
        Ok(Self { model, params: params.to_vec(), u, p, jacobian, lu, mass, loaded })
    }

    #[must_use]
    pub fn displacement(&self) -> &[f64] {
        &self.u
    }

    #[must_use]
    pub fn displacement_of(&self, y: &[f64]) -> Vec<f64> {
        let n3 = 3 * self.model.n();
        (0..n3)
            .map(|i| if self.model.unknown[i] == usize::MAX { 0.0 } else { y[self.model.unknown[i]] })
            .collect()
    }

    fn mass_mul(&self, y: &[f64]) -> CaeResult<Vec<f64>> {
        let m = self.model;
        let n3 = 3 * m.n();
        let u = self.displacement_of(y);
        let mu = self.mass.matvec(&u).map_err(|e| CaeError::contract(e.to_string()))?;
        let mut out = vec![0.0; m.n_unknowns];
        for i in 0..n3 {
            if m.unknown[i] != usize::MAX {
                out[m.unknown[i]] = mu[i];
            }
        }
        Ok(out)
    }

    fn a_mul(&self, y: &[f64]) -> CaeResult<Vec<f64>> {
        self.jacobian.matvec(y).map_err(|e| CaeError::contract(e.to_string()))
    }

    fn solve(&self, b: &mut [f64]) -> CaeResult<()> {
        self.lu.solve_in_place(b).map_err(|e| CaeError::convergence(format!("modal solve: {e}")))
    }


    #[allow(clippy::too_many_lines)]
    pub fn modes(&self, options: &ModalOptions) -> CaeResult<Modes> {
        let m = self.model;
        if options.modes == 0 || options.guard_vectors == 0 {
            return contract("modal analysis needs at least one mode and one guard vector");
        }
        let nu = m.n_unknowns;
        let p = options.modes + options.guard_vectors;
        if p > m.n_free_u {
            return contract("the model has fewer free displacement unknowns than requested modes");
        }
        let n3 = 3 * m.n();

        let mut basis: Vec<Vec<f64>> = (0..p)
            .map(|j| {
                let mut y = vec![0.0; nu];
                for i in 0..n3 {
                    if m.unknown[i] != usize::MAX {
                        let x = m.mesh.points[i / 3];
                        let s = (j + 1) as f64;
                        y[m.unknown[i]] = (s * 311.0 * x[0] + (i % 3) as f64 + s).sin()
                            + (s * 173.0 * x[1] + 0.5 * s).cos() * (1.0 + (i % 3) as f64)
                            + (s * 97.0 * x[2] + 0.25 * s).sin();
                    }
                }
                y
            })
            .collect();
        let mut eigenvalues: Vec<f64> = Vec::new();
        let mut previous: Vec<f64> = Vec::new();
        let mut iterations = 0;
        let mut stalled = 0_usize;
        let mut best_residual = f64::INFINITY;
        while iterations < options.max_iterations {
            iterations += 1;
            let mut next = Vec::with_capacity(p);
            for y in &basis {
                let mut rhs = self.mass_mul(y)?;
                self.solve(&mut rhs)?;
                next.push(rhs);
            }
            let ax: Vec<Vec<f64>> = next.iter().map(|y| self.a_mul(y)).collect::<CaeResult<_>>()?;
            let mx: Vec<Vec<f64>> = next.iter().map(|y| self.mass_mul(y)).collect::<CaeResult<_>>()?;
            let mut ar = DenseMatrix::zeros(p, p);
            let mut mr = DenseMatrix::zeros(p, p);
            for a in 0..p {
                for b in 0..p {
                    ar.data[a * p + b] = 0.5 * (dot(&next[a], &ax[b]) + dot(&next[b], &ax[a]));
                    mr.data[a * p + b] = 0.5 * (dot(&next[a], &mx[b]) + dot(&next[b], &mx[a]));
                }
            }
            let ritz = generalized_eigh(&ar, &mr, None, f64::NEG_INFINITY)?;
            basis = (0..p)
                .map(|c| {
                    let mut y = vec![0.0; nu];
                    for (r, v) in next.iter().enumerate() {
                        let w = ritz.eigenvectors.data[r * ritz.eigenvectors.ncols + c];
                        y.iter_mut().zip(v).for_each(|(o, x)| *o += w * x);
                    }
                    y
                })
                .collect();
            eigenvalues.clone_from(&ritz.eigenvalues);
            if eigenvalues[..options.modes].iter().any(|l| !(l.is_finite() && *l > 0.0)) {
                return Err(CaeError::convergence(
                    "modal analysis: non-positive eigenvalue (unsupported rigid motion or a buckled state)",
                ));
            }
            let mut worst = 0.0_f64;
            for (i, y) in basis.iter().take(options.modes).enumerate() {
                let ay = self.a_mul(y)?;
                let my = self.mass_mul(y)?;
                let r: f64 =
                    ay.iter().zip(&my).map(|(a, b)| (a - eigenvalues[i] * b).powi(2)).sum::<f64>().sqrt();
                worst = worst.max(r / dot(&ay, &ay).sqrt());
            }

            let fixed_values = previous.len() == eigenvalues.len()
                && (0..options.modes).all(|i| (eigenvalues[i] - previous[i]).abs() <= 1e-13 * eigenvalues[i]);
            stalled = if fixed_values && worst > 0.5 * best_residual { stalled + 1 } else { 0 };
            best_residual = best_residual.min(worst);
            if worst <= options.tolerance || stalled >= 3 {
                break;
            }
            previous.clone_from(&eigenvalues);
            if iterations == options.max_iterations {
                return Err(CaeError::convergence(format!(
                    "modal subspace iteration did not converge in {} iterations (relative residual {worst:.2e})",
                    options.max_iterations
                )));
            }
        }
        let mut out = Modes {
            eigenvalues: Vec::with_capacity(options.modes),
            frequencies_hz: Vec::with_capacity(options.modes),
            vectors: Vec::with_capacity(options.modes),
            residuals: Vec::with_capacity(options.modes),
            iterations,
        };
        for (i, y) in basis.iter().take(options.modes).enumerate() {
            let lambda = eigenvalues[i];
            let ay = self.a_mul(y)?;
            let my = self.mass_mul(y)?;
            let r: f64 = ay.iter().zip(&my).map(|(a, b)| (a - lambda * b).powi(2)).sum::<f64>().sqrt();
            let norm = dot(y, &my).sqrt();
            out.eigenvalues.push(lambda);
            out.frequencies_hz.push(lambda.sqrt() / (2.0 * std::f64::consts::PI));
            out.residuals.push(r / dot(&ay, &ay).sqrt());
            out.vectors.push(y.iter().map(|v| v / norm).collect());
        }
        Ok(out)
    }

    fn local(&self, e: usize, y: &[f64]) -> [f64; 16] {
        let m = self.model;
        let dofs = m.element_dofs(e);
        let nl = m.nl();
        core::array::from_fn(|k| {
            if k >= nl {
                return 0.0;
            }
            let i = m.unknown[dofs[k]];
            if i == usize::MAX { 0.0 } else { y[i] }
        })
    }

    fn element_jet(&self, e: usize, a: &[f64; 17], b: &[f64; 17]) -> CaeResult<Jet3<17>> {
        let m = self.model;
        let d0 = m.gather(e, &self.u, &self.p);
        let mut values = [0.0; 17];
        values[..16].copy_from_slice(&d0);
        values[16] = 1.0;
        let jet = Jet3::<17>::seed(&values, a, b);
        let d: [Jet3<17>; 16] = core::array::from_fn(|k| jet[k]);
        let ne = m.ne();
        let rho = Jet3::<17>::from_f64(self.params[e].clamp(0.0, 1.0));
        let theta = Jet3::<17>::from_f64(self.params[ne + e]);
        let h = [Jet3::<17>::zero(); 6];
        let value = m.element_energy_scaled(e, &d, rho, theta, &h, 1.0, jet[16])?;
        if !value.third_is_finite() {
            return contract(format!(
                "modal sensitivity: the law of element {e} provides no third derivatives (see implexity_ad::Jet3)"
            ));
        }
        Ok(value)
    }


    pub fn stiffness_scale_sensitivities(
        &self,
        modes: &Modes,
        groups: &[Vec<usize>],
    ) -> CaeResult<Vec<Vec<f64>>> {
        let m = self.model;
        let ne = m.ne();
        if groups.iter().flatten().any(|e| *e >= ne) {
            return contract("modal sensitivity: element group index out of range");
        }
        let nl = m.nl();
        let mut group_of = vec![usize::MAX; ne];
        for (g, list) in groups.iter().enumerate() {
            for &e in list {
                group_of[e] = g;
            }
        }
        let mut out = Vec::with_capacity(modes.vectors.len());
        for phi in &modes.vectors {
            let mut explicit = vec![0.0; groups.len()];
            let mut c = vec![0.0; m.n_unknowns];
            for e in 0..ne {
                let in_group = group_of[e] != usize::MAX;
                if !in_group && !self.loaded {
                    continue;
                }
                let local = self.local(e, phi);
                let mut a = [0.0; 17];
                a[..16].copy_from_slice(&local);
                let jet = self.element_jet(e, &a, &a)?;
                if in_group {
                    explicit[group_of[e]] += jet.third()[16];
                }
                if self.loaded {
                    let dofs = m.element_dofs(e);
                    for k in 0..nl {
                        let i = m.unknown[dofs[k]];
                        if i != usize::MAX {
                            c[i] += jet.third()[k];
                        }
                    }
                }
            }
            if self.loaded {

                let mut z = c;
                self.solve(&mut z)?;
                let mut b = [0.0; 17];
                b[16] = 1.0;
                for (e, &g) in group_of.iter().enumerate() {
                    if g == usize::MAX {
                        continue;
                    }
                    let local = self.local(e, &z);
                    let mut a = [0.0; 17];
                    a[..16].copy_from_slice(&local);
                    let jet = self.element_jet(e, &a, &b)?;
                    explicit[g] -= jet.d12();
                }
            }
            out.push(explicit);
        }
        Ok(out)
    }
}

#[must_use]
pub fn frequency_derivative(eigenvalue: f64, d_eigenvalue: f64) -> f64 {
    d_eigenvalue / (4.0 * std::f64::consts::PI * eigenvalue.sqrt())
}
