// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::cell::Cell;

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;

use crate::checkpointed_history::{
    CheckpointPolicy, HistoryGradient, HistoryRun, history_adjoint_many, history_tangent, run_history,
};
use crate::state_store::StoreBudget;
use crate::time_stepper::{StepParameters, TimeStepper};

pub(crate) struct PeriodMap<'a> {
    pub(crate) stepper: &'a dyn TimeStepper,
    pub(crate) design: &'a [f64],
    pub(crate) steps: usize,
    pub(crate) nominal_period_s: f64,
    policy: &'a CheckpointPolicy,
    budget: &'a StoreBudget,
    forward: Cell<usize>,
    tangent: Cell<usize>,
    sweeps: Cell<usize>,
}

pub(crate) struct PeriodTangent {
    pub(crate) state: Vec<f64>,
    pub(crate) samples: DenseMatrix,
}

impl<'a> PeriodMap<'a> {
    pub(crate) fn new(
        stepper: &'a dyn TimeStepper,
        design: &'a [f64],
        steps: usize,
        policy: &'a CheckpointPolicy,
        budget: &'a StoreBudget,
    ) -> CaeResult<Self> {
        if design.len() != stepper.design_size() || !design.iter().all(|v| v.is_finite()) {
            return Err(CaeError::contract(format!(
                "periodic design must be a finite vector of length {} (got {})",
                stepper.design_size(),
                design.len()
            )));
        }
        let dt = stepper.nominal_step_s();
        if !(dt.is_finite() && dt > 0.0) {
            return Err(CaeError::contract("time stepper nominal step must be finite and positive"));
        }
        if stepper.state_size() == 0 {
            return Err(CaeError::contract("periodic orbits need a nonempty stepper state"));
        }
        Ok(Self {
            stepper,
            design,
            steps,
            nominal_period_s: dt * steps as f64,
            policy,
            budget,
            forward: Cell::new(0),
            tangent: Cell::new(0),
            sweeps: Cell::new(0),
        })
    }

    pub(crate) fn state_size(&self) -> usize {
        self.stepper.state_size()
    }

    pub(crate) fn sample_count(&self) -> usize {
        self.stepper.sample_names().len()
    }

    pub(crate) fn time_scale(&self, period_s: f64) -> f64 {
        period_s / self.nominal_period_s
    }

    fn params(&self, period_s: f64) -> StepParameters<'a> {
        StepParameters { design: self.design, time_scale: self.time_scale(period_s) }
    }

    pub(crate) fn run(&self, z0: &[f64], period_s: f64) -> CaeResult<HistoryRun> {
        self.forward.set(self.forward.get() + 1);
        let run = run_history(
            self.stepper,
            self.params(period_s),
            z0,
            1,
            Some(self.steps),
            None,
            self.policy.clone(),
            self.budget,
        )?;
        if run.final_state().len() != z0.len() || !run.final_state().iter().all(|v| v.is_finite()) {
            return Err(crate::periodic::nonperiodic(
                "a period run produced a non-finite state (the trajectory diverged)",
            ));
        }
        Ok(run)
    }

    pub(crate) fn tangent(
        &self,
        run: &HistoryRun,
        period_s: f64,
        d_initial: &[f64],
        relative_period: f64,
    ) -> CaeResult<PeriodTangent> {
        self.tangent.set(self.tangent.get() + 1);
        let tau = self.time_scale(period_s);
        let (state, samples) = history_tangent(
            self.stepper,
            self.params(period_s),
            run,
            d_initial,
            None,
            relative_period * tau,
        )?;
        Ok(PeriodTangent { state, samples })
    }

    pub(crate) fn adjoint(
        &self,
        run: &HistoryRun,
        period_s: f64,
        sample_bars: &[DenseMatrix],
        final_bars: &[Vec<f64>],
    ) -> CaeResult<Vec<HistoryGradient>> {
        if sample_bars.len() != final_bars.len() {
            return Err(CaeError::contract("periodic adjoint sweep received unequal cotangent batches"));
        }
        if sample_bars.is_empty() {
            return Ok(Vec::new());
        }
        self.sweeps.set(self.sweeps.get() + 1);
        let out = history_adjoint_many(self.stepper, self.params(period_s), run, sample_bars, final_bars)?;
        if out.len() != sample_bars.len() {
            return Err(CaeError::contract("history_adjoint_many returned a different number of gradients"));
        }
        Ok(out)
    }

    pub(crate) fn zero_samples(&self) -> DenseMatrix {
        DenseMatrix::zeros(self.steps, self.sample_count())
    }

    pub(crate) fn forward_runs(&self) -> usize {
        self.forward.get()
    }

    pub(crate) fn tangent_runs(&self) -> usize {
        self.tangent.get()
    }

    pub(crate) fn adjoint_sweeps(&self) -> usize {
        self.sweeps.get()
    }
}
