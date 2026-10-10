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
use implexity_core::{CaeError,CaeResult};

#[derive(Clone,Copy)]
pub(crate) enum NormalFormula{OrientedArea(f64),SourcePlane}
pub(crate) struct OwnedNormalChart<S:Scalar>{chart:[S;3],authored:[S;3]}
impl<S:Scalar> OwnedNormalChart<S>{
 pub(crate) fn component(&self,index:usize)->CaeResult<(S,S)>{
  if index>=3{return Err(CaeError::contract("normal chart component is absent"));}
  Ok((self.chart[index],self.authored[index]))
 }
}
fn authored_normal<S:Scalar>(raw:[S;3],weight:S,formula:NormalFormula)->[S;3]{
 match formula{NormalFormula::OrientedArea(orientation)=>raw.map(|v|v*orientation/weight),NormalFormula::SourcePlane=>raw.map(|v|v/weight)}
}
fn prepare<S:Scalar>(raw:[S;3],weight:S,formula:NormalFormula)->Option<OwnedNormalChart<S>>{
 let orientation=match formula{NormalFormula::OrientedArea(v)=>v,NormalFormula::SourcePlane=>1.};
 if orientation!=1.&&orientation!= -1.||!weight.is_finite()||weight.value()<=0.||raw.iter().any(|v|!v.is_finite()){return None;}
 let length=raw.iter().copied().map(|v|v*v).fold(S::zero(),|a,b|a+b).sqrt();
 if !length.is_finite()||length.value().to_bits()!=weight.value().to_bits(){return None;}
 let authored=authored_normal(raw,weight,formula);
 if authored.iter().any(|v|!v.is_finite()){return None;}
 let mut pivot=0;for k in 1..3{if raw[k].value().abs()>raw[pivot].value().abs(){pivot=k;}}
 if raw[pivot].value()==0.{return None;}
 let sign=if raw[pivot].value()<0.{-orientation}else{orientation};
 let other=match pivot{0=>[1,2],1=>[0,2],_=>[0,1]};
 let a=raw[other[0]]/raw[pivot];let b=raw[other[1]]/raw[pivot];
 let denominator=(S::one()+a*a+b*b).sqrt();let axial=S::from_f64(sign)/denominator;
 let mut chart=[S::zero();3];chart[pivot]=axial;chart[other[0]]=a*axial;chart[other[1]]=b*axial;
 if chart.iter().any(|v|!v.is_finite()){return None;}
 for k in 0..3{
  let x=chart[k].value();let y=authored[k].value();
  if x==0.||y==0.{if x!=0.||y!=0.{return None;}}else{
   let scale=x.abs().max(y.abs());let difference=(x-y).abs();
   if !difference.is_finite()||difference>8.*f64::EPSILON*scale{return None;}
  }
 }
 Some(OwnedNormalChart{chart,authored})
}
pub(crate) fn normalize_with_rebind<S:Scalar>(raw:[S;3],weight:S,formula:NormalFormula,rebind:impl Fn(&OwnedNormalChart<S>,usize)->CaeResult<S>)->CaeResult<[S;3]>{
 let authored=authored_normal(raw,weight,formula);
 let Some(owner)=prepare(raw,weight,formula)else{return Ok(authored);};
 let result=[rebind(&owner,0)?,rebind(&owner,1)?,rebind(&owner,2)?];
 if result.iter().any(|v|!v.is_finite())||(0..3).any(|k|result[k].value().to_bits()!=authored[k].value().to_bits()){
  return Err(CaeError::contract("normal chart authored output bits differ"));
 }
 Ok(result)
}
pub(crate) fn normalize<S:Scalar>(raw:[S;3],weight:S,formula:NormalFormula)->[S;3]{
 let authored=authored_normal(raw,weight,formula);
 normalize_with_rebind(raw,weight,formula,|owner,index|{
  let(chart,authored)=owner.component(index)?;
  Ok(chart.chain3(authored.value(),1.,0.,||0.))
 }).unwrap_or(authored)
}
