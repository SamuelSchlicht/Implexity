// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::collections::BTreeMap;
use implexity_core::{CaeError,CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_solve::{checkpointed_history::{run_history,history_adjoint_many},state_store::StoreBudget,time_stepper::{TimeStepper,StepParameters}};
use serde::{Serialize,Deserialize};
use serde_json::Value;
use super::{closed_surface_problem::{ClosedSurfaceProblemDocument,ClosedSurfaceProblem},sample_projection::{from_observables,PairSampleProjection}};
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
pub const SCHEMA:&str="implexity-closed-surface-fsi-history/1";
const DESIGN_KINDS:[&str;4]=["design_volume_fraction","removed_volume_fraction","removed_volume_m3","removal_depth_m"];
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedSurfaceHistoryDocument{pub schema:String,pub problem:ClosedSurfaceProblemDocument,pub observables:Value,pub responses:Value,#[serde(default,skip_serializing_if="Option::is_none")]pub geometry_outputs:Option<[String;2]>}
pub struct ClosedSurfaceHistoryResponse{pub value:f64,pub control_gradients:Option<[Vec<f64>;2]>}
pub struct ClosedSurfaceHistory{pub responses:BTreeMap<String,ClosedSurfaceHistoryResponse>,pub samples:DenseMatrix,pub sample_names:Vec<String>,pub final_state:Vec<f64>,pub ledger:Vec<BTreeMap<String,f64>>,pub record:Value,pub coupling_admission:Value,pub problem_identity:String}
pub struct ClosedSurfaceHistoryProblem{pub problem:ClosedSurfaceProblem,pub document:ClosedSurfaceHistoryDocument}
fn projected(projection:&PairSampleProjection,source:&DenseMatrix)->CaeResult<DenseMatrix>{
 let mut samples=DenseMatrix::zeros(source.nrows,projection.names().len());
 for r in 0..source.nrows{let values=projection.values(&source.data[r*source.ncols..(r+1)*source.ncols])?;let width=samples.ncols;samples.data[r*width..(r+1)*width].copy_from_slice(&values);}Ok(samples)
}
fn source_bar(projection:&PairSampleProjection,bar:&DenseMatrix)->CaeResult<DenseMatrix>{
 if bar.ncols!=projection.names().len(){return Err(fail("closed history response sample cotangent"));}
 let mut result=DenseMatrix::zeros(bar.nrows,projection.source_names().len());
 for r in 0..bar.nrows{let values=projection.pullback(&bar.data[r*bar.ncols..(r+1)*bar.ncols])?;let width=result.ncols;result.data[r*width..(r+1)*width].copy_from_slice(&values);}Ok(result)
}
impl ClosedSurfaceHistoryDocument{
 pub fn from_json(bytes:&[u8])->CaeResult<Self>{serde_json::from_slice(bytes).map_err(fail)}
 pub fn build(self)->CaeResult<ClosedSurfaceHistoryProblem>{
  if self.schema!=SCHEMA{return Err(fail("closed surface history schema"));}
  if let Some(refs)=&self.geometry_outputs{if refs[0]==refs[1]||refs.iter().any(|v|v.is_empty()||v.trim()!=v){return Err(fail("closed surface geometry output refs must be distinct nonempty trimmed strings"));}}
  let problem=self.problem.clone().build()?;let autonomous=problem.model.fixed_horizon_autonomous()?;
  let time=&problem.model.native_body(0)?.problem.time;let other=&problem.model.native_body(1)?.problem.time;
  if time.history_steps()!=other.history_steps()||format!("{:?}",time.checkpoint)!=format!("{:?}",other.checkpoint){return Err(fail("closed surface history native horizon or checkpoint policies differ"));}
  let fields=problem.model.solid_field()?;let fluid=problem.model.fluid_field()?;
  use implexity_solve::multirate_coupling::{FluxDrivenField,SubcycledField};
  let mut names=fields.sample_names().to_vec();names.extend(fluid.sample_names().iter().cloned());
  let projection=from_observables(&self.observables,[&self.problem.bodies[0]["observables"],&self.problem.bodies[1]["observables"]],names)?;
  let program=projection.bind_program(&self.responses,&DESIGN_KINDS,time.history_steps(),false,autonomous)?;
  let count=problem.model.native_body(0)?.chain.len()+problem.model.native_body(1)?.chain.len();let _=problem.model.event_design_terms(&vec![0.;count],&program)?;
  Ok(ClosedSurfaceHistoryProblem{problem,document:self})
 }
}
impl ClosedSurfaceHistoryProblem{
 pub fn execute(&self,controls:[&[f64];2],budget:&StoreBudget,gradients:bool)->CaeResult<ClosedSurfaceHistory>{
  let design=self.problem.physical_design(controls)?;let stepper=self.problem.stepper(&design)?;
  let projection=from_observables(&self.document.observables,[&self.document.problem.bodies[0]["observables"],&self.document.problem.bodies[1]["observables"]],stepper.sample_names().to_vec())?;
  let time=&self.problem.model.native_body(0)?.problem.time;let autonomous=self.problem.model.fixed_horizon_autonomous()?;
  let program=projection.bind_program(&self.document.responses,&DESIGN_KINDS,time.history_steps(),false,autonomous)?;
  let coupling_admission=self.problem.admit_loose_coupling(&stepper,&design)?;
  let initial=stepper.initial_state(&design)?;let p=StepParameters{design:&design,time_scale:1.};
  let run=run_history(&stepper,p,&initial,1,Some(time.history_steps()),None,time.checkpoint.clone(),budget)?;
  let samples=projected(&projection,run.samples())?;let values=program.evaluate(&samples,stepper.nominal_step_s(),false,autonomous)?;
  let dynamic_gradients=if gradients{
   let bars:Vec<_>=values.iter().map(|(_,v)|source_bar(&projection,&v.d_samples)).collect::<CaeResult<_>>()?;
   let final_bars=vec![vec![0.;stepper.state_size()];bars.len()];
   Some(history_adjoint_many(&stepper,p,&run,&bars,&final_bars)?)
  }else{None};
  let split=self.problem.model.native_body(0)?.chain.len();
  let pullback=|g:&[f64]|->CaeResult<[Vec<f64>;2]>{
   if g.len()!=design.len(){return Err(fail("closed history physical design cotangent shape"));}
   Ok([self.problem.model.native_body(0)?.chain.pullback(controls[0],&g[..split])?,self.problem.model.native_body(1)?.chain.pullback(controls[1],&g[split..])?])
  };
  let mut responses=BTreeMap::new();
  for (i,(name,v)) in values.into_iter().enumerate(){
   let control_gradients=if let Some(all)=&dynamic_gradients{
    let mut g=all[i].design.clone();let initial_bar=stepper.initial_state_vjp(&design,&all[i].initial_state)?;
    if initial_bar.len()!=g.len(){return Err(fail("closed history initial design cotangent shape"));}for(a,b)in g.iter_mut().zip(initial_bar){*a+=b;}Some(pullback(&g)?)
   }else{None};
   if responses.insert(name,ClosedSurfaceHistoryResponse{value:v.value,control_gradients}).is_some(){return Err(fail("closed history response duplicated"));}
  }
  for(name,value,g)in self.problem.model.event_design_terms(&design,&program)?{
   let control_gradients=if gradients{Some(pullback(&g)?)}else{None};
   if responses.insert(name,ClosedSurfaceHistoryResponse{value,control_gradients}).is_some(){return Err(fail("closed history design response duplicated"));}
  }
  Ok(ClosedSurfaceHistory{responses,samples,sample_names:projection.names().to_vec(),final_state:run.final_state().to_vec(),ledger:run.ledger().to_vec(),record:run.record(),coupling_admission,problem_identity:self.problem.identity.clone()})
 }
}
