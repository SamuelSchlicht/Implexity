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



use crate::error::AdError;
use crate::revolve::{Action, BinomialSchedule};
use crate::tape::{Tape, Var};

pub trait ScanStep {


    fn step(&self, t: usize, state: &[f64], params: &[f64]) -> Result<Vec<f64>, AdError>;



    fn vjp(
        &self,
        t: usize,
        state: &[f64],
        params: &[f64],
        cotangent: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), AdError>;




    fn jvp(
        &self,
        t: usize,
        state: &[f64],
        params: &[f64],
        d_state: &[f64],
        d_params: &[f64],
    ) -> Result<Vec<f64>, AdError> {
        if d_state.len() != state.len() || d_params.len() != params.len() {
            return Err(AdError::Shape(format!(
                "scan jvp at step {t}: directions of lengths {} / {} for a state of {} and {} parameters",
                d_state.len(),
                d_params.len(),
                state.len(),
                params.len()
            )));
        }
        let mut out = vec![0.0; state.len()];
        let mut unit = vec![0.0; state.len()];
        for (i, value) in out.iter_mut().enumerate() {
            unit[i] = 1.0;
            let (gx, gp) = self.vjp(t, state, params, &unit)?;
            unit[i] = 0.0;
            if gx.len() != state.len() || gp.len() != params.len() {
                return Err(AdError::Shape(format!(
                    "scan vjp at step {t} returned lengths {} / {}",
                    gx.len(),
                    gp.len()
                )));
            }
            *value = gx.iter().zip(d_state).map(|(a, b)| a * b).sum::<f64>()
                + gp.iter().zip(d_params).map(|(a, b)| a * b).sum::<f64>();
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TermGradient {
    pub value: f64,
    pub d_state: Vec<f64>,
    pub d_params: Vec<f64>,
}

pub trait ScanObjective {


    fn stage(&self, _t: usize, _state: &[f64], _params: &[f64]) -> Result<Option<TermGradient>, AdError> {
        Ok(None)
    }



    fn terminal(&self, state: &[f64], params: &[f64]) -> Result<TermGradient, AdError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checkpointing {
    All,
    Sqrt,
    Every(usize),
    Binomial {
        snapshots: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScanGradient {
    pub value: f64,
    pub final_state: Vec<f64>,
    pub grad_initial: Vec<f64>,
    pub grad_params: Vec<f64>,
    pub peak_stored_states: usize,
    pub step_evaluations: usize,
}

fn add_into(acc: &mut [f64], x: &[f64], what: &str) -> Result<(), AdError> {
    if acc.len() != x.len() {
        return Err(AdError::Shape(format!("{what}: length {} where {} was expected", x.len(), acc.len())));
    }
    for (a, b) in acc.iter_mut().zip(x) {
        *a += b;
    }
    Ok(())
}



pub fn scan_adjoint<S, O>(
    step: &S,
    objective: &O,
    initial: &[f64],
    params: &[f64],
    n_steps: usize,
    policy: Checkpointing,
) -> Result<ScanGradient, AdError>
where
    S: ScanStep + ?Sized,
    O: ScanObjective + ?Sized,
{
    let segment = match policy {
        Checkpointing::All => 1,
        Checkpointing::Sqrt => {
            let mut s = 1usize;
            while s * s < n_steps {
                s += 1;
            }
            s
        }
        Checkpointing::Every(0) => {
            return Err(AdError::Invalid("checkpoint interval must be positive".into()));
        }
        Checkpointing::Every(k) => k,
        Checkpointing::Binomial { snapshots } => {
            if snapshots == 0 {
                return Err(AdError::Invalid("binomial checkpointing needs at least one snapshot".into()));
            }
            if n_steps > 0 {
                return binomial_scan(step, objective, initial, params, n_steps, snapshots);
            }
            1
        }
    };
    let dim = initial.len();
    let np = params.len();
    let mut value = 0.0;
    let mut grad_params = vec![0.0; np];
    let mut evaluations = 0usize;

    let mut checkpoints: Vec<(usize, Vec<f64>)> = vec![(0, initial.to_vec())];
    let mut x = initial.to_vec();
    for t in 0..n_steps {
        let next = step.step(t, &x, params)?;
        evaluations += 1;
        if next.len() != dim {
            return Err(AdError::Shape(format!(
                "scan step {t} returned {} values for a state of {dim}",
                next.len()
            )));
        }
        if let Some(term) = objective.stage(t, &next, params)? {
            value += term.value;
            add_into(&mut grad_params, &term.d_params, "stage cost parameter gradient")?;
        }
        x = next;
        if (t + 1) % segment == 0 && t + 1 < n_steps {
            checkpoints.push((t + 1, x.clone()));
        }
    }
    let terminal = objective.terminal(&x, params)?;
    value += terminal.value;
    add_into(&mut grad_params, &terminal.d_params, "terminal parameter gradient")?;
    let mut lambda = terminal.d_state;
    if lambda.len() != dim {
        return Err(AdError::Shape("terminal state gradient has the wrong length".into()));
    }
    let final_state = x;
    let mut peak = checkpoints.len();

    let mut after = final_state.clone();
    while let Some((start, x_start)) = checkpoints.pop() {
        let end = (start + segment).min(n_steps);

        let mut states = Vec::with_capacity(end - start + 1);
        states.push(x_start);
        for t in start..end.saturating_sub(1) {
            let next = step.step(t, &states[t - start], params)?;
            evaluations += 1;
            states.push(next);
        }
        peak = peak.max(checkpoints.len() + states.len() + 1);
        for t in (start..end).rev() {
            let x_next = if t + 1 < end { &states[t + 1 - start] } else { &after };
            reverse_step(
                step,
                objective,
                t,
                &states[t - start],
                x_next,
                params,
                &mut lambda,
                &mut grad_params,
            )?;
        }
        after = states.swap_remove(0);
    }
    Ok(ScanGradient {
        value,
        final_state,
        grad_initial: lambda,
        grad_params,
        peak_stored_states: peak,
        step_evaluations: evaluations,
    })
}

#[allow(clippy::too_many_arguments)]
fn reverse_step<S, O>(
    step: &S,
    objective: &O,
    t: usize,
    x_t: &[f64],
    x_next: &[f64],
    params: &[f64],
    lambda: &mut Vec<f64>,
    grad_params: &mut [f64],
) -> Result<(), AdError>
where
    S: ScanStep + ?Sized,
    O: ScanObjective + ?Sized,
{
    if let Some(term) = objective.stage(t, x_next, params)? {
        add_into(lambda, &term.d_state, "stage cost state gradient")?;
    }
    let (gx, gp) = step.vjp(t, x_t, params, lambda)?;
    if gx.len() != x_t.len() || gp.len() != params.len() {
        return Err(AdError::Shape(format!(
            "scan vjp at step {t} returned lengths {} / {}",
            gx.len(),
            gp.len()
        )));
    }
    add_into(grad_params, &gp, "step parameter cotangent")?;
    *lambda = gx;
    Ok(())
}

fn missing(what: &str) -> AdError {
    AdError::Invalid(format!("binomial checkpoint schedule is inconsistent: {what}"))
}

#[allow(clippy::too_many_lines)]
fn binomial_scan<S, O>(
    step: &S,
    objective: &O,
    initial: &[f64],
    params: &[f64],
    n_steps: usize,
    snapshots: usize,
) -> Result<ScanGradient, AdError>
where
    S: ScanStep + ?Sized,
    O: ScanObjective + ?Sized,
{
    let schedule = BinomialSchedule::new(n_steps, snapshots, 0)?;
    let dim = initial.len();
    let mut slots: Vec<Option<(usize, Vec<f64>)>> = vec![None; snapshots];
    let mut cursor: Option<(usize, Vec<f64>)> = Some((0, initial.to_vec()));
    let mut ahead: Option<(usize, Vec<f64>)> = None;
    let mut value = 0.0;
    let mut grad_params = vec![0.0; params.len()];
    let mut lambda: Vec<f64> = Vec::new();
    let mut final_state: Vec<f64> = Vec::new();
    let mut evaluations = 0usize;
    let mut peak = 0usize;
    for (index, action) in schedule.actions().iter().enumerate() {
        let forward = index < schedule.reverse_start();
        match *action {
            Action::Snapshot { step: at, slot, .. } => {
                let state = match &cursor {
                    Some((p, x)) if *p == at => x.clone(),
                    _ => return Err(missing("snapshot of a state the cursor does not hold")),
                };
                *slots.get_mut(slot).ok_or_else(|| missing("slot out of range"))? = Some((at, state));
            }
            Action::Restore { step: at, slot } => match slots.get(slot) {
                Some(Some((p, x))) if *p == at => cursor = Some((at, x.clone())),
                _ => return Err(missing("restore of an empty or foreign slot")),
            },
            Action::Release { slot } => {
                if let Some(s) = slots.get_mut(slot) {
                    *s = None;
                }
            }
            Action::Advance { from, to } => {
                let Some((p, mut x)) = cursor.take() else { return Err(missing("advance without a cursor")) };
                if p != from {
                    return Err(missing("advance from a position the cursor does not hold"));
                }
                let mut position = from;
                for t in from..to {
                    let next = step.step(t, &x, params)?;
                    evaluations += 1;
                    if next.len() != dim {
                        return Err(AdError::Shape(format!(
                            "scan step {t} returned {} values for a state of {dim}",
                            next.len()
                        )));
                    }
                    if forward && let Some(term) = objective.stage(t, &next, params)? {
                        value += term.value;
                        add_into(&mut grad_params, &term.d_params, "stage cost parameter gradient")?;
                    }
                    if t + 1 == n_steps {

                        ahead = Some((n_steps, next));
                        break;
                    }
                    x = next;
                    position = t + 1;
                }
                cursor = Some((position, x));
                if to == n_steps {
                    let Some((_, xn)) = &ahead else { return Err(missing("final state not computed")) };
                    let terminal = objective.terminal(xn, params)?;
                    value += terminal.value;
                    add_into(&mut grad_params, &terminal.d_params, "terminal parameter gradient")?;
                    if terminal.d_state.len() != dim {
                        return Err(AdError::Shape("terminal state gradient has the wrong length".into()));
                    }
                    lambda = terminal.d_state;
                    final_state.clone_from(xn);
                }
            }
            Action::Reverse { step: k } => {
                let (Some((pc, x_t)), Some((pa, x_next))) = (cursor.take(), ahead.take()) else {
                    return Err(missing("reverse without both states"));
                };
                if pc + 1 != k || pa != k {
                    return Err(missing("reverse of a step whose states are not held"));
                }
                reverse_step(step, objective, k - 1, &x_t, &x_next, params, &mut lambda, &mut grad_params)?;
                ahead = Some((pc, x_t));
            }
        }
        let held = slots.iter().filter(|s| s.is_some()).count()
            + usize::from(cursor.is_some())
            + usize::from(ahead.is_some());
        peak = peak.max(held);
    }
    Ok(ScanGradient {
        value,
        final_state,
        grad_initial: lambda,
        grad_params,
        peak_stored_states: peak,
        step_evaluations: evaluations,
    })
}


pub struct TapeStep<F> {
    body: F,
}

impl<F> TapeStep<F>
where
    F: Fn(&mut Tape, usize, Var, Var) -> Result<Var, AdError>,
{
    pub const fn new(body: F) -> Self {
        Self { body }
    }
}

impl<F> ScanStep for TapeStep<F>
where
    F: Fn(&mut Tape, usize, Var, Var) -> Result<Var, AdError>,
{
    fn step(&self, t: usize, state: &[f64], params: &[f64]) -> Result<Vec<f64>, AdError> {
        let mut tape = Tape::new();
        let x = tape.input(state.to_vec());
        let p = tape.input(params.to_vec());
        let y = (self.body)(&mut tape, t, x, p)?;
        Ok(tape.value(y)?.to_vec())
    }

    fn vjp(
        &self,
        t: usize,
        state: &[f64],
        params: &[f64],
        cotangent: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), AdError> {
        let mut tape = Tape::new();
        let x = tape.input(state.to_vec());
        let p = tape.input(params.to_vec());
        let y = (self.body)(&mut tape, t, x, p)?;
        let g = tape.vjp(y, cotangent)?;
        Ok((g.wrt(x)?, g.wrt(p)?))
    }
}
