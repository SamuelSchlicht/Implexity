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

use crate::moving_contact::contact_set_kinematics::ContactSetKinematics;
use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{multirate_coupling::FluxDrivenField,time_stepper::StepParameters};
use crate::moving_contact::{contact_field::ContactField,mapped_pair_law::MappedPairLaw};
use crate::moving_contact::{collection::MultipleContact,history_branch::{ContactOwnerIdentity,ContactHistoryBranch}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct PhaseRecord{pub previous:Vec<f64>,pub result:OnsetResult,pub begin_s:f64,pub end_s:f64,pub duration_s:f64,pub time_scale:f64,pub checkpoint:serde_json::Value}
pub struct PhaseHistory{pub records:Vec<PhaseRecord>,pub final_state:Vec<f64>,pub final_branch:ContactHistoryBranch,pub final_owner:ContactOwnerIdentity,pub total_duration_s:f64,pub velocity_certificate:crate::moving_contact::macro_control::avf_admission::AvfVelocityCertificate}
pub fn advance_constant_flux_phases<L:ContactSetKinematics>(field:&ContactField<crate::moving_contact::separate_body::PairSolidField<'_>,L>,previous:&[f64],flux:&[f64],p:StepParameters<'_>,velocity_trace:&CsrMatrix,policy:OnsetPolicy,branch:&ContactHistoryBranch,owner:&ContactOwnerIdentity,begin_s:f64,subdivisions:usize,observer:&mut dyn FnMut(&PhaseRecord)->CaeResult<()>,previous_certificate:Option<&crate::moving_contact::macro_control::avf_admission::AvfVelocityCertificate>)->CaeResult<PhaseHistory>{
 if !subdivisions.is_power_of_two()||subdivisions>16||!begin_s.is_finite()||!p.time_scale.is_finite()||p.time_scale<=0.{return Err(fail("contact phase finite dyadic subdivision"));}
 owner.check_clock(begin_s)?;owner.check_parameters(p.design,p.time_scale,policy)?;let target=policy.residual_tolerance*policy.gap_target_fraction;branch.check_binding(owner,field.law().contact_count(),target)?;
 let scale=p.time_scale/subdivisions as f64;let duration=field.nominal_step_s()*scale;let total=field.nominal_step_s()*p.time_scale;if !duration.is_finite()||duration<=0.||(duration*subdivisions as f64).to_bits()!=total.to_bits(){return Err(fail("contact phase total duration identity"));}
 let mut last_certificate=previous_certificate.cloned();let mut state=previous.to_vec();let mut selected=branch.selected();let mut records=Vec::with_capacity(subdivisions);let mut final_owner=owner.clone();let mut final_branch=branch.clone();
 for k in 0..subdivisions{let begin=begin_s+k as f64*duration;let end=begin_s+(k+1)as f64*duration;if !end.is_finite()||end<=begin{return Err(fail("contact phase absolute clock precision"));}let phase_owner=owner.for_phase(scale,begin)?;let phase_branch=ContactHistoryBranch::from_selected(&phase_owner,&selected,target)?;let q=StepParameters{design:p.design,time_scale:scale};let(result,_,certificate)=solve_contact_history_transition(field,&state,flux,q,velocity_trace,policy,&phase_branch,&phase_owner,last_certificate.as_ref())?;
  field.check_state_domain(1,&result.state,&state,q)?;selected=result.active.clone();final_owner=owner.for_phase(scale,end)?;final_branch=ContactHistoryBranch::from_selected(&final_owner,&selected,target)?;last_certificate=Some(certificate.bind_phase_endpoint(final_branch.clone(),&result.state)?);let checkpoint=final_branch.checkpoint(&result.state)?;let previous=state;state=result.state.clone();let record=PhaseRecord{previous,result,begin_s:begin,end_s:end,duration_s:duration,time_scale:scale,checkpoint};observer(&record)?;records.push(record);
 }
 Ok(PhaseHistory{records,final_state:state,final_branch,final_owner,total_duration_s:total,velocity_certificate:last_certificate.ok_or_else(||fail("AVF phase certificate missing"))?})
}

use super::selected_transition::{OnsetPolicy,OnsetResult,solve_contact_history_transition};
