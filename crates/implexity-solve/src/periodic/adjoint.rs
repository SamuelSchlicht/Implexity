// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use serde_json::json;

use super::arnoldi::real_eigen;
use super::krylov::{BatchedGmresOptions, batched_gmres, norm};
use super::period_map::PeriodMap;
use super::shooting::PhaseData;
use super::subspace::{SpectralInverse, deterministic_basis, orthonormalize, projected};
use super::{
    PERIODIC_ADJOINT_CERTIFICATE_SCHEMA, PERIODIC_ORBIT_EXACT, PeriodKind, PeriodicGradient, PeriodicMethod,
    PeriodicOptions, PeriodicOrbit,
};
use crate::time_stepper::{StepParameters, TimeStepper};
use crate::trace;

const NEWTON_PICARD_RESTART: usize = 60;
const LEFT_SUBSPACE_ITERATIONS: usize = 12;
const LEFT_SUBSPACE_SETTLED: f64 = 1e-6;
const NEUTRAL_FLOOR: f64 = 1e-3;

struct Pullback {
    initial: Vec<f64>,
    design: Vec<f64>,
    time_scale: f64,
}

type Solved = (Vec<Vec<f64>>, Vec<f64>, Vec<Pullback>, usize);

struct Context<'m, 'a> {
    map: &'m PeriodMap<'a>,
    orbit: &'m PeriodicOrbit,
    sample_bars: Vec<DenseMatrix>,
    autonomous: bool,
    tau: f64,
}

impl Context<'_, '_> {
    fn sweep(&self, items: &[(usize, &[f64], f64, bool)]) -> CaeResult<Vec<Pullback>> {
        let mut bars = Vec::with_capacity(items.len());
        let mut finals = Vec::with_capacity(items.len());
        for (system, w, nu, sources) in items {
            let mut bar = if *sources { self.sample_bars[*system].clone() } else { self.map.zero_samples() };
            if let PhaseData::Section { sample, .. } = &self.orbit.phase {
                let row = bar.nrows - 1;
                bar.data[row * bar.ncols + *sample] += nu;
            }
            bars.push(bar);
            finals.push(w.to_vec());
        }
        let out = self.map.adjoint(&self.orbit.run, self.orbit.period_s, &bars, &finals)?;
        Ok(out
            .into_iter()
            .zip(items)
            .map(|(g, (_, _, nu, _))| {
                let mut initial = g.initial_state;
                if let PhaseData::Integral { derivative, .. } = &self.orbit.phase {
                    for (l, d) in initial.iter_mut().zip(derivative) {
                        *l += nu * d;
                    }
                }
                Pullback { initial, design: g.design, time_scale: g.time_scale }
            })
            .collect())
    }

    fn operator(&self, batch: &[Vec<f64>]) -> CaeResult<Vec<Vec<f64>>> {
        let n = self.map.state_size();
        let items: Vec<(usize, &[f64], f64, bool)> =
            batch.iter().map(|u| (0, &u[..n], if self.autonomous { u[n] } else { 0.0 }, false)).collect();
        let out = self.sweep(&items)?;
        Ok(batch
            .iter()
            .zip(out)
            .map(|(u, p)| {
                let mut row: Vec<f64> = u[..n].iter().zip(&p.initial).map(|(w, l)| w - l).collect();
                if self.autonomous {
                    row.push(self.tau * p.time_scale);
                }
                row
            })
            .collect())
    }
}




#[allow(clippy::too_many_lines)]
pub fn periodic_adjoint_many(
    stepper: &dyn TimeStepper,
    design: &[f64],
    kind: &PeriodKind,
    orbit: &PeriodicOrbit,
    sample_bars: &[DenseMatrix],
    period_bars: &[f64],
    options: &PeriodicOptions,
) -> CaeResult<Vec<PeriodicGradient>> {
    let n = stepper.state_size();
    kind.validate(stepper)?;
    options.validate(kind, n)?;
    let map =
        PeriodMap::new(stepper, design, options.steps_per_period, &options.checkpoint, &options.budget)?;
    let autonomous = kind.is_autonomous();
    check_orbit(&map, kind, orbit)?;
    if sample_bars.len() != period_bars.len() {
        return Err(CaeError::contract("periodic adjoint needs one period cotangent per sample cotangent"));
    }
    let n_s = map.sample_count();
    for (r, bar) in sample_bars.iter().enumerate() {
        if bar.nrows != map.steps || bar.ncols != n_s || !bar.data.iter().all(|v| v.is_finite()) {
            return Err(CaeError::contract(format!(
                "sample cotangent {r} must be a finite {} × {n_s} matrix",
                map.steps
            )));
        }
    }
    if !period_bars.iter().all(|v| v.is_finite()) {
        return Err(CaeError::contract("period cotangents must be finite"));
    }
    let responses = sample_bars.len();
    if responses == 0 && !autonomous {
        return Ok(Vec::new());
    }
    let tau = map.time_scale(orbit.period_s);
    let mut bars: Vec<DenseMatrix> = sample_bars.to_vec();
    let mut pbars: Vec<f64> = if autonomous { period_bars.to_vec() } else { vec![0.0; responses] };
    if autonomous {
        bars.push(map.zero_samples());
        pbars.push(1.0);
    }
    let systems = bars.len();
    let context = Context { map: &map, orbit, sample_bars: bars, autonomous, tau };
    let zero = vec![0.0; n];

    let mut left_sweeps = 0usize;
    let (unknowns, rhs_norms, closing, solve_sweeps) = if options.method == PeriodicMethod::Picard {
        picard(&context, systems, options)?
    } else {
        let items: Vec<(usize, &[f64], f64, bool)> =
            (0..systems).map(|s| (s, &zero[..], 0.0, true)).collect();
        let sources = context.sweep(&items)?;
        let rhs: Vec<Vec<f64>> = sources
            .iter()
            .zip(&pbars)
            .map(|(p, pbar)| {
                let mut b = p.initial.clone();
                if autonomous {
                    b.push(-tau * (map.nominal_period_s * pbar + p.time_scale));
                }
                b
            })
            .collect();
        let order = if autonomous { n + 1 } else { n };
        let (restart, preconditioner) = match options.method {
            PeriodicMethod::NewtonKrylov { krylov_dimension } => (krylov_dimension, None),
            PeriodicMethod::NewtonPicard { subspace } => {
                let before = map.adjoint_sweeps();
                let inverse = left_subspace(&context, subspace)?;
                left_sweeps = map.adjoint_sweeps() - before;
                (NEWTON_PICARD_RESTART, Some(inverse))
            }
            PeriodicMethod::Picard => {
                return Err(CaeError::contract("Picard adjoint is handled by its own iteration"));
            }
        };
        let apply_preconditioner = |v: &[f64]| -> CaeResult<Vec<f64>> {
            let Some(p) = &preconditioner else { return Ok(v.to_vec()) };
            let mut out = p.apply(&v[..n]);
            out.extend_from_slice(&v[n..]);
            Ok(out)
        };
        let gmres = BatchedGmresOptions {
            rtol: options.adjoint_tolerance,
            atol: 0.0,
            restart,
            max_products: options.max_periods,
        };
        let before = map.adjoint_sweeps();
        let solved = batched_gmres(
            order,
            &rhs,
            None,
            &gmres,
            preconditioner.as_ref().map(|_| &apply_preconditioner as &dyn Fn(&[f64]) -> CaeResult<Vec<f64>>),
            |batch| context.operator(batch),
        )?;
        if !solved.all_converged() {
            return Err(CaeError::convergence(format!(
                "periodic adjoint not certified within {} sweeps (worst residual/tolerance {:.3e})",
                options.max_periods,
                solved.worst_ratio()
            )));
        }
        let solve_sweeps = 1 + map.adjoint_sweeps() - before;
        let unknowns = solved.solutions;
        let items: Vec<(usize, &[f64], f64, bool)> = unknowns
            .iter()
            .enumerate()
            .map(|(s, u)| (s, &u[..n], if autonomous { u[n] } else { 0.0 }, true))
            .collect();
        let closing = context.sweep(&items)?;
        (unknowns, solved.rhs_norms, closing, solve_sweeps)
    };
    let mut designs = Vec::with_capacity(systems);
    let mut residuals = Vec::with_capacity(systems);
    let mut worst = 0.0f64;
    for (s, (u, p)) in unknowns.iter().zip(&closing).enumerate() {
        let mut residual: f64 = u[..n].iter().zip(&p.initial).map(|(w, l)| (w - l) * (w - l)).sum();
        if autonomous {
            let r2 = tau * (p.time_scale + map.nominal_period_s * pbars[s]);
            residual += r2 * r2;
        }
        let scale = rhs_norms[s].max(norm(u));
        let relative = if scale > 0.0 { residual.sqrt() / scale } else { 0.0 };
        if relative.is_nan() || relative > 2.0 * options.adjoint_tolerance {
            return Err(CaeError::convergence(format!(
                "periodic adjoint system {s} failed its closing certification (relative residual {relative:.3e} above {:.3e})",
                2.0 * options.adjoint_tolerance
            )));
        }
        worst = worst.max(relative);
        residuals.push(relative);
        designs.push(p.design.clone());
    }
    if let PhaseData::Integral { reference, guess_time_scale, guess_step_s, .. } = &orbit.phase {
        add_integral_phase_design_terms(
            stepper,
            design,
            orbit,
            reference,
            *guess_time_scale,
            *guess_step_s,
            &unknowns,
            &mut designs,
        )?;
    }
    let period_gradient = if autonomous { designs.pop() } else { None };
    let total_sweeps = map.adjoint_sweeps();
    let certificate = json!({
        "schema": PERIODIC_ADJOINT_CERTIFICATE_SCHEMA,
        "stepper": stepper.identity(),
        "derivative_scope": PERIODIC_ORBIT_EXACT,
        "kind": if autonomous { "autonomous" } else { "forced" },
        "method": options.method.name(),
        "responses": responses,
        "systems": systems,
        "period_gradient": autonomous,
        "sweeps": total_sweeps,
        "solve_sweeps": solve_sweeps,
        "left_subspace_sweeps": left_sweeps,
        "closing_sweeps": usize::from(options.method != PeriodicMethod::Picard),
        "relative_residuals": residuals,
        "adjoint_tolerance": options.adjoint_tolerance,
        "orbit_residual": orbit.residual,
        "certified": true,
    });
    trace::point("history_adjoint", || {
        let mut f = trace::Fields::new();
        f.insert("stage".into(), json!("periodic_adjoint"));
        f.insert("steps".into(), json!(options.steps_per_period));
        f.insert("iterations".into(), json!(total_sweeps));
        f.insert("residual_norm".into(), json!(worst));
        f
    })?;
    Ok(designs
        .into_iter()
        .map(|d| PeriodicGradient {
            design: d,
            period_s: period_gradient.clone(),
            certificate: certificate.clone(),
        })
        .collect())
}

fn check_orbit(map: &PeriodMap<'_>, kind: &PeriodKind, orbit: &PeriodicOrbit) -> CaeResult<()> {
    let n = map.state_size();
    if orbit.initial_state.len() != n
        || orbit.samples.nrows != map.steps
        || orbit.samples.ncols != map.sample_count()
    {
        return Err(CaeError::contract(
            "periodic orbit does not belong to this stepper and period discretisation",
        ));
    }
    match (kind, &orbit.phase) {
        (PeriodKind::Forced { period_s }, PhaseData::Forced) => {
            if (orbit.period_s - period_s).abs() > 1e-12 * period_s {
                return Err(CaeError::contract("periodic orbit period differs from the forced period"));
            }
        }
        (PeriodKind::Autonomous { .. }, PhaseData::Section { .. } | PhaseData::Integral { .. }) => {}
        _ => return Err(CaeError::contract("periodic orbit kind differs from the requested kind")),
    }
    Ok(())
}

fn picard(context: &Context<'_, '_>, systems: usize, options: &PeriodicOptions) -> CaeResult<Solved> {
    let n = context.map.state_size();
    let mut w: Vec<Vec<f64>> = vec![vec![0.0; n]; systems];
    let mut closing: Vec<Option<Pullback>> = (0..systems).map(|_| None).collect();
    let mut sources = vec![0.0f64; systems];
    let mut sweeps = 0usize;
    loop {
        let active: Vec<usize> = (0..systems).filter(|s| closing[*s].is_none()).collect();
        if active.is_empty() {
            let closing = closing
                .into_iter()
                .map(|c| c.ok_or_else(|| CaeError::contract("missing closing sweep")))
                .collect::<CaeResult<Vec<_>>>()?;
            return Ok((w, sources, closing, sweeps));
        }
        if sweeps >= options.max_periods {
            return Err(CaeError::convergence(format!(
                "Picard periodic adjoint did not converge within {} sweeps",
                options.max_periods
            )));
        }
        let items: Vec<(usize, &[f64], f64, bool)> =
            active.iter().map(|&s| (s, &w[s][..], 0.0, true)).collect();
        let out = context.sweep(&items)?;
        sweeps += 1;
        for (&s, p) in active.iter().zip(out) {
            if sweeps == 1 {
                sources[s] = norm(&p.initial);
            }
            let change: f64 = w[s].iter().zip(&p.initial).map(|(a, b)| (a - b) * (a - b)).sum::<f64>().sqrt();
            if change <= options.adjoint_tolerance * sources[s].max(norm(&w[s])) {
                closing[s] = Some(p);
            } else {
                w[s] = p.initial;
            }
        }
    }
}

fn left_subspace(context: &Context<'_, '_>, p: usize) -> CaeResult<SpectralInverse> {
    let n = context.map.state_size();
    let mut basis = deterministic_basis(n, p)?;
    let mut previous: Option<Vec<f64>> = None;
    for iteration in 0..LEFT_SUBSPACE_ITERATIONS {
        let items: Vec<(usize, &[f64], f64, bool)> = basis.iter().map(|u| (0, &u[..], 0.0, false)).collect();
        let images: Vec<Vec<f64>> = context.sweep(&items)?.into_iter().map(|p| p.initial).collect();
        let k = projected(&basis, &images);
        let (values, _) = real_eigen(&k)?;
        let mut moduli: Vec<f64> = values.iter().map(|v| v.norm()).collect();
        moduli.sort_by(|a, b| b.total_cmp(a));
        let settled = previous.as_ref().is_some_and(|prev| {
            prev.iter()
                .zip(&moduli)
                .all(|(a, b)| (a - b).abs() <= LEFT_SUBSPACE_SETTLED * b.abs().max(1e-300))
        });
        if settled || iteration + 1 == LEFT_SUBSPACE_ITERATIONS {
            return SpectralInverse::new(basis, &k, NEUTRAL_FLOOR);
        }
        previous = Some(moduli);
        basis = orthonormalize(&images, n)?;
    }
    Err(CaeError::contract("left subspace iteration budget must be positive"))
}

#[allow(clippy::too_many_arguments)]
fn add_integral_phase_design_terms(
    stepper: &dyn TimeStepper,
    design: &[f64],
    orbit: &PeriodicOrbit,
    reference: &[f64],
    guess_time_scale: f64,
    guess_step_s: f64,
    unknowns: &[Vec<f64>],
    designs: &mut [Vec<f64>],
) -> CaeResult<()> {
    let n = reference.len();
    let p = StepParameters { design, time_scale: guess_time_scale };
    let current = stepper.advance(1, reference, p)?.state;
    let offset: Vec<f64> =
        orbit.initial_state.iter().zip(reference).map(|(z, r)| (z - r) / guess_step_s).collect();
    let state_bars: Vec<Vec<f64>> =
        unknowns.iter().map(|u| offset.iter().map(|o| u[n] * o).collect()).collect();
    let sample_bars = vec![vec![0.0; stepper.sample_names().len()]; unknowns.len()];
    let out = stepper.adjoint_many(1, reference, &current, p, &state_bars, &sample_bars)?;
    if out.len() != designs.len() {
        return Err(CaeError::contract(
            "time stepper adjoint_many returned a different number of cotangents",
        ));
    }
    for (d, c) in designs.iter_mut().zip(out) {
        if c.design.len() != d.len() {
            return Err(CaeError::contract("time stepper design cotangent has the wrong size"));
        }
        for (a, b) in d.iter_mut().zip(&c.design) {
            *a += b;
        }
    }
    Ok(())
}
