// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_solve::time_stepper::StepParameters;
use crate::moving_contact::{collection::MultipleContact,contact_set_kinematics::ContactFeatureKinematics};
use serde::{Serialize,Deserialize};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn digest(s:&str)->bool{s.len()==64&&s.bytes().all(|x|x.is_ascii_hexdigit())}
pub trait IntrinsicContactFamily:ContactFeatureKinematics{fn physical_family_identity(&self)->&str;}
impl IntrinsicContactFamily for super::primal_vf_family::PrimalVfFamily{fn physical_family_identity(&self)->&str{self.physical_family_identity()}}

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum ContactFamilyBranch{Open,SelectedClosed}
#[derive(Clone,Serialize,Deserialize)]
pub struct DynamicFamilyCheckpoint{source:String,design:String,clock_bits:u64,native:usize,auxiliary:usize,families:Vec<String>,branches:Vec<ContactFamilyBranch>,state_bits:Vec<u64>}
impl DynamicFamilyCheckpoint{
 pub fn identity(&self)->String{implexity_core::json::canonical_sha256(&serde_json::to_value(self).unwrap())}
 pub fn family_ids(&self)->&[String]{&self.families}
 pub fn absolute_clock(&self)->f64{f64::from_bits(self.clock_bits)}
}
pub struct ContactLayoutMap{native:usize,auxiliary:usize,old:Vec<String>,new:Vec<String>}
impl ContactLayoutMap{
 pub fn old_family_ids(&self)->&[String]{&self.old}
 pub fn new_family_ids(&self)->&[String]{&self.new}
 pub fn forward(&self,state:&[f64])->CaeResult<Vec<f64>>{
  if state.len()!=self.native+self.old.len()+self.auxiliary||state.iter().any(|x|!x.is_finite()){return Err(fail("dynamic contact old packed state"));}
  let mut result=Vec::with_capacity(self.native+self.new.len()+self.auxiliary);result.extend_from_slice(&state[..self.native]);for id in &self.new{result.push(self.old.iter().position(|x|x==id).map(|i|state[self.native+i]).unwrap_or(0.));}result.extend_from_slice(&state[self.native+self.old.len()..]);Ok(result)
 }
 pub fn fixed_layout_transpose(&self,bar:&[f64])->CaeResult<Vec<f64>>{
  if bar.len()!=self.native+self.new.len()+self.auxiliary||bar.iter().any(|x|!x.is_finite()){return Err(fail("dynamic contact new packed cotangent"));}
  let mut result=Vec::with_capacity(self.native+self.old.len()+self.auxiliary);result.extend_from_slice(&bar[..self.native]);for id in &self.old{let i=self.new.iter().position(|x|x==id).ok_or_else(||fail("dynamic contact surviving family missing"))?;result.push(bar[self.native+i]);}result.extend_from_slice(&bar[self.native+self.new.len()..]);Ok(result)
 }
 pub fn branch_forward(&self,branches:&[ContactFamilyBranch])->CaeResult<Vec<ContactFamilyBranch>>{if branches.len()!=self.old.len(){return Err(fail("dynamic contact old selected branch width"));}Ok(self.new.iter().map(|id|self.old.iter().position(|x|x==id).map(|k|branches[k]).unwrap_or(ContactFamilyBranch::Open)).collect())}
 pub fn ordinary_event_derivative(&self)->CaeResult<()>{Err(fail("contact layout remap transpose is not localization/impulse/saltation or changing-family history derivative"))}
}
pub struct LocalizedFamilyInsertion{identity:String,absolute_clock:f64,normal_velocities:Vec<f64>,families:Vec<String>}
impl LocalizedFamilyInsertion{
 pub fn identity(&self)->&str{&self.identity}
 pub fn absolute_clock(&self)->f64{self.absolute_clock}
 pub fn normal_velocities(&self)->&[f64]{&self.normal_velocities}
 pub fn family_ids(&self)->&[String]{&self.families}
}
pub struct DynamicContactFamilies<L>{laws:Vec<L>,ids:Vec<String>,native:usize,fluxes:usize,design:usize,auxiliary:usize,source:String}
impl<L:IntrinsicContactFamily> DynamicContactFamilies<L>{
 pub fn new(laws:Vec<L>,native:usize,fluxes:usize,design:usize,auxiliary:usize,source:String)->CaeResult<Self>{
  if native==0||fluxes==0||!digest(&source)||native.checked_add(laws.len()).and_then(|n|n.checked_add(auxiliary)).is_none(){return Err(fail("dynamic contact native dimensions/source"));}
  let ids=laws.iter().map(|l|l.physical_family_identity().to_string()).collect::<Vec<_>>();if ids.iter().any(|x|!digest(x))||ids.iter().collect::<std::collections::BTreeSet<_>>().len()!=ids.len()||laws.iter().any(|l|l.multipliers()!=1){return Err(fail("dynamic contact intrinsic duplicate family/layout"));}Ok(Self{laws,ids,native,fluxes,design,auxiliary,source})
 }
 pub fn from_native_collection(collection:MultipleContact<L>,auxiliary:usize,source:String)->CaeResult<Self>{let(laws,native,fluxes,design)=collection.into_parts();Self::new(laws,native,fluxes,design,auxiliary,source)}
 pub fn family_ids(&self)->&[String]{&self.ids}
 pub fn into_native_collection(self)->CaeResult<Option<MultipleContact<L>>>{if self.laws.is_empty(){Ok(None)}else{Ok(Some(MultipleContact::new(self.laws,self.native,self.fluxes,self.design)?))}}
 pub fn checkpoint(&self,state:&[f64],branches:&[ContactFamilyBranch],design:&[f64],absolute_clock:f64)->CaeResult<DynamicFamilyCheckpoint>{
  if branches.len()!=self.ids.len()||state.len()!=self.native+self.ids.len()+self.auxiliary||design.len()!=self.design||state.iter().chain(design).any(|x|!x.is_finite())||!absolute_clock.is_finite(){return Err(fail("dynamic contact checkpoint complete inputs"));}
  for(k,law)in self.laws.iter().enumerate(){if branches[k]==ContactFamilyBranch::SelectedClosed&&state[self.native+k]<=0.{return Err(fail("dynamic selected-active checkpoint requires positive native multiplier"));}let mut local=state[..self.native].to_vec();local.push(state[self.native+k]);law.check_state_domain(1,&local,&local,StepParameters{design,time_scale:1.})?;}
  Ok(DynamicFamilyCheckpoint{source:self.source.clone(),design:implexity_core::json::canonical_sha256(&serde_json::json!(design)),clock_bits:absolute_clock.to_bits(),native:self.native,auxiliary:self.auxiliary,families:self.ids.clone(),branches:branches.to_vec(),state_bits:state.iter().map(|x|x.to_bits()).collect()})
 }
 pub fn restore(&self,checkpoint:&DynamicFamilyCheckpoint,trusted_digest:&str,design:&[f64],absolute_clock:f64)->CaeResult<(Vec<f64>,Vec<ContactFamilyBranch>)>{
  if !digest(trusted_digest)||checkpoint.identity()!=trusted_digest||checkpoint.source!=self.source||checkpoint.families!=self.ids||checkpoint.native!=self.native||checkpoint.auxiliary!=self.auxiliary||checkpoint.clock_bits!=absolute_clock.to_bits()||checkpoint.design!=implexity_core::json::canonical_sha256(&serde_json::json!(design)){return Err(fail("dynamic contact trusted checkpoint identity mismatch"));}
  let state=checkpoint.state_bits.iter().map(|x|f64::from_bits(*x)).collect::<Vec<_>>();self.checkpoint(&state,&checkpoint.branches,design,absolute_clock)?;Ok((state,checkpoint.branches.clone()))
 }
 pub fn append_exact_localized(mut self,new_laws:Vec<L>,previous:&[f64],event:&[f64],design:&[f64],physical_velocity:&[f64],interval:[f64;2],fraction:f64)->CaeResult<(Self,ContactLayoutMap,LocalizedFamilyInsertion)>{
  let(map,insertion)=self.register_exact_localized(new_laws,previous,event,design,physical_velocity,interval,fraction)?;Ok((self,map,insertion))
 }
 pub fn register_exact_localized(&mut self,new_laws:Vec<L>,previous:&[f64],event:&[f64],design:&[f64],physical_velocity:&[f64],interval:[f64;2],fraction:f64)->CaeResult<(ContactLayoutMap,LocalizedFamilyInsertion)>{
  let width=self.native+self.ids.len()+self.auxiliary;if new_laws.is_empty()||previous.len()!=width||event.len()!=width||physical_velocity.len()!=self.fluxes||design.len()!=self.design||previous.iter().chain(event).chain(design).chain(physical_velocity).chain(&interval).chain(std::iter::once(&fraction)).any(|x|!x.is_finite())||interval[1]<=interval[0]||fraction<=0.||fraction>1.{return Err(fail("dynamic exact localized event inputs"));}
  let dt=interval[1]-interval[0];let absolute_clock=interval[0]+fraction*dt;if !dt.is_finite()||!absolute_clock.is_finite(){return Err(fail("dynamic localized event clock overflow"));}
  let old=self.ids.clone();let mut added=Vec::new();let mut velocities=Vec::new();let mut previous_local=previous[..self.native].to_vec();previous_local.push(0.);let mut event_local=event[..self.native].to_vec();event_local.push(0.);let p=StepParameters{design,time_scale:1.};
  for(k,law)in self.laws.iter().enumerate(){let mut a=previous[..self.native].to_vec();a.push(previous[self.native+k]);let mut c=event[..self.native].to_vec();c.push(event[self.native+k]);law.check_state_domain(1,&c,&a,p)?;}
  for law in &new_laws{let id=law.physical_family_identity().to_string();if !digest(&id)||law.multipliers()!=1||self.ids.contains(&id)||added.contains(&id){return Err(fail("dynamic event duplicate intrinsic physical family"));}law.check_state_domain(1,&event_local,&previous_local,p)?;let(row,gap)=law.instantaneous_row(&event_local,design)?;let(_,old_gap)=law.instantaneous_row(&previous_local,design)?;let velocity:f64=row.iter().zip(physical_velocity).map(|(a,b)|a*b).sum();if row.len()!=self.fluxes||row.iter().any(|x|!x.is_finite())||gap!=0.||!old_gap.is_finite()||old_gap<=0.||!velocity.is_finite()||velocity>=0.{return Err(fail("new family requires native exact-zero closing root from positive gap; approximate/grazing/perimeter roots need qualified event owner"));}added.push(id);velocities.push(velocity);}
  let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"source":self.source,"old_families":old,"new_families":added,"native_previous_bits":previous.iter().map(|x|x.to_bits()).collect::<Vec<_>>(),"native_event_bits":event.iter().map(|x|x.to_bits()).collect::<Vec<_>>(),"design":design,"velocity":physical_velocity,"interval":interval,"fraction_bits":fraction.to_bits(),"clock_bits":absolute_clock.to_bits(),"scope":"native root/layout admission only; physical impulse and complete swept-shell admission still required"}));self.ids.extend(added.clone());self.laws.extend(new_laws);let map=ContactLayoutMap{native:self.native,auxiliary:self.auxiliary,old,new:self.ids.clone()};Ok((map,LocalizedFamilyInsertion{identity,absolute_clock,normal_velocities:velocities,families:added}))
 }
}
