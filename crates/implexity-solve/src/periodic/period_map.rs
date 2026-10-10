// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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
