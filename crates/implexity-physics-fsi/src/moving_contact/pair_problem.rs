// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use serde::{Deserialize,Serialize};
use serde_json::Value;
use implexity_core::{CaeError,CaeResult};
use implexity_physics_lbm::moving::pushforward::InterpolationKernel;
use crate::problem::normalise;
use super::{separate_body::MovingFsiPairModel,model::MovingContactSpec,surface_map::SurfaceFeature,linear_path::PathPolicy,event_step::EventAdvancePolicy,impact::ImpactTolerance};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairProblemDocument{pub schema:String,pub bodies:[Value;2],pub contact:PairContactDocument,pub event:PairEventDocument}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairContactDocument{pub body_nodes:[Vec<usize>;2],pub features:[PairFeatureDocument;4],pub bodies:[usize;4],pub gap_scale_m:f64,pub force_scale_n:f64,pub path:PairPathDocument}
#[derive(Clone,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum PairFeatureDocument{Vertex{index:usize},FixedTetrahedron{nodes:[usize;4],weights:[f64;4],weight_tolerance:f64},DensityEdge{nodes:[usize;2],iso:f64,contrast_min:f64,interior_margin:f64}}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairPathDocument{pub minimum_barycentric:f64,pub minimum_area_ratio:f64,pub minimum_signed_gap:f64,pub time_resolution:f64,pub maximum_intervals:usize,pub maximum_depth:u32}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairEventDocument{pub restitution:f64,pub maintained_gap_target_fraction:f64,pub residual_tolerance:f64,pub maximum_newton_iterations:usize,pub maximum_localization_iterations:usize,pub interpolation_kernel:String,pub impact:PairImpactDocument}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairImpactDocument{pub event_gap_m:f64,pub normal_velocity_m_s:f64,pub impulse_n_s:f64,pub momentum_n_s:f64,pub energy_j:f64}
pub struct PairProblemProvider{pub model:MovingFsiPairModel,pub event:EventAdvancePolicy,pub document_identity:String}
impl PairProblemDocument{
 pub fn from_json(bytes:&[u8])->CaeResult<Self>{serde_json::from_slice(bytes).map_err(|e|CaeError::contract(e.to_string()))}
 pub fn build(self)->CaeResult<PairProblemProvider>{
  if self.schema!="implexity-moving-fsi-pair/1"{return Err(fail("two-body schema"));}
  let p=&self.contact.path;let e=&self.event;let i=&e.impact;
  if ![self.contact.gap_scale_m,self.contact.force_scale_n,p.minimum_area_ratio,p.time_resolution,e.residual_tolerance,i.event_gap_m,i.normal_velocity_m_s,i.impulse_n_s,i.momentum_n_s,i.energy_j].iter().all(|v|v.is_finite()&&*v>0.)||!p.minimum_barycentric.is_finite()||!(0. ..1./3.).contains(&p.minimum_barycentric)||p.minimum_signed_gap!=0.||p.maximum_intervals==0||p.maximum_depth==0||p.maximum_depth>60||!e.restitution.is_finite()||!(0. ..=1.).contains(&e.restitution)||!e.maintained_gap_target_fraction.is_finite()||!(0. ..=0.5).contains(&e.maintained_gap_target_fraction)||e.maximum_newton_iterations==0||e.maximum_localization_iterations==0{return Err(fail("two-body explicit numerical policy domain"));}
  let kernel=match e.interpolation_kernel.as_str(){"cubic"=>InterpolationKernel::Cubic,"peskin4"=>InterpolationKernel::Peskin4,_=>return Err(fail("two-body interpolation kernel"))};
  let mut features=Vec::new();for f in &self.contact.features{features.push(match f{PairFeatureDocument::Vertex{index}=>SurfaceFeature::Vertex(*index),PairFeatureDocument::FixedTetrahedron{nodes,weights,weight_tolerance}=>SurfaceFeature::FixedTetrahedron{nodes:*nodes,weights:*weights,weight_tolerance:*weight_tolerance},PairFeatureDocument::DensityEdge{nodes,iso,contrast_min,interior_margin}=>{if !iso.is_finite()||!(0. ..=1.).contains(iso)||!contrast_min.is_finite()||*contrast_min<=0.||!interior_margin.is_finite()||!(0. ..0.5).contains(interior_margin)||nodes[0]==nodes[1]{return Err(fail("two-body density feature domain"));}SurfaceFeature::DensityEdge{nodes:*nodes,iso:*iso,contrast_min:*contrast_min,interior_margin:*interior_margin}}});}
  if self.contact.bodies[0]>1||self.contact.bodies[1..].iter().any(|b|*b>1||*b==self.contact.bodies[0]){return Err(fail("two-body contact requires opposite point and triangle owners"));}
  let value=serde_json::to_value(&self).map_err(|e|CaeError::contract(e.to_string()))?;let identity=implexity_core::json::canonical_sha256(&value);
  let spec=MovingContactSpec{body_nodes:self.contact.body_nodes.clone(),features:features.try_into().map_err(|_|fail("two-body feature count"))?,bodies:self.contact.bodies,replaced_planes:vec![],gap_scale_m:self.contact.gap_scale_m,force_scale_n:self.contact.force_scale_n,path_policy:PathPolicy{minimum_barycentric:p.minimum_barycentric,minimum_area_ratio:p.minimum_area_ratio,minimum_signed_gap:p.minimum_signed_gap,time_resolution:p.time_resolution,maximum_intervals:p.maximum_intervals,maximum_depth:p.maximum_depth}};
  let model=MovingFsiPairModel::new(normalise(&self.bodies[0])?,normalise(&self.bodies[1])?,spec)?;let _=model.contact_field()?;for b in 0..2{if model.native_body(b)?.problem.solid.supports.iter().any(|s|s.motion.is_some()){return Err(fail("two-body event provider requires stationary prescribed supports"));}}
  let event=EventAdvancePolicy{restitution:e.restitution,maintained_gap_target_fraction:e.maintained_gap_target_fraction,residual_tolerance:e.residual_tolerance,maximum_newton_iterations:e.maximum_newton_iterations,maximum_localization_iterations:e.maximum_localization_iterations,interpolation_kernel:kernel,impact:ImpactTolerance{event_gap_m:i.event_gap_m,normal_velocity_m_s:i.normal_velocity_m_s,impulse_n_s:i.impulse_n_s,momentum_n_s:i.momentum_n_s,energy_j:i.energy_j}};
  Ok(PairProblemProvider{model,event,document_identity:identity})
 }
}

impl PairProblemProvider{
 pub fn initial_state(&self,design:&[f64],origin_s:f64)->CaeResult<super::event_step::EventTickState>{
  use implexity_solve::multirate_coupling::{FluxDrivenField,SubcycledField};
  use super::contact_field::NativeContactLaw;
  if !origin_s.is_finite()||origin_s<0.{return Err(fail("two-body absolute origin"));}
  let field=self.model.contact_field()?;let solid=field.initial_state(design)?;
  let(_,gap)=field.law().instantaneous_row(&solid,design)?;
  if gap<=0.{return Err(fail("initial closed contact requires a qualified preload/equilibrium state"));}
  let fluid=self.model.event_fluid_field(self.event)?.initial_state(design)?;
  let count=self.model.solid_field()?.trace_operator().nrows();
  Ok(super::event_step::EventTickState{solid,fluid,lagged_force_n:vec![0.;count],origin_s})
 }
}

impl PairEventDocument{
 pub fn policy(&self)->CaeResult<EventAdvancePolicy>{let e=self;let i=&e.impact;if ![e.residual_tolerance,i.event_gap_m,i.normal_velocity_m_s,i.impulse_n_s,i.momentum_n_s,i.energy_j].iter().all(|v|v.is_finite()&&*v>0.)||!e.restitution.is_finite()||!(0. ..=1.).contains(&e.restitution)||!e.maintained_gap_target_fraction.is_finite()||!(0. ..=0.5).contains(&e.maintained_gap_target_fraction)||e.maximum_newton_iterations==0||e.maximum_localization_iterations==0{return Err(fail("native pair event numerical policy domain"));}let interpolation_kernel=match e.interpolation_kernel.as_str(){"cubic"=>InterpolationKernel::Cubic,"peskin4"=>InterpolationKernel::Peskin4,_=>return Err(fail("two-body interpolation kernel"))};Ok(EventAdvancePolicy{restitution:e.restitution,maintained_gap_target_fraction:e.maintained_gap_target_fraction,residual_tolerance:e.residual_tolerance,maximum_newton_iterations:e.maximum_newton_iterations,maximum_localization_iterations:e.maximum_localization_iterations,interpolation_kernel,impact:ImpactTolerance{event_gap_m:i.event_gap_m,normal_velocity_m_s:i.normal_velocity_m_s,impulse_n_s:i.impulse_n_s,momentum_n_s:i.momentum_n_s,energy_j:i.energy_j}})}
}
