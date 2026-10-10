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

use std::collections::BTreeMap;
use implexity_core::{CaeError,CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_solve::{checkpointed_history::history_adjoint_many,state_store::StoreBudget,time_stepper::{TimeStepper,StepParameters}};
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
  let design=self.problem.physical_design(controls)?;
  let stepper=self.problem.stepper(&design)?;
  execute_pair_history(
   &self.problem.model,&stepper,controls,&design,
   &self.document.observables,
   [&self.document.problem.bodies[0]["observables"],&self.document.problem.bodies[1]["observables"]],
   &self.document.responses,budget,gradients,||self.problem.admit_loose_coupling(&stepper,&design),self.problem.identity.clone(),
  )
 }
}

pub(crate) fn execute_pair_history<S:TimeStepper,A:FnOnce()->CaeResult<Value>>(
 model:&super::separate_body::MovingFsiPairModel,
 stepper:&S,
 controls:[&[f64];2],
 design:&[f64],
 observables:&Value,
 body_observables:[&Value;2],
 response_program:&Value,
 budget:&StoreBudget,
 gradients:bool,
 admission:A,
 problem_identity:String,
)->CaeResult<ClosedSurfaceHistory>{
 execute_pair_history_observed(model,stepper,controls,design,observables,body_observables,response_program,budget,gradients,admission,problem_identity,None)
}

pub(crate) fn execute_pair_history_observed<S:TimeStepper,A:FnOnce()->CaeResult<Value>>(
 model:&super::separate_body::MovingFsiPairModel,
 stepper:&S,
 controls:[&[f64];2],
 design:&[f64],
 observables:&Value,
 body_observables:[&Value;2],
 response_program:&Value,
 budget:&StoreBudget,
 gradients:bool,
 admission:A,
 problem_identity:String,
 observer:Option<&mut dyn implexity_solve::checkpointed_history::HistoryObserver>,
)->CaeResult<ClosedSurfaceHistory>{
  let projection=from_observables(observables,[body_observables[0],body_observables[1]],stepper.sample_names().to_vec())?;
  let time=&model.native_body(0)?.problem.time;let autonomous=model.fixed_horizon_autonomous()?;
  let program=projection.bind_program(response_program,&DESIGN_KINDS,time.history_steps(),false,autonomous)?;
  let coupling_admission=admission()?;
  let initial=stepper.initial_state(&design)?;let p=StepParameters{design:&design,time_scale:1.};
  let run=implexity_solve::checkpointed_history::run_history_observed(stepper,p,&initial,1,Some(time.history_steps()),None,time.checkpoint.clone(),budget,observer)?;
  let samples=projected(&projection,run.samples())?;let values=program.evaluate(&samples,stepper.nominal_step_s(),false,autonomous)?;
  let dynamic_gradients=if gradients{
   let bars:Vec<_>=values.iter().map(|(_,v)|source_bar(&projection,&v.d_samples)).collect::<CaeResult<_>>()?;
   let final_bars=vec![vec![0.;stepper.state_size()];bars.len()];
   Some(history_adjoint_many(stepper,p,&run,&bars,&final_bars)?)
  }else{None};
  let split=model.native_body(0)?.chain.len();
  let pullback=|g:&[f64]|->CaeResult<[Vec<f64>;2]>{
   if g.len()!=design.len(){return Err(fail("closed history physical design cotangent shape"));}
   Ok([model.native_body(0)?.chain.pullback(controls[0],&g[..split])?,model.native_body(1)?.chain.pullback(controls[1],&g[split..])?])
  };
  let mut responses=BTreeMap::new();
  for (i,(name,v)) in values.into_iter().enumerate(){
   let control_gradients=if let Some(all)=&dynamic_gradients{
    let mut g=all[i].design.clone();let initial_bar=stepper.initial_state_vjp(&design,&all[i].initial_state)?;
    if initial_bar.len()!=g.len(){return Err(fail("closed history initial design cotangent shape"));}for(a,b)in g.iter_mut().zip(initial_bar){*a+=b;}Some(pullback(&g)?)
   }else{None};
   if responses.insert(name,ClosedSurfaceHistoryResponse{value:v.value,control_gradients}).is_some(){return Err(fail("closed history response duplicated"));}
  }
  for(name,value,g)in model.event_design_terms(&design,&program)?{
   let control_gradients=if gradients{Some(pullback(&g)?)}else{None};
   if responses.insert(name,ClosedSurfaceHistoryResponse{value,control_gradients}).is_some(){return Err(fail("closed history design response duplicated"));}
  }
  Ok(ClosedSurfaceHistory{responses,samples,sample_names:projection.names().to_vec(),final_state:run.final_state().to_vec(),ledger:run.ledger().to_vec(),record:run.record(),coupling_admission,problem_identity:problem_identity})
}
