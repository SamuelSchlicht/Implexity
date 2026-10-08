// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseMatrix, solve};
use serde_json::{Value, json};

use super::arnoldi::{ArnoldiOptions, ArnoldiResult, dominant_eigenpairs};
use super::krylov::{BatchedGmresOptions, batched_gmres, dot, norm};
use super::period_map::PeriodMap;
use super::subspace::{coefficients, combine, deterministic_basis, orthonormalize, project_out, projected};
use super::{
    Multiplier, NONPERIODIC_REGIME_RECORD, PERIODIC_ORBIT_CERTIFICATE_SCHEMA, PeriodKind, PeriodicGuess,
    PeriodicMethod, PeriodicOptions, PeriodicOrbit, PhaseCondition, UNSTABLE_ORBIT_RECORD, nonperiodic,
    residual_scale,
};
use crate::checkpointed_history::HistoryRun;
use crate::step_stability::forcing_term;
use crate::time_stepper::{StepParameters, TimeStepper};
use crate::trace;

pub const MAX_NEWTON_ITERATIONS: usize = 60;
const LINE_SEARCH_HALVINGS: usize = 12;
const INNER_PICARD: usize = 40;
const ARNOLDI_RESTARTS: usize = 300;
pub const TRIVIAL_MULTIPLIER_RADIUS: f64 = 1e-4;

#[derive(Clone, Debug)]
pub(crate) enum PhaseData {
    Forced,
    Section { sample: usize, level: f64 },
    Integral { reference: Vec<f64>, derivative: Vec<f64>, guess_time_scale: f64, guess_step_s: f64 },
}

impl PhaseData {
    pub(crate) fn build(map: &PeriodMap<'_>, kind: &PeriodKind) -> CaeResult<Self> {
        match kind {
            PeriodKind::Forced { .. } => Ok(Self::Forced),
            PeriodKind::Autonomous { phase: PhaseCondition::Section { sample, level }, .. } => {
                if map.steps < 2 {
                    return Err(CaeError::contract(
                        "a section phase condition needs at least two steps per period",
                    ));
                }
                Ok(Self::Section { sample: *sample, level: *level })
            }
            PeriodKind::Autonomous {
                phase: PhaseCondition::Integral { reference }, period_guess_s, ..
            } => {
                let tau = map.time_scale(*period_guess_s);
                let dt = *period_guess_s / map.steps as f64;
                let record = map.stepper.advance(
                    1,
                    reference,
                    StepParameters { design: map.design, time_scale: tau },
                )?;
                if record.state.len() != reference.len() || !record.state.iter().all(|v| v.is_finite()) {
                    return Err(CaeError::convergence("integral phase reference step is not finite"));
                }
                let derivative: Vec<f64> =
                    record.state.iter().zip(reference).map(|(a, b)| (a - b) / dt).collect();
                if norm(&derivative) == 0.0 {
                    return Err(CaeError::contract(
                        "integral phase condition reference is a steady state (zero time derivative)",
                    ));
                }
                Ok(Self::Integral {
                    reference: reference.clone(),
                    derivative,
                    guess_time_scale: tau,
                    guess_step_s: dt,
                })
            }
        }
    }

    fn is_forced(&self) -> bool {
        matches!(self, Self::Forced)
    }

    pub(crate) fn value(&self, z0: &[f64], samples: &DenseMatrix) -> f64 {
        match self {
            Self::Forced => 0.0,
            Self::Section { sample, level } => samples.get(samples.nrows - 1, *sample) - level,
            Self::Integral { reference, derivative, .. } => {
                z0.iter().zip(reference).zip(derivative).map(|((z, r), d)| (z - r) * d).sum()
            }
        }
    }

    fn scale(&self, z0: &[f64], samples: &DenseMatrix) -> f64 {
        let s = match self {
            Self::Forced => 1.0,
            Self::Section { sample, level } => {
                (0..samples.nrows).map(|i| samples.get(i, *sample).abs()).fold(level.abs(), f64::max)
            }
            Self::Integral { reference, derivative, .. } => norm(derivative) * norm(z0).max(norm(reference)),
        };
        if s > 0.0 && s.is_finite() { s } else { 1.0 }
    }

    fn linear(&self, v: &[f64], d_samples: &DenseMatrix) -> f64 {
        match self {
            Self::Forced => 0.0,
            Self::Section { sample, .. } => d_samples.get(d_samples.nrows - 1, *sample),
            Self::Integral { derivative, .. } => dot(derivative, v),
        }
    }

    fn check_crossing(&self, samples: &DenseMatrix) -> CaeResult<()> {
        if let Self::Section { sample, level } = self {
            let n = samples.nrows;
            let before = samples.get(n - 2, *sample);
            let after = samples.get(0, *sample);
            if !(before < after && before <= *level && *level <= after) {
                return Err(nonperiodic(format!(
                    "the section phase condition converged to a crossing that is not upward and transversal (sample {sample}: {before:.6e} before, {after:.6e} after the level {level:.6e})"
                )));
            }
        }
        Ok(())
    }
}

struct Iterate {
    z: Vec<f64>,
    period_s: f64,
    run: HistoryRun,
    residual_vector: Vec<f64>,
    residual: f64,
    phase: f64,
    phase_scale: f64,
    phase_residual: f64,
    merit: f64,
}

impl Iterate {
    fn converged(&self, tolerance: f64) -> bool {
        self.residual <= tolerance && self.phase_residual <= tolerance
    }
}

struct Solver<'m, 'a> {
    map: &'m PeriodMap<'a>,
    phase: &'m PhaseData,
    options: &'m PeriodicOptions,
    bounds: Option<(f64, f64)>,
}

impl Solver<'_, '_> {
    fn evaluate(&self, z: Vec<f64>, period_s: f64, best: f64) -> CaeResult<Iterate> {
        if self.map.forward_runs() >= self.options.max_periods {
            return Err(nonperiodic(format!(
                "the periodic residual {best:.3e} did not reach {:.3e} within the budget of {} forward periods",
                self.options.tolerance, self.options.max_periods
            )));
        }
        let run = self.map.run(&z, period_s)?;
        let end = run.final_state();
        let residual_vector: Vec<f64> = end.iter().zip(&z).map(|(a, b)| a - b).collect();
        let residual = norm(&residual_vector) / residual_scale(&z, end);
        let samples = run.samples();
        let phase = self.phase.value(&z, samples);
        let phase_scale = self.phase.scale(&z, samples);
        let phase_residual = phase.abs() / phase_scale;
        Ok(Iterate {
            merit: residual.hypot(phase_residual),
            z,
            period_s,
            run,
            residual_vector,
            residual,
            phase,
            phase_scale,
            phase_residual,
        })
    }

    fn picard(&self, mut it: Iterate) -> CaeResult<(Iterate, usize)> {
        let mut iterations = 0usize;
        while !it.converged(self.options.tolerance) {
            iterations += 1;
            let next = it.run.final_state().to_vec();
            it = self.evaluate(next, it.period_s, it.residual)?;
            let residual = it.residual;
            trace::point("history_solve", || {
                let mut f = trace::Fields::new();
                f.insert("stage".into(), json!("periodic_picard"));
                f.insert("iterations".into(), json!(iterations));
                f.insert("residual_norm".into(), json!(residual));
                f
            })?;
        }
        Ok((it, iterations))
    }

    fn newton(&self, mut it: Iterate, method: PeriodicMethod) -> CaeResult<(Iterate, usize, usize)> {
        let tolerance = self.options.tolerance;
        let mut previous = f64::NAN;
        let mut iterations = 0usize;
        let mut krylov_products = 0usize;
        let mut basis: Option<Vec<Vec<f64>>> = None;
        while !it.converged(tolerance) {
            if iterations >= MAX_NEWTON_ITERATIONS {
                return Err(nonperiodic(format!(
                    "shooting did not converge within {MAX_NEWTON_ITERATIONS} Newton iterations (residual {:.3e})",
                    it.merit
                )));
            }
            iterations += 1;
            let eta = forcing_term(it.merit, previous, tolerance);
            let before = self.map.tangent_runs();
            let (dz, theta) = match method {
                PeriodicMethod::NewtonKrylov { krylov_dimension } => {
                    self.krylov_step(&it, eta, krylov_dimension)?
                }
                PeriodicMethod::NewtonPicard { subspace } => {
                    self.newton_picard_step(&it, eta, subspace, &mut basis)?
                }
                PeriodicMethod::Picard => {
                    return Err(CaeError::contract("Picard is not a Newton method"));
                }
            };
            let products = self.map.tangent_runs() - before;
            krylov_products += products;
            let (next, alpha) = self.line_search(&it, &dz, theta)?;
            previous = it.merit;
            it = next;
            let merit = it.merit;
            trace::point("newton_iteration", || {
                let mut f = trace::Fields::new();
                f.insert("stage".into(), json!(format!("periodic_{}", method.name())));
                f.insert("newton_iteration".into(), json!(iterations));
                f.insert("residual_norm".into(), json!(merit));
                f.insert("iterations".into(), json!(products));
                f.insert("alpha".into(), json!(alpha));
                f
            })?;
        }
        Ok((it, iterations, krylov_products))
    }

    fn line_search(&self, it: &Iterate, dz: &[f64], theta: f64) -> CaeResult<(Iterate, f64)> {
        let mut alpha = 1.0;
        let mut ran = false;
        for _ in 0..LINE_SEARCH_HALVINGS {
            let period = it.period_s * (1.0 + alpha * theta);
            let inside = match self.bounds {
                Some((lo, hi)) => period.is_finite() && period > lo && period < hi,
                None => true,
            };
            if inside {
                ran = true;
                let z: Vec<f64> = it.z.iter().zip(dz).map(|(a, d)| a + alpha * d).collect();
                let trial = self.evaluate(z, period, it.merit)?;
                if trial.converged(self.options.tolerance) || trial.merit <= (1.0 - 1e-4 * alpha) * it.merit {
                    return Ok((trial, alpha));
                }
            }
            alpha *= 0.5;
        }
        if !ran {
            let (lo, hi) = self.bounds.unwrap_or((0.0, f64::INFINITY));
            return Err(nonperiodic(format!(
                "the period left its admissible interval [{lo:.6e}, {hi:.6e}] s along every Newton step"
            )));
        }
        Err(nonperiodic(format!(
            "shooting Newton stalled: no decrease of the periodic residual {:.3e} along the Newton direction",
            it.merit
        )))
    }

    fn phase_weight(it: &Iterate) -> f64 {
        residual_scale(&it.z, it.run.final_state()) / it.phase_scale
    }

    fn krylov_step(&self, it: &Iterate, eta: f64, dimension: usize) -> CaeResult<(Vec<f64>, f64)> {
        let n = it.z.len();
        let options =
            BatchedGmresOptions { rtol: eta, atol: 0.0, restart: dimension, max_products: dimension };
        if self.phase.is_forced() {
            let out =
                batched_gmres(n, std::slice::from_ref(&it.residual_vector), None, &options, None, |batch| {
                    batch
                        .iter()
                        .map(|v| {
                            let t = self.map.tangent(&it.run, it.period_s, v, 0.0)?;
                            Ok(v.iter().zip(&t.state).map(|(a, b)| a - b).collect())
                        })
                        .collect()
                })?;
            return Ok((out.solutions.into_iter().next().unwrap_or_default(), 0.0));
        }
        let weight = Self::phase_weight(it);
        let mut rhs = it.residual_vector.clone();
        rhs.push(-weight * it.phase);
        let out = batched_gmres(n + 1, &[rhs], None, &options, None, |batch| {
            batch
                .iter()
                .map(|u| {
                    let (v, t) = u.split_at(n);
                    let tangent = self.map.tangent(&it.run, it.period_s, v, t[0])?;
                    let mut out: Vec<f64> = v.iter().zip(&tangent.state).map(|(a, b)| a - b).collect();
                    out.push(weight * self.phase.linear(v, &tangent.samples));
                    Ok(out)
                })
                .collect()
        })?;
        let mut u = out.solutions.into_iter().next().unwrap_or_default();
        let theta = u.pop().unwrap_or(0.0);
        Ok((u, theta))
    }

    fn complement_picard(
        &self,
        it: &Iterate,
        basis: &[Vec<f64>],
        b: &[f64],
        eta: f64,
    ) -> CaeResult<(Vec<f64>, Vec<f64>, f64)> {
        let scale = norm(b).max(f64::MIN_POSITIVE);
        let mut q = project_out(basis, b);
        for sweep in 0..INNER_PICARD {
            let t = self.map.tangent(&it.run, it.period_s, &q, 0.0)?;
            let target: Vec<f64> = b.iter().zip(&t.state).map(|(x, y)| x + y).collect();
            let next = project_out(basis, &target);
            let change = norm(&next.iter().zip(&q).map(|(a, c)| a - c).collect::<Vec<_>>());
            if change <= eta * scale || sweep + 1 == INNER_PICARD {
                let g = self.phase.linear(&q, &t.samples);
                return Ok((q, t.state, g));
            }
            q = next;
        }
        Err(CaeError::contract("Newton–Picard inner iteration budget must be positive"))
    }

    fn newton_picard_step(
        &self,
        it: &Iterate,
        eta: f64,
        subspace: usize,
        basis: &mut Option<Vec<Vec<f64>>>,
    ) -> CaeResult<(Vec<f64>, f64)> {
        let n = it.z.len();
        let v = match basis.take() {
            Some(v) => v,
            None => deterministic_basis(n, subspace)?,
        };
        let p = v.len();
        let mut images = Vec::with_capacity(p);
        let mut phase_images = Vec::with_capacity(p);
        for column in &v {
            let t = self.map.tangent(&it.run, it.period_s, column, 0.0)?;
            phase_images.push(self.phase.linear(column, &t.samples));
            images.push(t.state);
        }
        let h = projected(&v, &images);
        let r = &it.residual_vector;
        let (q_r, mq_r, g_r) = self.complement_picard(it, &v, r, eta)?;
        let solution = if self.phase.is_forced() {
            let mut a = DenseMatrix::identity(p);
            for (x, y) in a.data.iter_mut().zip(&h.data) {
                *x -= y;
            }
            let rhs: Vec<f64> =
                coefficients(&v, &r.iter().zip(&mq_r).map(|(x, y)| x + y).collect::<Vec<_>>());
            let coeffs = solve(&a, &rhs, 1).map_err(|e| {
                CaeError::convergence(format!("Newton–Picard subspace system is singular: {e}"))
            })?;
            let mut dz = combine(&v, &coeffs, n);
            for (d, q) in dz.iter_mut().zip(&q_r) {
                *d += q;
            }
            (dz, 0.0)
        } else {
            let t_period = self.map.tangent(&it.run, it.period_s, &vec![0.0; n], 1.0)?;
            let g_period = self.phase.linear(&vec![0.0; n], &t_period.samples);
            let (q_t, mq_t, g_t) = self.complement_picard(it, &v, &t_period.state, eta)?;
            let m = p + 1;
            let mut a = DenseMatrix::zeros(m, m);
            let border =
                coefficients(&v, &t_period.state.iter().zip(&mq_t).map(|(x, y)| x + y).collect::<Vec<_>>());
            for i in 0..p {
                for j in 0..p {
                    a.data[i * m + j] = f64::from(u8::from(i == j)) - h.data[i * p + j];
                }
                a.data[i * m + p] = -border[i];
                a.data[p * m + i] = phase_images[i];
            }
            a.data[p * m + p] = g_t + g_period;
            let mut rhs = coefficients(&v, &r.iter().zip(&mq_r).map(|(x, y)| x + y).collect::<Vec<_>>());
            rhs.push(-it.phase - g_r);
            let x = solve(&a, &rhs, 1).map_err(|e| {
                CaeError::convergence(format!("Newton–Picard bordered subspace system is singular: {e}"))
            })?;
            let theta = x[p];
            let mut dz = combine(&v, &x[..p], n);
            for ((d, qr), qt) in dz.iter_mut().zip(&q_r).zip(&q_t) {
                *d += qr + theta * qt;
            }
            (dz, theta)
        };

        *basis = Some(orthonormalize(&images, n)?);
        Ok(solution)
    }
}




pub fn solve_periodic(
    stepper: &dyn TimeStepper,
    design: &[f64],
    kind: &PeriodKind,
    options: &PeriodicOptions,
    guess: Option<&PeriodicGuess>,
) -> CaeResult<PeriodicOrbit> {
    let n = stepper.state_size();
    kind.validate(stepper)?;
    options.validate(kind, n)?;
    let map =
        PeriodMap::new(stepper, design, options.steps_per_period, &options.checkpoint, &options.budget)?;
    let phase = PhaseData::build(&map, kind)?;
    let (bounds, declared) = match kind {
        PeriodKind::Forced { period_s } => (None, *period_s),
        PeriodKind::Autonomous { period_guess_s, period_bounds_s, .. } => {
            (Some(*period_bounds_s), *period_guess_s)
        }
    };
    let (start, period, warm) = start_point(stepper, design, guess, bounds, declared)?;
    let solver = Solver { map: &map, phase: &phase, options, bounds };
    let mut z = start;
    if !warm {
        for _ in 0..options.spin_up_periods {
            let it = solver.evaluate(z, period, f64::INFINITY)?;
            z = it.run.final_state().to_vec();
        }
    }
    let first = solver.evaluate(z, period, f64::INFINITY)?;
    let (orbit, iterations, krylov) = match options.method {
        PeriodicMethod::Picard => {
            let (it, k) = solver.picard(first)?;
            (it, k, 0)
        }
        method => solver.newton(first, method)?,
    };
    phase.check_crossing(orbit.run.samples())?;
    let tangent_before_gate = map.tangent_runs();
    let arnoldi = dominant_eigenpairs(
        n,
        &ArnoldiOptions {
            wanted: options.floquet_modes,
            basis: None,
            tolerance: options.adjoint_tolerance,
            max_restarts: ARNOLDI_RESTARTS,
        },
        None,
        |v| Ok(map.tangent(&orbit.run, orbit.period_s, v, 0.0)?.state),
    )?;
    let multipliers: Vec<Multiplier> =
        arnoldi.pairs.iter().map(|p| Multiplier { re: p.value.re, im: p.value.im }).collect();
    let gate = stability_gate(&multipliers, kind.is_autonomous(), options.stability_margin)?;
    let certificate = orbit_certificate(&Certified {
        stepper,
        design,
        kind,
        options,
        map: &map,
        phase: &phase,
        orbit: &orbit,
        iterations,
        krylov,
        warm,
        arnoldi: &arnoldi,
        gate: &gate,
        tangent_before_gate,
    });
    let residual = orbit.residual;
    trace::point("history_solve", || {
        let mut f = trace::Fields::new();
        f.insert("stage".into(), json!("periodic_orbit"));
        f.insert("steps".into(), json!(options.steps_per_period));
        f.insert("state_size".into(), json!(n));
        f.insert("design_size".into(), json!(design.len()));
        f.insert("iterations".into(), json!(iterations));
        f.insert("residual_norm".into(), json!(residual));
        f.insert("dominant_multiplier_modulus".into(), json!(gate.largest));
        f
    })?;
    Ok(PeriodicOrbit {
        samples: orbit.run.samples().clone(),
        initial_state: orbit.z,
        period_s: orbit.period_s,
        multipliers,
        residual: orbit.residual,
        periods_run: map.forward_runs(),
        tangent_periods_run: map.tangent_runs(),
        certificate,
        run: orbit.run,
        phase,
    })
}

fn start_point(
    stepper: &dyn TimeStepper,
    design: &[f64],
    guess: Option<&PeriodicGuess>,
    bounds: Option<(f64, f64)>,
    declared: f64,
) -> CaeResult<(Vec<f64>, f64, bool)> {
    let n = stepper.state_size();
    let Some(g) = guess else {
        let z = stepper.initial_state(design)?;
        if z.len() != n || !z.iter().all(|v| v.is_finite()) {
            return Err(CaeError::contract("time stepper initial state is not a finite state vector"));
        }
        return Ok((z, declared, false));
    };
    if g.state.len() != n || !g.state.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract(format!(
            "periodic guess state must be a finite vector of length {n}"
        )));
    }
    match bounds {
        None if (g.period_s - declared).abs() > 1e-12 * declared => Err(CaeError::contract(format!(
            "periodic guess period {:.12e} s differs from the forced period {declared:.12e} s",
            g.period_s
        ))),
        Some((lo, hi)) if !(g.period_s > lo && g.period_s < hi) => Err(CaeError::contract(format!(
            "periodic guess period {:.6e} s lies outside the admissible interval [{lo:.6e}, {hi:.6e}] s",
            g.period_s
        ))),
        None => Ok((g.state.clone(), declared, true)),
        Some(_) => Ok((g.state.clone(), g.period_s, true)),
    }
}

struct Certified<'c, 'a> {
    stepper: &'c dyn TimeStepper,
    design: &'c [f64],
    kind: &'c PeriodKind,
    options: &'c PeriodicOptions,
    map: &'c PeriodMap<'a>,
    phase: &'c PhaseData,
    orbit: &'c Iterate,
    iterations: usize,
    krylov: usize,
    warm: bool,
    arnoldi: &'c ArnoldiResult,
    gate: &'c Gate,
    tangent_before_gate: usize,
}

fn orbit_certificate(c: &Certified<'_, '_>) -> Value {
    let multipliers: Vec<Value> = c
        .arnoldi
        .pairs
        .iter()
        .map(|p| json!({"re": p.value.re, "im": p.value.im, "modulus": p.value.norm(), "ritz_residual": p.residual}))
        .collect();
    json!({
        "schema": PERIODIC_ORBIT_CERTIFICATE_SCHEMA,
        "stepper": c.stepper.identity(),
        "kind": if c.kind.is_autonomous() { "autonomous" } else { "forced" },
        "method": c.options.method.name(),
        "phase_condition": match c.phase {
            PhaseData::Forced => Value::Null,
            PhaseData::Section { sample, level } => json!({"type": "section", "sample": sample, "level": level}),
            PhaseData::Integral { .. } => json!({"type": "integral"}),
        },
        "state_size": c.stepper.state_size(),
        "design_size": c.design.len(),
        "steps_per_period": c.options.steps_per_period,
        "period_s": c.orbit.period_s,
        "time_scale": c.map.time_scale(c.orbit.period_s),
        "residual": c.orbit.residual,
        "phase_residual": c.orbit.phase_residual,
        "tolerance": c.options.tolerance,
        "iterations": c.iterations,
        "warm_start": c.warm,
        "spin_up_periods": if c.warm { 0 } else { c.options.spin_up_periods },
        "periods_run": c.map.forward_runs(),
        "tangent_periods_run": c.map.tangent_runs(),
        "shooting_tangent_periods": c.krylov,
        "floquet": {
            "multipliers": multipliers,
            "arnoldi_products": c.map.tangent_runs() - c.tangent_before_gate,
            "arnoldi_restarts": c.arnoldi.restarts,
            "tolerance": c.options.adjoint_tolerance,
            "trivial_index": c.gate.trivial,
            "largest_nontrivial_modulus": c.gate.largest,
            "stability_margin": c.options.stability_margin,
            "stable": true,
        },
        "certified": true,
    })
}

struct Gate {
    trivial: Option<usize>,
    largest: f64,
}

fn stability_gate(multipliers: &[Multiplier], autonomous: bool, margin: f64) -> CaeResult<Gate> {
    let trivial = if autonomous {
        let (index, distance) = multipliers
            .iter()
            .enumerate()
            .map(|(i, m)| (i, (m.re - 1.0).hypot(m.im)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .ok_or_else(|| CaeError::contract("the stability gate needs at least one multiplier"))?;
        if distance > TRIVIAL_MULTIPLIER_RADIUS {
            return Err(nonperiodic(format!(
                "no neutral multiplier within {TRIVIAL_MULTIPLIER_RADIUS:.0e} of 1 among the {} dominant multipliers of the autonomous orbit (closest at distance {distance:.3e}); increase floquet_modes",
                multipliers.len()
            )));
        }
        Some(index)
    } else {
        None
    };
    let largest = multipliers
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != trivial)
        .map(|(_, m)| m.modulus())
        .fold(0.0, f64::max);
    if largest >= 1.0 - margin {
        let list: Vec<String> = multipliers.iter().map(|m| format!("{:.6e}{:+.6e}i", m.re, m.im)).collect();
        return Err(CaeError::convergence(format!(
            "{UNSTABLE_ORBIT_RECORD}: a non-trivial Floquet multiplier has modulus {largest:.6e} ≥ 1 − {margin:.1e}; the periodic orbit is not stable and would not be reached by a physical run (multipliers: {})",
            list.join(", ")
        )));
    }
    debug_assert!(NONPERIODIC_REGIME_RECORD != UNSTABLE_ORBIT_RECORD);
    Ok(Gate { trivial, largest })
}
