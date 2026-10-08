// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_physics_solid::contact_compliance::UnilateralContactCompliance;
use implexity_solve::multirate_coupling::{MultirateStepper,MultirateOptions};
use serde::{Serialize,Deserialize};
use serde_json::Value;
use super::{separate_body::{MovingFsiPairModel,PairSolidField},native_surface_binding::NativeNodalBinding,native_closed_surface_law::NativeClosedSurfaceLaw,contact_field::ContactField,boundary_law::{exposed_surface::IsoSurfacePolicy,closed_surface_distance::DistancePolicy}};
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
pub const SCHEMA:&str="implexity-closed-surface-fsi-pair/1";
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedSurfaceContactDocument{
 pub stiffness_pa_per_m:f64,pub transition_m:f64,pub iso:f64,pub contrast_min:f64,pub interior_margin:f64,pub cap_domain:bool,
 pub minimum_area_ratio:f64,pub minimum_parameter:f64,pub tie_distance_m:f64,pub minimum_distance_m:f64,pub winding_tolerance:f64,
}
impl ClosedSurfaceContactDocument{
 fn policies(&self)->CaeResult<(UnilateralContactCompliance,IsoSurfacePolicy,DistancePolicy)>{
  let law=UnilateralContactCompliance{stiffness_pa_per_m:self.stiffness_pa_per_m,transition_m:self.transition_m};law.evaluate(0.).map_err(fail)?;
  if !self.iso.is_finite()||self.iso<=0.||self.iso>=1.||!self.contrast_min.is_finite()||self.contrast_min<=0.||!self.interior_margin.is_finite()||self.interior_margin<=0.||self.interior_margin>=0.5||!self.minimum_area_ratio.is_finite()||self.minimum_area_ratio<=0.||self.minimum_area_ratio>=1.||!self.minimum_parameter.is_finite()||self.minimum_parameter<=0.||self.minimum_parameter>=1./3.||!self.tie_distance_m.is_finite()||self.tie_distance_m<0.||!self.minimum_distance_m.is_finite()||self.minimum_distance_m<=0.||!self.winding_tolerance.is_finite()||self.winding_tolerance<=0.||self.winding_tolerance>=0.5{return Err(fail("closed surface contact numerical policies"));}
  Ok((law,IsoSurfacePolicy{iso:self.iso,contrast_min:self.contrast_min,interior_margin:self.interior_margin,cap_domain:self.cap_domain},DistancePolicy{minimum_area_ratio:self.minimum_area_ratio,minimum_parameter:self.minimum_parameter,tie_distance_m:self.tie_distance_m,minimum_distance_m:self.minimum_distance_m,winding_tolerance:self.winding_tolerance}))
 }
}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedSurfaceProblemDocument{pub schema:String,pub bodies:[Value;2],pub contact:ClosedSurfaceContactDocument}
pub struct ClosedSurfaceProblem{pub model:MovingFsiPairModel,pub document:ClosedSurfaceProblemDocument,pub identity:String}
pub type ClosedSurfaceStepper<'a>=MultirateStepper<crate::interface::FluidField,ContactField<PairSolidField<'a>,NativeClosedSurfaceLaw>>;
impl ClosedSurfaceProblemDocument{
 pub fn from_json(bytes:&[u8])->CaeResult<Self>{serde_json::from_slice(bytes).map_err(fail)}
 pub fn build(self)->CaeResult<ClosedSurfaceProblem>{
  if self.schema!=SCHEMA{return Err(fail("closed surface FSI pair schema"));}self.contact.policies()?;
  let model=MovingFsiPairModel::from_bodies(crate::problem::normalise(&self.bodies[0])?,crate::problem::normalise(&self.bodies[1])?)?;
  let _=model.solid_field()?;
  if format!("{:?}",model.native_body(0)?.problem.coupling)!=format!("{:?}",model.native_body(1)?.problem.coupling){return Err(fail("closed surface FSI pair coupling policies differ"));}
  let identity=implexity_core::json::canonical_sha256(&serde_json::to_value(&self).map_err(fail)?);
  Ok(ClosedSurfaceProblem{model,document:self,identity})
 }
}
impl ClosedSurfaceProblem{
 pub fn physical_design(&self,controls:[&[f64];2])->CaeResult<Vec<f64>>{self.model.event_physical_design(controls)}
 pub fn stepper(&self,design:&[f64])->CaeResult<ClosedSurfaceStepper<'_>>{
  let field=self.model.solid_field()?;let native=NativeNodalBinding::new(&self.model,&field,design)?;let(law,surface,distance)=self.document.contact.policies()?;
  let contact=NativeClosedSurfaceLaw::new(&native,law,surface,distance)?;let field=ContactField::new(field,contact)?;let c=&self.model.native_body(0)?.problem.coupling;
  Ok(MultirateStepper::new(self.model.fluid_field()?,field,MultirateOptions{mode:c.mode.clone(),schur_ratio_limit:c.schur_ratio_limit,work_defect_limit:c.work_defect_limit})?.with_identity(self.identity.clone()).with_field_newton(c.field_newton)?.with_linear_solves(c.linear_solves)?.with_step_cache_bytes(c.step_cache_bytes))
 }
}

impl ClosedSurfaceProblem{
 pub fn admit_loose_coupling(&self,stepper:&ClosedSurfaceStepper<'_>,design:&[f64])->CaeResult<serde_json::Value>{
  use implexity_solve::{multirate_coupling::{CouplingMode,FluxDrivenField},time_stepper::{TimeStepper,StepParameters},factorization::Factorization,local_condensation::LocalEliminationPartition};
  use implexity_physics_solid::soft::stepper::Scheme;
  let policy=&self.model.native_body(0)?.problem.coupling;
  if !matches!(policy.mode,CouplingMode::Loose{..}){return Ok(serde_json::json!({"required":false}));}
  let state=stepper.initial_state(design)?;let layout=stepper.layout();let field=stepper.field_b();let native=field.native();let previous=&state[layout.field_b.clone()];let p=StepParameters{design,time_scale:1.};let pred=field.predict(1,previous,None,p);
  let current=field.current_jacobian(1,&pred,previous,&vec![0.;field.trace_operator().nrows()],p)?;let(_,flux)=native.current_flux_jacobians(1,&pred[..native.state_size()],&previous[..native.state_size()],&vec![0.;native.trace_operator().nrows()],p)?;let flux=flux.to_csr()?;
  let partition=field.local_elimination_groups()?.map(|g|LocalEliminationPartition::new(field.state_size(),g).map_err(fail)).transpose()?;
  let factor=Factorization::correction_with_partition(current,field.state_size(),None,partition.as_ref())?;
  let dt=stepper.nominal_step_s();let mut blocks=vec![];let mut state_offset=0;let mut flux_offset=0;let mut design_offset=0;
  for body in 0..2{
   let body_field=native.body_field(body)?;let size=body_field.design_size();let local=StepParameters{design:&design[design_offset..design_offset+size],time_scale:1.};let history=body_field.core().history(local)?;let mass=history.mass_matrix().clone();let mut row=vec![0.;mass.nrows()];
   for(i,v)in row.iter_mut().enumerate(){let(columns,values)=flux.row(state_offset+i);*v=-columns.iter().zip(values).find(|(j,_)|**j==flux_offset+i).map(|(_,v)|*v).unwrap_or(0.);}
   let coefficient=match self.model.native_body(body)?.problem.solid.scheme{Scheme::Quasistatic=>0.,Scheme::Newmark{beta,..}=>1./beta,Scheme::GeneralizedAlpha{rho_inf}=>{let am=(2.*rho_inf-1.)/(rho_inf+1.);let af=rho_inf/(rho_inf+1.);(1.-am)/(0.25*(1.-am+af).powi(2))},Scheme::AvfMidpoint{..}=>2.};
   if !coefficient.is_finite()||coefficient<=0.{return Err(fail("closed surface loose coupling needs inertial bodies"));}
   blocks.push((mass,row,coefficient,state_offset));state_offset+=body_field.state_size();flux_offset+=body_field.trace_operator().nrows();design_offset+=size;
  }
  let mass_action=|phi:&[f64]|->Vec<f64>{let mut out=vec![0.;field.state_size()];for(mass,row,coefficient,offset)in &blocks{let product=mass.matvec(&phi[*offset..*offset+mass.ncols()]).unwrap_or_else(|_|vec![f64::NAN;mass.nrows()]);for i in 0..product.len(){out[*offset+i]=row[i]*coefficient/(dt*dt)*product[i];}}out};
  let binding=NativeNodalBinding::new(&self.model,native,design)?;let mut free=vec![false;field.state_size()];let mut modes=vec![vec![0.;field.state_size()];policy.preflight_modes];
  for body in 0..2{let body_field=native.body_field(body)?;let offset=blocks[body].3;for node in &binding.nodes()[body]{for a in 0..3{free[node.state[a]]= !body_field.core().model().fixed[node.state[a]-offset];}for(m,mode)in modes.iter_mut().enumerate(){let axis=m%3;if free[node.state[axis]]{let sign=if body==1&&(m/3)%2==0{-1.}else{1.};let weight=1.+(m/6)as f64*node.reference_m.iter().sum::<f64>();mode[node.state[axis]]=sign*weight;}}}}
  for _ in 0..8{
   let mut next=vec![];for mode in &modes{let rhs=mass_action(mode);let(x,_,_)=factor.solve(&rhs,false)?;let mut value=vec![0.;field.state_size()];for i in 0..value.len(){if free[i]{value[i]=x[i];}}next.push(value);}
   let mut orthogonal:Vec<Vec<f64>>=vec![];for mut value in next{for basis in &orthogonal{let mass=mass_action(basis);let projection:f64=value.iter().zip(mass).map(|(a,b)|a*b).sum();for(a,b)in value.iter_mut().zip(basis){*a-=projection*b;}}let mass=mass_action(&value);let norm=value.iter().zip(mass).map(|(a,b)|a*b).sum::<f64>().abs().sqrt();if norm.is_finite()&&norm>0.{for v in &mut value{*v/=norm;}orthogonal.push(value);}}
   if orthogonal.is_empty(){return Err(fail("closed surface bodies have no free inertial modes"));}modes=orthogonal;
  }
  let ratios=stepper.interface_schur_ratio(design,&state,&modes,&mass_action)?;
  Ok(serde_json::json!({"required":true,"ratios":ratios,"limit":policy.schur_ratio_limit,"mode_count":modes.len(),"inverse_iterations":8,"native_inertia":true,"exact_local_elimination":partition.is_some(),"global_spectral_bound_certified":false}))
 }
}
