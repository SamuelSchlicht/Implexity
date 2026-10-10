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

use super::*;
use std::any::TypeId;

pub(crate) struct PresentPrimalCornerIncidence<'a,'g,B:GeometryTimeBase>{
 source:&'a NativeSourceFaceWindows<'g,B>,
 target:&'a PhysicalFacet<GeometryTime<B>>,
 candidate_facet:&'a PhysicalFacet<GeometryTime<B>>,
 window:usize,
 candidate:usize,
 corner:usize,
 target_corner:usize,
 point_bits:[[u64;2];3],
}

impl<'g,B:GeometryTimeBase> NativeSourceFaceWindows<'g,B>{
 pub(crate) fn present_primal_corner_incidence<'a>(&'a self,window:usize,candidate:usize,corner:usize)->CaeResult<Option<PresentPrimalCornerIncidence<'a,'g,B>>>{
  let w=self.windows.get(window).ok_or_else(||fail("present corner window absent"))?;
  let f=self.windows.get(candidate).ok_or_else(||fail("present corner candidate absent"))?;
  if corner>=3{return Err(fail("present corner index absent"));}
  if !matches!(w.kind,SourceWindowKind::NativePhysical)||!matches!(f.kind,SourceWindowKind::NativePhysical){return Ok(None);}
  let target=&w.facet;
  let candidate_facet=&f.facet;
  let Some(target_body)=target.owner.get(1) else{return Err(fail("present corner target body absent"));};
  let Some(candidate_body)=candidate_facet.owner.get(1) else{return Err(fail("present corner candidate body absent"));};
  if target_body==candidate_body{return Ok(None);}
  let point=candidate_facet.vertices[corner];
  if target.vertices.iter().chain(candidate_facet.vertices.iter()).flatten().any(|v|!v.finite_first_order()){
   return Err(fail("present corner source nonfinite"));
  }
  let matches=(0..3).filter(|&j|(0..3).all(|k|point[k].value.value().to_bits()==target.vertices[j][k].value.value().to_bits())).collect::<Vec<_>>();
  if matches.is_empty(){return Ok(None);}
  if matches.len()!=1{return Err(fail("present corner target incidence ambiguous"));}
  if TypeId::of::<B>()!=TypeId::of::<f64>(){
   return Err(fail("independent present corner has no derivative continuation"));
  }
  if target.vertices.iter().chain(candidate_facet.vertices.iter()).flatten().any(|v|v.rate.value()!=0.){
   return Err(fail("independent present corner has no moving continuation"));
  }
  if target.vertex_features.iter().chain(candidate_facet.vertex_features.iter()).any(|v|v.is_empty()){
   return Err(fail("present corner feature absent"));
  }
  Ok(Some(PresentPrimalCornerIncidence{source:self,target,candidate_facet,window,candidate,corner,target_corner:matches[0],point_bits:point.map(|p|[p.value.value().to_bits(),p.rate.value().to_bits()])}))
 }
}

impl<B:GeometryTimeBase> PresentPrimalCornerIncidence<'_,'_,B>{
 pub(crate) fn coordinates(&self,source:&NativeSourceFaceWindows<'_,B>,window:usize,candidate:usize,corner:usize,point:[GeometryTime<B>;3])->CaeResult<[GeometryTime<B>;2]>{
  if !std::ptr::eq(self.source,source)||[self.window,self.candidate,self.corner]!=[window,candidate,corner]{
   return Err(fail("present corner owner differs"));
  }
  let w=source.windows.get(window).ok_or_else(||fail("present corner window absent"))?;
  let f=source.windows.get(candidate).ok_or_else(||fail("present corner candidate absent"))?;
  if !std::ptr::eq(self.target,&w.facet)||!std::ptr::eq(self.candidate_facet,&f.facet)||point.map(|p|[p.value.value().to_bits(),p.rate.value().to_bits()])!=self.point_bits{
   return Err(fail("present corner source binding differs"));
  }
  if TypeId::of::<B>()!=TypeId::of::<f64>(){return Err(fail("present corner scalar differs"));}
  let uv=[[0.,0.],[1.,0.],[0.,1.]][self.target_corner];
  Ok(uv.map(GeometryTime::<B>::from_f64))
 }
}
