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
use implexity_solve::{multirate_coupling::FluxDrivenField,time_stepper::StepParameters};
use implexity_linalg::sparse::CsrMatrix;
use crate::moving_contact::{contact_field::{ContactField,NativeContactLaw},collection::MultipleContact,mapped_pair_law::MappedPairLaw,separate_body::PairSolidField,history_branch::{ContactOwnerIdentity,ContactHistoryBranch},selected_transition::OnsetPolicy};
use serde_json::{Value,json};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn digest(x:&[f64])->String{implexity_core::json::sha256_hex(&x.iter().flat_map(|x|x.to_le_bytes()).collect::<Vec<_>>())}
#[derive(Clone)]pub struct AvfVelocityCertificate{branch:ContactHistoryBranch,state_digest:String,scale:f64,transported:Vec<f64>,raw:Vec<f64>}
impl AvfVelocityCertificate{
 pub fn check(&self,state:&[f64],owner:&ContactOwnerIdentity,selected:&[bool],policy:OnsetPolicy)->CaeResult<()>{
  if !policy.normal_velocity_tolerance_m_s.is_finite()||policy.normal_velocity_tolerance_m_s<0.||!policy.residual_tolerance.is_finite()||policy.residual_tolerance<=0.||!policy.gap_target_fraction.is_finite()||!(0. ..=0.5).contains(&policy.gap_target_fraction)||policy.maximum_newton_iterations==0||policy.maximum_active_set_iterations==0{return Err(fail("AVF certificate finite admission policy"));}
  if state.len()<selected.len()||state.iter().any(|v|!v.is_finite())||selected.iter().enumerate().any(|(k,a)|*a&&state[state.len()-selected.len()+k]<=0.){return Err(fail("AVF maintained certificate requires positive active multipliers"));}
  if self.state_digest!=digest(state)||self.transported.len()!=selected.len()||self.raw.len()!=selected.len()||self.transported.iter().chain(&self.raw).any(|v|!v.is_finite())||self.branch.selected()!=selected{return Err(fail("AVF maintained certificate native state/branch"));}
  let ck=self.branch.checkpoint(state)?;let clock=f64::from_bits(u64::from_str_radix(ck["owner_descriptor"]["absolute_clock_bits"].as_str().ok_or_else(||fail("AVF clock"))?,16).map_err(|_|fail("AVF clock"))?);
  owner.check_clock(clock)?;self.branch.check_binding(&owner.for_phase(self.scale,clock)?,selected.len(),policy.residual_tolerance*policy.gap_target_fraction)?;
  if selected.iter().zip(&self.transported).any(|(a,v)|*a&&*v < -policy.normal_velocity_tolerance_m_s){return Err(fail("AVF maintained transported velocity certificate"));}Ok(())
 }
 pub fn value(&self,state:&[f64])->CaeResult<Value>{Ok(json!({"schema":"avf-discrete-frame-maintained-admission/1","branch":self.branch.checkpoint(state)?,"state_sha256":self.state_digest,"scale_bits":format!("{:016x}",self.scale.to_bits()),"transported_bits":self.transported.iter().map(|v|format!("{:016x}",v.to_bits())).collect::<Vec<_>>(),"raw_endpoint_bits":self.raw.iter().map(|v|format!("{:016x}",v.to_bits())).collect::<Vec<_>>()}))}
 pub fn restore(v:&Value,expected_payload_sha256:&str,state:&[f64],owner:&ContactOwnerIdentity,selected:&[bool],policy:OnsetPolicy)->CaeResult<Self>{if implexity_core::json::canonical_sha256(v)!=expected_payload_sha256{return Err(fail("AVF certificate trusted payload digest"));}let o=v.as_object().ok_or_else(||fail("AVF certificate object"))?;if o.len()!=6||v["schema"]!="avf-discrete-frame-maintained-admission/1"||v["state_sha256"]!=digest(state){return Err(fail("AVF certificate identity"));}let scalar=|v:&Value|->CaeResult<f64>{Ok(f64::from_bits(u64::from_str_radix(v.as_str().ok_or_else(||fail("AVF certificate bits"))?,16).map_err(|_|fail("AVF certificate bits"))?))};let scale=scalar(&v["scale_bits"])?;let clock=scalar(&v["branch"]["owner_descriptor"]["absolute_clock_bits"])?;owner.check_clock(clock)?;let branch=ContactHistoryBranch::restore(&v["branch"],state,&owner.for_phase(scale,clock)?,selected.len(),policy.residual_tolerance*policy.gap_target_fraction)?;let array=|v:&Value|->CaeResult<Vec<f64>>{v.as_array().ok_or_else(||fail("AVF certificate array"))?.iter().map(scalar).collect()};let out=Self{branch,state_digest:digest(state),scale,transported:array(&v["transported_bits"])?,raw:array(&v["raw_endpoint_bits"])?};out.check(state,owner,selected,policy)?;Ok(out)}
 pub(crate) fn bind_phase_endpoint(mut self,branch:ContactHistoryBranch,state:&[f64])->CaeResult<Self>{if self.state_digest!=digest(state)||self.branch.selected()!=branch.selected(){return Err(fail("AVF phase endpoint binding"));}self.branch=branch;Ok(self)}
 pub fn transported(&self)->&[f64]{&self.transported}pub fn raw(&self)->&[f64]{&self.raw}
}
pub fn require_avf<L:ContactSetKinematics>(field:&ContactField<PairSolidField<'_>,L>,design:&[f64],scale:f64)->CaeResult<()>{if design.len()!=field.design_size()||design.iter().any(|x|!x.is_finite())||!scale.is_finite()||scale<=0.{return Err(fail("AVF native parameters"));}let mut off=0;for b in 0..2{let body=field.native().body_field(b)?;let n=body.design_size();let h=body.core().history(StepParameters{design:&design[off..off+n],time_scale:scale})?;if !matches!(h.scheme,implexity_physics_solid::soft::stepper::Scheme::AvfMidpoint{..}){return Err(fail("discrete-frame maintained admission requires native AVF"));}off+=n;}if off!=design.len(){return Err(fail("AVF native body design shape"));}Ok(())}
pub fn transported<F:FluxDrivenField,L:ContactSetKinematics>(field:&ContactField<F,L>,old:&[f64],new:&[f64],p:StepParameters<'_>,velocity:&CsrMatrix)->CaeResult<Vec<f64>>{
 let v0=velocity.matvec(old).map_err(|e|CaeError::contract(e.to_string()))?;let v1=velocity.matvec(new).map_err(|e|CaeError::contract(e.to_string()))?;let mean:Vec<_>=v0.iter().zip(&v1).map(|(a,b)|0.5*(a+b)).collect();let old_normal=field.law().normal_velocities(old,p.design,&v0)?;let matrix=field.law().evaluate(1,new,old,p)?.force_current.to_csr()?;let n=field.law().native_states();let count=field.law().contact_count();let mut dg=vec![0.;count];for i in 0..matrix.nrows(){let(js,vs)=matrix.row(i);for(&j,&a)in js.iter().zip(vs){if j>=n{dg[j-n]+=a*mean[i];}}}let out:Vec<_>=dg.iter().zip(old_normal).map(|(a,b)|2.*a-b).collect();if out.iter().any(|v|!v.is_finite()){return Err(fail("AVF transported velocity finite"));}Ok(out)
}
pub(crate) fn certificate(branch:ContactHistoryBranch,state:&[f64],scale:f64,transported:Vec<f64>,raw:Vec<f64>)->AvfVelocityCertificate{AvfVelocityCertificate{branch,state_digest:digest(state),scale,transported,raw}}
