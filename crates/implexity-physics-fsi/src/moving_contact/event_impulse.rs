// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::{model::MovingFsiModel,impact::{self,MassMetric,ImpactTolerance,Impact}};
use implexity_core::{CaeError,CaeResult};
use implexity_linalg::{sparse::CsrMatrix,lu::{SparseLu,LuSymbolic,Parallelism}};
use implexity_solve::{time_stepper::StepParameters,multirate_coupling::{FluxDrivenField,SubcycledField}};
use implexity_ad::Dual;
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
pub(super) struct PhysicalMass {matrix:CsrMatrix,fixed:Vec<bool>,free:Vec<usize>,factor:SparseLu}
impl PhysicalMass {
 pub(super) fn new(matrix:CsrMatrix,fixed:Vec<bool>)->CaeResult<Self>{
  let free:Vec<_>=(0..fixed.len()).filter(|i|!fixed[*i]).collect();let mut map=vec![usize::MAX;fixed.len()];for(i,&j)in free.iter().enumerate(){map[j]=i;}
  let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);for(i,&r)in free.iter().enumerate(){let(js,v)=matrix.row(r);for(&c,&x)in js.iter().zip(v){if map[c]!=usize::MAX{ri.push(i);ci.push(map[c]);vs.push(x);}}}
  let sub=CsrMatrix::from_triplets(free.len(),free.len(),&ri,&ci,&vs).map_err(fail)?.to_csc();let factor=LuSymbolic::analyze(&sub).and_then(|s|s.factor(&sub,Parallelism::Sequential)).map_err(fail)?;
  Ok(Self{matrix,fixed,free,factor})
 }
}
impl MassMetric for PhysicalMass {
 fn size(&self)->usize{self.fixed.len()}fn fixed(&self)->&[bool]{&self.fixed}
 fn apply(&self,v:&[f64])->CaeResult<Vec<f64>>{self.matrix.matvec(v).map_err(fail)}
 fn inverse_free(&self,f:&[f64])->CaeResult<Vec<f64>>{let rhs:Vec<_>=self.free.iter().map(|i|f[*i]).collect();let x=self.factor.solve(&rhs).map_err(fail)?;let mut out=vec![0.;self.size()];for(&i,&v)in self.free.iter().zip(&x){out[i]=v;}Ok(out)}
}
pub struct SolidImpactEvent<'a>{pub solid:Vec<f64>,pub fluid_unchanged:&'a[f64],pub absolute_time_s:f64,pub impulse:Impact,pub(super) tolerance:ImpactTolerance}
pub struct AugmentedImpactEvent{pub state:Vec<f64>,pub absolute_time_s:f64,pub impulse:Impact}
impl MovingFsiModel {
 pub fn apply_augmented_impact(&self,state:&[f64],design:&[f64],time_scale:f64,absolute_time_s:f64,restitution:f64,tolerance:ImpactTolerance)->CaeResult<AugmentedImpactEvent>{
  let layout=self.stepper()?.layout();if state.len()!=layout.lags.end||state.iter().any(|x|!x.is_finite()){return Err(fail("augmented impact state shape"));}
  let event=self.apply_solid_impact(&state[layout.field_b.clone()],&state[layout.field_a.clone()],design,time_scale,absolute_time_s,restitution,tolerance)?;
  let mut out=state.to_vec();out[layout.field_b].copy_from_slice(&event.solid);Ok(AugmentedImpactEvent{state:out,absolute_time_s,impulse:event.impulse})
 }
 pub fn apply_solid_impact<'a>(&self,solid:&[f64],fluid:&'a[f64],design:&[f64],time_scale:f64,absolute_time_s:f64,restitution:f64,tolerance:ImpactTolerance)->CaeResult<SolidImpactEvent<'a>>{
  if !absolute_time_s.is_finite()||absolute_time_s<0.||fluid.len()!=self.native().fluid_field()?.state_size()||fluid.iter().any(|x|!x.is_finite()||*x<0.){return Err(fail("native instantaneous event/fluid domain"));}
  let contact=self.contact_field()?;let core=contact.native().core();let model=core.model();let params=core.params(design)?;
  let history=core.history(StepParameters{design,time_scale})?;let native_len=contact.native().state_size();if solid.len()!=native_len+1||solid.iter().any(|x|!x.is_finite()){return Err(fail("native instantaneous solid state"));}
  let physical=core.to_physical(&solid[..native_len]);let layout=&history.layout;let v=physical[layout.v()..layout.v()+layout.n3].to_vec();
  let removed=self.native().problem.solid.inertia_compensation*self.native().problem.fluid.density_kg_m3;let ratio:Vec<_>=(0..model.ne()).map(|e|{let density=model.materials[model.element_material[e]].density;(density+removed)/density}).collect();if ratio.iter().any(|v|!v.is_finite()||*v<1.){return Err(fail("physical impact density restoration refused"));}let factors:Vec<_>=(0..model.ne()).map(|e|model.interpolation.mass(params[e])*ratio[e]).collect();let matrix=model.global_mass_and_reference(&factors,&vec![0.;model.ne()])?.0;let mass=PhysicalMass::new(matrix,model.fixed.clone())?;
  let(row,gap)=contact.law().instantaneous_row(solid,design)?;if row.len()!=layout.n3{return Err(fail("native contact velocity row shape"));}
  let impulse=impact::solve(&mass,&v,&[row],&[restitution],&[gap],tolerance)?;let mut post=physical;post[layout.v()..layout.v()+layout.n3].copy_from_slice(&impulse.velocity);let mut result=core.to_scaled(&post);result.push(solid[native_len]);
  for i in 0..native_len{if !(layout.v()..layout.v()+layout.n3).contains(&i){result[i]=solid[i];}}
  Ok(SolidImpactEvent{solid:result,fluid_unchanged:fluid,absolute_time_s,impulse,tolerance})
 }
 pub fn solid_impact_direction(&self,before:&[f64],design:&[f64],time_scale:f64,base:&SolidImpactEvent<'_>,direction:&[f64],design_direction:&[f64],restitution:f64,restitution_direction:f64)->CaeResult<Vec<f64>>{
  let fresh=self.apply_solid_impact(before,base.fluid_unchanged,design,time_scale,base.absolute_time_s,restitution,base.tolerance)?;if fresh.solid.len()!=base.solid.len()||fresh.solid.iter().zip(&base.solid).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("impact direction base identity differs"));}let contact=self.contact_field()?;let core=contact.native().core();let model=core.model();let n=contact.native().state_size();if before.len()!=n+1||direction.len()!=n+1||direction.iter().any(|x|!x.is_finite()){return Err(fail("native impact state direction shape"));}
  let params=core.params(design)?;let dp=core.params(design_direction)?;let history=core.history(StepParameters{design,time_scale})?;let l=&history.layout;
  let physical=core.to_physical(&before[..n]);let dphysical=core.to_physical(&direction[..n]);let v=&physical[l.v()..l.v()+l.n3];let dv=&dphysical[l.v()..l.v()+l.n3];
  let removed=self.native().problem.solid.inertia_compensation*self.native().problem.fluid.density_kg_m3;let ratio:Vec<_>=(0..model.ne()).map(|e|{let density=model.materials[model.element_material[e]].density;(density+removed)/density}).collect();if ratio.iter().any(|v|!v.is_finite()||*v<1.){return Err(fail("physical impact density restoration refused"));}let factors:Vec<_>=(0..model.ne()).map(|e|model.interpolation.mass(params[e])*ratio[e]).collect();let df:Vec<_>=(0..model.ne()).map(|i|model.interpolation.mass(Dual::new(params[i],[dp[i]])).eps[0]*ratio[i]).collect();
  let mass=PhysicalMass::new(model.global_mass_and_reference(&factors,&vec![0.;model.ne()])?.0,model.fixed.clone())?;let dm=model.global_mass_and_reference(&df,&vec![0.;model.ne()])?.0;
  let(row,_)=contact.law().instantaneous_row(before,design)?;let(dr,_)=contact.law().instantaneous_row_direction(before,design,direction,design_direction)?;
  let(dv,_)=impact::direction(&mass,&fresh.impulse,v,&[row],&[restitution],dv,&[dr],&[restitution_direction],&|x|dm.matvec(x).map_err(fail))?;
  let mut dp=dphysical;dp[l.v()..l.v()+l.n3].copy_from_slice(&dv);let mut out=core.to_scaled(&dp);out.push(direction[n]);for i in 0..n{if !(l.v()..l.v()+l.n3).contains(&i){out[i]=direction[i];}}Ok(out)
 }
}
