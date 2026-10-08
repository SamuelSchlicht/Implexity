// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use implexity_core::error::{CaeError, CaeResult};

use super::cache::{CacheEntry, design_key, step_key};
use super::derivatives::{CoupledPoint, Linearization};
use super::linear::{CurrentFactor, KrylovLimits, add_into, dot, negated, norm, norm_inf};
use super::{CouplingMode, FluxDrivenField, MultirateStepper, SubcycleRecord, SubcycledField, contract};
use crate::interface_quasi_newton::{IqnIls, SecantInverse};
use crate::newton_krylov::ExactKrylovPolicy;
use crate::time_stepper::{StepDiagnostics, StepParameters, StepRecord};

pub(super) const ROUNDING_FLOOR: f64 = 64.0 * f64::EPSILON;

pub const LEDGER_INTERFACE_WORK_A: &str = "interface_work_field_a";
pub const LEDGER_INTERFACE_WORK_B: &str = "interface_work_field_b";
pub const LEDGER_INTERFACE_WORK_DEFECT: &str = "interface_work_defect";
pub const LEDGER_COUPLING_RESIDUAL: &str = "coupling_residual";

struct StepInputs<'s> {
    n: usize,
    b_prev: &'s [f64],
    a_prev: &'s [f64],
    d_s: &'s [f64],
    p: StepParameters<'s>,
    hit: Option<&'s CacheEntry>,
}

struct Converged {
    field_b: Vec<f64>,
    rec: SubcycleRecord,
    trace_end: Vec<f64>,
    reference: f64,
    diagnostics: StepDiagnostics,
}

pub(super) struct FieldSolve {
    pub(super) z: Vec<f64>,
    pub(super) iterations: usize,
    pub(super) residual: f64,
}

fn finite(label: &str, v: &[f64], size: usize) -> CaeResult<()> {
    if v.len() != size {
        return Err(contract(format!("multirate coupling: {label} has length {}, expected {size}", v.len())));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(contract(format!("multirate coupling: {label} must be finite")));
    }
    Ok(())
}

impl<A: SubcycledField, B: FluxDrivenField> MultirateStepper<A, B> {
    pub(super) fn check_parameters(&self, p: StepParameters<'_>) -> CaeResult<()> {
        finite("design", p.design, self.n_d)?;
        if !p.time_scale.is_finite() || p.time_scale <= 0.0 {
            return Err(contract(format!(
                "multirate coupling: time scale must be finite and positive, got {}",
                p.time_scale
            )));
        }
        Ok(())
    }

    pub(super) fn check_state(&self, label: &str, z: &[f64]) -> CaeResult<()> {
        finite(label, z, self.total_state())
    }

    pub(super) fn split<'z>(&self, z: &'z [f64]) -> (&'z [f64], &'z [f64], &'z [f64]) {
        let layout = self.layout();
        (&z[layout.field_b], &z[layout.field_a], &z[layout.lags])
    }

    pub(super) fn trace_of(&self, z_b: &[f64]) -> CaeResult<Vec<f64>> {
        self.trace.matvec(z_b).map_err(|e| contract(format!("multirate coupling: trace operator: {e}")))
    }

    pub(super) fn trace_transpose(&self, t: &[f64]) -> CaeResult<Vec<f64>> {
        self.trace
            .matvec_transpose(t)
            .map_err(|e| contract(format!("multirate coupling: trace operator: {e}")))
    }

    pub(super) fn predicted_trace(&self, d_s: &[f64], lags: &[f64]) -> Vec<f64> {
        let mut out: Vec<f64> = d_s.iter().map(|v| self.lag_coefficients[0] * v).collect();
        for (i, lag) in lags.chunks(self.n_t).enumerate() {
            let c = self.lag_coefficients[i + 1];
            for (o, v) in out.iter_mut().zip(lag) {
                *o += c * v;
            }
        }
        out
    }

    pub(super) fn shifted_lags(&self, d_s: &[f64], lags: &[f64]) -> Vec<f64> {
        if lags.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(lags.len());
        out.extend_from_slice(d_s);
        out.extend_from_slice(&lags[..lags.len() - self.n_t]);
        out
    }

    fn admitted_b_trial(&self, n: usize, current: &[f64], direction: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<f64> {
        let alpha=self.b.admissible_step(n,current,direction,previous,p,1.0)?;
        if !alpha.is_finite() || alpha<=0.0 || alpha>1.0 {
            return Err(contract("multirate coupling: invalid field path limit"));
        }
        Ok(alpha)
    }

    fn check_b_time_path(&self, n: usize, previous: &[f64], current: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        let direction: Vec<f64> = current.iter().zip(previous).map(|(a,b)|a-b).collect();
        if self.admitted_b_trial(n,previous,&direction,previous,p)?<1.0 {
            return Err(contract("multirate coupling: physical field path is inadmissible"));
        }
        Ok(())
    }

    fn add_admitted_b_trial(&self, n: usize, current: &mut [f64], direction: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        let alpha=self.admitted_b_trial(n,current,direction,previous,p)?;
        if alpha==1.0 {add_into(current,direction);} else {
            for (a,b) in current.iter_mut().zip(direction) {*a+=alpha*b;}
        }
        Ok(())
    }

    pub(super) fn predict_b(&self, n: usize, b_prev: &[f64], p: StepParameters<'_>) -> CaeResult<Vec<f64>> {
        let pred = self.b.predict(n, b_prev, None, p);
        finite("field-B predictor", &pred, self.n_b)?;
        self.check_b_time_path(n,b_prev,&pred,p)?;
        Ok(pred)
    }

    pub(super) fn run_a(
        &self,
        n: usize,
        a_prev: &[f64],
        d_s: &[f64],
        d_e: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        self.b.check_trace_path(n,d_s,d_e,p)?;
        let rec = self.a.subcycle(n, a_prev, d_s, d_e, p)?;
        finite("field-A end state", &rec.state, self.n_a)?;
        finite("field-A flux", &rec.flux, self.n_t)?;
        finite("field-A samples", &rec.samples, self.sample_names.len() - self.ns_b)?;
        if !rec.pairing.is_finite() {
            return Err(contract("multirate coupling: field-A interface pairing must be finite"));
        }
        Ok(rec)
    }

    pub(super) fn residual_b(
        &self,
        n: usize,
        z: &[f64],
        b_prev: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        let r = self.b.residual(n, z, b_prev, flux, p)?;
        if r.len() != self.n_b {
            return Err(contract(format!(
                "multirate coupling: field-B residual has length {}, expected {}",
                r.len(),
                self.n_b
            )));
        }
        if r.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::newton("multirate coupling: field-B residual is not finite"));
        }
        Ok(r)
    }


    pub(super) fn solve_field_b(
        &self,
        n: usize,
        b_prev: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
        guess: Option<&[f64]>,
    ) -> CaeResult<FieldSolve> {
        let pred = self.predict_b(n, b_prev, p)?;
        let r_pred = self.residual_b(n, &pred, b_prev, flux, p)?;
        let reference = norm(&r_pred).max(norm(flux));
        let tol = self.newton.relative_tolerance * reference;
        let (mut z, mut r) = match guess {
            Some(g) => {
                finite("field-B Newton guess", g, self.n_b)?;
                self.check_b_time_path(n,b_prev,g,p)?;
                let r = self.residual_b(n, g, b_prev, flux, p)?;
                (g.to_vec(), r)
            }
            None => (pred, r_pred),
        };
        let explicit = self.b.explicit();
        let mut factor: Option<CurrentFactor> = None;
        let mut iterations = 0usize;
        loop {
            let rn = norm(&r);
            if rn == 0.0 || rn <= tol {
                self.check_b_time_path(n,b_prev,&z,p)?;
                self.b.check_state_domain(n,&z,b_prev,p)?;
                return Ok(FieldSolve { z, iterations, residual: rn });
            }
            if explicit && iterations == 1 {
                return Err(contract(format!(
                    "multirate coupling: field B declares an explicit (affine) step, but its residual {rn:e} \
                     survived the single linear correction (reference {reference:e})"
                )));
            }
            if iterations >= self.newton.max_iterations {
                return Err(CaeError::newton(format!(
                    "multirate coupling: field-B Newton did not converge in {iterations} iterations at step {n} \
                     (residual {rn:e}, tolerance {tol:e})"
                )));
            }
            if factor.is_none() || !explicit {
                let jac = self.b.current_jacobian(n, &z, b_prev, flux, p)?;
                factor = Some(CurrentFactor::new(&jac, self.n_b, self.b.local_elimination_groups()?)?);
            }
            let Some(f) = factor.as_ref() else {
                return Err(contract("multirate coupling: field-B factorization missing"));
            };
            let mut delta = f.solve(&negated(&r), false)?;
            let mut alternatives = 0usize;
            loop {
                let Some(matrix) = self.b.newton_alternative(n, &z, b_prev, flux, p, &delta, alternatives)? else { break; };
                if alternatives >= self.newton.max_iterations {
                    return Err(contract("field-B generalized Newton alternatives exhausted"));
                }
                let alternate_factor = CurrentFactor::new(&matrix, self.n_b, self.b.local_elimination_groups()?)?;
                delta = alternate_factor.solve(&negated(&r), false)?;
                factor = Some(alternate_factor);
                alternatives += 1;
            }
            iterations += 1;
            if norm_inf(&delta) <= ROUNDING_FLOOR * (1.0 + norm_inf(&z)) {
                self.check_b_time_path(n,b_prev,&z,p)?;
                self.b.check_state_domain(n,&z,b_prev,p)?;
                return Ok(FieldSolve { z, iterations, residual: rn });
            }
            self.add_admitted_b_trial(n, &mut z, &delta, b_prev, p)?;
            r = self.residual_b(n, &z, b_prev, flux, p)?;
        }
    }

    pub(super) fn cached(&self, key: &[u8; 32]) -> Option<CacheEntry> {
        self.cache.lock().ok().and_then(|c| c.get(key))
    }

    fn remember(&self, key: [u8; 32], entry: CacheEntry) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, entry);
        }
    }

    fn check_admitted(&self, design: &[f64]) -> CaeResult<()> {
        let key = design_key(design);
        let admitted =
            self.admitted.lock().map_err(|_| contract("multirate coupling: admission lock poisoned"))?;
        if admitted.contains(&key) {
            Ok(())
        } else {
            Err(contract(
                "multirate coupling: loose coupling of this design has not passed the interface Schur-ratio \
                 preflight (MultirateStepper::interface_schur_ratio; added-mass rule, FSI_DYNAMIC_TOPOLOGY.md §3.5)",
            ))
        }
    }

    pub fn admitted_loose_prediction(&self, previous: &[f64], p: StepParameters<'_>) -> CaeResult<Vec<f64>> {
        if !matches!(self.options.mode, CouplingMode::Loose { .. }) {
            return Err(contract("loose prediction requires the authored loose coupling mode"));
        }
        self.check_parameters(p)?;
        self.check_state("previous", previous)?;
        self.check_admitted(p.design)?;
        let (body, _, lags) = self.split(previous);
        Ok(self.predicted_trace(&self.trace_of(body)?, lags))
    }

    pub fn prepare_admitted_loose_subcycle(
        &self,
        n: usize,
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<(Vec<f64>, Vec<f64>, SubcycleRecord)> {
        if n == 0 || !matches!(self.options.mode, CouplingMode::Loose { .. }) {
            return Err(contract("loose subcycle requires an authored loose mode and positive macro index"));
        }
        self.check_parameters(p)?;
        self.check_state("previous", previous)?;
        self.check_admitted(p.design)?;
        let (body, fluid, lags) = self.split(previous);
        let start = self.trace_of(body)?;
        let end = self.predicted_trace(&start, lags);
        let record = self.run_a(n, fluid, &start, &end, p)?;
        Ok((end, self.shifted_lags(&start, lags), record))
    }

    fn loose_step(&self, s: &StepInputs<'_>, lags: &[f64]) -> CaeResult<Converged> {
        self.check_admitted(s.p.design)?;
        let d_e = self.predicted_trace(s.d_s, lags);
        let rec = self.run_a(s.n, s.a_prev, s.d_s, &d_e, s.p)?;
        let guess = s.hit.filter(|h| h.trace_end == d_e).map(|h| h.field_b.as_slice());
        let solve = self.solve_field_b(s.n, s.b_prev, &rec.flux, s.p, guess)?;
        let diagnostics = StepDiagnostics {
            newton_iterations: solve.iterations,
            coupling_iterations: 1,
            residual_norm: solve.residual,
            ..StepDiagnostics::default()
        };
        Ok(Converged { field_b: solve.z, rec, trace_end: d_e, reference: 0.0, diagnostics })
    }

    fn quasi_newton_step(
        &self,
        s: &StepInputs<'_>,
        tolerance: f64,
        max_iterations: usize,
        reuse_steps: usize,
        initial_relaxation: f64,
    ) -> CaeResult<Converged> {
        let mut memory =
            self.memory.lock().map_err(|_| contract("multirate coupling: IQN memory lock poisoned"))?;
        if memory.is_none() {
            *memory = Some(
                IqnIls::new(self.n_t, reuse_steps, self.secant_filter)?
                    .with_initial_relaxation(initial_relaxation)?,
            );
        }
        let Some(iqn) = memory.as_mut() else {
            return Err(contract("multirate coupling: IQN memory missing"));
        };
        let (mut d, mut guess) = match s.hit {
            Some(h) => (h.trace_end.clone(), Some(h.field_b.clone())),
            None => (self.trace_of(&self.predict_b(s.n, s.b_prev, s.p)?)?, None),
        };
        let mut diagnostics = StepDiagnostics::default();
        loop {
            let rec = self.run_a(s.n, s.a_prev, s.d_s, &d, s.p)?;
            let solve = self.solve_field_b(s.n, s.b_prev, &rec.flux, s.p, guess.as_deref())?;
            diagnostics.newton_iterations += solve.iterations;
            diagnostics.coupling_iterations += 1;
            let d_tilde = self.trace_of(&solve.z)?;
            let r: Vec<f64> = d_tilde.iter().zip(&d).map(|(a, b)| a - b).collect();
            let rn = norm(&r);
            let reference = norm(&d_tilde).max(norm(s.d_s));

            let floor = ROUNDING_FLOOR * (1.0 + norm_inf(&d));
            if rn == 0.0 || rn <= tolerance * reference || norm_inf(&r) <= floor {
                iqn.end_step();
                diagnostics.residual_norm = rn;
                return Ok(Converged { field_b: solve.z, rec, trace_end: d, reference: 0.0, diagnostics });
            }
            if diagnostics.coupling_iterations >= max_iterations {
                iqn.end_step();
                return Err(CaeError::convergence(format!(
                    "multirate coupling: IQN-ILS did not converge in {max_iterations} iterations at step {} \
                     (trace residual {rn:e}, tolerance {:e})",
                    s.n,
                    tolerance * reference
                )));
            }
            let next = iqn.update(&r, &d)?;
            guess = Some(solve.z);
            d = next;
        }
    }

    fn newton_krylov_step(
        &self,
        s: &StepInputs<'_>,
        tolerance: f64,
        max_iterations: usize,
        krylov: &ExactKrylovPolicy,
    ) -> CaeResult<Converged> {
        let mut secant =
            SecantInverse::new(self.n_t, 0, self.secant_filter)?.with_max_columns(krylov.restart);
        let (mut z, mut reference) = match s.hit {
            Some(h) => (h.field_b.clone(), Some(h.reference)),
            None => (self.predict_b(s.n, s.b_prev, s.p)?, None),
        };
        let limits =
            KrylovLimits { rtol: krylov.rtol, restart: krylov.restart, max_iterations: krylov.maxiter };
        let mut diagnostics = StepDiagnostics::default();
        loop {
            self.check_b_time_path(s.n,s.b_prev,&z,s.p)?;
            let d_e = self.trace_of(&z)?;
            let rec = self.run_a(s.n, s.a_prev, s.d_s, &d_e, s.p)?;
            let g = self.residual_b(s.n, &z, s.b_prev, &rec.flux, s.p)?;
            let gn = norm(&g);
            let reference = *reference.get_or_insert_with(|| gn.max(norm(&rec.flux)));
            if gn == 0.0 || gn <= tolerance * reference {
                diagnostics.residual_norm = gn;
                return Ok(Converged { field_b: z, rec, trace_end: d_e, reference, diagnostics });
            }
            if diagnostics.coupling_iterations >= max_iterations {
                return Err(CaeError::newton(format!(
                    "multirate coupling: coupled Newton-Krylov did not converge in {max_iterations} iterations \
                     at step {} (residual {gn:e}, tolerance {:e})",
                    s.n,
                    tolerance * reference
                )));
            }
            let jac = self.b.jacobians(s.n, &z, s.b_prev, &rec.flux, s.p)?;
            let mut lin = Linearization::new(self, jac, rec.flux.clone())?;
            let point = CoupledPoint { n: s.n, a_prev: s.a_prev, d_s: s.d_s, d_e: &d_e, p: s.p };
            let (mut delta, krylov_iterations) =
                self.coupled_solve(&point, &lin, &negated(&g), &mut secant, limits)?;
            diagnostics.krylov_iterations += krylov_iterations;
            let mut attempt = 0;
            while let Some(current) = self.b.newton_alternative(
                s.n, &z, s.b_prev, &rec.flux, s.p, &delta, attempt,
            )? {
                attempt += 1;
                if attempt > self.newton.max_iterations {
                    return Err(contract("field-B Newton branch selection did not terminate"));
                }
                lin.jac.current = current;
                lin = Linearization::new(self, lin.jac, rec.flux.clone())?;
                secant = SecantInverse::new(self.n_t, 0, self.secant_filter)?
                    .with_max_columns(krylov.restart);
                let solved = self.coupled_solve(&point, &lin, &negated(&g), &mut secant, limits)?;
                delta = solved.0;
                diagnostics.krylov_iterations += solved.1;
            }
            diagnostics.coupling_iterations += 1;
            if norm_inf(&delta) <= ROUNDING_FLOOR * (1.0 + norm_inf(&z)) {
                diagnostics.residual_norm = gn;
                return Ok(Converged { field_b: z, rec, trace_end: d_e, reference, diagnostics });
            }
            self.add_admitted_b_trial(s.n, &mut z, &delta, s.b_prev, s.p)?;
        }
    }

    pub(super) fn advance_step(
        &self,
        n: usize,
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<StepRecord> {
        self.check_state("previous state", previous)?;
        self.check_parameters(p)?;
        let (b_prev, a_prev, lags) = self.split(previous);
        let d_s = self.trace_of(b_prev)?;
        let key = step_key(n, previous, p);
        let hit = self.cached(&key);
        let inputs = StepInputs { n, b_prev, a_prev, d_s: &d_s, p, hit: hit.as_ref() };
        let Converged { field_b, rec, trace_end, reference, mut diagnostics } = match &self.options.mode {
            CouplingMode::Loose { .. } => self.loose_step(&inputs, lags)?,
            CouplingMode::StrongQuasiNewton {
                tolerance,
                max_iterations,
                reuse_steps,
                initial_relaxation,
            } => self.quasi_newton_step(
                &inputs,
                *tolerance,
                *max_iterations,
                *reuse_steps,
                *initial_relaxation,
            )?,
            CouplingMode::StrongNewtonKrylov { tolerance, max_iterations, krylov } => {
                self.newton_krylov_step(&inputs, *tolerance, *max_iterations, krylov)?
            }
        };
        finite("field-B end state", &field_b, self.n_b)?;
        self.check_b_time_path(n,b_prev,&field_b,p)?;
        self.b.check_state_domain(n, &field_b, b_prev, p)?;
        let samples_b = self.b.samples(n, &field_b, b_prev, p)?;
        finite("field-B samples", &samples_b, self.ns_b)?;
        let d_end = self.trace_of(&field_b)?;
        let work_b: f64 = rec.flux.iter().zip(d_end.iter().zip(&d_s)).map(|(f, (e, s))| f * (e - s)).sum();
        let mut ledger: BTreeMap<String, f64> = rec.ledger.clone();
        for key in [
            LEDGER_INTERFACE_WORK_A,
            LEDGER_INTERFACE_WORK_B,
            LEDGER_INTERFACE_WORK_DEFECT,
            LEDGER_COUPLING_RESIDUAL,
        ] {
            if ledger.contains_key(key) {
                return Err(contract(format!(
                    "multirate coupling: field A must not write the driver ledger key {key:?}"
                )));
            }
        }
        ledger.insert(LEDGER_INTERFACE_WORK_A.to_string(), rec.pairing);
        ledger.insert(LEDGER_INTERFACE_WORK_B.to_string(), work_b);
        ledger.insert(LEDGER_INTERFACE_WORK_DEFECT.to_string(), rec.pairing - work_b);
        ledger.insert(LEDGER_COUPLING_RESIDUAL.to_string(), diagnostics.residual_norm);
        for (key,value) in self.b.step_ledger(n,&field_b,b_prev,p)? {
            if !value.is_finite() || ledger.contains_key(&key) { return Err(contract(format!("multirate coupling: invalid or duplicate field-B ledger key {key:?}"))); }
            ledger.insert(key,value);
        }
        let mut state = Vec::with_capacity(self.total_state());
        state.extend_from_slice(&field_b);
        state.extend_from_slice(&rec.state);
        state.extend(self.shifted_lags(&d_s, lags));
        let mut samples = samples_b;
        samples.extend_from_slice(&rec.samples);
        diagnostics.newton_iterations += rec.diagnostics.newton_iterations;
        diagnostics.krylov_iterations += rec.diagnostics.krylov_iterations;
        self.remember(key, CacheEntry { field_b, trace_end, flux: rec.flux, reference });
        Ok(StepRecord { state, samples, ledger, diagnostics })
    }

    pub(super) fn initial(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        finite("design", design, self.n_d)?;
        let b0 = self.b.initial_state(design)?;
        finite("field-B initial state", &b0, self.n_b)?;
        let a0 = self.a.initial_state(design)?;
        finite("field-A initial state", &a0, self.n_a)?;
        let d0 = self.trace_of(&b0)?;
        let mut z = b0;
        z.extend_from_slice(&a0);
        for _ in 0..self.lag_count() {
            z.extend_from_slice(&d0);
        }
        Ok(z)
    }

    pub(super) fn initial_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        finite("design", design, self.n_d)?;
        self.check_state("initial-state cotangent", cotangent)?;
        let (cb, ca, cl) = self.split(cotangent);
        let mut cb = cb.to_vec();
        for lag in cl.chunks(self.n_t) {
            add_into(&mut cb, &self.trace_transpose(lag)?);
        }
        let mut out = self.b.initial_state_vjp(design, &cb)?;
        finite("field-B initial-state design cotangent", &out, self.n_d)?;
        let from_a = self.a.initial_state_vjp(design, ca)?;
        finite("field-A initial-state design cotangent", &from_a, self.n_d)?;
        add_into(&mut out, &from_a);
        Ok(out)
    }




    pub fn interface_schur_ratio(
        &self,
        design: &[f64],
        state: &[f64],
        modes: &[Vec<f64>],
        mass_action: &dyn Fn(&[f64]) -> Vec<f64>,
    ) -> CaeResult<Vec<f64>> {
        let p = StepParameters { design, time_scale: 1.0 };
        self.check_parameters(p)?;
        self.check_state("state", state)?;
        if modes.is_empty() {
            return Err(contract("multirate coupling: the Schur-ratio preflight needs at least one mode"));
        }
        let n = 1;
        let (b_prev, a_prev, _) = self.split(state);
        let d_s = self.trace_of(b_prev)?;
        let pred = self.predict_b(n, b_prev, p)?;
        let d_e = self.trace_of(&pred)?;
        let rec = self.run_a(n, a_prev, &d_s, &d_e, p)?;
        let (_, flux_jacobian) = self.b.current_flux_jacobians(n, &pred, b_prev, &rec.flux, p)?;
        if flux_jacobian.shape() != (self.n_b, self.n_t) {
            return Err(contract("multirate coupling: field-B flux Jacobian has the wrong shape"));
        }
        let zero_a = vec![0.0; self.n_a];
        let zero_t = vec![0.0; self.n_t];
        let mut ratios = Vec::with_capacity(modes.len());
        for (i, phi) in modes.iter().enumerate() {
            finite(&format!("mode {i}"), phi, self.n_b)?;
            let e_phi = self.trace_of(phi)?;
            let tan =
                self.a.subcycle_tangent(n, a_prev, &d_s, &d_e, p, &zero_a, &zero_t, &e_phi, None, 0.0)?;
            finite("field-A flux tangent", &tan.flux, self.n_t)?;
            let added = flux_jacobian.apply(&tan.flux, false)?;
            let inertia = mass_action(phi);
            finite(&format!("inertia action of mode {i}"), &inertia, self.n_b)?;
            let denominator = dot(phi, &inertia);
            if !(denominator > 0.0 && denominator.is_finite()) {
                return Err(contract(format!(
                    "multirate coupling: inertia pairing of mode {i} must be positive, got {denominator:e}"
                )));
            }
            ratios.push(dot(phi, &added).abs() / denominator);
        }
        if matches!(self.options.mode, CouplingMode::Loose { .. }) {
            let worst = ratios.iter().copied().fold(0.0_f64, f64::max);
            if worst > self.options.schur_ratio_limit || worst.is_nan() {
                let offending: Vec<String> = ratios
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| **r > self.options.schur_ratio_limit || r.is_nan())
                    .map(|(i, r)| format!("mode {i}: r_a = {r:.6e}"))
                    .collect();
                return Err(contract(format!(
                    "multirate coupling: loose coupling refused by the added-mass preflight: interface Schur ratio \
                     above the limit {} ({}); use a strong coupling mode (FSI_DYNAMIC_TOPOLOGY.md §3.5)",
                    self.options.schur_ratio_limit,
                    offending.join(", ")
                )));
            }
            let mut admitted =
                self.admitted.lock().map_err(|_| contract("multirate coupling: admission lock poisoned"))?;
            admitted.insert(design_key(design));
        }
        Ok(ratios)
    }



    pub fn admit_work_defect(&self, cumulative_defect: f64, energy_scale: f64) -> CaeResult<f64> {
        if !(energy_scale.is_finite() && energy_scale > 0.0 && cumulative_defect.is_finite()) {
            return Err(contract(
                "multirate coupling: work-defect admission needs a finite defect and a positive energy scale",
            ));
        }
        let relative = cumulative_defect.abs() / energy_scale;
        if matches!(self.options.mode, CouplingMode::Loose { .. })
            && relative > self.options.work_defect_limit
        {
            return Err(contract(format!(
                "multirate coupling: loose run refused: cumulative interface work defect {cumulative_defect:e} is \
                 {relative:.3e} of the energy scale, above the limit {} (FSI_DYNAMIC_TOPOLOGY.md §3.5 rule 3)",
                self.options.work_defect_limit
            )));
        }
        Ok(relative)
    }
}
