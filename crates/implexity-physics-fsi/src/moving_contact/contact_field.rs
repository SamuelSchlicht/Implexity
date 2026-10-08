// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{
    matrix::{FnAction, Jacobian, checked_product},
    multirate_coupling::{FieldJacobians, FluxDrivenField},
    time_stepper::StepParameters,
};
use std::sync::Arc;
fn fail(s: &str) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone)]
pub struct ContactContribution {
    pub force: Vec<f64>,
    pub constraints: Vec<f64>,
    pub force_current: Jacobian,
    pub force_previous: Jacobian,
    pub force_design: Jacobian,
    pub force_time: Vec<f64>,
    pub constraint_current: Jacobian,
    pub constraint_previous: Jacobian,
    pub constraint_design: Jacobian,
    pub constraint_time: Vec<f64>,
}
pub trait NativeContactLaw: Send + Sync {
    fn step_ledger(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<std::collections::BTreeMap<String,f64>> { Ok(std::collections::BTreeMap::new()) }
    fn check_derivative_domain(&self, n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()>;
    fn check_state_domain(&self, n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()>;
    fn newton_constraints(&self, n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>, direction: &[f64], attempt: usize) -> CaeResult<Option<Jacobian>>;

    fn multipliers(&self) -> usize;
    fn evaluate(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<ContactContribution>;
    fn admitted_trial(
        &self,
        n: usize,
        current: &[f64],
        direction: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        trial: f64,
    ) -> CaeResult<f64>;
}
pub struct ContactField<F, L> {
    native: F,
    law: L,
    trace: CsrMatrix,
}
fn append_matrix(
    base: &Jacobian,
    flux: &Jacobian,
    chain: &Jacobian,
    constraint: &Jacobian,
    body: usize,
    total: usize,
) -> CaeResult<Jacobian> {
    if base.shape() != (body, body) || flux.shape().0 != body
        || chain.shape() != (flux.shape().1, total)
        || constraint.shape() != (total - body, total) {
        return Err(fail("contact current block shape"));
    }
    let a = base.to_csr()?;
    let f = flux.to_csr()?;
    let c = chain.to_csr()?;
    let g = constraint.to_csr()?;
    let mut rows = vec![];
    let mut cols = vec![];
    let mut values = vec![];
    for i in 0..body {
        let (js, vs) = a.row(i);
        for (&j, &v) in js.iter().zip(vs) {
            rows.push(i);
            cols.push(j);
            values.push(v);
        }
        let (ks, fs) = f.row(i);
        for (&k, &v) in ks.iter().zip(fs) {
            let (js, cs) = c.row(k);
            for (&j, &u) in js.iter().zip(cs) {
                rows.push(i);
                cols.push(j);
                values.push(v * u);
            }
        }
    }
    for i in 0..g.nrows() {
        let (js, vs) = g.row(i);
        for (&j, &v) in js.iter().zip(vs) {
            rows.push(body + i);
            cols.push(j);
            values.push(v);
        }
    }
    if values.iter().any(|v| !v.is_finite()) {
        return Err(fail("contact current block nonfinite"));
    }
    let result = CsrMatrix::from_triplets(total, total, &rows, &cols, &values)
        .map_err(|e| CaeError::contract(e.to_string()))?;
    Ok(Jacobian::Csr(result))
}
fn history_matrix(
    base: Jacobian,
    flux: Jacobian,
    chain: Jacobian,
    constraint: Jacobian,
    body: usize,
    columns: usize,
    native_columns: usize,
) -> Jacobian {
    let total = body + constraint.shape().0;
    let (a, f, c, g) = (
        base.clone(),
        flux.clone(),
        chain.clone(),
        constraint.clone(),
    );
    Jacobian::Operator(Arc::new(FnAction::new(
        (total, columns),
        move |x| {
            if x.len() != columns {
                return Err(fail("contact history direction shape"));
            }
            let mut y = checked_product(&a, &x[..native_columns], "native contact history", false)?;
            let z = checked_product(&c, x, "contact force direction", false)?;
            let z = checked_product(&f, &z, "native contact flux", false)?;
            for (v, w) in y.iter_mut().zip(z) {
                *v += w;
            }
            y.extend(checked_product(
                &g,
                x,
                "contact constraint direction",
                false,
            )?);
            if y.iter().any(|v| !v.is_finite()) {
                return Err(fail("contact history action nonfinite"));
            }
            Ok(y)
        },
        move |y| {
            if y.len() != total {
                return Err(fail("contact history cotangent shape"));
            }
            let mut x =
                checked_product(&base, &y[..body], "native contact history transpose", true)?;
            x.resize(columns, 0.);
            let z = checked_product(&flux, &y[..body], "native contact flux transpose", true)?;
            let z = checked_product(&chain, &z, "contact force transpose", true)?;
            let q = checked_product(
                &constraint,
                &y[body..],
                "contact constraint transpose",
                true,
            )?;
            for i in 0..columns {
                x[i] += z[i] + q[i];
            }
            if x.iter().any(|v| !v.is_finite()) {
                return Err(fail("contact history transpose nonfinite"));
            }
            Ok(x)
        },
    )))
}
impl<F: FluxDrivenField, L: NativeContactLaw> ContactField<F, L> {
    pub fn new(native: F, law: L) -> CaeResult<Self> {
        let n = native.state_size();
        let k = n
            .checked_add(law.multipliers())
            .ok_or_else(|| fail("contact state overflow"))?;
        let e = native.trace_operator();
        if e.ncols() != n {
            return Err(fail("native trace shape"));
        }
        let mut rows = vec![];
        let mut cols = vec![];
        let mut values = vec![];
        for i in 0..e.nrows() {
            let (js, vs) = e.row(i);
            for (&j, &v) in js.iter().zip(vs) {
                rows.push(i);
                cols.push(j);
                values.push(v);
            }
        }
        let trace = CsrMatrix::from_triplets(e.nrows(), k, &rows, &cols, &values)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(Self { native, law, trace })
    }
    pub fn into_parts(self)->(F,L) {(self.native,self.law)}
    pub fn law(&self)->&L {&self.law}
    pub fn native(&self) -> &F {
        &self.native
    }
    pub fn check_contact_trial(
        &self, n: usize, current: &[f64], direction: &[f64],
        previous: &[f64], p: StepParameters<'_>, trial: f64,
    ) -> CaeResult<f64> {
        self.check_states(current, previous)?;
        if direction.len() != self.state_size() || direction.iter().any(|v| !v.is_finite())
            || !trial.is_finite() || trial <= 0. || trial > 1. {
            return Err(fail("invalid contact trial"));
        }
        let fraction = self.law.admitted_trial(n, current, direction, previous, p, trial)?;
        if !fraction.is_finite() || fraction < 0. || fraction > trial {
            return Err(fail("invalid contact admission fraction"));
        }
        Ok(fraction)
    }
    fn contribution(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<ContactContribution> {
        let body = self.native.state_size();
        let total = self.state_size();
        let nc = total - body;
        let nf = self.trace.nrows();
        let nd = self.design_size();
        if current.len() != total
            || previous.len() != total
            || current.iter().chain(previous).any(|v| !v.is_finite())
        {
            return Err(fail("invalid complete contact state"));
        }
        let c = self.law.evaluate(n, current, previous, p)?;
        if c.force.len() != nf
            || c.constraints.len() != nc
            || c.force_time.len() != nf
            || c.constraint_time.len() != nc
            || c.force_current.shape() != (nf, total)
            || c.force_previous.shape() != (nf, total)
            || c.force_design.shape() != (nf, nd)
            || c.constraint_current.shape() != (nc, total)
            || c.constraint_previous.shape() != (nc, total)
            || c.constraint_design.shape() != (nc, nd)
            || c.force
                .iter()
                .chain(&c.constraints)
                .chain(&c.force_time)
                .chain(&c.constraint_time)
                .any(|v| !v.is_finite())
        {
            return Err(fail("contact contribution shape or finiteness"));
        }
        Ok(c)
    }
    fn check_states(&self, current: &[f64], previous: &[f64]) -> CaeResult<()> {
        if current.len() != self.state_size()
            || previous.len() != self.state_size()
            || current.iter().chain(previous).any(|v| !v.is_finite())
        {
            return Err(fail("invalid complete contact state"));
        }
        Ok(())
    }
    fn combined_flux(&self, external: &[f64], force: &[f64]) -> CaeResult<Vec<f64>> {
        if external.len() != force.len() {
            return Err(fail("contact external flux shape"));
        }
        let out: Vec<_> = external.iter().zip(force).map(|(a, b)| a + b).collect();
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("contact combined flux nonfinite"));
        }
        Ok(out)
    }
}
impl<F: FluxDrivenField, L: NativeContactLaw> FluxDrivenField for ContactField<F, L> {
    fn check_trace_path(&self, n: usize, start_trace: &[f64], end_trace: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        self.native.check_trace_path(n, start_trace, end_trace, p)
    }
    fn check_derivative_domain(&self, n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        self.check_states(current, previous)?;
        let b=self.native.state_size();
        self.native.check_derivative_domain(n, &current[..b], &previous[..b], p)?;
        self.law.check_derivative_domain(n, current, previous, p)
    }
    fn check_state_domain(&self, n: usize, current: &[f64], previous: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        self.check_states(current, previous)?;
        self.native.check_state_domain(n, &current[..self.native.state_size()], &previous[..self.native.state_size()], p)?;
        self.law.check_state_domain(n, current, previous, p)
    }
    fn admissible_step(&self, n: usize, current: &[f64], direction: &[f64], previous: &[f64], p: StepParameters<'_>, trial: f64) -> CaeResult<f64> {
        let mut fraction = trial;
        let b = self.native.state_size();
        for _ in 0..24 {
            let contact = self.check_contact_trial(n, current, direction, previous, p, fraction)?;
            if contact == 0. { return Ok(0.); }
            let native = self.native.admissible_step(n, &current[..b], &direction[..b], &previous[..b], p, contact)?;
            if !native.is_finite() || native < 0. || native > contact {
                return Err(fail("native contact trial fraction"));
            }
            if native == 0. || native == contact { return Ok(native); }
            fraction = native;
        }
        Err(fail("combined native and contact admission did not stabilize"))
    }
    fn newton_alternative(&self, n: usize, current: &[f64], previous: &[f64], flux: &[f64], p: StepParameters<'_>, direction: &[f64], attempt: usize) -> CaeResult<Option<Jacobian>> {
        self.check_states(current, previous)?;
        if direction.len()!=self.state_size() || direction.iter().any(|v| !v.is_finite()) {return Err(fail("contact Newton direction"));}
        let Some(rows) = self.law.newton_constraints(n,current,previous,p,direction,attempt)? else {return Ok(None)};
        let b=self.native.state_size(); let total=self.state_size();
        if rows.shape()!=(total-b,total) {return Err(fail("contact alternate row shape"));}
        let current=self.current_jacobian(n,current,previous,flux,p)?.to_csr()?;
        let rows=rows.to_csr()?;let mut ri=vec![];let mut ci=vec![];let mut vs=vec![];
        for i in 0..b {let (js,values)=current.row(i);for (&j,&v) in js.iter().zip(values){ri.push(i);ci.push(j);vs.push(v);}}
        for i in 0..rows.nrows(){let (js,values)=rows.row(i);for (&j,&v) in js.iter().zip(values){ri.push(b+i);ci.push(j);vs.push(v);}}
        if vs.iter().any(|v| !v.is_finite()){return Err(fail("contact alternate row overflow"));}
        Ok(Some(Jacobian::Csr(CsrMatrix::from_triplets(total,total,&ri,&ci,&vs).map_err(|e|fail(&e.to_string()))?)))
    }

    fn state_size(&self) -> usize {
        self.native.state_size() + self.law.multipliers()
    }
    fn design_size(&self) -> usize {
        self.native.design_size()
    }
    fn sample_names(&self) -> &[String] {
        self.native.sample_names()
    }
    fn local_elimination_groups(&self) -> CaeResult<Option<Vec<Vec<usize>>>> { self.native.local_elimination_groups() }
    fn nominal_step_s(&self) -> f64 {
        self.native.nominal_step_s()
    }
    fn trace_operator(&self) -> &CsrMatrix {
        &self.trace
    }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        let mut z = self.native.initial_state(design)?;
        if z.len() != self.native.state_size() || z.iter().any(|v| !v.is_finite()) {
            return Err(fail("invalid native initial state"));
        }
        z.resize(self.state_size(), 0.);
        Ok(z)
    }
    fn initial_state_vjp(&self, design: &[f64], bar: &[f64]) -> CaeResult<Vec<f64>> {
        if bar.len() != self.state_size() || bar.iter().any(|v| !v.is_finite()) {
            return Err(fail("contact initial cotangent shape"));
        }
        self.native
            .initial_state_vjp(design, &bar[..self.native.state_size()])
    }
    fn predict(
        &self,
        n: usize,
        previous: &[f64],
        before: Option<&[f64]>,
        p: StepParameters<'_>,
    ) -> Vec<f64> {
        let b = self.native.state_size();
        if self.check_states(previous, previous).is_err()
            || before.is_some_and(|v| self.check_states(v, v).is_err()) {
            return Vec::new();
        }
        let mut z = self
            .native
            .predict(n, &previous[..b], before.map(|v| &v[..b]), p);
        if z.len() != b || z.iter().any(|v| !v.is_finite()) {
            return Vec::new();
        }
        z.extend_from_slice(&previous[b..]);
        if self.law.check_state_domain(n,&z,previous,p).is_err(){return previous.to_vec();}
        z
    }
    fn residual(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        let c = self.contribution(n, current, previous, p)?;
        let f = self.combined_flux(flux, &c.force)?;
        let b = self.native.state_size();
        let mut r = self
            .native
            .residual(n, &current[..b], &previous[..b], &f, p)?;
        if r.len() != b || r.iter().any(|v| !v.is_finite()) {
            return Err(fail("invalid native contact residual"));
        }
        r.extend(c.constraints);
        Ok(r)
    }
    fn current_jacobian(&self, n: usize, current: &[f64], previous: &[f64], flux: &[f64], p: StepParameters<'_>) -> CaeResult<Jacobian> {
        let c = self.contribution(n, current, previous, p)?;
        let f = self.combined_flux(flux, &c.force)?;
        let b = self.native.state_size();
        let (current, flux) = self.native.current_flux_jacobians(n, &current[..b], &previous[..b], &f, p)?;
        append_matrix(&current, &flux, &c.force_current, &c.constraint_current, b, self.state_size())
    }

    fn jacobians(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<FieldJacobians> {
        let c = self.contribution(n, current, previous, p)?;
        let f = self.combined_flux(flux, &c.force)?;
        let b = self.native.state_size();
        let total = self.state_size();
        let nd = self.design_size();
        let j = self
            .native
            .jacobians(n, &current[..b], &previous[..b], &f, p)?;
        if j.current.shape() != (b, b) || j.previous.shape() != (b, b)
            || j.design.shape() != (b, nd) || j.flux.shape() != (b, f.len())
            || j.time_scale.len() != b || j.time_scale.iter().any(|v| !v.is_finite()) {
            return Err(fail("native contact Jacobian shape"));
        }
        let current = append_matrix(
            &j.current,
            &j.flux,
            &c.force_current,
            &c.constraint_current,
            b,
            total,
        )?;
        let previous = history_matrix(
            j.previous,
            j.flux.clone(),
            c.force_previous,
            c.constraint_previous,
            b,
            total,
            b,
        );
        let design = history_matrix(
            j.design,
            j.flux.clone(),
            c.force_design,
            c.constraint_design,
            b,
            nd,
            nd,
        );
        let mut time = j.time_scale;
        let ft = checked_product(&j.flux, &c.force_time, "contact time force", false)?;
        for (v, w) in time.iter_mut().zip(ft) {
            *v += w;
        }
        time.extend(c.constraint_time);
        if time.iter().any(|v| !v.is_finite()) {
            return Err(fail("contact time action nonfinite"));
        }
        let flux_native = j.flux.clone();
        let cols = flux_native.shape().1;
        let transpose = flux_native.clone();
        let flux = Jacobian::Operator(Arc::new(FnAction::new(
            (total, cols),
            move |x| {
                if x.len() != cols {
                    return Err(fail("contact external flux direction shape"));
                }
                let mut y = checked_product(&flux_native, x, "native external flux", false)?;
                y.resize(total, 0.);
                Ok(y)
            },
            move |y| {
                if y.len() != total {
                    return Err(fail("contact external flux cotangent shape"));
                }
                checked_product(&transpose, &y[..b], "native external flux transpose", true)
            },
        )));
        Ok(FieldJacobians {
            current,
            previous,
            flux,
            design,
            time_scale: time,
        })
    }
    fn step_ledger(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<std::collections::BTreeMap<String,f64>> {
        self.check_states(current,previous)?;let b=self.native.state_size();let mut out=self.native.step_ledger(n,&current[..b],&previous[..b],p)?;
        for (key,value) in self.law.step_ledger(n,current,previous,p)? { if !value.is_finite() || out.contains_key(&key) { return Err(fail("native contact ledger duplicate or nonfinite value")); } out.insert(key,value); }
        Ok(out)
    }
    fn samples(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        self.check_states(current, previous)?;
        let b = self.native.state_size();
        self.native.samples(n, &current[..b], &previous[..b], p)
    }
    fn samples_vjp(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> {
        self.check_states(current, previous)?;
        let b = self.native.state_size();
        let (mut a, mut z, d) =
            self.native
                .samples_vjp(n, &current[..b], &previous[..b], p, bar)?;
        if a.len() != b || z.len() != b || d.len() != self.design_size()
            || a.iter().chain(&z).chain(&d).any(|v| !v.is_finite()) {
            return Err(fail("native contact sample cotangent shape"));
        }
        a.resize(self.state_size(), 0.);
        z.resize(self.state_size(), 0.);
        Ok((a, z, d))
    }
}
