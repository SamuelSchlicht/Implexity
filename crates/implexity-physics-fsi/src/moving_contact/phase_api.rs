// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
