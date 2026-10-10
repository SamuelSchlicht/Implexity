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
use implexity_ad::Scalar;
use std::ops::{Add,Sub,Mul,Div,Neg,AddAssign,SubAssign,MulAssign,DivAssign};
#[derive(Clone,Copy,Debug,Default)]
pub(crate) struct ProducerRounded { value:f64, low:f64, high:f64 }
impl ProducerRounded {
    pub(crate) fn point(value:f64)->Self {Self{value,low:value,high:value}}
    fn bounded(value:f64,low:f64,high:f64)->Self {Self{value,low:low.next_down(),high:high.next_up()}}
    fn unknown(value:f64)->Self {Self{value,low:f64::NEG_INFINITY,high:f64::INFINITY}}
    pub(crate) fn snap(self)->Self {Self{value:self.value-self.value,low:self.low.min(0.),high:self.high.max(0.)}}
    pub(crate) fn contains(&self,value:f64)->bool {self.low.is_finite()&&self.high.is_finite()&&self.low<=value&&value<=self.high}
}
impl Add for ProducerRounded {type Output=Self;fn add(self,b:Self)->Self {Self::bounded(self.value+b.value,self.low+b.low,self.high+b.high)}}
impl Sub for ProducerRounded {type Output=Self;fn sub(self,b:Self)->Self {Self::bounded(self.value-b.value,self.low-b.high,self.high-b.low)}}
impl Mul for ProducerRounded {type Output=Self;fn mul(self,b:Self)->Self {let values=[self.low*b.low,self.low*b.high,self.high*b.low,self.high*b.high];if values.iter().any(|v|v.is_nan()){return Self::unknown(self.value*b.value)}Self::bounded(self.value*b.value,values.into_iter().fold(f64::INFINITY,f64::min),values.into_iter().fold(f64::NEG_INFINITY,f64::max))}}
impl Div for ProducerRounded {type Output=Self;fn div(self,b:Self)->Self {if b.low<=0.&&b.high>=0.{return Self::unknown(self.value/b.value)}let inv=Self::bounded(1./b.value,1./b.high,1./b.low);let mut out=self*inv;out.value=self.value/b.value;out}}
impl Neg for ProducerRounded {type Output=Self;fn neg(self)->Self {Self{value:-self.value,low:-self.high,high:-self.low}}}
macro_rules! scalar_rhs {($op:ident,$method:ident)=>{impl $op<f64> for ProducerRounded{type Output=Self;fn $method(self,b:f64)->Self{self.$method(Self::point(b))}}};}
scalar_rhs!(Add,add);scalar_rhs!(Sub,sub);scalar_rhs!(Mul,mul);scalar_rhs!(Div,div);
macro_rules! assign {($op:ident,$method:ident,$binary:tt)=>{impl $op for ProducerRounded{fn $method(&mut self,b:Self){*self=*self $binary b;}}};}
assign!(AddAssign,add_assign,+);assign!(SubAssign,sub_assign,-);assign!(MulAssign,mul_assign,*);assign!(DivAssign,div_assign,/);
impl std::iter::Sum for ProducerRounded {fn sum<I:Iterator<Item=Self>>(it:I)->Self{it.fold(Self::point(0.),|a,b|a+b)}}
impl Scalar for ProducerRounded {
fn from_f64(v:f64)->Self {Self::point(v)}
fn value(&self)->f64{self.value}
fn chain(self,f:f64,_df:f64,_d2f:f64)->Self{Self::unknown(f)}
fn chain2(_a:Self,_b:Self,f:f64,_fa:f64,_fb:f64,_faa:f64,_fab:f64,_fbb:f64)->Self{Self::unknown(f)}
fn lift(f:f64,_inputs:&[Self],_grad:&[f64],_hess:&[f64])->Self{Self::unknown(f)}
fn sqrt(self)->Self{if self.low<0.{return Self::unknown(self.value.sqrt())}Self::bounded(self.value.sqrt(),self.low.sqrt(),self.high.sqrt())}
}
pub(crate) fn producer_snap<S:Scalar>(d:S)->S {
    let mut out=d;
    if let Some(value)=(&mut out as &mut dyn std::any::Any).downcast_mut::<ProducerRounded>() {
        *value=value.snap();out
    }else{d-d.value()}
}
impl crate::gas_gap_mass::GasMassScalar for ProducerRounded {
fn implicit_solve(_factor:&implexity_linalg::lu::SparseLu,_rhs:&[Self])->implexity_core::CaeResult<Vec<Self>>{Err(implexity_core::CaeError::contract("producer interval implicit solve unsupported"))}
fn has_direction(self)->bool{false}
fn finite_first_order(self)->bool{self.value.is_finite()}
}
impl crate::gas_transient_kinetic::TransientGasScalar for ProducerRounded {
fn maximum_direction(self)->f64{0.}
fn first_order_directions(self)->Option<Vec<f64>>{Some(Vec::new())}
fn lift_geometry_partial(_value:f64,_inputs:&[Self],_partials:&[f64])->implexity_core::CaeResult<Self>{Err(implexity_core::CaeError::contract("producer interval approximate geometry lift unsupported"))}
}
