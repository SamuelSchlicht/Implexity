// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use serde_json::{Value, json};

use super::arnoldi::deterministic_vector;
use super::krylov::norm;
use super::nonperiodic;
use crate::checkpointed_history::{CheckpointPolicy, HistoryRun, history_adjoint_many, run_history};
use crate::state_store::StoreBudget;
use crate::time_stepper::{StepParameters, TimeStepper};
use crate::trace;

pub const DISCRETE_HISTORY_EXACT: &str = "discrete_history_exact";
pub const BIASED_ESTIMATE: &str = "biased_estimate";
pub const FIXED_HORIZON_CERTIFICATE_SCHEMA: &str = "implexity-fixed-horizon-gradient-certificate/1";
pub const ENSEMBLE_CERTIFICATE_SCHEMA: &str = "implexity-ensemble-window-gradient-certificate/1";
pub const DEFAULT_MAX_ADJOINT_GROWTH: f64 = 1.1;

pub type HorizonResponses<'a> = &'a dyn Fn(&DenseMatrix) -> CaeResult<Vec<(f64, DenseMatrix)>>;

#[derive(Clone, Debug)]
pub struct FixedHorizonOptions {
    pub periods: usize,
    pub steps_per_period: usize,
    pub time_scale: f64,
    pub max_adjoint_growth: f64,
    pub window_check_tolerance: Option<f64>,
    pub checkpoint: CheckpointPolicy,
    pub budget: StoreBudget,
}

#[derive(Clone, Debug)]
pub struct FixedHorizonGradient {
    pub values: Vec<f64>,
    pub design: Vec<Vec<f64>>,
    pub initial_state: Vec<Vec<f64>>,
    pub time_scale: Vec<f64>,
    pub samples: DenseMatrix,
    pub final_state: Vec<f64>,
    pub adjoint_growth_per_period: f64,
    pub adjoint_norms: Vec<Vec<f64>>,
    pub window_check: Option<Vec<f64>>,
    pub certificate: Value,
}

#[derive(Clone, Debug)]
pub struct EnsembleOptions {
    pub members: usize,
    pub window_periods: usize,
    pub spacing_periods: usize,
    pub spin_up_periods: usize,
    pub steps_per_period: usize,
    pub time_scale: f64,
    pub checkpoint: CheckpointPolicy,
    pub budget: StoreBudget,
}

#[derive(Clone, Debug)]
pub struct EnsembleGradient {
    pub values: Vec<f64>,
    pub design: Vec<Vec<f64>>,
    pub member_values: Vec<Vec<f64>>,
    pub standard_error: Vec<f64>,
    pub certificate: Value,
}

struct Horizon<'a> {
    stepper: &'a dyn TimeStepper,
    design: &'a [f64],
    time_scale: f64,
    steps: usize,
    periods: usize,
    first_period: usize,
    checkpoint: &'a CheckpointPolicy,
    budget: StoreBudget,
}

struct Forward {
    boundaries: Vec<Vec<f64>>,
    samples: DenseMatrix,
    last_run: HistoryRun,
    runs: usize,
}

struct Backward {
    initial: Vec<Vec<f64>>,
    design: Vec<Vec<f64>>,
    time_scale: Vec<f64>,
    norms: Vec<Vec<f64>>,
    growth: f64,
    sweeps: usize,
    recomputations: usize,
}

impl Horizon<'_> {
    fn params(&self) -> StepParameters<'_> {
        StepParameters { design: self.design, time_scale: self.time_scale }
    }

    fn period_run(&self, k: usize, start: &[f64]) -> CaeResult<HistoryRun> {
        let first_step = (self.first_period + k) * self.steps + 1;
        let run = run_history(
            self.stepper,
            self.params(),
            start,
            first_step,
            Some(self.steps),
            None,
            self.checkpoint.clone(),
            &self.budget,
        )?;
        if !run.final_state().iter().all(|v| v.is_finite()) {
            return Err(nonperiodic("the trajectory diverged (non-finite state) within the horizon"));
        }
        Ok(run)
    }

    fn forward(&self, z0: &[f64]) -> CaeResult<Forward> {
        let n_s = self.stepper.sample_names().len();
        let mut samples = DenseMatrix::zeros(self.periods * self.steps, n_s);
        let mut boundaries = Vec::with_capacity(self.periods + 1);
        boundaries.push(z0.to_vec());
        let mut last = None;
        for k in 0..self.periods {
            let run = self.period_run(k, &boundaries[k])?;
            let block = run.samples();
            if block.nrows != self.steps || block.ncols != n_s {
                return Err(CaeError::contract("period run returned samples of the wrong shape"));
            }
            let offset = k * self.steps * n_s;
            samples.data[offset..offset + block.data.len()].copy_from_slice(&block.data);
            boundaries.push(run.final_state().to_vec());
            last = Some(run);
        }
        let last_run = last.ok_or_else(|| CaeError::contract("a horizon needs at least one period"))?;
        Ok(Forward { boundaries, samples, last_run, runs: self.periods })
    }

    fn backward(
        &self,
        forward: &Forward,
        periods: usize,
        bars: &[DenseMatrix],
        homogeneous: bool,
    ) -> CaeResult<Backward> {
        let n = self.stepper.state_size();
        let n_s = self.stepper.sample_names().len();
        let m = self.design.len();
        let r = bars.len();
        let mut lambda = vec![vec![0.0; n]; r];
        let mut design = vec![vec![0.0; m]; r];
        let mut time_scale = vec![0.0; r];
        let mut norms: Vec<Vec<f64>> = vec![vec![0.0]; r];
        let mut h: Vec<f64> = {
            let v = deterministic_vector(n, 7);
            let s = norm(&v);
            v.into_iter().map(|x| x / s).collect()
        };
        let mut factors = Vec::with_capacity(periods);
        let mut sweeps = 0usize;
        let mut recomputations = 0usize;
        for k in (0..periods).rev() {
            let recomputed;
            let run = if k + 1 == self.periods {
                &forward.last_run
            } else {
                recomputed = self.period_run(k, &forward.boundaries[k])?;
                recomputations += 1;
                if recomputed.final_state() != forward.boundaries[k + 1].as_slice() {
                    return Err(CaeError::convergence(
                        "checkpoint recomputation diverged from the forward sweep",
                    ));
                }
                &recomputed
            };
            let mut sample_bars = Vec::with_capacity(r + 1);
            for bar in bars {
                let offset = k * self.steps * n_s;
                let data = bar.data[offset..offset + self.steps * n_s].to_vec();
                sample_bars.push(
                    DenseMatrix::new(self.steps, n_s, data)
                        .map_err(|e| CaeError::contract(format!("sample cotangent block: {e}")))?,
                );
            }
            let mut finals = lambda.clone();
            if homogeneous {
                sample_bars.push(DenseMatrix::zeros(self.steps, n_s));
                finals.push(h.clone());
            }
            let out = history_adjoint_many(self.stepper, self.params(), run, &sample_bars, &finals)?;
            sweeps += 1;
            if out.len() != finals.len() {
                return Err(CaeError::contract(
                    "history_adjoint_many returned a different number of gradients",
                ));
            }
            for (i, g) in out.into_iter().enumerate() {
                if i < r {
                    for (a, b) in design[i].iter_mut().zip(&g.design) {
                        *a += b;
                    }
                    time_scale[i] += g.time_scale;
                    norms[i].push(norm(&g.initial_state));
                    lambda[i] = g.initial_state;
                } else {
                    let size = norm(&g.initial_state);
                    factors.push(size);
                    h = if size > 0.0 && size.is_finite() {
                        g.initial_state.into_iter().map(|x| x / size).collect()
                    } else {
                        let v = deterministic_vector(n, 11 + k);
                        let s = norm(&v);
                        v.into_iter().map(|x| x / s).collect()
                    };
                }
            }
        }

        let skip = factors.len().div_ceil(4).min(factors.len().saturating_sub(1));
        let tail = &factors[skip..];
        let growth = if tail.is_empty() {
            0.0
        } else if tail.iter().any(|f| !f.is_finite()) {
            f64::INFINITY
        } else if tail.contains(&0.0) {
            0.0
        } else {
            (tail.iter().map(|f| f.ln()).sum::<f64>() / tail.len() as f64).exp()
        };
        Ok(Backward { initial: lambda, design, time_scale, norms, growth, sweeps, recomputations })
    }
}

fn evaluate_responses(
    responses: HorizonResponses<'_>,
    samples: &DenseMatrix,
) -> CaeResult<(Vec<f64>, Vec<DenseMatrix>)> {
    let out = responses(samples)?;
    let mut values = Vec::with_capacity(out.len());
    let mut bars = Vec::with_capacity(out.len());
    for (r, (v, bar)) in out.into_iter().enumerate() {
        if bar.nrows != samples.nrows
            || bar.ncols != samples.ncols
            || !bar.data.iter().all(|x| x.is_finite())
            || !v.is_finite()
        {
            return Err(CaeError::contract(format!(
                "horizon response {r} must return a finite value and a finite {} × {} sample cotangent",
                samples.nrows, samples.ncols
            )));
        }
        values.push(v);
        bars.push(bar);
    }
    Ok((values, bars))
}

fn check_common(stepper: &dyn TimeStepper, design: &[f64], steps: usize, time_scale: f64) -> CaeResult<()> {
    if design.len() != stepper.design_size() || !design.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract(format!(
            "horizon design must be a finite vector of length {}",
            stepper.design_size()
        )));
    }
    if steps == 0 {
        return Err(CaeError::contract("steps_per_period must be positive"));
    }
    if !(time_scale.is_finite() && time_scale > 0.0) {
        return Err(CaeError::contract("time scale must be finite and positive"));
    }
    Ok(())
}

fn start_state(stepper: &dyn TimeStepper, design: &[f64], initial: Option<&[f64]>) -> CaeResult<Vec<f64>> {
    let z = match initial {
        Some(z) => z.to_vec(),
        None => stepper.initial_state(design)?,
    };
    if z.len() != stepper.state_size() || !z.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract(format!(
            "horizon initial state must be a finite vector of length {}",
            stepper.state_size()
        )));
    }
    Ok(z)
}

fn boundary_budget(budget: &StoreBudget, boundaries: usize, n: usize) -> CaeResult<StoreBudget> {
    let bytes = (boundaries as u64).saturating_mul(n as u64).saturating_mul(8);
    if bytes > budget.ram_bytes {
        return Err(CaeError::contract(format!(
            "the {boundaries} period-boundary states ({bytes} bytes) exceed the RAM snapshot budget of {} bytes",
            budget.ram_bytes
        )));
    }
    let mut inner = budget.clone();
    inner.ram_bytes -= bytes;
    Ok(inner)
}




pub fn fixed_horizon_gradient(
    stepper: &dyn TimeStepper,
    design: &[f64],
    initial: Option<&[f64]>,
    responses: HorizonResponses<'_>,
    options: &FixedHorizonOptions,
) -> CaeResult<FixedHorizonGradient> {
    check_common(stepper, design, options.steps_per_period, options.time_scale)?;
    if options.periods == 0 {
        return Err(CaeError::contract("a fixed horizon needs at least one period"));
    }
    if !(options.max_adjoint_growth.is_finite() && options.max_adjoint_growth >= 1.0) {
        return Err(CaeError::contract(
            "max_adjoint_growth must be finite and at least 1 (the neutral direction of a limit cycle)",
        ));
    }
    if let Some(t) = options.window_check_tolerance {
        if !(t.is_finite() && t > 0.0) {
            return Err(CaeError::contract("window check tolerance must be finite and positive"));
        }
        if options.periods < 2 || !options.periods.is_multiple_of(2) {
            return Err(CaeError::contract("the window check needs an even number of periods (K and K/2)"));
        }
    }
    let n = stepper.state_size();
    let z0 = start_state(stepper, design, initial)?;
    let horizon = Horizon {
        stepper,
        design,
        time_scale: options.time_scale,
        steps: options.steps_per_period,
        periods: options.periods,
        first_period: 0,
        checkpoint: &options.checkpoint,
        budget: boundary_budget(&options.budget, options.periods + 1, n)?,
    };
    let forward = horizon.forward(&z0)?;
    let (values, bars) = evaluate_responses(responses, &forward.samples)?;
    let mut backward = horizon.backward(&forward, options.periods, &bars, true)?;
    let growth = backward.growth;
    if growth > options.max_adjoint_growth {
        return Err(nonperiodic(format!(
            "the adjoint grows by a factor {growth:.4e} per period (limit {:.4e}): the finite-horizon adjoint of a chaotic or unstable regime has no authoritative gradient (§4.8)",
            options.max_adjoint_growth
        )));
    }
    if initial.is_none() {
        add_initial_state_terms(stepper, design, &backward.initial, &mut backward.design)?;
    }
    let window_check = match options.window_check_tolerance {
        Some(tolerance) => {
            Some(window_check(&horizon, &forward, responses, initial.is_none(), &backward.design, tolerance)?)
        }
        None => None,
    };
    let certificate = json!({
        "schema": FIXED_HORIZON_CERTIFICATE_SCHEMA,
        "stepper": stepper.identity(),
        "derivative_scope": DISCRETE_HISTORY_EXACT,
        "periods": options.periods,
        "steps_per_period": options.steps_per_period,
        "time_scale": options.time_scale,
        "responses": values.len(),
        "adjoint_growth_per_period": growth,
        "max_adjoint_growth": options.max_adjoint_growth,
        "window_check": window_check,
        "window_check_tolerance": options.window_check_tolerance,
        "forward_periods": forward.runs,
        "recomputed_periods": backward.recomputations,
        "sweeps": backward.sweeps,
        "initial_state_from_design": initial.is_none(),
        "regime": "periodic_or_steady",
    });
    trace::point("history_adjoint", || {
        let mut f = trace::Fields::new();
        f.insert("stage".into(), json!("fixed_horizon_adjoint"));
        f.insert("steps".into(), json!(options.periods * options.steps_per_period));
        f.insert("iterations".into(), json!(backward.sweeps));
        f
    })?;
    Ok(FixedHorizonGradient {
        values,
        design: backward.design,
        initial_state: backward.initial,
        time_scale: backward.time_scale,
        final_state: forward.boundaries[options.periods].clone(),
        samples: forward.samples,
        adjoint_growth_per_period: growth,
        adjoint_norms: backward.norms,
        window_check,
        certificate,
    })
}

fn window_check(
    horizon: &Horizon<'_>,
    forward: &Forward,
    responses: HorizonResponses<'_>,
    initial_from_design: bool,
    full: &[Vec<f64>],
    tolerance: f64,
) -> CaeResult<Vec<f64>> {
    let half = horizon.periods / 2;
    let rows = half * horizon.steps;
    let n_s = forward.samples.ncols;
    let head = DenseMatrix::new(rows, n_s, forward.samples.data[..rows * n_s].to_vec())
        .map_err(|e| CaeError::contract(format!("half-horizon samples: {e}")))?;
    let (_, head_bars) = evaluate_responses(responses, &head)?;

    let embedded: Vec<DenseMatrix> = head_bars
        .into_iter()
        .map(|b| {
            let mut data = b.data;
            data.resize(forward.samples.data.len(), 0.0);
            DenseMatrix { nrows: forward.samples.nrows, ncols: n_s, data }
        })
        .collect();
    let mut half_pass = horizon.backward(forward, half, &embedded, false)?;
    if initial_from_design {
        add_initial_state_terms(horizon.stepper, horizon.design, &half_pass.initial, &mut half_pass.design)?;
    }
    let mut differences = Vec::with_capacity(full.len());
    for (g, h) in full.iter().zip(&half_pass.design) {
        let diff: f64 = g.iter().zip(h).map(|(a, b)| (a - b) * (a - b)).sum::<f64>().sqrt();
        let scale = norm(g);
        differences.push(if scale > 0.0 { diff / scale } else { diff });
    }
    if let Some(worst) = differences.iter().copied().find(|d| *d > tolerance) {
        return Err(nonperiodic(format!(
            "window check failed: the gradient over {} periods differs from the gradient over {half} periods by {worst:.3e} (relative, tolerance {tolerance:.3e})",
            horizon.periods
        )));
    }
    Ok(differences)
}

fn add_initial_state_terms(
    stepper: &dyn TimeStepper,
    design: &[f64],
    initial_bars: &[Vec<f64>],
    designs: &mut [Vec<f64>],
) -> CaeResult<()> {
    for (bar, d) in initial_bars.iter().zip(designs.iter_mut()) {
        let pulled = stepper.initial_state_vjp(design, bar)?;
        if pulled.len() != d.len() {
            return Err(CaeError::contract("initial_state_vjp returned a cotangent of the wrong size"));
        }
        for (a, b) in d.iter_mut().zip(&pulled) {
            *a += b;
        }
    }
    Ok(())
}




pub fn ensemble_window_gradient(
    stepper: &dyn TimeStepper,
    design: &[f64],
    initial: Option<&[f64]>,
    responses: HorizonResponses<'_>,
    options: &EnsembleOptions,
) -> CaeResult<EnsembleGradient> {
    check_common(stepper, design, options.steps_per_period, options.time_scale)?;
    if options.members == 0 || options.window_periods == 0 || options.spacing_periods == 0 {
        return Err(CaeError::contract("ensemble members, window and spacing must be positive"));
    }
    let n = stepper.state_size();
    let budget = boundary_budget(&options.budget, options.window_periods + 2, n)?;
    let mut z = start_state(stepper, design, initial)?;
    let mut period = 0usize;
    let advance = |z: &[f64], from: usize, count: usize| -> CaeResult<Vec<f64>> {
        let mut state = z.to_vec();
        for k in 0..count {
            let horizon = Horizon {
                stepper,
                design,
                time_scale: options.time_scale,
                steps: options.steps_per_period,
                periods: 1,
                first_period: from + k,
                checkpoint: &options.checkpoint,
                budget: budget.clone(),
            };
            state = horizon.period_run(0, &state)?.final_state().to_vec();
        }
        Ok(state)
    };
    z = advance(&z, period, options.spin_up_periods)?;
    period += options.spin_up_periods;
    let mut member_values = Vec::with_capacity(options.members);
    let mut member_designs: Vec<Vec<Vec<f64>>> = Vec::with_capacity(options.members);
    for e in 0..options.members {
        let horizon = Horizon {
            stepper,
            design,
            time_scale: options.time_scale,
            steps: options.steps_per_period,
            periods: options.window_periods,
            first_period: period,
            checkpoint: &options.checkpoint,
            budget: budget.clone(),
        };
        let forward = horizon.forward(&z)?;
        let (values, bars) = evaluate_responses(responses, &forward.samples)?;
        let backward = horizon.backward(&forward, options.window_periods, &bars, false)?;
        member_values.push(values);
        member_designs.push(backward.design);
        if e + 1 < options.members {
            z = advance(&z, period, options.spacing_periods)?;
            period += options.spacing_periods;
        }
    }
    let r = member_values[0].len();
    if member_values.iter().any(|v| v.len() != r) {
        return Err(CaeError::contract("horizon responses returned a varying number of responses"));
    }
    let count = options.members as f64;
    let m = design.len();
    let values: Vec<f64> = (0..r).map(|i| member_values.iter().map(|v| v[i]).sum::<f64>() / count).collect();
    let mean: Vec<Vec<f64>> = (0..r)
        .map(|i| (0..m).map(|j| member_designs.iter().map(|d| d[i][j]).sum::<f64>() / count).collect())
        .collect();
    let standard_error: Vec<f64> = (0..r)
        .map(|i| {
            if options.members < 2 {
                return f64::INFINITY;
            }
            let variance: f64 = member_designs
                .iter()
                .map(|d| d[i].iter().zip(&mean[i]).map(|(a, b)| (a - b) * (a - b)).sum::<f64>())
                .sum::<f64>()
                / (count - 1.0);
            (variance / count).sqrt()
        })
        .collect();
    let certificate = json!({
        "schema": ENSEMBLE_CERTIFICATE_SCHEMA,
        "stepper": stepper.identity(),
        "derivative_scope": BIASED_ESTIMATE,
        "authoritative": false,
        "members": options.members,
        "window_periods": options.window_periods,
        "spacing_periods": options.spacing_periods,
        "spin_up_periods": options.spin_up_periods,
        "steps_per_period": options.steps_per_period,
        "standard_error": standard_error.iter().map(|v| if v.is_finite() { json!(v) } else { Value::Null }).collect::<Vec<_>>(),
    });
    Ok(EnsembleGradient { values, design: mean, member_values, standard_error, certificate })
}
