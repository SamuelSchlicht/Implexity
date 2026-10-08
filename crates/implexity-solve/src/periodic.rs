// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


pub mod arnoldi;
pub mod krylov;
pub mod regime;

mod adjoint;
mod period_map;
mod shooting;
mod subspace;

use implexity_core::contracts::MatchingTimeNewtonGuess;
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use serde_json::{Map, Value, json};

use crate::checkpointed_history::{CheckpointPolicy, HistoryRun};
use crate::state_store::StoreBudget;
use crate::time_stepper::TimeStepper;

pub use adjoint::periodic_adjoint_many;
pub use shooting::{MAX_NEWTON_ITERATIONS, TRIVIAL_MULTIPLIER_RADIUS, solve_periodic};

pub const PERIODIC_ORBIT_CERTIFICATE_SCHEMA: &str = "implexity-periodic-orbit-certificate/1";
pub const PERIODIC_ADJOINT_CERTIFICATE_SCHEMA: &str = "implexity-periodic-adjoint-certificate/1";
pub const UNSTABLE_ORBIT_RECORD: &str = "periodic_orbit_unstable";
pub const NONPERIODIC_REGIME_RECORD: &str = "nonperiodic_regime";
pub const PERIODIC_ORBIT_EXACT: &str = "periodic_orbit_exact";
pub const PERIODIC_GUESS_ROLE: &str = "periodic_orbit_seed";

#[derive(Clone, Debug, PartialEq)]
pub enum PhaseCondition {
    Section {
        sample: usize,
        level: f64,
    },
    Integral {
        reference: Vec<f64>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PeriodKind {
    Forced {
        period_s: f64,
    },
    Autonomous {
        phase: PhaseCondition,
        period_guess_s: f64,
        period_bounds_s: (f64, f64),
    },
}

impl PeriodKind {
    #[must_use]
    pub fn is_autonomous(&self) -> bool {
        matches!(self, Self::Autonomous { .. })
    }

    fn validate(&self, stepper: &dyn TimeStepper) -> CaeResult<()> {
        match self {
            Self::Forced { period_s } => {
                if !(period_s.is_finite() && *period_s > 0.0) {
                    return Err(CaeError::contract("forced period must be finite and positive"));
                }
            }
            Self::Autonomous { phase, period_guess_s, period_bounds_s } => {
                let (lo, hi) = *period_bounds_s;
                if !(lo.is_finite() && hi.is_finite() && lo > 0.0 && lo < hi) {
                    return Err(CaeError::contract(
                        "autonomous period bounds must satisfy 0 < lower < upper",
                    ));
                }
                if !(period_guess_s.is_finite() && *period_guess_s > lo && *period_guess_s < hi) {
                    return Err(CaeError::contract(
                        "autonomous period guess must lie strictly inside its bounds",
                    ));
                }
                match phase {
                    PhaseCondition::Section { sample, level } => {
                        if *sample >= stepper.sample_names().len() || !level.is_finite() {
                            return Err(CaeError::contract(format!(
                                "section phase condition names sample {sample} of {} with a finite level",
                                stepper.sample_names().len()
                            )));
                        }
                    }
                    PhaseCondition::Integral { reference } => {
                        if reference.len() != stepper.state_size() || !reference.iter().all(|v| v.is_finite())
                        {
                            return Err(CaeError::contract(
                                "integral phase condition reference must be a finite state vector",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PeriodicMethod {
    Picard,
    NewtonKrylov {
        krylov_dimension: usize,
    },
    NewtonPicard {
        subspace: usize,
    },
}

impl PeriodicMethod {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Picard => "picard",
            Self::NewtonKrylov { .. } => "newton_krylov",
            Self::NewtonPicard { .. } => "newton_picard",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PeriodicOptions {
    pub steps_per_period: usize,
    pub method: PeriodicMethod,
    pub tolerance: f64,
    pub adjoint_tolerance: f64,
    pub max_periods: usize,
    pub spin_up_periods: usize,
    pub stability_margin: f64,
    pub floquet_modes: usize,
    pub checkpoint: CheckpointPolicy,
    pub budget: StoreBudget,
}

impl PeriodicOptions {
    fn validate(&self, kind: &PeriodKind, state_size: usize) -> CaeResult<()> {
        if self.steps_per_period == 0 {
            return Err(CaeError::contract("steps_per_period must be positive"));
        }
        for (name, v) in [("tolerance", self.tolerance), ("adjoint_tolerance", self.adjoint_tolerance)] {
            if !(v.is_finite() && v > 0.0 && v < 1.0) {
                return Err(CaeError::contract(format!("periodic {name} must lie in (0, 1)")));
            }
        }
        if !(self.stability_margin.is_finite() && (0.0..1.0).contains(&self.stability_margin)) {
            return Err(CaeError::contract("periodic stability margin must lie in [0, 1)"));
        }
        if self.max_periods == 0 {
            return Err(CaeError::contract("periodic max_periods must be positive"));
        }
        let minimum = if kind.is_autonomous() { 2 } else { 1 };
        if self.floquet_modes < minimum || self.floquet_modes > state_size {
            return Err(CaeError::contract(format!(
                "floquet_modes must lie in [{minimum}, {state_size}] for this orbit (the stability gate needs them)"
            )));
        }
        match self.method {
            PeriodicMethod::Picard if kind.is_autonomous() => Err(CaeError::contract(
                "Picard iteration has no period update and is refused for autonomous orbits: use newton_krylov or newton_picard shooting, or a fixed-horizon history with a smooth window (windowing theorem, §4.4)",
            )),
            PeriodicMethod::NewtonKrylov { krylov_dimension: 0 } => {
                Err(CaeError::contract("newton_krylov krylov_dimension must be positive"))
            }
            PeriodicMethod::NewtonPicard { subspace } if subspace == 0 || subspace > state_size => {
                Err(CaeError::contract(format!("newton_picard subspace must lie in [1, {state_size}]")))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Multiplier {
    pub re: f64,
    pub im: f64,
}

impl Multiplier {
    #[must_use]
    pub fn modulus(self) -> f64 {
        self.re.hypot(self.im)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeriodicGuess {
    pub state: Vec<f64>,
    pub period_s: f64,
}

impl PeriodicGuess {
    #[must_use]
    pub fn from_orbit(orbit: &PeriodicOrbit) -> Self {
        Self { state: orbit.initial_state.clone(), period_s: orbit.period_s }
    }



    pub fn to_newton_guess(
        &self,
        provider_identity: Map<String, Value>,
        mut provenance: Map<String, Value>,
    ) -> CaeResult<MatchingTimeNewtonGuess> {
        provenance.insert("role".into(), json!(PERIODIC_GUESS_ROLE));
        MatchingTimeNewtonGuess::new(
            vec![self.state.clone(), vec![self.period_s]],
            provider_identity,
            provenance,
        )
    }



    pub fn from_newton_guess(guess: &MatchingTimeNewtonGuess, state_size: usize) -> CaeResult<Self> {
        let role = guess.provenance().get("role").and_then(Value::as_str);
        let states = guess.states();
        if role != Some(PERIODIC_GUESS_ROLE) || states.len() != 2 || states[1].len() != 1 {
            return Err(CaeError::contract("matching-time Newton guess does not hold a periodic orbit seed"));
        }
        if states[0].len() != state_size {
            return Err(CaeError::contract(format!(
                "periodic orbit seed has {} states, the stepper {state_size}",
                states[0].len()
            )));
        }
        let period_s = states[1][0];
        if !(period_s.is_finite() && period_s > 0.0) {
            return Err(CaeError::contract("periodic orbit seed period must be finite and positive"));
        }
        Ok(Self { state: states[0].to_vec(), period_s })
    }
}

#[derive(Debug)]
pub struct PeriodicOrbit {
    pub initial_state: Vec<f64>,
    pub period_s: f64,
    pub samples: DenseMatrix,
    pub multipliers: Vec<Multiplier>,
    pub residual: f64,
    pub periods_run: usize,
    pub tangent_periods_run: usize,
    pub certificate: Value,
    run: HistoryRun,
    phase: shooting::PhaseData,
}

impl PeriodicOrbit {
    #[must_use]
    pub fn run(&self) -> &HistoryRun {
        &self.run
    }

    #[must_use]
    pub fn step_s(&self) -> f64 {
        self.period_s / self.samples.nrows.max(1) as f64
    }
}

#[derive(Clone, Debug)]
pub struct PeriodicGradient {
    pub design: Vec<f64>,
    pub period_s: Option<Vec<f64>>,
    pub certificate: Value,
}

#[must_use]
pub fn regime_record(error: &CaeError) -> Option<&'static str> {
    let message = error.message();
    [UNSTABLE_ORBIT_RECORD, NONPERIODIC_REGIME_RECORD]
        .into_iter()
        .find(|record| message.starts_with(&format!("{record}:")))
}

pub(crate) fn residual_scale(a: &[f64], b: &[f64]) -> f64 {
    let s = krylov::norm(a).max(krylov::norm(b));
    if s > 0.0 { s } else { 1.0 }
}

pub(crate) fn nonperiodic(message: impl std::fmt::Display) -> CaeError {
    CaeError::convergence(format!("{NONPERIODIC_REGIME_RECORD}: {message}"))
}
