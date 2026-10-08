// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::*;
#[derive(Clone,Copy)]
pub struct EventTickTrace<'a>{
 pub origin_s:f64,pub event_s:f64,pub sample_tick:usize,
 pub start:&'a[f64],pub before_midpoint:&'a[f64],pub after_midpoint:&'a[f64],pub end:&'a[f64],
 pub before_velocity_m_s:&'a[f64],pub after_velocity_m_s:&'a[f64],
}
#[derive(Clone,Copy)]
pub struct EventTickDirection<'a>{
 pub origin_s:f64,pub event_s:f64,pub previous:&'a[f64],pub design:&'a[f64],
 pub start:&'a[f64],pub before_midpoint:&'a[f64],pub after_midpoint:&'a[f64],pub end:&'a[f64],
 pub before_velocity_m_s:&'a[f64],pub after_velocity_m_s:&'a[f64],
}
pub struct EventTickOutput{
 pub state:Vec<f64>,pub before_impulse_n_s:Vec<f64>,pub after_impulse_n_s:Vec<f64>,
 pub start_compensation_impulse_n_s:Vec<f64>,pub end_compensation_impulse_n_s:Vec<f64>,pub samples:Vec<f64>,
 pub minimum_population:f64,pub fraction:f64,pub origin_s:f64,pub event_s:f64,
}
pub struct EventTickTangent{
 pub primal:EventTickOutput,pub state:Vec<f64>,pub before_impulse_n_s:Vec<f64>,pub after_impulse_n_s:Vec<f64>,
 pub start_compensation_impulse_n_s:Vec<f64>,pub end_compensation_impulse_n_s:Vec<f64>,pub samples:Vec<f64>,
}
struct EventPrepared<S>{mac:[Macro<S>;2],acc:[Accum<S>;2],post:[Vec<S>;2],comp:Macro<S>,compacc:[Accum<S>;2],out:Vec<S>,samples:Vec<S>,impulses:[Vec<S>;4],fraction:S,minimum:f64}
impl<const Q:usize,L:Lattice<Q>> MovingLbm<Q,L>{
 fn event_check(&self,previous:&[f64],design:&[f64],trace:EventTickTrace<'_>,direction:Option<EventTickDirection<'_>>)->CaeResult<()> {
  let dt=self.nominal_fluid_step_s();let a=(trace.event_s-trace.origin_s)/dt;
  let nt=self.trace_size();let arrays=[trace.start,trace.before_midpoint,trace.after_midpoint,trace.end,trace.before_velocity_m_s,trace.after_velocity_m_s];
  if !trace.origin_s.is_finite()||!trace.event_s.is_finite()||!a.is_finite()||a<=0.||a>=1.||trace.sample_tick==0||trace.sample_tick>self.config.substeps||arrays.iter().any(|v|v.len()!=nt||v.iter().any(|x|!x.is_finite()))||previous.len()!=self.state_size()||design.len()!=self.design_size()||previous.iter().chain(design).any(|x|!x.is_finite()){return Err(CaeError::contract("native event tick input refused"));}
  if let Some(d)=direction {let da=[d.start,d.before_midpoint,d.after_midpoint,d.end,d.before_velocity_m_s,d.after_velocity_m_s];if !d.origin_s.is_finite()||!d.event_s.is_finite()||d.previous.len()!=previous.len()||d.design.len()!=design.len()||d.previous.iter().chain(d.design).any(|x|!x.is_finite())||da.iter().any(|v|v.len()!=nt||v.iter().any(|x|!x.is_finite())){return Err(CaeError::contract("native event tick direction refused"));}}
  self.admit_time_scale(1.)
 }
 fn event_macro<S:Lane>(&self,mid:&[f64],velocity:&[f64],design:&[f64],dmid:Option<&[f64]>,dvelocity:Option<&[f64]>,ddesign:Option<&[f64]>)->CaeResult<Macro<S>>{
  let mut mac=self.setup::<S>(mid,mid,design,dmid,dmid,ddesign,true)?;let c=self.config.spacing_m/self.nominal_fluid_step_s();let v=self.carrier.flux_to_impulses(velocity,c);let dv=dvelocity.map(|d|self.carrier.flux_to_impulses(d,c));
  if v.len()!=mac.v.len(){return Err(CaeError::contract("native event velocity cloud shape"));}
  mac.v=(0..v.len()).map(|i|std::array::from_fn(|a|S::make(v[i][a],dv.as_ref().map_or(0.,|v|v[i][a])))).collect();mac.occ=pushforward::occupancy(&self.frame,[&mac.ends[0],&mac.ends[1]],&mac.v);Ok(mac)
 }
 fn event_branch<S:Lane>(&self,previous:&[S],p:&Params<S>,mac:&Macro<S>,k:usize)->(Vec<S>,Accum<S>){
  let mut out=vec![S::zero();previous.len()];let mut dp=vec![[S::zero();3];mac.occ.cells.len()];let mut fields=self.needs_fields(k).then(||Fields{rho:vec![S::zero();self.cells()],u:vec![[S::zero();3];self.cells()]});let speed=self.substep(previous,&mut out,p,&mac.occ,fields.as_mut(),&mut dp);let mut acc=self.new_accum(&mac.occ);acc.max_speed=speed;
  let sampled=match self.config.sampling{Sampling::End=>k==self.config.substeps,Sampling::MacroMean=>true};if sampled{let weight=if self.config.sampling==Sampling::MacroMean{1./self.config.substeps as f64}else{1.};let mut dpsum=[S::zero();3];for v in &dp{for a in 0..3{dpsum[a]+=v[a];}}
   for(o,s)in self.observables.iter().zip(&mut acc.samples){if o.reads_populations(){*s+=o.population_value(&p.scales,previous)*weight;}else if o.reads_occupancy(){*s+=o.open_area(|x|Self::solid_fraction(&mac.occ,x,p.theta,self.config.saturation_width),None,|_,_|{})*weight;}else if !o.reads_cells(){*s+=o.value(&p.scales,&[],&[],&self.fluid,dpsum)*weight;}}
  }
  self.accumulate(&mut acc,&mac.occ,p,&dp,fields.as_ref(),k);(out,acc)
 }
 fn event_prepare<S:Lane>(&self,previous:&[f64],design:&[f64],t:EventTickTrace<'_>,d:Option<EventTickDirection<'_>>)->CaeResult<EventPrepared<S>>{
  self.event_check(previous,design,t,d)?;let mac=[self.event_macro::<S>(t.before_midpoint,t.before_velocity_m_s,design,d.map(|d|d.before_midpoint),d.map(|d|d.before_velocity_m_s),d.map(|d|d.design))?,self.event_macro::<S>(t.after_midpoint,t.after_velocity_m_s,design,d.map(|d|d.after_midpoint),d.map(|d|d.after_velocity_m_s),d.map(|d|d.design))?];
  let prev:Vec<_>=previous.iter().enumerate().map(|(i,&x)|S::make(x,d.map_or(0.,|d|d.previous[i]))).collect();let origin=S::make(t.origin_s,d.map_or(0.,|d|d.origin_s));let event=S::make(t.event_s,d.map_or(0.,|d|d.event_s));let fraction=(event-origin)/self.nominal_fluid_step_s();let mut p=self.params_absolute(S::one(),origin,1);p.theta=0.5;
  let(a,aa)=self.event_branch(&prev,&p,&mac[0],t.sample_tick);let(b,ab)=self.event_branch(&prev,&p,&mac[1],t.sample_tick);let out:Vec<_>=a.iter().zip(&b).map(|(a,b)|*a*fraction+*b*(S::one()-fraction)).collect();let values:Vec<_>=out.iter().map(|v|v.value()).collect();let minimum=self.check_state(&values,aa.max_speed.max(ab.max_speed))?;
  let mut i0=self.flux_of(&mac[0],&aa,S::one())?.0;let mut i1=self.flux_of(&mac[1],&ab,S::one())?.0;let duration=self.config.substeps as f64*self.nominal_fluid_step_s();for v in &mut i0{*v=*v*duration*fraction;}for v in &mut i1{*v=*v*duration*(S::one()-fraction);}
  let comp=self.setup::<S>(t.start,t.end,design,d.map(|d|d.start),d.map(|d|d.end),d.map(|d|d.design),true)?;let mut ca=self.new_accum(&comp.occ);let mut cb=self.new_accum(&comp.occ);
  if self.compensated(){let a=self.entrained(&prev,&comp.occ,0);let b=self.entrained(&out,&comp.occ,1);for i in 0..a.len(){for e in 0..3{ca.g[0][i][e]-=a[i][e];cb.g[1][i][e]+=b[i][e];}}}
  let mut c0=self.flux_of(&comp,&ca,S::one())?.0;let mut c1=self.flux_of(&comp,&cb,S::one())?.0;for v in c0.iter_mut().chain(&mut c1){*v=*v*duration;}
  let samples=aa.samples.iter().zip(&ab.samples).map(|(a,b)|*a*fraction+*b*(S::one()-fraction)).collect();
  if out.iter().chain(&i0).chain(&i1).chain(&c0).chain(&c1).any(|v|!v.value().is_finite()||!v.tangent().is_finite()){return Err(CaeError::contract("native event tick output refused"));}
  Ok(EventPrepared{mac,acc:[aa,ab],post:[a,b],comp,compacc:[ca,cb],out,samples,impulses:[i0,i1,c0,c1],fraction,minimum})
 }
 pub fn event_tick(&self,previous:&[f64],design:&[f64],trace:EventTickTrace<'_>)->CaeResult<EventTickOutput>{let p=self.event_prepare::<f64>(previous,design,trace,None)?;Ok(EventTickOutput{state:p.out,before_impulse_n_s:p.impulses[0].clone(),after_impulse_n_s:p.impulses[1].clone(),start_compensation_impulse_n_s:p.impulses[2].clone(),end_compensation_impulse_n_s:p.impulses[3].clone(),samples:p.samples,minimum_population:p.minimum,fraction:p.fraction,origin_s:trace.origin_s,event_s:trace.event_s})}
 pub fn event_tick_tangent(&self,previous:&[f64],design:&[f64],trace:EventTickTrace<'_>,direction:EventTickDirection<'_>)->CaeResult<EventTickTangent>{let p=self.event_prepare::<Dual<1>>(previous,design,trace,Some(direction))?;let values=|a:&[Dual<1>]|a.iter().map(|v|v.value()).collect();let eps=|a:&[Dual<1>]|a.iter().map(|v|v.eps[0]).collect();Ok(EventTickTangent{primal:EventTickOutput{state:values(&p.out),before_impulse_n_s:values(&p.impulses[0]),after_impulse_n_s:values(&p.impulses[1]),start_compensation_impulse_n_s:values(&p.impulses[2]),end_compensation_impulse_n_s:values(&p.impulses[3]),samples:values(&p.samples),minimum_population:p.minimum,fraction:p.fraction.value(),origin_s:trace.origin_s,event_s:trace.event_s},state:eps(&p.out),before_impulse_n_s:eps(&p.impulses[0]),after_impulse_n_s:eps(&p.impulses[1]),start_compensation_impulse_n_s:eps(&p.impulses[2]),end_compensation_impulse_n_s:eps(&p.impulses[3]),samples:eps(&p.samples)})}
}
pub struct EventTickCotangent<'a>{pub state:&'a[f64],pub before_impulse_n_s:&'a[f64],pub after_impulse_n_s:&'a[f64],pub start_compensation_impulse_n_s:&'a[f64],pub end_compensation_impulse_n_s:&'a[f64],pub samples:&'a[f64]}
pub struct EventTickBars{pub previous:Vec<f64>,pub design:Vec<f64>,pub start:Vec<f64>,pub before_midpoint:Vec<f64>,pub after_midpoint:Vec<f64>,pub end:Vec<f64>,pub before_velocity_m_s:Vec<f64>,pub after_velocity_m_s:Vec<f64>,pub origin_s:f64,pub event_s:f64}
impl<const Q:usize,L:Lattice<Q>> MovingLbm<Q,L>{
 fn event_impulse_bar(&self,bar:&[f64])->Vec<[f64;3]>{let scale=self.params_absolute(1.,0.,1).impulse_scale;self.carrier.flux_to_impulses(bar,1.).iter().map(|v|v.map(|x|x*scale)).collect()}
 fn event_reverse_branch(&self,previous:&[f64],design:&[f64],mid:&[f64],p:&Params<f64>,mac:&Macro<f64>,acc:&Accum<f64>,state_bar:&[f64],impulse_bar:&[f64],sample_bar:&[f64],k:usize,origin_s:f64)->CaeResult<(Vec<f64>,Vec<f64>,Vec<f64>,Vec<f64>,f64)>{
  let ib=self.event_impulse_bar(impulse_bar);let(pb0,g0)=pushforward::endpoint_impulses_vjp(&self.frame,&mac.ends[0],&mac.occ.slot,&mac.occ.cells,&acc.g[0],&ib);let(pb1,g1)=pushforward::endpoint_impulses_vjp(&self.frame,&mac.ends[1],&mac.occ.slot,&mac.occ.cells,&acc.g[1],&ib);
  let rows=mac.occ.cells.len();let mut db=[vec![0.;rows],vec![0.;rows]];let mut mb=[vec![[0.;3];rows],vec![[0.;3];rows]];let mut fb=vec![0.;previous.len()];
  let params=self.reverse_substep(previous,Cotangent::Direct(state_bar),&mut fb,p,&mac.occ,[&g0,&g1],sample_bar,k,&mut db,&mut mb);let previous=self.streaming_transpose(&fb,state_bar,vec![]);
  let mut vb=vec![[0.;3];mac.v.len()];let e0=pushforward::endpoint_vjp(&self.frame,&mac.ends[0],&mac.occ.slot,&mac.v,&db[0],&mb[0],&mut vb);let e1=pushforward::endpoint_vjp(&self.frame,&mac.ends[1],&mac.occ.slot,&mac.v,&db[1],&mb[1],&mut vb);
  let(qb,dd)=self.points_pullback(mid,design,&[&e0,&e1,&pb0,&pb1],None,None)?;let c=self.config.spacing_m/self.nominal_fluid_step_s();let velocity=self.carrier.impulses_to_flux(&vb,c);
  let origin_derivative=self.params_absolute(Dual::<1>::new(1.,[0.]),Dual::<1>::new(origin_s,[1.]),1).map(|v|v.eps[0]);
  Ok((previous,qb.0,dd.0,velocity,params.contract(&origin_derivative)))
 }
 pub fn event_tick_adjoint(&self,previous:&[f64],design:&[f64],t:EventTickTrace<'_>,bar:EventTickCotangent<'_>)->CaeResult<EventTickBars>{
  let nt=self.trace_size();if bar.state.len()!=self.state_size()||bar.samples.len()!=self.observables.len()||[bar.before_impulse_n_s,bar.after_impulse_n_s,bar.start_compensation_impulse_n_s,bar.end_compensation_impulse_n_s].iter().any(|v|v.len()!=nt)||bar.state.iter().chain(bar.samples).chain(bar.before_impulse_n_s).chain(bar.after_impulse_n_s).chain(bar.start_compensation_impulse_n_s).chain(bar.end_compensation_impulse_n_s).any(|x|!x.is_finite()){return Err(CaeError::contract("native event tick cotangent refused"));}
  let z=self.event_prepare::<f64>(previous,design,t,None)?;let mut state_bar=bar.state.to_vec();let mut previous_comp=vec![0.;previous.len()];let comp=&z.comp;let rows=comp.occ.cells.len();let mut db=[vec![0.;rows],vec![0.;rows]];let mb=[vec![[0.;3];rows],vec![[0.;3];rows]];
  let i0=self.event_impulse_bar(bar.start_compensation_impulse_n_s);let i1=self.event_impulse_bar(bar.end_compensation_impulse_n_s);let(pb0,g0)=pushforward::endpoint_impulses_vjp(&self.frame,&comp.ends[0],&comp.occ.slot,&comp.occ.cells,&z.compacc[0].g[0],&i0);let(pb1,g1)=pushforward::endpoint_impulses_vjp(&self.frame,&comp.ends[1],&comp.occ.slot,&comp.occ.cells,&z.compacc[1].g[1],&i1);
  if self.compensated(){let start=self.entrained(previous,&comp.occ,0);let end=self.entrained(&z.out,&comp.occ,1);self.entrained_vjp(&mut previous_comp,&comp.occ,0,&g0,-1.,&mut db[0],Some(&start));self.entrained_vjp(&mut state_bar,&comp.occ,1,&g1,1.,&mut db[1],Some(&end));}
  let mut vb=vec![[0.;3];comp.v.len()];let e0=pushforward::endpoint_vjp(&self.frame,&comp.ends[0],&comp.occ.slot,&comp.v,&db[0],&mb[0],&mut vb);let e1=pushforward::endpoint_vjp(&self.frame,&comp.ends[1],&comp.occ.slot,&comp.v,&db[1],&mb[1],&mut vb);let(start,ds)=self.points_pullback(t.start,design,&[&e0,&pb0],None,None)?;let(end,de)=self.points_pullback(t.end,design,&[&e1,&pb1],None,None)?;
  let mut p=self.params_absolute(1.,t.origin_s,1);p.theta=0.5;let a=z.fraction;let scalar=|v:&[f64],s:f64|v.iter().map(|v|v*s).collect::<Vec<_>>();let r0=self.event_reverse_branch(previous,design,t.before_midpoint,&p,&z.mac[0],&z.acc[0],&scalar(&state_bar,a),&scalar(bar.before_impulse_n_s,a),&scalar(bar.samples,a),t.sample_tick,t.origin_s)?;let r1=self.event_reverse_branch(previous,design,t.after_midpoint,&p,&z.mac[1],&z.acc[1],&scalar(&state_bar,1.-a),&scalar(bar.after_impulse_n_s,1.-a),&scalar(bar.samples,1.-a),t.sample_tick,t.origin_s)?;
  let mut alpha=state_bar.iter().zip(z.post[0].iter().zip(&z.post[1])).map(|(g,(a,b))|g*(a-b)).sum::<f64>();alpha+=bar.samples.iter().zip(z.acc[0].samples.iter().zip(&z.acc[1].samples)).map(|(g,(a,b))|g*(a-b)).sum::<f64>();alpha+=bar.before_impulse_n_s.iter().zip(&z.impulses[0]).map(|(g,i)|g*i/a).sum::<f64>();alpha-=bar.after_impulse_n_s.iter().zip(&z.impulses[1]).map(|(g,i)|g*i/(1.-a)).sum::<f64>();
  let event_s=alpha/self.nominal_fluid_step_s();let origin_s=r0.4+r1.4-event_s;let previous:Vec<_>=previous_comp.iter().zip(r0.0.iter().zip(&r1.0)).map(|(c,(a,b))|c+a+b).collect();let design:Vec<_>=ds.0.iter().zip(de.0.iter().zip(r0.2.iter().zip(&r1.2))).map(|(s,(e,(a,b)))|s+e+a+b).collect();
  if previous.iter().chain(&design).chain(&start.0).chain(&end.0).chain(&r0.1).chain(&r1.1).chain(&r0.3).chain(&r1.3).any(|x|!x.is_finite())||!event_s.is_finite()||!origin_s.is_finite(){return Err(CaeError::contract("native event tick reverse refused"));}
  Ok(EventTickBars{previous,design,start:start.0,before_midpoint:r0.1,after_midpoint:r1.1,end:end.0,before_velocity_m_s:r0.3,after_velocity_m_s:r1.3,origin_s,event_s})
 }
}
