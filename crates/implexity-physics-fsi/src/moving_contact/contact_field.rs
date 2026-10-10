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

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{
    matrix::{checked_product, FnAction, Jacobian},
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
pub struct ContactClosedConstraintRows {
    pub current: Jacobian,
    pub previous: Jacobian,
    pub design: Jacobian,
    pub time: Vec<f64>,
}
pub struct ContactTraceConstraintData {
    pub contribution: ContactContribution,
    pub branches: Vec<crate::contact_constraint_tangent::ContactBranch>,
    pub closed_rows: Option<ContactClosedConstraintRows>,
    pub origins: std::collections::BTreeMap<
        usize,
        crate::contact_constraint_tangent::ContactOriginSelection,
    >,
}
pub struct ContactTraceConstraintPair {
    pub start: ContactTraceConstraintData,
    pub end: ContactTraceConstraintData,
}
pub trait NativeContactLaw: Send + Sync {
    fn check_trace_tangent_domain(
        &self,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
        _dc: &[f64],
        _dp: &[f64],
        _dd: Option<&[f64]>,
        _dt: f64,
    ) -> CaeResult<()> {
        Ok(())
    }
    fn owned_subcycle_context(
        &self,
        _n: usize,
        _trace_start: &[f64],
        _trace_end: &[f64],
        _p: StepParameters<'_>,
    ) -> CaeResult<Option<std::sync::Arc<implexity_solve::multirate_coupling::SubcyclePrimalContext>>>
    {
        Ok(None)
    }
    fn owned_trace_derivative_extension(
        &self,
        _trace: &CsrMatrix,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
        _policy: crate::contact_constraint_tangent::ContactTangentPolicy,
    ) -> CaeResult<
        Option<std::sync::Arc<dyn implexity_solve::multirate_coupling::TraceDerivativeExtension>>,
    > {
        Ok(None)
    }
    fn owned_numerical_trace_extension(
        &self,
        _trace: &CsrMatrix,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
        _direction: Option<&[f64]>,
        _policy: crate::contact_constraint_tangent::ContactTangentPolicy,
    ) -> CaeResult<
        Option<std::sync::Arc<dyn implexity_solve::multirate_coupling::TraceDerivativeExtension>>,
    > {
        Ok(None)
    }
    fn newton_direction_constraints(
        &self,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
        _direction: &[f64],
    ) -> CaeResult<Vec<(usize, f64)>> {
        Ok(Vec::new())
    }
    fn trace_constraint_data(
        &self,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
    ) -> CaeResult<Option<ContactTraceConstraintPair>> {
        Ok(None)
    }

    fn trace_constraint_data_for_direction(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        _direction: &[f64],
    ) -> CaeResult<Option<ContactTraceConstraintPair>> {
        self.trace_constraint_data(n, current, previous, p)
    }
    fn check_trace_derivative_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        self.check_derivative_domain(n, current, previous, p)
    }
    fn step_ledger(
        &self,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
    ) -> CaeResult<std::collections::BTreeMap<String, f64>> {
        Ok(std::collections::BTreeMap::new())
    }
    fn check_derivative_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()>;
    fn check_state_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()>;
    fn newton_constraints(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
        attempt: usize,
    ) -> CaeResult<Option<Jacobian>>;

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
    trace_extension_policy: Option<crate::contact_constraint_tangent::ContactTangentPolicy>,
}
fn append_matrix(
    base: &Jacobian,
    flux: &Jacobian,
    chain: &Jacobian,
    constraint: &Jacobian,
    body: usize,
    total: usize,
) -> CaeResult<Jacobian> {
    if base.shape() != (body, body)
        || flux.shape().0 != body
        || chain.shape() != (flux.shape().1, total)
        || constraint.shape() != (total - body, total)
    {
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
        Ok(Self {
            native,
            law,
            trace,
            trace_extension_policy: None,
        })
    }
    pub fn with_trace_derivative_extension(
        mut self,
        policy: crate::contact_constraint_tangent::ContactTangentPolicy,
    ) -> CaeResult<Self> {
        if !policy.rank_relative_tolerance.is_finite()
            || policy.rank_relative_tolerance < 32. * f64::EPSILON
            || policy.rank_relative_tolerance >= 1.
            || !policy.compatibility_relative_tolerance.is_finite()
            || policy.compatibility_relative_tolerance < 32. * f64::EPSILON
            || policy.compatibility_relative_tolerance >= 1.
        {
            return Err(fail("invalid contact trace extension policy"));
        }
        self.trace_extension_policy = Some(policy);
        Ok(self)
    }
    pub fn map_law<M: NativeContactLaw>(
        self,
        map: impl FnOnce(L) -> CaeResult<M>,
    ) -> CaeResult<ContactField<F, M>> {
        let multipliers = self.law.multipliers();
        let law = map(self.law)?;
        if law.multipliers() != multipliers {
            return Err(fail("contact law conversion changes multiplier layout"));
        }
        let mut field = ContactField::new(self.native, law)?;
        if field.trace.nrows() != self.trace.nrows() || field.trace.ncols() != self.trace.ncols() {
            return Err(fail("contact law conversion changes trace layout"));
        }
        field.trace = self.trace;
        field.trace_extension_policy = self.trace_extension_policy;
        Ok(field)
    }
    pub fn into_parts(self) -> (F, L) {
        (self.native, self.law)
    }
    pub fn law(&self) -> &L {
        &self.law
    }
    pub fn native(&self) -> &F {
        &self.native
    }
    pub fn check_contact_trial(
        &self,
        n: usize,
        current: &[f64],
        direction: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        trial: f64,
    ) -> CaeResult<f64> {
        self.check_states(current, previous)?;
        if direction.len() != self.state_size()
            || direction.iter().any(|v| !v.is_finite())
            || !trial.is_finite()
            || trial <= 0.
            || trial > 1.
        {
            return Err(fail("invalid contact trial"));
        }
        let fraction = self
            .law
            .admitted_trial(n, current, direction, previous, p, trial)?;
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
    fn newton_direction_constraints(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
    ) -> CaeResult<Vec<(usize, f64)>> {
        self.check_states(current, previous)?;
        if direction.len() != self.state_size() || direction.iter().any(|v| !v.is_finite()) {
            return Err(fail("contact Newton constraint direction"));
        }
        let body = self.native.state_size();
        let mut constraints = self.native.newton_direction_constraints(
            n,
            &current[..body],
            &previous[..body],
            flux,
            p,
            &direction[..body],
        )?;
        constraints.extend(
            self.law
                .newton_direction_constraints(n, current, previous, p, direction)?,
        );
        Ok(constraints)
    }
    fn subcycle_context(
        &self,
        n: usize,
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Option<std::sync::Arc<implexity_solve::multirate_coupling::SubcyclePrimalContext>>>
    {
        if self.trace_extension_policy.is_none() {
            return Ok(None);
        }
        self.law.owned_subcycle_context(n, start, end, p)
    }
    fn numerical_trace_derivative_extension(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<
        Option<std::sync::Arc<dyn implexity_solve::multirate_coupling::TraceDerivativeExtension>>,
    > {
        if let Some(policy) = self.trace_extension_policy {
            self.check_states(current, previous)?;
            if let Some(owner) = self.law.owned_numerical_trace_extension(
                &self.trace,
                n,
                current,
                previous,
                p,
                None,
                policy,
            )? {
                return Ok(Some(owner));
            }
        }
        self.trace_derivative_extension(n, current, previous, p)
    }
    fn numerical_trace_derivative_extension_for_direction(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
    ) -> CaeResult<
        Option<std::sync::Arc<dyn implexity_solve::multirate_coupling::TraceDerivativeExtension>>,
    > {
        if let Some(policy) = self.trace_extension_policy {
            self.check_states(current, previous)?;
            if let Some(owner) = self.law.owned_numerical_trace_extension(
                &self.trace,
                n,
                current,
                previous,
                p,
                Some(direction),
                policy,
            )? {
                return Ok(Some(owner));
            }
        }
        self.trace_derivative_extension_for_direction(n, current, previous, p, direction)
    }
    fn trace_derivative_extension(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<
        Option<std::sync::Arc<dyn implexity_solve::multirate_coupling::TraceDerivativeExtension>>,
    > {
        let Some(policy) = self.trace_extension_policy else {
            return Ok(None);
        };
        self.check_states(current, previous)?;
        if let Some(owner) = self.law.owned_trace_derivative_extension(
            &self.trace,
            n,
            current,
            previous,
            p,
            policy,
        )? {
            return Ok(Some(owner));
        }
        let data = self
            .law
            .trace_constraint_data(n, current, previous, p)?
            .ok_or_else(|| fail("contact law does not supply native trace constraints"))?;
        Ok(Some(std::sync::Arc::new(
            crate::contact_constraint_tangent::ContactTraceExtension::new(
                &self.trace,
                data,
                policy,
            )?,
        )))
    }

    fn check_trace_path(
        &self,
        n: usize,
        start_trace: &[f64],
        end_trace: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        self.native.check_trace_path(n, start_trace, end_trace, p)
    }
    fn check_tangent_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        dc: &[f64],
        dp: &[f64],
        dd: Option<&[f64]>,
        dt: f64,
    ) -> CaeResult<()> {
        self.check_states(current, previous)?;
        if dc.len() != current.len() || dp.len() != previous.len() {
            return Err(fail("contact physical tangent shape"));
        }
        let b = self.native.state_size();
        self.native.check_tangent_domain(
            n,
            &current[..b],
            &previous[..b],
            p,
            &dc[..b],
            &dp[..b],
            dd,
            dt,
        )?;
        if self.trace_extension_policy.is_some() {
            self.law
                .check_trace_tangent_domain(n, current, previous, p, dc, dp, dd, dt)?;
        }
        Ok(())
    }
    fn check_derivative_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        self.check_states(current, previous)?;
        let b = self.native.state_size();
        self.native
            .check_derivative_domain(n, &current[..b], &previous[..b], p)?;
        if self.trace_extension_policy.is_some() {
            self.trace_derivative_extension(n, current, previous, p)?
                .ok_or_else(|| fail("missing contact trace extension certificate"))?;
            self.law
                .check_trace_derivative_domain(n, current, previous, p)
        } else {
            self.law.check_derivative_domain(n, current, previous, p)
        }
    }
    fn check_state_domain(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<()> {
        self.check_states(current, previous)?;
        self.native.check_state_domain(
            n,
            &current[..self.native.state_size()],
            &previous[..self.native.state_size()],
            p,
        )?;
        self.law.check_state_domain(n, current, previous, p)
    }
    fn admissible_step(
        &self,
        n: usize,
        current: &[f64],
        direction: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        trial: f64,
    ) -> CaeResult<f64> {
        let mut fraction = trial;
        let b = self.native.state_size();
        for _ in 0..24 {
            let contact = self.check_contact_trial(n, current, direction, previous, p, fraction)?;
            if contact == 0. {
                return Ok(0.);
            }
            let native = self.native.admissible_step(
                n,
                &current[..b],
                &direction[..b],
                &previous[..b],
                p,
                contact,
            )?;
            if !native.is_finite() || native < 0. || native > contact {
                return Err(fail("native contact trial fraction"));
            }
            if native == 0. || native == contact {
                return Ok(native);
            }
            fraction = native;
        }
        Err(fail(
            "combined native and contact admission did not stabilize",
        ))
    }
    fn newton_alternative(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
        attempt: usize,
    ) -> CaeResult<Option<Jacobian>> {
        self.check_states(current, previous)?;
        if direction.len() != self.state_size() || direction.iter().any(|v| !v.is_finite()) {
            return Err(fail("contact Newton direction"));
        }
        let Some(rows) = self
            .law
            .newton_constraints(n, current, previous, p, direction, attempt)?
        else {
            return Ok(None);
        };
        let b = self.native.state_size();
        let total = self.state_size();
        if rows.shape() != (total - b, total) {
            return Err(fail("contact alternate row shape"));
        }
        let current = self
            .current_jacobian(n, current, previous, flux, p)?
            .to_csr()?;
        let rows = rows.to_csr()?;
        let mut ri = vec![];
        let mut ci = vec![];
        let mut vs = vec![];
        for i in 0..b {
            let (js, values) = current.row(i);
            for (&j, &v) in js.iter().zip(values) {
                ri.push(i);
                ci.push(j);
                vs.push(v);
            }
        }
        for i in 0..rows.nrows() {
            let (js, values) = rows.row(i);
            for (&j, &v) in js.iter().zip(values) {
                ri.push(b + i);
                ci.push(j);
                vs.push(v);
            }
        }
        if vs.iter().any(|v| !v.is_finite()) {
            return Err(fail("contact alternate row overflow"));
        }
        Ok(Some(Jacobian::Csr(
            CsrMatrix::from_triplets(total, total, &ri, &ci, &vs)
                .map_err(|e| fail(&e.to_string()))?,
        )))
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
    fn local_elimination_groups(&self) -> CaeResult<Option<Vec<Vec<usize>>>> {
        self.native.local_elimination_groups()
    }
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
            || before.is_some_and(|v| self.check_states(v, v).is_err())
        {
            return Vec::new();
        }
        let mut z = self
            .native
            .predict(n, &previous[..b], before.map(|v| &v[..b]), p);
        if z.len() != b || z.iter().any(|v| !v.is_finite()) {
            return Vec::new();
        }
        z.extend_from_slice(&previous[b..]);
        if self.law.check_state_domain(n, &z, previous, p).is_err() {
            return previous.to_vec();
        }
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
    fn current_jacobian(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Jacobian> {
        let c = self.contribution(n, current, previous, p)?;
        let f = self.combined_flux(flux, &c.force)?;
        let b = self.native.state_size();
        let (current, flux) =
            self.native
                .current_flux_jacobians(n, &current[..b], &previous[..b], &f, p)?;
        append_matrix(
            &current,
            &flux,
            &c.force_current,
            &c.constraint_current,
            b,
            self.state_size(),
        )
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
        if j.current.shape() != (b, b)
            || j.previous.shape() != (b, b)
            || j.design.shape() != (b, nd)
            || j.flux.shape() != (b, f.len())
            || j.time_scale.len() != b
            || j.time_scale.iter().any(|v| !v.is_finite())
        {
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
    fn trace_derivative_extension_for_direction(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        direction: &[f64],
    ) -> CaeResult<
        Option<std::sync::Arc<dyn implexity_solve::multirate_coupling::TraceDerivativeExtension>>,
    > {
        let Some(policy) = self.trace_extension_policy else {
            return Ok(None);
        };
        self.check_states(current, previous)?;
        if let Some(owner) = self.law.owned_trace_derivative_extension(
            &self.trace,
            n,
            current,
            previous,
            p,
            policy,
        )? {
            return Ok(Some(owner));
        }
        let data = self
            .law
            .trace_constraint_data_for_direction(n, current, previous, p, direction)?
            .ok_or_else(|| {
                fail("contact law does not supply native directional trace constraints")
            })?;
        Ok(Some(std::sync::Arc::new(
            crate::contact_constraint_tangent::ContactTraceExtension::new(
                &self.trace,
                data,
                policy,
            )?,
        )))
    }
    fn step_ledger(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<std::collections::BTreeMap<String, f64>> {
        self.check_states(current, previous)?;
        let b = self.native.state_size();
        let mut out = self
            .native
            .step_ledger(n, &current[..b], &previous[..b], p)?;
        for (key, value) in self.law.step_ledger(n, current, previous, p)? {
            if !value.is_finite() || out.contains_key(&key) {
                return Err(fail("native contact ledger duplicate or nonfinite value"));
            }
            out.insert(key, value);
        }
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
        if a.len() != b
            || z.len() != b
            || d.len() != self.design_size()
            || a.iter().chain(&z).chain(&d).any(|v| !v.is_finite())
        {
            return Err(fail("native contact sample cotangent shape"));
        }
        a.resize(self.state_size(), 0.);
        z.resize(self.state_size(), 0.);
        Ok((a, z, d))
    }
}
