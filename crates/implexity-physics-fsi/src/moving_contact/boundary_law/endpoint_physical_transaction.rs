// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_solve::multirate_coupling::FluxDrivenField;
use crate::{model::FsiModel,moving_contact::{collection::MultipleContact,contact_field::ContactField,separate_body::PairSolidField,impact::{ImpactTolerance,ActiveSetImpactPolicy}}};
use super::{endpoint_manifold_root::EndpointManifoldRoot,reference_embedding::AuthenticatedReferencePatch,primal_vf_family::PrimalVfFamily,plane_manifold::CompleteSurfaceTrace,native_family_impulse::{self,NativeFamilyImpulse}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn digest(s:&str)->bool{s.len()==64&&s.bytes().all(|x|x.is_ascii_hexdigit())}
fn bits(x:&[f64])->String{implexity_core::json::canonical_sha256(&serde_json::json!(x.iter().map(|v|v.to_bits()).collect::<Vec<_>>()))}
pub struct EndpointPhysicalSelection{root_digest:String,surface:String,previous:String,event:String,design:String,clock:u64,force_families:Vec<String>,participating:Vec<usize>,identity:String}
impl EndpointPhysicalSelection{
 pub fn identity(&self)->&str{&self.identity}
 pub fn participating(&self)->&[usize]{&self.participating}
 pub fn restore_trusted(root:&EndpointManifoldRoot,trusted_digest:&str,surface:&CompleteSurfaceTrace,patches:[&AuthenticatedReferencePatch;2],field:&ContactField<PairSolidField<'_>,MultipleContact<PrimalVfFamily>>,previous:&[f64],event:&[f64],design:&[f64],clock:f64)->CaeResult<Self>{
  surface.require_authenticated_patches(patches)?;
  let value=serde_json::to_value(root).map_err(|_|fail("endpoint root serialization"))?;
  if !digest(trusted_digest)||implexity_core::json::canonical_sha256(&value)!=trusted_digest||previous.len()!=field.state_size()||event.len()!=field.state_size()||design.len()!=field.design_size()||previous.iter().chain(event).chain(design).any(|x|!x.is_finite())||!clock.is_finite(){return Err(fail("trusted endpoint physical transaction inputs"));}
  let mut unbound=value.clone();unbound["identity"]=serde_json::Value::String(String::new());
  let report=&root.primitive_coverage;
  if report.root.path_owner!=super::shell_root::ShellPathOwner::PhysicalLinearSegment{return Err(fail("numerical predictor root is not accepted physical phase or impulse time"));}
  if root.identity!=implexity_core::json::canonical_sha256(&unbound)||root.fraction.to_bits()!=1f64.to_bits()||root.absolute_clock.to_bits()!=clock.to_bits()||report.root.physical_interval[1].to_bits()!=clock.to_bits()||report.surface_identity!=surface.identity()||report.previous_state_identity!=bits(previous)||report.current_state_identity!=bits(event)||report.root.design_identity!=bits(design)||report.multipliers!=field.law().contact_count()||root.simultaneous.is_empty(){return Err(fail("endpoint proof state/design/clock/surface binding"));}
  let positions=surface.positions(event,report.multipliers,design)?;
  let force_families=field.law().laws().iter().map(|l|l.physical_family_identity().to_string()).collect::<Vec<_>>();
  if force_families.iter().collect::<std::collections::BTreeSet<_>>().len()!=force_families.len(){return Err(fail("duplicate physical force family"));}
  for family in &root.simultaneous{for b in 0..2{if positions[b].get(family.vertices[b])!=Some(&family.position_m){return Err(fail("endpoint actual tissue position differs"));}}}
  let participating=resolve_physical_families(root,patches,field.law().laws())?;
  let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"trusted_root":trusted_digest,"surface":surface.identity(),"previous":bits(previous),"event":bits(event),"design":bits(design),"clock_bits":clock.to_bits(),"all_physical_families":force_families,"participating":participating}));
  Ok(Self{root_digest:trusted_digest.into(),surface:surface.identity().into(),previous:bits(previous),event:bits(event),design:bits(design),clock:clock.to_bits(),force_families,participating,identity})
 }
 pub fn apply<'a>(&self,models:[&FsiModel;2],surface:&CompleteSurfaceTrace,field:&ContactField<PairSolidField<'_>,MultipleContact<PrimalVfFamily>>,previous:&[f64],event:&[f64],fluid:&'a[f64],trace_history:&'a[f64],design:&[f64],time_scale:f64,clock:f64,restitution:&[f64],tolerance:ImpactTolerance,policy:ActiveSetImpactPolicy,source_graph:&str)->CaeResult<NativeFamilyImpulse<'a>>{
  if !digest(&self.root_digest)||self.surface!=surface.identity()||self.previous!=bits(previous)||self.event!=bits(event)||self.design!=bits(design)||self.clock!=clock.to_bits()||self.force_families!=field.law().laws().iter().map(|l|l.physical_family_identity().to_string()).collect::<Vec<_>>(){return Err(fail("endpoint physical impulse stale state or family layout"));}
  native_family_impulse::apply_subset(models,field,event,fluid,trace_history,design,time_scale,clock,restitution,tolerance,policy,source_graph,&self.participating)
 }
 pub fn ordinary_event_derivative(&self)->CaeResult<()>{Err(fail("endpoint selection and impulse are not localized-root saltation or ordinary changing-contact gradient"))}
}

pub fn resolve_physical_families(root:&EndpointManifoldRoot,patches:[&AuthenticatedReferencePatch;2],laws:&[PrimalVfFamily])->CaeResult<Vec<usize>>{
 let force_families=laws.iter().map(|l|l.physical_family_identity()).collect::<Vec<_>>();
 if root.simultaneous.is_empty()||force_families.iter().collect::<std::collections::BTreeSet<_>>().len()!=force_families.len(){return Err(fail("empty or duplicate physical force families"));}
 let mut participating=vec![];let mut pairs=std::collections::BTreeSet::new();
 for family in &root.simultaneous{
  let pair=family.vertices;if !pairs.insert(pair)||!digest(&family.cone_identity)||family.native_star_rows==0||family.incident_facets.iter().any(Vec::is_empty){return Err(fail("endpoint duplicate/unqualified manifold family"));}
  let expected=implexity_core::json::canonical_sha256(&serde_json::json!({"source_patch":patches[0].identity(),"target_patch":patches[1].identity(),"source_vertex":pair[0],"target_vertex":pair[1],"kind":"authenticated native incident VF physical family"}));
  participating.push(force_families.iter().position(|id|*id==expected).ok_or_else(||fail("localized physical family not registered; primitive aliases cannot create force rows"))?);
 }
 Ok(participating)
}

#[derive(serde::Serialize,serde::Deserialize)]
pub struct PhysicalEventCheckpoint{selection:String,impulse:String,source:String,design:String,time_scale:u64,clock:u64,solid:Vec<u64>,fluid:Vec<u64>,trace_history:Vec<u64>,branches:Vec<super::dynamic_family::ContactFamilyBranch>,cumulative_work:u64,cumulative_absolute_work:u64,prior_avf_certificate_invalidated:bool}
impl PhysicalEventCheckpoint{
 pub fn identity(&self)->String{implexity_core::json::canonical_sha256(&serde_json::json!(self))}
 pub fn clock(&self)->f64{f64::from_bits(self.clock)}
 pub fn solid(&self)->Vec<f64>{self.solid.iter().map(|v|f64::from_bits(*v)).collect()}
 pub fn restore_trusted(&self,trusted_digest:&str,selection:&EndpointPhysicalSelection,field:&ContactField<PairSolidField<'_>,MultipleContact<PrimalVfFamily>>,design:&[f64],time_scale:f64,clock:f64,source_graph:&str)->CaeResult<(Vec<f64>,Vec<f64>,Vec<f64>,Vec<super::dynamic_family::ContactFamilyBranch>,f64,f64)>{
  if !digest(trusted_digest)||self.identity()!=trusted_digest||self.selection!=selection.identity()||self.source!=source_graph||!digest(source_graph)||self.design!=bits(design)||self.time_scale!=time_scale.to_bits()||!time_scale.is_finite()||time_scale<=0.||self.clock!=clock.to_bits()||!self.prior_avf_certificate_invalidated||!digest(&self.impulse)||self.branches.len()!=field.law().contact_count(){return Err(fail("trusted physical event checkpoint identity"));}
  let solid=self.solid();let fluid=self.fluid.iter().map(|v|f64::from_bits(*v)).collect::<Vec<_>>();let trace=self.trace_history.iter().map(|v|f64::from_bits(*v)).collect::<Vec<_>>();let work=f64::from_bits(self.cumulative_work);let absolute=f64::from_bits(self.cumulative_absolute_work);
  if solid.len()!=field.state_size()||solid.iter().chain(&fluid).chain(&trace).any(|v|!v.is_finite())||fluid.iter().any(|v|*v<0.)||!work.is_finite()||!absolute.is_finite()||absolute<0.{return Err(fail("physical event checkpoint state domain"));}
  for(k,branch)in self.branches.iter().enumerate(){if *branch==super::dynamic_family::ContactFamilyBranch::SelectedClosed&&solid[field.native().state_size()+k]<=0.{return Err(fail("physical event selected maintained multiplier"));}}
  if selection.force_families!=field.law().laws().iter().map(|l|l.physical_family_identity().to_string()).collect::<Vec<_>>(){return Err(fail("physical event restored family layout"));}
  field.check_state_domain(1,&solid,&solid,implexity_solve::time_stepper::StepParameters{design,time_scale})?;
  Ok((solid,fluid,trace,self.branches.clone(),work,absolute))
 }
}
impl EndpointPhysicalSelection{
 pub fn apply_checkpointed<'a>(&self,models:[&FsiModel;2],surface:&CompleteSurfaceTrace,field:&ContactField<PairSolidField<'_>,MultipleContact<PrimalVfFamily>>,previous:&[f64],event:&[f64],fluid:&'a[f64],trace_history:&'a[f64],design:&[f64],time_scale:f64,clock:f64,restitution:&[f64],tolerance:ImpactTolerance,policy:ActiveSetImpactPolicy,source_graph:&str,branches:&[super::dynamic_family::ContactFamilyBranch],cumulative_work:f64,cumulative_absolute_work:f64)->CaeResult<(NativeFamilyImpulse<'a>,PhysicalEventCheckpoint)>{
  if branches.len()!=field.law().contact_count()||!cumulative_work.is_finite()||!cumulative_absolute_work.is_finite()||cumulative_absolute_work<0.{return Err(fail("physical event auxiliary state"));}
  for(k,branch)in branches.iter().enumerate(){if *branch==super::dynamic_family::ContactFamilyBranch::SelectedClosed&&event.get(field.native().state_size()+k).map_or(true,|v|*v<=0.){return Err(fail("physical event incoming maintained branch"));}}
  let result=self.apply(models,surface,field,previous,event,fluid,trace_history,design,time_scale,clock,restitution,tolerance,policy,source_graph)?;
  let checkpoint=PhysicalEventCheckpoint{selection:self.identity.clone(),impulse:result.identity.clone(),source:source_graph.into(),design:bits(design),time_scale:time_scale.to_bits(),clock:clock.to_bits(),solid:result.solid.iter().map(|v|v.to_bits()).collect(),fluid:fluid.iter().map(|v|v.to_bits()).collect(),trace_history:trace_history.iter().map(|v|v.to_bits()).collect(),branches:branches.to_vec(),cumulative_work:cumulative_work.to_bits(),cumulative_absolute_work:cumulative_absolute_work.to_bits(),prior_avf_certificate_invalidated:true};
  Ok((result,checkpoint))
 }
}
