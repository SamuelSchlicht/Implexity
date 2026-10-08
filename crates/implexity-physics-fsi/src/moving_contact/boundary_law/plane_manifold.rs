// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{matrix::Jacobian,time_stepper::StepParameters};
use crate::moving_contact::{surface_map::SurfaceFeature,contact_field::{NativeContactLaw,ContactContribution}};
use super::{boundary_mapped_pair_law::NodeBinding,boundary_path::{self,PathPolicy,PathStatus},surface_admission::DensitySurfaceMap,reference_embedding::AuthenticatedReferencePatch,exposed_surface::ExposedSurface,swept_coverage::{exhaustive_swept_candidates,SweptCandidate}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
#[derive(Clone)]
enum GeometryOwner{FixedReference{references:Vec<[f64;3]>,identity:String},DensitySurface{surface:ExposedSurface}}
#[derive(Clone)]
struct BodySurface{features:Vec<SurfaceFeature>,facets:Vec<[usize;3]>,owner:GeometryOwner}
#[derive(Clone)]
pub struct CompleteSurfaceTrace{nodes:[Vec<NodeBinding>;2],surfaces:[BodySurface;2],phase:CsrMatrix,native_states:usize,identity:String}
impl CompleteSurfaceTrace{
 pub fn authenticated(nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,native_states:usize,patches:[&AuthenticatedReferencePatch;2])->CaeResult<Self>{
  for b in 0..2{if patches[b].patch().body()!=b||!patches[b].patch().boundary_edges().is_empty()||nodes[b].len()!=patches[b].nodes().len()||nodes[b].iter().zip(patches[b].nodes()).any(|(n,p)|n.reference_m.map(f64::to_bits)!=p.map(f64::to_bits)){return Err(fail("complete authenticated native surface binding"));}}
  let surfaces:[CaeResult<BodySurface>;2]=[0,1].map(|b|{let p=patches[b];Ok(BodySurface{features:p.patch().features().to_vec(),facets:p.patch().facets().to_vec(),owner:GeometryOwner::FixedReference{references:(0..p.patch().features().len()).map(|i|p.reference(i)).collect::<CaeResult<_>>()?,identity:p.identity()}})});let [a,b]=surfaces;Self::new(nodes,[a?,b?],phase,native_states)
 }
 pub fn density(nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,native_states:usize,surfaces:[&DensitySurfaceMap;2])->CaeResult<Self>{
  for b in 0..2{if surfaces[b].body()!=b||nodes[b].len()!=surfaces[b].reference_nodes().len()||nodes[b].iter().zip(surfaces[b].reference_nodes()).any(|(n,p)|n.reference_m.map(f64::to_bits)!=p.map(f64::to_bits)){return Err(fail("complete density native surface binding"));}surfaces[b].surface().require_closed_surface()?;}
  let owners=[0,1].map(|b|BodySurface{features:surfaces[b].surface().features().to_vec(),facets:surfaces[b].surface().facets().to_vec(),owner:GeometryOwner::DensitySurface{surface:surfaces[b].surface().clone()}});Self::new(nodes,owners,phase,native_states)
 }
 fn new(nodes:[Vec<NodeBinding>;2],surfaces:[BodySurface;2],phase:CsrMatrix,native_states:usize)->CaeResult<Self>{
  if native_states==0||phase.nrows()!=nodes[0].len()+nodes[1].len(){return Err(fail("complete surface state/design layout"));}let mut state_indices=std::collections::BTreeSet::new();for n in nodes.iter().flatten(){if n.reference_m.iter().any(|v|!v.is_finite())||n.inverse_state_scale.iter().any(|v|!v.is_finite()||*v<=0.){return Err(fail("complete surface node finite scale"));}for i in n.state{if i>=native_states||!state_indices.insert(i){return Err(fail("complete surface native state indices"));}}}for r in 0..phase.nrows(){if phase.row(r).1.iter().any(|v|!v.is_finite()){return Err(fail("complete surface phase map finite"));}}
  let sources:[String;2]=std::array::from_fn(|b|match &surfaces[b].owner{GeometryOwner::FixedReference{identity,..}=>identity.clone(),GeometryOwner::DensitySurface{surface}=>implexity_core::json::canonical_sha256(&serde_json::json!({"mesh":surface.mesh_identity(),"stratum":surface.stratum_identity()}))});let bindings:Vec<_>=nodes.iter().map(|ns|ns.iter().map(|n|serde_json::json!({"reference":n.reference_m,"state":n.state,"scale":n.inverse_state_scale})).collect::<Vec<_>>()).collect();let phase_rows:Vec<_>=(0..phase.nrows()).map(|i|{let(j,v)=phase.row(i);serde_json::json!([j,v])}).collect();let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"sources":sources,"bindings":bindings,"phase_rows":phase_rows,"design_size":phase.ncols(),"native_states":native_states}));Ok(Self{nodes,surfaces,phase,native_states,identity})
 }
 pub fn require_authenticated_patches(&self,patches:[&AuthenticatedReferencePatch;2])->CaeResult<()>{for b in 0..2{match &self.surfaces[b].owner{GeometryOwner::FixedReference{identity,..} if *identity==patches[b].identity()&&patches[b].patch().body()==b=>(),_=>return Err(fail("complete surface authenticated patch owner mismatch"))}}Ok(())}
 pub fn identity(&self)->&str{&self.identity}
 pub fn positions(&self,state:&[f64],multipliers:usize,design:&[f64])->CaeResult<[Vec<[f64;3]>;2]>{
  if self.native_states.checked_add(multipliers)!=Some(state.len())||state.iter().chain(design).any(|v|!v.is_finite())||design.len()!=self.phase.ncols(){return Err(fail("complete surface full state/design shape"));}let mut density=Vec::with_capacity(self.phase.nrows());for r in 0..self.phase.nrows(){let(j,v)=self.phase.row(r);let value=j.iter().zip(v).map(|(i,a)|design[*i]*a).sum::<f64>();if !value.is_finite()||!(0. ..=1.).contains(&value){return Err(fail("complete surface density domain"));}density.push(value);}
  let mut out=[Vec::new(),Vec::new()];for b in 0..2{let offset=if b==0{0}else{self.nodes[0].len()};let rho=&density[offset..offset+self.nodes[b].len()];let displacement:Vec<[f64;3]>=self.nodes[b].iter().map(|n|std::array::from_fn(|k|state[n.state[k]]*n.inverse_state_scale[k])).collect();out[b]=match &self.surfaces[b].owner{GeometryOwner::FixedReference{references,..}=>self.surfaces[b].features.iter().zip(references).map(|(f,q)|super::reference_embedding::position(&f.map(rho)?,*q,&displacement)).collect::<CaeResult<_>>()?,GeometryOwner::DensitySurface{surface}=>{let absolute:Vec<_>=self.nodes[b].iter().zip(displacement).map(|(n,u)|std::array::from_fn(|k|n.reference_m[k]+u[k])).collect();surface.positions(&absolute,rho)?}};}Ok(out)
 }
 pub fn facets(&self)->[&[[usize;3]];2]{[&self.surfaces[0].facets,&self.surfaces[1].facets]}
}
#[derive(Clone,Copy,Debug)]
pub struct SupportingPlaneSelection{pub body:usize,pub facet:usize,pub positive_body:usize,pub maximum_pairs:usize,pub path_policy:PathPolicy}
pub struct PlaneManifoldCertificate{identity:String,examined_pairs:usize,unresolved:Vec<SweptCandidate>,certified_vertices:[usize;2]}
impl PlaneManifoldCertificate{
 pub fn identity(&self)->&str{&self.identity}
 pub fn examined_pairs(&self)->usize{self.examined_pairs}
 pub fn unresolved_candidates(&self)->&[SweptCandidate]{&self.unresolved}
 pub fn certified_vertices(&self)->[usize;2]{self.certified_vertices}
}
pub fn certify_supporting_plane(old:[&[[f64;3]];2],new:[&[[f64;3]];2],facets:[&[[usize;3]];2],selection:SupportingPlaneSelection,source_identity:&str)->CaeResult<PlaneManifoldCertificate>{
 if selection.body>1||selection.positive_body>1||source_identity.len()!=64||!source_identity.bytes().all(|b|b.is_ascii_hexdigit())||selection.path_policy.minimum_signed_gap!=0.||selection.path_policy.minimum_barycentric!=0.{return Err(fail("strict supporting plane source/policy"));}let report=exhaustive_swept_candidates(old,new,facets,selection.maximum_pairs)?;let ids=*facets[selection.body].get(selection.facet).ok_or_else(||fail("supporting plane actual surface facet"))?;let a=ids.map(|i|old[selection.body][i]);let b=ids.map(|i|new[selection.body][i]);let area=boundary_path::check_linear_path([a[0],a[0],a[1],a[2]],[b[0],b[0],b[1],b[2]],selection.path_policy).map_err(fail)?;if area.status!=PathStatus::Admitted{return Err(fail("supporting plane whole-path area not certified"));}
 for body in 0..2{for (p,q)in old[body].iter().zip(new[body]){if !boundary_path::certify_supporting_star_path(a,b,a[0],b[0],*p,*q,body==selection.positive_body).map_err(fail)?{return Err(fail("complete surface leaves supporting-plane normal cone"));}}}
 let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"source":source_identity,"endpoints":report.endpoint_identity,"plane_body":selection.body,"plane_facet":selection.facet,"positive_body":selection.positive_body,"policy":{"maximum_pairs":selection.maximum_pairs,"minimum_area_ratio":selection.path_policy.minimum_area_ratio,"minimum_signed_gap":selection.path_policy.minimum_signed_gap,"minimum_barycentric":selection.path_policy.minimum_barycentric,"time_resolution":selection.path_policy.time_resolution,"maximum_intervals":selection.path_policy.maximum_intervals,"maximum_depth":selection.path_policy.maximum_depth},"examined_pairs":report.examined_pairs,"covered_unresolved_facets":report.unresolved.iter().map(|q|q.facets).collect::<Vec<_>>()}));Ok(PlaneManifoldCertificate{identity,examined_pairs:report.examined_pairs,unresolved:report.unresolved,certified_vertices:[old[0].len(),old[1].len()]})
}
pub struct PlaneCertifiedContact<L>{law:L,surface:CompleteSurfaceTrace,selection:SupportingPlaneSelection}
impl<L:NativeContactLaw> PlaneCertifiedContact<L>{
 pub fn new(law:L,surface:CompleteSurfaceTrace,selection:SupportingPlaneSelection)->CaeResult<Self>{if selection.body>1||selection.positive_body>1||selection.maximum_pairs==0||selection.path_policy.minimum_signed_gap!=0.||selection.path_policy.minimum_barycentric!=0.{return Err(fail("supporting-plane contact policy"));}Ok(Self{law,surface,selection})}
 pub fn law(&self)->&L{&self.law}
 pub fn kinematics_owner(&self)->serde_json::Value{serde_json::json!({"surface_identity":self.surface.identity(),"plane_body":self.selection.body,"plane_facet":self.selection.facet,"positive_body":self.selection.positive_body,"maximum_pairs":self.selection.maximum_pairs,"path_policy":format!("{:?}",self.selection.path_policy)})}
 pub fn certificate(&self,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<PlaneManifoldCertificate>{let count=self.law.multipliers();let old=self.surface.positions(previous,count,design)?;let new=self.surface.positions(current,count,design)?;certify_supporting_plane([&old[0],&old[1]],[&new[0],&new[1]],self.surface.facets(),self.selection,self.surface.identity())}
}
impl<L:NativeContactLaw> NativeContactLaw for PlaneCertifiedContact<L>{
 fn multipliers(&self)->usize{self.law.multipliers()}
 fn evaluate(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<ContactContribution>{self.law.evaluate(n,c,o,p)}
 fn check_state_domain(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.law.check_state_domain(n,c,o,p)?;self.certificate(c,o,p.design)?;Ok(())}
 fn check_derivative_domain(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.law.check_derivative_domain(n,c,o,p)?;if !self.certificate(c,o,p.design)?.unresolved_candidates().is_empty(){return Err(fail("closed supporting-plane manifold has no ordinary feature-selection derivative"));}Ok(())}
 fn newton_constraints(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>,d:&[f64],attempt:usize)->CaeResult<Option<Jacobian>>{self.law.newton_constraints(n,c,o,p,d,attempt)}
 fn admitted_trial(&self,n:usize,c:&[f64],d:&[f64],o:&[f64],p:StepParameters<'_>,trial:f64)->CaeResult<f64>{if c.len()!=d.len()||d.iter().any(|x|!x.is_finite())||!trial.is_finite()||trial<=0.||trial>1.{return Err(fail("complete surface trial shape/fraction"));}let mut fraction=trial;for _ in 0..24{let admitted=self.law.admitted_trial(n,c,d,o,p,fraction)?;if !admitted.is_finite()||admitted<=0.||admitted>fraction{return Err(fail("native contact fraction domain"));}if admitted<fraction{fraction=admitted;continue;}let candidate:Vec<_>=c.iter().zip(d).map(|(x,v)|x+fraction*v).collect();if self.certificate(&candidate,o,p.design).is_ok(){return Ok(fraction);}fraction*=0.5;}Err(fail("all contact laws and complete surface plane did not admit shared fraction"))}
}
