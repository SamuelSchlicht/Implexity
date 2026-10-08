// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::{any::Any,collections::BTreeMap,sync::{Arc,Mutex}};
use ndarray::{ArrayD,IxDyn};
use serde_json::{Value,Map,json};
use implexity_core::{CaeError,CaeResult,contracts::{CaeProvider,ProviderProblem,ProviderCapabilities,ProviderDescriptor,Evaluation,Sensitivity},packages::InstallContext};
use implexity_optim::{design::{NamedArrays,design_identity},provider_ops::{DesignOperations,DesignOp,DesignSensitivities,DesignSensitivity,LifecycleDeclaration},optimizer::OptimizerLifecycleConfig};
use implexity_solve::multirate_coupling::{FluxDrivenField,SubcycledField};
use super::{pair_problem::PairProblemDocument,sample_projection::from_observables,event_macro::EventMacroStepper};
const NAME:&str="moving_fsi_pair_event_history";
const COORDS:[&str;2]=["body0:model:control","body1:model:control"];
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn value(p:&ProviderProblem)->CaeResult<&Value>{p.downcast_ref::<Value>().ok_or_else(||fail("event provider problem type"))}
fn diagnostics()->Map<String,Value>{json!({"physical_qualification":false,"derivative_scope":"fixed_feature_transverse_event_discrete_history","autonomous_orbit_qualified":false,"nominal_grid_time_derivative_available":false,"memory_cache":"one exact document and named design","full_history_growth_certificate_available":false}).as_object().unwrap().clone()}
#[derive(Clone)]
struct Cached{key:String,responses:BTreeMap<String,f64>,gradients:BTreeMap<String,NamedArrays>,clock:BTreeMap<String,f64>}
#[derive(Default)]
pub struct PairEventHistoryProvider{cache:Mutex<Option<Cached>>}
impl PairEventHistoryProvider{
 fn document(v:&Value)->CaeResult<(super::pair_problem::PairProblemProvider,implexity_solve::dynamic_program::DynamicProgram,usize,f64)>{
  let o=v.as_object().ok_or_else(||fail("event optimizer document"))?;if o.keys().any(|k|!["schema","pair","observables","responses","macro_steps","origin_s"].contains(&k.as_str()))||v["schema"]!="implexity-moving-fsi-event-optimizer/1"{return Err(fail("event optimizer schema"));}
  let pair=PairProblemDocument::from_json(&serde_json::to_vec(&v["pair"]).map_err(|e|fail(&e.to_string()))?)?.build()?;let n=v["macro_steps"].as_u64().filter(|n|*n>0).ok_or_else(||fail("event macro steps"))? as usize;let origin=v["origin_s"].as_f64().filter(|x|x.is_finite()&&*x>=0.).ok_or_else(||fail("event absolute clock"))?;
  if n>pair.model.native_body(0)?.problem.time.history_steps(){return Err(fail("event requested history exceeds authored horizon"));}let program=implexity_solve::dynamic_program::normalise(&v["responses"])?;let field=pair.model.solid_field()?;let fluid=pair.model.event_fluid_field(pair.event)?;let mut names=field.sample_names().to_vec();names.extend(fluid.sample_names().iter().cloned());let projection=from_observables(&v["observables"],[&v["pair"]["bodies"][0]["observables"],&v["pair"]["bodies"][1]["observables"]],names)?;let bound=program.bind(projection.names(),&["design_volume_fraction","removed_volume_fraction","removed_volume_m3","removal_depth_m"])?;bound.admit(n,false,pair.model.fixed_horizon_autonomous()?)?;let count=pair.model.native_body(0)?.chain.len()+pair.model.native_body(1)?.chain.len();let _=pair.model.event_design_terms(&vec![0.;count],&bound)?;Ok((pair,program,n,origin))
 }
 fn solve(&self,problem:&ProviderProblem,design:&NamedArrays,point:usize)->CaeResult<Cached>{
  if point!=0{return Err(fail("event provider declares one operating point"));}let v=value(problem)?;let key=format!("{}:{}",implexity_core::json::canonical_sha256(v),design_identity(design)?);let mut cache=self.cache.lock().map_err(|_|fail("event cache mutex"))?;if let Some(c)=cache.as_ref().filter(|c|c.key==key){return Ok(c.clone());}
  let(pair,program,steps,origin)=Self::document(v)?;if design.names().len()!=2||COORDS.iter().any(|n|!design.contains(n)){return Err(fail("event provider needs exactly two native body coordinates"));}
  let mut controls=vec![];for i in 0..2{let a=design.get(COORDS[i]).ok_or_else(||fail("event coordinate missing"))?;let m=pair.model.native_body(i)?;if a.shape()!=m.problem.solid.grid.shape{return Err(fail("event coordinate must match native body voxel shape"));}controls.push(a.iter().copied().collect::<Vec<_>>());}
  let rho=pair.model.event_physical_design([&controls[0],&controls[1]])?;let initial=pair.initial_state(&rho,origin)?;let field=pair.model.solid_field()?;let fluid=pair.model.event_fluid_field(pair.event)?;let mut names=field.sample_names().to_vec();names.extend(fluid.sample_names().iter().cloned());let projection=from_observables(&v["observables"],[&v["pair"]["bodies"][0]["observables"],&v["pair"]["bodies"][1]["observables"]],names)?;
  use implexity_solve::{time_stepper::{TimeStepper,StepParameters},checkpointed_history::{run_history,history_adjoint_many},state_store::StoreBudget};
  let stepper=EventMacroStepper::new(&pair.model,pair.event,projection,origin)?;let packed=stepper.pack(&initial)?;let parameters=StepParameters{design:&rho,time_scale:1.};let run=run_history(&stepper,parameters,&packed,1,Some(steps),None,pair.model.native_body(0)?.problem.time.checkpoint.clone(),&StoreBudget::from_environment()?)?;
  let bound=program.bind(stepper.sample_names(),&["design_volume_fraction","removed_volume_fraction","removed_volume_m3","removal_depth_m"])?;let dynamic=bound.evaluate(run.samples(),stepper.nominal_step_s(),false,pair.model.fixed_horizon_autonomous()?)?;if dynamic.iter().any(|(_,v)|v.d_period_s!=0.){return Err(fail("event fixed-grid functional period derivative unavailable"));}let bars:Vec<_>=dynamic.iter().map(|(_,v)|v.d_samples.clone()).collect();let final_bars=vec![vec![0.;stepper.state_size()];bars.len()];let gradients=history_adjoint_many(&stepper,parameters,&run,&bars,&final_bars)?;
  let mut physical=vec![];for((name,v),mut g)in dynamic.into_iter().zip(gradients){let init=stepper.initial_state_vjp(&rho,&g.initial_state)?;for(a,b)in g.design.iter_mut().zip(init){*a+=b;}let clock=*g.initial_state.last().ok_or_else(||fail("event initial clock cotangent absent"))?;physical.push((name,v.value,g.design,clock));}for(name,value,g)in pair.model.event_design_terms(&rho,&bound)?{physical.push((name,value,g,0.));}
  let mut out=Cached{key,responses:BTreeMap::new(),gradients:BTreeMap::new(),clock:BTreeMap::new()};let n0=controls[0].len();for(name,value,g,initial_clock_s)in physical{let bodies=[pair.model.native_body(0)?.chain.pullback(&controls[0],&g[..n0])?,pair.model.native_body(1)?.chain.pullback(&controls[1],&g[n0..])?];let mut arrays=NamedArrays::new();for(i,g)in bodies.into_iter().enumerate(){let shape=pair.model.native_body(i)?.problem.solid.grid.shape;arrays.insert(COORDS[i].to_string(),ArrayD::from_shape_vec(IxDyn(&shape),g).map_err(|e|fail(&e.to_string()))?);}out.responses.insert(name.clone(),value);out.clock.insert(name.clone(),initial_clock_s);out.gradients.insert(name,arrays);}*cache=Some(out.clone());Ok(out)
 }
}
impl CaeProvider for PairEventHistoryProvider{
 fn name(&self)->&str{NAME}fn implementation(&self)->&str{"implexity_physics_fsi::moving_contact::event_provider::PairEventHistoryProvider"}
 fn capabilities(&self)->CaeResult<ProviderCapabilities>{let mut d=ProviderDescriptor::new(NAME,vec![NAME.to_string()],vec![]);d.nonlinear=true;d.sensitivities=true;d.design_coordinates=COORDS.iter().map(|s|s.to_string()).collect();d.traits=diagnostics();Ok(ProviderCapabilities::Descriptor(Box::new(d)))}
 fn normalise_problem(&self,p:&Value)->CaeResult<ProviderProblem>{let _=Self::document(p)?;Ok(Arc::new(p.clone()))}
 fn preflight(&self,p:&ProviderProblem,_:Option<&ArrayD<f64>>)->CaeResult<Map<String,Value>>{let _=Self::document(value(p)?)?;Ok(diagnostics())}
 fn evaluate(&self,_:&ProviderProblem,_:&ArrayD<f64>)->CaeResult<Evaluation>{Err(fail("event provider requires named native body coordinates"))}
 fn sensitivity(&self,_:&ProviderProblem,_:&ArrayD<f64>,_:&str)->CaeResult<Sensitivity>{Err(fail("event provider requires named native body coordinates"))}
 fn interface(&self,n:&str)->Option<&(dyn Any+Send+Sync)>{implexity_optim::provider_ops::design_interface::<Self>(n)}fn as_any(&self)->&dyn Any{self}
}
impl DesignOperations for PairEventHistoryProvider{
 fn provides(&self,o:DesignOp)->bool{matches!(o,DesignOp::EvaluateDesign|DesignOp::SensitivityDesign|DesignOp::SensitivitiesDesign|DesignOp::OptimizerLifecycle)}
 fn problem_responses(&self,p:&ProviderProblem)->Option<CaeResult<Vec<String>>>{Some(value(p).and_then(|v|Self::document(v).map(|(_,q,_,_)|q.responses())))}
 fn evaluate_design(&self,p:&ProviderProblem,d:&NamedArrays,o:usize)->CaeResult<Evaluation>{let r=self.solve(p,d,o)?;Ok(Evaluation{provider:NAME.into(),responses:r.responses,diagnostics:diagnostics(),fields:BTreeMap::new()})}
 fn sensitivity_design(&self,p:&ProviderProblem,d:&NamedArrays,n:&str,o:usize)->CaeResult<DesignSensitivity>{let mut r=self.solve(p,d,o)?;Ok(DesignSensitivity{value:*r.responses.get(n).ok_or_else(||fail("event response unknown"))?,gradients:r.gradients.remove(n).ok_or_else(||fail("event response gradient unknown"))?,diagnostics:diagnostics()})}
 fn sensitivities_design(&self,p:&ProviderProblem,d:&NamedArrays,n:&[String],o:usize)->CaeResult<DesignSensitivities>{let r=self.solve(p,d,o)?;let mut responses=BTreeMap::new();let mut gradients=BTreeMap::new();for name in n{responses.insert(name.clone(),*r.responses.get(name).ok_or_else(||fail("event response unknown"))?);gradients.insert(name.clone(),r.gradients.get(name).ok_or_else(||fail("event response gradient unknown"))?.clone());}Ok(DesignSensitivities{responses,gradients,diagnostics:diagnostics()})}
 fn optimizer_lifecycle(&self,_:Option<&ProviderProblem>)->CaeResult<LifecycleDeclaration>{Ok(LifecycleDeclaration::Typed(OptimizerLifecycleConfig::new(COORDS.iter().map(|s|s.to_string()).collect(),"sensitivity_design","evaluate_design",None,None,false,false)?))}
}
pub fn install(ctx:&InstallContext<'_>)->CaeResult<()>{ctx.register_provider(Arc::new(PairEventHistoryProvider::default())).map(|_|())}
