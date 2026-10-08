// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_solve::{multirate_coupling::{FluxDrivenField,SubcycledField},time_stepper::StepParameters,dynamic_program::DynamicProgram};
use super::{separate_body::{MovingFsiPairModel,MovingEventHistory,MovingEventTickCotangent,MovingEventHistoryBars},event_step::{EventAdvancePolicy,EventTickStateDirection}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct EventNativeSeries{pub names:Vec<String>,pub samples:DenseMatrix,pub macro_step_s:f64}
pub struct EventResponseGradient{pub name:String,pub value:f64,pub bars:MovingEventHistoryBars}
impl MovingFsiPairModel{
 fn event_series_shape(&self,history:&MovingEventHistory,p:EventAdvancePolicy)->CaeResult<(usize,usize,usize)>{
  let m=self.native_body(0)?.problem.coupling.substeps;let count=history.outputs.len();let field=self.solid_field()?;let fluid=self.event_fluid_field(p)?;
  if count==0||!count.is_multiple_of(m)||history.ticks.len()!=count||history.ticks.iter().enumerate().any(|(i,&t)|t!=i%m+1){return Err(fail("event series requires complete ordered native fluid-tick groups"));}
  let dt=fluid.inner().nominal_fluid_step_s();let mut old=history.initial.origin_s;
  for out in &history.outputs{if out.state.origin_s.to_bits()!=(old+dt).to_bits()||out.state.solid.len()!=field.state_size()+1||out.fluid_tick.samples.len()!=fluid.sample_names().len(){return Err(fail("event series clock/layout differs"));}old=out.state.origin_s;}
  Ok((m,field.sample_names().len(),fluid.sample_names().len()))
 }
 pub fn event_history_native_samples_at(&self,design:&[f64],p:EventAdvancePolicy,history:&MovingEventHistory,macro_offset:usize)->CaeResult<EventNativeSeries>{
  let(m,ns,nf)=self.event_series_shape(history,p)?;let field=self.solid_field()?;let fluid=self.event_fluid_field(p)?;let na=fluid.inner().sample_names().len();let width=ns+nf;let rows=history.outputs.len()/m;let mut samples=DenseMatrix::zeros(rows,width);let mut names=field.sample_names().to_vec();names.extend(fluid.sample_names().iter().cloned());
  for r in 0..rows{let first=r*m;let last=(r+1)*m-1;let previous=if first==0{&history.initial.solid}else{&history.outputs[first-1].state.solid};let end=&history.outputs[last].state.solid;let y=field.samples(r+macro_offset+1,&end[..field.state_size()],&previous[..field.state_size()],StepParameters{design,time_scale:1.})?;samples.data[r*width..r*width+ns].copy_from_slice(&y);
   for t in first..=last{for j in 0..nf{samples.data[r*width+ns+j]+=history.outputs[t].fluid_tick.samples[j]*if j<na{1.}else{1./m as f64};}}
  }
  if samples.data.iter().any(|x|!x.is_finite()){return Err(fail("event native sample overflow"));}Ok(EventNativeSeries{names,samples,macro_step_s:field.nominal_step_s()})
 }
 pub fn event_history_sample_adjoint_at(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,bar:&DenseMatrix,macro_offset:usize,terminal:Option<&EventTickStateDirection>)->CaeResult<MovingEventHistoryBars>{
  let(m,ns,nf)=self.event_series_shape(history,p)?;let field=self.solid_field()?;let fluid=self.event_fluid_field(p)?;let na=fluid.inner().sample_names().len();let n=field.state_size();let rows=history.outputs.len()/m;
  if bar.nrows!=rows||bar.ncols!=ns+nf||bar.data.len()!=rows*(ns+nf)||bar.data.iter().any(|x|!x.is_finite()){return Err(fail("event native sample cotangent shape"));}
  let mut bars:Vec<_>=history.outputs.iter().map(|o|MovingEventTickCotangent{state:EventTickStateDirection{solid:vec![0.;o.state.solid.len()],fluid:vec![0.;o.state.fluid.len()],lagged_force_n:vec![0.;o.state.lagged_force_n.len()],origin_s:0.},event_time_s:0.,fluid_samples:vec![0.;nf]}).collect();let mut initial=vec![0.;history.initial.solid.len()];let mut direct=vec![0.;design.len()];
  if let Some(t)=terminal{let b=&mut bars.last_mut().ok_or_else(||fail("event terminal empty history"))?.state;if b.solid.len()!=t.solid.len()||b.fluid.len()!=t.fluid.len()||b.lagged_force_n.len()!=t.lagged_force_n.len()||t.solid.iter().chain(&t.fluid).chain(&t.lagged_force_n).any(|v|!v.is_finite())||!t.origin_s.is_finite(){return Err(fail("event terminal cotangent layout"));}for(a,v)in b.solid.iter_mut().zip(&t.solid){*a+=v;}for(a,v)in b.fluid.iter_mut().zip(&t.fluid){*a+=v;}for(a,v)in b.lagged_force_n.iter_mut().zip(&t.lagged_force_n){*a+=v;}b.origin_s+=t.origin_s;}
  for r in 0..rows{let first=r*m;let last=(r+1)*m-1;let previous=if first==0{&history.initial.solid}else{&history.outputs[first-1].state.solid};let end=&history.outputs[last].state.solid;let(c,o,d)=field.samples_vjp(r+macro_offset+1,&end[..n],&previous[..n],StepParameters{design,time_scale:1.},&bar.data[r*bar.ncols..r*bar.ncols+ns])?;
   for i in 0..n{bars[last].state.solid[i]+=c[i];if first==0{initial[i]+=o[i];}else{bars[first-1].state.solid[i]+=o[i];}}for(a,b)in direct.iter_mut().zip(d){*a+=b;}
   for t in first..=last{for j in 0..nf{bars[t].fluid_samples[j]+=bar.data[r*bar.ncols+ns+j]*if j<na{1.}else{1./m as f64};}}
  }
  let mut result=self.event_history_adjoint(design,external,p,history,&bars)?;for(a,b)in result.initial.solid.iter_mut().zip(initial){*a+=b;}for(a,b)in result.design.iter_mut().zip(direct){*a+=b;}
  if result.design.iter().chain(&result.initial.solid).any(|x|!x.is_finite()){return Err(fail("event native sample adjoint overflow"));}Ok(result)
 }
 pub fn event_history_series_gradients(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,program:&DynamicProgram)->CaeResult<Vec<EventResponseGradient>>{
  let series=self.event_history_native_samples(design,p,history)?;let bound=program.bind(&series.names,&[])?;let values=bound.evaluate(&series.samples,series.macro_step_s,false,self.fixed_horizon_autonomous()?)?;let mut out=Vec::with_capacity(values.len());
  for(name,v)in values{if v.d_period_s!=0.{return Err(fail("event fixed-horizon period derivative unavailable"));}let bars=self.event_history_sample_adjoint(design,external,p,history,&v.d_samples)?;out.push(EventResponseGradient{name,value:v.value,bars});}Ok(out)
 }
}

impl MovingFsiPairModel{
 pub fn event_history_projected_gradients(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,projection:&super::sample_projection::PairSampleProjection,program:&DynamicProgram)->CaeResult<Vec<EventResponseGradient>>{
  let series=self.event_history_native_samples(design,p,history)?;
  if projection.source_names()!=series.names{return Err(fail("event projection source names differ"));}
  let mut samples=DenseMatrix::zeros(series.samples.nrows,projection.names().len());
  for r in 0..samples.nrows{let values=projection.values(&series.samples.data[r*series.samples.ncols..(r+1)*series.samples.ncols])?;samples.data[r*samples.ncols..(r+1)*samples.ncols].copy_from_slice(&values);}
  let bound=program.bind(projection.names(),&[])?;let values=bound.evaluate(&samples,series.macro_step_s,false,self.fixed_horizon_autonomous()?)?;let mut out=Vec::with_capacity(values.len());
  for(name,v)in values{if v.d_period_s!=0.{return Err(fail("event projected fixed-horizon period derivative unavailable"));}let mut bar=DenseMatrix::zeros(series.samples.nrows,series.samples.ncols);
   for r in 0..samples.nrows{let x=projection.pullback(&v.d_samples.data[r*samples.ncols..(r+1)*samples.ncols])?;bar.data[r*bar.ncols..(r+1)*bar.ncols].copy_from_slice(&x);}
   let bars=self.event_history_sample_adjoint(design,external,p,history,&bar)?;out.push(EventResponseGradient{name,value:v.value,bars});
  }Ok(out)
 }
}

impl MovingFsiPairModel{
 pub fn event_history_native_samples(&self,design:&[f64],p:EventAdvancePolicy,history:&MovingEventHistory)->CaeResult<EventNativeSeries>{self.event_history_native_samples_at(design,p,history,0)}
 pub fn event_history_sample_adjoint(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,bar:&DenseMatrix)->CaeResult<MovingEventHistoryBars>{self.event_history_sample_adjoint_at(design,external,p,history,bar,0,None)}
}
