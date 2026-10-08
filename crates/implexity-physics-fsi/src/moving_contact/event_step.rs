// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::{model::MovingFsiModel,impact::{Impact,ImpactTolerance},contact_field::{ContactField,NativeContactLaw},mapped_pair_law::MappedPairLaw};
use implexity_physics_solid::soft_fsi::field::SoftSolidField;
use implexity_linalg::sparse::CsrMatrix;
use crate::interface::FluidField;
use implexity_core::{CaeError,CaeResult};
use implexity_linalg::lu::Parallelism;
use implexity_solve::factorization::symbolic_for;
use implexity_physics_lbm::moving::{field::{AnyMovingLbm,event_trace::{EventTickTrace,EventTickOutput}},pushforward::InterpolationKernel,carrier::LagrangianCarrier};
use implexity_solve::{multirate_coupling::{FluxDrivenField,SubcycledField},time_stepper::StepParameters};
use std::sync::Arc;
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
fn norm(v:&[f64])->f64{v.iter().map(|x|x.abs()).fold(0.,f64::max)}
fn dot(a:&[f64],b:&[f64])->f64{a.iter().zip(b).map(|(a,b)|a*b).sum()}
#[derive(Clone,Copy)]
pub struct EventAdvancePolicy{pub restitution:f64,pub maintained_gap_target_fraction:f64,pub impact:ImpactTolerance,pub residual_tolerance:f64,pub maximum_newton_iterations:usize,pub maximum_localization_iterations:usize,pub interpolation_kernel:InterpolationKernel}
pub struct EventTickState{pub solid:Vec<f64>,pub fluid:Vec<f64>,pub lagged_force_n:Vec<f64>,pub origin_s:f64}
pub struct MovingEventTick{pub state:EventTickState,pub fluid_tick:EventTickOutput,pub event_time_s:Option<f64>,pub impact:Option<Impact>,pub final_gap_m:f64,pub final_normal_velocity_m_s:f64,pub multiplier_n:f64,pub final_residual:f64,pub body_lagged_work_j:f64,pub fluid_reaction_work_j:f64,pub lagged_work_defect_j:f64,pub maintained:bool,pub(crate) linearization:EventLinearization}
pub(super) fn solve<F:FluxDrivenField>(field:&F,old:&[f64],flux:&[f64],p:StepParameters<'_>,policy:EventAdvancePolicy)->CaeResult<(Vec<f64>,f64)>{
 let mut x=field.predict(1,old,None,p);if x.len()!=field.state_size(){return Err(fail("event native predictor shape"));}
 for _ in 0..policy.maximum_newton_iterations{
  let r=field.residual(1,&x,old,flux,p)?;let n=norm(&r);if !n.is_finite(){return Err(fail("event native residual overflow"));}if n<=policy.residual_tolerance{field.check_state_domain(1,&x,old,p)?;return Ok((x,n));}
  let a=field.current_jacobian(1,&x,old,flux,p)?.to_csr()?.to_csc();let factor=symbolic_for(&a).and_then(|s|s.factor(&a,Parallelism::Sequential)).map_err(fail)?;let rhs:Vec<_>=r.iter().map(|r|-r).collect();let d=factor.solve(&rhs).map_err(fail)?;
  let mut alpha=field.admissible_step(1,&x,&d,old,p,1.)?;if alpha<=0.{return Err(fail("event native/contact path refused Newton direction"));}let mut accepted=None;for _ in 0..24{let y:Vec<_>=x.iter().zip(&d).map(|(x,d)|x+alpha*d).collect();if y.iter().all(|x|x.is_finite()){let nr=norm(&field.residual(1,&y,old,flux,p)?);if nr<n{accepted=Some(y);break;}}alpha*=0.5;}
  x=accepted.ok_or_else(||fail("event native Newton did not decrease residual"))?;
 }
 Err(fail("event native Newton iteration limit"))
}
pub(super) fn solve_maintained<F:FluxDrivenField>(field:&ContactField<F,MappedPairLaw>,old:&[f64],flux:&[f64],p:StepParameters<'_>,policy:EventAdvancePolicy)->CaeResult<(Vec<f64>,f64)>{
 let mut x=old.to_vec();let b=field.native().state_size();eprintln!("maintained start gap={} lambda={}",field.law().instantaneous_row(old,p.design)?.1,old[b]);
 for _ in 0..policy.maximum_newton_iterations{
  let physical_r=field.residual(1,&x,old,flux,p)?;let(gap,row)=field.law().gap_current_matrix(&x,p.design)?;let target=policy.residual_tolerance*policy.maintained_gap_target_fraction;let mut r=physical_r.clone();r[b]=gap-target;let merit=norm(&r);eprintln!("maintained Newton gap_scaled={gap} lambda={} merit={merit} originalFB={}",x[b],physical_r[b]);
  if merit<=policy.residual_tolerance&&norm(&physical_r)<=policy.residual_tolerance&&x[b]>0.{field.check_state_domain(1,&x,old,p)?;return Ok((x,norm(&physical_r)));}
  let a=field.current_jacobian(1,&x,old,flux,p)?.to_csr()?;let row=row.to_csr()?;let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);for i in 0..b{let(js,v)=a.row(i);for(&j,&v)in js.iter().zip(v){ri.push(i);ci.push(j);vs.push(v);}}let(js,v)=row.row(0);for(&j,&v)in js.iter().zip(v){ri.push(b);ci.push(j);vs.push(v);}
  let a=CsrMatrix::from_triplets(b+1,b+1,&ri,&ci,&vs).map_err(fail)?.to_csc();let factor=symbolic_for(&a).and_then(|s|s.factor(&a,Parallelism::Sequential)).map_err(fail)?;let mut rhs:Vec<_>=r.iter().map(|r|-r).collect();if policy.maintained_gap_target_fraction==0.&&gap>=0.&&gap<=policy.residual_tolerance{rhs[b]=0.;}let d=factor.solve(&rhs).map_err(fail)?;let mut alpha=field.admissible_step(1,&x,&d,old,p,1.)?;if alpha<=0.{return Err(fail("maintained contact native/path Newton trial refused"));}let mut accepted=None;
  for _ in 0..24{let y:Vec<_>=x.iter().zip(&d).map(|(x,d)|x+alpha*d).collect();if y.iter().all(|x|x.is_finite())&&y[b]>=0.{let mut nr=field.residual(1,&y,old,flux,p)?;nr[b]=field.law().gap_current_matrix(&y,p.design)?.0-target;if norm(&nr)<merit{accepted=Some(y);break;}}alpha*=0.5;}
  x=accepted.ok_or_else(||fail("maintained contact Newton merit did not decrease"))?;
 }
 Err(fail("maintained contact Newton iteration limit"))
}
impl MovingFsiModel{
 pub fn event_fluid_field(&self,policy:EventAdvancePolicy)->CaeResult<FluidField>{let mut config=self.native().lbm_config();config.interpolation_kernel=policy.interpolation_kernel;let inner=AnyMovingLbm::new(self.native().problem.fluid.lattice,config,Arc::clone(&self.native().carrier) as Arc<dyn LagrangianCarrier>)?;Ok(FluidField::new(inner,vec![],self.native().problem.time.macro_step_s()))}
 pub fn event_tick_lagged(&self,state:&EventTickState,design:&[f64],external_force_n:&[f64],sample_tick:usize,policy:EventAdvancePolicy)->CaeResult<MovingEventTick>{
  if !policy.maintained_gap_target_fraction.is_finite()||!(0. ..=0.5).contains(&policy.maintained_gap_target_fraction)||!policy.restitution.is_finite()||!(0. ..=1.).contains(&policy.restitution)||!policy.residual_tolerance.is_finite()||policy.residual_tolerance<=0.||policy.maximum_newton_iterations==0||policy.maximum_localization_iterations==0||!state.origin_s.is_finite()||state.origin_s<0.||self.native().problem.solid.supports.iter().any(|s|s.motion.is_some()){return Err(fail("event step requires finite explicit policy and stationary prescribed supports"));}
  let contact=self.contact_field()?;let native=contact.native();let n=native.state_size();let fluid=self.event_fluid_field(policy)?;let dt=fluid.inner().nominal_fluid_step_s();let macro_dt=native.nominal_step_s();let scale=dt/macro_dt;let p=StepParameters{design,time_scale:scale};let nf=native.trace_operator().nrows();
  if state.solid.len()!=n+1||state.fluid.len()!=fluid.state_size()||state.lagged_force_n.len()!=nf||external_force_n.len()!=nf||state.solid.iter().chain(&state.fluid).chain(&state.lagged_force_n).chain(external_force_n).any(|x|!x.is_finite()){return Err(fail("event full native state/force shape"));}
  let flux:Vec<_>=state.lagged_force_n.iter().zip(external_force_n).map(|(a,b)|a+b).collect();let old=&state.solid;let (_,g0)=contact.law().instantaneous_row(old,design)?;if g0< -policy.impact.event_gap_m||old[n]<0.{return Err(fail("event initial contact domain"));}
  let mut event=None;let mut impulse=None;let mut maintained=false;let mut fraction=0.5;let mut pre=old.clone();let mut post=old.clone();
  let (end,residual)=if g0>=0. && contact.law().gap_current_matrix(old,design)?.0<=policy.residual_tolerance && old[n]>0.{maintained=true;solve_maintained(&contact,old,&flux,p,policy)?}else{
   let(free,r)=solve(native,&old[..n],&flux,p,policy)?;let mut trial=free;trial.push(0.);let(_,g1)=contact.law().instantaneous_row(&trial,design)?;eprintln!("free gap0={g0} gap1={g1}");
   if g1>=0.{(trial,r)}else{
    if g0<=0.{return Err(fail("new impact needs a strictly open initial gap"));}
    let(mut lo,mut hi,mut gl,mut gh)=(0.,1.,g0,g1);let mut selected=None;
    for _ in 0..policy.maximum_localization_iterations{let a=(lo+gl/(gl-gh)*(hi-lo)).clamp(lo+(hi-lo)*0.05,hi-(hi-lo)*0.05);let q=StepParameters{design,time_scale:scale*a};let(x,_)=solve(native,&old[..n],&flux,q,policy)?;let mut x=x;x.push(0.);let(_,g)=contact.law().instantaneous_row(&x,design)?;eprintln!("TOI a={a} gap={g} lo={lo} hi={hi}");if g>=0.&&g<=policy.impact.event_gap_m&&contact.law().check_state_domain(1,&x,old,q).is_ok(){selected=Some((a,x));break;}if g>0.{lo=a;gl=g;}else{hi=a;gh=g;}}
    let(a,at)=selected.ok_or_else(||fail("native segment impact localization unresolved"))?;fraction=a;pre=at;eprintln!("TOI selected fraction={a} gap={}",contact.law().instantaneous_row(&pre,design)?.1);let time=state.origin_s+a*dt;let jump=self.apply_solid_impact(&pre,&state.fluid,design,scale*a,time,policy.restitution,policy.impact)?;event=Some(time);post=jump.solid;impulse=Some(jump.impulse);
    let rest=StepParameters{design,time_scale:scale*(1.-a)};let(free,fr)=solve(native,&post[..n],&flux,rest,policy)?;let mut trial=free;trial.push(0.);let(_,gap)=contact.law().instantaneous_row(&trial,design)?;
    if gap>=0.{(trial,fr)}else{if policy.restitution!=0.{return Err(fail("recontact during event remainder needs another localized event"));}maintained=true;solve_maintained(&contact,&post,&flux,rest,policy)?}
   }
  };
  let history=native.core().history(p)?;let l=&history.layout;let unpack=|z:&[f64]|{let x=native.core().to_physical(&z[..n]);(x[l.u()..l.u()+l.n3].to_vec(),x[l.v()..l.v()+l.n3].to_vec())};let(u0,v0)=unpack(old);let(ue,ve)=unpack(&end);let(up,vp)=unpack(&pre);let(ua,va)=unpack(&post);
  let(row,gap)=contact.law().instantaneous_row(&end,design)?;let normal=dot(&row,&ve);if gap< -policy.impact.event_gap_m||!normal.is_finite(){return Err(fail("event final nonpenetration refused"));}if maintained&&(end[n]<=0.||normal< -policy.impact.normal_velocity_m_s){return Err(fail("maintained event needs positive multiplier and nonclosing final velocity"));}
  let(before_mid,after_mid,before_v,after_v)=if event.is_some(){(u0.iter().zip(&up).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),ua.iter().zip(&ue).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),v0.iter().zip(&vp).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),va.iter().zip(&ve).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>())}else{let mid:Vec<_>=u0.iter().zip(&ue).map(|(a,b)|0.5*(a+b)).collect();let v:Vec<_>=v0.iter().zip(&ve).map(|(a,b)|0.5*(a+b)).collect();(mid.clone(),mid,v.clone(),v)};
  let tick=fluid.event_tick(&state.fluid,design,EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+fraction*dt,sample_tick,start:&u0,before_midpoint:&before_mid,after_midpoint:&after_mid,end:&ue,before_velocity_m_s:&before_v,after_velocity_m_s:&after_v})?;
  let reaction:Vec<_>=(0..nf).map(|i|tick.before_impulse_n_s[i]+tick.after_impulse_n_s[i]+tick.start_compensation_impulse_n_s[i]+tick.end_compensation_impulse_n_s[i]).collect();let lagged_force:Vec<_>=reaction.iter().map(|x|x/dt).collect();let delta_u:Vec<_>=ue.iter().zip(&u0).map(|(a,b)|a-b).collect();let body_work=dot(&state.lagged_force_n,&delta_u);let fluid_work=dot(&tick.before_impulse_n_s,&before_v)+dot(&tick.after_impulse_n_s,&after_v)+dot(&tick.start_compensation_impulse_n_s,&v0)+dot(&tick.end_compensation_impulse_n_s,&ve);let newstate=EventTickState{solid:end.clone(),fluid:tick.state.clone(),lagged_force_n:lagged_force,origin_s:state.origin_s+dt};
  let linearization=EventLinearization{input:input_identity(self.identity(),state,design,external_force_n,sample_tick,policy),pre:pre.clone(),post:post.clone(),end:end.clone(),fraction,event,maintained};
  Ok(MovingEventTick{linearization,state:newstate,fluid_tick:tick,event_time_s:event,impact:impulse,final_gap_m:gap,final_normal_velocity_m_s:normal,multiplier_n:end[n],final_residual:residual,body_lagged_work_j:body_work,fluid_reaction_work_j:fluid_work,lagged_work_defect_j:body_work-fluid_work,maintained})
 }
}

use implexity_solve::matrix::checked_product;
use implexity_physics_lbm::moving::field::event_trace::EventTickDirection;
use sha2::{Digest,Sha256};
pub(crate) struct EventLinearization{pub(super) input:[u8;32],pub(super) pre:Vec<f64>,pub(super) post:Vec<f64>,pub(super) end:Vec<f64>,pub(super) fraction:f64,pub(super) event:Option<f64>,pub(super) maintained:bool}
pub struct EventTickStateDirection{pub solid:Vec<f64>,pub fluid:Vec<f64>,pub lagged_force_n:Vec<f64>,pub origin_s:f64}
pub struct MovingEventTickDirection{pub state:EventTickStateDirection,pub event_time_s:Option<f64>,pub fluid_samples:Vec<f64>}
pub(super) fn input_identity(model:&str,state:&EventTickState,design:&[f64],external:&[f64],tick:usize,p:EventAdvancePolicy)->[u8;32]{
 let mut h=Sha256::new();h.update(model.as_bytes());for a in [&state.solid[..],&state.fluid[..],&state.lagged_force_n[..],design,external]{h.update((a.len()as u64).to_le_bytes());for v in a{h.update(v.to_bits().to_le_bytes());}}
 for v in [state.origin_s,p.restitution,p.maintained_gap_target_fraction,p.residual_tolerance,p.impact.event_gap_m,p.impact.normal_velocity_m_s,p.impact.impulse_n_s,p.impact.momentum_n_s,p.impact.energy_j]{h.update(v.to_bits().to_le_bytes());}for v in [tick,p.maximum_newton_iterations,p.maximum_localization_iterations]{h.update((v as u64).to_le_bytes());}h.update(format!("{:?}",p.interpolation_kernel).as_bytes());h.finalize().into()
}
pub(super) fn implicit_direction<F:FluxDrivenField>(field:&F,current:&[f64],previous:&[f64],flux:&[f64],p:StepParameters<'_>,dprevious:&[f64],dflux:&[f64],ddesign:&[f64],dscale:f64)->CaeResult<Vec<f64>>{
 let j=field.jacobians(1,current,previous,flux,p)?;let a=j.current.to_csr()?.to_csc();let mut rhs=checked_product(&j.previous,dprevious,"event previous direction",false)?;let f=checked_product(&j.flux,dflux,"event flux direction",false)?;let q=checked_product(&j.design,ddesign,"event design direction",false)?;if rhs.len()!=j.time_scale.len(){return Err(fail("event implicit direction shape"));}for i in 0..rhs.len(){rhs[i]=-(rhs[i]+f[i]+q[i]+dscale*j.time_scale[i]);}let factor=symbolic_for(&a).and_then(|s|s.factor(&a,Parallelism::Sequential)).map_err(fail)?;factor.solve(&rhs).map_err(fail)
}
impl MovingFsiModel{
 pub fn event_tick_lagged_direction(&self,state:&EventTickState,design:&[f64],external:&[f64],tick:usize,p:EventAdvancePolicy,base:&MovingEventTick,dstate:&EventTickStateDirection,ddesign:&[f64],dexternal:&[f64],drestitution:f64)->CaeResult<MovingEventTickDirection>{
  let b=&base.linearization;if b.input!=input_identity(self.identity(),state,design,external,tick,p){return Err(fail("event direction input identity differs"));}
  if dstate.solid.len()!=state.solid.len()||dstate.fluid.len()!=state.fluid.len()||dstate.lagged_force_n.len()!=state.lagged_force_n.len()||ddesign.len()!=design.len()||dexternal.len()!=external.len()||!dstate.origin_s.is_finite()||!drestitution.is_finite()||dstate.solid.iter().chain(&dstate.fluid).chain(&dstate.lagged_force_n).chain(ddesign).chain(dexternal).any(|v|!v.is_finite()){return Err(fail("event history direction shape"));}
  let contact=self.contact_field()?;let native=contact.native();let n=native.state_size();let fluid=self.event_fluid_field(p)?;let dt=fluid.inner().nominal_fluid_step_s();let macro_dt=native.nominal_step_s();let scale=dt/macro_dt;let flux:Vec<_>=state.lagged_force_n.iter().zip(external).map(|(a,b)|a+b).collect();let dflux:Vec<_>=dstate.lagged_force_n.iter().zip(dexternal).map(|(a,b)|a+b).collect();let zero_design=vec![0.;design.len()];let mut dpre=dstate.solid.clone();let mut dpost=dstate.solid.clone();let mut dtau=0.;
  if let Some(time)=b.event{
   let pre_p=StepParameters{design,time_scale:scale*b.fraction};let mut fixed=implicit_direction(native,&b.pre[..n],&state.solid[..n],&flux,pre_p,&dstate.solid[..n],&dflux,ddesign,0.)?;fixed.push(0.);
   let mut rate=implicit_direction(native,&b.pre[..n],&state.solid[..n],&flux,pre_p,&vec![0.;n],&vec![0.;flux.len()],&zero_design,1./macro_dt)?;rate.push(0.);
   let dg=contact.law().instantaneous_row_direction(&b.pre,design,&fixed,ddesign)?.1;let speed=contact.law().instantaneous_row_direction(&b.pre,design,&rate,&zero_design)?.1;if !speed.is_finite()||speed>=0.{return Err(fail("event direction needs transverse closing discrete impact"));}dtau=-dg/speed;if !dtau.is_finite(){return Err(fail("event time direction overflow"));}dpre=fixed.iter().zip(&rate).map(|(a,b)|a+dtau*b).collect();
   let impact=self.apply_solid_impact(&b.pre,&state.fluid,design,scale*b.fraction,time,p.restitution,p.impact)?;if impact.solid.iter().zip(&b.post).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("event impact linearization identity differs"));}dpost=self.solid_impact_direction(&b.pre,design,scale*b.fraction,&impact,&dpre,ddesign,p.restitution,drestitution)?;
  }else if drestitution!=0.{return Err(fail("restitution direction requires active localized event"));}
  let previous=if b.event.is_some(){&b.post}else{&state.solid};let dprevious=if b.event.is_some(){&dpost}else{&dstate.solid};let end_p=StepParameters{design,time_scale:if b.event.is_some(){scale*(1.-b.fraction)}else{scale}};
  let dend=if b.maintained{maintained_direction(&contact,&b.end,previous,&flux,end_p,dprevious,&dflux,ddesign,-dtau/macro_dt)?}else{let mut d=implicit_direction(native,&b.end[..n],&previous[..n],&flux,end_p,&dprevious[..n],&dflux,ddesign,-dtau/macro_dt)?;d.push(0.);d};
  let h=native.core().history(StepParameters{design,time_scale:scale})?;let l=&h.layout;let unpack=|x:&[f64]|{let x=native.core().to_physical(&x[..n]);(x[l.u()..l.u()+l.n3].to_vec(),x[l.v()..l.v()+l.n3].to_vec())};let(u0,v0)=unpack(&state.solid);let(up,vp)=unpack(&b.pre);let(ua,va)=unpack(&b.post);let(ue,ve)=unpack(&b.end);let(du0,dv0)=unpack(&dstate.solid);let(dup,dvp)=unpack(&dpre);let(dua,dva)=unpack(&dpost);let(due,dve)=unpack(&dend);let mean=|a:&[f64],b:&[f64]|a.iter().zip(b).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>();
  let(bm,am,bv,av,dbm,dam,dbv,dav)=if b.event.is_some(){(mean(&u0,&up),mean(&ua,&ue),mean(&v0,&vp),mean(&va,&ve),mean(&du0,&dup),mean(&dua,&due),mean(&dv0,&dvp),mean(&dva,&dve))}else{let m=mean(&u0,&ue);let v=mean(&v0,&ve);let dm=mean(&du0,&due);let dv=mean(&dv0,&dve);(m.clone(),m,v.clone(),v,dm.clone(),dm,dv.clone(),dv)};
  let trace=EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+b.fraction*dt,sample_tick:tick,start:&u0,before_midpoint:&bm,after_midpoint:&am,end:&ue,before_velocity_m_s:&bv,after_velocity_m_s:&av};let direction=EventTickDirection{origin_s:dstate.origin_s,event_s:dstate.origin_s+dtau,previous:&dstate.fluid,design:ddesign,start:&du0,before_midpoint:&dbm,after_midpoint:&dam,end:&due,before_velocity_m_s:&dbv,after_velocity_m_s:&dav};let f=fluid.event_tick_tangent(&state.fluid,design,trace,direction)?;
  let lagged_force_n=(0..state.lagged_force_n.len()).map(|i|(f.before_impulse_n_s[i]+f.after_impulse_n_s[i]+f.start_compensation_impulse_n_s[i]+f.end_compensation_impulse_n_s[i])/dt).collect();Ok(MovingEventTickDirection{state:EventTickStateDirection{solid:dend,fluid:f.state,lagged_force_n,origin_s:dstate.origin_s},event_time_s:b.event.map(|_|dstate.origin_s+dtau),fluid_samples:f.samples})
 }
}

pub(super) struct ImplicitBars{pub previous:Vec<f64>,pub flux:Vec<f64>,pub design:Vec<f64>,pub time_scale:f64}
pub(super) fn implicit_pullback(j:&implexity_solve::multirate_coupling::FieldJacobians,bar:&[f64])->CaeResult<ImplicitBars>{
 let a=j.current.to_csr()?.to_csc();if bar.len()!=a.nrows()||bar.iter().any(|x|!x.is_finite()){return Err(fail("event implicit cotangent shape"));}
 let factor=symbolic_for(&a).and_then(|s|s.factor(&a,Parallelism::Sequential)).map_err(fail)?;
 let z=factor.solve_transpose(bar).map_err(fail)?;let nz:Vec<_>=z.iter().map(|x|-x).collect();
 let previous=checked_product(&j.previous,&nz,"event previous pullback",true)?;
 let flux=checked_product(&j.flux,&nz,"event flux pullback",true)?;
 let design=checked_product(&j.design,&nz,"event design pullback",true)?;
 if nz.len()!=j.time_scale.len(){return Err(fail("event implicit time shape"));}let time_scale=dot(&nz,&j.time_scale);
 if previous.iter().chain(&flux).chain(&design).any(|x|!x.is_finite())||!time_scale.is_finite(){return Err(fail("event implicit pullback overflow"));}
 Ok(ImplicitBars{previous,flux,design,time_scale})
}
pub(super) fn maintained_jacobians<F:FluxDrivenField>(field:&ContactField<F,MappedPairLaw>,current:&[f64],previous:&[f64],flux:&[f64],p:StepParameters<'_>)->CaeResult<implexity_solve::multirate_coupling::FieldJacobians>{
 let mut j=field.jacobians(1,current,previous,flux,p)?;let n=field.native().state_size();
 let replace=|a:&implexity_solve::matrix::Jacobian,row:Option<&implexity_solve::matrix::Jacobian>|->CaeResult<implexity_solve::matrix::Jacobian>{
  if matches!(a,implexity_solve::matrix::Jacobian::Operator(_)){
   use implexity_solve::matrix::{Jacobian,FnAction};let shape=a.shape();let forward=a.clone();let reverse=a.clone();let rf=row.cloned();let rr=row.cloned();
   return Ok(Jacobian::Operator(Arc::new(FnAction::new(shape,move|x|{let mut y=checked_product(&forward,x,"maintained primal matrix action",false)?;y[n]=if let Some(r)=&rf{checked_product(r,x,"maintained row action",false)?[0]}else{0.};Ok(y)},move|y|{if y.len()!=shape.0{return Err(fail("maintained matrix transpose shape"));}let mut yy=y.to_vec();yy[n]=0.;let mut x=checked_product(&reverse,&yy,"maintained native transpose",true)?;if let Some(r)=&rr{let z=checked_product(r,&[y[n]],"maintained row transpose",true)?;for(a,b)in x.iter_mut().zip(z){*a+=b;}}Ok(x)}))));
  }
  let a=a.to_csr()?;let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);
  for i in 0..n{let(c,v)=a.row(i);for(&c,&v)in c.iter().zip(v){ri.push(i);ci.push(c);vs.push(v);}}
  if let Some(row)=row{let row=row.to_csr()?;let(c,v)=row.row(0);for(&c,&v)in c.iter().zip(v){ri.push(n);ci.push(c);vs.push(v);}}
  Ok(implexity_solve::matrix::Jacobian::Csr(CsrMatrix::from_triplets(a.nrows(),a.ncols(),&ri,&ci,&vs).map_err(fail)?))
 };
 let row=field.law().gap_current_matrix(current,p.design)?.1;let design=field.law().gap_design_matrix(current,p.design)?;
 j.current=replace(&j.current,Some(&row))?;j.previous=replace(&j.previous,None)?;j.flux=replace(&j.flux,None)?;j.design=replace(&j.design,Some(&design))?;
 j.time_scale[n]=0.;Ok(j)
}
pub(super) fn maintained_direction<F:FluxDrivenField>(field:&ContactField<F,MappedPairLaw>,current:&[f64],previous:&[f64],flux:&[f64],p:StepParameters<'_>,dprevious:&[f64],dflux:&[f64],ddesign:&[f64],dscale:f64)->CaeResult<Vec<f64>>{
 let j=maintained_jacobians(field,current,previous,flux,p)?;let mut rhs=checked_product(&j.previous,dprevious,"maintained previous direction",false)?;
 let f=checked_product(&j.flux,dflux,"maintained force direction",false)?;let q=checked_product(&j.design,ddesign,"maintained design direction",false)?;
 for i in 0..rhs.len(){rhs[i]=-(rhs[i]+f[i]+q[i]+dscale*j.time_scale[i]);}
 let a=j.current.to_csr()?.to_csc();symbolic_for(&a).and_then(|s|s.factor(&a,Parallelism::Sequential)).and_then(|f|f.solve(&rhs)).map_err(fail)
}
