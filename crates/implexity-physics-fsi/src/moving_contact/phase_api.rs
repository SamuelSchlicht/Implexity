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
use serde_json::{Value,json};
use super::{resources::PersistentContactFields,collection::MultipleContact,mapped_pair_law::MappedPairLaw,coupled_tick_primal::MultiContactNativeState,history_branch::ContactOwnerIdentity,selected_transition::OnsetPolicy,segment_quadrature::SegmentClock,phase_scheduler::{self,PhaseTick},phase_derivative::{self,FixedPhaseLinearization}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
#[derive(Clone)]
pub struct PhaseCheckpointIdentity{pub source_graph_sha256:String,pub model_sha256:String,pub design_sha256:String,pub contact_maps_sha256:String,pub body_layout_sha256:String,pub fluid_configuration_sha256:String,pub transition_policy_sha256:String,pub nominal_dt_bits:u64}
impl PhaseCheckpointIdentity{
 pub fn value(&self)->CaeResult<Value>{let hashes=[&self.source_graph_sha256,&self.model_sha256,&self.design_sha256,&self.contact_maps_sha256,&self.body_layout_sha256,&self.fluid_configuration_sha256,&self.transition_policy_sha256];if hashes.iter().any(|s|s.len()!=64||!s.bytes().all(|c|c.is_ascii_hexdigit()))||!f64::from_bits(self.nominal_dt_bits).is_finite()||f64::from_bits(self.nominal_dt_bits)<=0.{return Err(fail("phase checkpoint exact source/model/design/body/fluid policy identity"));}Ok(json!({"source_graph_sha256":self.source_graph_sha256,"model_sha256":self.model_sha256,"design_sha256":self.design_sha256,"contact_maps_sha256":self.contact_maps_sha256,"body_layout_sha256":self.body_layout_sha256,"fluid_configuration_sha256":self.fluid_configuration_sha256,"transition_policy_sha256":self.transition_policy_sha256,"nominal_dt_bits":format!("{:016x}",self.nominal_dt_bits)}))}
}
#[derive(Clone,Copy)]
pub struct PhaseSchedule{subdivisions:usize,clock:SegmentClock}
impl PhaseSchedule{
 pub fn new(subdivisions:usize,clock:SegmentClock)->CaeResult<Self>{if !subdivisions.is_power_of_two()||subdivisions>16{return Err(fail("phase schedule dyadic subdivision"));}Ok(Self{subdivisions,clock})}
 pub fn subdivisions(&self)->usize{self.subdivisions}
 pub fn clock(&self)->SegmentClock{self.clock}
 pub fn advance(&self,fields:&PersistentContactFields<'_,MultipleContact<MappedPairLaw>>,previous:&MultiContactNativeState,design:&[f64],external:&[f64],sample_tick:usize,owner:&ContactOwnerIdentity,velocity_trace:&implexity_linalg::sparse::CsrMatrix,policy:OnsetPolicy)->CaeResult<PhaseTick>{if sample_tick==0{return Err(fail("phase native sample tick is one based"));}phase_scheduler::advance(fields,previous,design,external,sample_tick,owner,velocity_trace,policy,self.subdivisions,self.clock)}
 pub fn linearize(&self,fields:&PersistentContactFields<'_,MultipleContact<MappedPairLaw>>,previous:&MultiContactNativeState,design:&[f64],external:&[f64],base:&PhaseTick,owner:&ContactOwnerIdentity,policy:OnsetPolicy,sample_tick:usize)->CaeResult<FixedPhaseLinearization>{if base.phases.records.len()!=self.subdivisions||sample_tick==0{return Err(fail("phase derivative actual schedule binding"));}phase_derivative::linearize(fields,previous,design,external,base,owner,policy,sample_tick,self.clock)}
 pub fn checkpoint(&self,state:&MultiContactNativeState,identity:&PhaseCheckpointIdentity,next_sample_tick:usize)->CaeResult<Value>{phase_scheduler::checkpoint(state,&identity.value()?,next_sample_tick,self.subdivisions,self.clock)}
 pub fn restore(&self,v:&Value,identity:&PhaseCheckpointIdentity,owner:&ContactOwnerIdentity,contacts:usize,target:f64,shapes:[usize;3],next_sample_tick:usize)->CaeResult<MultiContactNativeState>{phase_scheduler::restore(v,&identity.value()?,owner,contacts,target,shapes,next_sample_tick,self.subdivisions,self.clock)}
}
