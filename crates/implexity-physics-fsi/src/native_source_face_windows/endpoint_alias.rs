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
use super::column_uv::{SourceColumnUv,roundoff::ColumnRecompositionProof};
use super::parallelogram_uv::{SourceParallelogramUv,ParallelogramRecompositionProof};
use super::affine_corner::{SourceAffineCorner,AffineCornerProof};
use super::reference_fibre::{SourceReferenceFibreUv,ReferenceFibreProof};
use crate::moving_contact::surface_map::SurfaceFeature;
fn same<S:TransientGasScalar>(a:S,b:S)->bool{a.value().to_bits()==b.value().to_bits()&&match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(x),Some(y))=>x.iter().map(|v|v.to_bits()).eq(y.iter().map(|v|v.to_bits())),_=>false}}
fn point_same<S:TransientGasScalar>(a:[S;3],b:[S;3])->bool{(0..3).all(|k|same(a[k],b[k]))}
enum BaseToken<'a,'g,S:GeometryTimeBase>{Column(SourceColumnUv<'a,'g,S>),Parallelogram(SourceParallelogramUv<'a,'g,S>),AffineCorner(SourceAffineCorner<'a,'g,S>),ReferenceFibre(SourceReferenceFibreUv<'a,'g,S>)}
enum BaseProof<'a,'g,S:GeometryTimeBase>{Column(ColumnRecompositionProof<'a,'g,S>),Parallelogram(ParallelogramRecompositionProof<'a,'g,S>),AffineCorner(AffineCornerProof<'a,'g,S>),ReferenceFibre(ReferenceFibreProof<'a,'g,S>)}
pub(crate) struct NativeEndpointAlias<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,layered_candidate:usize,layered_corner:usize,point:[GeometryTime<S>;3],base:BaseToken<'a,'g,S>}
pub(crate) struct NativeEndpointAliasProof<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,layered_candidate:usize,layered_corner:usize,point:[GeometryTime<S>;3],base:BaseProof<'a,'g,S>}
impl<'a,'g,S:GeometryTimeBase> NativeEndpointAlias<'a,'g,S>{
 pub(crate) fn uses_reference_fibre(&self)->bool{matches!(self.base,BaseToken::ReferenceFibre(_))}
 pub(crate) fn point(&self)->[GeometryTime<S>;3]{self.point}
 pub(crate) fn certify_recomposition(&self)->CaeResult<NativeEndpointAliasProof<'a,'g,S>>{
  let base=match &self.base{BaseToken::Column(t)=>BaseProof::Column(t.certify_recomposition()?),BaseToken::Parallelogram(t)=>BaseProof::Parallelogram(t.certify_recomposition()?),BaseToken::AffineCorner(t)=>BaseProof::AffineCorner(t.certify_recomposition()?),BaseToken::ReferenceFibre(t)=>BaseProof::ReferenceFibre(t.certify_recomposition()?)};
  Ok(NativeEndpointAliasProof{source:self.source,window:self.window,candidate:self.candidate,corner:self.corner,layered_candidate:self.layered_candidate,layered_corner:self.layered_corner,point:self.point,base})
 }
}
impl<S:GeometryTimeBase> NativeEndpointAliasProof<'_,'_,S>{
 pub(crate) fn uv(&self)->[f64;2]{match &self.base{BaseProof::Column(p)=>p.uv(),BaseProof::Parallelogram(p)=>p.uv(),BaseProof::AffineCorner(p)=>p.uv(),BaseProof::ReferenceFibre(p)=>p.uv()}}
 pub(crate) fn validate_owner(&self,source:&NativeSourceFaceWindows<'_,S>,window:usize,candidate:usize,corner:usize)->CaeResult<()>{
  if !std::ptr::eq(self.source,source)||[self.window,self.candidate,self.corner]!=[window,candidate,corner]{return Err(fail("native endpoint alias owner differs"));}
  match &self.base{BaseProof::Column(p)=>p.validate_owner(source,window,self.layered_candidate,self.layered_corner),BaseProof::Parallelogram(p)=>p.validate_owner(source,window,self.layered_candidate,self.layered_corner),BaseProof::AffineCorner(p)=>p.validate_owner(source,window,self.layered_candidate,self.layered_corner),BaseProof::ReferenceFibre(p)=>p.validate_owner(source,window,self.layered_candidate,self.layered_corner)}
 }
 pub(crate) fn validate_bindings(&self,point:[GeometryTime<S>;3],reconstructed:[GeometryTime<S>;3],uv:[f64;2])->CaeResult<()>{
  if !point_same(self.point,point){return Err(fail("native endpoint alias fullpoint differs"));}
  match &self.base{BaseProof::Column(p)=>p.validate_bindings(point,reconstructed,uv),BaseProof::Parallelogram(p)=>p.validate_bindings(point,reconstructed,uv),BaseProof::AffineCorner(p)=>p.validate_bindings(point,reconstructed,uv),BaseProof::ReferenceFibre(p)=>p.validate_bindings(point,reconstructed,uv)}
 }
}
impl<'g,S:GeometryTimeBase> NativeSourceFaceWindows<'g,S>{
 pub(crate) fn native_endpoint_alias<'a>(&'a self,window:usize,candidate:usize,corner:usize)->CaeResult<Option<NativeEndpointAlias<'a,'g,S>>>{
  let w=self.windows.get(window).ok_or_else(||fail("native alias window absent"))?;
  let f=self.windows.get(candidate).ok_or_else(||fail("native alias candidate absent"))?;
  if corner>=3{return Err(fail("native alias corner absent"));}
  if !matches!(w.kind,SourceWindowKind::Layered{..})||!matches!(f.kind,SourceWindowKind::NativePhysical){return Ok(None);}
  let feature=&f.facet.vertex_features[corner];
  if feature.first()==Some(&1){return Err(fail("native alias density-edge recipe is unsupported"));}
  if feature.len()!=3||feature[0]!=0{return Err(fail("native alias vertex feature malformed"));}
  let body=feature[1];let node=feature[2];let owner=self.adapter.owner.capacity_owner();
  self.adapter.kinematics.validate_native_owner_bindings(owner)?;
  if body>=2||node>=owner.nodes[body].len()||f.facet.owner.get(1)!=Some(&body){return Err(fail("native alias body or node owner differs"));}
  let point=f.facet.vertices[corner];
  for(chart,ids)in self.adapter.paired_vertex_ids.iter().enumerate(){
   let features=self.adapter.kinematics.chart_features(chart)?;
   for j in 0..3{
    if !matches!(features[body][j],SurfaceFeature::Vertex(n)if n==node){continue;}
    let mapped=self.mapped_columns[chart][body*3+j];
    if !point_same(mapped,point){return Err(fail("native alias chart and native fullpoint differ"));}
    let level=if body==0{0}else{self.adapter.layers};let key=vec![2,ids[j],level];
    for(layered_candidate,g)in self.windows.iter().enumerate(){if !matches!(g.kind,SourceWindowKind::Layered{..}){continue;}
     for layered_corner in 0..3{if g.facet.vertex_features[layered_corner]!=key{continue;}
      if !point_same(g.facet.vertices[layered_corner],point){return Err(fail("native alias layered and native fullpoint differ"));}
      let base=if let Some(t)=self.column_uv(window,layered_candidate,layered_corner)?{BaseToken::Column(t)}else if let Some(t)=self.parallelogram_uv(window,layered_candidate,layered_corner)?{BaseToken::Parallelogram(t)}else if let Some(t)=self.affine_corner_uv(window,layered_candidate,layered_corner)?{BaseToken::AffineCorner(t)}else if let Some(t)=self.reference_fibre_uv(window,layered_candidate,layered_corner)?{BaseToken::ReferenceFibre(t)}else{continue;};
      return Ok(Some(NativeEndpointAlias{source:self,window,candidate,corner,layered_candidate,layered_corner,point,base}));
     }
    }
   }
  }
  Ok(None)
 }
}
