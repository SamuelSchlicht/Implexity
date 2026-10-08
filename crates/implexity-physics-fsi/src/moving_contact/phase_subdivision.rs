// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{multirate_coupling::FluxDrivenField,time_stepper::StepParameters};
use crate::moving_contact::{contact_field::ContactField,mapped_pair_law::MappedPairLaw};
use crate::moving_contact::{collection::MultipleContact,selected_transition::{OnsetPolicy,OnsetResult,solve_contact_history_transition},history_branch::{ContactOwnerIdentity,ContactHistoryBranch}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct PhaseRecord{pub previous:Vec<f64>,pub result:OnsetResult,pub begin_s:f64,pub end_s:f64,pub duration_s:f64,pub time_scale:f64,pub checkpoint:serde_json::Value}
pub struct PhaseHistory{pub records:Vec<PhaseRecord>,pub final_state:Vec<f64>,pub final_branch:ContactHistoryBranch,pub final_owner:ContactOwnerIdentity,pub total_duration_s:f64}
pub fn advance_constant_flux_phases<F:FluxDrivenField>(field:&ContactField<F,MultipleContact<MappedPairLaw>>,previous:&[f64],flux:&[f64],p:StepParameters<'_>,velocity_trace:&CsrMatrix,policy:OnsetPolicy,branch:&ContactHistoryBranch,owner:&ContactOwnerIdentity,begin_s:f64,subdivisions:usize,observer:&mut dyn FnMut(&PhaseRecord)->CaeResult<()>)->CaeResult<PhaseHistory>{
 if !subdivisions.is_power_of_two()||subdivisions>16||!begin_s.is_finite()||!p.time_scale.is_finite()||p.time_scale<=0.{return Err(fail("contact phase finite dyadic subdivision"));}
 owner.check_clock(begin_s)?;owner.check_parameters(p.design,p.time_scale,policy)?;let target=policy.residual_tolerance*policy.gap_target_fraction;branch.check_binding(owner,field.law().contact_count(),target)?;
 let scale=p.time_scale/subdivisions as f64;let duration=field.nominal_step_s()*scale;let total=field.nominal_step_s()*p.time_scale;if !duration.is_finite()||duration<=0.||(duration*subdivisions as f64).to_bits()!=total.to_bits(){return Err(fail("contact phase total duration identity"));}
 let mut state=previous.to_vec();let mut selected=branch.selected();let mut records=Vec::with_capacity(subdivisions);let mut final_owner=owner.clone();let mut final_branch=branch.clone();
 for k in 0..subdivisions{let begin=begin_s+k as f64*duration;let end=begin_s+(k+1)as f64*duration;if !end.is_finite()||end<=begin{return Err(fail("contact phase absolute clock precision"));}let phase_owner=owner.for_phase(scale,begin)?;let phase_branch=ContactHistoryBranch::from_selected(&phase_owner,&selected,target)?;let q=StepParameters{design:p.design,time_scale:scale};let(result,_)=solve_contact_history_transition(field,&state,flux,q,velocity_trace,policy,&phase_branch,&phase_owner)?;
  field.check_state_domain(1,&result.state,&state,q)?;selected=result.active.clone();final_owner=owner.for_phase(scale,end)?;final_branch=ContactHistoryBranch::from_selected(&final_owner,&selected,target)?;let checkpoint=final_branch.checkpoint(&result.state)?;let previous=state;state=result.state.clone();let record=PhaseRecord{previous,result,begin_s:begin,end_s:end,duration_s:duration,time_scale:scale,checkpoint};observer(&record)?;records.push(record);
 }
 Ok(PhaseHistory{records,final_state:state,final_branch,final_owner,total_duration_s:total})
}
