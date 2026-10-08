// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{matrix::{Jacobian,FnAction,checked_product},multirate_coupling::{FluxDrivenField,SubcycledField,FieldJacobians,MultirateStepper,MultirateOptions},time_stepper::StepParameters};
use implexity_physics_solid::soft_fsi::field::SoftSolidField;
use implexity_physics_lbm::moving::{carrier::{LagrangianCarrier,PointCloud},field::AnyMovingLbm};
use std::sync::Arc;
use crate::{model::FsiModel,problem::FsiProblem,interface::FluidField};
use super::{model::MovingContactSpec,contact_field::{ContactField,NativeContactLaw},mapped_pair_law::{MappedPairLaw,NodeBinding}};
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
fn block_csr(a:&CsrMatrix,b:&CsrMatrix)->CaeResult<CsrMatrix>{
 let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);for(k,x)in [a,b].into_iter().enumerate(){let ro=if k==0{0}else{a.nrows()};let co=if k==0{0}else{a.ncols()};for r in 0..x.nrows(){let(js,v)=x.row(r);for(&j,&v)in js.iter().zip(v){ri.push(ro+r);ci.push(co+j);vs.push(v);}}}CsrMatrix::from_triplets(a.nrows()+b.nrows(),a.ncols()+b.ncols(),&ri,&ci,&vs).map_err(fail)
}
fn block_jac(a:Jacobian,b:Jacobian)->Jacobian{
 let(ar,ac)=a.shape();let(br,bc)=b.shape();let at=a.clone();let bt=b.clone();Jacobian::Operator(Arc::new(FnAction::new((ar+br,ac+bc),move|x|{if x.len()!=ac+bc{return Err(fail("two-body direction shape"));}let mut y=checked_product(&a,&x[..ac],"body0 direction",false)?;y.extend(checked_product(&b,&x[ac..],"body1 direction",false)?);Ok(y)},move|x|{if x.len()!=ar+br{return Err(fail("two-body cotangent shape"));}let mut y=checked_product(&at,&x[..ar],"body0 cotangent",true)?;y.extend(checked_product(&bt,&x[ar..],"body1 cotangent",true)?);Ok(y)})))
}
pub struct PairSolidField<'a>{bodies:[SoftSolidField<'a>;2],trace:CsrMatrix,samples:Vec<String>,ns:[usize;2],nd:[usize;2],nf:[usize;2],ny:[usize;2]}
impl<'a> PairSolidField<'a>{
 pub fn body_field(&self,index:usize)->CaeResult<&SoftSolidField<'a>>{self.bodies.get(index).ok_or_else(||fail("native pair body field index"))}

 pub fn new(a:SoftSolidField<'a>,b:SoftSolidField<'a>)->CaeResult<Self>{
  if a.nominal_step_s().to_bits()!=b.nominal_step_s().to_bits(){return Err(fail("two solid bodies require identical native clocks"));}let ns=[a.state_size(),b.state_size()];let nd=[a.design_size(),b.design_size()];let nf=[a.trace_operator().nrows(),b.trace_operator().nrows()];let ny=[a.sample_names().len(),b.sample_names().len()];let trace=block_csr(a.trace_operator(),b.trace_operator())?;let samples=a.sample_names().iter().map(|s|format!("body0:{s}")).chain(b.sample_names().iter().map(|s|format!("body1:{s}"))).collect();Ok(Self{bodies:[a,b],trace,samples,ns,nd,nf,ny})
 }
 fn range(length:[usize;2],b:usize)->std::ops::Range<usize>{if b==0{0..length[0]}else{length[0]..length[0]+length[1]}}
 fn p<'p>(&self,p:StepParameters<'p>,b:usize)->StepParameters<'p>{StepParameters{design:&p.design[Self::range(self.nd,b)],time_scale:p.time_scale}}
 fn check(&self,current:&[f64],previous:&[f64],flux:&[f64],p:StepParameters<'_>)->CaeResult<()>{if current.len()!=self.state_size()||previous.len()!=self.state_size()||flux.len()!=self.nf.iter().sum::<usize>()||p.design.len()!=self.design_size()||current.iter().chain(previous).chain(flux).chain(p.design).any(|v|!v.is_finite())||!p.time_scale.is_finite()||p.time_scale<=0.{return Err(fail("two-body native state/flux/design shape"));}Ok(())}
}
impl FluxDrivenField for PairSolidField<'_>{
 fn local_elimination_groups(&self)->CaeResult<Option<Vec<Vec<usize>>>>{let mut groups=vec![];for b in 0..2{if let Some(local)=self.bodies[b].local_elimination_groups()?{for g in local{let mut shifted=vec![];for index in g{if index>=self.ns[b]{return Err(fail("native body local index outside complete layout"));}shifted.push(index.checked_add(if b==0{0}else{self.ns[0]}).ok_or_else(||fail("native pair local index overflow"))?);}groups.push(shifted);}}}Ok(if groups.is_empty(){None}else{Some(groups)})}
 fn state_size(&self)->usize{self.ns.iter().sum::<usize>()}fn design_size(&self)->usize{self.nd.iter().sum::<usize>()}fn sample_names(&self)->&[String]{&self.samples}fn nominal_step_s(&self)->f64{self.bodies[0].nominal_step_s()}fn trace_operator(&self)->&CsrMatrix{&self.trace}
 fn initial_state(&self,d:&[f64])->CaeResult<Vec<f64>>{if d.len()!=self.design_size(){return Err(fail("two-body initial design shape"));}let mut x=self.bodies[0].initial_state(&d[..self.nd[0]])?;x.extend(self.bodies[1].initial_state(&d[self.nd[0]..])?);Ok(x)}
 fn initial_state_vjp(&self,d:&[f64],bar:&[f64])->CaeResult<Vec<f64>>{if d.len()!=self.design_size()||bar.len()!=self.state_size(){return Err(fail("two-body initial cotangent shape"));}let mut x=self.bodies[0].initial_state_vjp(&d[..self.nd[0]],&bar[..self.ns[0]])?;x.extend(self.bodies[1].initial_state_vjp(&d[self.nd[0]..],&bar[self.ns[0]..])?);Ok(x)}
 fn predict(&self,n:usize,prev:&[f64],older:Option<&[f64]>,p:StepParameters<'_>)->Vec<f64>{if prev.len()!=self.state_size()||p.design.len()!=self.design_size()||older.is_some_and(|x|x.len()!=self.state_size()){return vec![f64::NAN;self.state_size()];}let mut x=vec![];for b in 0..2{x.extend(self.bodies[b].predict(n,&prev[Self::range(self.ns,b)],older.map(|x|&x[Self::range(self.ns,b)]),self.p(p,b)));}x}
 fn residual(&self,n:usize,x:&[f64],old:&[f64],f:&[f64],p:StepParameters<'_>)->CaeResult<Vec<f64>>{self.check(x,old,f,p)?;let mut r=vec![];for b in 0..2{r.extend(self.bodies[b].residual(n,&x[Self::range(self.ns,b)],&old[Self::range(self.ns,b)],&f[Self::range(self.nf,b)],self.p(p,b))?);}Ok(r)}
 fn current_jacobian(&self,n:usize,x:&[f64],old:&[f64],f:&[f64],p:StepParameters<'_>)->CaeResult<Jacobian>{self.check(x,old,f,p)?;let a=self.bodies[0].current_jacobian(n,&x[..self.ns[0]],&old[..self.ns[0]],&f[..self.nf[0]],self.p(p,0))?.to_csr()?;let b=self.bodies[1].current_jacobian(n,&x[self.ns[0]..],&old[self.ns[0]..],&f[self.nf[0]..],self.p(p,1))?.to_csr()?;Ok(Jacobian::Csr(block_csr(&a,&b)?))}
 fn current_flux_jacobians(&self,n:usize,x:&[f64],old:&[f64],f:&[f64],p:StepParameters<'_>)->CaeResult<(Jacobian,Jacobian)>{self.check(x,old,f,p)?;let(ac,af)=self.bodies[0].current_flux_jacobians(n,&x[..self.ns[0]],&old[..self.ns[0]],&f[..self.nf[0]],self.p(p,0))?;let(bc,bf)=self.bodies[1].current_flux_jacobians(n,&x[self.ns[0]..],&old[self.ns[0]..],&f[self.nf[0]..],self.p(p,1))?;Ok((Jacobian::Csr(block_csr(&ac.to_csr()?,&bc.to_csr()?)?),Jacobian::Csr(block_csr(&af.to_csr()?,&bf.to_csr()?)?)))}
 fn jacobians(&self,n:usize,x:&[f64],old:&[f64],f:&[f64],p:StepParameters<'_>)->CaeResult<FieldJacobians>{self.check(x,old,f,p)?;let a=self.bodies[0].jacobians(n,&x[..self.ns[0]],&old[..self.ns[0]],&f[..self.nf[0]],self.p(p,0))?;let b=self.bodies[1].jacobians(n,&x[self.ns[0]..],&old[self.ns[0]..],&f[self.nf[0]..],self.p(p,1))?;let current=Jacobian::Csr(block_csr(&a.current.to_csr()?,&b.current.to_csr()?)?);let mut time_scale=a.time_scale;time_scale.extend(b.time_scale);Ok(FieldJacobians{current,previous:block_jac(a.previous,b.previous),flux:Jacobian::Csr(block_csr(&a.flux.to_csr()?,&b.flux.to_csr()?)?),design:block_jac(a.design,b.design),time_scale})}
 fn admissible_step(&self,n:usize,x:&[f64],d:&[f64],old:&[f64],p:StepParameters<'_>,trial:f64)->CaeResult<f64>{if x.len()!=self.state_size()||d.len()!=self.state_size()||old.len()!=self.state_size()||p.design.len()!=self.design_size(){return Err(fail("two-body trial shape"));}let mut a=trial;for b in 0..2{a=a.min(self.bodies[b].admissible_step(n,&x[Self::range(self.ns,b)],&d[Self::range(self.ns,b)],&old[Self::range(self.ns,b)],self.p(p,b),a)?);}Ok(a)}
 fn check_trace_path(&self,n:usize,start:&[f64],end:&[f64],p:StepParameters<'_>)->CaeResult<()>{if start.len()!=self.nf.iter().sum::<usize>()||end.len()!=start.len()||p.design.len()!=self.design_size(){return Err(fail("two-body trace path shape"));}for b in 0..2{self.bodies[b].check_trace_path(n,&start[Self::range(self.nf,b)],&end[Self::range(self.nf,b)],self.p(p,b))?;}Ok(())}
 fn samples(&self,n:usize,x:&[f64],old:&[f64],p:StepParameters<'_>)->CaeResult<Vec<f64>>{self.check(x,old,&vec![0.;self.nf.iter().sum::<usize>()],p)?;let mut y=vec![];for b in 0..2{y.extend(self.bodies[b].samples(n,&x[Self::range(self.ns,b)],&old[Self::range(self.ns,b)],self.p(p,b))?);}Ok(y)}
 fn samples_vjp(&self,n:usize,x:&[f64],old:&[f64],p:StepParameters<'_>,bar:&[f64])->CaeResult<(Vec<f64>,Vec<f64>,Vec<f64>)>{self.check(x,old,&vec![0.;self.nf.iter().sum::<usize>()],p)?;if bar.len()!=self.ny.iter().sum::<usize>(){return Err(fail("two-body sample cotangent shape"));}let(mut a,mut b,mut q)=(vec![],vec![],vec![]);for i in 0..2{let(c,o,d)=self.bodies[i].samples_vjp(n,&x[Self::range(self.ns,i)],&old[Self::range(self.ns,i)],self.p(p,i),&bar[Self::range(self.ny,i)])?;a.extend(c);b.extend(o);q.extend(d);}Ok((a,b,q))}
}
pub struct PairCarrier{bodies:[Arc<dyn LagrangianCarrier>;2],nt:[usize;2],nd:[usize;2],np:[usize;2]}
impl PairCarrier{pub fn new(a:Arc<dyn LagrangianCarrier>,b:Arc<dyn LagrangianCarrier>)->Self{let nt=[a.trace_size(),b.trace_size()];let nd=[a.design_size(),b.design_size()];let np=[a.point_count(),b.point_count()];Self{bodies:[a,b],nt,nd,np}}fn check(&self,t:&[f64],d:&[f64])->CaeResult<()>{if t.len()!=self.trace_size()||d.len()!=self.design_size()||t.iter().chain(d).any(|v|!v.is_finite()){return Err(fail("two-body carrier shape"));}Ok(())}}
impl LagrangianCarrier for PairCarrier{
 fn trace_size(&self)->usize{self.nt.iter().sum::<usize>()}fn design_size(&self)->usize{self.nd.iter().sum::<usize>()}fn point_count(&self)->usize{self.np.iter().sum::<usize>()}
 fn points(&self,t:&[f64],d:&[f64])->CaeResult<PointCloud>{self.check(t,d)?;let mut a=self.bodies[0].points(&t[..self.nt[0]],&d[..self.nd[0]])?;let b=self.bodies[1].points(&t[self.nt[0]..],&d[self.nd[0]..])?;a.positions.extend(b.positions);a.weights.extend(b.weights);Ok(a)}
 fn points_jvp(&self,t:&[f64],d:&[f64],dt:&[f64],dd:Option<&[f64]>)->CaeResult<PointCloud>{self.check(t,d)?;if dt.len()!=t.len()||dd.is_some_and(|q|q.len()!=d.len()){return Err(fail("two-body carrier direction shape"));}let mut a=self.bodies[0].points_jvp(&t[..self.nt[0]],&d[..self.nd[0]],&dt[..self.nt[0]],dd.map(|q|&q[..self.nd[0]]))?;let b=self.bodies[1].points_jvp(&t[self.nt[0]..],&d[self.nd[0]..],&dt[self.nt[0]..],dd.map(|q|&q[self.nd[0]..]))?;a.positions.extend(b.positions);a.weights.extend(b.weights);Ok(a)}
 fn points_vjp(&self,t:&[f64],d:&[f64],pb:&[[f64;3]],wb:&[f64])->CaeResult<(Vec<f64>,Vec<f64>)>{self.check(t,d)?;if pb.len()!=self.point_count()||wb.len()!=self.point_count(){return Err(fail("two-body carrier cotangent shape"));}let(mut a,mut q)=self.bodies[0].points_vjp(&t[..self.nt[0]],&d[..self.nd[0]],&pb[..self.np[0]],&wb[..self.np[0]])?;let(b,r)=self.bodies[1].points_vjp(&t[self.nt[0]..],&d[self.nd[0]..],&pb[self.np[0]..],&wb[self.np[0]..])?;a.extend(b);q.extend(r);Ok((a,q))}
 fn impulses_to_flux(&self,i:&[[f64;3]],dt:f64)->Vec<f64>{if i.len()!=self.point_count()||!dt.is_finite()||dt<=0.{return vec![f64::NAN;self.trace_size()];}let mut a=self.bodies[0].impulses_to_flux(&i[..self.np[0]],dt);a.extend(self.bodies[1].impulses_to_flux(&i[self.np[0]..],dt));a}
 fn flux_to_impulses(&self,f:&[f64],dt:f64)->Vec<[f64;3]>{if f.len()!=self.trace_size()||!dt.is_finite()||dt<=0.{return vec![[f64::NAN;3];self.point_count()];}let mut a=self.bodies[0].flux_to_impulses(&f[..self.nt[0]],dt);a.extend(self.bodies[1].flux_to_impulses(&f[self.nt[0]..],dt));a}
}
pub struct MovingFsiPairModel{bodies:[FsiModel;2],carrier:Arc<PairCarrier>,contact:Option<MovingContactSpec>,identity:String}
impl MovingFsiPairModel{
 pub fn new(a:FsiProblem,b:FsiProblem,contact:MovingContactSpec)->CaeResult<Self>{
  for key in ["fluid","time","coupling"]{if a.normal_form()[key]!=b.normal_form()[key]{return Err(fail(format!("two-body common {key} differs")));}}if !contact.replaced_planes.is_empty()||a.solid.contact_planes.len()!=0||b.solid.contact_planes.len()!=0{return Err(fail("two-body moving contact requires explicit plane-free problems"));}
  let bodies=[FsiModel::new(a)?,FsiModel::new(b)?];for i in 0..2{let n=bodies[i].problem.solid.grid.node_count();let mut seen=std::collections::BTreeSet::new();if contact.body_nodes[i].is_empty()||contact.body_nodes[i].iter().any(|j|*j>=n||!seen.insert(*j)){return Err(fail("two-body local contact node indices"));}}
  if format!("{:?}",bodies[0].lbm_config())!=format!("{:?}",bodies[1].lbm_config())||bodies[0].problem.observables.interface!=bodies[1].problem.observables.interface{return Err(fail("two-body common fluid configuration or interface ledger differs"));}
  let carrier=Arc::new(PairCarrier::new(bodies[0].carrier.clone(),bodies[1].carrier.clone()));let d=serde_json::json!({"schema":"implexity-private-two-native-body-fsi/1","body_identities":[bodies[0].problem.identity(),bodies[1].problem.identity()],"contact_nodes":contact.body_nodes,"contact_bodies":contact.bodies,"features":format!("{:?}",contact.features),"gap_scale":contact.gap_scale_m,"force_scale":contact.force_scale_n,"path":format!("{:?}",contact.path_policy)});let identity=implexity_core::json::canonical_sha256(&d);Ok(Self{bodies,carrier,contact:Some(contact),identity})
 }
 pub fn from_bodies(a:FsiProblem,b:FsiProblem)->CaeResult<Self>{
  for key in ["fluid","time","coupling"]{if a.normal_form()[key]!=b.normal_form()[key]{return Err(fail(format!("two-body common {key} differs")));}}if !a.solid.contact_planes.is_empty()||!b.solid.contact_planes.is_empty(){return Err(fail("two-body external contact owner requires explicit plane-free problems"));}
  let bodies=[FsiModel::new(a)?,FsiModel::new(b)?];if format!("{:?}",bodies[0].lbm_config())!=format!("{:?}",bodies[1].lbm_config())||bodies[0].problem.observables.interface!=bodies[1].problem.observables.interface{return Err(fail("two-body common fluid configuration or interface ledger differs"));}let carrier=Arc::new(PairCarrier::new(bodies[0].carrier.clone(),bodies[1].carrier.clone()));let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"schema":"implexity-two-native-body-external-contact-owner/1","body_identities":[bodies[0].problem.identity(),bodies[1].problem.identity()]}));Ok(Self{bodies,carrier,contact:None,identity})
 }
 pub fn native_body(&self,i:usize)->CaeResult<&FsiModel>{self.bodies.get(i).ok_or_else(||fail("two-body native index"))}
 pub fn carrier(&self)->&PairCarrier{&self.carrier}
 pub fn identity(&self)->&str{&self.identity}
 pub fn solid_field(&self)->CaeResult<PairSolidField<'_>>{PairSolidField::new(self.bodies[0].solid_field()?,self.bodies[1].solid_field()?)}
 pub fn contact_field(&self)->CaeResult<ContactField<PairSolidField<'_>,MappedPairLaw>>{
  let contact=self.contact.as_ref().ok_or_else(||fail("two-body model has no authored selected contact law; use its declared external contact owner"))?;let solid=self.solid_field()?;let mut nodes:[Vec<NodeBinding>;2]=std::array::from_fn(|_|vec![]);let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);let mut row=0;
  for b in 0..2{let grid=&self.bodies[b].problem.solid.grid;let body=&solid.bodies[b];let offset_state=if b==0{0}else{solid.ns[0]};let offset_flux=if b==0{0}else{solid.nf[0]};let offset_design=if b==0{0}else{solid.nd[0]};let scale=body.core().scale();for &id in &contact.body_nodes[b]{let mut state=[0;3];let mut inverse=[0.;3];for a in 0..3{let(c,v)=body.trace_operator().row(3*id+a);if c.len()!=1||v!=[1.]{return Err(fail("two-body contact nodal trace"));}state[a]=offset_state+c[0];inverse[a]=1./scale[c[0]];}nodes[b].push(NodeBinding{reference_m:grid.node_position(id),state,inverse_state_scale:inverse,force_flux:[offset_flux+3*id,offset_flux+3*id+1,offset_flux+3*id+2]});let ijk=grid.node_ijk(id);let mut incident=vec![];for x in 0..2{for y in 0..2{for z in 0..2{let s=[x,y,z];let mut v=[0;3];let mut ok=true;for a in 0..3{if ijk[a]<s[a]{ok=false;break;}v[a]=ijk[a]-s[a];if v[a]>=grid.shape[a]{ok=false;break;}}if ok{incident.push(grid.voxel_index(v));}}}}if incident.is_empty(){return Err(fail("two-body contact incident phase"));}let weight=1./incident.len()as f64;for v in incident{ri.push(row);ci.push(offset_design+v);vs.push(weight);}row+=1;}}
  let phase=CsrMatrix::from_triplets(row,solid.design_size(),&ri,&ci,&vs).map_err(fail)?;let law=MappedPairLaw::new(nodes,contact.features.clone(),contact.bodies,phase,solid.state_size(),solid.trace_operator().nrows(),contact.gap_scale_m,contact.force_scale_n)?.with_path_policy(contact.path_policy)?;ContactField::new(solid,law)
 }
 pub fn fluid_field(&self)->CaeResult<FluidField>{let a=&self.bodies[0];let inner=AnyMovingLbm::new(a.problem.fluid.lattice,a.lbm_config(),self.carrier.clone())?;Ok(FluidField::new(inner,a.problem.observables.interface.clone(),a.problem.time.macro_step_s()))}
 pub fn stepper(&self)->CaeResult<MultirateStepper<FluidField,ContactField<PairSolidField<'_>,MappedPairLaw>>>{let c=&self.bodies[0].problem.coupling;Ok(MultirateStepper::new(self.fluid_field()?,self.contact_field()?,MultirateOptions{mode:c.mode.clone(),schur_ratio_limit:c.schur_ratio_limit,work_defect_limit:c.work_defect_limit})?.with_identity(self.identity.clone()).with_field_newton(c.field_newton)?.with_linear_solves(c.linear_solves)?.with_step_cache_bytes(c.step_cache_bytes))}
}

use super::{event_impulse::{PhysicalMass,SolidImpactEvent},impact::{self,ImpactTolerance},event_step::{EventAdvancePolicy,EventTickState,MovingEventTick,EventLinearization,EventTickStateDirection,MovingEventTickDirection,input_identity,solve,solve_maintained,implicit_direction,maintained_direction,maintained_jacobians,implicit_pullback}};
use implexity_physics_lbm::moving::field::event_trace::{EventTickTrace,EventTickDirection};
use implexity_ad::Dual;
fn dot(a:&[f64],b:&[f64])->f64{a.iter().zip(b).map(|(a,b)|a*b).sum()}
impl MovingFsiPairModel{
 fn physical_mass(&self,design:&[f64],direction:Option<&[f64]>)->CaeResult<(CsrMatrix,Vec<bool>)>{
  let field=self.solid_field()?;if design.len()!=field.design_size()||direction.is_some_and(|d|d.len()!=design.len()){return Err(fail("two-body physical mass design shape"));}let mut matrices=vec![];let mut fixed=vec![];
  for b in 0..2{let core=field.bodies[b].core();let model=core.model();let parameters=core.params(&design[PairSolidField::range(field.nd,b)])?;let removed=self.bodies[b].problem.solid.inertia_compensation*self.bodies[b].problem.fluid.density_kg_m3;let ratio:Vec<_>=(0..model.ne()).map(|e|{let rho=model.materials[model.element_material[e]].density;(rho+removed)/rho}).collect();if ratio.iter().any(|r|!r.is_finite()||*r<1.){return Err(fail("two-body physical mass compensation domain"));}let factors=if let Some(d)=direction{let dp=core.params(&d[PairSolidField::range(field.nd,b)])?;(0..model.ne()).map(|i|model.interpolation.mass(Dual::new(parameters[i],[dp[i]])).eps[0]*ratio[i]).collect::<Vec<_>>()}else{(0..model.ne()).map(|i|model.interpolation.mass(parameters[i])*ratio[i]).collect::<Vec<_>>()};matrices.push(model.global_mass_and_reference(&factors,&vec![0.;model.ne()])?.0);fixed.extend(&model.fixed);}
  Ok((block_csr(&matrices[0],&matrices[1])?,fixed))
 }
 fn unpack(&self,state:&[f64],design:&[f64],scale:f64)->CaeResult<(Vec<f64>,Vec<f64>)>{let field=self.solid_field()?;if state.len()!=field.state_size()+1||design.len()!=field.design_size(){return Err(fail("two-body physical state shape"));}let(mut u,mut v)=(vec![],vec![]);for b in 0..2{let core=field.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(field.nd,b)],time_scale:scale})?;let p=core.to_physical(&state[PairSolidField::range(field.ns,b)]);u.extend(&p[h.layout.u()..h.layout.u()+h.layout.n3]);v.extend(&p[h.layout.v()..h.layout.v()+h.layout.n3]);}Ok((u,v))}
 pub fn apply_solid_impact<'a>(&self,before:&[f64],fluid:&'a[f64],design:&[f64],scale:f64,time:f64,restitution:f64,tolerance:ImpactTolerance)->CaeResult<SolidImpactEvent<'a>>{
  if !time.is_finite()||time<0.||fluid.len()!=self.fluid_field()?.state_size()||fluid.iter().any(|v|!v.is_finite()||*v<0.){return Err(fail("two-body event fluid/time domain"));}let field=self.solid_field()?;let(_,velocity)=self.unpack(before,design,scale)?;let(matrix,fixed)=self.physical_mass(design,None)?;let mass=PhysicalMass::new(matrix,fixed)?;let contact=self.contact_field()?;let(row,gap)=contact.law().instantaneous_row(before,design)?;let impulse=impact::solve(&mass,&velocity,&[row],&[restitution],&[gap],tolerance)?;let mut solid=before.to_vec();let mut offset=0;
  for b in 0..2{let core=field.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(field.nd,b)],time_scale:scale})?;let range=PairSolidField::range(field.ns,b);let mut p=core.to_physical(&before[range.clone()]);p[h.layout.v()..h.layout.v()+h.layout.n3].copy_from_slice(&impulse.velocity[offset..offset+h.layout.n3]);let scaled=core.to_scaled(&p);for i in h.layout.v()..h.layout.v()+h.layout.n3{solid[range.start+i]=scaled[i];}offset+=h.layout.n3;}
  Ok(SolidImpactEvent{solid,fluid_unchanged:fluid,absolute_time_s:time,impulse,tolerance})
 }
 pub fn solid_impact_direction(&self,before:&[f64],design:&[f64],scale:f64,base:&SolidImpactEvent<'_>,direction:&[f64],ddesign:&[f64],restitution:f64,drestitution:f64)->CaeResult<Vec<f64>>{
  let fresh=self.apply_solid_impact(before,base.fluid_unchanged,design,scale,base.absolute_time_s,restitution,base.tolerance)?;if fresh.solid.len()!=base.solid.len()||fresh.solid.iter().zip(&base.solid).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("two-body impact direction identity"));}let field=self.solid_field()?;let(_,v)=self.unpack(before,design,scale)?;let(_,dv)=self.unpack(direction,design,scale)?;let(matrix,fixed)=self.physical_mass(design,None)?;let mass=PhysicalMass::new(matrix,fixed)?;let dm=self.physical_mass(design,Some(ddesign))?.0;let contact=self.contact_field()?;let(row,_)=contact.law().instantaneous_row(before,design)?;let(dr,_)=contact.law().instantaneous_row_direction(before,design,direction,ddesign)?;let(dv,_)=impact::direction(&mass,&fresh.impulse,&v,&[row],&[restitution],&dv,&[dr],&[drestitution],&|x|dm.matvec(x).map_err(fail))?;let mut out=direction.to_vec();let mut offset=0;
  for b in 0..2{let core=field.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(field.nd,b)],time_scale:scale})?;let range=PairSolidField::range(field.ns,b);let mut p=core.to_physical(&direction[range.clone()]);p[h.layout.v()..h.layout.v()+h.layout.n3].copy_from_slice(&dv[offset..offset+h.layout.n3]);let scaled=core.to_scaled(&p);for i in h.layout.v()..h.layout.v()+h.layout.n3{out[range.start+i]=scaled[i];}offset+=h.layout.n3;}Ok(out)
 }
 pub fn event_fluid_field(&self,p:EventAdvancePolicy)->CaeResult<FluidField>{let a=&self.bodies[0];let mut config=a.lbm_config();config.interpolation_kernel=p.interpolation_kernel;let inner=AnyMovingLbm::new(a.problem.fluid.lattice,config,self.carrier.clone())?;Ok(FluidField::new(inner,a.problem.observables.interface.clone(),a.problem.time.macro_step_s()))}
}

impl MovingFsiPairModel{
 pub fn event_tick_lagged(&self,state:&EventTickState,design:&[f64],external_force_n:&[f64],sample_tick:usize,policy:EventAdvancePolicy)->CaeResult<MovingEventTick>{
  if !policy.maintained_gap_target_fraction.is_finite()||!(0. ..=0.5).contains(&policy.maintained_gap_target_fraction)||!policy.restitution.is_finite()||!(0. ..=1.).contains(&policy.restitution)||!policy.residual_tolerance.is_finite()||policy.residual_tolerance<=0.||policy.maximum_newton_iterations==0||policy.maximum_localization_iterations==0||!state.origin_s.is_finite()||state.origin_s<0.||self.bodies.iter().any(|b|b.problem.solid.supports.iter().any(|s|s.motion.is_some())){return Err(fail("event step requires finite explicit policy and stationary prescribed supports"));}
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
  let(u0,v0)=self.unpack(old,design,scale)?;let(ue,ve)=self.unpack(&end,design,scale)?;let(up,vp)=self.unpack(&pre,design,scale)?;let(ua,va)=self.unpack(&post,design,scale)?;
  let(row,gap)=contact.law().instantaneous_row(&end,design)?;let normal=dot(&row,&ve);if gap< -policy.impact.event_gap_m||!normal.is_finite(){return Err(fail("event final nonpenetration refused"));}if maintained&&(end[n]<=0.||normal< -policy.impact.normal_velocity_m_s){return Err(fail("maintained event needs positive multiplier and nonclosing final velocity"));}
  let(before_mid,after_mid,before_v,after_v)=if event.is_some(){(u0.iter().zip(&up).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),ua.iter().zip(&ue).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),v0.iter().zip(&vp).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),va.iter().zip(&ve).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>())}else{let mid:Vec<_>=u0.iter().zip(&ue).map(|(a,b)|0.5*(a+b)).collect();let v:Vec<_>=v0.iter().zip(&ve).map(|(a,b)|0.5*(a+b)).collect();(mid.clone(),mid,v.clone(),v)};
  let tick=fluid.event_tick(&state.fluid,design,EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+fraction*dt,sample_tick,start:&u0,before_midpoint:&before_mid,after_midpoint:&after_mid,end:&ue,before_velocity_m_s:&before_v,after_velocity_m_s:&after_v})?;
  let reaction:Vec<_>=(0..nf).map(|i|tick.before_impulse_n_s[i]+tick.after_impulse_n_s[i]+tick.start_compensation_impulse_n_s[i]+tick.end_compensation_impulse_n_s[i]).collect();let lagged_force:Vec<_>=reaction.iter().map(|x|x/dt).collect();let delta_u:Vec<_>=ue.iter().zip(&u0).map(|(a,b)|a-b).collect();let body_work=dot(&state.lagged_force_n,&delta_u);let fluid_work=dot(&tick.before_impulse_n_s,&before_v)+dot(&tick.after_impulse_n_s,&after_v)+dot(&tick.start_compensation_impulse_n_s,&v0)+dot(&tick.end_compensation_impulse_n_s,&ve);let newstate=EventTickState{solid:end.clone(),fluid:tick.state.clone(),lagged_force_n:lagged_force,origin_s:state.origin_s+dt};
  let linearization=EventLinearization{input:input_identity(self.identity(),state,design,external_force_n,sample_tick,policy),pre:pre.clone(),post:post.clone(),end:end.clone(),fraction,event,maintained};
  Ok(MovingEventTick{linearization,state:newstate,fluid_tick:tick,event_time_s:event,impact:impulse,final_gap_m:gap,final_normal_velocity_m_s:normal,multiplier_n:end[n],final_residual:residual,body_lagged_work_j:body_work,fluid_reaction_work_j:fluid_work,lagged_work_defect_j:body_work-fluid_work,maintained})
 }
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
  let(u0,v0)=self.unpack(&state.solid,design,scale)?;let(up,vp)=self.unpack(&b.pre,design,scale)?;let(ua,va)=self.unpack(&b.post,design,scale)?;let(ue,ve)=self.unpack(&b.end,design,scale)?;let(du0,dv0)=self.unpack(&dstate.solid,design,scale)?;let(dup,dvp)=self.unpack(&dpre,design,scale)?;let(dua,dva)=self.unpack(&dpost,design,scale)?;let(due,dve)=self.unpack(&dend,design,scale)?;let mean=|a:&[f64],b:&[f64]|a.iter().zip(b).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>();
  let(bm,am,bv,av,dbm,dam,dbv,dav)=if b.event.is_some(){(mean(&u0,&up),mean(&ua,&ue),mean(&v0,&vp),mean(&va,&ve),mean(&du0,&dup),mean(&dua,&due),mean(&dv0,&dvp),mean(&dva,&dve))}else{let m=mean(&u0,&ue);let v=mean(&v0,&ve);let dm=mean(&du0,&due);let dv=mean(&dv0,&dve);(m.clone(),m,v.clone(),v,dm.clone(),dm,dv.clone(),dv)};
  let trace=EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+b.fraction*dt,sample_tick:tick,start:&u0,before_midpoint:&bm,after_midpoint:&am,end:&ue,before_velocity_m_s:&bv,after_velocity_m_s:&av};let direction=EventTickDirection{origin_s:dstate.origin_s,event_s:dstate.origin_s+dtau,previous:&dstate.fluid,design:ddesign,start:&du0,before_midpoint:&dbm,after_midpoint:&dam,end:&due,before_velocity_m_s:&dbv,after_velocity_m_s:&dav};let f=fluid.event_tick_tangent(&state.fluid,design,trace,direction)?;
  let lagged_force_n=(0..state.lagged_force_n.len()).map(|i|(f.before_impulse_n_s[i]+f.after_impulse_n_s[i]+f.start_compensation_impulse_n_s[i]+f.end_compensation_impulse_n_s[i])/dt).collect();Ok(MovingEventTickDirection{state:EventTickStateDirection{solid:dend,fluid:f.state,lagged_force_n,origin_s:dstate.origin_s},event_time_s:b.event.map(|_|dstate.origin_s+dtau),fluid_samples:f.samples})
 }
}

pub struct MovingEventTickCotangent{pub state:EventTickStateDirection,pub event_time_s:f64,pub fluid_samples:Vec<f64>}
pub struct MovingEventTickBars{pub state:EventTickStateDirection,pub design:Vec<f64>,pub external_force_n:Vec<f64>,pub restitution:f64}
fn add(a:&mut[f64],b:&[f64])->CaeResult<()>{if a.len()!=b.len(){return Err(fail("event cotangent accumulation shape"));}for(a,b)in a.iter_mut().zip(b){*a+=b;}Ok(())}
impl MovingFsiPairModel{
 fn unpack_pullback(&self,state:&[f64],design:&[f64],scale:f64,u:&[f64],v:&[f64])->CaeResult<Vec<f64>>{
  let f=self.solid_field()?;let mut out=vec![0.;state.len()];let mut offset=0;
  for b in 0..2{let core=f.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(f.nd,b)],time_scale:scale})?;let r=PairSolidField::range(f.ns,b);if offset+h.layout.n3>u.len()||u.len()!=v.len(){return Err(fail("event physical cotangent shape"));}
   for i in 0..h.layout.n3{out[r.start+h.layout.u()+i]=u[offset+i]/core.scale()[h.layout.u()+i];out[r.start+h.layout.v()+i]=v[offset+i]/core.scale()[h.layout.v()+i];}offset+=h.layout.n3;
  }
  if offset!=u.len(){return Err(fail("event physical cotangent length"));}Ok(out)
 }
 fn impact_pullback(&self,before:&[f64],design:&[f64],scale:f64,base:&SolidImpactEvent<'_>,restitution:f64,bar:&[f64])->CaeResult<(Vec<f64>,Vec<f64>,f64)>{
  if bar.len()!=before.len()||bar.iter().any(|x|!x.is_finite()){return Err(fail("two-body impact cotangent shape"));}
  let fresh=self.apply_solid_impact(before,base.fluid_unchanged,design,scale,base.absolute_time_s,restitution,base.tolerance)?;
  if fresh.solid.iter().zip(&base.solid).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("two-body impact pullback identity"));}
  let f=self.solid_field()?;let(_,v)=self.unpack(before,design,scale)?;let mut bv=vec![];let mut old=bar.to_vec();
  for b in 0..2{let core=f.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(f.nd,b)],time_scale:scale})?;let r=PairSolidField::range(f.ns,b);for i in 0..h.layout.n3{let j=r.start+h.layout.v()+i;bv.push(bar[j]*core.scale()[h.layout.v()+i]);old[j]=0.;}}
  let(matrix,fixed)=self.physical_mass(design,None)?;let mass=PhysicalMass::new(matrix,fixed)?;let contact=self.contact_field()?;let(row,_)=contact.law().instantaneous_row(before,design)?;
  let ib=impact::adjoint(&mass,&fresh.impulse,&v,&[row],&[restitution],&bv,&[0.])?;
  add(&mut old,&self.unpack_pullback(before,design,scale,&vec![0.;v.len()],&ib.velocity)?)?;
  let(sb,mut db)=contact.law().instantaneous_row_adjoint(before,design,&ib.rows[0],0.)?;add(&mut old,&sb)?;
  let mut offset=0;
  for b in 0..2{let core=f.bodies[b].core();let model=core.model();let pp=core.params(&design[PairSolidField::range(f.nd,b)])?;let r=PairSolidField::range(f.nd,b);let removed=self.bodies[b].problem.solid.inertia_compensation*self.bodies[b].problem.fluid.density_kg_m3;
   if self.bodies[b].kuhn.owner.len()!=model.ne(){return Err(fail("impact voxel mass map shape"));}
   for(e,t)in model.mesh.elements.iter().enumerate(){let rho=model.materials[model.element_material[e]].density;let dm=model.interpolation.mass(Dual::new(pp[e],[1.])).eps[0]*(rho+removed)/rho;let m=model.element_mass(e,1.);let mut contraction=0.;
    for(y,x)in &ib.mass_pairs{for a in 0..4{for c in 0..4{for k in 0..3{contraction+=y[offset+3*t[a]+k]*m[a][c]*x[offset+3*t[c]+k];}}}}
    let j=r.start+self.bodies[b].kuhn.owner[e];if j>=r.end{return Err(fail("impact voxel mass owner"));}db[j]+=dm*contraction;
   }offset+=3*model.n();
  }
  Ok((old,db,ib.restitution[0]))
 }
 pub fn event_tick_lagged_adjoint(&self,state:&EventTickState,design:&[f64],external:&[f64],tick:usize,p:EventAdvancePolicy,base:&MovingEventTick,bar:&MovingEventTickCotangent)->CaeResult<MovingEventTickBars>{
  let b=&base.linearization;if b.input!=input_identity(self.identity(),state,design,external,tick,p){return Err(fail("event pullback input identity differs"));}
  if bar.state.solid.len()!=state.solid.len()||bar.state.fluid.len()!=state.fluid.len()||bar.state.lagged_force_n.len()!=state.lagged_force_n.len()||bar.fluid_samples.len()!=base.fluid_tick.samples.len()||!bar.state.origin_s.is_finite()||!bar.event_time_s.is_finite()||bar.state.solid.iter().chain(&bar.state.fluid).chain(&bar.state.lagged_force_n).chain(&bar.fluid_samples).any(|x|!x.is_finite()){return Err(fail("event pullback output cotangent shape"));}
  if b.event.is_none()&&bar.event_time_s!=0.{return Err(fail("event-time cotangent without event"));}
  let contact=self.contact_field()?;let native=contact.native();let n=native.state_size();let fluid=self.event_fluid_field(p)?;let dt=fluid.inner().nominal_fluid_step_s();let macro_dt=native.nominal_step_s();let scale=dt/macro_dt;
  let flux:Vec<_>=state.lagged_force_n.iter().zip(external).map(|(a,b)|a+b).collect();
  let(u0,v0)=self.unpack(&state.solid,design,scale)?;let(up,vp)=self.unpack(&b.pre,design,scale)?;let(ua,va)=self.unpack(&b.post,design,scale)?;let(ue,ve)=self.unpack(&b.end,design,scale)?;
  let mean=|a:&[f64],b:&[f64]|a.iter().zip(b).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>();
  let(bm,am,bv,av)=if b.event.is_some(){(mean(&u0,&up),mean(&ua,&ue),mean(&v0,&vp),mean(&va,&ve))}else{let m=mean(&u0,&ue);let v=mean(&v0,&ve);(m.clone(),m,v.clone(),v)};
  let trace=EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+b.fraction*dt,sample_tick:tick,start:&u0,before_midpoint:&bm,after_midpoint:&am,end:&ue,before_velocity_m_s:&bv,after_velocity_m_s:&av};
  let reaction:Vec<_>=bar.state.lagged_force_n.iter().map(|x|x/dt).collect();
  let fb=fluid.event_tick_adjoint(&state.fluid,design,trace,implexity_physics_lbm::moving::field::event_trace::EventTickCotangent{state:&bar.state.fluid,before_impulse_n_s:&reaction,after_impulse_n_s:&reaction,start_compensation_impulse_n_s:&reaction,end_compensation_impulse_n_s:&reaction,samples:&bar.fluid_samples})?;
  let(mut old,mut pre,mut post,mut end)=(vec![0.;state.solid.len()],vec![0.;state.solid.len()],vec![0.;state.solid.len()],bar.state.solid.clone());
  let(mut ou,mut ov,mut eu,mut ev)=(fb.start,vec![0.;v0.len()],fb.end,vec![0.;ve.len()]);
  let(mut pu,mut pv,mut au,mut avbar)=(vec![0.;up.len()],vec![0.;vp.len()],vec![0.;ua.len()],vec![0.;va.len()]);
  for i in 0..ou.len(){if b.event.is_some(){ou[i]+=0.5*fb.before_midpoint[i];pu[i]+=0.5*fb.before_midpoint[i];au[i]+=0.5*fb.after_midpoint[i];eu[i]+=0.5*fb.after_midpoint[i];ov[i]+=0.5*fb.before_velocity_m_s[i];pv[i]+=0.5*fb.before_velocity_m_s[i];avbar[i]+=0.5*fb.after_velocity_m_s[i];ev[i]+=0.5*fb.after_velocity_m_s[i];}else{let u=0.5*(fb.before_midpoint[i]+fb.after_midpoint[i]);let v=0.5*(fb.before_velocity_m_s[i]+fb.after_velocity_m_s[i]);ou[i]+=u;eu[i]+=u;ov[i]+=v;ev[i]+=v;}}
  add(&mut old,&self.unpack_pullback(&state.solid,design,scale,&ou,&ov)?)?;add(&mut pre,&self.unpack_pullback(&b.pre,design,scale,&pu,&pv)?)?;add(&mut post,&self.unpack_pullback(&b.post,design,scale,&au,&avbar)?)?;add(&mut end,&self.unpack_pullback(&b.end,design,scale,&eu,&ev)?)?;
  let mut db=fb.design;let mut force=vec![0.;flux.len()];let mut tau=fb.event_s+bar.event_time_s;let clock=bar.state.origin_s+fb.origin_s+fb.event_s+bar.event_time_s;
  let previous=if b.event.is_some(){&b.post}else{&state.solid};let ep=StepParameters{design,time_scale:if b.event.is_some(){scale*(1.-b.fraction)}else{scale}};
  let eb=if b.maintained{implicit_pullback(&maintained_jacobians(&contact,&b.end,previous,&flux,ep)?,&end)?}else{implicit_pullback(&native.jacobians(1,&b.end[..n],&previous[..n],&flux,ep)?,&end[..n])?};
  add(&mut db,&eb.design)?;add(&mut force,&eb.flux)?;tau-=eb.time_scale/macro_dt;
  if b.event.is_some(){add(&mut post[..eb.previous.len()],&eb.previous)?;}else{add(&mut old[..eb.previous.len()],&eb.previous)?;}
  let mut restitution=0.;
  if let Some(time)=b.event{
   let jump=self.apply_solid_impact(&b.pre,&state.fluid,design,scale*b.fraction,time,p.restitution,p.impact)?;if jump.solid.iter().zip(&b.post).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("event pullback impact identity"));}
   let(ib,id,ie)=self.impact_pullback(&b.pre,design,scale*b.fraction,&jump,p.restitution,&post)?;add(&mut pre,&ib)?;add(&mut db,&id)?;restitution=ie;
   let pp=StepParameters{design,time_scale:scale*b.fraction};let zero=vec![0.;design.len()];let mut rate=implicit_direction(native,&b.pre[..n],&state.solid[..n],&flux,pp,&vec![0.;n],&vec![0.;flux.len()],&zero,1./macro_dt)?;rate.push(0.);
   let speed=contact.law().instantaneous_row_direction(&b.pre,design,&rate,&zero)?.1;if !speed.is_finite()||speed>=0.{return Err(fail("event pullback needs transverse closing"));}
   tau+=dot(&pre,&rate);let(sb,gd)=contact.law().instantaneous_row_adjoint(&b.pre,design,&vec![0.;flux.len()],-tau/speed)?;add(&mut pre,&sb)?;add(&mut db,&gd)?;
   let pb=implicit_pullback(&native.jacobians(1,&b.pre[..n],&state.solid[..n],&flux,pp)?,&pre[..n])?;add(&mut old[..n],&pb.previous)?;add(&mut force,&pb.flux)?;add(&mut db,&pb.design)?;
  }
  if old.iter().chain(&fb.previous).chain(&force).chain(&db).any(|x|!x.is_finite())||![clock,restitution].iter().all(|x|x.is_finite()){return Err(fail("event history pullback overflow"));}
  Ok(MovingEventTickBars{state:EventTickStateDirection{solid:old,fluid:fb.previous,lagged_force_n:force.clone(),origin_s:clock},design:db,external_force_n:force,restitution})
 }
}

pub struct MovingEventHistory{pub initial:EventTickState,pub outputs:Vec<MovingEventTick>,pub ticks:Vec<usize>}
pub struct MovingEventHistoryBars{pub initial:EventTickStateDirection,pub design:Vec<f64>,pub external_force_n:Vec<Vec<f64>>,pub restitution:f64}
fn copy_event_state(s:&EventTickState)->EventTickState{EventTickState{solid:s.solid.clone(),fluid:s.fluid.clone(),lagged_force_n:s.lagged_force_n.clone(),origin_s:s.origin_s}}
impl MovingFsiPairModel{
 pub fn advance_event_history(&self,initial:&EventTickState,design:&[f64],external:&[Vec<f64>],ticks:&[usize],p:EventAdvancePolicy)->CaeResult<MovingEventHistory>{
  if ticks.is_empty()||ticks.len()!=external.len(){return Err(fail("event history schedule shape"));}
  let mut state=copy_event_state(initial);let mut outputs=Vec::with_capacity(ticks.len());
  for(&tick,force)in ticks.iter().zip(external){let out=self.event_tick_lagged(&state,design,force,tick,p)?;state=copy_event_state(&out.state);outputs.push(out);}
  Ok(MovingEventHistory{initial:copy_event_state(initial),outputs,ticks:ticks.to_vec()})
 }
 pub fn event_history_adjoint(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,bars:&[MovingEventTickCotangent])->CaeResult<MovingEventHistoryBars>{
  let count=history.outputs.len();if count==0||external.len()!=count||history.ticks.len()!=count||bars.len()!=count{return Err(fail("event history cotangent schedule shape"));}
  let initial=&history.initial;let mut carry=EventTickStateDirection{solid:vec![0.;initial.solid.len()],fluid:vec![0.;initial.fluid.len()],lagged_force_n:vec![0.;initial.lagged_force_n.len()],origin_s:0.};
  let mut db=vec![0.;design.len()];let mut force=vec![vec![];count];let mut restitution=0.;
  for t in (0..count).rev(){let state=if t==0{initial}else{&history.outputs[t-1].state};let b=&bars[t];let mut bar=MovingEventTickCotangent{state:EventTickStateDirection{solid:b.state.solid.clone(),fluid:b.state.fluid.clone(),lagged_force_n:b.state.lagged_force_n.clone(),origin_s:b.state.origin_s+carry.origin_s},event_time_s:b.event_time_s,fluid_samples:b.fluid_samples.clone()};
   add(&mut bar.state.solid,&carry.solid)?;add(&mut bar.state.fluid,&carry.fluid)?;add(&mut bar.state.lagged_force_n,&carry.lagged_force_n)?;
   let x=self.event_tick_lagged_adjoint(state,design,&external[t],history.ticks[t],p,&history.outputs[t],&bar)?;carry=x.state;add(&mut db,&x.design)?;force[t]=x.external_force_n;restitution+=x.restitution;
  }
  if !restitution.is_finite(){return Err(fail("event history restitution cotangent overflow"));}Ok(MovingEventHistoryBars{initial:carry,design:db,external_force_n:force,restitution})
 }
}

use super::event_response::EventNativeSeries;
use implexity_linalg::dense::DenseMatrix;
pub struct PairEventContext<'a>{model:&'a MovingFsiPairModel,contact:ContactField<PairSolidField<'a>,MappedPairLaw>,fluid:FluidField,kernel:implexity_physics_lbm::moving::pushforward::InterpolationKernel}
impl std::ops::Deref for PairEventContext<'_>{type Target=MovingFsiPairModel;fn deref(&self)->&Self::Target{self.model}}
impl<'ctx> PairEventContext<'ctx>{
 pub fn new(model:&'ctx MovingFsiPairModel,p:EventAdvancePolicy)->CaeResult<Self>{Ok(Self{model,contact:model.contact_field()?,fluid:model.event_fluid_field(p)?,kernel:p.interpolation_kernel})}
 pub(crate) fn solid_field(&self)->CaeResult<&PairSolidField<'ctx>>{Ok(self.contact.native())}
 pub(crate) fn contact_field(&self)->CaeResult<&ContactField<PairSolidField<'ctx>,MappedPairLaw>>{Ok(&self.contact)}
 pub(crate) fn fluid_field(&self)->CaeResult<&FluidField>{Ok(&self.fluid)}
 pub(crate) fn event_fluid_field(&self,p:EventAdvancePolicy)->CaeResult<&FluidField>{if p.interpolation_kernel!=self.kernel{return Err(fail("event context numerical interpolation differs"));}Ok(&self.fluid)}
fn physical_mass(&self,design:&[f64],direction:Option<&[f64]>)->CaeResult<(CsrMatrix,Vec<bool>)>{
  let field=self.solid_field()?;if design.len()!=field.design_size()||direction.is_some_and(|d|d.len()!=design.len()){return Err(fail("two-body physical mass design shape"));}let mut matrices=vec![];let mut fixed=vec![];
  for b in 0..2{let core=field.bodies[b].core();let model=core.model();let parameters=core.params(&design[PairSolidField::range(field.nd,b)])?;let removed=self.bodies[b].problem.solid.inertia_compensation*self.bodies[b].problem.fluid.density_kg_m3;let ratio:Vec<_>=(0..model.ne()).map(|e|{let rho=model.materials[model.element_material[e]].density;(rho+removed)/rho}).collect();if ratio.iter().any(|r|!r.is_finite()||*r<1.){return Err(fail("two-body physical mass compensation domain"));}let factors=if let Some(d)=direction{let dp=core.params(&d[PairSolidField::range(field.nd,b)])?;(0..model.ne()).map(|i|model.interpolation.mass(Dual::new(parameters[i],[dp[i]])).eps[0]*ratio[i]).collect::<Vec<_>>()}else{(0..model.ne()).map(|i|model.interpolation.mass(parameters[i])*ratio[i]).collect::<Vec<_>>()};matrices.push(model.global_mass_and_reference(&factors,&vec![0.;model.ne()])?.0);fixed.extend(&model.fixed);}
  Ok((block_csr(&matrices[0],&matrices[1])?,fixed))
 }
fn unpack(&self,state:&[f64],design:&[f64],scale:f64)->CaeResult<(Vec<f64>,Vec<f64>)>{let field=self.solid_field()?;if state.len()!=field.state_size()+1||design.len()!=field.design_size(){return Err(fail("two-body physical state shape"));}let(mut u,mut v)=(vec![],vec![]);for b in 0..2{let core=field.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(field.nd,b)],time_scale:scale})?;let p=core.to_physical(&state[PairSolidField::range(field.ns,b)]);u.extend(&p[h.layout.u()..h.layout.u()+h.layout.n3]);v.extend(&p[h.layout.v()..h.layout.v()+h.layout.n3]);}Ok((u,v))}
pub fn apply_solid_impact<'a>(&self,before:&[f64],fluid:&'a[f64],design:&[f64],scale:f64,time:f64,restitution:f64,tolerance:ImpactTolerance)->CaeResult<SolidImpactEvent<'a>>{
  if !time.is_finite()||time<0.||fluid.len()!=self.fluid_field()?.state_size()||fluid.iter().any(|v|!v.is_finite()||*v<0.){return Err(fail("two-body event fluid/time domain"));}let field=self.solid_field()?;let(_,velocity)=self.unpack(before,design,scale)?;let(matrix,fixed)=self.physical_mass(design,None)?;let mass=PhysicalMass::new(matrix,fixed)?;let contact=self.contact_field()?;let(row,gap)=contact.law().instantaneous_row(before,design)?;let impulse=impact::solve(&mass,&velocity,&[row],&[restitution],&[gap],tolerance)?;let mut solid=before.to_vec();let mut offset=0;
  for b in 0..2{let core=field.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(field.nd,b)],time_scale:scale})?;let range=PairSolidField::range(field.ns,b);let mut p=core.to_physical(&before[range.clone()]);p[h.layout.v()..h.layout.v()+h.layout.n3].copy_from_slice(&impulse.velocity[offset..offset+h.layout.n3]);let scaled=core.to_scaled(&p);for i in h.layout.v()..h.layout.v()+h.layout.n3{solid[range.start+i]=scaled[i];}offset+=h.layout.n3;}
  Ok(SolidImpactEvent{solid,fluid_unchanged:fluid,absolute_time_s:time,impulse,tolerance})
 }
pub fn solid_impact_direction(&self,before:&[f64],design:&[f64],scale:f64,base:&SolidImpactEvent<'_>,direction:&[f64],ddesign:&[f64],restitution:f64,drestitution:f64)->CaeResult<Vec<f64>>{
  let fresh=self.apply_solid_impact(before,base.fluid_unchanged,design,scale,base.absolute_time_s,restitution,base.tolerance)?;if fresh.solid.len()!=base.solid.len()||fresh.solid.iter().zip(&base.solid).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("two-body impact direction identity"));}let field=self.solid_field()?;let(_,v)=self.unpack(before,design,scale)?;let(_,dv)=self.unpack(direction,design,scale)?;let(matrix,fixed)=self.physical_mass(design,None)?;let mass=PhysicalMass::new(matrix,fixed)?;let dm=self.physical_mass(design,Some(ddesign))?.0;let contact=self.contact_field()?;let(row,_)=contact.law().instantaneous_row(before,design)?;let(dr,_)=contact.law().instantaneous_row_direction(before,design,direction,ddesign)?;let(dv,_)=impact::direction(&mass,&fresh.impulse,&v,&[row],&[restitution],&dv,&[dr],&[drestitution],&|x|dm.matvec(x).map_err(fail))?;let mut out=direction.to_vec();let mut offset=0;
  for b in 0..2{let core=field.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(field.nd,b)],time_scale:scale})?;let range=PairSolidField::range(field.ns,b);let mut p=core.to_physical(&direction[range.clone()]);p[h.layout.v()..h.layout.v()+h.layout.n3].copy_from_slice(&dv[offset..offset+h.layout.n3]);let scaled=core.to_scaled(&p);for i in h.layout.v()..h.layout.v()+h.layout.n3{out[range.start+i]=scaled[i];}offset+=h.layout.n3;}Ok(out)
 }
fn unpack_pullback(&self,state:&[f64],design:&[f64],scale:f64,u:&[f64],v:&[f64])->CaeResult<Vec<f64>>{
  let f=self.solid_field()?;let mut out=vec![0.;state.len()];let mut offset=0;
  for b in 0..2{let core=f.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(f.nd,b)],time_scale:scale})?;let r=PairSolidField::range(f.ns,b);if offset+h.layout.n3>u.len()||u.len()!=v.len(){return Err(fail("event physical cotangent shape"));}
   for i in 0..h.layout.n3{out[r.start+h.layout.u()+i]=u[offset+i]/core.scale()[h.layout.u()+i];out[r.start+h.layout.v()+i]=v[offset+i]/core.scale()[h.layout.v()+i];}offset+=h.layout.n3;
  }
  if offset!=u.len(){return Err(fail("event physical cotangent length"));}Ok(out)
 }
fn impact_pullback(&self,before:&[f64],design:&[f64],scale:f64,base:&SolidImpactEvent<'_>,restitution:f64,bar:&[f64])->CaeResult<(Vec<f64>,Vec<f64>,f64)>{
  if bar.len()!=before.len()||bar.iter().any(|x|!x.is_finite()){return Err(fail("two-body impact cotangent shape"));}
  let fresh=self.apply_solid_impact(before,base.fluid_unchanged,design,scale,base.absolute_time_s,restitution,base.tolerance)?;
  if fresh.solid.iter().zip(&base.solid).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("two-body impact pullback identity"));}
  let f=self.solid_field()?;let(_,v)=self.unpack(before,design,scale)?;let mut bv=vec![];let mut old=bar.to_vec();
  for b in 0..2{let core=f.bodies[b].core();let h=core.history(StepParameters{design:&design[PairSolidField::range(f.nd,b)],time_scale:scale})?;let r=PairSolidField::range(f.ns,b);for i in 0..h.layout.n3{let j=r.start+h.layout.v()+i;bv.push(bar[j]*core.scale()[h.layout.v()+i]);old[j]=0.;}}
  let(matrix,fixed)=self.physical_mass(design,None)?;let mass=PhysicalMass::new(matrix,fixed)?;let contact=self.contact_field()?;let(row,_)=contact.law().instantaneous_row(before,design)?;
  let ib=impact::adjoint(&mass,&fresh.impulse,&v,&[row],&[restitution],&bv,&[0.])?;
  add(&mut old,&self.unpack_pullback(before,design,scale,&vec![0.;v.len()],&ib.velocity)?)?;
  let(sb,mut db)=contact.law().instantaneous_row_adjoint(before,design,&ib.rows[0],0.)?;add(&mut old,&sb)?;
  let mut offset=0;
  for b in 0..2{let core=f.bodies[b].core();let model=core.model();let pp=core.params(&design[PairSolidField::range(f.nd,b)])?;let r=PairSolidField::range(f.nd,b);let removed=self.bodies[b].problem.solid.inertia_compensation*self.bodies[b].problem.fluid.density_kg_m3;
   if self.bodies[b].kuhn.owner.len()!=model.ne(){return Err(fail("impact voxel mass map shape"));}
   for(e,t)in model.mesh.elements.iter().enumerate(){let rho=model.materials[model.element_material[e]].density;let dm=model.interpolation.mass(Dual::new(pp[e],[1.])).eps[0]*(rho+removed)/rho;let m=model.element_mass(e,1.);let mut contraction=0.;
    for(y,x)in &ib.mass_pairs{for a in 0..4{for c in 0..4{for k in 0..3{contraction+=y[offset+3*t[a]+k]*m[a][c]*x[offset+3*t[c]+k];}}}}
    let j=r.start+self.bodies[b].kuhn.owner[e];if j>=r.end{return Err(fail("impact voxel mass owner"));}db[j]+=dm*contraction;
   }offset+=3*model.n();
  }
  Ok((old,db,ib.restitution[0]))
 }
pub fn event_tick_lagged(&self,state:&EventTickState,design:&[f64],external_force_n:&[f64],sample_tick:usize,policy:EventAdvancePolicy)->CaeResult<MovingEventTick>{
  if !policy.maintained_gap_target_fraction.is_finite()||!(0. ..=0.5).contains(&policy.maintained_gap_target_fraction)||!policy.restitution.is_finite()||!(0. ..=1.).contains(&policy.restitution)||!policy.residual_tolerance.is_finite()||policy.residual_tolerance<=0.||policy.maximum_newton_iterations==0||policy.maximum_localization_iterations==0||!state.origin_s.is_finite()||state.origin_s<0.||self.bodies.iter().any(|b|b.problem.solid.supports.iter().any(|s|s.motion.is_some())){return Err(fail("event step requires finite explicit policy and stationary prescribed supports"));}
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
  let(u0,v0)=self.unpack(old,design,scale)?;let(ue,ve)=self.unpack(&end,design,scale)?;let(up,vp)=self.unpack(&pre,design,scale)?;let(ua,va)=self.unpack(&post,design,scale)?;
  let(row,gap)=contact.law().instantaneous_row(&end,design)?;let normal=dot(&row,&ve);if gap< -policy.impact.event_gap_m||!normal.is_finite(){return Err(fail("event final nonpenetration refused"));}if maintained&&(end[n]<=0.||normal< -policy.impact.normal_velocity_m_s){return Err(fail("maintained event needs positive multiplier and nonclosing final velocity"));}
  let(before_mid,after_mid,before_v,after_v)=if event.is_some(){(u0.iter().zip(&up).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),ua.iter().zip(&ue).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),v0.iter().zip(&vp).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>(),va.iter().zip(&ve).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>())}else{let mid:Vec<_>=u0.iter().zip(&ue).map(|(a,b)|0.5*(a+b)).collect();let v:Vec<_>=v0.iter().zip(&ve).map(|(a,b)|0.5*(a+b)).collect();(mid.clone(),mid,v.clone(),v)};
  let tick=fluid.event_tick(&state.fluid,design,EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+fraction*dt,sample_tick,start:&u0,before_midpoint:&before_mid,after_midpoint:&after_mid,end:&ue,before_velocity_m_s:&before_v,after_velocity_m_s:&after_v})?;
  let reaction:Vec<_>=(0..nf).map(|i|tick.before_impulse_n_s[i]+tick.after_impulse_n_s[i]+tick.start_compensation_impulse_n_s[i]+tick.end_compensation_impulse_n_s[i]).collect();let lagged_force:Vec<_>=reaction.iter().map(|x|x/dt).collect();let delta_u:Vec<_>=ue.iter().zip(&u0).map(|(a,b)|a-b).collect();let body_work=dot(&state.lagged_force_n,&delta_u);let fluid_work=dot(&tick.before_impulse_n_s,&before_v)+dot(&tick.after_impulse_n_s,&after_v)+dot(&tick.start_compensation_impulse_n_s,&v0)+dot(&tick.end_compensation_impulse_n_s,&ve);let newstate=EventTickState{solid:end.clone(),fluid:tick.state.clone(),lagged_force_n:lagged_force,origin_s:state.origin_s+dt};
  let linearization=EventLinearization{input:input_identity(self.identity(),state,design,external_force_n,sample_tick,policy),pre:pre.clone(),post:post.clone(),end:end.clone(),fraction,event,maintained};
  Ok(MovingEventTick{linearization,state:newstate,fluid_tick:tick,event_time_s:event,impact:impulse,final_gap_m:gap,final_normal_velocity_m_s:normal,multiplier_n:end[n],final_residual:residual,body_lagged_work_j:body_work,fluid_reaction_work_j:fluid_work,lagged_work_defect_j:body_work-fluid_work,maintained})
 }
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
  let(u0,v0)=self.unpack(&state.solid,design,scale)?;let(up,vp)=self.unpack(&b.pre,design,scale)?;let(ua,va)=self.unpack(&b.post,design,scale)?;let(ue,ve)=self.unpack(&b.end,design,scale)?;let(du0,dv0)=self.unpack(&dstate.solid,design,scale)?;let(dup,dvp)=self.unpack(&dpre,design,scale)?;let(dua,dva)=self.unpack(&dpost,design,scale)?;let(due,dve)=self.unpack(&dend,design,scale)?;let mean=|a:&[f64],b:&[f64]|a.iter().zip(b).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>();
  let(bm,am,bv,av,dbm,dam,dbv,dav)=if b.event.is_some(){(mean(&u0,&up),mean(&ua,&ue),mean(&v0,&vp),mean(&va,&ve),mean(&du0,&dup),mean(&dua,&due),mean(&dv0,&dvp),mean(&dva,&dve))}else{let m=mean(&u0,&ue);let v=mean(&v0,&ve);let dm=mean(&du0,&due);let dv=mean(&dv0,&dve);(m.clone(),m,v.clone(),v,dm.clone(),dm,dv.clone(),dv)};
  let trace=EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+b.fraction*dt,sample_tick:tick,start:&u0,before_midpoint:&bm,after_midpoint:&am,end:&ue,before_velocity_m_s:&bv,after_velocity_m_s:&av};let direction=EventTickDirection{origin_s:dstate.origin_s,event_s:dstate.origin_s+dtau,previous:&dstate.fluid,design:ddesign,start:&du0,before_midpoint:&dbm,after_midpoint:&dam,end:&due,before_velocity_m_s:&dbv,after_velocity_m_s:&dav};let f=fluid.event_tick_tangent(&state.fluid,design,trace,direction)?;
  let lagged_force_n=(0..state.lagged_force_n.len()).map(|i|(f.before_impulse_n_s[i]+f.after_impulse_n_s[i]+f.start_compensation_impulse_n_s[i]+f.end_compensation_impulse_n_s[i])/dt).collect();Ok(MovingEventTickDirection{state:EventTickStateDirection{solid:dend,fluid:f.state,lagged_force_n,origin_s:dstate.origin_s},event_time_s:b.event.map(|_|dstate.origin_s+dtau),fluid_samples:f.samples})
 }
pub fn event_tick_lagged_adjoint(&self,state:&EventTickState,design:&[f64],external:&[f64],tick:usize,p:EventAdvancePolicy,base:&MovingEventTick,bar:&MovingEventTickCotangent)->CaeResult<MovingEventTickBars>{
  let b=&base.linearization;if b.input!=input_identity(self.identity(),state,design,external,tick,p){return Err(fail("event pullback input identity differs"));}
  if bar.state.solid.len()!=state.solid.len()||bar.state.fluid.len()!=state.fluid.len()||bar.state.lagged_force_n.len()!=state.lagged_force_n.len()||bar.fluid_samples.len()!=base.fluid_tick.samples.len()||!bar.state.origin_s.is_finite()||!bar.event_time_s.is_finite()||bar.state.solid.iter().chain(&bar.state.fluid).chain(&bar.state.lagged_force_n).chain(&bar.fluid_samples).any(|x|!x.is_finite()){return Err(fail("event pullback output cotangent shape"));}
  if b.event.is_none()&&bar.event_time_s!=0.{return Err(fail("event-time cotangent without event"));}
  let contact=self.contact_field()?;let native=contact.native();let n=native.state_size();let fluid=self.event_fluid_field(p)?;let dt=fluid.inner().nominal_fluid_step_s();let macro_dt=native.nominal_step_s();let scale=dt/macro_dt;
  let flux:Vec<_>=state.lagged_force_n.iter().zip(external).map(|(a,b)|a+b).collect();
  let(u0,v0)=self.unpack(&state.solid,design,scale)?;let(up,vp)=self.unpack(&b.pre,design,scale)?;let(ua,va)=self.unpack(&b.post,design,scale)?;let(ue,ve)=self.unpack(&b.end,design,scale)?;
  let mean=|a:&[f64],b:&[f64]|a.iter().zip(b).map(|(a,b)|0.5*(a+b)).collect::<Vec<_>>();
  let(bm,am,bv,av)=if b.event.is_some(){(mean(&u0,&up),mean(&ua,&ue),mean(&v0,&vp),mean(&va,&ve))}else{let m=mean(&u0,&ue);let v=mean(&v0,&ve);(m.clone(),m,v.clone(),v)};
  let trace=EventTickTrace{origin_s:state.origin_s,event_s:state.origin_s+b.fraction*dt,sample_tick:tick,start:&u0,before_midpoint:&bm,after_midpoint:&am,end:&ue,before_velocity_m_s:&bv,after_velocity_m_s:&av};
  let reaction:Vec<_>=bar.state.lagged_force_n.iter().map(|x|x/dt).collect();
  let fb=fluid.event_tick_adjoint(&state.fluid,design,trace,implexity_physics_lbm::moving::field::event_trace::EventTickCotangent{state:&bar.state.fluid,before_impulse_n_s:&reaction,after_impulse_n_s:&reaction,start_compensation_impulse_n_s:&reaction,end_compensation_impulse_n_s:&reaction,samples:&bar.fluid_samples})?;
  let(mut old,mut pre,mut post,mut end)=(vec![0.;state.solid.len()],vec![0.;state.solid.len()],vec![0.;state.solid.len()],bar.state.solid.clone());
  let(mut ou,mut ov,mut eu,mut ev)=(fb.start,vec![0.;v0.len()],fb.end,vec![0.;ve.len()]);
  let(mut pu,mut pv,mut au,mut avbar)=(vec![0.;up.len()],vec![0.;vp.len()],vec![0.;ua.len()],vec![0.;va.len()]);
  for i in 0..ou.len(){if b.event.is_some(){ou[i]+=0.5*fb.before_midpoint[i];pu[i]+=0.5*fb.before_midpoint[i];au[i]+=0.5*fb.after_midpoint[i];eu[i]+=0.5*fb.after_midpoint[i];ov[i]+=0.5*fb.before_velocity_m_s[i];pv[i]+=0.5*fb.before_velocity_m_s[i];avbar[i]+=0.5*fb.after_velocity_m_s[i];ev[i]+=0.5*fb.after_velocity_m_s[i];}else{let u=0.5*(fb.before_midpoint[i]+fb.after_midpoint[i]);let v=0.5*(fb.before_velocity_m_s[i]+fb.after_velocity_m_s[i]);ou[i]+=u;eu[i]+=u;ov[i]+=v;ev[i]+=v;}}
  add(&mut old,&self.unpack_pullback(&state.solid,design,scale,&ou,&ov)?)?;add(&mut pre,&self.unpack_pullback(&b.pre,design,scale,&pu,&pv)?)?;add(&mut post,&self.unpack_pullback(&b.post,design,scale,&au,&avbar)?)?;add(&mut end,&self.unpack_pullback(&b.end,design,scale,&eu,&ev)?)?;
  let mut db=fb.design;let mut force=vec![0.;flux.len()];let mut tau=fb.event_s+bar.event_time_s;let clock=bar.state.origin_s+fb.origin_s+fb.event_s+bar.event_time_s;
  let previous=if b.event.is_some(){&b.post}else{&state.solid};let ep=StepParameters{design,time_scale:if b.event.is_some(){scale*(1.-b.fraction)}else{scale}};
  let eb=if b.maintained{implicit_pullback(&maintained_jacobians(&contact,&b.end,previous,&flux,ep)?,&end)?}else{implicit_pullback(&native.jacobians(1,&b.end[..n],&previous[..n],&flux,ep)?,&end[..n])?};
  add(&mut db,&eb.design)?;add(&mut force,&eb.flux)?;tau-=eb.time_scale/macro_dt;
  if b.event.is_some(){add(&mut post[..eb.previous.len()],&eb.previous)?;}else{add(&mut old[..eb.previous.len()],&eb.previous)?;}
  let mut restitution=0.;
  if let Some(time)=b.event{
   let jump=self.apply_solid_impact(&b.pre,&state.fluid,design,scale*b.fraction,time,p.restitution,p.impact)?;if jump.solid.iter().zip(&b.post).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("event pullback impact identity"));}
   let(ib,id,ie)=self.impact_pullback(&b.pre,design,scale*b.fraction,&jump,p.restitution,&post)?;add(&mut pre,&ib)?;add(&mut db,&id)?;restitution=ie;
   let pp=StepParameters{design,time_scale:scale*b.fraction};let zero=vec![0.;design.len()];let mut rate=implicit_direction(native,&b.pre[..n],&state.solid[..n],&flux,pp,&vec![0.;n],&vec![0.;flux.len()],&zero,1./macro_dt)?;rate.push(0.);
   let speed=contact.law().instantaneous_row_direction(&b.pre,design,&rate,&zero)?.1;if !speed.is_finite()||speed>=0.{return Err(fail("event pullback needs transverse closing"));}
   tau+=dot(&pre,&rate);let(sb,gd)=contact.law().instantaneous_row_adjoint(&b.pre,design,&vec![0.;flux.len()],-tau/speed)?;add(&mut pre,&sb)?;add(&mut db,&gd)?;
   let pb=implicit_pullback(&native.jacobians(1,&b.pre[..n],&state.solid[..n],&flux,pp)?,&pre[..n])?;add(&mut old[..n],&pb.previous)?;add(&mut force,&pb.flux)?;add(&mut db,&pb.design)?;
  }
  if old.iter().chain(&fb.previous).chain(&force).chain(&db).any(|x|!x.is_finite())||![clock,restitution].iter().all(|x|x.is_finite()){return Err(fail("event history pullback overflow"));}
  Ok(MovingEventTickBars{state:EventTickStateDirection{solid:old,fluid:fb.previous,lagged_force_n:force.clone(),origin_s:clock},design:db,external_force_n:force,restitution})
 }
pub fn advance_event_history(&self,initial:&EventTickState,design:&[f64],external:&[Vec<f64>],ticks:&[usize],p:EventAdvancePolicy)->CaeResult<MovingEventHistory>{
  if ticks.is_empty()||ticks.len()!=external.len(){return Err(fail("event history schedule shape"));}
  let mut state=copy_event_state(initial);let mut outputs=Vec::with_capacity(ticks.len());
  for(&tick,force)in ticks.iter().zip(external){let out=self.event_tick_lagged(&state,design,force,tick,p)?;state=copy_event_state(&out.state);outputs.push(out);}
  Ok(MovingEventHistory{initial:copy_event_state(initial),outputs,ticks:ticks.to_vec()})
 }
pub fn event_history_adjoint(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,bars:&[MovingEventTickCotangent])->CaeResult<MovingEventHistoryBars>{
  let count=history.outputs.len();if count==0||external.len()!=count||history.ticks.len()!=count||bars.len()!=count{return Err(fail("event history cotangent schedule shape"));}
  let initial=&history.initial;let mut carry=EventTickStateDirection{solid:vec![0.;initial.solid.len()],fluid:vec![0.;initial.fluid.len()],lagged_force_n:vec![0.;initial.lagged_force_n.len()],origin_s:0.};
  let mut db=vec![0.;design.len()];let mut force=vec![vec![];count];let mut restitution=0.;
  for t in (0..count).rev(){let state=if t==0{initial}else{&history.outputs[t-1].state};let b=&bars[t];let mut bar=MovingEventTickCotangent{state:EventTickStateDirection{solid:b.state.solid.clone(),fluid:b.state.fluid.clone(),lagged_force_n:b.state.lagged_force_n.clone(),origin_s:b.state.origin_s+carry.origin_s},event_time_s:b.event_time_s,fluid_samples:b.fluid_samples.clone()};
   add(&mut bar.state.solid,&carry.solid)?;add(&mut bar.state.fluid,&carry.fluid)?;add(&mut bar.state.lagged_force_n,&carry.lagged_force_n)?;
   let x=self.event_tick_lagged_adjoint(state,design,&external[t],history.ticks[t],p,&history.outputs[t],&bar)?;carry=x.state;add(&mut db,&x.design)?;force[t]=x.external_force_n;restitution+=x.restitution;
  }
  if !restitution.is_finite(){return Err(fail("event history restitution cotangent overflow"));}Ok(MovingEventHistoryBars{initial:carry,design:db,external_force_n:force,restitution})
 }
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
pub fn event_history_native_samples(&self,design:&[f64],p:EventAdvancePolicy,history:&MovingEventHistory)->CaeResult<EventNativeSeries>{self.event_history_native_samples_at(design,p,history,0)}
pub fn event_history_sample_adjoint(&self,design:&[f64],external:&[Vec<f64>],p:EventAdvancePolicy,history:&MovingEventHistory,bar:&DenseMatrix)->CaeResult<MovingEventHistoryBars>{self.event_history_sample_adjoint_at(design,external,p,history,bar,0,None)}
}
