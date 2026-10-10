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
use crate::{Dual,error::AdError,tape::Pullback};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Hash)]
pub struct CompactScalarVar{index:usize}
impl CompactScalarVar{pub const fn index(self)->usize{self.index}}
enum Operation{Leaf,Scale{parent:usize,weight:f64},Add{left:usize,right:usize},Sub{left:usize,right:usize},Mul{left:usize,right:usize},Div{left:usize,right:usize},Map{parent:usize,weight:f64},Map2{left:usize,right:usize,left_weight:f64,right_weight:f64},Custom{index:usize}}
struct Node{value:f64,operation:Operation}
struct Custom{parents:Vec<usize>,pullback:Pullback}
#[derive(Default)]
pub struct CompactScalarTape{nodes:Vec<Node>,custom:Vec<Custom>}
pub struct CompactScalarGrads{values:Vec<Option<f64>>}
impl CompactScalarGrads{pub fn wrt(&self,var:CompactScalarVar)->Result<Vec<f64>,AdError>{self.values.get(var.index).map(|v|vec![v.unwrap_or(0.)]).ok_or_else(||AdError::Shape("scalar gradient variable absent".into()))}}
impl CompactScalarTape{
 pub fn new()->Self{Self::default()}
 fn push(&mut self,value:f64,operation:Operation)->CompactScalarVar{let var=CompactScalarVar{index:self.nodes.len()};self.nodes.push(Node{value,operation});var}
 fn value(&self,var:CompactScalarVar)->Result<f64,AdError>{self.nodes.get(var.index).map(|v|v.value).ok_or_else(||AdError::Shape("scalar variable absent".into()))}
 pub fn input(&mut self,value:f64)->CompactScalarVar{self.push(value,Operation::Leaf)}
 pub fn scale(&mut self,parent:CompactScalarVar,weight:f64)->Result<CompactScalarVar,AdError>{let value=weight*self.value(parent)?;Ok(self.push(value,Operation::Scale{parent:parent.index,weight}))}
 pub fn add(&mut self,left:CompactScalarVar,right:CompactScalarVar)->Result<CompactScalarVar,AdError>{let value=self.value(left)?+self.value(right)?;Ok(self.push(value,Operation::Add{left:left.index,right:right.index}))}
 pub fn sub(&mut self,left:CompactScalarVar,right:CompactScalarVar)->Result<CompactScalarVar,AdError>{let value=self.value(left)?-self.value(right)?;Ok(self.push(value,Operation::Sub{left:left.index,right:right.index}))}
 pub fn mul(&mut self,left:CompactScalarVar,right:CompactScalarVar)->Result<CompactScalarVar,AdError>{let value=self.value(left)?*self.value(right)?;Ok(self.push(value,Operation::Mul{left:left.index,right:right.index}))}
 pub fn div(&mut self,left:CompactScalarVar,right:CompactScalarVar)->Result<CompactScalarVar,AdError>{let value=self.value(left)?/self.value(right)?;Ok(self.push(value,Operation::Div{left:left.index,right:right.index}))}
 pub fn map(&mut self,parent:CompactScalarVar,function:impl Fn(Dual<1>)->Dual<1>)->Result<CompactScalarVar,AdError>{let output=function(Dual::variable(self.value(parent)?,0));Ok(self.push(output.re,Operation::Map{parent:parent.index,weight:output.eps[0]}))}
 pub fn map2(&mut self,left:CompactScalarVar,right:CompactScalarVar,function:impl Fn(Dual<2>,Dual<2>)->Dual<2>)->Result<CompactScalarVar,AdError>{let output=function(Dual::variable(self.value(left)?,0),Dual::variable(self.value(right)?,1));Ok(self.push(output.re,Operation::Map2{left:left.index,right:right.index,left_weight:output.eps[0],right_weight:output.eps[1]}))}
 pub fn custom(&mut self,inputs:&[CompactScalarVar],value:Vec<f64>,pullback:Pullback)->Result<CompactScalarVar,AdError>{if value.len()!=1{return Err(AdError::Shape("scalar custom output shape".into()))}for &input in inputs{self.value(input)?;}let index=self.custom.len();self.custom.push(Custom{parents:inputs.iter().map(|v|v.index).collect(),pullback});Ok(self.push(value[0],Operation::Custom{index}))}
 pub fn vjp(&self,output:CompactScalarVar,cotangent:&[f64])->Result<CompactScalarGrads,AdError>{self.value(output)?;if cotangent.len()!=1{return Err(AdError::Shape("scalar cotangent shape".into()))}let mut values=vec![None;self.nodes.len()];values[output.index]=Some(cotangent[0]);let accumulate=|values:&mut[Option<f64>],parent:usize,contribution:f64|{match &mut values[parent]{Some(value)=>*value+=contribution,value@None=>*value=Some(contribution)}};
  for index in (0..=output.index).rev(){let Some(bar)=values[index].take() else{continue};match &self.nodes[index].operation{
   Operation::Leaf=>{},
   Operation::Scale{parent,weight}=>accumulate(&mut values,*parent,weight*bar),
   Operation::Add{left,right}=>{accumulate(&mut values,*left,bar);accumulate(&mut values,*right,bar);},
   Operation::Sub{left,right}=>{accumulate(&mut values,*left,bar);accumulate(&mut values,*right,-bar);},
   Operation::Mul{left,right}=>{let a=bar*self.nodes[*right].value;let b=bar*self.nodes[*left].value;accumulate(&mut values,*left,a);accumulate(&mut values,*right,b);},
   Operation::Div{left,right}=>{let denominator=self.nodes[*right].value;let a=bar/denominator;let b=-bar*self.nodes[*left].value/(denominator*denominator);accumulate(&mut values,*left,a);accumulate(&mut values,*right,b);},
   Operation::Map{parent,weight}=>accumulate(&mut values,*parent,if bar==0.{0.}else{bar*weight}),
   Operation::Map2{left,right,left_weight,right_weight}=>{accumulate(&mut values,*left,if bar==0.{0.}else{bar*left_weight});accumulate(&mut values,*right,if bar==0.{0.}else{bar*right_weight});},
   Operation::Custom{index}=>{let node=&self.custom[*index];let result=(node.pullback)(&[bar])?;if result.len()!=node.parents.len(){return Err(AdError::Shape(format!("custom pullback returned {} cotangents for {} inputs",result.len(),node.parents.len())))}for (&parent,value) in node.parents.iter().zip(result){if value.len()!=1{return Err(AdError::Shape(format!("custom pullback cotangent of length {} for input of length 1",value.len())))}accumulate(&mut values,parent,value[0]);}},
  }values[index]=Some(bar);}
  Ok(CompactScalarGrads{values})
 }
}
