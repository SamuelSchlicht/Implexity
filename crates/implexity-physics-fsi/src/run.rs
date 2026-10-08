// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::lu::{LuSymbolic, Parallelism};
use implexity_physics_solid::soft::stepper::Scheme;
use implexity_solve::checkpointed_history::{history_adjoint_many, run_history};
use implexity_solve::dynamic_program::TermQuantity;
use implexity_solve::matrix::Jacobian;
use implexity_solve::multirate_coupling::{
    LEDGER_INTERFACE_WORK_B, LEDGER_INTERFACE_WORK_DEFECT,
};
use implexity_solve::periodic::regime::{
    BIASED_ESTIMATE, DISCRETE_HISTORY_EXACT, EnsembleOptions, FixedHorizonGradient, FixedHorizonOptions,
    HorizonResponses, ensemble_window_gradient, fixed_horizon_gradient,
};
use implexity_solve::periodic::{
    NONPERIODIC_REGIME_RECORD, PERIODIC_ORBIT_EXACT, PeriodKind, PeriodicGuess, PeriodicOptions,
    PeriodicOrbit, PhaseCondition, periodic_adjoint_many, regime_record, solve_periodic,
};
use implexity_solve::state_store::StoreBudget;
use implexity_solve::step_stability::{
    SteadyState, StepMode, dominant_modes, growth_rate_gradient, steady_state,
};
use implexity_solve::time_stepper::{StepParameters, TimeStepper};

use crate::model::FsiModel;
use crate::interface::FluidField;
use crate::moving_contact::solid_view::SolidView;
use implexity_solve::multirate_coupling::MultirateStepper;
use crate::problem::{DESIGN_VOLUME, PhaseSpec, TimeKind};

pub const STABILITY_EIGENVALUE_EXACT: &str = "stability_eigenvalue_exact";

pub const NEUTRAL_INTEGRATOR_HINT: &str = "the solid integrator conserves energy (avf_midpoint or newmark with gamma = 1/2 without stiffness-proportional damping, or generalized_alpha with rho_inf = 1): unresolved stiff solid modes stay on the unit circle (and generalized_alpha with rho_inf = 1 maps every algorithmic acceleration with the multiplier -1), so periodic orbits and steady modes have neutral multipliers that the stability gate cannot separate; use solid.integrator = generalized_alpha with rho_inf < 1 (e.g. 0.5) for periodic_forced, periodic_autonomous and steady_stability, or add solid.rayleigh.beta_stiffness_s > 0 to an avf_midpoint/newmark solid";

#[must_use]
pub fn neutral_integrator(model: &FsiModel) -> bool {
    let undamped = model.problem.solid.rayleigh.1 == 0.0;
    match model.problem.solid.scheme {
        Scheme::AvfMidpoint { .. } => undamped,
        Scheme::Newmark { gamma, .. } => (gamma - 0.5).abs() < 1e-12 && undamped,
        Scheme::GeneralizedAlpha { rho_inf } => (rho_inf - 1.0).abs() < 1e-12,
        Scheme::Quasistatic => false,
    }
}

fn with_integrator_hint(model: &FsiModel, e: CaeError) -> CaeError {
    let stability = ["Arnoldi", "Floquet", "periodic_orbit_unstable", "multiplier"];
    if !(neutral_integrator(model) && stability.iter().any(|k| e.message().contains(k))) {
        return e;
    }
    match e {
        e @ CaeError::Recovery { .. } => e.context(NEUTRAL_INTEGRATOR_HINT),
        CaeError::Contract(m) => CaeError::Contract(format!("{m} (hint: {NEUTRAL_INTEGRATOR_HINT})")),
        CaeError::Convergence(m) => CaeError::Convergence(format!("{m} (hint: {NEUTRAL_INTEGRATOR_HINT})")),
        CaeError::NewtonConvergence(m) => {
            CaeError::NewtonConvergence(format!("{m} (hint: {NEUTRAL_INTEGRATOR_HINT})"))
        }
    }
}

pub const MIN_ARNOLDI_MODES: usize = 8;

pub const STABILITY_RESPONSES: [&str; 2] = ["growth_rate_per_s", "angular_frequency_rad_s"];

#[derive(Clone, Debug, Default)]
pub struct RunSettings {
    pub budget: StoreBudget,
    pub guess: Option<PeriodicGuess>,
}

#[derive(Clone, Debug)]
pub struct DynamicResult {
    pub values: BTreeMap<String, f64>,
    pub samples: DenseMatrix,
    pub sample_names: Vec<String>,
    pub step_s: f64,
    pub period_s: f64,
    pub regime: String,
    pub derivative_scope: String,
    pub start_state: Vec<f64>,
    pub first_step: usize,
    pub time_scale: f64,
    pub certificate: Value,
    pub ledger: Value,
    pub guess: Option<PeriodicGuess>,
}

#[derive(Clone, Debug)]
pub struct DynamicGradient {
    pub values: BTreeMap<String, (f64, Vec<f64>)>,
    pub derivative_scope: String,
    pub authoritative: bool,
    pub certificate: Value,
}


pub fn design_term(model: &FsiModel, name: &str, design: &[f64]) -> CaeResult<(f64, Vec<f64>)> {
    let kind = model
        .problem
        .program
        .terms()
        .iter()
        .find(|t| t.name == name)
        .and_then(|t| match &t.quantity {
            TermQuantity::Design { kind, .. } => Some(kind.as_str()),
            _ => None,
        })
        .ok_or_else(|| {
            CaeError::contract(format!("{name:?} is not a design term of the response program"))
        })?;
    if kind == DESIGN_VOLUME {
        return Ok(volume_fraction(design));
    }
    match &model.problem.design.removal {
        Some(removal) => removal.term(kind, design),
        None => Err(CaeError::contract(format!(
            "design term {kind:?} needs a removal-only design (design.removal)"
        ))),
    }
}

fn volume_fraction(design: &[f64]) -> (f64, Vec<f64>) {
    let n = design.len().max(1) as f64;
    (design.iter().sum::<f64>() / n, vec![1.0 / n; design.len()])
}

#[must_use]
pub fn ledger_summary(rows: &[BTreeMap<String, f64>]) -> Value {
    let mut acc: BTreeMap<String, [f64; 4]> = BTreeMap::new();
    for row in rows {
        for (k, v) in row {
            let e = acc.entry(k.clone()).or_insert([0.0, f64::INFINITY, f64::NEG_INFINITY, 0.0]);
            e[0] += v;
            e[1] = e[1].min(*v);
            e[2] = e[2].max(*v);
            e[3] = *v;
        }
    }
    Value::Object(
        acc.into_iter()
            .map(|(k, [s, lo, hi, last])| (k, json!({"sum": s, "min": lo, "max": hi, "last": last})))
            .collect::<Map<String, Value>>(),
    )
}

pub(crate) fn inertia_coefficient(scheme: Scheme) -> f64 {
    match scheme {
        Scheme::Quasistatic => 0.0,
        Scheme::Newmark { beta, .. } => 1.0 / beta,
        Scheme::GeneralizedAlpha { rho_inf } => {
            let am = (2.0 * rho_inf - 1.0) / (rho_inf + 1.0);
            let af = rho_inf / (rho_inf + 1.0);
            let beta = 0.25 * (1.0 - am + af).powi(2);
            (1.0 - am) / beta
        }
        Scheme::AvfMidpoint { .. } => 2.0,
    }
}


#[allow(clippy::too_many_lines)]
pub fn schur_preflight<'m,B: SolidView<'m>>(model: &FsiModel, stepper: &MultirateStepper<FluidField, B>, design: &[f64]) -> CaeResult<Value> {
    let scheme = model.problem.solid.scheme;
    let c = inertia_coefficient(scheme);
    if c == 0.0 {
        return Err(CaeError::contract(
            "the interface Schur-ratio preflight needs an inertial solid integrator (the quasistatic solid has no mass)",
        ));
    }
    let b = stepper.field_b();
    let p = StepParameters { design, time_scale: 1.0 };
    let z0 = stepper.initial_state(design)?;
    let layout = stepper.layout();
    let zb = &z0[layout.field_b.clone()];
    let pred = b.predict(1, zb, None, p);
    let flux = vec![0.0; b.trace_operator().nrows()];
    let jac = b.jacobians(1, &pred, zb, &flux, p)?;
    let (Jacobian::Csr(current), Jacobian::Csr(fj)) = (&jac.current, &jac.flux) else {
        return Err(CaeError::contract("internal: the soft-solid field publishes CSR Jacobian blocks"));
    };
    let n_b = b.state_size();
    let n_t = fj.ncols();
    let mut row = vec![0.0; n_t];
    for (i, r) in row.iter_mut().enumerate() {
        let (cols, vals) = fj.row(i);
        if let Some(pos) = cols.iter().position(|&j| j == i) {
            *r = -vals[pos];
        }
    }
    let history = b.solid_core().history(p)?;
    let dt = model.problem.time.macro_step_s();
    let mass = history.mass_matrix();
    let mass_action = |phi: &[f64]| -> Vec<f64> {
        let mu = mass.matvec(&phi[..n_t]).unwrap_or_else(|_| vec![f64::NAN; n_t]);
        let mut out = vec![0.0; n_b];
        for i in 0..n_t {
            out[i] = row[i] * c / (dt * dt) * mu[i];
        }
        out
    };
    let csc = current.to_csc();
    let lu = LuSymbolic::analyze(&csc)
        .and_then(|s| s.factor(&csc, Parallelism::Sequential))
        .map_err(|e| CaeError::convergence(format!("Schur-ratio preflight: solid step Jacobian: {e}")))?;
    let k = model.problem.coupling.preflight_modes;
    let free: Vec<bool> =
        (0..n_t).map(|i| row[i] != 0.0 && model.soft.fixed.get(i).is_some_and(|f| !f)).collect();

    let pts = &model.soft.mesh.points;
    let mut modes: Vec<Vec<f64>> = (0..k)
        .map(|m| {
            let mut v = vec![0.0; n_b];
            for (node, x) in pts.iter().enumerate() {
                let comp = m % 3;
                let weight = 1.0 + (m / 3) as f64 * (x[0] + x[1] + x[2]);
                if free[3 * node + comp] {
                    v[3 * node + comp] = weight;
                }
            }
            v
        })
        .collect();
    for _ in 0..8 {
        let mut next = Vec::with_capacity(k);
        for phi in &modes {
            let rhs = mass_action(phi);
            let x =
                lu.solve(&rhs).map_err(|e| CaeError::convergence(format!("Schur-ratio preflight: {e}")))?;
            let mut v = vec![0.0; n_b];
            for i in 0..n_t {
                if free[i] {
                    v[i] = x[i];
                }
            }
            next.push(v);
        }

        let mut ortho: Vec<Vec<f64>> = Vec::with_capacity(k);
        for mut v in next {
            for q in &ortho {
                let mq = mass_action(q);
                let proj: f64 = v.iter().zip(&mq).map(|(a, b)| a * b).sum();
                for (a, b) in v.iter_mut().zip(q) {
                    *a -= proj * b;
                }
            }
            let mv = mass_action(&v);
            let norm: f64 = v.iter().zip(&mv).map(|(a, b)| a * b).sum::<f64>().abs().sqrt();
            if norm > 0.0 && norm.is_finite() {
                for a in &mut v {
                    *a /= norm;
                }
                ortho.push(v);
            }
        }
        if ortho.is_empty() {
            return Err(CaeError::contract(
                "Schur-ratio preflight: the solid has no free inertial degrees of freedom",
            ));
        }
        modes = ortho;
    }
    let ratios = stepper.interface_schur_ratio(design, &z0, &modes, &mass_action)?;
    Ok(json!({"ratios": ratios, "limit": model.problem.coupling.schur_ratio_limit,
        "mode": model.problem.coupling.mode.name(), "modes": modes.len(),
        "rule": "FSI_DYNAMIC_TOPOLOGY.md 3.5 rule 1: loose coupling refused above the limit"}))
}

fn is_loose(model: &FsiModel) -> bool {
    matches!(model.problem.coupling.mode, implexity_solve::multirate_coupling::CouplingMode::Loose { .. })
}

fn work_defect<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    rows: &[BTreeMap<String, f64>],
    record: bool,
) -> CaeResult<Value> {
    let defect: f64 = rows.iter().filter_map(|r| r.get(LEDGER_INTERFACE_WORK_DEFECT)).sum();
    let work: f64 = rows.iter().filter_map(|r| r.get(LEDGER_INTERFACE_WORK_B)).map(|w| w.abs()).sum();
    let scale = model.problem.coupling.energy_scale_j.unwrap_or(work);
    if !(scale > 0.0 && scale.is_finite()) {
        return Ok(json!({"cumulative_defect_J": defect, "energy_scale_J": scale, "relative": null,
            "note": "no interface work exchanged; the rule is vacuous"}));
    }
    let relative = match stepper.admit_work_defect(defect, scale) {
        Ok(r) => r,

        Err(e) if record && model.problem.coupling.work_defect_action_record => {
            return Ok(json!({"cumulative_defect_J": defect, "energy_scale_J": scale,
                "relative": defect.abs() / scale, "limit": model.problem.coupling.work_defect_limit,
                "admitted": false, "refusal": e.message(),
                "note": "coupling.work_defect_action = record: the loose run failed the work-defect rule; its values are reported, not admitted"}));
        }
        Err(e) => return Err(e),
    };
    Ok(json!({"cumulative_defect_J": defect, "energy_scale_J": scale, "relative": relative, "admitted": true,
        "limit": model.problem.coupling.work_defect_limit,
        "energy_scale_source": if model.problem.coupling.energy_scale_j.is_some() { "coupling.energy_scale_j" } else { "cumulative absolute interface work" }}))
}

fn periodic_options(model: &FsiModel, budget: &StoreBudget) -> PeriodicOptions {
    let t = &model.problem.time;
    PeriodicOptions {
        steps_per_period: t.steps_per_period,
        method: t.method,
        tolerance: t.tolerance,
        adjoint_tolerance: t.adjoint_tolerance,
        max_periods: t.max_periods,
        spin_up_periods: t.spin_up_periods,
        stability_margin: t.stability_margin,
        floquet_modes: t.floquet_modes.max(MIN_ARNOLDI_MODES),
        checkpoint: t.checkpoint.clone(),
        budget: budget.clone(),
    }
}

fn period_kind<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    settings: &RunSettings,
) -> CaeResult<(PeriodKind, Option<PeriodicGuess>)> {
    let t = &model.problem.time;
    match &t.kind {
        TimeKind::PeriodicForced => Ok((PeriodKind::Forced { period_s: t.period_s }, settings.guess.clone())),
        TimeKind::PeriodicAutonomous { phase, bounds_s } => {
            let (phase, guess) = match phase {
                PhaseSpec::Section { sample, level } => {
                    let index = stepper.sample_names().iter().position(|n| n == sample).ok_or_else(|| {
                        CaeError::contract(format!("phase sample {sample:?} is not a stepper sample"))
                    })?;
                    (PhaseCondition::Section { sample: index, level: *level }, settings.guess.clone())
                }
                PhaseSpec::Integral => {

                    let z0 = stepper.initial_state(design)?;
                    let steps = t.spin_up_periods.max(1) * t.steps_per_period;
                    let run = run_history(
                        stepper,
                        StepParameters { design, time_scale: 1.0 },
                        &z0,
                        1,
                        Some(steps),
                        None,
                        implexity_solve::checkpointed_history::CheckpointPolicy::Binomial {
                            ram_snapshots: 2,
                            disk_snapshots: 0,
                        },
                        &settings.budget,
                    )?;
                    let reference = run.final_state().to_vec();
                    let guess = settings
                        .guess
                        .clone()
                        .or_else(|| Some(PeriodicGuess { state: reference.clone(), period_s: t.period_s }));
                    (PhaseCondition::Integral { reference }, guess)
                }
            };
            Ok((
                PeriodKind::Autonomous { phase, period_guess_s: t.period_s, period_bounds_s: *bounds_s },
                guess,
            ))
        }
        _ => Err(CaeError::contract("internal: period kind of a non-periodic problem")),
    }
}

fn program_values(
    model: &FsiModel,
    samples: &DenseMatrix,
    step_s: f64,
    periodic: bool,
    autonomous: bool,
    design: &[f64],
) -> CaeResult<BTreeMap<String, f64>> {
    let mut out = BTreeMap::new();
    for (name, v) in model.problem.program.evaluate(samples, step_s, periodic, autonomous)? {
        out.insert(name, v.value);
    }
    for term in model.problem.program.design_terms() {
        out.insert(term.name.clone(), design_term(model, &term.name, design)?.0);
    }
    Ok(out)
}

fn orbit_result(
    model: &FsiModel,
    orbit: &PeriodicOrbit,
    design: &[f64],
    autonomous: bool,
    extra: &Value,
) -> CaeResult<DynamicResult> {
    let values = program_values(model, &orbit.samples, orbit.step_s(), true, autonomous, design)?;
    let time_scale = orbit.run().time_scale();
    Ok(DynamicResult {
        values,
        samples: orbit.samples.clone(),
        sample_names: model.problem.observables.names(),
        step_s: orbit.step_s(),
        period_s: orbit.period_s,
        regime: if autonomous { "periodic_autonomous" } else { "periodic_forced" }.into(),
        derivative_scope: PERIODIC_ORBIT_EXACT.into(),
        start_state: orbit.initial_state.clone(),
        first_step: 1,
        time_scale,
        certificate: json!({"orbit": orbit.certificate, "admission": extra}),
        ledger: ledger_summary(orbit.run().ledger()),
        guess: Some(PeriodicGuess::from_orbit(orbit)),
    })
}


pub fn evaluate<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    settings: &RunSettings,
) -> CaeResult<DynamicResult> {
    evaluate_inner(model, stepper, design, settings).map_err(|e| with_integrator_hint(model, e))
}

fn evaluate_inner<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    settings: &RunSettings,
) -> CaeResult<DynamicResult> {
    let t = &model.problem.time;
    let admission = if is_loose(model) { schur_preflight(model, stepper, design)? } else { Value::Null };
    match &t.kind {
        TimeKind::FixedHorizon { autonomous, .. } => {
            let z0 = stepper.initial_state(design)?;
            let steps = t.history_steps();
            let run = run_history(
                stepper,
                StepParameters { design, time_scale: 1.0 },
                &z0,
                1,
                Some(steps),
                None,
                t.checkpoint.clone(),
                &settings.budget,
            )?;
            let defect =
                if is_loose(model) { work_defect(model, stepper, run.ledger(), true)? } else { Value::Null };
            let values = program_values(model, run.samples(), t.macro_step_s(), false, *autonomous, design)?;
            Ok(DynamicResult {
                values,
                samples: run.samples().clone(),
                sample_names: model.problem.observables.names(),
                step_s: t.macro_step_s(),
                period_s: t.macro_step_s() * steps as f64,
                regime: "transient".into(),
                derivative_scope: DISCRETE_HISTORY_EXACT.into(),
                start_state: z0,
                first_step: 1,
                time_scale: 1.0,
                certificate: json!({"history": run.record(), "schur_preflight": admission, "work_defect": defect}),
                ledger: ledger_summary(run.ledger()),
                guess: None,
            })
        }
        TimeKind::PeriodicForced | TimeKind::PeriodicAutonomous { .. } => {
            let (kind, guess) = period_kind(model, stepper, design, settings)?;
            let options = periodic_options(model, &settings.budget);
            let orbit = solve_periodic(stepper, design, &kind, &options, guess.as_ref())?;
            let defect = if is_loose(model) {
                work_defect(model, stepper, orbit.run().ledger(), true)?
            } else {
                Value::Null
            };
            orbit_result(
                model,
                &orbit,
                design,
                kind.is_autonomous(),
                &json!({"schur_preflight": admission, "work_defect": defect}),
            )
        }
        TimeKind::SteadyStability { modes, tolerance, spin_up_steps } => {
            steady_analysis(model, stepper, design, (*modes, *tolerance, *spin_up_steps), &admission)
                .map(|(r, _, _)| r)
        }
    }
}

fn single_period_gradient<'m,B: SolidView<'m>>(
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    pick: HorizonResponses<'_>,
    options: &FixedHorizonOptions,
) -> CaeResult<FixedHorizonGradient> {
    let p = StepParameters { design, time_scale: options.time_scale };
    let z0 = stepper.initial_state(design)?;
    let run = run_history(
        stepper,
        p,
        &z0,
        1,
        Some(options.steps_per_period),
        None,
        options.checkpoint.clone(),
        &options.budget,
    )?;
    let picked = pick(run.samples())?;
    let bars: Vec<DenseMatrix> = picked.iter().map(|(_, d)| d.clone()).collect();
    let finals = vec![vec![0.0; stepper.state_size()]; bars.len()];
    let grads = history_adjoint_many(stepper, p, &run, &bars, &finals)?;
    let mut out = FixedHorizonGradient {
        values: picked.iter().map(|(v, _)| *v).collect(),
        design: Vec::with_capacity(grads.len()),
        initial_state: Vec::with_capacity(grads.len()),
        time_scale: Vec::with_capacity(grads.len()),
        samples: run.samples().clone(),
        final_state: run.final_state().to_vec(),
        adjoint_growth_per_period: f64::NAN,
        adjoint_norms: Vec::new(),
        window_check: None,
        certificate: json!({"schema": "implexity-fsi-single-period-gradient/1", "history": run.record(),
            "derivative_scope": DISCRETE_HISTORY_EXACT,
            "adjoint_growth_monitor": "not applicable: a single-period horizon has no per-period growth statistic (FSI_DYNAMIC_TOPOLOGY.md 4.8 (c) needs at least two periods)"}),
    };
    for g in grads {
        let mut d = g.design.clone();
        let init = stepper.initial_state_vjp(design, &g.initial_state)?;
        for (a, b) in d.iter_mut().zip(&init) {
            *a += b;
        }
        out.design.push(d);
        out.initial_state.push(g.initial_state);
        out.time_scale.push(g.time_scale);
    }
    Ok(out)
}

fn steady_analysis<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    (modes, tolerance, spin_up_steps): (usize, f64, usize),
    admission: &Value,
) -> CaeResult<(DynamicResult, SteadyState, StepMode)> {
    let t = &model.problem.time;

    let mut z0 = stepper.initial_state(design)?;
    for _ in 0..spin_up_steps {
        z0 = stepper.advance(1, &z0, StepParameters { design, time_scale: 1.0 })?.state;
    }
    let steady = steady_state(stepper, design, &z0, tolerance)?;

    let found = dominant_modes(stepper, design, &steady, modes.max(MIN_ARNOLDI_MODES), tolerance)?;
    let least = found
        .iter()
        .max_by(|a, b| a.growth_rate_per_s.total_cmp(&b.growth_rate_per_s))
        .ok_or_else(|| CaeError::convergence("steady stability: no mode was computed"))?
        .clone();
    let rec = stepper.advance(1, &steady.state, StepParameters { design, time_scale: 1.0 })?;
    let ns = rec.samples.len();
    let samples = DenseMatrix::new(1, ns, rec.samples.clone())
        .map_err(|e| CaeError::contract(format!("steady samples: {e}")))?;
    let mut values = program_values(model, &samples, t.macro_step_s(), true, false, design)?;
    values.insert(STABILITY_RESPONSES[0].into(), least.growth_rate_per_s);
    values.insert(STABILITY_RESPONSES[1].into(), least.angular_frequency_rad_s);
    let spectrum: Vec<Value> = found
        .iter()
        .map(|m| {
            json!({"multiplier": [m.multiplier.re, m.multiplier.im], "growth_rate_per_s": m.growth_rate_per_s,
            "angular_frequency_rad_s": m.angular_frequency_rad_s, "condition": m.condition, "gate": m.gate})
        })
        .collect();
    let result = DynamicResult {
        values,
        samples,
        sample_names: model.problem.observables.names(),
        step_s: t.macro_step_s(),
        period_s: t.macro_step_s(),
        regime: "steady".into(),
        derivative_scope: STABILITY_EIGENVALUE_EXACT.into(),
        start_state: steady.state.clone(),
        first_step: 1,
        time_scale: 1.0,
        certificate: json!({"steady_state": steady.certificate, "modes": spectrum, "schur_preflight": admission.clone()}),
        ledger: ledger_summary(std::slice::from_ref(&rec.ledger)),
        guess: None,
    };
    Ok((result, steady, least))
}

fn steady_gradients<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    names: &[String],
    settings: (usize, f64, usize),
) -> CaeResult<(DynamicResult, DynamicGradient)> {
    let mut design_names = Vec::new();
    let mut growth = false;
    for name in names {
        if name == STABILITY_RESPONSES[0] {
            growth = true;
            continue;
        }
        let (kernel, design_terms) = split_names(model, std::slice::from_ref(name))
            .unwrap_or_else(|_| (vec![name.clone()], Vec::new()));
        if !kernel.is_empty() {
            return Err(CaeError::contract(format!(
                "steady_stability gradients exist for growth_rate_per_s (stability_eigenvalue_exact) and design terms; {name:?} is refused (steady-state sample responses and the angular frequency have no exact gradient here)"
            )));
        }
        design_names.extend(design_terms);
    }
    if growth && stepper.second_order().is_none() {
        return Err(CaeError::contract(
            "steady_stability gradients are refused for this problem: the growth-rate gradient needs the second-order capability of both coupled fields, which interface_power samples, the cumulant collision, the WALE closure, the Ogden law and nodal potentials without third derivatives do not provide",
        ));
    }
    let admission = if is_loose(model) { schur_preflight(model, stepper, design)? } else { Value::Null };
    let (result, steady, least) = steady_analysis(model, stepper, design, settings, &admission)?;
    let mut values = BTreeMap::new();
    if growth {
        let g = growth_rate_gradient(stepper, design, &steady, &least)?;
        values.insert(STABILITY_RESPONSES[0].to_string(), (least.growth_rate_per_s, g));
    }
    for name in &design_names {
        values.insert(name.clone(), design_term(model, name, design)?);
    }
    let certificate = json!({"mode": {"multiplier": [least.multiplier.re, least.multiplier.im],
        "condition": least.condition, "gate": least.gate}});
    Ok((
        result,
        DynamicGradient {
            values,
            derivative_scope: STABILITY_EIGENVALUE_EXACT.into(),
            authoritative: true,
            certificate,
        },
    ))
}

fn split_names(model: &FsiModel, names: &[String]) -> CaeResult<(Vec<String>, Vec<String>)> {
    let mut kernel = Vec::new();
    let mut design = Vec::new();
    for name in names {
        let term = model.problem.program.terms().iter().find(|t| &t.name == name).ok_or_else(|| {
            CaeError::contract(format!(
                "unknown response {name:?} (program terms: {})",
                model.problem.responses().join(", ")
            ))
        })?;
        match term.quantity {
            TermQuantity::Design { .. } => design.push(name.clone()),
            _ => kernel.push(name.clone()),
        }
    }
    Ok((kernel, design))
}


pub fn gradients<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    names: &[String],
    settings: &RunSettings,
) -> CaeResult<(DynamicResult, DynamicGradient)> {
    gradients_inner(model, stepper, design, names, settings).map_err(|e| with_integrator_hint(model, e))
}

#[allow(clippy::too_many_lines)]
fn gradients_inner<'m,B: SolidView<'m>>(
    model: &FsiModel,
    stepper: &MultirateStepper<FluidField, B>,
    design: &[f64],
    names: &[String],
    settings: &RunSettings,
) -> CaeResult<(DynamicResult, DynamicGradient)> {
    let t = &model.problem.time;
    if let TimeKind::SteadyStability { modes, tolerance, spin_up_steps } = &t.kind {
        return steady_gradients(model, stepper, design, names, (*modes, *tolerance, *spin_up_steps));
    }
    let (kernel_names, design_names) = split_names(model, names)?;
    let program = &model.problem.program;
    let add_design_terms = |out: &mut BTreeMap<String, (f64, Vec<f64>)>| -> CaeResult<()> {
        for name in &design_names {
            out.insert(name.clone(), design_term(model, name, design)?);
        }
        Ok(())
    };
    match &t.kind {
        TimeKind::SteadyStability { modes, tolerance, spin_up_steps } => {
            steady_gradients(model, stepper, design, names, (*modes, *tolerance, *spin_up_steps))
        }
        TimeKind::FixedHorizon { periods, autonomous } => {
            let admission =
                if is_loose(model) { schur_preflight(model, stepper, design)? } else { Value::Null };
            let dt = t.macro_step_s();
            let autonomous = *autonomous;
            let pick = |samples: &DenseMatrix| -> CaeResult<Vec<(f64, DenseMatrix)>> {
                let all = program.evaluate(samples, dt, false, autonomous)?;
                kernel_names
                    .iter()
                    .map(|n| {
                        all.iter()
                            .find(|(k, _)| k == n)
                            .map(|(_, v)| (v.value, v.d_samples.clone()))
                            .ok_or_else(|| CaeError::contract(format!("response {n:?} was not evaluated")))
                    })
                    .collect()
            };
            let options = FixedHorizonOptions {
                periods: *periods,
                steps_per_period: t.steps_per_period,
                time_scale: 1.0,
                max_adjoint_growth: t.max_adjoint_growth,
                window_check_tolerance: t.window_check_tolerance,
                checkpoint: t.checkpoint.clone(),
                budget: settings.budget.clone(),
            };
            let outcome = if *periods == 1 {
                single_period_gradient(stepper, design, &pick, &options)
            } else {
                fixed_horizon_gradient(stepper, design, None, &pick, &options)
            };
            let (grad, scope, authoritative, cert) = match outcome {
                Ok(g) => {
                    let cert = g.certificate.clone();
                    (g, DISCRETE_HISTORY_EXACT, true, cert)
                }
                Err(e) if regime_record(&e) == Some(NONPERIODIC_REGIME_RECORD) && t.allow_biased_gradient => {
                    let members = (*periods / 2).max(2);
                    let eo = EnsembleOptions {
                        members,
                        window_periods: 1,
                        spacing_periods: 1,
                        spin_up_periods: periods.saturating_sub(members + 1),
                        steps_per_period: t.steps_per_period,
                        time_scale: 1.0,
                        checkpoint: t.checkpoint.clone(),
                        budget: settings.budget.clone(),
                    };
                    let e_pick = |samples: &DenseMatrix| pick(samples);
                    let ens = ensemble_window_gradient(stepper, design, None, &e_pick, &eo)?;
                    let mut out = BTreeMap::new();
                    for (i, n) in kernel_names.iter().enumerate() {
                        out.insert(n.clone(), (ens.values[i], ens.design[i].clone()));
                    }
                    add_design_terms(&mut out)?;
                    let result = evaluate(model, stepper, design, settings)?;
                    return Ok((
                        result,
                        DynamicGradient {
                            values: out,
                            derivative_scope: BIASED_ESTIMATE.into(),
                            authoritative: false,
                            certificate: json!({"refusal": e.message(), "ensemble": ens.certificate,
                                "standard_error": ens.standard_error}),
                        },
                    ));
                }
                Err(e) => return Err(e),
            };
            let mut out = BTreeMap::new();
            for (i, n) in kernel_names.iter().enumerate() {
                out.insert(n.clone(), (grad.values[i], grad.design[i].clone()));
            }
            add_design_terms(&mut out)?;
            let mut values = program_values(model, &grad.samples, dt, false, autonomous, design)?;
            for (k, (v, _)) in &out {
                values.insert(k.clone(), *v);
            }
            let result = DynamicResult {
                values,
                samples: grad.samples.clone(),
                sample_names: model.problem.observables.names(),
                step_s: dt,
                period_s: dt * t.history_steps() as f64,
                regime: "transient".into(),
                derivative_scope: scope.into(),
                start_state: stepper.initial_state(design)?,
                first_step: 1,
                time_scale: 1.0,
                certificate: json!({"schur_preflight": admission}),
                ledger: Value::Null,
                guess: None,
            };
            Ok((
                result,
                DynamicGradient {
                    values: out,
                    derivative_scope: scope.into(),
                    authoritative,
                    certificate: json!({"fixed_horizon": cert, "adjoint_growth_per_period": grad.adjoint_growth_per_period,
                        "window_check": grad.window_check}),
                },
            ))
        }
        TimeKind::PeriodicForced | TimeKind::PeriodicAutonomous { .. } => {
            let admission =
                if is_loose(model) { schur_preflight(model, stepper, design)? } else { Value::Null };
            let (kind, guess) = period_kind(model, stepper, design, settings)?;
            let options = periodic_options(model, &settings.budget);
            let orbit = solve_periodic(stepper, design, &kind, &options, guess.as_ref())?;
            let autonomous = kind.is_autonomous();
            let all = program.evaluate(&orbit.samples, orbit.step_s(), true, autonomous)?;
            let mut bars = Vec::new();
            let mut period_bars = Vec::new();
            let mut picked = Vec::new();
            for n in &kernel_names {
                let (_, v) = all
                    .iter()
                    .find(|(k, _)| k == n)
                    .ok_or_else(|| CaeError::contract(format!("response {n:?} was not evaluated")))?;
                bars.push(v.d_samples.clone());
                period_bars.push(v.d_period_s);
                picked.push((n.clone(), v.value));
            }
            let mut out = BTreeMap::new();
            let mut cert = Value::Null;
            if !bars.is_empty() {
                let grads =
                    periodic_adjoint_many(stepper, design, &kind, &orbit, &bars, &period_bars, &options)?;
                for ((n, v), g) in picked.into_iter().zip(grads) {
                    cert = g.certificate.clone();
                    out.insert(n, (v, g.design));
                }
            }
            add_design_terms(&mut out)?;
            let defect = if is_loose(model) {
                work_defect(model, stepper, orbit.run().ledger(), false)?
            } else {
                Value::Null
            };
            let result = orbit_result(
                model,
                &orbit,
                design,
                autonomous,
                &json!({"schur_preflight": admission, "work_defect": defect}),
            )?;
            Ok((
                result,
                DynamicGradient {
                    values: out,
                    derivative_scope: PERIODIC_ORBIT_EXACT.into(),
                    authoritative: true,
                    certificate: json!({"periodic_adjoint": cert}),
                },
            ))
        }
    }
}
