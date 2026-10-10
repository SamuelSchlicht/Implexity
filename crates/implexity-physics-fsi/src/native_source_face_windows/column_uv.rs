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
#[path="column_uv/roundoff.rs"] pub(crate) mod roundoff;
fn identical<S:TransientGasScalar>(a:S,b:S)->bool{a.value().to_bits()==b.value().to_bits()&&match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(x),Some(y))=>x.len()==y.len()&&x.iter().zip(y).all(|(a,b)|a.to_bits()==b.to_bits()),_=>false}}
fn point_same<S:TransientGasScalar>(a:[S;3],b:[S;3])->bool{(0..3).all(|i|identical(a[i],b[i]))}
pub(crate) struct SourceColumnUv<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,window_corners:[usize;2],layers:[usize;3],paired_vertex:usize,lower:[GeometryTime<S>;3],upper:[GeometryTime<S>;3],point:[GeometryTime<S>;3],uv:[f64;2]}
impl<'a,'g,S:GeometryTimeBase> SourceColumnUv<'a,'g,S>{
 pub(crate) fn source(&self)->&'a NativeSourceFaceWindows<'g,S>{self.source}
 pub(crate) fn window(&self)->usize{self.window}
 pub(crate) fn candidate(&self)->usize{self.candidate}
 pub(crate) fn corner(&self)->usize{self.corner}
 pub(crate) fn window_corners(&self)->[usize;2]{self.window_corners}
 pub(crate) fn layers(&self)->[usize;3]{self.layers}
 pub(crate) fn paired_vertex(&self)->usize{self.paired_vertex}
 pub(crate) fn operands(&self)->[[GeometryTime<S>;3];2]{[self.lower,self.upper]}
 pub(crate) fn point(&self)->[GeometryTime<S>;3]{self.point}
 pub(crate) fn uv(&self)->[f64;2]{self.uv}
 pub(crate) fn validate_point(&self,source:&NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,point:[GeometryTime<S>;3])->CaeResult<()>{if !std::ptr::eq(self.source,source)||self.window!=window||self.candidate!=candidate||self.corner!=corner||!point_same(self.point,point){return Err(fail("source column UV binding differs"));}Ok(())}
}
impl<'g,S:GeometryTimeBase> NativeSourceFaceWindows<'g,S>{
 pub(crate) fn column_uv<'a>(&'a self,window:usize,candidate:usize,corner:usize)->CaeResult<Option<SourceColumnUv<'a,'g,S>>>{
 let w=self.windows.get(window).ok_or_else(||fail("source column window absent"))?;let f=self.windows.get(candidate).ok_or_else(||fail("source column candidate absent"))?;
 if corner>=3{return Err(fail("source column corner absent"));}
 if !matches!(w.kind,SourceWindowKind::Layered{..})||!matches!(f.kind,SourceWindowKind::Layered{..}){return Ok(None);}
 let feature=&f.facet.vertex_features[corner];if feature.len()!=3||feature[0]!=2{return Err(fail("source column feature shape differs"));}let pair=feature[1];let level=feature[2];if level>self.adapter.layers{return Err(fail("source column level outside actual mesh"));}
 let mut endpoints=Vec::new();for k in 0..3{let q=&w.facet.vertex_features[k];if q.len()==3&&q[0]==2&&q[1]==pair{endpoints.push((k,q[2]));}}
 if endpoints.len()!=2||endpoints[0].1==endpoints[1].1{return Ok(None);}
 let [(k0,l0),(k1,l1)]=[endpoints[0],endpoints[1]];
 for (chart,ids) in self.adapter.paired_vertex_ids.iter().enumerate(){for c in 0..3{if ids[c]!=pair{continue;}let p=self.mapped_columns[chart];let at=|l:usize|std::array::from_fn(|a|p[c][a]+(p[c+3][a]-p[c][a])*(l as f64/self.adapter.layers as f64));
 if !point_same(at(l0),w.facet.vertices[k0])||!point_same(at(l1),w.facet.vertices[k1])||!point_same(at(level),f.facet.vertices[corner]){continue;}
 if self.adapter.layers==0||self.adapter.layers as u128>(1u128<<53)||[l0,l1,level].iter().any(|&l|l as u128>(1u128<<53)){return Err(fail("source column layer integers are not exactly represented"));}
 let fractions=[l0,l1,level].map(|l|l as f64/self.adapter.layers as f64);let denominator=fractions[1]-fractions[0];if denominator==0.||!denominator.is_finite(){return Err(fail("source column rounded fractions lose rank"));}
 let lambda=(fractions[2]-fractions[0])/denominator;let corners=[[0.,0.],[1.,0.],[0.,1.]];let uv=std::array::from_fn(|a|corners[k0][a]+(corners[k1][a]-corners[k0][a])*lambda);
 return Ok(Some(SourceColumnUv{source:self,window,candidate,corner,window_corners:[k0,k1],layers:[l0,l1,level],paired_vertex:pair,lower:p[c],upper:p[c+3],point:f.facet.vertices[corner],uv}));
 }}
 Err(fail("source column vertices differ from actual interpolation recipe"))
 }
}

pub(crate) struct SourceColumnCoordinates<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize}
impl<'a,'g,S:GeometryTimeBase> SourceColumnCoordinates<'a,'g,S>{
 pub(crate) fn source(&self)->&'a NativeSourceFaceWindows<'g,S>{self.source}
 pub(crate) fn window(&self)->usize{self.window}
 pub(crate) fn validate_window(&self,facet:&PhysicalFacet<GeometryTime<S>>)->CaeResult<()>{let w=&self.source.windows[self.window].facet;if w.owner!=facet.owner||w.vertex_features!=facet.vertex_features||(0..3).any(|i|!point_same(w.vertices[i],facet.vertices[i])){return Err(fail("source column coordinate window binding differs"));}Ok(())}
 pub(crate) fn corner_uv(&self,facet:&PhysicalFacet<GeometryTime<S>>,corner:usize)->CaeResult<Option<[GeometryTime<S>;2]>>{
 let &candidate=self.source.facet_owner_index.get(&(facet.owner.clone(),facet.vertex_features.clone())).ok_or_else(||fail("source column coordinate facet owner absent"))?;let actual=&self.source.windows[candidate].facet;
 if actual.vertex_features!=facet.vertex_features||(0..3).any(|i|!point_same(actual.vertices[i],facet.vertices[i])){return Err(fail("source column coordinate facet binding differs"));}
 match self.source.column_uv(self.window,candidate,corner)?{Some(t)=>{let proof=t.certify_recomposition()?;Ok(Some(proof.uv().map(GeometryTime::<S>::from_f64)))},None=>Ok(None)}
 }
}
impl<'g,S:GeometryTimeBase> NativeSourceFaceWindows<'g,S>{
 pub(crate) fn column_coordinates<'a>(&'a self,window:usize)->CaeResult<SourceColumnCoordinates<'a,'g,S>>{if window>=self.windows.len(){return Err(fail("source column coordinate window absent"));}Ok(SourceColumnCoordinates{source:self,window})}
}
