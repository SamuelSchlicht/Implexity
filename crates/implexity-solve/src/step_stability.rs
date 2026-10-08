// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use serde_json::{Value, json};

use crate::complex_spectral::{ComplexMatrix, c64, exceptional_point_gate, identify_cluster};
use crate::periodic::arnoldi::{ArnoldiOptions, dominant_eigenpairs, real_eigen};
use crate::periodic::krylov::{BatchedGmresOptions, batched_gmres, dot, norm};
use crate::periodic::{Multiplier, residual_scale};
use crate::time_stepper::{StepParameters, TimeStepper};
use crate::trace;

pub const STEADY_STATE_CERTIFICATE_SCHEMA: &str = "implexity-steady-state-certificate/1";
pub const CLUSTER_RADIUS: f64 = 1e-6;
pub const MIN_SEPARATION: f64 = 1e-6;
pub const MODULUS_GAP: f64 = 1e-3;
pub const MAX_CONDITION: f64 = 1e8;
pub const BASE_STATE_ADJOINT_RTOL: f64 = 1e-12;
const MAX_NEWTON: usize = 50;
const KRYLOV_RESTART: usize = 40;
const KRYLOV_PRODUCTS: usize = 2000;
const ARNOLDI_RESTARTS: usize = 300;

#[derive(Clone, Debug)]
pub struct SteadyState {
    pub state: Vec<f64>,
    pub residual: f64,
    pub certificate: Value,
}

#[derive(Clone, Debug)]
pub struct StepMode {
    pub multiplier: Multiplier,
    pub growth_rate_per_s: f64,
    pub angular_frequency_rad_s: f64,
    pub right: (Vec<f64>, Vec<f64>),
    pub left: (Vec<f64>, Vec<f64>),
    pub condition: f64,
    pub separation: f64,
    pub gate: Value,
}

impl StepMode {
    #[must_use]
    pub fn value(&self) -> c64 {
        c64::new(self.multiplier.re, self.multiplier.im)
    }
}

fn params(design: &[f64]) -> StepParameters<'_> {
    StepParameters { design, time_scale: 1.0 }
}

fn check_design(stepper: &dyn TimeStepper, design: &[f64]) -> CaeResult<()> {
    if design.len() != stepper.design_size() || !design.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract(format!(
            "step stability design must be a finite vector of length {} (got {})",
            stepper.design_size(),
            design.len()
        )));
    }
    Ok(())
}

fn check_state(stepper: &dyn TimeStepper, state: &[f64], what: &str) -> CaeResult<()> {
    if state.len() != stepper.state_size() || !state.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract(format!(
            "step stability {what} must be a finite vector of length {}",
            stepper.state_size()
        )));
    }
    Ok(())
}

fn advance(stepper: &dyn TimeStepper, state: &[f64], design: &[f64]) -> CaeResult<Vec<f64>> {
    let record = stepper.advance(1, state, params(design))?;
    if record.state.len() != state.len() || !record.state.iter().all(|v| v.is_finite()) {
        return Err(CaeError::convergence("time step produced a non-finite or misshapen state"));
    }
    Ok(record.state)
}

fn tangent(
    stepper: &dyn TimeStepper,
    state: &[f64],
    next: &[f64],
    design: &[f64],
    v: &[f64],
) -> CaeResult<Vec<f64>> {
    Ok(stepper.tangent(1, state, next, params(design), v, None, 0.0)?.state)
}

fn adjoint_batch(
    stepper: &dyn TimeStepper,
    state: &[f64],
    next: &[f64],
    design: &[f64],
    bars: &[Vec<f64>],
) -> CaeResult<Vec<crate::time_stepper::StepCotangent>> {
    let samples = vec![vec![0.0; stepper.sample_names().len()]; bars.len()];
    let out = stepper.adjoint_many(1, state, next, params(design), bars, &samples)?;
    if out.len() != bars.len() {
        return Err(CaeError::contract(
            "time stepper adjoint_many returned a different number of cotangents",
        ));
    }
    Ok(out)
}



pub fn steady_state(
    stepper: &dyn TimeStepper,
    design: &[f64],
    guess: &[f64],
    tolerance: f64,
) -> CaeResult<SteadyState> {
    check_design(stepper, design)?;
    check_state(stepper, guess, "guess")?;
    if !(tolerance.is_finite() && tolerance > 0.0) {
        return Err(CaeError::contract("steady-state tolerance must be finite and positive"));
    }
    let n = guess.len();
    let mut z = guess.to_vec();
    let mut next = advance(stepper, &z, design)?;
    let mut g: Vec<f64> = next.iter().zip(&z).map(|(a, b)| a - b).collect();
    let mut residual = relative(&g, &z, &next);
    let mut previous_residual = f64::NAN;
    let mut krylov_products = 0usize;
    let mut steps = 1usize;
    let mut iteration = 0usize;
    while residual > tolerance {
        if iteration >= MAX_NEWTON {
            return Err(CaeError::convergence(format!(
                "steady state did not converge: relative step residual {residual:.3e} above {tolerance:.3e} after {MAX_NEWTON} Newton iterations"
            )));
        }
        iteration += 1;
        let eta = forcing_term(residual, previous_residual, tolerance);
        let options = BatchedGmresOptions {
            rtol: eta,
            atol: 0.0,
            restart: KRYLOV_RESTART,
            max_products: KRYLOV_PRODUCTS,
        };
        let (zc, nc) = (z.clone(), next.clone());
        let solve = batched_gmres(n, &[g.clone()], None, &options, None, |batch| {
            batch
                .iter()
                .map(|v| {
                    let lv = tangent(stepper, &zc, &nc, design, v)?;
                    Ok(v.iter().zip(&lv).map(|(a, b)| a - b).collect())
                })
                .collect()
        })?;
        krylov_products += solve.products[0];
        let delta = &solve.solutions[0];
        let mut alpha = 1.0;
        let mut accepted = false;
        for _ in 0..12 {
            let trial: Vec<f64> = z.iter().zip(delta).map(|(a, d)| a + alpha * d).collect();
            let trial_next = advance(stepper, &trial, design)?;
            steps += 1;
            let trial_g: Vec<f64> = trial_next.iter().zip(&trial).map(|(a, b)| a - b).collect();
            let trial_residual = relative(&trial_g, &trial, &trial_next);
            if norm(&trial_g) < norm(&g) || trial_residual <= tolerance {
                previous_residual = residual;
                z = trial;
                next = trial_next;
                g = trial_g;
                residual = trial_residual;
                accepted = true;
                break;
            }
            alpha *= 0.5;
        }
        if !accepted {
            return Err(CaeError::convergence(format!(
                "steady state Newton stalled: no residual decrease along the Newton direction at relative residual {residual:.3e}"
            )));
        }
        trace::point("newton_iteration", || {
            let mut f = trace::Fields::new();
            f.insert("stage".into(), json!("steady_state"));
            f.insert("newton_iteration".into(), json!(iteration));
            f.insert("residual_norm".into(), json!(residual));
            f.insert("iterations".into(), json!(solve.products[0]));
            f
        })?;
    }
    let certificate = json!({
        "schema": STEADY_STATE_CERTIFICATE_SCHEMA,
        "stepper": stepper.identity(),
        "state_size": n,
        "design_size": design.len(),
        "step_s": stepper.nominal_step_s(),
        "residual": residual,
        "tolerance": tolerance,
        "newton_iterations": iteration,
        "step_evaluations": steps,
        "krylov_products": krylov_products,
        "certified": true,
    });
    Ok(SteadyState { state: z, residual, certificate })
}

fn relative(g: &[f64], a: &[f64], b: &[f64]) -> f64 {
    norm(g) / residual_scale(a, b)
}

pub(crate) fn forcing_term(residual: f64, previous: f64, tolerance: f64) -> f64 {
    let base =
        if previous.is_finite() && previous > 0.0 { 0.9 * (residual / previous).powi(2) } else { 1e-2 };
    let floor = (0.1 * tolerance / residual.max(f64::MIN_POSITIVE)).max(1e-13);
    base.min(1e-2).max(floor.min(1e-2))
}




pub fn dominant_modes(
    stepper: &dyn TimeStepper,
    design: &[f64],
    steady: &SteadyState,
    wanted: usize,
    tolerance: f64,
) -> CaeResult<Vec<StepMode>> {
    check_design(stepper, design)?;
    check_state(stepper, &steady.state, "steady state")?;
    let n = steady.state.len();
    if wanted == 0 || wanted > n {
        return Err(CaeError::contract(format!("dominant_modes needs 1 ≤ wanted ≤ {n} (got {wanted})")));
    }
    let z = &steady.state;
    let next = advance(stepper, z, design)?;
    let options = ArnoldiOptions { wanted, basis: None, tolerance, max_restarts: ARNOLDI_RESTARTS };
    let right = dominant_eigenpairs(n, &options, None, |v| tangent(stepper, z, &next, design, v))?;
    let left = dominant_eigenpairs(n, &options, None, |v| {
        Ok(adjoint_batch(stepper, z, &next, design, &[v.to_vec()])?.remove(0).previous)
    })?;
    let cut = modulus_cut(&right.ritz_values, right.pairs.len(), n)?;
    let dt = stepper.nominal_step_s();
    let projected = ComplexMatrix::from_real(&right.projected);
    let (values, _) = real_eigen(&right.projected)?;
    let mut real_order: Vec<usize> = (0..values.len()).collect();
    real_order
        .sort_by(|&i, &j| values[i].re.total_cmp(&values[j].re).then(values[i].im.total_cmp(&values[j].im)));
    let mut modes = Vec::with_capacity(right.pairs.len());
    for pair in right.pairs.iter().take(cut) {
        let mu = pair.value;
        let chi = left
            .pairs
            .iter()
            .min_by(|a, b| (a.value - mu).norm().total_cmp(&(b.value - mu).norm()))
            .ok_or_else(|| CaeError::convergence("no left Ritz pair for a dominant mode"))?;
        let scale = mu.norm().max(1.0);
        if (chi.value - mu).norm() > 1e3 * tolerance * scale {
            return Err(CaeError::convergence(format!(
                "left and right Arnoldi runs disagree on the dominant eigenvalue ({:.6e}{:+.6e}i vs {:.6e}{:+.6e}i)",
                mu.re, mu.im, chi.value.re, chi.value.im
            )));
        }
        let (a, b) = (&chi.vector_re, &chi.vector_im);
        let (c, d) = (&pair.vector_re, &pair.vector_im);
        let overlap = c64::new(dot(a, c) - dot(b, d), dot(a, d) + dot(b, c));
        let condition = if overlap.norm() > 0.0 { 1.0 / overlap.norm() } else { f64::INFINITY };
        let separation = right
            .ritz_values
            .iter()
            .map(|v| (*v - mu).norm())
            .filter(|d| *d > 64.0 * f64::EPSILON * scale)
            .fold(f64::INFINITY, f64::min);
        let seed =
            real_order.iter().position(|&i| (values[i] - mu).norm() <= 1e3 * tolerance * scale).ok_or_else(
                || CaeError::convergence("dominant mode is not a Ritz value of the projected matrix"),
            )?;
        let (mut gate, cluster_size) = if projected.nrows == 1 {

            (
                json!({"admissible": true, "cluster_separation": Value::Null, "cluster_condition": 1.0, "reason": Value::Null}),
                1,
            )
        } else {
            let cluster = identify_cluster(&projected, seed, CLUSTER_RADIUS * scale)?;
            (exceptional_point_gate(&cluster, MIN_SEPARATION * scale, MAX_CONDITION), cluster.indices.len())
        };
        let admissible = gate["admissible"].as_bool().unwrap_or(false)
            && cluster_size == 1
            && condition < MAX_CONDITION
            && separation > MIN_SEPARATION * scale;
        gate["admissible"] = json!(admissible);
        gate["cluster_size"] = json!(cluster_size);
        gate["eigenvalue_condition"] = if condition.is_finite() { json!(condition) } else { Value::Null };
        if !admissible && gate["reason"].is_null() {
            gate["reason"] = json!(
                "eigenvalue is clustered or ill-conditioned; a simple-eigenvalue gradient is not admissible"
            );
        }
        modes.push(StepMode {
            multiplier: Multiplier { re: mu.re, im: mu.im },
            growth_rate_per_s: mu.norm().ln() / dt,
            angular_frequency_rad_s: mu.im.atan2(mu.re) / dt,
            right: (c.clone(), d.clone()),
            left: (a.clone(), b.iter().map(|v| -v).collect()),
            condition,
            separation,
            gate,
        });
    }
    Ok(modes)
}

fn modulus_cut(values: &[c64], wanted: usize, order: usize) -> CaeResult<usize> {
    if wanted >= values.len() && values.len() == order {
        return Ok(wanted);
    }
    (1..=wanted.min(values.len().saturating_sub(1)))
        .rev()
        .find(|&c| {
            let (a, b) = (values[c - 1].norm(), values[c].norm());
            a - b >= MODULUS_GAP * a.max(1.0)
        })
        .ok_or_else(|| {
            CaeError::convergence(format!(
                "dominant_modes: no modulus gap of {MODULUS_GAP:.0e} (relative) separates the {wanted} dominant Ritz values from the rest; ask for more modes"
            ))
        })
}



pub fn growth_rate_gradient(
    stepper: &dyn TimeStepper,
    design: &[f64],
    steady: &SteadyState,
    mode: &StepMode,
) -> CaeResult<Vec<f64>> {
    check_design(stepper, design)?;
    check_state(stepper, &steady.state, "steady state")?;
    let Some(second) = stepper.second_order() else {
        return Err(CaeError::contract(
            "growth-rate gradients need the SecondOrderStepper capability of the time stepper (directional second derivatives)",
        ));
    };
    if !mode.gate["admissible"].as_bool().unwrap_or(false) {
        return Err(CaeError::contract(format!(
            "growth-rate gradient refused: {}",
            mode.gate["reason"].as_str().unwrap_or("mode failed the cluster/exceptional-point gate")
        )));
    }
    let n = steady.state.len();
    let (c, d) = (&mode.right.0, &mode.right.1);
    let (a, b): (Vec<f64>, Vec<f64>) = (mode.left.0.clone(), mode.left.1.iter().map(|v| -v).collect());
    if [c, d, &a, &b].iter().any(|v| v.len() != n) {
        return Err(CaeError::contract("mode eigenvectors do not match the state size"));
    }
    let z = &steady.state;
    let next = advance(stepper, z, design)?;
    let zero_samples = vec![0.0; stepper.sample_names().len()];
    let second_term =
        |u: &[f64], v: &[f64]| second.adjoint_tangent(1, z, &next, params(design), u, &zero_samples, v, None);
    let ac = second_term(&a, c)?;
    let bd = second_term(&b, d)?;
    let ad = second_term(&a, d)?;
    let bc = second_term(&b, c)?;
    let m = design.len();
    for t in [&ac, &bd, &ad, &bc] {
        if t.previous.len() != n || t.design.len() != m {
            return Err(CaeError::contract("SecondOrderStepper returned cotangents of the wrong size"));
        }
    }
    let p_re: Vec<f64> = (0..n).map(|i| ac.previous[i] - bd.previous[i]).collect();
    let p_im: Vec<f64> = (0..n).map(|i| ad.previous[i] + bc.previous[i]).collect();
    let mut num_re: Vec<f64> = (0..m).map(|i| ac.design[i] - bd.design[i]).collect();
    let mut num_im: Vec<f64> = (0..m).map(|i| ad.design[i] + bc.design[i]).collect();
    if norm(&p_re) > 0.0 || norm(&p_im) > 0.0 {
        let options = BatchedGmresOptions {
            rtol: BASE_STATE_ADJOINT_RTOL,
            atol: 0.0,
            restart: KRYLOV_RESTART,
            max_products: KRYLOV_PRODUCTS,
        };
        let solve = batched_gmres(n, &[p_re, p_im], None, &options, None, |batch| {
            let out = adjoint_batch(stepper, z, &next, design, batch)?;
            Ok(batch
                .iter()
                .zip(out)
                .map(|(w, c)| w.iter().zip(&c.previous).map(|(x, y)| x - y).collect())
                .collect())
        })?;
        if !solve.all_converged() {
            return Err(CaeError::convergence(format!(
                "growth-rate base-state adjoint not certified (residual/tolerance {:.3e})",
                solve.worst_ratio()
            )));
        }
        let pulled = adjoint_batch(stepper, z, &next, design, &solve.solutions)?;
        for i in 0..m {
            num_re[i] += pulled[0].design[i];
            num_im[i] += pulled[1].design[i];
        }
    }
    let overlap = c64::new(dot(&a, c) - dot(&b, d), dot(&a, d) + dot(&b, c));
    let mu = mode.value();
    let dt = stepper.nominal_step_s();
    let factor = mu.conj() / (overlap * mu.norm_sqr() * dt);
    Ok((0..m).map(|i| (c64::new(num_re[i], num_im[i]) * factor).re).collect())
}
