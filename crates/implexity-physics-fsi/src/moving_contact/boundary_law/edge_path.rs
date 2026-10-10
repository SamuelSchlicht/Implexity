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

use implexity_core::{CaeError,CaeResult};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn down(x:f64)->f64{if x==f64::NEG_INFINITY{x}else if x==0.{-f64::from_bits(1)}else{f64::from_bits(if x>0.{x.to_bits()-1}else{x.to_bits()+1})}}
fn up(x:f64)->f64{-down(-x)}
#[derive(Clone,Copy)]struct I{lo:f64,hi:f64}
impl I{
 fn point(x:f64)->Self{Self{lo:x,hi:x}}
 fn add(self,b:Self)->Self{Self{lo:down(self.lo+b.lo),hi:up(self.hi+b.hi)}}
 fn sub(self,b:Self)->Self{Self{lo:down(self.lo-b.hi),hi:up(self.hi-b.lo)}}
 fn mul(self,b:Self)->Self{let v=[self.lo*b.lo,self.lo*b.hi,self.hi*b.lo,self.hi*b.hi];Self{lo:down(v.into_iter().fold(f64::INFINITY,f64::min)),hi:up(v.into_iter().fold(f64::NEG_INFINITY,f64::max))}}
 fn finite(self)->bool{self.lo.is_finite()&&self.hi.is_finite()&&self.lo<=self.hi}
 fn magnitude(self)->f64{self.lo.abs().max(self.hi.abs())}
}
fn dot(a:[I;3],b:[I;3])->I{(0..3).fold(I::point(0.),|v,i|v.add(a[i].mul(b[i])))}
fn cross(a:[I;3],b:[I;3])->[I;3]{[a[1].mul(b[2]).sub(a[2].mul(b[1])),a[2].mul(b[0]).sub(a[0].mul(b[2])),a[0].mul(b[1]).sub(a[1].mul(b[0]))]}
pub struct EdgePathCertificate{pub identity:String,pub leaves:usize,pub intervals_examined:usize,pub minimum_cross_squared_m4:f64}
pub fn certify_edge_path(old:[[f64;3];4],new:[[f64;3];4],orientation:f64,maximum_intervals:usize,maximum_depth:u32)->CaeResult<EdgePathCertificate>{
 if old.iter().chain(&new).flatten().any(|x|!x.is_finite())||(orientation!=1.&&orientation!= -1.)||maximum_intervals==0||maximum_depth==0||maximum_depth>64{return Err(fail("edge swept path input/policy"));}if !super::boundary_path::certify_edge_gap_path(old,new,orientation).map_err(fail)?{return Err(fail("whole swept native edge gap not certified"));}
 let mut pending=vec![(0.,1.,0u32)];let mut count=0;let mut leaves=0;let mut minimum=f64::INFINITY;
 while let Some((a,b,depth))=pending.pop(){count+=1;if count>maximum_intervals{return Err(fail("edge swept parameter certificate budget exhausted"));}let t=I{lo:a,hi:b};let p:[[I;3];4]=std::array::from_fn(|i|std::array::from_fn(|j|I::point(old[i][j]).add(t.mul(I::point(new[i][j]).sub(I::point(old[i][j]))))));let u=std::array::from_fn(|j|p[1][j].sub(p[0][j]));let v=std::array::from_fn(|j|p[3][j].sub(p[2][j]));let w=std::array::from_fn(|j|p[0][j].sub(p[2][j]));let n=cross(u,v);let den=dot(n,n);let aa=dot(u,u);let ab=dot(u,v);let bb=dot(v,v);let d=dot(u,w);let e=dot(v,w);let s=ab.mul(e).sub(bb.mul(d));let z=aa.mul(e).sub(ab.mul(d));let scale=u.iter().chain(&v).map(|x|x.magnitude()).fold(0.,f64::max);let threshold=I::point(1e-8).mul(I::point(1e-8)).mul(I::point(scale).mul(I::point(scale))).mul(I::point(scale).mul(I::point(scale)));let values=[den,aa,ab,bb,d,e,s,z,den.sub(s),den.sub(z),threshold];if values.iter().any(|x|!x.finite()){return Err(fail("edge path enclosure overflow"));}
  if den.lo>threshold.hi&&s.lo>=0.&&z.lo>=0.&&den.sub(s).lo>=0.&&den.sub(z).lo>=0.{minimum=minimum.min(den.lo);leaves+=1;continue;}if depth>=maximum_depth{return Err(fail("edge normal/parameter tie needs a boundary feature event owner"));}let mid=0.5*(a+b);if mid==a||mid==b{return Err(fail("edge swept temporal enclosure resolution"));}pending.push((mid,b,depth+1));pending.push((a,mid,depth+1));
 }
 let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"old":old,"new":new,"orientation":orientation,"maximum_intervals":maximum_intervals,"maximum_depth":maximum_depth,"leaves":leaves,"minimum_cross_squared_m4":minimum,"strict_gap_floor_m":0.}));Ok(EdgePathCertificate{identity,leaves,intervals_examined:count,minimum_cross_squared_m4:minimum})
}

pub fn edge_root_domain(old:[[f64;3];4],new:[[f64;3];4],interval:[f64;2],ratio:f64)->CaeResult<super::boundary_path::RootDomain>{
 use super::boundary_path::RootDomain;
 if old.iter().chain(&new).flatten().chain(&interval).any(|v|!v.is_finite())||interval[0]<0.||interval[1]>1.||interval[0]>interval[1]||!ratio.is_finite()||ratio<=0.||ratio>=1.{return Err(fail("edge root domain input"));}
 let t=I{lo:interval[0],hi:interval[1]};let p:[[I;3];4]=std::array::from_fn(|i|std::array::from_fn(|j|I::point(old[i][j]).add(t.mul(I::point(new[i][j]).sub(I::point(old[i][j]))))));let u=std::array::from_fn(|j|p[1][j].sub(p[0][j]));let v=std::array::from_fn(|j|p[3][j].sub(p[2][j]));let w=std::array::from_fn(|j|p[0][j].sub(p[2][j]));let n=cross(u,v);let den=dot(n,n);let aa=dot(u,u);let ab=dot(u,v);let bb=dot(v,v);let d=dot(u,w);let e=dot(v,w);let s=ab.mul(e).sub(bb.mul(d));let z=aa.mul(e).sub(ab.mul(d));let scale=u.iter().chain(&v).map(|x|x.magnitude()).fold(0.,f64::max);let threshold=I::point(ratio).mul(I::point(ratio)).mul(I::point(scale).mul(I::point(scale))).mul(I::point(scale).mul(I::point(scale)));let ds=den.sub(s);let dz=den.sub(z);
 if [den,aa,ab,bb,d,e,s,z,ds,dz,threshold].iter().any(|v|!v.finite()){return Err(fail("edge root enclosure overflow"));}
 if den.lo<=threshold.hi{return Ok(RootDomain::Unresolved);}if [s,z,ds,dz].iter().any(|v|v.hi<0.){return Ok(RootDomain::Outside);}if [s,z,ds,dz].iter().all(|v|v.lo>0.){Ok(RootDomain::Interior)}else{Ok(RootDomain::Unresolved)}
}
