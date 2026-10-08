// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use implexity_core::CaeError;
use implexity_linalg::dense::DenseMatrix;
use implexity_solve::checkpointed_history::CheckpointPolicy;
use implexity_solve::dynamic_program::{self, DynamicProgram, TermQuantity};
use implexity_solve::periodic::{
    PeriodKind, PeriodicGuess, PeriodicMethod, PeriodicOptions, PeriodicOrbit, PhaseCondition,
    periodic_adjoint_many, solve_periodic,
};
use implexity_solve::state_store::StoreBudget;
use implexity_solve::time_stepper::{StepParameters, TimeStepper};

use super::field::{DesignLayout, SoftSolidStepper, SoftStepConfig, SoftStepCore};
use super::observables::{Observables, SolidObservable};
use super::pushforward::{BlockingMap, PushforwardPoints};
use crate::soft::problem::{Case, number, number_or, object, only};
use crate::util::contract;

pub const PERIODIC_KEYS: [&str; 11] = [
    "observables",
    "autonomous",
    "responses",
    "method",
    "tolerance",
    "adjoint_tolerance",
    "spin_up_periods",
    "max_periods",
    "stability_margin",
    "floquet_modes",
    "checkpoint",
];

pub const DESIGN_VOLUME: &str = "design_volume_fraction";

pub const OBSERVABLE_KINDS: [&str; 6] = [
    "probe_displacement",
    "probe_separation",
    "plane_gap",
    "strain_energy",
    "kinetic_energy",
    "stress_aggregate",
];

#[derive(Debug, Clone, PartialEq)]
pub enum AutonomousPhase {
    Section {
        sample: String,
        level: f64,
    },
    Integral,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutonomousSpec {
    pub phase: AutonomousPhase,
    pub period_guess_s: f64,
    pub period_bounds_s: (f64, f64),
}

#[derive(Debug, Clone)]
pub struct PeriodicSolid {
    pub observables: Vec<(String, SolidObservable)>,
    pub program: DynamicProgram,
    pub options: PeriodicOptions,
    pub period_s: f64,
    pub autonomous: Option<AutonomousSpec>,
}

#[derive(Debug, Clone)]
pub struct PeriodicEvaluation {
    pub responses: BTreeMap<String, f64>,
    pub cycles: BTreeMap<String, Vec<f64>>,
    pub initial_state: Vec<f64>,
    pub certificate: Value,
}

fn usize_of(v: Option<&Value>, default: usize, what: &str) -> Result<usize, CaeError> {
    match v {
        None => Ok(default),
        Some(x) => x
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| CaeError::contract(format!("{what} must be a nonnegative integer"))),
    }
}

fn triple(v: Option<&Value>, what: &str) -> Result<[f64; 3], CaeError> {
    let a = v
        .and_then(Value::as_array)
        .filter(|a| a.len() == 3)
        .ok_or_else(|| CaeError::contract(format!("{what} must hold three numbers")))?;
    Ok([number(a.first(), what)?, number(a.get(1), what)?, number(a.get(2), what)?])
}


pub fn parse_observables(v: &Value) -> Result<Vec<(String, SolidObservable)>, CaeError> {
    let list = v
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 64)
        .ok_or_else(|| CaeError::contract("periodic.observables must be a list of 1..64 observables"))?;
    list.iter()
        .map(|o| {
            let m = object(o, "periodic observable")?;
            let name = m
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CaeError::contract("periodic observables need a nonempty name"))?
                .to_string();
            let kind = m.get("kind").and_then(Value::as_str).unwrap_or_default();
            let obs = match kind {
                "probe_displacement" => {
                    only(m, &["name", "kind", "point_m", "component"], "probe_displacement")?;
                    SolidObservable::ProbeDisplacement {
                        point_m: triple(m.get("point_m"), "probe_displacement.point_m")?,
                        component: usize_of(m.get("component"), 0, "probe_displacement.component")?,
                    }
                }
                "probe_separation" => {
                    only(m, &["name", "kind", "point_a_m", "point_b_m", "component"], "probe_separation")?;
                    SolidObservable::ProbeSeparation {
                        point_a_m: triple(m.get("point_a_m"), "probe_separation.point_a_m")?,
                        point_b_m: triple(m.get("point_b_m"), "probe_separation.point_b_m")?,
                        component: usize_of(m.get("component"), 0, "probe_separation.component")?,
                    }
                }
                "plane_gap" => {
                    only(m, &["name", "kind", "normal", "offset_m", "beta"], "plane_gap")?;
                    SolidObservable::PlaneGap {
                        normal: triple(m.get("normal"), "plane_gap.normal")?,
                        offset_m: number(m.get("offset_m"), "plane_gap.offset_m")?,
                        beta: number(m.get("beta"), "plane_gap.beta")?,
                    }
                }
                "strain_energy" => {
                    only(m, &["name", "kind"], "strain_energy")?;
                    SolidObservable::StrainEnergy
                }
                "kinetic_energy" => {
                    only(m, &["name", "kind"], "kinetic_energy")?;
                    SolidObservable::KineticEnergy
                }
                "stress_aggregate" => {
                    only(m, &["name", "kind", "p"], "stress_aggregate")?;
                    SolidObservable::StressAggregate { p: number_or(m.get("p"), 8.0, "stress_aggregate.p")? }
                }
                _ => {
                    return contract(format!(
                        "periodic observable kind must be one of {}",
                        OBSERVABLE_KINDS.join(", ")
                    ));
                }
            };
            Ok((name, obs))
        })
        .collect()
}

impl PeriodicSolid {

    pub fn parse(v: &Value, case: &Case) -> Result<Self, CaeError> {
        let m = object(v, "periodic")?;
        only(m, &PERIODIC_KEYS, "periodic")?;
        let observables = parse_observables(m.get("observables").unwrap_or(&Value::Null))?;
        let names: Vec<String> = observables.iter().map(|(n, _)| n.clone()).collect();
        let program = dynamic_program::normalise(
            m.get("responses").ok_or_else(|| CaeError::contract("periodic.responses is required"))?,
        )?
        .bind(&names, &[DESIGN_VOLUME])?;
        let steps = case.loading.steps();
        let times = &case.loading.times;
        let dt = times[1] - times[0];
        if times.windows(2).any(|w| ((w[1] - w[0]) - dt).abs() > 1e-12 * dt) {
            return contract("periodic responses need uniform steps (one forcing period)");
        }
        #[allow(clippy::cast_precision_loss)]
        let span_s = steps as f64 * dt;
        let autonomous = match m.get("autonomous") {
            None => None,
            Some(av) => Some(Self::parse_autonomous(av, case, &names, span_s)?),
        };
        program.admit(steps, true, autonomous.is_some())?;
        let method = match m.get("method") {
            None => PeriodicMethod::NewtonKrylov { krylov_dimension: 30 },
            Some(mv) => {
                let o = object(mv, "periodic.method")?;
                match o.get("kind").and_then(Value::as_str).unwrap_or_default() {
                    "picard" => {
                        only(o, &["kind"], "periodic.method")?;
                        PeriodicMethod::Picard
                    }
                    "newton_krylov" => {
                        only(o, &["kind", "krylov_dimension"], "periodic.method")?;
                        PeriodicMethod::NewtonKrylov {
                            krylov_dimension: usize_of(o.get("krylov_dimension"), 30, "krylov_dimension")?,
                        }
                    }
                    "newton_picard" => {
                        only(o, &["kind", "subspace"], "periodic.method")?;
                        PeriodicMethod::NewtonPicard { subspace: usize_of(o.get("subspace"), 4, "subspace")? }
                    }
                    _ => {
                        return contract(
                            "periodic.method.kind must be picard, newton_krylov or newton_picard",
                        );
                    }
                }
            }
        };
        let checkpoint = match m.get("checkpoint") {
            None => CheckpointPolicy::All,
            Some(cv) => {
                let o = object(cv, "periodic.checkpoint")?;
                only(o, &["policy", "ram_snapshots"], "periodic.checkpoint")?;
                match o.get("policy").and_then(Value::as_str).unwrap_or("all") {
                    "all" => CheckpointPolicy::All,
                    "binomial" => CheckpointPolicy::Binomial {
                        ram_snapshots: usize_of(o.get("ram_snapshots"), 8, "ram_snapshots")?,
                        disk_snapshots: 0,
                    },
                    _ => return contract("periodic.checkpoint.policy must be all or binomial"),
                }
            }
        };
        let options = PeriodicOptions {
            steps_per_period: steps,
            method,
            tolerance: number_or(m.get("tolerance"), 1e-10, "periodic.tolerance")?,
            adjoint_tolerance: number_or(m.get("adjoint_tolerance"), 1e-10, "periodic.adjoint_tolerance")?,
            max_periods: usize_of(m.get("max_periods"), 400, "periodic.max_periods")?,
            spin_up_periods: usize_of(m.get("spin_up_periods"), 1, "periodic.spin_up_periods")?,
            stability_margin: number_or(m.get("stability_margin"), 1e-3, "periodic.stability_margin")?,
            floquet_modes: usize_of(m.get("floquet_modes"), 2, "periodic.floquet_modes")?,
            checkpoint,
            budget: StoreBudget::default(),
        };
        let period_s = autonomous.as_ref().map_or(span_s, |a| a.period_guess_s);
        let out = Self { observables, program, options, period_s, autonomous };

        out.core(case)?;
        Ok(out)
    }

    #[allow(clippy::float_cmp)]
    fn parse_autonomous(
        v: &Value,
        case: &Case,
        names: &[String],
        span_s: f64,
    ) -> Result<AutonomousSpec, CaeError> {
        let o = object(v, "periodic.autonomous")?;
        only(o, &["phase", "period_guess_s", "period_bounds_s"], "periodic.autonomous")?;
        let l = &case.loading;
        let constant = |a: &[f64]| a.windows(2).all(|w| w[0] == w[1]);
        if !(constant(&l.force_amplitude)
            && constant(&l.pressure_amplitude)
            && constant(&l.displacement_amplitude)
            && case.prescribed_patterns.iter().all(|(_, a)| constant(a)))
        {
            return contract(
                "autonomous periodic orbits need time-invariant loads (constant force, pressure and displacement amplitudes)",
            );
        }
        let phase = match o.get("phase") {
            None => AutonomousPhase::Integral,
            Some(pv) => {
                let p = object(pv, "periodic.autonomous.phase")?;
                match p.get("kind").and_then(Value::as_str).unwrap_or_default() {
                    "integral" => {
                        only(p, &["kind"], "periodic.autonomous.phase")?;
                        AutonomousPhase::Integral
                    }
                    "section" => {
                        only(p, &["kind", "sample", "level"], "periodic.autonomous.phase")?;
                        let sample = p
                            .get("sample")
                            .and_then(Value::as_str)
                            .filter(|s| names.iter().any(|n| n == s))
                            .ok_or_else(|| {
                                CaeError::contract(
                                    "periodic.autonomous.phase.sample must name a periodic observable",
                                )
                            })?
                            .to_string();
                        AutonomousPhase::Section {
                            sample,
                            level: number_or(p.get("level"), 0.0, "phase.level")?,
                        }
                    }
                    _ => return contract("periodic.autonomous.phase.kind must be section or integral"),
                }
            }
        };
        let guess = number_or(o.get("period_guess_s"), span_s, "periodic.autonomous.period_guess_s")?;
        let bounds = match o.get("period_bounds_s") {
            None => (0.5 * guess, 2.0 * guess),
            Some(b) => {
                let a = b.as_array().filter(|a| a.len() == 2).ok_or_else(|| {
                    CaeError::contract("periodic.autonomous.period_bounds_s must hold [lo, hi]")
                })?;
                (number(a.first(), "period_bounds_s")?, number(a.get(1), "period_bounds_s")?)
            }
        };
        if !(guess.is_finite() && guess > 0.0 && bounds.0 > 0.0 && bounds.0 < guess && guess < bounds.1) {
            return contract(
                "periodic.autonomous needs 0 < period_bounds_s[0] < period_guess_s < period_bounds_s[1]",
            );
        }
        Ok(AutonomousSpec { phase, period_guess_s: guess, period_bounds_s: bounds })
    }

    #[must_use]
    pub fn responses(&self) -> Vec<String> {
        self.program.responses()
    }

    #[must_use]
    pub fn regime(&self) -> &'static str {
        if self.autonomous.is_some() { "periodic_autonomous" } else { "periodic_forced" }
    }

    fn core<'m>(&self, case: &'m Case) -> Result<SoftStepCore<'m>, CaeError> {
        let model = &case.model;
        let points = if self.observables.iter().any(|(_, o)| matches!(o, SolidObservable::PlaneGap { .. })) {
            Some(Arc::new(PushforwardPoints::new(&model.mesh, 2)?))
        } else {
            None
        };
        let observables = Observables::new(model, self.observables.clone(), points, BlockingMap::identity())?;
        let config = SoftStepConfig {
            scheme: case.scheme,
            loading: case.loading.clone(),
            periodic_loading: true,
            rayleigh: case.rayleigh,
            newton: case.newton,
        };
        SoftStepCore::new(model, config, DesignLayout::ElementParams, observables)?
            .with_factorization_reuse(case.factorization_reuse)?
            .with_prescribed_patterns(case.prescribed_patterns.clone())
    }

    fn kind_and_guess(
        &self,
        stepper: &SoftSolidStepper<'_>,
        params: &[f64],
    ) -> Result<(PeriodKind, Option<PeriodicGuess>), CaeError> {
        let Some(spec) = &self.autonomous else {
            return Ok((PeriodKind::Forced { period_s: self.period_s }, None));
        };
        let steps = self.options.steps_per_period;
        let p = StepParameters { design: params, time_scale: 1.0 };
        let mut z = stepper.initial_state(params)?;
        for n in 1..=self.options.spin_up_periods.max(1) * steps {
            z = stepper.advance(n, &z, p)?.state;
        }
        let phase = match &spec.phase {
            AutonomousPhase::Section { sample, level } => PhaseCondition::Section {
                sample: stepper
                    .sample_names()
                    .iter()
                    .position(|n| n == sample)
                    .ok_or_else(|| CaeError::contract("internal: unknown section sample"))?,
                level: *level,
            },
            AutonomousPhase::Integral => PhaseCondition::Integral { reference: z.clone() },
        };
        Ok((
            PeriodKind::Autonomous {
                phase,
                period_guess_s: spec.period_guess_s,
                period_bounds_s: spec.period_bounds_s,
            },
            Some(PeriodicGuess { state: z, period_s: spec.period_guess_s }),
        ))
    }

    fn orbit(
        &self,
        stepper: &SoftSolidStepper<'_>,
        params: &[f64],
    ) -> Result<(PeriodKind, PeriodicOrbit), CaeError> {
        let (kind, guess) = self.kind_and_guess(stepper, params)?;
        let orbit = solve_periodic(stepper, params, &kind, &self.options, guess.as_ref())?;
        Ok((kind, orbit))
    }


    pub fn evaluate(&self, case: &Case, params: &[f64], volume: f64) -> Result<PeriodicEvaluation, CaeError> {
        let stepper = SoftSolidStepper::new(self.core(case)?, "soft_periodic")?;
        let (_, orbit) = self.orbit(&stepper, params)?;
        let mut responses = BTreeMap::new();
        for (name, value) in
            self.program.evaluate(&orbit.samples, orbit.step_s(), true, self.autonomous.is_some())?
        {
            responses.insert(name, value.value);
        }
        for term in self.program.design_terms() {
            responses.insert(term.name.clone(), volume);
        }
        let names = stepper.sample_names().to_vec();
        let mut cycles = BTreeMap::new();
        for (k, name) in names.iter().enumerate() {
            cycles.insert(
                format!("cycle_{name}"),
                (0..orbit.samples.nrows).map(|r| orbit.samples.get(r, k)).collect(),
            );
        }
        Ok(PeriodicEvaluation {
            responses,
            cycles,
            initial_state: stepper.core().to_physical(&orbit.initial_state),
            certificate: json!({"orbit": orbit.certificate, "period_s": orbit.period_s}),
        })
    }


    pub fn gradients(
        &self,
        case: &Case,
        params: &[f64],
        names: &[String],
        volume: (f64, &[f64]),
    ) -> Result<(BTreeMap<String, (f64, Vec<f64>)>, Value), CaeError> {
        let stepper = SoftSolidStepper::new(self.core(case)?, "soft_periodic")?;
        let (kind, orbit) = self.orbit(&stepper, params)?;
        let values =
            self.program.evaluate(&orbit.samples, orbit.step_s(), true, self.autonomous.is_some())?;
        let mut out = BTreeMap::new();
        let mut batch: Vec<(String, f64, DenseMatrix, f64)> = Vec::new();
        for name in names {
            if let Some((_, v)) = values.iter().find(|(n, _)| n == name) {
                batch.push((name.clone(), v.value, v.d_samples.clone(), v.d_period_s));
            } else if self
                .program
                .terms()
                .iter()
                .any(|t| &t.name == name && matches!(t.quantity, TermQuantity::Design { .. }))
            {
                let mut g = volume.1.to_vec();
                g.resize(params.len(), 0.0);
                out.insert(name.clone(), (volume.0, g));
            } else {
                return contract(format!("unknown periodic response {name}"));
            }
        }
        let mut certificate = json!({"orbit": orbit.certificate, "period_s": orbit.period_s});
        if !batch.is_empty() {
            let bars: Vec<DenseMatrix> = batch.iter().map(|(_, _, d, _)| d.clone()).collect();
            let period_bars: Vec<f64> = batch.iter().map(|(_, _, _, t)| *t).collect();
            let grads =
                periodic_adjoint_many(&stepper, params, &kind, &orbit, &bars, &period_bars, &self.options)?;
            for ((name, value, _, _), g) in batch.into_iter().zip(grads) {
                certificate["adjoint"] = g.certificate.clone();
                if let Some(dt) = &g.period_s {
                    certificate["period_gradient_available"] = json!(dt.len() == params.len());
                }
                out.insert(name, (value, g.design));
            }
        }
        Ok((out, certificate))
    }
}
