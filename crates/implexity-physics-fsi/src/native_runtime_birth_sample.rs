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

use crate::{gas_geometry_time::GeometryTimeBase,native_closed_release::{NativeClosedGeometryWitness,NativeClosedTrialSample},native_continuous_coupled_field::NativeContinuousCoupledField,native_gas_geometry_adapter::NativeGasGeometryAdapter};
use implexity_core::{CaeError,CaeResult};
use implexity_solve::time_stepper::StepParameters;
use std::sync::Arc;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
enum NativeRuntimeBirthOwner {
    Primal{macro_witness:Arc<NativeClosedGeometryWitness>,child_witness:Arc<NativeClosedGeometryWitness>,fractions:(f64,f64)},
    Trial(Arc<NativeClosedTrialSample>),
}
pub(crate) struct NativeRuntimeBirthSample<'a,S:GeometryTimeBase>{
    field:&'a NativeContinuousCoupledField,owner:NativeRuntimeBirthOwner,
    macro_start:Vec<S>,macro_end:Vec<S>,child_start:Vec<S>,child_end:Vec<S>,design:Vec<S>,time_scale:S,
    macro_index:usize,tick:usize,nominal_tick_step_s:f64,
    closed_start_charts:Vec<usize>,closed_end_charts:Vec<usize>,maintained:Vec<(usize,usize)>,released:Vec<(usize,usize)>,
}
impl<'a,S:GeometryTimeBase> NativeRuntimeBirthSample<'a,S>{
    pub(crate) fn adapter(&self)->&'a NativeGasGeometryAdapter{self.field.config.geometry.as_ref()}
    pub(crate) fn binding_identity(&self)->String{self.adapter().kinematics.binding_identity()}
    pub(crate) fn validate_path_owner(&self,path:&crate::native_gas_geometry_adapter::NativeGasGeometryPath<S>)->CaeResult<()>{match &self.owner{NativeRuntimeBirthOwner::Primal{child_witness,..}=>self.adapter().validate_birth_primal_owner(path,child_witness),NativeRuntimeBirthOwner::Trial(sample)=>self.adapter().validate_birth_trial_owner(path,sample)}}
    pub(crate) fn primal_child_witness_arc(&self)->CaeResult<Arc<NativeClosedGeometryWitness>>{match &self.owner{NativeRuntimeBirthOwner::Primal{child_witness,..}=>Ok(child_witness.clone()),_=>Err(fail("birth sample is not a genuine primal witness"))}}
    pub(crate) fn child_start(&self)->&[S]{&self.child_start}
    pub(crate) fn child_end(&self)->&[S]{&self.child_end}
    pub(crate) fn macro_start(&self)->&[S]{&self.macro_start}
    pub(crate) fn macro_end(&self)->&[S]{&self.macro_end}
    pub(crate) fn design(&self)->&[S]{&self.design}
    pub(crate) fn time_scale(&self)->S{self.time_scale}
    pub(crate) fn nominal_tick_step_s(&self)->f64{self.nominal_tick_step_s}
    pub(crate) fn primal_child_witness(&self)->CaeResult<&NativeClosedGeometryWitness>{match &self.owner{NativeRuntimeBirthOwner::Primal{child_witness,..}=>Ok(child_witness),_=>Err(fail("runtime sample is not a primal witness"))}}
    pub(crate) fn closed_start_charts(&self)->&[usize]{&self.closed_start_charts}
    pub(crate) fn closed_end_charts(&self)->&[usize]{&self.closed_end_charts}
    pub(crate) fn maintained_corners(&self)->&[(usize,usize)]{&self.maintained}
    pub(crate) fn released_corners(&self)->&[(usize,usize)]{&self.released}
    pub(crate) fn validate_birth_chart(&self,chart:usize)->CaeResult<()>{
        self.validate()?;
        if self.tick!=1||!self.closed_start_charts.contains(&chart)||self.closed_end_charts.contains(&chart)||!self.released.iter().any(|v|v.0==chart){return Err(fail("runtime sample does not own a released closed-start chart"));}Ok(())
    }
    fn validate(&self)->CaeResult<()>{
        let cfg=self.field.config.bulk.config();let x=self.design.iter().map(|v|v.value()).collect::<Vec<_>>();let a=self.macro_start.iter().map(|v|v.value()).collect::<Vec<_>>();let b=self.macro_end.iter().map(|v|v.value()).collect::<Vec<_>>();
        if self.tick==0||self.tick>cfg.substeps||self.nominal_tick_step_s.to_bits()!=(cfg.macro_step_s/cfg.substeps as f64).to_bits(){return Err(fail("runtime birth sample clock owner differs"));}
        let f=((self.tick-1)as f64/cfg.substeps as f64,self.tick as f64/cfg.substeps as f64);
        let p=StepParameters{design:&x,time_scale:self.time_scale.value()};
        match &self.owner {
            NativeRuntimeBirthOwner::Primal{macro_witness,child_witness,fractions}=>{if fractions.0.to_bits()!=f.0.to_bits()||fractions.1.to_bits()!=f.1.to_bits()||!std::ptr::eq(child_witness.macro_witness(),macro_witness.as_ref()){return Err(fail("primal birth sample parent or schedule differs"));}macro_witness.validate_macro(self.macro_index,p)?;macro_witness.validate_path(&self.adapter().kinematics,&a,&b,&x)?;child_witness.validate_path(&self.adapter().kinematics,&self.child_start.iter().map(|v|v.value()).collect::<Vec<_>>(),&self.child_end.iter().map(|v|v.value()).collect::<Vec<_>>(),&x)?;},
            NativeRuntimeBirthOwner::Trial(sample)=>{let actual=sample.fractions();if actual.0.to_bits()!=f.0.to_bits()||actual.1.to_bits()!=f.1.to_bits(){return Err(fail("trial birth sample schedule differs"));}sample.parent().validate_macro(self.macro_index,p)?;sample.validate_path(&self.adapter().kinematics,&a,&b,&self.child_start.iter().map(|v|v.value()).collect::<Vec<_>>(),&self.child_end.iter().map(|v|v.value()).collect::<Vec<_>>(),&x)?;},
        }Ok(())
    }
}
impl NativeContinuousCoupledField {
    fn checked_birth_tick(&self,tick:usize)->CaeResult<(f64,f64,f64)>{let c=self.config.bulk.config();if tick==0||tick>c.substeps||!c.macro_step_s.is_finite()||c.macro_step_s<=0.{return Err(fail("invalid runtime birth tick"));}Ok(((tick-1)as f64/c.substeps as f64,tick as f64/c.substeps as f64,c.macro_step_s/c.substeps as f64))}
    fn classify_birth_corners(&self,a:&[f64],b:&[f64],x:&[f64])->CaeResult<(Vec<(usize,usize)>,Vec<(usize,usize)>)>{
        let law=self.config.closed_contact_law.as_ref().ok_or_else(||fail("runtime birth sample lacks its actual contact law"))?;
        let ga=law.release_trace_chart_gaps(a,x)?;let gb=law.release_trace_chart_gaps(b,x)?;if ga.len()!=gb.len(){return Err(fail("birth gap chart count differs"));}
        let mut maintained=Vec::new();let mut released=Vec::new();for chart in 0..ga.len(){for corner in 0..3{let s=ga[chart][corner];let e=gb[chart][corner];if !s.is_finite()||!e.is_finite()||s<0.||e<0.{return Err(fail("birth contact gap is outside nonnegative domain"));}if s==0.{if e==0.{maintained.push((chart,corner));}else{released.push((chart,corner));}}}}Ok((maintained,released))
    }
    pub(crate) fn primal_birth_sample_for_tick<'a>(&'a self,n:usize,tick:usize,a:&[f64],b:&[f64],x:&[f64],time_scale:f64,macro_witness:Arc<NativeClosedGeometryWitness>)->CaeResult<Arc<NativeRuntimeBirthSample<'a,f64>>>{
        if time_scale.to_bits()!=1f64.to_bits(){return Err(fail("primal birth uses the fixed native time scale"));}
        let(alpha,beta,dt)=self.checked_birth_tick(tick)?;let law=self.config.closed_contact_law.as_ref().ok_or_else(||fail("primal birth lacks its actual contact law"))?;
        macro_witness.validate_macro(n,StepParameters{design:x,time_scale})?;macro_witness.validate_path(&self.config.geometry.kinematics,a,b,x)?;
        let child=Arc::new(macro_witness.sample_child(law,alpha,beta)?);let aa=a.iter().zip(b).map(|(a,b)|a+(b-a)*alpha).collect::<Vec<_>>();let bb=a.iter().zip(b).map(|(a,b)|a+(b-a)*beta).collect::<Vec<_>>();
        let(maintained,released)=self.classify_birth_corners(&aa,&bb,x)?;
        let sample=Arc::new(NativeRuntimeBirthSample{field:self,owner:NativeRuntimeBirthOwner::Primal{macro_witness,child_witness:child.clone(),fractions:(alpha,beta)},macro_start:a.to_vec(),macro_end:b.to_vec(),child_start:aa,child_end:bb,design:x.to_vec(),time_scale,macro_index:n,tick,nominal_tick_step_s:dt,closed_start_charts:child.closed_start_charts().to_vec(),closed_end_charts:child.closed_end_charts().to_vec(),maintained,released});sample.validate()?;Ok(sample)
    }
    pub(crate) fn trial_birth_sample_for_receipt<'a,S:GeometryTimeBase>(&'a self,n:usize,tick:usize,receipt:crate::native_gas_geometry_adapter::NativeProjectedTrialChildReceipt<'a,S>)->CaeResult<Arc<NativeRuntimeBirthSample<'a,S>>>{
        if !std::ptr::eq(receipt.adapter(),self.config.geometry.as_ref()){return Err(fail("trial receipt belongs to another actual runtime adapter"));}
        let(alpha,beta,dt)=self.checked_birth_tick(tick)?;let sample=receipt.sample().clone();let actual=sample.fractions();if actual.0.to_bits()!=alpha.to_bits()||actual.1.to_bits()!=beta.to_bits(){return Err(fail("trial receipt schedule differs"));}
        let time_scale=receipt.time_scale();if time_scale.value().to_bits()!=1f64.to_bits()||!time_scale.finite_first_order()||time_scale.geometry_first_order_directions().map_or(time_scale.has_direction(),|v|v.iter().any(|v|*v!=0.)){return Err(fail("trial receipt time scale is outside native domain"));}
        let values=receipt.design().iter().map(|v|v.value()).collect::<Vec<_>>();sample.parent().validate_macro(n,StepParameters{design:&values,time_scale:time_scale.value()})?;
        let(maintained,released)=self.classify_birth_corners(&receipt.child_start().iter().map(|v|v.value()).collect::<Vec<_>>(),&receipt.child_end().iter().map(|v|v.value()).collect::<Vec<_>>(),&values)?;
        let owner=Arc::new(NativeRuntimeBirthSample{field:self,closed_start_charts:sample.closed_start_charts().to_vec(),closed_end_charts:sample.closed_end_charts().to_vec(),owner:NativeRuntimeBirthOwner::Trial(sample),macro_start:receipt.macro_start().to_vec(),macro_end:receipt.macro_end().to_vec(),child_start:receipt.child_start().to_vec(),child_end:receipt.child_end().to_vec(),design:receipt.design().to_vec(),time_scale,macro_index:n,tick,nominal_tick_step_s:dt,maintained,released});owner.validate()?;Ok(owner)
    }
    pub(crate) fn trial_birth_sample_for_tick<'a,S:GeometryTimeBase>(&'a self,n:usize,tick:usize,a:&[S],b:&[S],x:&[S],time_scale:S,sample:Arc<NativeClosedTrialSample>)->CaeResult<Arc<NativeRuntimeBirthSample<'a,S>>>{
        if time_scale.value().to_bits()!=1f64.to_bits()||!time_scale.finite_first_order()||time_scale.geometry_first_order_directions().map_or(time_scale.has_direction(),|v|v.iter().any(|v|*v!=0.))||a.iter().chain(b).chain(x).any(|v|!v.finite_first_order()){return Err(fail("trial birth is outside fixed native scalar domain"));}
        let(alpha,beta,dt)=self.checked_birth_tick(tick)?;let actual=sample.fractions();if actual.0.to_bits()!=alpha.to_bits()||actual.1.to_bits()!=beta.to_bits(){return Err(fail("trial birth sample fractions differ"));}
        let values=x.iter().map(|v|v.value()).collect::<Vec<_>>();sample.parent().validate_macro(n,StepParameters{design:&values,time_scale:time_scale.value()})?;
        let(aa,bb)=self.config.geometry.project_and_sample_closed_trial_geometry_trace(a,b,x,time_scale,&sample)?;
        let(maintained,released)=self.classify_birth_corners(&aa.iter().map(|v|v.value()).collect::<Vec<_>>(),&bb.iter().map(|v|v.value()).collect::<Vec<_>>(),&values)?;
        let owner=Arc::new(NativeRuntimeBirthSample{field:self,closed_start_charts:sample.closed_start_charts().to_vec(),closed_end_charts:sample.closed_end_charts().to_vec(),owner:NativeRuntimeBirthOwner::Trial(sample),macro_start:a.to_vec(),macro_end:b.to_vec(),child_start:aa,child_end:bb,design:x.to_vec(),time_scale,macro_index:n,tick,nominal_tick_step_s:dt,maintained,released});owner.validate()?;Ok(owner)
    }
}
