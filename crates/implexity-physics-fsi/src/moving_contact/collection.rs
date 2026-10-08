// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{matrix::Jacobian,time_stepper::StepParameters};
use crate::moving_contact::contact_field::{NativeContactLaw,ContactContribution};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct MultipleContact<L>{laws:Vec<L>,native:usize,fluxes:usize,design:usize}
impl<L:NativeContactLaw> MultipleContact<L>{
 pub fn new(laws:Vec<L>,native:usize,fluxes:usize,design:usize)->CaeResult<Self>{if laws.is_empty()||native==0||fluxes==0||laws.iter().any(|l|l.multipliers()!=1)||native.checked_add(laws.len()).is_none(){return Err(fail("multiple contact layout"));}Ok(Self{laws,native,fluxes,design})}
 fn local(&self,x:&[f64],k:usize)->CaeResult<Vec<f64>>{if x.len()!=self.native+self.laws.len()||x.iter().any(|v|!v.is_finite()){return Err(fail("multiple contact complete state"));}let mut v=x[..self.native].to_vec();v.push(x[self.native+k]);Ok(v)}
 fn matrix(&self,blocks:&[Jacobian],constraint:bool,state_columns:bool)->CaeResult<Jacobian>{let rows=if constraint{self.laws.len()}else{self.fluxes};let cols=if state_columns{self.native+self.laws.len()}else{self.design};let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);for(k,j)in blocks.iter().enumerate(){let a=j.to_csr()?;if a.nrows()!=if constraint{1}else{self.fluxes}||a.ncols()!=if state_columns{self.native+1}else{self.design}{return Err(fail("multiple contact block shape"));}for i in 0..a.nrows(){let(js,v)=a.row(i);for(&c,&x)in js.iter().zip(v){ri.push(if constraint{k}else{i});ci.push(if state_columns&&c==self.native{self.native+k}else{c});vs.push(x);}}}Ok(Jacobian::Csr(CsrMatrix::from_triplets(rows,cols,&ri,&ci,&vs).map_err(|e|fail(&e.to_string()))?))}
 pub fn laws(&self)->&[L]{&self.laws}
 pub fn contact_count(&self)->usize{self.laws.len()}
 pub fn into_parts(self)->(Vec<L>,usize,usize,usize){(self.laws,self.native,self.fluxes,self.design)}
}
impl<L:NativeContactLaw> NativeContactLaw for MultipleContact<L>{
 fn multipliers(&self)->usize{self.laws.len()}
 fn check_derivative_domain(&self,n:usize,x:&[f64],old:&[f64],p:StepParameters<'_>)->CaeResult<()>{for(k,l)in self.laws.iter().enumerate(){l.check_derivative_domain(n,&self.local(x,k)?,&self.local(old,k)?,p)?;}Ok(())}
 fn check_state_domain(&self,n:usize,x:&[f64],old:&[f64],p:StepParameters<'_>)->CaeResult<()>{for(k,l)in self.laws.iter().enumerate(){l.check_state_domain(n,&self.local(x,k)?,&self.local(old,k)?,p)?;}Ok(())}
 fn evaluate(&self,n:usize,x:&[f64],old:&[f64],p:StepParameters<'_>)->CaeResult<ContactContribution>{let mut parts=vec![];for(k,l)in self.laws.iter().enumerate(){parts.push(l.evaluate(n,&self.local(x,k)?,&self.local(old,k)?,p)?);}let mut force=vec![0.;self.fluxes];let mut force_time=vec![0.;self.fluxes];let mut constraints=vec![];let mut constraint_time=vec![];for a in &parts{if a.force.len()!=self.fluxes||a.force_time.len()!=self.fluxes||a.constraints.len()!=1||a.constraint_time.len()!=1{return Err(fail("multiple contact value shape"));}for i in 0..self.fluxes{force[i]+=a.force[i];force_time[i]+=a.force_time[i];}constraints.extend(&a.constraints);constraint_time.extend(&a.constraint_time);}if force.iter().chain(&force_time).chain(&constraints).chain(&constraint_time).any(|v|!v.is_finite()){return Err(fail("multiple contact value overflow"));}Ok(ContactContribution{force,constraints,force_time,constraint_time,force_current:self.matrix(&parts.iter().map(|a|a.force_current.clone()).collect::<Vec<_>>(),false,true)?,force_previous:self.matrix(&parts.iter().map(|a|a.force_previous.clone()).collect::<Vec<_>>(),false,true)?,force_design:self.matrix(&parts.iter().map(|a|a.force_design.clone()).collect::<Vec<_>>(),false,false)?,constraint_current:self.matrix(&parts.iter().map(|a|a.constraint_current.clone()).collect::<Vec<_>>(),true,true)?,constraint_previous:self.matrix(&parts.iter().map(|a|a.constraint_previous.clone()).collect::<Vec<_>>(),true,true)?,constraint_design:self.matrix(&parts.iter().map(|a|a.constraint_design.clone()).collect::<Vec<_>>(),true,false)?})}
 fn admitted_trial(&self,n:usize,x:&[f64],d:&[f64],old:&[f64],p:StepParameters<'_>,trial:f64)->CaeResult<f64>{if !trial.is_finite()||!(0. ..=1.).contains(&trial){return Err(fail("multiple contact trial fraction"));}let mut a=trial;if a==0.{return Ok(0.);}for _ in 0..24{let mut reduced=false;for(k,l)in self.laws.iter().enumerate(){let bound=l.admitted_trial(n,&self.local(x,k)?,&self.local(d,k)?,&self.local(old,k)?,p,a)?;if !bound.is_finite()||bound<0.||bound>a{return Err(fail("multiple contact trial bound"));}if bound<a{a=bound;reduced=true;break;}}if a==0.{return Ok(0.);}if !reduced{return Ok(a);}}Err(fail("multiple contact all-law admission did not settle"))}
 fn newton_constraints(&self,n:usize,x:&[f64],old:&[f64],p:StepParameters<'_>,d:&[f64],attempt:usize)->CaeResult<Option<Jacobian>>{let mut changed=false;let mut rows=vec![];for(k,l)in self.laws.iter().enumerate(){let a=self.local(x,k)?;let b=self.local(old,k)?;let v=self.local(d,k)?;if let Some(j)=l.newton_constraints(n,&a,&b,p,&v,attempt)?{changed=true;rows.push(j);}else{rows.push(l.evaluate(n,&a,&b,p)?.constraint_current);}}if changed{Ok(Some(self.matrix(&rows,true,true)?))}else{Ok(None)}}
}

pub struct InitialContactEquilibrium{pub state:Vec<f64>,pub residual_infinity_norm:f64,pub multipliers:usize}
pub fn initial_rest_equilibrium<F:implexity_solve::multirate_coupling::FluxDrivenField,L:NativeContactLaw>(field:&crate::moving_contact::contact_field::ContactField<F,L>,design:&[f64],external:&[f64],time_scale:f64,tolerance:f64)->CaeResult<InitialContactEquilibrium>{
 use implexity_solve::multirate_coupling::FluxDrivenField;
 if !time_scale.is_finite()||time_scale<=0.||!tolerance.is_finite()||tolerance<=0.||external.len()!=field.trace_operator().nrows()||external.iter().any(|v|!v.is_finite()){return Err(fail("initial contact equilibrium explicit inputs"));}
 let state=field.initial_state(design)?;let p=StepParameters{design,time_scale};let residual=field.residual(1,&state,&state,external,p)?;if residual.iter().any(|v|!v.is_finite()){return Err(fail("initial contact equilibrium residual"));}let norm=residual.iter().map(|v|v.abs()).fold(0.,f64::max);if norm>tolerance{return Err(fail("initial contact state is not an equilibrium; solve/preload required"));}field.check_state_domain(1,&state,&state,p)?;Ok(InitialContactEquilibrium{state,residual_infinity_norm:norm,multipliers:field.law().multipliers()})
}

impl MultipleContact<crate::moving_contact::mapped_pair_law::MappedPairLaw>{
 pub fn native_states(&self)->usize{self.native}
 pub fn scaled_gap_rows(&self,x:&[f64],design:&[f64])->CaeResult<(Vec<f64>,implexity_solve::matrix::Jacobian)>{let mut gaps=vec![];let mut rows=vec![];for(k,l)in self.laws.iter().enumerate(){let(g,j)=l.gap_current_matrix(&self.local(x,k)?,design)?;gaps.push(g);rows.push(j);}Ok((gaps,self.matrix(&rows,true,true)?))}
 pub fn inverse_force_scales(&self,x:&[f64],p:StepParameters<'_>)->CaeResult<Vec<f64>>{let mut scales=vec![];for(k,l)in self.laws.iter().enumerate(){let mut local=self.local(x,k)?;let gap=l.instantaneous_row(&local,p.design)?.1;if gap<0.{return Err(fail("contact transition previous penetration"));}local[self.native]=0.;let row=if gap==0.{let mut direction=vec![0.;local.len()];direction[self.native]=-1.;l.newton_constraints(1,&local,&local,p,&direction,0)?.ok_or_else(||fail("native contact origin scale row missing"))?}else{l.evaluate(1,&local,&local,p)?.constraint_current};let row=row.to_csr()?;let(columns,values)=row.row(0);let coefficient: f64=columns.iter().zip(values).filter(|(j,_)|**j==self.native).map(|(_,v)|*v).sum();if !coefficient.is_finite()||coefficient>=0.{return Err(fail("native contact force scale row"));}scales.push(-coefficient);}Ok(scales)}
 pub fn normal_velocities(&self,x:&[f64],design:&[f64],velocity:&[f64])->CaeResult<Vec<f64>>{if velocity.len()!=self.fluxes||velocity.iter().any(|v|!v.is_finite()){return Err(fail("contact physical velocity trace"));}let mut result:Vec<f64>=vec![];for(k,l)in self.laws.iter().enumerate(){let(row,_)=l.instantaneous_row(&self.local(x,k)?,design)?;result.push(row.iter().zip(velocity).map(|(a,b)|a*b).sum());}if result.iter().any(|v|!v.is_finite()){return Err(fail("contact normal velocity overflow"));}Ok(result)}
}

impl MultipleContact<crate::moving_contact::mapped_pair_law::MappedPairLaw>{
 pub fn scaled_gap_design_rows(&self,x:&[f64],design:&[f64])->CaeResult<Jacobian>{let mut rows=vec![];for(k,l)in self.laws.iter().enumerate(){rows.push(l.gap_design_matrix(&self.local(x,k)?,design)?);}self.matrix(&rows,true,false)}
}

impl MultipleContact<crate::moving_contact::mapped_pair_law::MappedPairLaw>{
 pub fn instantaneous_rows(&self,x:&[f64],design:&[f64])->CaeResult<(Vec<Vec<f64>>,Vec<f64>)>{let(mut rows,mut gaps)=(vec![],vec![]);for(k,l)in self.laws.iter().enumerate(){let(row,gap)=l.instantaneous_row(&self.local(x,k)?,design)?;if row.len()!=self.fluxes||row.iter().any(|v|!v.is_finite())||!gap.is_finite(){return Err(fail("multiple contact physical gap/velocity row"));}rows.push(row);gaps.push(gap);}Ok((rows,gaps))}
}

impl MultipleContact<crate::moving_contact::mapped_pair_law::MappedPairLaw>{
 pub fn with_residual_path_allowance(mut self,tolerance:f64)->CaeResult<Self>{self.laws=self.laws.into_iter().map(|law|law.with_residual_path_allowance(tolerance)).collect::<CaeResult<Vec<_>>>()?;Ok(self)}
 pub fn residual_path_certificates(&self,previous:&[f64],current:&[f64],design:&[f64],tolerance:f64)->CaeResult<Vec<crate::moving_contact::linear_path::ApproximatePathCertificate>>{self.laws.iter().enumerate().map(|(k,law)|law.residual_path_certificate(&self.local(previous,k)?,&self.local(current,k)?,design,tolerance)).collect()}
}

impl MultipleContact<crate::moving_contact::mapped_pair_law::MappedPairLaw>{
 pub fn fixed_reference_geometry(&self)->bool{self.laws.iter().all(|l|l.fixed_reference_geometry())}
}

impl<L:super::contact_set_kinematics::ContactFeatureKinematics> super::contact_set_kinematics::ContactSetKinematics for MultipleContact<L>{
 fn check_selected_geometry_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>{for(k,l)in self.laws.iter().enumerate(){l.check_selected_geometry_domain(n,&self.local(current,k)?,&self.local(previous,k)?,p)?;}Ok(())}
 fn native_states(&self)->usize{self.native}
 fn scaled_gap_rows(&self,x:&[f64],design:&[f64])->CaeResult<(Vec<f64>,implexity_solve::matrix::Jacobian)>{let mut gaps=vec![];let mut rows=vec![];for(k,l)in self.laws.iter().enumerate(){let(g,j)=l.gap_current_matrix(&self.local(x,k)?,design)?;gaps.push(g);rows.push(j);}Ok((gaps,self.matrix(&rows,true,true)?))}
 fn inverse_force_scales(&self,x:&[f64],p:StepParameters<'_>)->CaeResult<Vec<f64>>{let mut scales=vec![];for(k,l)in self.laws.iter().enumerate(){let mut local=self.local(x,k)?;let gap=l.instantaneous_row(&local,p.design)?.1;if gap<0.{return Err(fail("contact transition previous penetration"));}local[self.native]=0.;let row=if gap==0.{let mut direction=vec![0.;local.len()];direction[self.native]=-1.;l.newton_constraints(1,&local,&local,p,&direction,0)?.ok_or_else(||fail("native contact origin scale row missing"))?}else{l.evaluate(1,&local,&local,p)?.constraint_current};let row=row.to_csr()?;let(columns,values)=row.row(0);let coefficient: f64=columns.iter().zip(values).filter(|(j,_)|**j==self.native).map(|(_,v)|*v).sum();if !coefficient.is_finite()||coefficient>=0.{return Err(fail("native contact force scale row"));}scales.push(-coefficient);}Ok(scales)}
 fn normal_velocities(&self,x:&[f64],design:&[f64],velocity:&[f64])->CaeResult<Vec<f64>>{if velocity.len()!=self.fluxes||velocity.iter().any(|v|!v.is_finite()){return Err(fail("contact physical velocity trace"));}let mut result:Vec<f64>=vec![];for(k,l)in self.laws.iter().enumerate(){let(row,_)=l.instantaneous_row(&self.local(x,k)?,design)?;result.push(row.iter().zip(velocity).map(|(a,b)|a*b).sum());}if result.iter().any(|v|!v.is_finite()){return Err(fail("contact normal velocity overflow"));}Ok(result)}

 fn scaled_gap_design_rows(&self,x:&[f64],design:&[f64])->CaeResult<Jacobian>{let mut rows=vec![];for(k,l)in self.laws.iter().enumerate(){rows.push(l.gap_design_matrix(&self.local(x,k)?,design)?);}self.matrix(&rows,true,false)}
 fn instantaneous_rows(&self,x:&[f64],design:&[f64])->CaeResult<(Vec<Vec<f64>>,Vec<f64>)>{let(mut rows,mut gaps)=(vec![],vec![]);for(k,l)in self.laws.iter().enumerate(){let(row,gap)=l.instantaneous_row(&self.local(x,k)?,design)?;if row.len()!=self.fluxes||row.iter().any(|v|!v.is_finite())||!gap.is_finite(){return Err(fail("multiple contact physical gap/velocity row"));}rows.push(row);gaps.push(gap);}Ok((rows,gaps))}
 fn residual_path_certificates(&self,previous:&[f64],current:&[f64],design:&[f64],tolerance:f64)->CaeResult<Vec<crate::moving_contact::linear_path::ApproximatePathCertificate>>{self.laws.iter().enumerate().map(|(k,law)|law.residual_path_certificate(&self.local(previous,k)?,&self.local(current,k)?,design,tolerance)).collect()}
 fn fixed_reference_geometry(&self)->bool{self.laws.iter().all(|l|l.fixed_reference_geometry())}
}
