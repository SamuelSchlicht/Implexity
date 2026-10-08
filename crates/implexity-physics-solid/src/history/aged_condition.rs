// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::{Dual,Scalar};
use implexity_core::error::{CaeError,CaeResult};
use implexity_linalg::dense::DenseMatrix;
use super::{local_advance::{LocalAdvanceRequest,LocalAdvanceResult}};

fn evaluate<S:Scalar>(r:&LocalAdvanceRequest<'_>,input:&[S],enabled:bool)->(Vec<S>,S) {
 let m=r.model;let w=m.local_width();let n=m.internal_size;let ls=m.local_size();
 let previous=&input[w..2*w];let design=&input[2*w..];let mut reference=input[..w].to_vec();
 reference[16..16+n].copy_from_slice(&previous[16..16+n]);
 let fields=m.fields(r.reference_gradients,&reference,design);
 let old=m.fields(r.reference_gradients,previous,design);let mut state=old.state.clone();let dt=reference[ls];
 if enabled {
  if let Some(law)=m.creep {let range=m.layout.creep();let mut residual=vec![S::zero();range.len()];law.residual(&fields.stress,&old.state[range.clone()],&old.state[range.clone()],&fields.prop,dt,&mut residual);for (index,value) in range.zip(residual) {state[index]=old.state[index]-value;}}
  if let Some(history)=&m.history {let start=m.layout.material_start();let mut residual=vec![S::zero();n-start];history.residual(&old.state[start..],&old.state[start..],fields.prop.temperature,&fields.stress,dt,&reference[ls+2..],&mut residual);for (index,value) in (start..n).zip(residual) {state[index]=old.state[index]-value;}}
 }
 let heat=if enabled {m.creep.map_or(S::zero(),|law|law.dissipated_increment(&fields.stress,&state[m.layout.creep()],&old.state[m.layout.creep()]))*m.stiffness(design[0])}else{S::zero()};
 let heat = if enabled {
  if let Some(history) = &m.history {
   let start = m.layout.material_start();
   heat + design[0] * history.energy(&state[start..], fields.prop.temperature, &reference[ls+2..], design[4]).sensible_heat * dt
  } else { heat }
 } else { heat };

 (state.iter().zip(&m.scales).map(|(value,scale)|*value / *scale).collect(),heat)
}

pub fn frozen_reference_age_condition(r:&LocalAdvanceRequest<'_>,enabled:bool)->CaeResult<LocalAdvanceResult> {
 let m=r.model;let w=m.local_width();let n=m.internal_size;let ls=m.local_size();
 if r.driving.len()!=w || r.previous.len()!=w || r.design.len()!=5 || n==0 || m.plastic.is_some() || m.viscoelastic.is_some() || m.history.as_ref().is_some_and(|h|!h.law.supports_closed_inventory_step()) {return Err(CaeError::contract("frozen reference age condition requires native creep and closed inventory history and exact local layout"));}
 let input:Vec<f64>=r.driving.iter().chain(r.previous).chain(r.design).copied().collect();
 if input.iter().any(|v|!v.is_finite()) || r.driving[..4].iter().chain(&r.previous[..4]).any(|t|m.t0+m.ts*t<=0.) || r.driving[ls]<0. || !(0.0..=1.0).contains(&r.design[0]) || !(0.0..=1.0).contains(&r.design[4]) || r.design[1..4].iter().any(|v|*v<=0.) {return Err(CaeError::contract("frozen reference age condition requires finite inputs, positive temperatures/dimensions and nonnegative age"));}
 let (state,heat)=evaluate(r,&input,enabled);let mut frame=r.driving.to_vec();frame[16..16+n].copy_from_slice(&state);
 if let Some(history)=&m.history {let old=m.fields(r.reference_gradients,r.previous,r.design);history.check_state(&old.state[m.layout.material_start()..])?;let fields=m.fields(r.reference_gradients,&frame,r.design);history.check_state(&fields.state[m.layout.material_start()..])?;}
 let mut tangent=DenseMatrix::zeros(n,input.len());let mut heat_gradient=vec![0.;input.len()];
 for column in 0..input.len() {let mut seeded:Vec<Dual<1>>=input.iter().map(|v|Dual::constant(*v)).collect();seeded[column]=Dual::variable(input[column],0);let (h,q)=evaluate(r,&seeded,enabled);for row in 0..n {tangent.data[row*input.len()+column]=h[row].eps[0];}heat_gradient[column]=q.eps[0];}
 if state.iter().chain(&tangent.data).chain(&heat_gradient).any(|v|!v.is_finite()) || !heat.is_finite() || heat<0. {return Err(CaeError::convergence("nonfinite frozen reference age condition or negative native dissipation"));}
 Ok(LocalAdvanceResult{normalized_history:state,integrated_heat_J_m3:heat,residual_norm:0.,iterations:0,local_condition:1.,input_width:input.len(),state_input_jacobian:tangent,heat_input_gradient:heat_gradient})
}
