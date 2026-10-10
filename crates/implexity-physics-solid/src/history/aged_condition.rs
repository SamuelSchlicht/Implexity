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
 if r.driving.len()!=w || r.previous.len()!=w || r.design.len()!=5 || n==0 || m.creep.is_some_and(|c| c != crate::inelastic::CreepLaw::Norton) || m.plastic.is_some() || m.viscoelastic.is_some() || m.history.as_ref().is_some_and(|h|!h.law.supports_closed_inventory_step()) {return Err(CaeError::contract("frozen reference age condition requires native creep and closed inventory history and exact local layout"));}
 let input:Vec<f64>=r.driving.iter().chain(r.previous).chain(r.design).copied().collect();
 if input.iter().any(|v|!v.is_finite()) || r.driving[..4].iter().chain(&r.previous[..4]).any(|t|m.t0+m.ts*t<=0.) || r.driving[ls]<0. || !(0.0..=1.0).contains(&r.design[0]) || !(0.0..=1.0).contains(&r.design[4]) || r.design[1..4].iter().any(|v|*v<=0.) {return Err(CaeError::contract("frozen reference age condition requires finite inputs, positive temperatures/dimensions and nonnegative age"));}
 let (state,heat)=evaluate(r,&input,enabled);let mut frame=r.driving.to_vec();frame[16..16+n].copy_from_slice(&state);
 if let Some(history)=&m.history {let old=m.fields(r.reference_gradients,r.previous,r.design);history.check_state(&old.state[m.layout.material_start()..])?;let fields=m.fields(r.reference_gradients,&frame,r.design);history.check_state(&fields.state[m.layout.material_start()..])?;}
 let mut tangent=DenseMatrix::zeros(n,input.len());let mut heat_gradient=vec![0.;input.len()];
 for column in 0..input.len() {let mut seeded:Vec<Dual<1>>=input.iter().map(|v|Dual::constant(*v)).collect();seeded[column]=Dual::variable(input[column],0);let (h,q)=evaluate(r,&seeded,enabled);for row in 0..n {tangent.data[row*input.len()+column]=h[row].eps[0];}heat_gradient[column]=q.eps[0];}
 if state.iter().chain(&tangent.data).chain(&heat_gradient).any(|v|!v.is_finite()) || !heat.is_finite() || heat<0. {return Err(CaeError::convergence("nonfinite frozen reference age condition or negative native dissipation"));}
 Ok(LocalAdvanceResult{normalized_history:state,integrated_heat_J_m3:heat,residual_norm:0.,iterations:0,local_condition:1.,input_width:input.len(),state_input_jacobian:tangent,heat_input_gradient:heat_gradient})
}
