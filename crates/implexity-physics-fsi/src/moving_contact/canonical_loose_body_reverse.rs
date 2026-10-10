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

use crate::moving_contact::{contact_field::ContactField,contact_set_kinematics::ContactSetKinematics,history_branch::{ContactOwnerIdentity,ContactHistoryBranch},macro_control::phase_subdivision::PhaseHistory,separate_body::PairSolidField,selected_transition::OnsetPolicy};
use super::selected_derivative::{SelectedContactLinearization,SelectedContactDirection,contact_set_selected_linearization_on_grid,PhaseGridInterval,require_history_branch_preserved};
use implexity_core::{CaeError,CaeResult};
use implexity_solve::{multirate_coupling::FluxDrivenField,time_stepper::StepParameters};

fn fail(message:&str)->CaeError{CaeError::contract(message)}
fn same(a:&[f64],b:&[f64])->bool{a.len()==b.len()&&a.iter().zip(b).all(|(a,b)|a.to_bits()==b.to_bits())}
fn finite(values:&[f64],width:usize)->CaeResult<()>{if values.len()!=width||values.iter().any(|v|!v.is_finite()){return Err(fail("body phase derivative finite input layout"));}Ok(())}
fn add(target:&mut[f64],source:&[f64])->CaeResult<()>{finite(source,target.len())?;for(a,b)in target.iter_mut().zip(source){*a+=b;}if target.iter().any(|v|!v.is_finite()){return Err(fail("body phase cotangent overflow"));}Ok(())}

pub struct BodyPhaseDirection{pub previous:Vec<f64>,pub force:Vec<f64>,pub design:Vec<f64>,pub time_scales:Vec<f64>}
pub struct BodyPhasePullback{pub previous:Vec<f64>,pub force:Vec<f64>,pub design:Vec<f64>,pub time_scales:Vec<f64>}
pub struct BodyPhaseLinearization{phases:Vec<SelectedContactLinearization>,state_size:usize,force_size:usize,design_size:usize}

pub fn linearize<L:ContactSetKinematics>(field:&ContactField<PairSolidField<'_>,L>,history:&PhaseHistory,design:&[f64],force:&[f64],parent_time_scale:f64,owner:&ContactOwnerIdentity,initial_branch:&ContactHistoryBranch,policy:OnsetPolicy,condition_limit:f64)->CaeResult<BodyPhaseLinearization>{
 let state_size=field.state_size();let force_size=field.trace_operator().nrows();let design_size=field.design_size();let count=field.law().contact_count();let native=field.law().native_states();
 finite(design,design_size)?;finite(force,force_size)?;if history.records.is_empty()||native.checked_add(count)!=Some(state_size)||!condition_limit.is_finite()||condition_limit<=1.{return Err(fail("body phase derivative captured history domain"));}
 owner.check_parameters(design,parent_time_scale,policy)?;
 let target=policy.residual_tolerance*policy.gap_target_fraction;initial_branch.check_binding(owner,count,target)?;owner.check_clock(history.records[0].begin_s)?;
 let selected=initial_branch.selected();let mut phases=Vec::with_capacity(history.records.len());let mut end_branch=None;let mut end_owner=None;let mut duration=0.;
 for(i,record)in history.records.iter().enumerate(){
  finite(&record.previous,state_size)?;finite(&record.result.state,state_size)?;
  if !record.begin_s.is_finite()||!record.end_s.is_finite()||record.end_s<=record.begin_s||!record.time_scale.is_finite()||record.time_scale<=0.||record.duration_s.to_bits()!=(field.nominal_step_s()*record.time_scale).to_bits(){return Err(fail("body phase derivative exact duration/clock"));}
  if i>0{let prior=&history.records[i-1];if prior.end_s.to_bits()!=record.begin_s.to_bits()||!same(&prior.result.state,&record.previous){return Err(fail("body phase derivative complete captured state continuity"));}}
  if selected!=record.result.active{return Err(fail("body phase derivative contact transition needs event derivative"));}
  let phase_owner=owner.for_phase(record.time_scale,record.begin_s)?;phase_owner.check_parameters(design,record.time_scale,policy)?;
  let interval=PhaseGridInterval::new(history.records[0].begin_s,field.nominal_step_s(),parent_time_scale,i,history.records.len(),record.begin_s,record.end_s,record.duration_s,record.time_scale)?;
  let next_owner=interval.end_owner(owner,design,policy)?;next_owner.check_clock(record.end_s)?;
  let branch=ContactHistoryBranch::restore(&record.checkpoint,&record.result.state,&next_owner,count,target)?;
  require_history_branch_preserved(initial_branch,&branch)?;
  let gaps=field.law().scaled_gap_rows(&record.previous,design)?.0;
  if gaps.len()!=count{return Err(fail("body phase derivative previous gap layout"));}
  for k in 0..count{if (selected[k]&&record.previous[native+k]<=0.)||(!selected[k]&&gaps[k]<=0.){return Err(fail("body phase derivative initial tie or grazing"));}}
  phases.push(contact_set_selected_linearization_on_grid(field,&record.previous,force,StepParameters{design,time_scale:record.time_scale},policy,&record.result,owner,&interval,&branch,condition_limit)?);
  duration+=record.duration_s;end_branch=Some(branch);end_owner=Some(next_owner);
 }
 let last=history.records.last().ok_or_else(||fail("body phase derivative missing endpoint"))?;
 if !same(&last.result.state,&history.final_state)||duration.to_bits()!=history.total_duration_s.to_bits()||end_owner.as_ref()!=Some(&history.final_owner){return Err(fail("body phase derivative final state/clock identity"));}
 let final_branch=end_branch.ok_or_else(||fail("body phase derivative missing branch"))?;
 if final_branch.checkpoint(&history.final_state)?!=history.final_branch.checkpoint(&history.final_state)?{return Err(fail("body phase derivative final branch identity"));}
 Ok(BodyPhaseLinearization{phases,state_size,force_size,design_size})
}

impl BodyPhaseLinearization{
 pub fn phases(&self)->usize{self.phases.len()}
 pub fn tangent(&self,direction:&BodyPhaseDirection)->CaeResult<Vec<Vec<f64>>>{
  finite(&direction.previous,self.state_size)?;finite(&direction.force,self.force_size)?;finite(&direction.design,self.design_size)?;finite(&direction.time_scales,self.phases.len())?;
  let mut states=Vec::with_capacity(self.phases.len()+1);states.push(direction.previous.clone());
  for(i,phase)in self.phases.iter().enumerate(){let previous=states.last().ok_or_else(||fail("body phase tangent previous missing"))?.clone();let next=phase.tangent(&SelectedContactDirection{previous,flux:direction.force.clone(),design:direction.design.clone(),time_scale:direction.time_scales[i]})?;finite(&next,self.state_size)?;states.push(next);}
  Ok(states)
 }
 pub fn adjoint(&self,state_bars:&[Vec<f64>])->CaeResult<BodyPhasePullback>{
  if state_bars.len()!=self.phases.len()+1{return Err(fail("body phase cotangent complete trajectory layout"));}
  for bar in state_bars{finite(bar,self.state_size)?;}
  let mut previous=state_bars.last().ok_or_else(||fail("body phase cotangent endpoint missing"))?.clone();let mut force=vec![0.;self.force_size];let mut design=vec![0.;self.design_size];let mut time_scales=vec![0.;self.phases.len()];
  for i in(0..self.phases.len()).rev(){let bars=self.phases[i].adjoint(&previous)?;finite(&bars.previous,self.state_size)?;add(&mut force,&bars.flux)?;add(&mut design,&bars.design)?;if !bars.time_scale.is_finite(){return Err(fail("body phase time cotangent finite"));}time_scales[i]=bars.time_scale;previous=bars.previous;add(&mut previous,&state_bars[i])?;}
  Ok(BodyPhasePullback{previous,force,design,time_scales})
 }
}
