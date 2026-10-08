// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::CaeResult;
use implexity_solve::checkpointed_history::CheckpointPolicy;
use implexity_solve::periodic::PeriodicMethod;
use implexity_solve::periodic::regime::DEFAULT_MAX_ADJOINT_GROWTH;

use crate::json::{Section, kind_of, refuse};

pub const TIME_KINDS: [&str; 4] =
    ["fixed_horizon", "periodic_forced", "periodic_autonomous", "steady_stability"];

const COMMON: [&str; 3] = ["kind", "checkpoint", "allow_biased_gradient"];

#[derive(Clone, Debug, PartialEq)]
pub enum PhaseSpec {
    Section {
        sample: String,
        level: f64,
    },
    Integral,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TimeKind {
    FixedHorizon {
        periods: usize,
        autonomous: bool,
    },
    PeriodicForced,
    PeriodicAutonomous {
        phase: PhaseSpec,
        bounds_s: (f64, f64),
    },
    SteadyStability {
        modes: usize,
        tolerance: f64,
        spin_up_steps: usize,
    },
}

impl TimeKind {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::FixedHorizon { .. } => "fixed_horizon",
            Self::PeriodicForced => "periodic_forced",
            Self::PeriodicAutonomous { .. } => "periodic_autonomous",
            Self::SteadyStability { .. } => "steady_stability",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TimeSpec {
    pub kind: TimeKind,
    pub steps_per_period: usize,
    pub period_s: f64,
    pub method: PeriodicMethod,
    pub tolerance: f64,
    pub adjoint_tolerance: f64,
    pub spin_up_periods: usize,
    pub max_periods: usize,
    pub stability_margin: f64,
    pub floquet_modes: usize,
    pub max_adjoint_growth: f64,
    pub window_check_tolerance: Option<f64>,
    pub checkpoint: CheckpointPolicy,
    pub allow_biased_gradient: bool,
}

impl TimeSpec {
    #[must_use]
    pub fn macro_step_s(&self) -> f64 {
        self.period_s / self.steps_per_period as f64
    }

    #[must_use]
    pub fn history_steps(&self) -> usize {
        match self.kind {
            TimeKind::FixedHorizon { periods, .. } => periods * self.steps_per_period,
            _ => self.steps_per_period,
        }
    }

    #[must_use]
    pub fn periodic(&self) -> bool {
        matches!(self.kind, TimeKind::PeriodicForced | TimeKind::PeriodicAutonomous { .. })
    }

    #[must_use]
    pub fn autonomous(&self) -> bool {
        matches!(
            self.kind,
            TimeKind::PeriodicAutonomous { .. } | TimeKind::FixedHorizon { autonomous: true, .. }
        )
    }
}

fn unit_interval(x: f64) -> bool {
    x > 0.0 && x < 1.0
}

fn method(s: &mut Section<'_>, autonomous: bool) -> CaeResult<PeriodicMethod> {
    let default = json!({"kind": "newton_krylov"});
    let v = s.raw("method").cloned().unwrap_or(default);
    let path = s.at("method");
    let kind = kind_of(&v, &path, &["picard", "newton_krylov", "newton_picard"])?;
    let (m, normal) = match kind {
        "picard" => {
            if autonomous {
                return refuse(format!(
                    "{path}: Picard iteration has no period update and is refused for autonomous orbits (use newton_krylov or newton_picard)"
                ));
            }
            Section::new(&v, &path, &["kind"])?;
            (PeriodicMethod::Picard, json!({"kind": "picard"}))
        }
        "newton_krylov" => {
            let mut c = Section::new(&v, &path, &["kind", "krylov_dimension"])?;
            c.put("kind", json!("newton_krylov"));
            let k = c.integer_or("krylov_dimension", 30, 1..=500)?;
            (PeriodicMethod::NewtonKrylov { krylov_dimension: k }, c.finish())
        }
        _ => {
            let mut c = Section::new(&v, &path, &["kind", "subspace"])?;
            c.put("kind", json!("newton_picard"));
            let p = c.integer_or("subspace", 8, 1..=200)?;
            (PeriodicMethod::NewtonPicard { subspace: p }, c.finish())
        }
    };
    s.put("method", normal);
    Ok(m)
}

fn checkpoint(s: &mut Section<'_>) -> CaeResult<CheckpointPolicy> {
    let mut c = s.child_or_empty("checkpoint", &["policy", "ram_snapshots", "disk_snapshots"])?;
    let policy = c.choice_or("policy", "all", &["all", "binomial", "online"])?;
    let out = match policy.as_str() {
        "all" => {
            if c.has("ram_snapshots") || c.has("disk_snapshots") {
                return refuse("time.checkpoint snapshot counts apply to the binomial and online policies");
            }
            CheckpointPolicy::All
        }
        "binomial" => CheckpointPolicy::Binomial {
            ram_snapshots: c.integer_or("ram_snapshots", 16, 2..=100_000)?,
            disk_snapshots: c.integer_or("disk_snapshots", 0, 0..=100_000)?,
        },
        _ => CheckpointPolicy::Online {
            ram_snapshots: c.integer_or("ram_snapshots", 16, 2..=100_000)?,
            disk_snapshots: c.integer_or("disk_snapshots", 0, 0..=100_000)?,
        },
    };
    s.put("checkpoint", c.finish());
    Ok(out)
}


#[allow(clippy::too_many_lines)]
pub fn parse(v: &Value) -> CaeResult<(TimeSpec, Value)> {
    let kind = kind_of(v, "time", &TIME_KINDS)?;
    let periodic_keys = [
        "method",
        "tolerance",
        "adjoint_tolerance",
        "spin_up_periods",
        "max_periods",
        "stability_margin",
        "floquet_modes",
    ];
    let mut keys: Vec<&str> = COMMON.to_vec();
    match kind {
        "fixed_horizon" => keys.extend([
            "period_s",
            "steps_per_period",
            "periods",
            "autonomous",
            "max_adjoint_growth",
            "window_check_tolerance",
        ]),
        "periodic_forced" => {
            keys.extend(["period_s", "steps_per_period"]);
            keys.extend(periodic_keys);
        }
        "periodic_autonomous" => {
            keys.extend(["period_guess_s", "period_bounds_s", "steps_per_period", "phase_condition"]);
            keys.extend(periodic_keys);
        }
        _ => keys.extend(["step_s", "modes", "tolerance", "spin_up_steps"]),
    }
    let mut s = Section::new(v, "time", &keys)?;
    s.put("kind", json!(kind));
    let positive = |x: f64| x > 0.0;
    let mut spec = TimeSpec {
        kind: TimeKind::PeriodicForced,
        steps_per_period: 1,
        period_s: 1.0,
        method: PeriodicMethod::NewtonKrylov { krylov_dimension: 30 },
        tolerance: 1e-8,
        adjoint_tolerance: 1e-9,
        spin_up_periods: 0,
        max_periods: 200,
        stability_margin: 1e-3,
        floquet_modes: 2,
        max_adjoint_growth: DEFAULT_MAX_ADJOINT_GROWTH,
        window_check_tolerance: None,
        checkpoint: CheckpointPolicy::All,
        allow_biased_gradient: false,
    };
    match kind {
        "fixed_horizon" => {
            spec.period_s = s.number("period_s", positive, "positive")?;
            spec.steps_per_period = s.integer("steps_per_period", 1..=1_000_000)?;
            let periods = s.integer_or("periods", 1, 1..=100_000)?;
            let autonomous = s.boolean_or("autonomous", false)?;
            spec.max_adjoint_growth =
                s.number_or("max_adjoint_growth", DEFAULT_MAX_ADJOINT_GROWTH, |x| x >= 1.0, "at least 1")?;
            spec.window_check_tolerance = if s.raw("window_check_tolerance").is_some_and(|v| !v.is_null()) {
                if periods < 2 || periods % 2 != 0 {
                    return refuse("time.window_check_tolerance needs an even number of periods (K and K/2)");
                }
                Some(s.number("window_check_tolerance", positive, "positive")?)
            } else {
                s.put("window_check_tolerance", Value::Null);
                None
            };
            spec.kind = TimeKind::FixedHorizon { periods, autonomous };
        }
        "periodic_forced" | "periodic_autonomous" => {
            let autonomous = kind == "periodic_autonomous";
            spec.steps_per_period = s.integer("steps_per_period", 2..=1_000_000)?;
            if autonomous {
                spec.period_s = s.number("period_guess_s", positive, "positive")?;
                let b = s
                    .raw("period_bounds_s")
                    .ok_or_else(|| implexity_core::CaeError::contract("time.period_bounds_s is required"))?;
                let b = s.numbers_of("period_bounds_s", b, Some(2))?;
                if !(b[0] > 0.0 && b[0] < spec.period_s && spec.period_s < b[1]) {
                    return refuse("time.period_bounds_s must satisfy 0 < lower < period_guess_s < upper");
                }
                s.put("period_bounds_s", json!(b));
                let pv = s.raw("phase_condition").cloned().unwrap_or_else(|| json!({"kind": "integral"}));
                let ppath = s.at("phase_condition");
                let phase = if kind_of(&pv, &ppath, &["section", "integral"])? == "section" {
                    let mut p = Section::new(&pv, &ppath, &["kind", "sample", "level"])?;
                    p.put("kind", json!("section"));
                    let sample = p.text_or("sample", "", 64)?;
                    if sample.is_empty() {
                        return refuse(format!("{ppath}.sample must name an observable"));
                    }
                    let level = p.number_or("level", 0.0, |_| true, "finite")?;
                    s.put("phase_condition", p.finish());
                    PhaseSpec::Section { sample, level }
                } else {
                    Section::new(&pv, &ppath, &["kind"])?;
                    s.put("phase_condition", json!({"kind": "integral"}));
                    PhaseSpec::Integral
                };
                spec.kind = TimeKind::PeriodicAutonomous { phase, bounds_s: (b[0], b[1]) };
            } else {
                spec.period_s = s.number("period_s", positive, "positive")?;
                spec.kind = TimeKind::PeriodicForced;
            }
            spec.method = method(&mut s, autonomous)?;
            spec.tolerance = s.number_or("tolerance", 1e-8, unit_interval, "in (0, 1)")?;
            spec.adjoint_tolerance = s.number_or("adjoint_tolerance", 1e-9, unit_interval, "in (0, 1)")?;
            spec.spin_up_periods = s.integer_or("spin_up_periods", 2, 0..=100_000)?;
            spec.max_periods = s.integer_or("max_periods", 200, 1..=1_000_000)?;
            spec.stability_margin =
                s.number_or("stability_margin", 1e-3, |x| (0.0..1.0).contains(&x), "in [0, 1)")?;
            spec.floquet_modes = s.integer_or(
                "floquet_modes",
                if autonomous { 3 } else { 2 },
                usize::from(autonomous) + 1..=64,
            )?;
        }
        _ => {
            spec.period_s = s.number("step_s", positive, "positive")?;
            spec.steps_per_period = 1;
            let modes = s.integer_or("modes", 4, 1..=64)?;
            let tolerance = s.number_or("tolerance", 1e-10, unit_interval, "in (0, 1)")?;
            let spin_up_steps = s.integer_or("spin_up_steps", 0, 0..=1_000_000)?;
            spec.kind = TimeKind::SteadyStability { modes, tolerance, spin_up_steps };
        }
    }
    spec.checkpoint = checkpoint(&mut s)?;
    spec.allow_biased_gradient = s.boolean_or("allow_biased_gradient", false)?;
    Ok((spec, s.finish()))
}
