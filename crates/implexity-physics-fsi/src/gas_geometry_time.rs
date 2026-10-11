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

use crate::{gas_gap_mass::GasMassScalar, gas_transient_kinetic::TransientGasScalar};
use implexity_ad::{Dual, Scalar};
use implexity_core::CaeResult;
use implexity_linalg::lu::SparseLu;
use std::iter::Sum;
use std::ops::*;
pub trait GeometryTimeBase: TransientGasScalar {
    fn direction_count() -> usize;
    fn direction_at(self, index: usize) -> f64;
}
impl GeometryTimeBase for f64 {
    fn direction_count() -> usize {
        0
    }
    fn direction_at(self, _: usize) -> f64 {
        0.
    }
}
impl<const N: usize> GeometryTimeBase for Dual<N> {
    fn direction_count() -> usize {
        N
    }
    fn direction_at(self, index: usize) -> f64 {
        self.eps[index]
    }
}
impl GeometryTimeBase for crate::gas_vjp::ReverseScalar {
    fn direction_count() -> usize {
        0
    }
    fn direction_at(self, _: usize) -> f64 {
        0.
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct GeometryTime<S: GeometryTimeBase> {
    pub value: S,
    pub rate: S,
}
impl<S: GeometryTimeBase> GeometryTime<S> {
    pub fn new(value: S, rate: S) -> Self {
        Self { value, rate }
    }
}
impl<S: GeometryTimeBase> Add for GeometryTime<S> {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self::new(self.value + rhs.value, self.rate + rhs.rate)
    }
}
impl<S: GeometryTimeBase> Add<f64> for GeometryTime<S> {
    type Output = Self;
    fn add(self, rhs: f64) -> Self {
        self.add(Self::from_f64(rhs))
    }
}
impl<S: GeometryTimeBase> AddAssign for GeometryTime<S> {
    fn add_assign(&mut self, rhs: Self) {
        *self = self.add(rhs);
    }
}
impl<S: GeometryTimeBase> Sub for GeometryTime<S> {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.value - rhs.value, self.rate - rhs.rate)
    }
}
impl<S: GeometryTimeBase> Sub<f64> for GeometryTime<S> {
    type Output = Self;
    fn sub(self, rhs: f64) -> Self {
        self.sub(Self::from_f64(rhs))
    }
}
impl<S: GeometryTimeBase> SubAssign for GeometryTime<S> {
    fn sub_assign(&mut self, rhs: Self) {
        *self = self.sub(rhs);
    }
}
impl<S: GeometryTimeBase> Mul for GeometryTime<S> {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self::new(
            self.value * rhs.value,
            self.rate * rhs.value + self.value * rhs.rate,
        )
    }
}
impl<S: GeometryTimeBase> Mul<f64> for GeometryTime<S> {
    type Output = Self;
    fn mul(self, rhs: f64) -> Self {
        self.mul(Self::from_f64(rhs))
    }
}
impl<S: GeometryTimeBase> MulAssign for GeometryTime<S> {
    fn mul_assign(&mut self, rhs: Self) {
        *self = self.mul(rhs);
    }
}
impl<S: GeometryTimeBase> Div for GeometryTime<S> {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        Self::new(
            self.value / rhs.value,
            (self.rate - (self.value / rhs.value) * rhs.rate) / rhs.value,
        )
    }
}
impl<S: GeometryTimeBase> Div<f64> for GeometryTime<S> {
    type Output = Self;
    fn div(self, rhs: f64) -> Self {
        self.div(Self::from_f64(rhs))
    }
}
impl<S: GeometryTimeBase> DivAssign for GeometryTime<S> {
    fn div_assign(&mut self, rhs: Self) {
        *self = self.div(rhs);
    }
}
impl<S: GeometryTimeBase> Neg for GeometryTime<S> {
    type Output = Self;
    fn neg(self) -> Self {
        Self::new(-self.value, -self.rate)
    }
}
impl<S: GeometryTimeBase> Sum for GeometryTime<S> {
    fn sum<I: Iterator<Item = Self>>(items: I) -> Self {
        items.fold(Self::zero(), |a, b| a + b)
    }
}
impl<S: GeometryTimeBase> Scalar for GeometryTime<S> {
    fn from_f64(v: f64) -> Self {
        Self::new(S::from_f64(v), S::zero())
    }
    fn value(&self) -> f64 {
        self.value.value()
    }
    fn chain(self, f: f64, df: f64, d2f: f64) -> Self {
        Self::new(
            self.value.chain(f, df, d2f),
            self.rate * self.value.chain(df, d2f, 0.),
        )
    }
    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, faa: f64, fab: f64, fbb: f64) -> Self {
        let da = S::chain2(a.value, b.value, fa, faa, fab, 0., 0., 0.);
        let db = S::chain2(a.value, b.value, fb, fab, fbb, 0., 0., 0.);
        Self::new(
            S::chain2(a.value, b.value, f, fa, fb, faa, fab, fbb),
            a.rate * da + b.rate * db,
        )
    }
    fn lift(f: f64, inputs: &[Self], grad: &[f64], hess: &[f64]) -> Self {
        let values: Vec<_> = inputs.iter().map(|x| x.value).collect();
        let n = inputs.len();
        let mut rate = S::zero();
        for i in 0..n {
            let row = &hess[i * n..(i + 1) * n];
            rate += inputs[i].rate * S::lift(grad[i], &values, row, &vec![0.; n * n]);
        }
        Self::new(S::lift(f, &values, grad, hess), rate)
    }
}
impl<S: GeometryTimeBase> GasMassScalar for GeometryTime<S> {
    fn implicit_solve(factor: &SparseLu, rhs: &[Self]) -> CaeResult<Vec<Self>> {
        let values = S::implicit_solve(factor, &rhs.iter().map(|x| x.value).collect::<Vec<_>>())?;
        let rates = S::implicit_solve(factor, &rhs.iter().map(|x| x.rate).collect::<Vec<_>>())?;
        Ok(values
            .into_iter()
            .zip(rates)
            .map(|(value, rate)| Self::new(value, rate))
            .collect())
    }
    fn has_direction(self) -> bool {
        self.value.has_direction() || self.rate.has_direction() || self.rate.value() != 0.
    }
    fn finite_first_order(self) -> bool {
        self.value.finite_first_order() && self.rate.finite_first_order()
    }
}
impl<S: GeometryTimeBase> TransientGasScalar for GeometryTime<S> {
    fn geometry_has_time_channel() -> bool { true }
    fn geometry_product_factors_direction_bounds(factors:&[Self])->Option<Vec<f64>>{
        if factors.is_empty(){return Some(vec![]);}
        if factors.iter().any(|v|!v.finite_first_order()){return None;}
        let values=factors.iter().map(|v|v.value).collect::<Vec<_>>();
        let mut value=S::geometry_product_factors_direction_bounds(&values)?;
        let mut rates=Vec::new();let mut time=0.;
        for i in 0..factors.len(){
            let product=(0..factors.len()).map(|j|if j==i{factors[j].rate}else{factors[j].value}).collect::<Vec<_>>();
            rates.push(S::geometry_product_factors_direction_bounds(&product)?);
            let magnitudes=product.iter().map(|v|v.value().abs()).collect::<Vec<_>>();
            time+=crate::gas_transient_kinetic::geometry_conditioning_product(&magnitudes)?;
            if !time.is_finite(){return None;}
        }
        let width=rates.iter().map(Vec::len).chain(std::iter::once(value.len())).max().unwrap_or(0);
        if std::iter::once(&value).chain(&rates).any(|row|(!row.is_empty()&&row.len()!=width)||row.iter().any(|v|!v.is_finite()||*v<0.)){return None;}
        value.resize(width,0.);let mut mixed=vec![0.;width];
        for row in rates{for(k,bound)in row.into_iter().enumerate(){mixed[k]+=bound;if !mixed[k].is_finite(){return None;}}}
        value.push(time);value.extend(mixed);Some(value)
    }

    fn supports_parallel_face_evaluation() -> bool { S::supports_parallel_face_evaluation() }
    fn geometry_product_direction_bounds(self, other: Self) -> Option<Vec<f64>> {
        let mut value = self.value.geometry_product_direction_bounds(other.value)?;
        let mut a = self.rate.geometry_product_direction_bounds(other.value)?;
        let mut b = self.value.geometry_product_direction_bounds(other.rate)?;
        let dimension=value.len().max(a.len()).max(b.len());
        if [&value,&a,&b].iter().any(|v|!v.is_empty() && v.len()!=dimension) {return None;}
        value.resize(dimension,0.);a.resize(dimension,0.);b.resize(dimension,0.);
        value.push(self.rate.value().abs()*other.value.value().abs()
            +self.value.value().abs()*other.rate.value().abs());
        value.extend(a.iter().zip(&b).map(|(a,b)|a+b));
        Some(value)
    }

    fn geometry_first_order_directions(self) -> Option<Vec<f64>> {
        let mut directions = self.value.geometry_first_order_directions()?;
        let mut rate = self.rate.geometry_first_order_directions()?;
        let count = directions.len().max(rate.len());
        directions.resize(count, 0.);
        rate.resize(count, 0.);
        directions.push(self.rate.value());
        directions.extend(rate);
        Some(directions)
    }

    fn maximum_direction(self) -> f64 {
        self.value
            .maximum_direction()
            .max(self.rate.maximum_direction())
            .max(self.rate.value().abs())
    }
}
