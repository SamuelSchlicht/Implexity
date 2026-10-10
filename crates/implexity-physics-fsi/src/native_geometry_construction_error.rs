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

#[derive(Clone, Copy, Debug)]
struct Component {
    value: f64,
    error: f64,
}

type Result<T> = std::result::Result<T, &'static str>;

fn admitted(x: f64) -> Result<f64> {
    if x.is_finite() && (x == 0.0 || x.is_normal()) {
        Ok(x)
    } else {
        Err("construction arithmetic overflow or subnormal")
    }
}

fn upper(x: f64) -> Result<f64> {
    admitted(x)?;
    if x < 0.0 {
        return Err("negative construction error");
    }
    if x == 0.0 {
        Ok(0.0)
    } else {
        admitted(f64::from_bits(x.to_bits() + 1))
    }
}

fn bound_add(a: f64, b: f64) -> Result<f64> {
    upper(a + b)
}

fn bound_mul(a: f64, b: f64) -> Result<f64> {
    let q = a * b;
    if q == 0.0 && a != 0.0 && b != 0.0 {
        return Err("construction error underflow");
    }
    upper(q)
}

fn bound_div(a: f64, b: f64) -> Result<f64> {
    let q = a / b;
    if q == 0.0 && a != 0.0 {
        return Err("construction error quotient underflow");
    }
    upper(q)
}

fn downward_positive(x: f64) -> Result<f64> {
    admitted(x)?;
    if x <= 0.0 {
        return Err("construction positive lower bound is absent");
    }
    admitted(f64::from_bits(x.to_bits() - 1))
}

fn rounding(x: f64) -> Result<f64> {
    bound_mul(
        x.abs(),
        f64::from_bits((2.0_f64.powi(-53) / (1.0 - 2.0_f64.powi(-53))).to_bits() + 1),
    )
}

impl Component {
    fn input(value: f64) -> Result<Self> {
        Ok(Self {
            value: admitted(value)?,
            error: 0.0,
        })
    }

    fn neg(self) -> Self {
        Self {
            value: -self.value,
            error: self.error,
        }
    }

    fn add(self, b: Self) -> Result<Self> {
        let value = admitted(self.value + b.value)?;
        let error = bound_add(bound_add(self.error, b.error)?, rounding(value)?)?;
        Ok(Self { value, error })
    }

    fn sub(self, b: Self) -> Result<Self> {
        self.add(b.neg())
    }

    fn mul(self, b: Self) -> Result<Self> {
        let value = admitted(self.value * b.value)?;
        if value == 0.0 && self.value != 0.0 && b.value != 0.0 {
            return Err("construction product underflow");
        }
        let upstream = bound_add(
            bound_add(
                bound_mul(self.value.abs(), b.error)?,
                bound_mul(b.value.abs(), self.error)?,
            )?,
            bound_mul(self.error, b.error)?,
        )?;
        Ok(Self {
            value,
            error: bound_add(upstream, rounding(value)?)?,
        })
    }

    fn div(self, b: Self) -> Result<Self> {
        let lower_raw = admitted(b.value.abs() - b.error)?;
        if lower_raw <= 0.0 {
            return Err("construction denominator not separated from zero");
        }
        let lower = f64::from_bits(lower_raw.to_bits() - 1);
        if !lower.is_normal() {
            return Err("construction denominator lower bound subnormal");
        }
        let value = admitted(self.value / b.value)?;
        if value == 0.0 && self.value != 0.0 {
            return Err("construction quotient underflow");
        }
        let quotient_magnitude = bound_add(value.abs(), rounding(value)?)?;
        let numerator = bound_add(self.error, bound_mul(quotient_magnitude, b.error)?)?;
        let upstream = bound_div(numerator, lower)?;
        Ok(Self {
            value,
            error: bound_add(upstream, rounding(value)?)?,
        })
    }

    fn sqrt(self) -> Result<Self> {
        let lower_raw = admitted(self.value - self.error)?;
        if lower_raw <= 0.0 {
            return Err("construction square root not separated from zero");
        }
        let lower = downward_positive(lower_raw)?;
        let value = admitted(self.value.sqrt())?;
        let root_value_lower = downward_positive(value)?;
        let root_interval_lower = downward_positive(lower.sqrt())?;
        let denominator_lower = downward_positive(root_value_lower + root_interval_lower)?;
        if !denominator_lower.is_normal() {
            return Err("construction square root conditioning");
        }
        let upstream = bound_div(self.error, denominator_lower)?;
        Ok(Self {
            value,
            error: bound_add(upstream, rounding(value)?)?,
        })
    }

    fn reconcile(self, actual: f64) -> Result<Self> {
        admitted(actual)?;
        let difference = admitted((actual - self.value).abs())?;
        let difference_bound = if difference == 0.0 {
            0.0
        } else {
            bound_add(difference, rounding(difference)?)?
        };
        Ok(Self {
            value: actual,
            error: bound_add(self.error, difference_bound)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JetLayout {
    FirstOrder(usize),
    TimeMixed(usize),
}

#[derive(Clone, Debug)]
pub(crate) struct ConstructionJet {
    layout: JetLayout,
    components: Vec<Component>,
}

impl ConstructionJet {
    pub(crate) fn input(layout: JetLayout, observed: &[f64]) -> Result<Self> {
        let width = match layout {
            JetLayout::FirstOrder(n) => 1 + n,
            JetLayout::TimeMixed(n) => 2 + 2 * n,
        };
        if observed.len() != width || width > 98 {
            return Err("construction jet shape outside bounded geometry scope");
        }
        Ok(Self {
            layout,
            components: observed
                .iter()
                .map(|&v| Component::input(v))
                .collect::<Result<_>>()?,
        })
    }

    fn compatible(&self, b: &Self) -> Result<()> {
        if self.layout == b.layout {
            Ok(())
        } else {
            Err("construction jet layout mismatch")
        }
    }

    pub(crate) fn reconcile(mut self, observed: &[f64]) -> Result<Self> {
        if observed.len() != self.components.len() {
            return Err("construction observation shape mismatch");
        }
        for (component, &actual) in self.components.iter_mut().zip(observed) {
            *component = component.reconcile(actual)?;
        }
        Ok(self)
    }

    pub(crate) fn add(&self, b: &Self) -> Result<Self> {
        self.compatible(b)?;
        Ok(Self {
            layout: self.layout,
            components: self
                .components
                .iter()
                .zip(&b.components)
                .map(|(&a, &b)| a.add(b))
                .collect::<Result<_>>()?,
        })
    }

    pub(crate) fn neg(&self) -> Self {
        Self {
            layout: self.layout,
            components: self.components.iter().map(|&v| v.neg()).collect(),
        }
    }

    pub(crate) fn sub(&self, b: &Self) -> Result<Self> {
        self.add(&b.neg())
    }

    pub(crate) fn mul(&self, b: &Self) -> Result<Self> {
        self.compatible(b)?;
        let a = &self.components;
        let b = &b.components;
        let n = match self.layout {
            JetLayout::FirstOrder(n) | JetLayout::TimeMixed(n) => n,
        };
        let mut out = vec![a[0].mul(b[0])?];
        for j in 1..=n {
            out.push(a[j].mul(b[0])?.add(a[0].mul(b[j])?)?);
        }
        if let JetLayout::TimeMixed(_) = self.layout {
            let t = n + 1;
            out.push(a[t].mul(b[0])?.add(a[0].mul(b[t])?)?);
            for j in 1..=n {
                out.push(
                    a[t + j]
                        .mul(b[0])?
                        .add(a[t].mul(b[j])?)?
                        .add(a[j].mul(b[t])?)?
                        .add(a[0].mul(b[t + j])?)?,
                );
            }
        }
        Ok(Self {
            layout: self.layout,
            components: out,
        })
    }

    pub(crate) fn div(&self, rhs: &Self) -> Result<Self> {
        self.compatible(rhs)?;
        let a = &self.components;
        let b = &rhs.components;
        let n = match self.layout {
            JetLayout::FirstOrder(n) | JetLayout::TimeMixed(n) => n,
        };
        let q0 = a[0].div(b[0])?;
        let mut out = vec![q0];
        for j in 1..=n {
            out.push(a[j].sub(q0.mul(b[j])?)?.div(b[0])?);
        }
        if let JetLayout::TimeMixed(_) = self.layout {
            let t = n + 1;
            let qt = a[t].sub(q0.mul(b[t])?)?.div(b[0])?;
            out.push(qt);
            for j in 1..=n {
                out.push(
                    a[t + j]
                        .sub(out[j].mul(b[t])?)?
                        .sub(q0.mul(b[t + j])?)?
                        .sub(qt.mul(b[j])?)?
                        .div(b[0])?,
                );
            }
        }
        Ok(Self {
            layout: self.layout,
            components: out,
        })
    }

    pub(crate) fn sqrt(&self) -> Result<Self> {
        let a = &self.components;
        let n = match self.layout {
            JetLayout::FirstOrder(n) | JetLayout::TimeMixed(n) => n,
        };
        let y0 = a[0].sqrt()?;
        let denominator = Component::input(2.0)?.mul(y0)?;
        let mut out = vec![y0];
        for j in 1..=n {
            out.push(a[j].div(denominator)?);
        }
        if let JetLayout::TimeMixed(_) = self.layout {
            let t = n + 1;
            let yt = a[t].div(denominator)?;
            out.push(yt);
            for j in 1..=n {
                out.push(
                    a[t + j]
                        .sub(Component::input(2.0)?.mul(yt)?.mul(out[j])?)?
                        .div(denominator)?,
                );
            }
        }
        Ok(Self {
            layout: self.layout,
            components: out,
        })
    }

    fn freeze_value(&self) -> Self {
        let mut components = vec![
            Component {
                value: 0.0,
                error: 0.0
            };
            self.components.len()
        ];
        components[0] = self.components[0];
        Self {
            layout: self.layout,
            components,
        }
    }

    pub(crate) fn observed_bits(&self) -> Vec<u64> {
        self.components.iter().map(|v| v.value.to_bits()).collect()
    }
    pub(crate) fn errors(&self) -> Vec<f64> {
        self.components.iter().map(|v| v.error).collect()
    }
    pub(crate) fn certifies_zero(&self) -> bool {
        self.components.iter().all(|v| v.value.abs() <= v.error)
    }
}



mod tracked {
    use super::{ConstructionJet, JetLayout};
    use crate::gas_gap_mass::GasMassScalar;
    use crate::gas_transient_kinetic::TransientGasScalar;
    use implexity_ad::Scalar;
    use implexity_core::{CaeError, CaeResult};
    use implexity_linalg::lu::SparseLu;
    use std::cell::RefCell;
    use std::iter::Sum;
    use std::ops::*;

    struct State {
        epoch: u64,
        layout: Option<JetLayout>,
        nodes: Vec<super::Result<ConstructionJet>>,
    }
    thread_local! {
        static STATE: RefCell<State> = const { RefCell::new(State { epoch: 0, layout: None, nodes: Vec::new() }) };
    }

    pub(crate) struct ConstructionScope {
        epoch: u64,
    }
    impl ConstructionScope {
        pub(crate) fn new(layout: JetLayout) -> CaeResult<Self> {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                let n = match layout {
                    JetLayout::FirstOrder(n) | JetLayout::TimeMixed(n) => n,
                };
                if n > 48 || state.layout.is_some() {
                    return Err(CaeError::contract(
                        "construction error scope nested or exceeds bounded geometry width",
                    ));
                }
                state.epoch = state
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| CaeError::contract("construction error epoch exhausted"))?;
                state.layout = Some(layout);
                state.nodes.clear();
                Ok(Self { epoch: state.epoch })
            })
        }
    }
    impl Drop for ConstructionScope {
        fn drop(&mut self) {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                if state.epoch == self.epoch {
                    state.layout = None;
                    state.nodes.clear();
                }
            });
        }
    }

    pub(super) fn bounded_expression<S: TransientGasScalar>(
        operands: &[TrackedScalar<S>],
        evaluate: impl FnOnce(&[TrackedScalar<S>]) -> CaeResult<ConstructionJet>,
    ) -> CaeResult<ConstructionJet> {
        let sources=operands.iter().map(|v|Ok((v.actual(),v.construction_proof()?)))
            .collect::<CaeResult<Vec<_>>>()?;
        struct Restore(Option<State>);
        impl Drop for Restore {
            fn drop(&mut self) {if let Some(saved)=self.0.take(){STATE.with(|state|*state.borrow_mut()=saved);}}
        }
        let saved=STATE.with(|state| {
            let mut state=state.borrow_mut();
            let layout=state.layout.ok_or_else(||CaeError::contract("bounded construction outside scope"))?;
            let epoch=state.epoch.checked_add(1).ok_or_else(||CaeError::contract("bounded construction epoch overflow"))?;
            Ok::<_,CaeError>(std::mem::replace(&mut *state,State{epoch,layout:Some(layout),nodes:Vec::new()}))
        })?;
        let _restore=Restore(Some(saved));
        let inputs=sources.iter().map(|(v,p)|TrackedScalar::from_bound(*v,p)).collect::<CaeResult<Vec<_>>>()?;
        evaluate(&inputs)
    }

    #[derive(Clone, Copy, Debug)]
    pub(crate) struct TrackedScalar<S: TransientGasScalar> {
        actual: S,
        node: usize,
        epoch: u64,
    }

    impl<S: TransientGasScalar> TrackedScalar<S> {
        fn observe(actual: S, layout: JetLayout) -> super::Result<Vec<f64>> {
            let mut directions = actual
                .geometry_first_order_directions()
                .ok_or("construction geometry directions opaque")?;
            let width = match layout {
                JetLayout::FirstOrder(n) => n,
                JetLayout::TimeMixed(n) => 1 + 2 * n,
            };
            if directions.is_empty()
                || (directions.len() < width
                    && !actual.has_direction()
                    && directions.iter().all(|&v| v == 0.0))
            {
                directions.resize(width, 0.0);
            }
            if directions.len() != width {
                return Err("construction geometry directions do not match typed layout");
            }
            let mut out = vec![actual.value()];
            out.extend(directions);
            Ok(out)
        }
        fn record(
            actual: S,
            make: impl FnOnce(JetLayout) -> super::Result<ConstructionJet>,
        ) -> Self {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                let layout = state
                    .layout
                    .expect("tracked construction Scalar used outside scope");
                let node = state.nodes.len();
                if node >= 16_384 {
                    return Self {
                        actual,
                        node: usize::MAX,
                        epoch: state.epoch,
                    };
                }
                let proof =
                    { make(layout).and_then(|v| v.reconcile(&Self::observe(actual, layout)?)) };
                state.nodes.push(proof);
                Self {
                    actual,
                    node,
                    epoch: state.epoch,
                }
            })
        }
        fn proof(self) -> super::Result<ConstructionJet> {
            STATE.with(|state| {
                let state = state.borrow();
                if state.layout.is_none() || state.epoch != self.epoch {
                    return Err("construction certificate belongs to another scope");
                }
                if self.node == usize::MAX - 1 {
                    let layout = state.layout.ok_or("construction input outside scope")?;
                    return ConstructionJet::input(layout, &Self::observe(self.actual, layout)?);
                }
                state
                    .nodes
                    .get(self.node)
                    .ok_or("construction certificate node missing")?
                    .clone()
            })
        }
        pub(crate) fn from_control(actual: S) -> Self {
            STATE.with(|state| {
                let state = state.borrow();
                assert!(
                    state.layout.is_some(),
                    "tracked construction input outside scope"
                );
                Self {
                    actual,
                    node: usize::MAX - 1,
                    epoch: state.epoch,
                }
            })
        }
        pub(crate) fn freeze_value(self) -> Self {
            let proof = self.proof();
            Self::record(S::from_f64(self.actual.value()), |_| {
                Ok(proof?.freeze_value())
            })
        }
        pub(crate) fn from_bound(actual: S, proof: &ConstructionJet) -> CaeResult<Self> {
            let layout = STATE
                .with(|state| state.borrow().layout)
                .ok_or_else(|| CaeError::contract("construction bound imported outside scope"))?;
            let observed = Self::observe(actual, layout).map_err(CaeError::contract)?;
            if proof.layout != layout
                || observed.iter().map(|v| v.to_bits()).collect::<Vec<_>>() != proof.observed_bits()
            {
                return Err(CaeError::contract(
                    "construction bound output binding differs",
                ));
            }
            let proof = proof.clone();
            Ok(Self::record(actual, |_| Ok(proof)))
        }
        pub(crate) fn from_native_source_zero_fan(actual:S,owner:&crate::native_gas_geometry_adapter::NativeSourceZeroFan<'_,S>)->CaeResult<Self>{
            owner.validate_observed(actual)?;
            let layout=STATE.with(|state|state.borrow().layout).ok_or_else(||CaeError::contract("native zero fan imported outside construction scope"))?;
            let observed=Self::observe(actual,layout).map_err(CaeError::contract)?;
            let proof=ConstructionJet::input(layout,&vec![0.;observed.len()]).and_then(|p|p.reconcile(&observed)).map_err(CaeError::contract)?;
            Self::from_bound(actual,&proof)
        }
        pub(crate) fn recompose_face(self, centroid: S) -> Self {
            let offset = self.actual - centroid;
            let actual = centroid + offset;
            let source = self.proof();
            Self::record(actual, |layout| {
                let auxiliary = ConstructionJet::input(layout, &Self::observe(centroid, layout)?)?;
                let offset_proof = source?.sub(&auxiliary)?
                    .reconcile(&Self::observe(offset, layout)?)?;
                auxiliary.add(&offset_proof)
            })
        }
        pub(crate) fn rebind_owned_normal_chart(owner:&crate::native_normalization::OwnedNormalChart<Self>,component:usize)->CaeResult<Self>{
            let(chart,authored)=owner.component(component)?;
            let proof=chart.proof().map_err(CaeError::contract)?;
            authored.proof().map_err(CaeError::contract)?;
            let actual=chart.actual.chain3(authored.actual.value(),1.,0.,||0.);
            let result=Self::record(actual,|_|Ok(proof));
            result.construction_proof()?;
            Ok(result)
        }
        pub(crate) fn actual(self) -> S {
            self.actual
        }
        pub(crate) fn construction_proof(self) -> CaeResult<ConstructionJet> {
            self.proof().map_err(CaeError::contract)
        }
        fn binary(
            self,
            b: Self,
            actual: S,
            op: impl FnOnce(&ConstructionJet, &ConstructionJet) -> super::Result<ConstructionJet>,
        ) -> Self {
            let a_proof = self.proof();
            let b_proof = b.proof();
            Self::record(actual, |_| op(&a_proof?, &b_proof?))
        }
        fn unsupported(actual: S) -> Self {
            Self::record(actual, |_| {
                Err("construction nonlinear operation has no certified recurrence")
            })
        }
    }

    impl<S: TransientGasScalar> Default for TrackedScalar<S> {
        fn default() -> Self {
            Self::from_f64(0.0)
        }
    }
    impl<S: TransientGasScalar> Add for TrackedScalar<S> {
        type Output = Self;
        fn add(self, b: Self) -> Self {
            self.binary(b, self.actual + b.actual, ConstructionJet::add)
        }
    }
    impl<S: TransientGasScalar> Sub for TrackedScalar<S> {
        type Output = Self;
        fn sub(self, b: Self) -> Self {
            self.binary(b, self.actual - b.actual, ConstructionJet::sub)
        }
    }
    impl<S: TransientGasScalar> Mul for TrackedScalar<S> {
        type Output = Self;
        fn mul(self, b: Self) -> Self {
            self.binary(b, self.actual * b.actual, ConstructionJet::mul)
        }
    }
    impl<S: TransientGasScalar> Div for TrackedScalar<S> {
        type Output = Self;
        fn div(self, b: Self) -> Self {
            self.binary(b, self.actual / b.actual, ConstructionJet::div)
        }
    }
    impl<S: TransientGasScalar> Neg for TrackedScalar<S> {
        type Output = Self;
        fn neg(self) -> Self {
            let proof = self.proof();
            Self::record(-self.actual, |_| Ok(proof?.neg()))
        }
    }
    macro_rules! scalar_ops {
        ($trait:ident,$method:ident,$assign:ident,$assign_method:ident) => {
            impl<S: TransientGasScalar> $trait<f64> for TrackedScalar<S> {
                type Output = Self;
                fn $method(self, b: f64) -> Self {
                    self.$method(Self::from_f64(b))
                }
            }
            impl<S: TransientGasScalar> $assign for TrackedScalar<S> {
                fn $assign_method(&mut self, b: Self) {
                    *self = (*self).$method(b);
                }
            }
        };
    }
    scalar_ops!(Add, add, AddAssign, add_assign);
    scalar_ops!(Sub, sub, SubAssign, sub_assign);
    scalar_ops!(Mul, mul, MulAssign, mul_assign);
    scalar_ops!(Div, div, DivAssign, div_assign);
    impl<S: TransientGasScalar> Sum for TrackedScalar<S> {
        fn sum<I: Iterator<Item = Self>>(it: I) -> Self {
            it.fold(Self::zero(), |a, b| a + b)
        }
    }
    impl<S: TransientGasScalar> Scalar for TrackedScalar<S> {
        fn from_f64(value: f64) -> Self {
            Self::from_control(S::from_f64(value))
        }
        fn value(&self) -> f64 {
            self.actual.value()
        }
        fn chain(self, f: f64, df: f64, d2f: f64) -> Self {
            Self::unsupported(self.actual.chain(f, df, d2f))
        }
        fn chain3(self, f: f64, df: f64, d2f: f64, d3f: impl FnOnce() -> f64) -> Self {
            Self::unsupported(self.actual.chain3(f, df, d2f, d3f))
        }
        fn chain2(
            a: Self,
            b: Self,
            f: f64,
            fa: f64,
            fb: f64,
            faa: f64,
            fab: f64,
            fbb: f64,
        ) -> Self {
            Self::unsupported(S::chain2(a.actual, b.actual, f, fa, fb, faa, fab, fbb))
        }
        fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self {
            let raw: Vec<_> = inputs.iter().map(|v| v.actual).collect();
            Self::unsupported(S::lift(f, &raw, grad, hess))
        }
        fn sqrt(self) -> Self {
            let proof = self.proof();
            Self::record(self.actual.sqrt(), |_| proof?.sqrt())
        }
        fn abs(self) -> Self {
            let proof = self.proof();
            Self::record(self.actual.abs(), |_| {
                let proof = proof?;
                if proof.strictly_positive() {
                    Ok(proof)
                } else if proof.strictly_negative() {
                    Ok(proof.neg())
                } else {
                    Err("construction absolute value sign is not separated from zero")
                }
            })
        }
    }
    impl<S: TransientGasScalar> GasMassScalar for TrackedScalar<S> {
        fn implicit_solve(_factor: &SparseLu, _rhs: &[Self]) -> CaeResult<Vec<Self>> {
            Err(CaeError::contract(
                "construction certificate cannot own implicit physical solve",
            ))
        }
        fn has_direction(self) -> bool {
            self.actual.has_direction()
        }
        fn finite_first_order(self) -> bool {
            self.actual.finite_first_order()
        }
    }
    impl<S: TransientGasScalar> TransientGasScalar for TrackedScalar<S> {
        fn maximum_direction(self) -> f64 {
            self.actual.maximum_direction()
        }
        fn first_order_directions(self) -> Option<Vec<f64>> {
            self.actual.first_order_directions()
        }
        fn geometry_first_order_directions(self) -> Option<Vec<f64>> {
            self.actual.geometry_first_order_directions()
        }
        fn geometry_product_direction_bounds(self, b: Self) -> Option<Vec<f64>> {
            self.actual.geometry_product_direction_bounds(b.actual)
        }
        fn geometry_product_factors_direction_bounds(factors:&[Self])->Option<Vec<f64>>{
            let actual=factors.iter().map(|v|v.actual).collect::<Vec<_>>();
            S::geometry_product_factors_direction_bounds(&actual)
        }
    }
}

pub(crate) use tracked::{ConstructionScope, TrackedScalar};

#[derive(Clone, Debug)]
pub(crate) struct ConvexSupportConstructionProof {
    layout: JetLayout,
    volume: ConstructionJet,
    centroid: [ConstructionJet; 3],
    vertices: Vec<[ConstructionJet; 3]>,
    planes: Vec<([ConstructionJet; 3], [ConstructionJet; 3], ConstructionJet)>,
    face_position: [ConstructionJet; 3],
    face_normal: [ConstructionJet; 3],
    vertex_residuals: Vec<Vec<ConstructionJet>>,
    owned_native_wall_plane:Option<usize>,
    centroid_residuals: Vec<ConstructionJet>,
    face_residuals: Vec<ConstructionJet>,
    normal_cross: Vec<[ConstructionJet; 3]>,
    wall_distance: ConstructionJet,
}

impl ConstructionJet {
    fn value_part(&self) -> Self {
        match self.layout {
            JetLayout::FirstOrder(_) => self.clone(),
            JetLayout::TimeMixed(n) => Self {
                layout: JetLayout::FirstOrder(n),
                components: self.components[..=n].to_vec(),
            },
        }
    }
    fn strictly_positive(&self) -> bool {
        self.components[0].value > self.components[0].error
    }
    fn strictly_negative(&self) -> bool {
        self.components[0].value < -self.components[0].error
    }
    fn member(&self) -> bool {
        self.strictly_negative() || self.certifies_zero()
    }
    fn matches<S: crate::gas_transient_kinetic::TransientGasScalar>(&self, actual: S) -> bool {
        let Some(mut d) = actual.geometry_first_order_directions() else {
            return false;
        };
        let width = self.components.len() - 1;
        if d.is_empty()
            || (d.len() < width && !actual.has_direction() && d.iter().all(|&v| v == 0.))
        {
            d.resize(width, 0.);
        }
        d.len() == width
            && actual.value().to_bits() == self.components[0].value.to_bits()
            && d.iter()
                .zip(&self.components[1..])
                .all(|(&v, c)| v.to_bits() == c.value.to_bits())
    }
}

impl ConvexSupportConstructionProof {
    pub(crate) fn from_tracked<S: crate::gas_transient_kinetic::TransientGasScalar>(
        volume: TrackedScalar<S>,
        centroid: [TrackedScalar<S>; 3],
        vertices: &[[TrackedScalar<S>; 3]],
        planes: &[(
            [TrackedScalar<S>; 3],
            [TrackedScalar<S>; 3],
            TrackedScalar<S>,
        )],
        face_position: [TrackedScalar<S>; 3],
        face_normal: [TrackedScalar<S>; 3],
    ) -> implexity_core::CaeResult<Self> {
        use implexity_ad::Scalar;
        use implexity_core::CaeResult;
        let point = |p: [TrackedScalar<S>; 3]| -> CaeResult<[ConstructionJet; 3]> {
            let [x, y, z] = p.map(TrackedScalar::construction_proof);
            Ok([x?, y?, z?])
        };
        let residual = |p: [TrackedScalar<S>;3],n:[TrackedScalar<S>;3],o:[TrackedScalar<S>;3],b:TrackedScalar<S>| {
            let operands:Vec<_>=p.into_iter().chain(n).chain(o).chain(std::iter::once(b)).collect();
            tracked::bounded_expression(&operands,|v| {
                (0..3).fold(v[9],|s,k|s+v[3+k]*(v[k]-v[6+k])).construction_proof()
            })
        };
        let mut vertex_residuals=Vec::new();
        for &v in vertices {
            vertex_residuals.push(planes.iter().map(|&(n,o,b)|residual(v,n,o,b)).collect::<CaeResult<_>>()?);
        }
        let centroid_residuals=planes.iter().map(|&(n,o,b)|residual(centroid,n,o,b)).collect::<CaeResult<_>>()?;
        let face_residuals=planes.iter().map(|&(n,o,b)|residual(face_position,n,o,b)).collect::<CaeResult<_>>()?;
        let normal_cross=planes.iter().map(|&(n,_,_)| {
            let operands:Vec<_>=face_normal.into_iter().chain(n).collect();
            let component=|a:usize,b:usize,c:usize,d:usize|tracked::bounded_expression(&operands,|v|(v[a]*v[b]-v[c]*v[d]).construction_proof());
            Ok([component(1,5,2,4)?,component(2,3,0,5)?,component(0,4,1,3)?])
        }).collect::<CaeResult<_>>()?;
        let operands:Vec<_>=face_normal.into_iter().chain(face_position).chain(centroid).collect();
        let wall_distance=tracked::bounded_expression(&operands,|v| {
            (0..3).fold(TrackedScalar::zero(),|s,k|s+v[k]*(v[3+k]-v[6+k])).construction_proof()
        })?;
        let volume = volume.construction_proof()?;
        Ok(Self {
            layout: volume.layout,
            volume,
            centroid: point(centroid)?,
            vertices: vertices
                .iter()
                .copied()
                .map(point)
                .collect::<CaeResult<_>>()?,
            planes: planes
                .iter()
                .map(|&(n, o, b)| Ok((point(n)?, point(o)?, b.construction_proof()?)))
                .collect::<CaeResult<_>>()?,
            face_position: point(face_position)?,
            face_normal: point(face_normal)?,
            vertex_residuals,
            owned_native_wall_plane:None,
            centroid_residuals,
            face_residuals,
            normal_cross,
            wall_distance,
        })
    }

    pub(crate) fn validate_bindings<S: crate::gas_transient_kinetic::TransientGasScalar>(
        &self,
        volume: S,
        centroid: [S; 3],
        vertices: &[[S; 3]],
        planes: &[([S; 3], [S; 3], S)],
        face_position: [S; 3],
        face_normal: [S; 3],
    ) -> implexity_core::CaeResult<()> {
        let point = |a: &[ConstructionJet; 3], b: [S; 3]| (0..3).all(|k| a[k].matches(b[k]));
        if !self.volume.matches(volume)
            || !point(&self.centroid, centroid)
            || !point(&self.face_position, face_position)
            || !point(&self.face_normal, face_normal)
            || vertices.len() != self.vertices.len()
            || planes.len() != self.planes.len()
            || !self
                .vertices
                .iter()
                .zip(vertices)
                .all(|(a, &b)| point(a, b))
            || !self
                .planes
                .iter()
                .zip(planes)
                .all(|((an, ao, ab), &(bn, bo, bb))| {
                    point(an, bn) && point(ao, bo) && ab.matches(bb)
                })
        {
            return Err(implexity_core::CaeError::contract(
                "convex support construction proof exact binding differs",
            ));
        }
        Ok(())
    }
    pub(crate) fn certifies_positive_volume(&self) -> bool {
        self.volume.strictly_positive()
    }
    pub(crate) fn certifies_vertex_membership(&self, vertex: usize, plane: usize) -> bool {
        self.vertex_residuals
            .get(vertex)
            .and_then(|r| r.get(plane))
            .is_some_and(ConstructionJet::member)
    }
    pub(crate) fn with_owned_native_wall_facet<S:crate::gas_geometry_time::GeometryTimeBase>(mut self,owner:&crate::native_gas_geometry_adapter::NativeOwnedWallFacet<'_,crate::gas_geometry_time::GeometryTime<S>>)->implexity_core::CaeResult<Self>{
        owner.validate()?;let index=owner.plane();let (n,o,b)=owner.plane_values();let Some((pn,po,pb))=self.planes.get(index)else{return Err(implexity_core::CaeError::contract("owned native wall plane is absent"));};
        if !(0..3).all(|k|pn[k].matches(n[k])&&po[k].matches(o[k])&&self.face_normal[k].matches(owner.normal()[k]))||!pb.matches(b){return Err(implexity_core::CaeError::contract("owned native wall source bindings differ"));}
        if !self.certifies_face_plane(index)||!self.certifies_parallel_normal(index){return Err(implexity_core::CaeError::contract("owned native wall incidence or normal is unproved"));}
        self.owned_native_wall_plane=Some(index);Ok(self)
    }
    pub(crate) fn owns_native_wall_plane(&self,index:usize)->bool{self.owned_native_wall_plane==Some(index)}
    pub(crate) fn centroid_residual_evidence(&self)->Vec<(f64,f64)>{self.centroid_residuals.iter().map(|r|(r.components[0].value,r.components[0].error)).collect()}
    pub(crate) fn certifies_centroid_membership(&self, plane: usize) -> bool {
        self.centroid_residuals
            .get(plane)
            .is_some_and(ConstructionJet::strictly_negative)
    }
    pub(crate) fn certifies_face_membership(&self, plane: usize) -> bool {
        self.face_residuals
            .get(plane)
            .is_some_and(ConstructionJet::member)
    }
    pub(crate) fn certifies_face_plane(&self, plane: usize) -> bool {
        self.face_residuals
            .get(plane)
            .is_some_and(ConstructionJet::certifies_zero)
    }
    pub(crate) fn certifies_parallel_normal(&self, plane: usize) -> bool {
        self.normal_cross
            .get(plane)
            .is_some_and(|p| p.iter().all(ConstructionJet::certifies_zero))
    }
    pub(crate) fn certifies_positive_wall_distance(&self) -> bool {
        self.wall_distance.strictly_positive()
    }
    pub(crate) fn value_part(&self) -> Self {
        let p = |p: &[ConstructionJet; 3]| p.clone().map(|v| v.value_part());
        let layout = match self.layout {
            JetLayout::FirstOrder(n) | JetLayout::TimeMixed(n) => JetLayout::FirstOrder(n),
        };
        Self {
            layout,
            volume: self.volume.value_part(),
            centroid: p(&self.centroid),
            vertices: self.vertices.iter().map(p).collect(),
            planes: self
                .planes
                .iter()
                .map(|(n, o, b)| (p(n), p(o), b.value_part()))
                .collect(),
            face_position: p(&self.face_position),
            face_normal: p(&self.face_normal),
            vertex_residuals: self
                .vertex_residuals
                .iter()
                .map(|v| v.iter().map(ConstructionJet::value_part).collect())
                .collect(),
            owned_native_wall_plane:self.owned_native_wall_plane,
            centroid_residuals: self
                .centroid_residuals
                .iter()
                .map(ConstructionJet::value_part)
                .collect(),
            face_residuals: self
                .face_residuals
                .iter()
                .map(ConstructionJet::value_part)
                .collect(),
            normal_cross: self.normal_cross.iter().map(p).collect(),
            wall_distance: self.wall_distance.value_part(),
        }
    }
}
