// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};

use super::cache::step_key;
use super::linear::{CurrentFactor, KrylovLimits, KrylovSystem, add_into, dot, fgmres, negated, sub_into};
use super::{
    CouplingMode, FieldJacobians, FluxDrivenField, MultirateStepper, SubcycleCotangent, SubcycleTangent,
    SubcycledField, contract,
};
use crate::interface_quasi_newton::SecantInverse;
use crate::time_stepper::{StepCotangent, StepParameters, StepTangent};

pub(super) struct Linearization {
    pub(super) jac: FieldJacobians,
    pub(super) factor: CurrentFactor,
    pub(super) flux: Vec<f64>,
}

impl Linearization {
    pub(super) fn new<A: SubcycledField, B: FluxDrivenField>(
        st: &MultirateStepper<A, B>,
        jac: FieldJacobians,
        flux: Vec<f64>,
    ) -> CaeResult<Self> {
        let (nb, nt, nd) = (st.n_b, st.n_t, st.n_d);
        let expect = |label: &str, got: (usize, usize), want: (usize, usize)| {
            if got == want {
                Ok(())
            } else {
                Err(contract(format!(
                    "multirate coupling: field-B {label} Jacobian has shape {got:?}, expected {want:?}"
                )))
            }
        };
        expect("previous", jac.previous.shape(), (nb, nb))?;
        expect("flux", jac.flux.shape(), (nb, nt))?;
        expect("design", jac.design.shape(), (nb, nd))?;
        if jac.time_scale.len() != nb || jac.time_scale.iter().any(|v| !v.is_finite()) {
            return Err(contract(
                "multirate coupling: field-B time-scale Jacobian must be finite with the state length",
            ));
        }
        let factor = CurrentFactor::new(&jac.current, nb, st.b.local_elimination_groups()?)?;
        Ok(Self { jac, factor, flux })
    }
}

pub(super) struct CoupledPoint<'p> {
    pub(super) n: usize,
    pub(super) a_prev: &'p [f64],
    pub(super) d_s: &'p [f64],
    pub(super) d_e: &'p [f64],
    pub(super) p: StepParameters<'p>,
}

fn flatten(c: &SubcycleCotangent) -> Vec<f64> {
    let mut out = Vec::with_capacity(c.previous.len() + 2 * c.trace_start.len() + c.design.len() + 1);
    out.extend_from_slice(&c.previous);
    out.extend_from_slice(&c.trace_start);
    out.extend_from_slice(&c.trace_end);
    out.extend_from_slice(&c.design);
    out.push(c.time_scale);
    out
}

struct FlatCotangent<'c> {
    previous: &'c [f64],
    trace_start: &'c [f64],
    design: &'c [f64],
    time_scale: f64,
}

fn unflatten(v: &[f64], na: usize, nt: usize) -> FlatCotangent<'_> {
    FlatCotangent {
        previous: &v[..na],
        trace_start: &v[na..na + nt],
        design: &v[na + 2 * nt..v.len() - 1],
        time_scale: v[v.len() - 1],
    }
}

struct CoupledTrace<'s, 'p, A, B> {
    st: &'s MultirateStepper<A, B>,
    point: &'s CoupledPoint<'p>,
    lin: &'s Linearization,
    secant: &'s mut SecantInverse,
    transpose: bool,
}

impl<A: SubcycledField, B: FluxDrivenField> KrylovSystem for CoupledTrace<'_, '_, A, B> {
    fn aux_len(&self) -> usize {
        if self.transpose { self.st.n_a + 2 * self.st.n_t + self.st.n_d + 1 } else { self.st.n_t }
    }

    fn matvec(&mut self, v: &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let st = self.st;
        let pt = self.point;
        let (y, aux) = if self.transpose {
            let w = self.lin.factor.solve(&st.trace_transpose(v)?, true)?;
            let flux_bar = self.lin.jac.flux.apply(&w, true)?;
            let cot = st.a_adjoint(pt, &vec![0.0; st.n_a], &flux_bar, &vec![0.0; st.a_samples()])?;
            let mut y = v.to_vec();
            add_into(&mut y, &cot.trace_end);
            (y, flatten(&cot))
        } else {
            let tan = st.a_tangent(pt, &vec![0.0; st.n_a], &vec![0.0; st.n_t], v, None, 0.0)?;
            let w = self.lin.jac.flux.apply(&tan.flux, false)?;
            let u = self.lin.factor.solve(&w, false)?;
            let mut y = v.to_vec();
            add_into(&mut y, &st.trace_of(&u)?);
            (y, tan.flux)
        };
        if v.iter().any(|x| *x != 0.0) {
            self.secant.push(v, &y, &y)?;
        }
        Ok((y, aux))
    }

    fn precondition(&mut self, x: &[f64]) -> CaeResult<Vec<f64>> {
        if self.secant.rank() == 0 { Ok(x.to_vec()) } else { self.secant.apply(x, x) }
    }
}

struct TangentParts {
    field_b: Vec<f64>,
    trace_end: Vec<f64>,
    field_a: SubcycleTangent,
}

fn check_len(label: &str, v: &[f64], size: usize) -> CaeResult<()> {
    if v.len() != size {
        return Err(contract(format!("multirate coupling: {label} has length {}, expected {size}", v.len())));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(contract(format!("multirate coupling: {label} must be finite")));
    }
    Ok(())
}

fn explicit_rhs(
    lin: &Linearization,
    d_b_prev: &[f64],
    d_design: Option<&[f64]>,
    d_tau: f64,
) -> CaeResult<Vec<f64>> {
    let mut rhs = lin.jac.previous.apply(d_b_prev, false)?;
    if let Some(dx) = d_design {
        add_into(&mut rhs, &lin.jac.design.apply(dx, false)?);
    }
    if d_tau != 0.0 {
        for (r, t) in rhs.iter_mut().zip(&lin.jac.time_scale) {
            *r += t * d_tau;
        }
    }
    Ok(negated(&rhs))
}

fn scaled(alpha: f64, x: &[f64]) -> Vec<f64> {
    x.iter().map(|v| alpha * v).collect()
}

impl<A: SubcycledField, B: FluxDrivenField> MultirateStepper<A, B> {
    fn a_samples(&self) -> usize {
        self.sample_names.len() - self.ns_b
    }

    fn a_tangent(
        &self,
        pt: &CoupledPoint<'_>,
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<SubcycleTangent> {
        let t = self.a.subcycle_tangent(
            pt.n,
            pt.a_prev,
            pt.d_s,
            pt.d_e,
            pt.p,
            d_previous,
            d_trace_start,
            d_trace_end,
            d_design,
            d_time_scale,
        )?;
        check_len("field-A state tangent", &t.state, self.n_a)?;
        check_len("field-A flux tangent", &t.flux, self.n_t)?;
        check_len("field-A sample tangent", &t.samples, self.a_samples())?;
        Ok(t)
    }

    fn a_adjoint(
        &self,
        pt: &CoupledPoint<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        let c = self
            .a
            .subcycle_adjoint(pt.n, pt.a_prev, pt.d_s, pt.d_e, pt.p, state_bar, flux_bar, sample_bar)?;
        self.check_a_cotangent(&c)?;
        Ok(c)
    }

    fn check_a_cotangent(&self, c: &SubcycleCotangent) -> CaeResult<()> {
        check_len("field-A previous-state cotangent", &c.previous, self.n_a)?;
        check_len("field-A start-trace cotangent", &c.trace_start, self.n_t)?;
        check_len("field-A end-trace cotangent", &c.trace_end, self.n_t)?;
        check_len("field-A design cotangent", &c.design, self.n_d)?;
        if !c.time_scale.is_finite() {
            return Err(contract("multirate coupling: field-A time-scale cotangent must be finite"));
        }
        Ok(())
    }

    fn strong(&self) -> bool {
        !matches!(self.options.mode, CouplingMode::Loose { .. })
    }

    fn fresh_secant(&self) -> CaeResult<SecantInverse> {
        Ok(SecantInverse::new(self.n_t, 0, self.secant_filter)?.with_max_columns(self.linear.restart))
    }

    fn linear_limits(&self) -> KrylovLimits {
        KrylovLimits {
            rtol: self.linear.relative_tolerance,
            restart: self.linear.restart,
            max_iterations: self.linear.max_iterations,
        }
    }

    pub(super) fn coupled_solve(
        &self,
        point: &CoupledPoint<'_>,
        lin: &Linearization,
        rhs: &[f64],
        secant: &mut SecantInverse,
        limits: KrylovLimits,
    ) -> CaeResult<(Vec<f64>, usize)> {
        let u = lin.factor.solve(rhs, false)?;
        let eu = self.trace_of(&u)?;
        let mut system = CoupledTrace { st: self, point, lin, secant, transpose: false };
        let out = fgmres(&mut system, &eu, limits)?;
        if !out.converged {
            return Err(CaeError::convergence(format!(
                "multirate coupling: coupled trace solve not certified after {} Krylov iterations \
                 (true relative residual {:e} > {:e})",
                out.iterations, out.relative_residual, limits.rtol
            )));
        }
        let w = lin.jac.flux.apply(&out.aux, false)?;
        let mut delta = u;
        sub_into(&mut delta, &lin.factor.solve(&w, false)?);
        Ok((delta, out.iterations))
    }

    fn coupled_solve_transpose(
        &self,
        point: &CoupledPoint<'_>,
        lin: &Linearization,
        g: &[f64],
        secant: &mut SecantInverse,
    ) -> CaeResult<(Vec<f64>, Vec<f64>)> {
        let u = lin.factor.solve(g, true)?;
        let y0 = lin.jac.flux.apply(&u, true)?;
        let c0 = self.a_adjoint(point, &vec![0.0; self.n_a], &y0, &vec![0.0; self.a_samples()])?;
        let limits = self.linear_limits();
        let mut system = CoupledTrace { st: self, point, lin, secant, transpose: true };
        let out = fgmres(&mut system, &c0.trace_end, limits)?;
        if !out.converged {
            return Err(CaeError::convergence(format!(
                "multirate coupling: transposed coupled trace solve not certified after {} Krylov iterations \
                 (true relative residual {:e} > {:e})",
                out.iterations, out.relative_residual, limits.rtol
            )));
        }
        let mut lambda = u;
        sub_into(&mut lambda, &lin.factor.solve(&self.trace_transpose(&out.x)?, true)?);
        let mut cot = out.aux;
        sub_into(&mut cot, &flatten(&c0));
        Ok((lambda, cot))
    }

    fn linearize(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<(Linearization, Vec<f64>, Vec<f64>)> {
        self.check_state("previous state", previous)?;
        self.check_state("current state", current)?;
        self.check_parameters(p)?;
        let (b_prev, a_prev, lags) = self.split(previous);
        let (b, _, _) = self.split(current);
        self.b.check_state_domain(n, b, b_prev, p)?;
        self.b.check_derivative_domain(n, b, b_prev, p)?;
        let d_s = self.trace_of(b_prev)?;
        let d_e = if self.strong() { self.trace_of(b)? } else { self.predicted_trace(&d_s, lags) };
        let flux = match self.cached(&step_key(n, previous, p)) {
            Some(entry) if entry.field_b == b && entry.trace_end == d_e => entry.flux,
            _ => self.run_a(n, a_prev, &d_s, &d_e, p)?.flux,
        };
        let jac = self.b.jacobians(n, b, b_prev, &flux, p)?;
        Ok((Linearization::new(self, jac, flux)?, d_s, d_e))
    }

    #[allow(clippy::too_many_arguments)]
    fn tangent_parts(
        &self,
        point: &CoupledPoint<'_>,
        lin: &Linearization,
        d_b_prev: &[f64],
        d_a_prev: &[f64],
        d_lags: &[f64],
        d_design: Option<&[f64]>,
        d_tau: f64,
    ) -> CaeResult<TangentParts> {
        let e_db_prev = self.trace_of(d_b_prev)?;
        let base = explicit_rhs(lin, d_b_prev, d_design, d_tau)?;
        if self.strong() {
            let t0 = self.a_tangent(point, d_a_prev, &e_db_prev, &vec![0.0; self.n_t], d_design, d_tau)?;
            let mut rhs = base;
            sub_into(&mut rhs, &lin.jac.flux.apply(&t0.flux, false)?);
            let mut secant = self.fresh_secant()?;
            let (db, _) = self.coupled_solve(point, lin, &rhs, &mut secant, self.linear_limits())?;
            let trace_end = self.trace_of(&db)?;
            let field_a = self.a_tangent(point, d_a_prev, &e_db_prev, &trace_end, d_design, d_tau)?;
            Ok(TangentParts { field_b: db, trace_end, field_a })
        } else {
            let trace_end = self.predicted_trace(&e_db_prev, d_lags);
            let field_a = self.a_tangent(point, d_a_prev, &e_db_prev, &trace_end, d_design, d_tau)?;
            let mut rhs = base;
            sub_into(&mut rhs, &lin.jac.flux.apply(&field_a.flux, false)?);
            let db = lin.factor.solve(&rhs, false)?;
            Ok(TangentParts { field_b: db, trace_end, field_a })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_tangent_b(
        &self,
        n: usize,
        b: &[f64],
        b_prev: &[f64],
        p: StepParameters<'_>,
        db: &[f64],
        db_prev: &[f64],
        dx: Option<&[f64]>,
    ) -> CaeResult<Vec<f64>> {
        let mut out = Vec::with_capacity(self.ns_b);
        for j in 0..self.ns_b {
            let mut e = vec![0.0; self.ns_b];
            e[j] = 1.0;
            let (cc, cp, cx) = self.samples_vjp_b(n, b, b_prev, p, &e)?;
            let mut v = dot(&cc, db) + dot(&cp, db_prev);
            if let Some(dx) = dx {
                v += dot(&cx, dx);
            }
            out.push(v);
        }
        Ok(out)
    }

    fn samples_vjp_b(
        &self,
        n: usize,
        b: &[f64],
        b_prev: &[f64],
        p: StepParameters<'_>,
        bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> {
        if bar.iter().all(|v| *v == 0.0) {
            return Ok((vec![0.0; self.n_b], vec![0.0; self.n_b], vec![0.0; self.n_d]));
        }
        let (cc, cp, cx) = self.b.samples_vjp(n, b, b_prev, p, bar)?;
        check_len("field-B sample cotangent (current)", &cc, self.n_b)?;
        check_len("field-B sample cotangent (previous)", &cp, self.n_b)?;
        check_len("field-B sample cotangent (design)", &cx, self.n_d)?;
        Ok((cc, cp, cx))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn tangent_step(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<StepTangent> {
        self.check_state("previous-state tangent", d_previous)?;
        if let Some(dx) = d_design {
            check_len("design tangent", dx, self.n_d)?;
        }
        if !d_time_scale.is_finite() {
            return Err(contract("multirate coupling: time-scale tangent must be finite"));
        }
        let (lin, d_s, d_e) = self.linearize(n, previous, current, p)?;
        let (b_prev, a_prev, _) = self.split(previous);
        let (b, _, _) = self.split(current);
        let (db_prev, da_prev, dlags) = self.split(d_previous);
        let point = CoupledPoint { n, a_prev, d_s: &d_s, d_e: &d_e, p };
        let parts = self.tangent_parts(&point, &lin, db_prev, da_prev, dlags, d_design, d_time_scale)?;
        let mut samples = self.sample_tangent_b(n, b, b_prev, p, &parts.field_b, db_prev, d_design)?;
        samples.extend_from_slice(&parts.field_a.samples);
        let mut state = parts.field_b;
        state.extend_from_slice(&parts.field_a.state);
        state.extend(self.shifted_lags(&self.trace_of(db_prev)?, dlags));
        Ok(StepTangent { state, samples })
    }

    pub(super) fn adjoint_steps(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        state_bars: &[Vec<f64>],
        sample_bars: &[Vec<f64>],
    ) -> CaeResult<Vec<StepCotangent>> {
        if state_bars.len() != sample_bars.len() {
            return Err(contract(
                "multirate coupling: adjoint batch needs one sample cotangent per state cotangent",
            ));
        }
        for (zbar, sbar) in state_bars.iter().zip(sample_bars) {
            self.check_state("state cotangent", zbar)?;
            check_len("sample cotangent", sbar, self.sample_names.len())?;
        }
        if state_bars.is_empty() {
            return Ok(Vec::new());
        }
        let (lin, d_s, d_e) = self.linearize(n, previous, current, p)?;
        let (b_prev, a_prev, _) = self.split(previous);
        let (b, _, _) = self.split(current);
        let point = CoupledPoint { n, a_prev, d_s: &d_s, d_e: &d_e, p };
        let mut secant = self.fresh_secant()?;
        let mut out = Vec::with_capacity(state_bars.len());
        for (zbar, sbar) in state_bars.iter().zip(sample_bars) {
            let (bbar, abar, lbar) = self.split(zbar);
            let (sbar_b, sbar_a) = sbar.split_at(self.ns_b);
            let (cb, cbp, cx) = self.samples_vjp_b(n, b, b_prev, p, sbar_b)?;
            let mut prev_b = cbp;
            let mut design = cx;
            let (prev_a, time_scale, lags) = if self.strong() {
                let a1 = self.a_adjoint(&point, abar, &vec![0.0; self.n_t], sbar_a)?;
                let mut g = bbar.to_vec();
                add_into(&mut g, &cb);
                add_into(&mut g, &self.trace_transpose(&a1.trace_end)?);
                let (lambda, cflat) = self.coupled_solve_transpose(&point, &lin, &g, &mut secant)?;
                let c = unflatten(&cflat, self.n_a, self.n_t);
                sub_into(&mut prev_b, &lin.jac.previous.apply(&lambda, true)?);
                let mut ts = a1.trace_start.clone();
                add_into(&mut ts, c.trace_start);
                add_into(&mut prev_b, &self.trace_transpose(&ts)?);
                add_into(&mut design, &a1.design);
                add_into(&mut design, c.design);
                sub_into(&mut design, &lin.jac.design.apply(&lambda, true)?);
                let mut prev_a = a1.previous;
                add_into(&mut prev_a, c.previous);
                let tau = a1.time_scale + c.time_scale - dot(&lin.jac.time_scale, &lambda);
                (prev_a, tau, Vec::new())
            } else {
                let mut w = bbar.to_vec();
                add_into(&mut w, &cb);
                let lambda = lin.factor.solve(&w, true)?;
                let flux_bar = negated(&lin.jac.flux.apply(&lambda, true)?);
                let a1 = self.a_adjoint(&point, abar, &flux_bar, sbar_a)?;
                let (lag_bars, trace_bar) = self.loose_lag_pullback(&a1.trace_end, lbar);
                sub_into(&mut prev_b, &lin.jac.previous.apply(&lambda, true)?);
                let mut ts = a1.trace_start.clone();
                add_into(&mut ts, &trace_bar);
                add_into(&mut prev_b, &self.trace_transpose(&ts)?);
                add_into(&mut design, &a1.design);
                sub_into(&mut design, &lin.jac.design.apply(&lambda, true)?);
                let tau = a1.time_scale - dot(&lin.jac.time_scale, &lambda);
                (a1.previous, tau, lag_bars)
            };
            let mut previous_bar = prev_b;
            previous_bar.extend_from_slice(&prev_a);
            previous_bar.extend_from_slice(&lags);
            out.push(StepCotangent { previous: previous_bar, design, time_scale });
        }
        Ok(out)
    }

    fn loose_lag_pullback(&self, te: &[f64], lbar: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let p = self.lag_count();
        let nt = self.n_t;
        let mut trace_bar = scaled(self.lag_coefficients[0], te);
        let mut lag_bars = vec![0.0; p * nt];
        if p > 0 {
            add_into(&mut trace_bar, &lbar[..nt]);
            for j in 0..p {
                let c = self.lag_coefficients[j + 1];
                let slot = &mut lag_bars[j * nt..(j + 1) * nt];
                for (s, t) in slot.iter_mut().zip(te) {
                    *s += c * t;
                }
                if j + 1 < p {
                    add_into(slot, &lbar[(j + 1) * nt..(j + 2) * nt]);
                }
            }
        }
        (lag_bars, trace_bar)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn adjoint_tangent_step(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        sample_bar: &[f64],
        d_previous: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<StepCotangent> {
        let (Some(a2), Some(b2)) = (self.a.second_order(), self.b.second_order()) else {
            return Err(contract("multirate coupling: both fields must provide second-order capabilities"));
        };
        self.check_state("state cotangent", state_bar)?;
        check_len("sample cotangent", sample_bar, self.sample_names.len())?;
        self.check_state("previous-state direction", d_previous)?;
        if let Some(dx) = d_design {
            check_len("design direction", dx, self.n_d)?;
        }
        let (sbar_b, sbar_a) = sample_bar.split_at(self.ns_b);
        if sbar_b.iter().any(|v| *v != 0.0) {
            return Err(contract(
                "multirate coupling: second-order adjoint with field-B sample cotangents is not available \
                 (field-B samples publish first derivatives only)",
            ));
        }
        let (lin, d_s, d_e) = self.linearize(n, previous, current, p)?;
        let (b_prev, a_prev, _) = self.split(previous);
        let (b, _, _) = self.split(current);
        let (bbar, abar, lbar) = self.split(state_bar);
        let (db_prev, da_prev, dlags) = self.split(d_previous);
        let point = CoupledPoint { n, a_prev, d_s: &d_s, d_e: &d_e, p };
        let parts = self.tangent_parts(&point, &lin, db_prev, da_prev, dlags, d_design, 0.0)?;
        let e_db_prev = self.trace_of(db_prev)?;
        let zero_a = vec![0.0; self.n_a];
        let zero_s = vec![0.0; self.a_samples()];
        let second_residual = |w: &[f64]| {
            let rt = b2.residual_adjoint_tangent(
                n,
                b,
                b_prev,
                &lin.flux,
                p,
                w,
                &parts.field_b,
                db_prev,
                &parts.field_a.flux,
                d_design,
            )?;
            check_len("second-order residual pullback (current)", &rt.current, self.n_b)?;
            check_len("second-order residual pullback (previous)", &rt.previous, self.n_b)?;
            check_len("second-order residual pullback (flux)", &rt.flux, self.n_t)?;
            check_len("second-order residual pullback (design)", &rt.design, self.n_d)?;
            if !rt.time_scale.is_finite() {
                return Err(contract(
                    "multirate coupling: second-order residual time-scale pullback must be finite",
                ));
            }
            Ok::<_, CaeError>(rt)
        };
        let second_a = |state_bar: &[f64], flux_bar: &[f64]| {
            let h = a2.subcycle_adjoint_tangent(
                n,
                a_prev,
                &d_s,
                &d_e,
                p,
                state_bar,
                flux_bar,
                sbar_a,
                da_prev,
                &e_db_prev,
                &parts.trace_end,
                d_design,
            )?;
            self.check_a_cotangent(&h)?;
            Ok::<_, CaeError>(h)
        };
        let mut prev_b;
        let mut design;
        let prev_a;
        let tau;
        let mut lags = Vec::new();
        if self.strong() {
            let a1 = self.a_adjoint(&point, abar, &vec![0.0; self.n_t], sbar_a)?;
            let mut g = bbar.to_vec();
            add_into(&mut g, &self.trace_transpose(&a1.trace_end)?);
            let mut secant = self.fresh_secant()?;
            let (lambda, _) = self.coupled_solve_transpose(&point, &lin, &g, &mut secant)?;
            let y = negated(&lin.jac.flux.apply(&lambda, true)?);
            let h = second_a(abar, &y)?;
            let rt = second_residual(&lambda)?;
            let d = self.a_adjoint(&point, &zero_a, &rt.flux, &zero_s)?;
            let mut te = h.trace_end.clone();
            sub_into(&mut te, &d.trace_end);
            let mut rhs2 = self.trace_transpose(&te)?;
            sub_into(&mut rhs2, &rt.current);
            let (dlambda, cflat) = self.coupled_solve_transpose(&point, &lin, &rhs2, &mut secant)?;
            let c = unflatten(&cflat, self.n_a, self.n_t);
            let mut pa = h.previous.clone();
            sub_into(&mut pa, &d.previous);
            add_into(&mut pa, c.previous);
            prev_a = pa;
            let mut ts = h.trace_start.clone();
            sub_into(&mut ts, &d.trace_start);
            add_into(&mut ts, c.trace_start);
            prev_b = negated(&rt.previous);
            sub_into(&mut prev_b, &lin.jac.previous.apply(&dlambda, true)?);
            add_into(&mut prev_b, &self.trace_transpose(&ts)?);
            design = h.design.clone();
            sub_into(&mut design, &d.design);
            add_into(&mut design, c.design);
            sub_into(&mut design, &rt.design);
            sub_into(&mut design, &lin.jac.design.apply(&dlambda, true)?);
            tau = h.time_scale - d.time_scale + c.time_scale
                - rt.time_scale
                - dot(&lin.jac.time_scale, &dlambda);
        } else {
            let lambda = lin.factor.solve(bbar, true)?;
            let y = negated(&lin.jac.flux.apply(&lambda, true)?);
            let rt = second_residual(&lambda)?;
            let dlambda = negated(&lin.factor.solve(&rt.current, true)?);
            let mut dy = negated(&rt.flux);
            sub_into(&mut dy, &lin.jac.flux.apply(&dlambda, true)?);
            let h = second_a(abar, &y)?;
            let dyc = self.a_adjoint(&point, &zero_a, &dy, &zero_s)?;
            let mut te = h.trace_end.clone();
            add_into(&mut te, &dyc.trace_end);
            let (lag_bars, trace_bar) = self.loose_lag_pullback(&te, &vec![0.0; lbar.len()]);
            lags = lag_bars;
            let mut pa = h.previous.clone();
            add_into(&mut pa, &dyc.previous);
            prev_a = pa;
            let mut ts = h.trace_start.clone();
            add_into(&mut ts, &dyc.trace_start);
            add_into(&mut ts, &trace_bar);
            prev_b = negated(&rt.previous);
            sub_into(&mut prev_b, &lin.jac.previous.apply(&dlambda, true)?);
            add_into(&mut prev_b, &self.trace_transpose(&ts)?);
            design = h.design.clone();
            add_into(&mut design, &dyc.design);
            sub_into(&mut design, &rt.design);
            sub_into(&mut design, &lin.jac.design.apply(&dlambda, true)?);
            tau = h.time_scale + dyc.time_scale - rt.time_scale - dot(&lin.jac.time_scale, &dlambda);
        }
        let mut previous_bar = prev_b;
        previous_bar.extend_from_slice(&prev_a);
        previous_bar.extend_from_slice(&lags);
        Ok(StepCotangent { previous: previous_bar, design, time_scale: tau })
    }
}
