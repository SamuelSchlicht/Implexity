// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_solve::{matrix::Jacobian,time_stepper::StepParameters};
use crate::moving_contact::contact_field::{NativeContactLaw,ContactContribution};
use crate::moving_contact::boundary_law::{boundary_path::{self,PathPolicy,PathStatus},swept_coverage::exhaustive_swept_candidates,plane_manifold::{CompleteSurfaceTrace,SupportingPlaneSelection,certify_supporting_plane}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn add(a:[f64;3],b:[f64;3])->[f64;3]{std::array::from_fn(|i|a[i]+b[i])}
fn sub(a:[f64;3],b:[f64;3])->[f64;3]{std::array::from_fn(|i|a[i]-b[i])}
#[derive(Clone,Debug)]pub enum PairSeparator{Face{body:usize,positive_other:bool},EdgeCross{edges:[usize;2],positive_other:bool}}
#[derive(Clone,Debug)]pub struct CertifiedShellPair{pub facets:[usize;2],pub separator:PairSeparator}
pub struct ShellPathCertificate{identity:String,pub examined_pairs:usize,pub broad_separated:usize,pub narrow_pairs:Vec<CertifiedShellPair>}
impl ShellPathCertificate{pub fn identity(&self)->&str{&self.identity}}
fn plane_admits(triangle_old:[[f64;3];3],triangle_new:[[f64;3];3],a_old:[[f64;3];3],a_new:[[f64;3];3],b_old:[[f64;3];3],b_new:[[f64;3];3],positive_other:bool)->CaeResult<bool>{
 let report=boundary_path::check_linear_path([triangle_old[0],triangle_old[0],triangle_old[1],triangle_old[2]],[triangle_new[0],triangle_new[0],triangle_new[1],triangle_new[2]],PathPolicy::default()).map_err(fail)?;if report.status!=PathStatus::Admitted{return Ok(false);}
 for i in 0..3{if !boundary_path::certify_supporting_star_path(triangle_old,triangle_new,triangle_old[0],triangle_new[0],a_old[i],a_new[i],!positive_other).map_err(fail)?||!boundary_path::certify_supporting_star_path(triangle_old,triangle_new,triangle_old[0],triangle_new[0],b_old[i],b_new[i],positive_other).map_err(fail)?{return Ok(false);}}
 Ok(true)
}
pub fn certify_complete_shell_path(old:[&[[f64;3]];2],new:[&[[f64;3]];2],facets:[&[[usize;3]];2],maximum_pairs:usize,source_identity:&str)->CaeResult<ShellPathCertificate>{
 if source_identity.len()!=64||!source_identity.bytes().all(|b|b.is_ascii_hexdigit()){return Err(fail("complete shell source identity"));}let broad=exhaustive_swept_candidates(old,new,facets,maximum_pairs)?;let mut certified=vec![];
 for pair in &broad.unresolved{let [i,j]=pair.facets;let f=facets[0][i];let g=facets[1][j];let a=[f.map(|k|old[0][k]),f.map(|k|new[0][k])];let b=[g.map(|k|old[1][k]),g.map(|k|new[1][k])];let mut separator=None;
  for positive in [true,false]{if plane_admits(a[0],a[1],a[0],a[1],b[0],b[1],positive)?{separator=Some(PairSeparator::Face{body:0,positive_other:positive});break;}if plane_admits(b[0],b[1],b[0],b[1],a[0],a[1],positive)?{separator=Some(PairSeparator::Face{body:1,positive_other:positive});break;}}
  if separator.is_none(){'edges:for e in 0..3{for h in 0..3{let plane=std::array::from_fn::<_,2,_>(|k|[a[k][e],a[k][(e+1)%3],add(a[k][e],sub(b[k][(h+1)%3],b[k][h]))]);for positive in [true,false]{if plane_admits(plane[0],plane[1],a[0],a[1],b[0],b[1],positive)?{separator=Some(PairSeparator::EdgeCross{edges:[e,h],positive_other:positive});break 'edges;}}}}}
  let separator=separator.ok_or_else(||fail("whole swept facet pair has no certified noncrossing separator; contact/feature event owner required"))?;certified.push(CertifiedShellPair{facets:pair.facets,separator});
 }
 let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"source":source_identity,"endpoints":broad.endpoint_identity,"maximum_pairs":maximum_pairs,"strict_gap_floor_m":0.,"pairs":certified.iter().map(|p|(p.facets,format!("{:?}",p.separator))).collect::<Vec<_>>(),"scope":"complete sampled boundary noncrossing certificate; initial interior disjointness separately authenticated"}));Ok(ShellPathCertificate{identity,examined_pairs:broad.examined_pairs,broad_separated:broad.aabb_separated+broad.exact_axis_separated,narrow_pairs:certified})
}
pub struct CompleteShellContact<L>{law:L,surface:CompleteSurfaceTrace,maximum_pairs:usize,initial_identity:String}
impl<L:NativeContactLaw> CompleteShellContact<L>{
 pub fn new(law:L,surface:CompleteSurfaceTrace,initial:&[f64],design:&[f64],initial_plane:SupportingPlaneSelection)->CaeResult<Self>{let points=surface.positions(initial,law.multipliers(),design)?;let initial_cert=certify_supporting_plane([&points[0],&points[1]],[&points[0],&points[1]],surface.facets(),initial_plane,surface.identity())?;let initial_identity=implexity_core::json::canonical_sha256(&serde_json::json!({"certificate":initial_cert.identity(),"initial_state_bits":initial.iter().map(|x|x.to_bits()).collect::<Vec<_>>(),"design_bits":design.iter().map(|x|x.to_bits()).collect::<Vec<_>>()}));Ok(Self{law,surface,maximum_pairs:initial_plane.maximum_pairs,initial_identity})}
 pub fn inner(&self)->&L{&self.law}
 pub fn initial_identity(&self)->&str{&self.initial_identity}
 pub fn certificate(&self,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<ShellPathCertificate>{let old=self.surface.positions(previous,self.law.multipliers(),design)?;let new=self.surface.positions(current,self.law.multipliers(),design)?;certify_complete_shell_path([&old[0],&old[1]],[&new[0],&new[1]],self.surface.facets(),self.maximum_pairs,self.surface.identity())}
}
impl<L:NativeContactLaw> NativeContactLaw for CompleteShellContact<L>{
 fn multipliers(&self)->usize{self.law.multipliers()}
 fn evaluate(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<ContactContribution>{self.law.evaluate(n,c,o,p)}
 fn check_state_domain(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.law.check_state_domain(n,c,o,p)?;self.certificate(c,o,p.design)?;Ok(())}
 fn check_derivative_domain(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.law.check_derivative_domain(n,c,o,p)?;if !self.certificate(c,o,p.design)?.narrow_pairs.is_empty(){return Err(fail("closed complete shell separator/feature selection is not an ordinary derivative owner"));}Ok(())}
 fn newton_constraints(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>,d:&[f64],attempt:usize)->CaeResult<Option<Jacobian>>{self.law.newton_constraints(n,c,o,p,d,attempt)}
 fn admitted_trial(&self,n:usize,c:&[f64],d:&[f64],o:&[f64],p:StepParameters<'_>,trial:f64)->CaeResult<f64>{if c.len()!=d.len()||c.iter().chain(d).any(|x|!x.is_finite())||!trial.is_finite()||trial<=0.||trial>1.{return Err(fail("complete shell Newton fraction domain"));}let mut fraction=trial;for _ in 0..24{let f=self.law.admitted_trial(n,c,d,o,p,fraction)?;if !f.is_finite()||f<=0.||f>fraction{return Err(fail("inner contact fraction domain"));}if f<fraction{fraction=f;continue;}let candidate:Vec<_>=c.iter().zip(d).map(|(x,v)|x+fraction*v).collect();if self.certificate(&candidate,o,p.design).is_ok(){return Ok(fraction);}fraction*=0.5;}Err(fail("native force laws and complete shell did not certify same Newton fraction"))}
}

use crate::moving_contact::{contact_set_kinematics::ContactSetKinematics,linear_path::ApproximatePathCertificate};
impl<L:ContactSetKinematics> ContactSetKinematics for CompleteShellContact<L>{
 fn native_states(&self)->usize{self.law.native_states()}
 fn check_selected_geometry_domain(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.law.check_selected_geometry_domain(n,c,o,p)?;if !self.certificate(c,o,p.design)?.narrow_pairs.is_empty(){return Err(fail("complete shell closed manifold has no ordinary geometry-selection derivative"));}Ok(())}
 fn scaled_gap_rows(&self,x:&[f64],d:&[f64])->CaeResult<(Vec<f64>,Jacobian)>{self.law.scaled_gap_rows(x,d)}
 fn scaled_gap_design_rows(&self,x:&[f64],d:&[f64])->CaeResult<Jacobian>{self.law.scaled_gap_design_rows(x,d)}
 fn inverse_force_scales(&self,x:&[f64],p:StepParameters<'_>)->CaeResult<Vec<f64>>{self.law.inverse_force_scales(x,p)}
 fn normal_velocities(&self,x:&[f64],d:&[f64],v:&[f64])->CaeResult<Vec<f64>>{self.law.normal_velocities(x,d,v)}
 fn instantaneous_rows(&self,x:&[f64],d:&[f64])->CaeResult<(Vec<Vec<f64>>,Vec<f64>)>{self.law.instantaneous_rows(x,d)}
 fn residual_path_certificates(&self,o:&[f64],c:&[f64],d:&[f64],t:f64)->CaeResult<Vec<ApproximatePathCertificate>>{self.certificate(c,o,d)?;self.law.residual_path_certificates(o,c,d,t)}
 fn fixed_reference_geometry(&self)->bool{self.law.fixed_reference_geometry()}
 fn bound_owner_features(&self,f:&serde_json::Value)->CaeResult<serde_json::Value>{Ok(serde_json::json!({"inner":self.law.bound_owner_features(f)?,"complete_shell_source":self.surface.identity(),"initial_disjointness":self.initial_identity,"maximum_pairs":self.maximum_pairs,"strict_gap_floor_m":0.}))}
}
