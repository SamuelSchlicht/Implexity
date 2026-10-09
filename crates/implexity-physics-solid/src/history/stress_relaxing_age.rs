// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::{Dual,Scalar};
use implexity_core::error::{CaeError,CaeResult};
use implexity_linalg::dense::DenseMatrix;
use crate::{inelastic::CreepLaw,mandel::{self,Mandel},material::{Props,idx}};
use super::{local_advance::{LocalAdvanceRequest,LocalAdvanceResult}};

fn scaled<S:Scalar>(trial:&Mandel<S>,a:S)->Mandel<S> {
 let d=mandel::dev(trial);std::array::from_fn(|i|trial[i]-d[i]+a*d[i])
}
fn equation<S:Scalar>(law:CreepLaw,trial:&Mandel<S>,prop:&Props<S>,dt:S,a:S)->S {
 let stress=scaled(trial,a);let q=mandel::equivalent(&stress);let g=prop.get(idx::E)/(S::from_f64(2.)*(S::one()+prop.get(idx::NU)));
 a+g*3.*law.increment(&stress,prop,dt)*a/q-S::one()
}
fn root<S:Scalar>(law:CreepLaw,trial:&Mandel<S>,prop:&Props<S>,dt:S)->CaeResult<(f64,f64,usize)> {
 let t: Mandel<f64>=std::array::from_fn(|i|trial[i].value());let p=Props::new(std::array::from_fn(|i|prop.values[i].value()),prop.temperature.value());let age=dt.value();
 if p.get(idx::E)<=0. || !(-1.0..0.5).contains(&p.get(idx::NU)) || p.get(idx::CREEP_EXPONENT)<1. || p.get(idx::CREEP_RATE)<0. || p.get(idx::CREEP_STRESS)<=0. {return Err(CaeError::contract("stress relaxation requires positive isotropic Norton coefficients and exponent at least one"));}
 let mut lo=0.;let mut hi=1.;let mut a=1.;let mut iterations=0;
 for i in 0..180 {a=(lo+hi)*0.5;let f=equation(law,&t,&p,age,a);if !f.is_finite(){return Err(CaeError::convergence("nonfinite native scalar stress relaxation residual"));}iterations=i+1;if f.abs()<=2e-13 {break;}if f>0. {hi=a;}else{lo=a;}if lo.to_bits()==hi.to_bits(){break;}}
 if age==0. {a=1.;iterations=0;}
 let td:Mandel<Dual<1>>=std::array::from_fn(|i|Dual::constant(t[i]));let pd=Props::new(std::array::from_fn(|i|Dual::constant(p.values[i])),Dual::constant(p.temperature));let f=equation(law,&td,&pd,Dual::constant(age),Dual::variable(a,0));
 if !f.re.is_finite() || f.re.abs()>1e-11 || !f.eps[0].is_finite() || f.eps[0]<=0. {return Err(CaeError::convergence("native scalar stress relaxation failed residual or monotonicity certificate"));}
 Ok((a,f.eps[0],iterations))
}
fn evaluate<S:Scalar>(r:&LocalAdvanceRequest<'_>,input:&[S],enabled:bool)->CaeResult<(Vec<S>,S,usize)> {
 let m=r.model;let w=m.local_width();let n=m.internal_size;let ls=m.local_size();let previous=&input[w..2*w];let design=&input[2*w..];let mut frame=input[..w].to_vec();frame[16..16+n].copy_from_slice(&previous[16..16+n]);let old=m.fields(r.reference_gradients,previous,design);let mut state=old.state.clone();let dt=frame[ls];
 if enabled && let Some(history)=&m.history {let start=m.layout.material_start();let f=m.fields(r.reference_gradients,&frame,design);let mut residual=vec![S::zero();n-start];history.residual(&old.state[start..],&old.state[start..],f.prop.temperature,&f.stress,dt,&frame[ls+2..],&mut residual);for (index,value) in (start..n).zip(residual){state[index]=old.state[index]-value;}frame[16..16+n].copy_from_slice(&state.iter().zip(&m.scales).map(|(v,s)|*v / *s).collect::<Vec<_>>());}
 let fields=m.fields(r.reference_gradients,&frame,design);let mut iterations=0;let mut relaxed_stress=fields.stress;
 if enabled && let Some(law)=m.creep {let (a,fa,count)=root(law,&fields.stress,&fields.prop,dt)?;iterations=count;let constant=S::from_f64(a);let fixed=equation(law,&fields.stress,&fields.prop,dt,constant);let implicit=constant-(fixed-S::from_f64(fixed.value()))/fa;let stress=scaled(&fields.stress,implicit);relaxed_stress=stress;let range=m.layout.creep();let mut residual=vec![S::zero();range.len()];law.residual(&stress,&old.state[range.clone()],&old.state[range.clone()],&fields.prop,dt,&mut residual);for (index,value) in range.zip(residual){state[index]=old.state[index]-value;}}
 let normalized:Vec<S>=state.iter().zip(&m.scales).map(|(v,s)|*v / *s).collect();frame[16..16+n].copy_from_slice(&normalized);let endpoint=m.fields(r.reference_gradients,&frame,design);let heat=if enabled {m.creep.map_or(S::zero(),|law|law.dissipated_increment(&relaxed_stress,&state[m.layout.creep()],&old.state[m.layout.creep()]))*m.stiffness(design[0])}else{S::zero()};
 let heat = if enabled {
  if let Some(history) = &m.history {
   let start = m.layout.material_start();
   heat + design[0] * history.energy(&state[start..], endpoint.prop.temperature, &frame[ls+2..], design[4]).sensible_heat * dt
  } else { heat }
 } else { heat };

 Ok((normalized,heat,iterations))
}
pub fn stress_relaxing_age_condition(r:&LocalAdvanceRequest<'_>,enabled:bool)->CaeResult<LocalAdvanceResult> {
 let m=r.model;let w=m.local_width();let n=m.internal_size;let ls=m.local_size();
 if r.driving.len()!=w || r.previous.len()!=w || r.design.len()!=5 || n==0 || m.creep.is_some_and(|c| c != crate::inelastic::CreepLaw::Norton) || m.plastic.is_some() || m.viscoelastic.is_some() || m.history.as_ref().is_some_and(|h|!h.law.supports_closed_inventory_step()){return Err(CaeError::contract("stress relaxing age condition requires creep and closed inventory native history"));}
 let input:Vec<f64>=r.driving.iter().chain(r.previous).chain(r.design).copied().collect();
 if input.iter().any(|v|!v.is_finite()) || r.driving[..4].iter().chain(&r.previous[..4]).any(|t|m.t0+m.ts*t<=0.) || r.driving[ls]<0. || !(0.0..=1.0).contains(&r.design[0]) || !(0.0..=1.0).contains(&r.design[4]) || r.design[1..4].iter().any(|v|*v<=0.) {return Err(CaeError::contract("invalid native stress relaxing age condition inputs"));}
 let (state,heat,iterations)=evaluate(r,&input,enabled)?;let mut frame=r.driving.to_vec();frame[16..16+n].copy_from_slice(&state);let endpoint=m.fields(r.reference_gradients,&frame,r.design);let old=m.fields(r.reference_gradients,r.previous,r.design);
 if let Some(history)=&m.history {history.check_state(&old.state[m.layout.material_start()..])?;history.check_state(&endpoint.state[m.layout.material_start()..])?;}
 let mut norm:f64=0.;if enabled && let Some(law)=m.creep {let range=m.layout.creep();let mut residual=vec![0.;range.len()];law.residual(&endpoint.stress,&endpoint.state[range.clone()],&old.state[range.clone()],&endpoint.prop,r.driving[ls],&mut residual);for (row,v) in range.zip(residual){norm=norm.max((v/m.scales[row]).abs());}}
 let mut tangent=DenseMatrix::zeros(n,input.len());let mut heat_gradient=vec![0.;input.len()];
 for start in (0..input.len()).step_by(8) {
  let end=(start+8).min(input.len());let mut seeded:Vec<Dual<8>>=input.iter().map(|v|Dual::constant(*v)).collect();
  for column in start..end {seeded[column]=Dual::variable(input[column],column-start);}
  let (h,q,_)=evaluate(r,&seeded,enabled)?;
  for column in start..end {for row in 0..n {tangent.data[row*input.len()+column]=h[row].eps[column-start];}heat_gradient[column]=q.eps[column-start];}
 }
 if state.iter().chain(&tangent.data).chain(&heat_gradient).any(|v|!v.is_finite()) || !heat.is_finite() || heat<0. || !norm.is_finite() {return Err(CaeError::convergence(format!("native stress relaxing age condition failed prescribed-state or dissipation certificate: original_normalized_row_max={norm}, integrated_heat_J_m3={heat}")));}
 Ok(LocalAdvanceResult{normalized_history:state,integrated_heat_J_m3:heat,residual_norm:norm,iterations,local_condition:1.,input_width:input.len(),state_input_jacobian:tangent,heat_input_gradient:heat_gradient})
}
