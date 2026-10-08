// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_solve::dynamic_program::{DynamicProgram,TermQuantity};
use super::{separate_body::{MovingFsiPairModel,MovingEventHistory,MovingEventHistoryBars},event_step::{EventAdvancePolicy,EventTickStateDirection},sample_projection::PairSampleProjection,event_response::EventResponseGradient};
const KINDS:[&str;4]=["design_volume_fraction","removed_volume_fraction","removed_volume_m3","removal_depth_m"];
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct EventControlGradient{pub name:String,pub value:f64,pub bodies:[Vec<f64>;2],pub initial_clock_s:f64,pub restitution:f64}
impl MovingFsiPairModel{
 pub(crate) fn fixed_horizon_autonomous(&self)->CaeResult<bool>{let mut flags=Vec::new();for i in 0..2{match self.native_body(i)?.problem.time.kind{crate::problem::time::TimeKind::FixedHorizon{autonomous,..}=>flags.push(autonomous),_=>return Err(fail("event optimizer requires both native bodies fixed_horizon"))}}if flags[0]!=flags[1]{return Err(fail("event native bodies autonomous declaration differs"));}Ok(flags[0])}

 pub fn event_physical_design(&self,controls:[&[f64];2])->CaeResult<Vec<f64>>{
  let mut out=Vec::new();for(i,x)in controls.into_iter().enumerate(){let m=self.native_body(i)?;if x.len()!=m.chain.len()||x.iter().any(|v|!v.is_finite()){return Err(fail("event control coordinate shape"));}out.extend(m.chain.forward(x)?);}Ok(out)
 }
 pub fn event_design_terms(&self,design:&[f64],program:&DynamicProgram)->CaeResult<Vec<(String,f64,Vec<f64>)>>{
  let n0=self.native_body(0)?.chain.len();let n1=self.native_body(1)?.chain.len();if design.len()!=n0+n1||design.iter().any(|x|!x.is_finite()){return Err(fail("event physical design shape"));}
  let mut out=Vec::new();for term in program.design_terms(){let TermQuantity::Design{kind,spec}=&term.quantity else{unreachable!()};if !KINDS.contains(&kind.as_str()){return Err(fail("event design functional unsupported"));}
   let owner=match spec.get("body"){Some(v)=>Some(v.as_u64().filter(|&i|i<2).ok_or_else(||fail("event design functional body must be zero or one"))? as usize),None=>None};let mut g=vec![0.;design.len()];
   let value=if kind=="design_volume_fraction"&&owner.is_none(){design.iter().sum::<f64>()/design.len() as f64}else{let i=owner.ok_or_else(||fail("event removal functional requires explicit body"))?;let range=if i==0{0..n0}else{n0..n0+n1};let rho=&design[range.clone()];let(value,local)=if kind=="design_volume_fraction"{(rho.iter().sum::<f64>()/rho.len() as f64,vec![1./rho.len() as f64;rho.len()])}else{self.native_body(i)?.problem.design.removal.as_ref().ok_or_else(||fail("event removal functional requires native removal-only body"))?.term(kind,rho)?};g[range].copy_from_slice(&local);value};
   if kind=="design_volume_fraction"&&owner.is_none(){let h0=self.native_body(0)?.problem.solid.grid.element_size_m;let h1=self.native_body(1)?.problem.solid.grid.element_size_m;if h0.to_bits()!=h1.to_bits(){return Err(fail("joint voxel fraction requires equal physical voxel sizes"));}g.fill(1./design.len() as f64);}
   if !value.is_finite()||g.iter().any(|x|!x.is_finite()){return Err(fail("event design functional overflow"));}out.push((term.name.clone(),value,g));
  }Ok(out)
 }
 pub fn event_history_optimizer_gradients(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,projection:&PairSampleProjection,program:&DynamicProgram)->CaeResult<Vec<EventResponseGradient>>{
  let series=self.event_history_native_samples(design,p,history)?;if projection.source_names()!=series.names{return Err(fail("event optimizer projection layout differs"));}
  let bound=program.bind(projection.names(),&KINDS)?;bound.admit(series.samples.nrows,false,self.fixed_horizon_autonomous()?)?;
  let mut values=implexity_linalg::dense::DenseMatrix::zeros(series.samples.nrows,projection.names().len());for r in 0..values.nrows{let y=projection.values(&series.samples.data[r*series.samples.ncols..(r+1)*series.samples.ncols])?;let width=values.ncols;values.data[r*width..(r+1)*width].copy_from_slice(&y);}
  let mut out=Vec::new();for(name,v)in bound.evaluate(&values,series.macro_step_s,false,self.fixed_horizon_autonomous()?)?{if v.d_period_s!=0.{return Err(fail("event fixed-grid period derivative unavailable"));}let mut b=implexity_linalg::dense::DenseMatrix::zeros(series.samples.nrows,series.samples.ncols);for r in 0..values.nrows{let x=projection.pullback(&v.d_samples.data[r*values.ncols..(r+1)*values.ncols])?;let width=b.ncols;b.data[r*width..(r+1)*width].copy_from_slice(&x);}let bars=self.event_history_sample_adjoint(design,external,p,history,&b)?;out.push(EventResponseGradient{name,value:v.value,bars});}
  for(name,value,g)in self.event_design_terms(design,&bound)?{let i=&history.initial;let bars=MovingEventHistoryBars{initial:EventTickStateDirection{solid:vec![0.;i.solid.len()],fluid:vec![0.;i.fluid.len()],lagged_force_n:vec![0.;i.lagged_force_n.len()],origin_s:0.},design:g,external_force_n:external.iter().map(|f|vec![0.;f.len()]).collect(),restitution:0.};out.push(EventResponseGradient{name,value,bars});}Ok(out)
 }
 pub fn event_history_control_gradients(&self,controls:[&[f64];2],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,projection:&PairSampleProjection,program:&DynamicProgram)->CaeResult<Vec<EventControlGradient>>{
  let design=self.event_physical_design(controls)?;let n=self.native_body(0)?.chain.len();let mut out=Vec::new();for v in self.event_history_optimizer_gradients(&design,external,p,history,projection,program)?{let a=self.native_body(0)?.chain.pullback(controls[0],&v.bars.design[..n])?;let b=self.native_body(1)?.chain.pullback(controls[1],&v.bars.design[n..])?;out.push(EventControlGradient{name:v.name,value:v.value,bodies:[a,b],initial_clock_s:v.bars.initial.origin_s,restitution:v.bars.restitution});}Ok(out)
 }
}

impl MovingFsiPairModel{
 pub fn event_history_from_rest_control_gradients(&self,controls:[&[f64];2],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,projection:&PairSampleProjection,program:&DynamicProgram)->CaeResult<Vec<EventControlGradient>>{
  use implexity_solve::multirate_coupling::{FluxDrivenField,SubcycledField};
  let design=self.event_physical_design(controls)?;let solid=self.contact_field()?;let fluid=self.event_fluid_field(p)?;let si=solid.initial_state(&design)?;let fi=fluid.initial_state(&design)?;
  if si.len()!=history.initial.solid.len()||fi.len()!=history.initial.fluid.len()||si.iter().zip(&history.initial.solid).any(|(a,b)|a.to_bits()!=b.to_bits())||fi.iter().zip(&history.initial.fluid).any(|(a,b)|a.to_bits()!=b.to_bits())||history.initial.lagged_force_n.iter().any(|x|*x!=0.){return Err(fail("event from-rest history initial state differs from native owner"));}
  let n=self.native_body(0)?.chain.len();let mut out=Vec::new();for mut v in self.event_history_optimizer_gradients(&design,external,p,history,projection,program)?{let s=solid.initial_state_vjp(&design,&v.bars.initial.solid)?;let f=fluid.initial_state_vjp(&design,&v.bars.initial.fluid)?;for(i,g)in v.bars.design.iter_mut().enumerate(){*g+=s[i]+f[i];}let a=self.native_body(0)?.chain.pullback(controls[0],&v.bars.design[..n])?;let b=self.native_body(1)?.chain.pullback(controls[1],&v.bars.design[n..])?;out.push(EventControlGradient{name:v.name,value:v.value,bodies:[a,b],initial_clock_s:v.bars.initial.origin_s,restitution:v.bars.restitution});}Ok(out)
 }
}
