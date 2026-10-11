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

fn observation_same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a == 0.0 && b == 0.0)
}

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

    pub(crate) fn rebind_enclosed_scalar<S:crate::gas_transient_kinetic::TransientGasScalar>(self, observed:S)->implexity_core::CaeResult<Self> {
        use implexity_core::CaeError;
        if !self.encloses_scalar(observed){return Err(CaeError::contract("construction view is outside original enclosure"));}
        let values=clip_observation(observed,self.layout).map_err(CaeError::contract)?;
        self.reconcile(&values).map_err(CaeError::contract)
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
                || observed.len() != proof.components.len()
                || !observed.iter().zip(&proof.components).all(|(&v, c)| super::observation_same(v, c.value))
            {
                return Err(CaeError::contract(
                    "construction bound output binding differs",
                ));
            }
            if observed.iter().zip(&proof.components).all(|(&v, c)| v.to_bits() == c.value.to_bits()) {
                let proof = proof.clone();
                return Ok(Self::record(actual, |_| Ok(proof)));
            }
            let mut proof = proof.clone();
            for (component, &value) in proof.components.iter_mut().zip(&observed) {
                component.value = value;
            }
            Ok(STATE.with(|state| {
                let mut state = state.borrow_mut();
                let node = state.nodes.len();
                if node >= 16_384 {
                    return Self { actual, node: usize::MAX, epoch: state.epoch };
                }
                state.nodes.push(Ok(proof));
                Self { actual, node, epoch: state.epoch }
            }))
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
        fn geometry_has_time_channel() -> bool { S::geometry_has_time_channel() }
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
    pub(crate) fn matches<S: crate::gas_transient_kinetic::TransientGasScalar>(&self, actual: S) -> bool {
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
            && observation_same(actual.value(), self.components[0].value)
            && d.iter()
                .zip(&self.components[1..])
                .all(|(&v, c)| observation_same(v, c.value))
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

fn clip_observation<S: crate::gas_transient_kinetic::TransientGasScalar>(value:S,layout:JetLayout)->Result<Vec<f64>> {
    let mut directions=value.geometry_first_order_directions().ok_or("clip construction channels opaque")?;
    let width=match layout {JetLayout::FirstOrder(n)=>n,JetLayout::TimeMixed(n)=>1+2*n};
    if directions.len()<width && !value.has_direction() && directions.iter().all(|v|*v==0.) {directions.resize(width,0.);}
    if directions.len()!=width || !value.finite_first_order() {return Err("clip construction channel layout mismatch");}
    let mut out=vec![value.value()];out.extend(directions);Ok(out)
}
impl ConstructionJet {
    pub(crate) fn encloses_scalar<S:crate::gas_transient_kinetic::TransientGasScalar>(&self,value:S)->bool {
        if matches!(self.layout,JetLayout::TimeMixed(_))!=S::geometry_has_time_channel(){return false;}
        clip_observation(value,self.layout).and_then(|v|Self::input(self.layout,&v)).and_then(|v|v.sub(self)).is_ok_and(|v|v.certifies_zero())
    }
}
pub(crate) fn clip_interpolation_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    triangle:[[S;3];3],triangle_anchor:[f64;3],normal:[S;3],origin:[S;3],
    a:[S;3],a_error:Option<&[ConstructionJet;3]>,b:[S;3],b_error:Option<&[ConstructionJet;3]>,
    anchor:[f64;3],da:S,db:S,point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    clip_triangle_interpolation_error(None,triangle,triangle_anchor,normal,origin,a,a_error,b,b_error,anchor,da,db,point)
}

pub(crate) fn clip_chart_interpolation_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    sources:&[crate::native_geometry_source::NativeChartPointSource<S>;3],triangle:[[S;3];3],triangle_anchor:[f64;3],normal:[S;3],origin:[S;3],
    a:[S;3],a_error:Option<&[ConstructionJet;3]>,b:[S;3],b_error:Option<&[ConstructionJet;3]>,anchor:[f64;3],da:S,db:S,point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    let errors=[chart_point_expression_error(&sources[0],triangle_anchor,triangle[0])?,chart_point_expression_error(&sources[1],triangle_anchor,triangle[1])?,chart_point_expression_error(&sources[2],triangle_anchor,triangle[2])?];
    clip_triangle_interpolation_error(Some(&errors),triangle,triangle_anchor,normal,origin,a,a_error,b,b_error,anchor,da,db,point)
}

fn clip_triangle_interpolation_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    triangle_error:Option<&[[ConstructionJet;3];3]>,triangle:[[S;3];3],triangle_anchor:[f64;3],normal:[S;3],origin:[S;3],
    a:[S;3],a_error:Option<&[ConstructionJet;3]>,b:[S;3],b_error:Option<&[ConstructionJet;3]>,
    anchor:[f64;3],da:S,db:S,point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    use implexity_core::CaeError;
    let execute=||->Result<[ConstructionJet;3]>{
        if triangle_anchor.iter().chain(anchor.iter()).any(|v|!v.is_finite()){return Err("clip construction frame invalid");}
        let values=triangle.iter().flatten().copied().chain(normal).chain(origin).chain(a).chain(b).chain([da,db]).chain(point).collect::<Vec<_>>();
        let width=values.iter().map(|v|v.geometry_first_order_directions().map(|d|d.len()).ok_or("clip construction channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("clip construction mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|v:S|ConstructionJet::input(layout,&clip_observation(v,layout)?);
        let vector=|v:[S;3]|->Result<[ConstructionJet;3]>{Ok([input(v[0])?,input(v[1])?,input(v[2])?])};
        let source=if let Some(errors)=triangle_error {
            if (0..3).any(|i|(0..3).any(|k|errors[i][k].layout!=layout || !errors[i][k].matches(triangle[i][k]))){return Err("clip triangle input bounds differ from original producer");}
            errors.clone()
        }else{[vector(triangle[0])?,vector(triangle[1])?,vector(triangle[2])?]};
        let u=[source[1][0].sub(&source[0][0])?,source[1][1].sub(&source[0][1])?,source[1][2].sub(&source[0][2])?];
        let v=[source[2][0].sub(&source[0][0])?,source[2][1].sub(&source[0][1])?,source[2][2].sub(&source[0][2])?];
        let cross=[u[1].mul(&v[2])?.sub(&u[2].mul(&v[1])?)?,u[2].mul(&v[0])?.sub(&u[0].mul(&v[2])?)?,u[0].mul(&v[1])?.sub(&u[1].mul(&v[0])?)?];
        let length=cross[0].mul(&cross[0])?.add(&cross[1].mul(&cross[1])?)?.add(&cross[2].mul(&cross[2])?)?.sqrt()?;
        let raw_cross=[(triangle[1][1]-triangle[0][1])*(triangle[2][2]-triangle[0][2])-(triangle[1][2]-triangle[0][2])*(triangle[2][1]-triangle[0][1]),(triangle[1][2]-triangle[0][2])*(triangle[2][0]-triangle[0][0])-(triangle[1][0]-triangle[0][0])*(triangle[2][2]-triangle[0][2]),(triangle[1][0]-triangle[0][0])*(triangle[2][1]-triangle[0][1])-(triangle[1][1]-triangle[0][1])*(triangle[2][0]-triangle[0][0])];
        let orientation=raw_cross.iter().zip(normal).map(|(a,b)|a.value()*b.value()).sum::<f64>();
        if !orientation.is_finite() || orientation==0.{return Err("clip construction normal orientation invalid");}
        let raw_length=raw_cross.iter().map(|v|*v * *v).fold(S::zero(),|a,b|a+b).sqrt();
        let raw_normal=raw_cross.map(|v|if orientation<0. {-v/raw_length}else{v/raw_length});
        for k in 0..3 {let derived=clip_observation(raw_normal[k],layout)?;let supplied=clip_observation(normal[k],layout)?;if derived.iter().zip(&supplied).any(|(a,b)|a!=b){return Err("clip normal differs from original triangle producer");}}
        let mut n=[cross[0].div(&length)?,cross[1].div(&length)?,cross[2].div(&length)?];
        for k in 0..3 {if orientation<0. {n[k]=n[k].neg();}n[k]=n[k].clone().reconcile(&clip_observation(normal[k],layout)?)?;}
        let mut o=source[0].clone();
        for k in 0..3 {if triangle_anchor[k]!=anchor[k] {let shift=input(S::from_f64(triangle_anchor[k]))?.sub(&input(S::from_f64(anchor[k]))?)?;o[k]=o[k].add(&shift)?;}}
        clip_from_plane_error(layout,n,o,input(S::zero())?,normal,origin,S::zero(),a,a_error,b,b_error,da,db,point)
    };
    execute().map_err(CaeError::contract)
}

fn clip_from_plane_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    layout:JetLayout,n:[ConstructionJet;3],o:[ConstructionJet;3],offset_error:ConstructionJet,
    normal:[S;3],origin:[S;3],offset:S,a:[S;3],a_error:Option<&[ConstructionJet;3]>,b:[S;3],b_error:Option<&[ConstructionJet;3]>,da:S,db:S,point:[S;3],
)->Result<[ConstructionJet;3]> {
    let input=|v:S|ConstructionJet::input(layout,&clip_observation(v,layout)?);
    let vector=|v:[S;3]|->Result<[ConstructionJet;3]>{Ok([input(v[0])?,input(v[1])?,input(v[2])?])};
        let endpoint=|actual:[S;3],bound:Option<&[ConstructionJet;3]>|->Result<[ConstructionJet;3]>{
            if let Some(bound)=bound {if bound.iter().zip(actual).any(|(p,a)|p.layout!=layout || !p.matches(a)){return Err("clip endpoint proof differs");}Ok(bound.clone())}else{vector(actual)}
        };
        let ap=endpoint(a,a_error)?;let bp=endpoint(b,b_error)?;
        let distance=|p:&[ConstructionJet;3],actual:S|->Result<ConstructionJet>{let mut d=input(S::zero())?;for k in 0..3 {d=d.add(&n[k].mul(&p[k].sub(&o[k])?)?)?;}d.add(&offset_error)?.reconcile(&clip_observation(actual,layout)?)};
        let ad=distance(&ap,da)?;let bd=distance(&bp,db)?;
        let raw_distance=|p:[S;3]|(0..3).map(|k|normal[k]*(p[k]-origin[k])).fold(S::zero(),|a,b|a+b)+offset;
        if !input(raw_distance(a))?.matches(da) || !input(raw_distance(b))?.matches(db){return Err("clip distance observation differs from producer");}
        let actual_t=da/(da-db);let t=ad.div(&ad.sub(&bd)?)?.reconcile(&clip_observation(actual_t,layout)?)?;
        let mut output=ap.clone();
        for k in 0..3 {let actual=a[k]+(b[k]-a[k])*actual_t;if !input(actual)?.matches(point[k]){return Err("clip point observation differs from producer");}output[k]=ap[k].add(&bp[k].sub(&ap[k])?.mul(&t)?)?.reconcile(&clip_observation(point[k],layout)?)?;}
        Ok(output)
}

pub(crate) fn clip_levelset_interpolation_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    vertices:[[S;3];4],values:[S;4],iso:f64,source_anchor:[f64;3],normal:[S;3],origin:[S;3],offset:S,
    a:[S;3],a_error:Option<&[ConstructionJet;3]>,b:[S;3],b_error:Option<&[ConstructionJet;3]>,anchor:[f64;3],da:S,db:S,point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    use implexity_core::CaeError;
    let execute=||->Result<[ConstructionJet;3]>{
        if !iso.is_finite() || source_anchor.iter().chain(anchor.iter()).any(|v|!v.is_finite()){return Err("levelset clip source invalid");}
        let operands=vertices.iter().flatten().copied().chain(values).chain(normal).chain(origin).chain([offset,da,db]).chain(a).chain(b).chain(point).collect::<Vec<_>>();
        let width=operands.iter().map(|v|v.geometry_first_order_directions().map(|d|d.len()).ok_or("levelset clip channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("levelset clip mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|v:S|ConstructionJet::input(layout,&clip_observation(v,layout)?);
        let vector=|v:[S;3]|->Result<[ConstructionJet;3]>{Ok([input(v[0])?,input(v[1])?,input(v[2])?])};
        let p=[vector(vertices[0])?,vector(vertices[1])?,vector(vertices[2])?,vector(vertices[3])?];
        let mut edge=[p[1].clone(),p[2].clone(),p[3].clone()];
        for i in 0..3 {for k in 0..3 {edge[i][k]=edge[i][k].sub(&p[0][k])?;}}
        let cross=|a:&[ConstructionJet;3],b:&[ConstructionJet;3]|->Result<[ConstructionJet;3]>{Ok([a[1].mul(&b[2])?.sub(&a[2].mul(&b[1])?)?,a[2].mul(&b[0])?.sub(&a[0].mul(&b[2])?)?,a[0].mul(&b[1])?.sub(&a[1].mul(&b[0])?)?])};
        let cof=[cross(&edge[1],&edge[2])?,cross(&edge[2],&edge[0])?,cross(&edge[0],&edge[1])?];
        let mut det=input(S::zero())?;for k in 0..3 {det=det.add(&edge[0][k].mul(&cof[0][k])?)?;}
        let delta=[input(values[1])?.sub(&input(values[0])?)?,input(values[2])?.sub(&input(values[0])?)?,input(values[3])?.sub(&input(values[0])?)?];
        let gradient_axis=|k|delta[0].mul(&cof[0][k])?.add(&delta[1].mul(&cof[1][k])?)?.add(&delta[2].mul(&cof[2][k])?)?.div(&det);
        let gradient=[gradient_axis(0)?,gradient_axis(1)?,gradient_axis(2)?];
        let mut square=input(S::zero())?;for g in &gradient {square=square.add(&g.mul(g)?)?;}let length=square.sqrt()?;
        let raw_edge=std::array::from_fn::<_,3,_>(|i|std::array::from_fn::<_,3,_>(|k|vertices[i+1][k]-vertices[0][k]));
        let raw_cross=|a:[S;3],b:[S;3]|[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
        let raw_cof=[raw_cross(raw_edge[1],raw_edge[2]),raw_cross(raw_edge[2],raw_edge[0]),raw_cross(raw_edge[0],raw_edge[1])];
        let raw_det=(0..3).map(|k|raw_edge[0][k]*raw_cof[0][k]).fold(S::zero(),|a,b|a+b);
        let raw_gradient=std::array::from_fn::<_,3,_>(|k|((values[1]-values[0])*raw_cof[0][k]+(values[2]-values[0])*raw_cof[1][k]+(values[3]-values[0])*raw_cof[2][k])/raw_det);
        let raw_length=raw_gradient.iter().map(|v|*v * *v).fold(S::zero(),|a,b|a+b).sqrt();
        let orientation=raw_gradient.iter().zip(normal).map(|(a,b)|a.value()*b.value()).sum::<f64>();
        if !orientation.is_finite() || orientation==0.{return Err("levelset clip orientation invalid");}
        let raw_normal=raw_gradient.map(|v|if orientation<0. {-v/raw_length}else{v/raw_length});
        let raw_offset=(values[0]-iso)/raw_length;let raw_offset=if orientation<0. {-raw_offset}else{raw_offset};
        let equal=|a:S,b:S|->Result<bool>{Ok(clip_observation(a,layout)?.iter().zip(clip_observation(b,layout)?).all(|(a,b)|*a==b))};
        if !equal(raw_offset,offset)? || (0..3).any(|k|!equal(raw_normal[k],normal[k]).unwrap_or(false)){return Err("levelset clip plane differs from original producer");}
        let mut n=[gradient[0].div(&length)?,gradient[1].div(&length)?,gradient[2].div(&length)?];
        let mut d=input(values[0])?.sub(&input(S::from_f64(iso))?)?.div(&length)?;
        if orientation<0. {n=n.map(|v|v.neg());d=d.neg();}
        for k in 0..3 {n[k]=n[k].clone().reconcile(&clip_observation(normal[k],layout)?)?;}d=d.reconcile(&clip_observation(offset,layout)?)?;
        let mut o=p[0].clone();for k in 0..3 {if source_anchor[k]!=anchor[k] {let shift=input(S::from_f64(source_anchor[k]))?.sub(&input(S::from_f64(anchor[k]))?)?;o[k]=o[k].add(&shift)?;}}
        clip_from_plane_error(layout,n,o,d,normal,origin,offset,a,a_error,b,b_error,da,db,point)
    };
    execute().map_err(CaeError::contract)
}


pub(crate) enum NativeClipSource<S:implexity_ad::Scalar> {
    Face([usize;3]),
    Levelset {values:[S;4],iso:f64},
}

fn native_reference_error<S:crate::gas_transient_kinetic::TransientGasScalar>(reference:&crate::native_geometry_source::NativeReferencePoint,anchor:[f64;3],layout:JetLayout)->Result<[ConstructionJet;3]> {
    use crate::native_geometry_source::NativeReferencePoint;
    let input=|v:f64|ConstructionJet::input(layout,&clip_observation(S::from_f64(v),layout)?);
    let actual=reference.in_frame(anchor).map_err(|_|"native clip reference is invalid")?;
    let axis=|k:usize|->Result<ConstructionJet>{
        let p=match reference {
            NativeReferencePoint::Grid{origin,index,spacing}=>{
                if (index[k] as u128)>(1u128<<53){return Err("native clip reference index exceeds exact integer scope");}
                input(origin[k])?.add(&input(index[k] as f64)?.mul(&input(*spacing)?)?)?.sub(&input(anchor[k])?)?
            }
            NativeReferencePoint::Stored(point)=>input(point[k])?.sub(&input(anchor[k])?)?,
        };
        p.reconcile(&clip_observation(S::from_f64(actual[k]),layout)?)
    };
    Ok([axis(0)?,axis(1)?,axis(2)?])
}

fn native_reference_difference_error<S:crate::gas_transient_kinetic::TransientGasScalar>(a:&crate::native_geometry_source::NativeReferencePoint,b:&crate::native_geometry_source::NativeReferencePoint,layout:JetLayout)->Result<[ConstructionJet;3]> {
    use crate::native_geometry_source::NativeReferencePoint;
    let input=|v:f64|ConstructionJet::input(layout,&clip_observation(S::from_f64(v),layout)?);
    let actual=a.difference(b).map_err(|_|"native clip reference producers differ")?;
    let axis=|k:usize|->Result<ConstructionJet>{
        let p=match(a,b){
            (NativeReferencePoint::Grid{index:i,spacing,..},NativeReferencePoint::Grid{index:j,..})=>{
                let (delta,negative)=if i[k]>=j[k]{(i[k]-j[k],false)}else{(j[k]-i[k],true)};
                if (delta as u128)>(1u128<<53){return Err("native clip edge index exceeds exact integer scope");}
                let p=input(delta as f64)?.mul(&input(*spacing)?)?;
                if negative{p.neg()}else{p}
            }
            (NativeReferencePoint::Stored(a),NativeReferencePoint::Stored(b))=>input(a[k])?.sub(&input(b[k])?)?,
            _=>return Err("native clip reference producers differ"),
        };
        p.reconcile(&clip_observation(S::from_f64(actual[k]),layout)?)
    };
    Ok([axis(0)?,axis(1)?,axis(2)?])
}

pub(crate) fn clip_native_source_interpolation_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    source:&crate::native_geometry_source::NativeElementSource<S>,kind:NativeClipSource<S>,normal:[S;3],origin:[S;3],offset:S,
    a:[S;3],a_error:Option<&[ConstructionJet;3]>,b:[S;3],b_error:Option<&[ConstructionJet;3]>,anchor:[f64;3],da:S,db:S,point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    use implexity_core::CaeError;
    let execute=||->Result<[ConstructionJet;3]>{
        source.validate().map_err(|_|"native clip coefficient source is invalid")?;
        if anchor.iter().any(|v|!v.is_finite()){return Err("native clip frame is invalid");}
        let mut values=source.points.iter().flat_map(|p|p.displacement).chain(normal).chain(origin).chain([offset,da,db]).chain(a).chain(b).chain(point).collect::<Vec<_>>();
        if let NativeClipSource::Levelset{values:density,..}=&kind{values.extend(density);}
        let width=values.iter().map(|v|v.geometry_first_order_directions().map(|d|d.len()).ok_or("native clip channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("native clip mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|v:S|ConstructionJet::input(layout,&clip_observation(v,layout)?);
        let edge=|i:usize,j:usize|->Result<[ConstructionJet;3]>{
            let reference=native_reference_difference_error::<S>(&source.points[i].reference,&source.points[j].reference,layout)?;
            let raw=source.points[i].edge_from(&source.points[j]).map_err(|_|"native clip source edge is invalid")?;
            let axis=|k|input(source.points[i].displacement[k])?.sub(&input(source.points[j].displacement[k])?)?.add(&reference[k])?.reconcile(&clip_observation(raw[k],layout)?);
            Ok([axis(0)?,axis(1)?,axis(2)?])
        };
        let cross=|a:&[ConstructionJet;3],b:&[ConstructionJet;3]|->Result<[ConstructionJet;3]>{Ok([a[1].mul(&b[2])?.sub(&a[2].mul(&b[1])?)?,a[2].mul(&b[0])?.sub(&a[0].mul(&b[2])?)?,a[0].mul(&b[1])?.sub(&a[1].mul(&b[0])?)?])};
        let dot=|a:&[ConstructionJet;3],b:&[ConstructionJet;3]|->Result<ConstructionJet>{let mut v=input(S::zero())?;for k in 0..3{v=v.add(&a[k].mul(&b[k])?)?;}Ok(v)};
        let (raw_normal,raw_offset,origin_index,mut n,mut d)=match kind {
            NativeClipSource::Face(nodes)=>{
                let indices=source.face_indices(nodes).ok_or("native clip source face is absent")?;
                let raw=source.face_normal(indices).map_err(|_|"native clip source face is invalid")?;
                let u=edge(indices[1],indices[0])?;let v=edge(indices[2],indices[0])?;
                let c=cross(&u,&v)?;let length=dot(&c,&c)?.sqrt()?;
                (raw,S::zero(),indices[0],[c[0].div(&length)?,c[1].div(&length)?,c[2].div(&length)?],input(S::zero())?)
            }
            NativeClipSource::Levelset{values,iso}=>{
                let (raw,raw_offset)=source.levelset(values,iso).map_err(|_|"native clip source levelset is invalid")?;
                let e=[edge(1,0)?,edge(2,0)?,edge(3,0)?];
                let cof=[cross(&e[1],&e[2])?,cross(&e[2],&e[0])?,cross(&e[0],&e[1])?];let determinant=dot(&e[0],&cof[0])?;
                let delta=[input(values[1])?.sub(&input(values[0])?)?,input(values[2])?.sub(&input(values[0])?)?,input(values[3])?.sub(&input(values[0])?)?];
                let axis=|k|delta[0].mul(&cof[0][k])?.add(&delta[1].mul(&cof[1][k])?)?.add(&delta[2].mul(&cof[2][k])?)?.div(&determinant);
                let g=[axis(0)?,axis(1)?,axis(2)?];let length=dot(&g,&g)?.sqrt()?;
                (raw,raw_offset,0,[g[0].div(&length)?,g[1].div(&length)?,g[2].div(&length)?],input(values[0])?.sub(&input(S::from_f64(iso))?)?.div(&length)?)
            }
        };
        let equal=|x:S,y:S|->Result<bool>{Ok(clip_observation(x,layout)?==clip_observation(y,layout)?)};
        let positive=(0..3).all(|k|equal(raw_normal[k],normal[k]).unwrap_or(false)) && equal(raw_offset,offset)?;
        let negative=(0..3).all(|k|equal(-raw_normal[k],normal[k]).unwrap_or(false)) && equal(-raw_offset,offset)?;
        if !positive && !negative{return Err("native clip plane differs from original coefficient producer");}
        if !positive{n=n.map(|v|v.neg());d=d.neg();}
        for k in 0..3{n[k]=n[k].clone().reconcile(&clip_observation(normal[k],layout)?)?;}
        d=d.reconcile(&clip_observation(offset,layout)?)?;
        let node=&source.points[origin_index];let actual=node.point_in_frame(anchor).map_err(|_|"native clip source origin is invalid")?;
        if (0..3).any(|k|!equal(actual[k],origin[k]).unwrap_or(false)){return Err("native clip origin differs from original point producer");}
        let reference=native_reference_error::<S>(&node.reference,anchor,layout)?;
        let axis=|k|input(node.displacement[k])?.add(&reference[k])?.reconcile(&clip_observation(actual[k],layout)?);
        let o=[axis(0)?,axis(1)?,axis(2)?];
        clip_from_plane_error(layout,n,o,d,normal,origin,offset,a,a_error,b,b_error,da,db,point)
    };
    execute().map_err(CaeError::contract)
}

pub(crate) fn chart_point_expression_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    source:&crate::native_geometry_source::NativeChartPointSource<S>,anchor:[f64;3],point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    use crate::moving_contact::surface_map::SurfaceFeature;
    let execute=||->Result<[ConstructionJet;3]>{
        let actual=source.point_in_frame(anchor).map_err(|_|"chart construction source is invalid")?;
        let values=source.endpoints.iter().flat_map(|e|e.points.iter().flat_map(|p|p.displacement).chain(e.values.iter().copied())).chain(point).collect::<Vec<_>>();
        let width=values.iter().map(|v|v.geometry_first_order_directions().map(|d|d.len()).ok_or("chart construction channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("chart construction mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|v:S|ConstructionJet::input(layout,&clip_observation(v,layout)?);
        for k in 0..3 {if clip_observation(actual[k],layout)?!=clip_observation(point[k],layout)?{return Err("chart construction output differs from original producer");}}
        let feature=|e:&crate::native_geometry_source::NativeChartFeatureSource<S>|->Result<[ConstructionJet;3]>{
            let actual_weights=e.weights().map_err(|_|"chart construction feature is invalid")?;
            let weights=match &e.feature {
                SurfaceFeature::Vertex(id)=>vec![(*id,input(S::from_f64(1.))?)],
                SurfaceFeature::FixedTetrahedron{nodes,weights,..}=>nodes.iter().copied().zip(weights.iter().map(|v|input(S::from_f64(*v)))).map(|(id,w)|w.map(|w|(id,w))).collect::<Result<Vec<_>>>()?,
                SurfaceFeature::DensityEdge{nodes,iso,..}=>{
                    let density=|id|e.points.iter().position(|p|p.node==id).map(|i|e.values[i]).ok_or("chart construction density node is absent");
                    let a=input(density(nodes[0])?)?;let b=input(density(nodes[1])?)?;
                    let t=input(S::from_f64(*iso))?.sub(&a)?.div(&b.sub(&a)?)?;
                    vec![(nodes[0],input(S::from_f64(1.))?.sub(&t)?),(nodes[1],t)]
                }
            };
            let mut out=[input(S::zero())?,input(S::zero())?,input(S::zero())?];
            for ((id,w),(actual_id,actual_weight)) in weights.into_iter().zip(actual_weights) {
                if id!=actual_id{return Err("chart construction weight order differs");}
                let w=w.reconcile(&clip_observation(actual_weight,layout)?)?;
                let i=e.points.iter().position(|p|p.node==id).ok_or("chart construction node is absent")?;
                let p=&e.points[i];let reference=native_reference_error::<S>(&p.reference,anchor,layout)?;
                for k in 0..3 {
                    let displacement=input(p.displacement[k])?.div(&input(S::from_f64(e.scales[i][k][0]))?)?.mul(&input(S::from_f64(e.scales[i][k][1]))?)?;
                    out[k]=out[k].add(&reference[k].add(&displacement)?.mul(&w)?)?;
                }
            }
            let raw=e.point_in_frame(anchor).map_err(|_|"chart construction feature point is invalid")?;
            for k in 0..3{out[k]=out[k].clone().reconcile(&clip_observation(raw[k],layout)?)?;}
            Ok(out)
        };
        let a=feature(&source.endpoints[0])?;let b=feature(&source.endpoints[1])?;
        let fraction=input(S::from_f64(source.fraction))?;
        let axis=|k:usize|a[k].add(&b[k].sub(&a[k])?.mul(&fraction)?)?.reconcile(&clip_observation(point[k],layout)?);
        Ok([axis(0)?,axis(1)?,axis(2)?])
    };
    execute().map_err(implexity_core::CaeError::contract)
}

pub(crate) fn chart_tetra_expression_rank<S:crate::gas_transient_kinetic::TransientGasScalar>(
    sources:&[crate::native_geometry_source::NativeChartPointSource<S>;4],anchor:[f64;3],
)->implexity_core::CaeResult<i32> {
    let execute=||->Result<i32>{
        let bound=|i:usize|{let point=sources[i].point_in_frame(anchor).map_err(|_|"chart tetra source is invalid")?;chart_point_expression_error(&sources[i],anchor,point).map_err(|_|"chart tetra source bound is absent")};
        let p=[bound(0)?,bound(1)?,bound(2)?,bound(3)?];
        let edge=|i:usize|->Result<[ConstructionJet;3]>{Ok([p[i][0].sub(&p[0][0])?,p[i][1].sub(&p[0][1])?,p[i][2].sub(&p[0][2])?])};
        let a=edge(1)?;let b=edge(2)?;let c=edge(3)?;
        let cross=[b[1].mul(&c[2])?.sub(&b[2].mul(&c[1])?)?,b[2].mul(&c[0])?.sub(&b[0].mul(&c[2])?)?,b[0].mul(&c[1])?.sub(&b[1].mul(&c[0])?)?];
        let determinant=a[0].mul(&cross[0])?.add(&a[1].mul(&cross[1])?)?.add(&a[2].mul(&cross[2])?)?;
        Ok(if determinant.strictly_positive(){1}else if determinant.strictly_negative(){-1}else{0})
    };
    execute().map_err(implexity_core::CaeError::contract)
}

pub(crate) fn chart_edge_cell_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    sources:&[crate::native_geometry_source::NativeChartPointSource<S>;2],axis:usize,coordinate:S,anchor:[f64;3],point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    let execute=||->Result<[ConstructionJet;3]>{
        if axis>=3{return Err("chart edge cell axis is invalid");}
        let a=sources[0].point_in_frame(anchor).map_err(|_|"chart edge first source is invalid")?;
        let b=sources[1].point_in_frame(anchor).map_err(|_|"chart edge second source is invalid")?;
        let values=a.into_iter().chain(b).chain(point).chain([coordinate]).collect::<Vec<_>>();
        let width=values.iter().map(|v|v.geometry_first_order_directions().map(|d|d.len()).ok_or("chart edge channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("chart edge mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let coordinate_observation=clip_observation(coordinate,layout)?;
        if coordinate_observation.iter().skip(1).any(|v|*v!=0.){return Err("chart edge cell coordinate is not fixed");}
        let t=(coordinate-a[axis])/(b[axis]-a[axis]);
        if !t.finite_first_order() || !(0. ..=1.).contains(&t.value()){return Err("chart edge cell fraction is invalid");}
        let mut raw:[S;3]=std::array::from_fn(|k|a[k]+(b[k]-a[k])*t);raw[axis]=coordinate;
        for k in 0..3{if clip_observation(raw[k],layout)?!=clip_observation(point[k],layout)?{return Err("chart edge output differs from original producer");}}
        let ae=chart_point_expression_error(&sources[0],anchor,a).map_err(|_|"chart edge first source bound is absent")?;
        let be=chart_point_expression_error(&sources[1],anchor,b).map_err(|_|"chart edge second source bound is absent")?;
        if ae.iter().chain(&be).any(|v|v.layout!=layout){return Err("chart edge source bound layout differs");}
        let fixed=ConstructionJet::input(layout,&coordinate_observation)?;
        let fraction=fixed.sub(&ae[axis])?.div(&be[axis].sub(&ae[axis])?)?.reconcile(&clip_observation(t,layout)?)?;
        let axis_bound=|k:usize|ae[k].add(&be[k].sub(&ae[k])?.mul(&fraction)?)?.reconcile(&clip_observation(point[k],layout)?);
        let mut out=[axis_bound(0)?,axis_bound(1)?,axis_bound(2)?];out[axis]=fixed;Ok(out)
    };
    execute().map_err(implexity_core::CaeError::contract)
}

pub(crate) fn clip_cell_interpolation_error<S:crate::gas_transient_kinetic::TransientGasScalar>(
    axis:usize,coordinate:S,normal:[S;3],origin:[S;3],offset:S,
    a:[S;3],a_error:Option<&[ConstructionJet;3]>,b:[S;3],b_error:Option<&[ConstructionJet;3]>,da:S,db:S,point:[S;3],
)->implexity_core::CaeResult<[ConstructionJet;3]> {
    use implexity_core::CaeError;
    let execute=||->Result<[ConstructionJet;3]>{
        if axis>=3{return Err("cell clip axis invalid");}
        let values=normal.into_iter().chain(origin).chain(a).chain(b).chain(point).chain([coordinate,offset,da,db]).collect::<Vec<_>>();
        let width=values.iter().map(|v|v.geometry_first_order_directions().map(|d|d.len()).ok_or("cell clip channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("cell clip mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|v:S|ConstructionJet::input(layout,&clip_observation(v,layout)?);
        let constant=|v:S|->Result<bool>{Ok(clip_observation(v,layout)?.iter().skip(1).all(|x|*x==0.))};
        if offset.value()!=0. || !constant(offset)? || !constant(origin[axis])? || !constant(coordinate)?{return Err("cell clip is not an original fixed coordinate");}
        for k in 0..3 {
            if !constant(normal[k])? || (k==axis && normal[k].value()!=1. && normal[k].value()!=-1.) || (k!=axis && normal[k].value()!=0.){return Err("cell clip normal is not a fixed axis");}
        }
        let supplied=clip_observation(coordinate,layout)?;
        let issued=clip_observation(origin[axis],layout)?;
        if supplied.iter().zip(&issued).any(|(a,b)|a!=b){return Err("cell clip coordinate differs from issued plane");}
        if !input(coordinate)?.matches(point[axis]){return Err("cell clip output differs from fixed coordinate");}
        if da.value()==0. || db.value()==0. || (da.value()>0.)==(db.value()>0.){return Err("cell clip is not a genuine crossing");}
        let t=da/(da-db);
        let raw=std::array::from_fn(|k|a[k]+(b[k]-a[k])*t);
        for k in 0..3 {if k!=axis && !input(raw[k])?.matches(point[k]){return Err("cell clip output differs from interpolation");}}
        let vector=|v:[S;3]|->Result<[ConstructionJet;3]>{Ok([input(v[0])?,input(v[1])?,input(v[2])?])};
        let mut proof=clip_from_plane_error(layout,vector(normal)?,vector(origin)?,input(offset)?,normal,origin,offset,a,a_error,b,b_error,da,db,raw)?;
        proof[axis]=input(coordinate)?;
        Ok(proof)
    };
    execute().map_err(CaeError::contract)
}

pub(crate) fn native_chart_cell_corner<S:crate::gas_transient_kinetic::TransientGasScalar>(
    source:&crate::native_geometry_source::NativeElementSource<S>,nodes:[usize;3],
    charts:&[crate::native_geometry_source::NativeChartPointSource<S>;3],axis:usize,coordinate:S,anchor:[f64;3],
)->implexity_core::CaeResult<([S;3],[ConstructionJet;3])> {
    source.validate()?;
    let indices=source.face_indices(nodes).ok_or_else(||implexity_core::CaeError::contract("native chart corner source face is absent"))?;
    let origin=source.points[indices[0]].point_in_frame(anchor)?;
    let chart=[charts[0].point_in_frame(anchor)?,charts[1].point_in_frame(anchor)?,charts[2].point_in_frame(anchor)?];
    let u=source.points[indices[1]].edge_from(&source.points[indices[0]])?;
    let v=source.points[indices[2]].edge_from(&source.points[indices[0]])?;
    let cross=|a:[S;3],b:[S;3]|[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
    let normal=cross(u,v);
    let chart_normal=cross(std::array::from_fn(|k|chart[1][k]-chart[0][k]),std::array::from_fn(|k|chart[2][k]-chart[0][k]));
    let execute=||->Result<([S;3],[ConstructionJet;3])>{
        if axis>=3 || anchor.iter().any(|v|!v.is_finite()){return Err("native chart corner frame or axis is invalid");}
        let values=origin.into_iter().chain(chart.into_iter().flatten()).chain(u).chain(v).chain([coordinate]).collect::<Vec<_>>();
        let width=values.iter().map(|v|v.geometry_first_order_directions().map(|d|d.len()).ok_or("native chart corner channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("native chart corner mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|x:S|ConstructionJet::input(layout,&clip_observation(x,layout)?);
        let fixed=input(coordinate)?;
        if fixed.components.iter().skip(1).any(|v|v.value!=0.){return Err("native chart corner cell is not fixed");}
        let point_bound=|i:usize|->Result<[ConstructionJet;3]>{let p=&source.points[i];let reference=native_reference_error::<S>(&p.reference,anchor,layout)?;let actual=p.point_in_frame(anchor).map_err(|_|"native chart corner original point is invalid")?;let component=|k|input(p.displacement[k])?.add(&reference[k])?.reconcile(&clip_observation(actual[k],layout)?);Ok([component(0)?,component(1)?,component(2)?])};
        let edge_bound=|i:usize|->Result<[ConstructionJet;3]>{let a=&source.points[i];let b=&source.points[indices[0]];let reference=native_reference_difference_error::<S>(&a.reference,&b.reference,layout)?;let actual=a.edge_from(b).map_err(|_|"native chart corner original edge is invalid")?;let component=|k|input(a.displacement[k])?.sub(&input(b.displacement[k])?)?.add(&reference[k])?.reconcile(&clip_observation(actual[k],layout)?);Ok([component(0)?,component(1)?,component(2)?])};
        let origin_bound=point_bound(indices[0])?;
        let chart_bound=[chart_point_expression_error(&charts[0],anchor,chart[0]).map_err(|_|"native chart first expression is unproved")?,chart_point_expression_error(&charts[1],anchor,chart[1]).map_err(|_|"native chart second expression is unproved")?,chart_point_expression_error(&charts[2],anchor,chart[2]).map_err(|_|"native chart third expression is unproved")?];
        if chart_bound.iter().flatten().any(|v|v.layout!=layout){return Err("native chart corner expression layout differs");}
        let cross_bound=|a:&[ConstructionJet;3],b:&[ConstructionJet;3]|->Result<[ConstructionJet;3]>{Ok([a[1].mul(&b[2])?.sub(&a[2].mul(&b[1])?)?,a[2].mul(&b[0])?.sub(&a[0].mul(&b[2])?)?,a[0].mul(&b[1])?.sub(&a[1].mul(&b[0])?)?])};
        let edge=|i:usize|->Result<[ConstructionJet;3]>{Ok([chart_bound[i][0].sub(&chart_bound[0][0])?,chart_bound[i][1].sub(&chart_bound[0][1])?,chart_bound[i][2].sub(&chart_bound[0][2])?])};
        let native_bound=cross_bound(&edge_bound(indices[1])?,&edge_bound(indices[2])?)?;
        let chart_normal_bound=cross_bound(&edge(1)?,&edge(2)?)?;
        let dot=|a:[S;3],b:[S;3]|(0..3).fold(S::zero(),|sum,k|sum+a[k]*b[k]);
        let dot_bound=|a:&[ConstructionJet;3],b:&[ConstructionJet;3]|->Result<ConstructionJet>{let mut value=input(S::zero())?;for k in 0..3{value=value.add(&a[k].mul(&b[k])?)?;}Ok(value)};
        let k=(axis+1)%3;let l=(axis+2)%3;
        let a=dot(normal,origin)-normal[axis]*coordinate;
        let b=dot(chart_normal,chart[0])-chart_normal[axis]*coordinate;
        let determinant=normal[k]*chart_normal[l]-normal[l]*chart_normal[k];
        let determinant_bound=native_bound[k].mul(&chart_normal_bound[l])?.sub(&native_bound[l].mul(&chart_normal_bound[k])?)?;
        if !(determinant_bound.strictly_positive() || determinant_bound.strictly_negative()){return Err("native chart corner original rank is unproved");}
        let a_bound=dot_bound(&native_bound,&origin_bound)?.sub(&native_bound[axis].mul(&fixed)?)?;
        let b_bound=dot_bound(&chart_normal_bound,&chart_bound[0])?.sub(&chart_normal_bound[axis].mul(&fixed)?)?;
        let mut point=[S::zero();3];point[axis]=coordinate;
        point[k]=(a*chart_normal[l]-normal[l]*b)/determinant;
        point[l]=(normal[k]*b-a*chart_normal[k])/determinant;
        let mut bounds=[fixed.clone(),fixed.clone(),fixed.clone()];
        bounds[k]=a_bound.mul(&chart_normal_bound[l])?.sub(&native_bound[l].mul(&b_bound)?)?.div(&determinant_bound)?.reconcile(&clip_observation(point[k],layout)?)?;
        bounds[l]=native_bound[k].mul(&b_bound)?.sub(&a_bound.mul(&chart_normal_bound[k])?)?.div(&determinant_bound)?.reconcile(&clip_observation(point[l],layout)?)?;
        for j in 0..3{if !bounds[j].matches(point[j]){return Err("native chart corner output binding differs");}}
        Ok((point,bounds))
    };
    execute().map_err(implexity_core::CaeError::contract)
}


pub(crate) fn native_chart_point_expression_rank<S:crate::gas_transient_kinetic::TransientGasScalar>(
    source:&crate::native_geometry_source::NativeElementSource<S>,nodes:[usize;3],
    chart:&crate::native_geometry_source::NativeChartPointSource<S>,anchor:[f64;3],
)->implexity_core::CaeResult<i32> {
    source.validate()?;
    let indices=source.face_indices(nodes).ok_or_else(||implexity_core::CaeError::contract("native chart rank face is absent"))?;
    let origin=source.points[indices[0]].point_in_frame(anchor)?;
    let point=chart.point_in_frame(anchor)?;
    let u=source.points[indices[1]].edge_from(&source.points[indices[0]])?;
    let v=source.points[indices[2]].edge_from(&source.points[indices[0]])?;
    let execute=||->Result<i32>{
        let width=origin.into_iter().chain(point).chain(u).chain(v).map(|x|x.geometry_first_order_directions().map(|d|d.len()).ok_or("native chart rank channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("native chart rank mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|x:S|ConstructionJet::input(layout,&clip_observation(x,layout)?);
        let original=&source.points[indices[0]];
        let reference=native_reference_error::<S>(&original.reference,anchor,layout)?;
        let origin_component=|k|input(original.displacement[k])?.add(&reference[k])?.reconcile(&clip_observation(origin[k],layout)?);
        let origin_bound=[origin_component(0)?,origin_component(1)?,origin_component(2)?];
        let edge=|i:usize,actual:[S;3]|->Result<[ConstructionJet;3]>{
            let a=&source.points[i];let reference=native_reference_difference_error::<S>(&a.reference,&original.reference,layout)?;
            let component=|k|input(a.displacement[k])?.sub(&input(original.displacement[k])?)?.add(&reference[k])?.reconcile(&clip_observation(actual[k],layout)?);
            Ok([component(0)?,component(1)?,component(2)?])
        };
        let a=edge(indices[1],u)?;let b=edge(indices[2],v)?;
        let point_bound=chart_point_expression_error(chart,anchor,point).map_err(|_|"native chart rank expression is unproved")?;
        if point_bound.iter().any(|v|v.layout!=layout){return Err("native chart rank expression layout differs");}
        let c=[point_bound[0].sub(&origin_bound[0])?,point_bound[1].sub(&origin_bound[1])?,point_bound[2].sub(&origin_bound[2])?];
        let normal=[a[1].mul(&b[2])?.sub(&a[2].mul(&b[1])?)?,a[2].mul(&b[0])?.sub(&a[0].mul(&b[2])?)?,a[0].mul(&b[1])?.sub(&a[1].mul(&b[0])?)?];
        let determinant=normal[0].mul(&c[0])?.add(&normal[1].mul(&c[1])?)?.add(&normal[2].mul(&c[2])?)?;
        Ok(if determinant.strictly_positive(){1}else if determinant.strictly_negative(){-1}else{0})
    };
    execute().map_err(implexity_core::CaeError::contract)
}


pub(crate) fn chart_native_point_expression_rank<S:crate::gas_transient_kinetic::TransientGasScalar>(
    source:&crate::native_geometry_source::NativeElementSource<S>,node:usize,
    charts:&[crate::native_geometry_source::NativeChartPointSource<S>;3],anchor:[f64;3],
)->implexity_core::CaeResult<i32> {
    source.validate()?;
    let index=source.nodes.iter().position(|id|*id==node).ok_or_else(||implexity_core::CaeError::contract("chart native rank node is absent"))?;
    let original=&source.points[index];let point=original.point_in_frame(anchor)?;
    let points=[charts[0].point_in_frame(anchor)?,charts[1].point_in_frame(anchor)?,charts[2].point_in_frame(anchor)?];
    let execute=||->Result<i32>{
        let width=points.into_iter().flatten().chain(point).map(|x|x.geometry_first_order_directions().map(|d|d.len()).ok_or("chart native rank channels opaque")).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(0);
        let layout=if S::geometry_has_time_channel(){if width==0 || width%2!=1{return Err("chart native rank mixed layout invalid");}JetLayout::TimeMixed((width-1)/2)}else{JetLayout::FirstOrder(width)};
        let input=|x:S|ConstructionJet::input(layout,&clip_observation(x,layout)?);
        let reference=native_reference_error::<S>(&original.reference,anchor,layout)?;
        let component=|k|input(original.displacement[k])?.add(&reference[k])?.reconcile(&clip_observation(point[k],layout)?);
        let native=[component(0)?,component(1)?,component(2)?];
        let p=[chart_point_expression_error(&charts[0],anchor,points[0]).map_err(|_|"chart native rank first expression is unproved")?,chart_point_expression_error(&charts[1],anchor,points[1]).map_err(|_|"chart native rank second expression is unproved")?,chart_point_expression_error(&charts[2],anchor,points[2]).map_err(|_|"chart native rank third expression is unproved")?];
        if p.iter().flatten().any(|v|v.layout!=layout){return Err("chart native rank expression layout differs");}
        let a=[p[1][0].sub(&p[0][0])?,p[1][1].sub(&p[0][1])?,p[1][2].sub(&p[0][2])?];
        let b=[p[2][0].sub(&p[0][0])?,p[2][1].sub(&p[0][1])?,p[2][2].sub(&p[0][2])?];
        let c=[native[0].sub(&p[0][0])?,native[1].sub(&p[0][1])?,native[2].sub(&p[0][2])?];
        let normal=[a[1].mul(&b[2])?.sub(&a[2].mul(&b[1])?)?,a[2].mul(&b[0])?.sub(&a[0].mul(&b[2])?)?,a[0].mul(&b[1])?.sub(&a[1].mul(&b[0])?)?];
        let determinant=normal[0].mul(&c[0])?.add(&normal[1].mul(&c[1])?)?.add(&normal[2].mul(&c[2])?)?;
        Ok(if determinant.strictly_positive(){1}else if determinant.strictly_negative(){-1}else{0})
    };
    execute().map_err(implexity_core::CaeError::contract)
}
