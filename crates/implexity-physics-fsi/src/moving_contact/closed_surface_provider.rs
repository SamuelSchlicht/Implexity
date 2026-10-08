// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::{any::Any,collections::BTreeMap,sync::{Arc,Mutex}};
use ndarray::{ArrayD,IxDyn};
use serde_json::{Value,Map,json};
use implexity_core::{CaeError,CaeResult,contracts::{CaeProvider,ProviderProblem,ProviderCapabilities,ProviderDescriptor,Evaluation,FieldValue,Sensitivity},packages::InstallContext};
use implexity_optim::{design::{NamedArrays,design_identity},provider_ops::{DesignOperations,DesignOp,DesignSensitivities,DesignSensitivity,LifecycleDeclaration},optimizer::OptimizerLifecycleConfig};
use implexity_solve::state_store::StoreBudget;
use super::closed_surface_history::{ClosedSurfaceHistoryDocument,ClosedSurfaceHistoryProblem};
pub const NAME:&str="moving_fsi_closed_surface_history";
const COORDS:[&str;2]=["body0:model:control","body1:model:control"];
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
fn normalized(p:&ProviderProblem)->CaeResult<&ClosedSurfaceHistoryProblem>{p.downcast_ref().ok_or_else(||fail("closed surface provider problem type"))}
fn diagnostics()->Map<String,Value>{json!({"physical_qualification":false,"whole_history_gradient_runtime_verified":false,"contact_scope":"symmetric full extracted boundary with closest-feature transfers","derivative_scope":"fixed native time grid and local differentiable surface feature strata","initial_state_owner":"native solid, fluid and lagged-force state","self_intersection_certificate_available":false,"checkpoint_owner":"generic checkpointed history"}).as_object().unwrap().clone()}
#[derive(Clone)]
struct Cached{key:String,evaluation:Evaluation,gradients:Option<BTreeMap<String,NamedArrays>>}
#[derive(Default)]
pub struct ClosedSurfaceHistoryProvider{cache:Mutex<Option<Cached>>}
impl ClosedSurfaceHistoryProvider{
 fn controls(p:&ClosedSurfaceHistoryProblem,d:&NamedArrays)->CaeResult<[Vec<f64>;2]>{
  if d.names().len()!=2||COORDS.iter().any(|n|!d.contains(n)){return Err(fail("closed surface provider requires both native named controls"));}
  let mut values=vec![];for i in 0..2{let a=d.get(COORDS[i]).ok_or_else(||fail("closed surface coordinate absent"))?;if a.shape()!=p.problem.model.native_body(i)?.problem.solid.grid.shape{return Err(fail("closed surface coordinate voxel shape"));}values.push(a.iter().copied().collect());}
  values.try_into().map_err(|_|fail("closed surface body count"))
 }
 fn solve(&self,p:&ProviderProblem,d:&NamedArrays,point:usize,need_gradients:bool)->CaeResult<Cached>{
  if point!=0{return Err(fail("closed surface provider has one operating point"));}let p=normalized(p)?;let key=format!("{}:{}",implexity_core::json::canonical_sha256(&serde_json::to_value(&p.document).map_err(fail)?),design_identity(d)?);
  let mut cache=self.cache.lock().map_err(|_|fail("closed surface provider cache lock"))?;
  if let Some(c)=cache.as_ref(){if c.key==key&&(!need_gradients||c.gradients.is_some()){return Ok(c.clone());}}
  let controls=Self::controls(p,d)?;let run=p.execute([&controls[0],&controls[1]],&StoreBudget::from_environment()?,need_gradients)?;
  let mut responses=BTreeMap::new();let mut gradients=BTreeMap::new();
  for(name,r)in run.responses{responses.insert(name.clone(),r.value);if need_gradients{let g=r.control_gradients.ok_or_else(||fail("closed surface response gradient absent"))?;let mut arrays=NamedArrays::new();for(i,g)in g.into_iter().enumerate(){arrays.insert(COORDS[i],ArrayD::from_shape_vec(IxDyn(&p.problem.model.native_body(i)?.problem.solid.grid.shape),g).map_err(fail)?);}gradients.insert(name,arrays);}}
  let mut fields=BTreeMap::new();for(col,name)in run.sample_names.iter().enumerate(){let values=(0..run.samples.nrows).map(|r|run.samples.data[r*run.samples.ncols+col]).collect();fields.insert(format!("history:{name}"),FieldValue::Array(ArrayD::from_shape_vec(IxDyn(&[run.samples.nrows]),values).map_err(fail)?));}
  fields.insert("native:final_packed_state".into(),FieldValue::Array(ArrayD::from_shape_vec(IxDyn(&[run.final_state.len()]),run.final_state).map_err(fail)?));
  let dt=p.problem.model.native_body(0)?.problem.time.macro_step_s();fields.insert("native:sample_time_s".into(),FieldValue::Array(ArrayD::from_shape_vec(IxDyn(&[run.samples.nrows]),(1..=run.samples.nrows).map(|i|i as f64*dt).collect()).map_err(fail)?));
  let mut info=diagnostics();info.insert("native_history".into(),run.record);info.insert("coupling_admission".into(),run.coupling_admission);info.insert("native_ledger".into(),serde_json::to_value(run.ledger).map_err(fail)?);info.insert("normalized_problem_identity".into(),json!(run.problem_identity));
  let out=Cached{key,evaluation:Evaluation{provider:NAME.into(),responses,diagnostics:info,fields},gradients:if need_gradients{Some(gradients)}else{None}};*cache=Some(out.clone());Ok(out)
 }
}
impl CaeProvider for ClosedSurfaceHistoryProvider{
 fn name(&self)->&str{NAME}
 fn implementation(&self)->&str{"implexity_physics_fsi::moving_contact::closed_surface_provider::ClosedSurfaceHistoryProvider"}
 fn capabilities(&self)->CaeResult<ProviderCapabilities>{let mut d=ProviderDescriptor::new(NAME,vec![NAME.into()],vec![]);d.nonlinear=true;d.sensitivities=true;d.design_coordinates=COORDS.iter().map(|s|(*s).into()).collect();d.traits=diagnostics();Ok(ProviderCapabilities::Descriptor(Box::new(d.with_presentation(json!({"kind":"native_json","title":"Two-body full-surface fluid–solid history","schema":editor_schema()}),"array")?)))}
 fn normalise_problem(&self,v:&Value)->CaeResult<ProviderProblem>{Ok(Arc::new(ClosedSurfaceHistoryDocument::from_json(&serde_json::to_vec(v).map_err(fail)?)?.build()?))}
 fn preflight(&self,p:&ProviderProblem,_:Option<&ArrayD<f64>>)->CaeResult<Map<String,Value>>{let _=normalized(p)?;Ok(diagnostics())}
 fn coupling_declaration(&self,problem:Option<&ProviderProblem>)->Option<CaeResult<Value>>{
  Some((||{
   use implexity_core::coupling_graph::{CouplingDeclaration,CouplingEdge,PhysicsPort};
   use implexity_solve::multirate_coupling::CouplingMode;
   let mut notes=vec!["Both bodies and their contact forces are assembled in the structural field.".into()];
   let mode=if let Some(problem)=problem{
    match &normalized(problem)?.problem.model.native_body(0)?.problem.coupling.mode{
     CouplingMode::StrongNewtonKrylov{..}=>"monolithic",
     CouplingMode::StrongQuasiNewton{..}=>"iterative",
     CouplingMode::Loose{..}=>{notes.push("Loose coupling uses lagged forces. The interface-work defect is recorded; a converged interface residual is not certified.".into());"iterative"}
    }
   }else{notes.push("The coupling mode is resolved from the normalized problem.".into());"iterative"};
   let edge=|a:&str,b:&str,q:&str,reason:&str|CouplingEdge::new(a,b,q,mode,true,reason).map_err(|e|fail(e.0));
   Ok(CouplingDeclaration{
    provider:NAME.into(),active_physics:vec!["flow".into(),"structure".into()],
    ports:vec![
     PhysicsPort{name:"interface_position".into(),owner:"flow".into(),direction:"input".into(),conserved:false,units:"m".into()},
     PhysicsPort{name:"interface_velocity".into(),owner:"flow".into(),direction:"input".into(),conserved:false,units:"m s-1".into()},
     PhysicsPort{name:"fsi_force".into(),owner:"structure".into(),direction:"input".into(),conserved:true,units:"N".into()},
    ],
    edges:vec![edge("flow","structure","pressure_and_shear_load","Fluid momentum exchange is transferred to both bodies through the native trace transpose.")?,edge("structure","flow","deformed_flow_domain","Both body positions and velocities determine moving occupancy and fluid momentum exchange.")?],
    closed_loops:vec![vec!["flow".into(),"structure".into()]],intentionally_frozen:vec![],notes,
   }.to_value())
  })())
 }

 fn evaluate(&self,_:&ProviderProblem,_:&ArrayD<f64>)->CaeResult<Evaluation>{Err(fail("closed surface provider requires named native body coordinates"))}
 fn sensitivity(&self,_:&ProviderProblem,_:&ArrayD<f64>,_:&str)->CaeResult<Sensitivity>{Err(fail("closed surface provider requires named native body coordinates"))}
 fn interface(&self,n:&str)->Option<&(dyn Any+Send+Sync)>{implexity_optim::provider_ops::design_interface::<Self>(n)}
 fn as_any(&self)->&dyn Any{self}
}
impl DesignOperations for ClosedSurfaceHistoryProvider{
 fn problem_document(&self,p:&ProviderProblem)->Option<CaeResult<Value>>{Some(normalized(p).and_then(|p|serde_json::to_value(&p.document).map_err(fail)))}
 fn provides(&self,o:DesignOp)->bool{matches!(o,DesignOp::EvaluateDesign|DesignOp::PreflightDesign|DesignOp::SensitivityDesign|DesignOp::SensitivitiesDesign|DesignOp::OptimizerLifecycle)}
 fn authoring_design_coordinates(&self,p:&ProviderProblem)->Option<CaeResult<Vec<String>>>{Some(normalized(p).map(|_|COORDS.iter().map(|n|(*n).into()).collect()))}
 fn problem_responses(&self,p:&ProviderProblem)->Option<CaeResult<Vec<String>>>{Some(normalized(p).and_then(|p|Ok(implexity_solve::dynamic_program::normalise(&p.document.responses)?.responses())))}
 fn preflight_design(&self,p:&ProviderProblem,d:&NamedArrays)->CaeResult<Map<String,Value>>{let p=normalized(p)?;let controls=Self::controls(p,d)?;let rho=p.problem.physical_design([&controls[0],&controls[1]])?;let _=p.problem.stepper(&rho)?;Ok(diagnostics())}
 fn evaluate_design(&self,p:&ProviderProblem,d:&NamedArrays,o:usize)->CaeResult<Evaluation>{Ok(self.solve(p,d,o,false)?.evaluation)}
 fn sensitivity_design(&self,p:&ProviderProblem,d:&NamedArrays,n:&str,o:usize)->CaeResult<DesignSensitivity>{let r=self.solve(p,d,o,true)?;let value=*r.evaluation.responses.get(n).ok_or_else(||fail("closed surface response unknown"))?;let gradients=r.gradients.ok_or_else(||fail("closed surface gradients absent"))?.remove(n).ok_or_else(||fail("closed surface response gradient unknown"))?;Ok(DesignSensitivity{value,gradients,diagnostics:r.evaluation.diagnostics})}
 fn sensitivities_design(&self,p:&ProviderProblem,d:&NamedArrays,n:&[String],o:usize)->CaeResult<DesignSensitivities>{let r=self.solve(p,d,o,true)?;let all=r.gradients.ok_or_else(||fail("closed surface gradients absent"))?;let mut responses=BTreeMap::new();let mut gradients=BTreeMap::new();for name in n{responses.insert(name.clone(),*r.evaluation.responses.get(name).ok_or_else(||fail("closed surface response unknown"))?);gradients.insert(name.clone(),all.get(name).ok_or_else(||fail("closed surface response gradient unknown"))?.clone());}Ok(DesignSensitivities{responses,gradients,diagnostics:r.evaluation.diagnostics})}
 fn optimizer_lifecycle(&self,_:Option<&ProviderProblem>)->CaeResult<LifecycleDeclaration>{Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(COORDS.iter().map(|s|(*s).into()).collect(),"sensitivity_design","evaluate_design",None,None,false,false)?))}
}
pub fn normalized_design_coordinate_shape(p:&ProviderProblem,coordinate:&str)->CaeResult<Option<Vec<usize>>>{
 let p=normalized(p)?;
 match COORDS.iter().position(|name|*name==coordinate){Some(i)=>Ok(Some(p.problem.model.native_body(i)?.problem.solid.grid.shape.to_vec())),None=>Ok(None)}
}
pub fn derived_geometry_enabled(p:&ProviderProblem)->bool{p.downcast_ref::<ClosedSurfaceHistoryProblem>().is_some_and(|p|p.document.geometry_outputs.is_some())}
pub fn derived_geometry_refs(p:&ProviderProblem)->CaeResult<Vec<String>>{normalized(p)?.document.geometry_outputs.as_ref().map(|v|v.to_vec()).ok_or_else(||fail("closed surface geometry outputs not configured"))}
pub fn derived_geometry_updates(p:&ProviderProblem,d:&BTreeMap<String,ArrayD<f64>>)->CaeResult<BTreeMap<String,ArrayD<f64>>>{
 let p=normalized(p)?;let refs=p.document.geometry_outputs.as_ref().ok_or_else(||fail("closed surface geometry outputs not configured"))?;
 let mut named=NamedArrays::new();for(name,a)in d{named.insert(name.clone(),a.clone());}let controls=ClosedSurfaceHistoryProvider::controls(p,&named)?;
 let mut outputs=BTreeMap::new();for i in 0..2{let body=p.problem.model.native_body(i)?;let rho=body.chain.forward(&controls[i])?;outputs.insert(refs[i].clone(),ArrayD::from_shape_vec(IxDyn(&body.problem.solid.grid.shape),rho).map_err(fail)?);}Ok(outputs)
}
pub fn install(ctx:&InstallContext<'_>)->CaeResult<()>{ctx.register_provider(Arc::new(ClosedSurfaceHistoryProvider::default())).map(|_|())}
pub fn editor_schema()->Value{
 let mut properties=Map::new();properties.insert("schema".into(),json!({"type":"string","enum":[super::closed_surface_history::SCHEMA]}));
 let fields=["stiffness_pa_per_m","transition_m","iso","contrast_min","interior_margin","minimum_area_ratio","minimum_parameter","tie_distance_m","minimum_distance_m","winding_tolerance"];
 let mut contact=Map::new();for name in fields{let mut value=json!({"type":"number","exclusiveMinimum":0});if name=="tie_distance_m"{value=json!({"type":"number","minimum":0});}if name=="iso"||name=="minimum_area_ratio"{value["exclusiveMaximum"]=json!(1.);}if name=="interior_margin"||name=="winding_tolerance"{value["exclusiveMaximum"]=json!(0.5);}if name=="minimum_parameter"{value["exclusiveMaximum"]=json!(1./3.);}if name=="stiffness_pa_per_m"{value["unit"]=json!("Pa/m");}if ["transition_m","tie_distance_m","minimum_distance_m"].contains(&name){value["unit"]=json!("m");}contact.insert(name.into(),value);}contact.insert("cap_domain".into(),json!({"type":"boolean"}));let mut required:Vec<_>=fields.iter().map(|v|(*v).to_string()).collect();required.push("cap_domain".into());
 properties.insert("problem".into(),json!({"type":"object","additionalProperties":false,"required":["schema","bodies","contact"],"properties":{"schema":{"type":"string","enum":[super::closed_surface_problem::SCHEMA]},"bodies":{"type":"array","minItems":2,"maxItems":2,"items":crate::editor::schema(None)},"contact":{"type":"object","additionalProperties":false,"required":required,"properties":contact}}}));
 properties.insert("geometry_outputs".into(),json!({"type":"array","minItems":2,"maxItems":2,"uniqueItems":true,"items":{"type":"string","minLength":1}}));
 properties.insert("observables".into(),json!({"type":"array"}));properties.insert("responses".into(),json!({"type":"object"}));json!({"type":"object","additionalProperties":false,"required":["schema","problem","observables","responses"],"properties":properties,"x-qualification":diagnostics()})
}
