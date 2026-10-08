// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::{any::Any,collections::BTreeMap,sync::{Arc,Mutex}};
use ndarray::{ArrayD,IxDyn};
use serde_json::{Value,Map,json};
use implexity_core::{CaeError,CaeResult,contracts::{CaeProvider,ProviderProblem,ProviderCapabilities,ProviderDescriptor,Evaluation,Sensitivity},packages::InstallContext};
use implexity_optim::{design::{NamedArrays,design_identity},provider_ops::{DesignOperations,DesignOp,DesignSensitivities,DesignSensitivity,LifecycleDeclaration},optimizer::OptimizerLifecycleConfig};
use implexity_solve::{multirate_coupling::{FluxDrivenField,SubcycledField},time_stepper::TimeStepper,checkpointed_history::HistoryRun,dynamic_program::DynamicProgram};
use crate::moving_contact::{phase_problem::{ContactSetDocument,ContactSetProblem,InitialDocument},resources::PairNativeResources,contact_set::{multiple_contact_law,native_velocity_trace_from_pair},sample_projection::{from_observables,PairSampleProjection},segment_quadrature::SegmentClock};
use super::macro_checkpoint::{CheckpointMacroStepper,MacroHistoryArchive};
pub const NAME:&str="moving_fsi_contact_set_fixed_horizon";
const COORDS:[&str;2]=["body0:model:control","body1:model:control"];
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn value(p:&ProviderProblem)->CaeResult<&Value>{p.downcast_ref::<Value>().ok_or_else(||fail("contact-set provider problem type"))}
fn diagnostics()->Map<String,Value>{json!({"physical_qualification":false,"derivative_scope":"declared fixed-reference material controls; strict selected schedule or authenticated unloaded initial design prefix","ambient_biactive_gradients":false,"surface_topology_gradients":false,"complete_surface_contact_qualified":false,"autonomous_orbit_qualified":false,"measured_frequency_qualified":false,"checkpoint_owner":"native authored Online history and exact branch/admission archive","time_scale_gradient_available":false}).as_object().unwrap().clone()}
struct Document{pair:ContactSetProblem,program:DynamicProgram,restricted:bool}
#[derive(Clone)]
struct Cached{key:String,run:Arc<HistoryRun>,archive:MacroHistoryArchive,responses:BTreeMap<String,f64>,gradients:Option<BTreeMap<String,NamedArrays>>}
#[derive(Default)]
pub struct ContactSetFixedHorizonProvider{cache:Mutex<Option<Cached>>}
impl ContactSetFixedHorizonProvider{
 fn document(v:&Value)->CaeResult<Document>{
  let o=v.as_object().ok_or_else(||fail("contact-set optimizer document"))?;
  if o.keys().any(|k|!["schema","problem","objective","constraints","derivative_route","contact_design_scope","contact_coverage_scope"].contains(&k.as_str()))||v["schema"]!="implexity-contact-set-fixed-horizon-optimizer/1"||v["contact_design_scope"]!="fixed_reference_material"||v["contact_coverage_scope"]!="selected_feature_diagnostic"{return Err(fail("contact-set optimizer requires explicit fixed-reference material scope; full surface topology route not qualified"));}
  let restricted=match v["derivative_route"].as_str(){Some("ordinary_fixed_branch")=>false,Some("restricted_unloaded_initial_design")=>true,_=>return Err(fail("contact-set optimizer derivative route"))};
  let pair=ContactSetDocument::from_value(&v["problem"])?.build()?;
  if pair.schedule.clock()!=SegmentClock::NativeTickEnd{return Err(fail("checkpoint macro provider requires native tick-end quadrature"));}
  match &pair.document.initial{InitialDocument::NativeRest{origin_s,selected}if *origin_s==0.&&selected.iter().all(|v|!*v)=>{},_=>return Err(fail("contact-set optimizer requires authenticated native rest; external checkpoint intake unavailable"))}
  if pair.document.macro_steps!=pair.model.native_body(0)?.problem.time.history_steps(){return Err(fail("contact-set optimizer must retain complete authored horizon"));}
  if pair.contacts.iter().flat_map(|c|c.features.iter()).any(|f|!matches!(f,crate::moving_contact::surface_map::SurfaceFeature::Vertex(_)|crate::moving_contact::surface_map::SurfaceFeature::FixedTetrahedron{..})){return Err(fail("fixed-reference material route excludes moving density/feature topology"));}
  let program=implexity_solve::dynamic_program::normalise(&pair.document.responses)?;let responses=program.responses();
  let objective=v["objective"].as_str().ok_or_else(||fail("contact-set objective response name"))?;
  if !responses.iter().any(|n|n==objective){return Err(fail("contact-set objective absent from complete DynamicProgram"));}
  for c in v["constraints"].as_array().ok_or_else(||fail("contact-set constraint response names"))?{if !c.as_str().is_some_and(|s|responses.iter().any(|n|n==s)){return Err(fail("contact-set constraint absent from complete DynamicProgram"));}}
  let mut names=Vec::new();for i in 0..2{names.extend(pair.model.native_body(i)?.solid_sample_name_metadata()?.into_iter().map(|n|format!("body{i}:{n}")));}let common=&pair.model.native_body(0)?.problem.observables;names.extend(common.fluid.iter().map(|(n,_)|n.clone()));names.extend(common.interface.iter().cloned());
  let projection=Self::projection(&pair,names)?;let bound=program.bind(projection.names(),&["design_volume_fraction","removed_volume_fraction","removed_volume_m3","removal_depth_m"])?;
  bound.admit(pair.document.macro_steps,false,pair.model.fixed_horizon_autonomous()?)?;
  Ok(Document{pair,program,restricted})
 }
 fn projection(pair:&ContactSetProblem,names:Vec<String>)->CaeResult<PairSampleProjection>{from_observables(&pair.document.observables,[&pair.document.bodies[0]["observables"],&pair.document.bodies[1]["observables"]],names)}
 fn calculate(&self,p:&ProviderProblem,d:&NamedArrays,point:usize,gradient:bool)->CaeResult<Cached>{
  if point!=0{return Err(fail("contact-set provider has one operating point"));}
  let v=value(p)?;let key=format!("{}:{}",implexity_core::json::canonical_sha256(v),design_identity(d)?);
  let mut cache=self.cache.lock().map_err(|_|fail("contact-set history cache lock"))?;
  if let Some(c)=cache.as_ref().filter(|c|c.key==key&&(!gradient||c.gradients.is_some())){return Ok(c.clone());}
  let document=Self::document(v)?;let pair=&document.pair;
  if d.names().len()!=2||COORDS.iter().any(|n|!d.contains(n)){return Err(fail("contact-set provider needs exactly both native body controls"));}
  let mut controls=Vec::new();for i in 0..2{let a=d.get(COORDS[i]).ok_or_else(||fail("contact-set native coordinate absent"))?;if a.shape()!=pair.model.native_body(i)?.problem.solid.grid.shape{return Err(fail("contact-set coordinate native voxel shape differs"));}controls.push(a.iter().copied().collect::<Vec<_>>());}
  let design=pair.model.event_physical_design([&controls[0],&controls[1]])?;
  let resources=PairNativeResources::new(&pair.model,pair.event)?;
  let velocity=native_velocity_trace_from_pair(&pair.model,&resources.solid,&design,1.,pair.contacts.len())?;
  let fields=resources.build_contact(|solid|multiple_contact_law(&pair.model,solid,&pair.contacts)?.with_residual_path_allowance(pair.onset.residual_tolerance))?;
  let ratio=pair.model.native_body(0)?.problem.time.macro_step_s()/fields.fluid.inner().nominal_fluid_step_s();
  if !ratio.is_finite()||ratio<1.||ratio.round()>usize::MAX as f64||(ratio-ratio.round()).abs()>32.*f64::EPSILON*ratio{return Err(fail("contact-set native fluid cadence is not an integral authored ratio"));}
  let stepper=CheckpointMacroStepper::from_rest(&pair.model,&fields,&design,&pair.document.external_force_n,&serde_json::to_value(&pair.document.contacts).map_err(|e|fail(&e.to_string()))?,&pair.document.source_graph_sha256,velocity,pair.onset,pair.schedule.subdivisions(),ratio.round()as usize)?;
  let projection=Self::projection(pair,stepper.sample_names().to_vec())?;
  let mut out=if let Some(c)=cache.as_ref().filter(|c|c.key==key){stepper.adopt_archive(&c.archive,&c.run)?;c.clone()}else{
   let run=Arc::new(stepper.run()?);let archive=stepper.archive()?;
   let responses=stepper.evaluate_program(&run,&projection,&document.program)?;
   Cached{key:key.clone(),run,archive,responses,gradients:None}
  };
  *cache=Some(out.clone());
  if gradient{let values=if document.restricted{stepper.controls_restricted_unloaded_initial_design([&controls[0],&controls[1]],&projection,&document.program,&out.run)?.into_iter().map(|g|(g.name,g.value,g.bodies)).collect::<Vec<_>>()}else{stepper.controls([&controls[0],&controls[1]],&projection,&document.program,&out.run)?.into_iter().map(|g|(g.name,g.value,g.bodies)).collect::<Vec<_>>()};let mut gradients=BTreeMap::new();for(name,x,bodies)in values{if out.responses.get(&name).map(|r|r.to_bits())!=Some(x.to_bits()){return Err(fail("contact-set forward/reverse objective value differs"));}let mut arrays=NamedArrays::new();for(i,g)in bodies.into_iter().enumerate(){let shape=pair.model.native_body(i)?.problem.solid.grid.shape;arrays.insert(COORDS[i],ArrayD::from_shape_vec(IxDyn(&shape),g).map_err(|e|fail(&e.to_string()))?);}gradients.insert(name,arrays);}out.gradients=Some(gradients);}
  *cache=Some(out.clone());Ok(out)
 }
}
impl CaeProvider for ContactSetFixedHorizonProvider{
 fn name(&self)->&str{NAME}fn implementation(&self)->&str{"implexity_physics_fsi::moving_contact::macro_control::provider::ContactSetFixedHorizonProvider"}
 fn capabilities(&self)->CaeResult<ProviderCapabilities>{let mut d=ProviderDescriptor::new(NAME,vec![NAME.into()],vec![]);d.nonlinear=true;d.sensitivities=true;d.design_coordinates=COORDS.iter().map(|n|(*n).into()).collect();d.traits=diagnostics();Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(json!({"kind":"native_json","title":"Moving contact-set fixed-horizon dynamics","schema":editor_schema()}),"array")?)))}
 fn normalise_problem(&self,p:&Value)->CaeResult<ProviderProblem>{let _=Self::document(p)?;Ok(Arc::new(p.clone()))}
 fn preflight(&self,p:&ProviderProblem,_:Option<&ArrayD<f64>>)->CaeResult<Map<String,Value>>{let _=Self::document(value(p)?)?;Ok(diagnostics())}
 fn evaluate(&self,_:&ProviderProblem,_:&ArrayD<f64>)->CaeResult<Evaluation>{Err(fail("contact-set provider needs named native body controls"))}
 fn sensitivity(&self,_:&ProviderProblem,_:&ArrayD<f64>,_:&str)->CaeResult<Sensitivity>{Err(fail("contact-set provider needs named native body controls"))}
 fn interface(&self,n:&str)->Option<&(dyn Any+Send+Sync)>{implexity_optim::provider_ops::design_interface::<Self>(n)}fn as_any(&self)->&dyn Any{self}
}
impl DesignOperations for ContactSetFixedHorizonProvider{
 fn provides(&self,o:DesignOp)->bool{matches!(o,DesignOp::EvaluateDesign|DesignOp::SensitivityDesign|DesignOp::SensitivitiesDesign|DesignOp::OptimizerLifecycle)}
 fn problem_responses(&self,p:&ProviderProblem)->Option<CaeResult<Vec<String>>>{Some(value(p).and_then(|v|Self::document(v).map(|d|d.program.responses())))}
 fn evaluate_design(&self,p:&ProviderProblem,d:&NamedArrays,o:usize)->CaeResult<Evaluation>{let c=self.calculate(p,d,o,false)?;Ok(Evaluation{provider:NAME.into(),responses:c.responses,diagnostics:diagnostics(),fields:BTreeMap::new()})}
 fn sensitivity_design(&self,p:&ProviderProblem,d:&NamedArrays,n:&str,o:usize)->CaeResult<DesignSensitivity>{let c=self.calculate(p,d,o,true)?;Ok(DesignSensitivity{value:*c.responses.get(n).ok_or_else(||fail("contact-set response absent"))?,gradients:c.gradients.as_ref().and_then(|g|g.get(n)).ok_or_else(||fail("contact-set response gradient absent"))?.clone(),diagnostics:diagnostics()})}
 fn sensitivities_design(&self,p:&ProviderProblem,d:&NamedArrays,n:&[String],o:usize)->CaeResult<DesignSensitivities>{let c=self.calculate(p,d,o,true)?;let mut responses=BTreeMap::new();let mut gradients=BTreeMap::new();for name in n{responses.insert(name.clone(),*c.responses.get(name).ok_or_else(||fail("contact-set response absent"))?);gradients.insert(name.clone(),c.gradients.as_ref().and_then(|g|g.get(name)).ok_or_else(||fail("contact-set response gradient absent"))?.clone());}Ok(DesignSensitivities{responses,gradients,diagnostics:diagnostics()})}
 fn optimizer_lifecycle(&self,_:Option<&ProviderProblem>)->CaeResult<LifecycleDeclaration>{Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(COORDS.iter().map(|n|(*n).into()).collect(),"sensitivity_design","evaluate_design",None,None,false,false)?))}
}
pub fn design_coordinate_shape(v:&Value,coordinate:&str)->CaeResult<Option<Vec<usize>>>{let d=ContactSetFixedHorizonProvider::document(v)?;if let Some(i)=COORDS.iter().position(|n|*n==coordinate){Ok(Some(d.pair.model.native_body(i)?.problem.solid.grid.shape.to_vec()))}else{Ok(None)}}
pub fn install(ctx:&InstallContext<'_>)->CaeResult<()>{ctx.register_provider(Arc::new(ContactSetFixedHorizonProvider::default())).map(|_|())}

pub fn editor_schema()->Value{json!({"type":"object","additionalProperties":false,"required":["schema","problem","objective","constraints","derivative_route","contact_design_scope","contact_coverage_scope"],"properties":{"schema":{"type":"string","enum":["implexity-contact-set-fixed-horizon-optimizer/1"]},"problem":crate::moving_contact::phase_problem::editor_schema(),"objective":{"type":"string"},"constraints":{"type":"array","items":{"type":"string"}},"derivative_route":{"type":"string","enum":["ordinary_fixed_branch","restricted_unloaded_initial_design"]},"contact_design_scope":{"type":"string","enum":["fixed_reference_material"]},"contact_coverage_scope":{"type":"string","enum":["selected_feature_diagnostic"]}},"x-qualification":diagnostics()})}

pub fn prepare_spatial_surface_binding(v:&Value,d:&NamedArrays,policy:crate::moving_contact::boundary_law::exposed_surface::IsoSurfacePolicy)->CaeResult<crate::moving_contact::native_surface_binding::NativeSurfaceBinding>{let document=ContactSetFixedHorizonProvider::document(v)?;let pair=&document.pair;if d.names().len()!=2||COORDS.iter().any(|n|!d.contains(n)){return Err(fail("spatial surface requires both native controls"));}let mut controls=Vec::new();for i in 0..2{let a=d.get(COORDS[i]).ok_or_else(||fail("spatial surface coordinate absent"))?;if a.shape()!=pair.model.native_body(i)?.problem.solid.grid.shape{return Err(fail("spatial surface coordinate native voxel shape"));}controls.push(a.iter().copied().collect::<Vec<_>>());}let design=pair.model.event_physical_design([&controls[0],&controls[1]])?;let solid=pair.model.solid_field()?;crate::moving_contact::native_surface_binding::NativeSurfaceBinding::new(&pair.model,&solid,&design,policy)}
