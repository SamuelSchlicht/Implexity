// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::contact_compliance::UnilateralContactCompliance;
use implexity_solve::{matrix::Jacobian,time_stepper::StepParameters};
use std::collections::{BTreeMap,BTreeSet};
use super::{contact_field::{NativeContactLaw,ContactContribution},mapped_pair_law::NodeBinding,surface_map::SurfaceFeature};
use super::boundary_law::{facet_area_trace::ReferenceFacetArea,compliant_energy_momentum_contact};
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
#[derive(Clone)]
pub struct SurfaceContactQuadrature {
 pub traces:[Vec<(f64,SurfaceFeature)>;4],pub bodies:[usize;4],
 pub area_traces:[Vec<(f64,SurfaceFeature)>;3],pub area_body:usize,pub weight:f64,
}
pub struct NativeCompliantLaw {
 nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,states:usize,fluxes:usize,
 quadrature:Vec<SurfaceContactQuadrature>,compliance:UnilateralContactCompliance,margin:f64,
}
type Entries=Vec<(usize,usize,f64)>;
fn matrix(rows:usize,cols:usize,e:&Entries)->CaeResult<Jacobian>{
 if e.iter().any(|v|!v.2.is_finite()){return Err(fail("native compliant derivative overflow"));}
 let ri:Vec<_>=e.iter().map(|e|e.0).collect();let ci:Vec<_>=e.iter().map(|e|e.1).collect();let v:Vec<_>=e.iter().map(|e|e.2).collect();
 Ok(Jacobian::Csr(CsrMatrix::from_triplets(rows,cols,&ri,&ci,&v).map_err(fail)?))
}
impl NativeCompliantLaw {
 pub fn new(nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,states:usize,fluxes:usize,quadrature:Vec<SurfaceContactQuadrature>,compliance:UnilateralContactCompliance,margin:f64)->CaeResult<Self>{
  if states==0||fluxes==0||phase.nrows()!=nodes[0].len()+nodes[1].len()||quadrature.is_empty()||!margin.is_finite()||margin<=0.||margin>=1./3. {return Err(fail("native compliant layout"));}
  compliance.evaluate(0.).map_err(fail)?;let mut seen_states=BTreeSet::new();let mut seen_forces=BTreeSet::new();
  for node in nodes.iter().flatten(){if node.reference_m.iter().any(|v|!v.is_finite())||node.inverse_state_scale.iter().any(|v|!v.is_finite()||*v<=0.){return Err(fail("native compliant node binding"));}
   for a in 0..3{if node.state[a]>=states||node.force_flux[a]>=fluxes||!seen_states.insert(node.state[a])||!seen_forces.insert(node.force_flux[a]){return Err(fail("native compliant index ownership"));}}}
  for q in &quadrature {
   if q.bodies.iter().any(|b|*b>1)||q.bodies[0]==q.bodies[1]||q.bodies[1]!=q.bodies[2]||q.bodies[2]!=q.bodies[3]||q.area_body>1||!q.weight.is_finite()||q.weight<=0. {return Err(fail("native compliant quadrature ownership"));}
   for trace in q.traces.iter().chain(&q.area_traces){if trace.is_empty()||trace.iter().any(|(w,_)|!w.is_finite()||*w<0.)||(trace.iter().map(|(w,_)|*w).sum::<f64>()-1.).abs()>32.*f64::EPSILON{return Err(fail("native compliant trace partition"));}}
  }
  Ok(Self{nodes,phase,states,fluxes,quadrature,compliance,margin})
 }
 pub fn quadrature_count(&self)->usize{self.quadrature.len()}
 fn coordinates(&self,state:&[f64])->CaeResult<[Vec<[f64;3]>;2]>{
  if state.len()!=self.states||state.iter().any(|v|!v.is_finite()){return Err(fail("native compliant state"));}
  Ok(std::array::from_fn(|b|self.nodes[b].iter().map(|n|std::array::from_fn(|a|n.reference_m[a]+n.inverse_state_scale[a]*state[n.state[a]])).collect()))
 }
 fn density(&self,design:&[f64])->CaeResult<[Vec<f64>;2]>{
  if design.len()!=self.phase.ncols()||design.iter().any(|v|!v.is_finite()){return Err(fail("native compliant design"));}
  let mut rho=vec![0.;self.phase.nrows()];for (i,r) in rho.iter_mut().enumerate(){let(c,v)=self.phase.row(i);*r=c.iter().zip(v).map(|(j,w)|design[*j]*w).sum();if !r.is_finite()||!(0. ..=1.).contains(r){return Err(fail("native compliant nodal density"));}}
  let other=rho.split_off(self.nodes[0].len());Ok([rho,other])
 }
 pub fn contribution(&self,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<ContactContribution>{
  let old=self.coordinates(previous)?;let new=self.coordinates(current)?;let density=self.density(design)?;
  let reference:[Vec<[f64;3]>;2]=std::array::from_fn(|b|self.nodes[b].iter().map(|n|n.reference_m).collect());
  let mut force=vec![0.;self.fluxes];let mut jc=vec![];let mut jp=vec![];let mut jd=vec![];
  for (quadrature_index,q) in self.quadrature.iter().enumerate() {
   let area=ReferenceFacetArea::new(&q.area_traces,&reference[q.area_body],&density[q.area_body])?;
   let mut weights:[BTreeMap<usize,f64>;4]=std::array::from_fn(|_|BTreeMap::new());
   let mut partials:[BTreeMap<usize,BTreeMap<usize,f64>>;4]=std::array::from_fn(|_|BTreeMap::new());
   let mut x0=[[0.;3];4];let mut x1=[[0.;3];4];
   for i in 0..4 {let body=q.bodies[i];for (w,feature) in &q.traces[i]{let map=feature.map(&density[body])?;
    for &(node,weight) in map.weights(){*weights[i].entry(node).or_default()+=w*weight;}
    for (dnode,row) in map.density_partials(){for &(node,dw) in row{*partials[i].entry(*dnode).or_default().entry(node).or_default()+=w*dw;}}
   }
    for (&node,&w) in &weights[i]{for a in 0..3{x0[i][a]+=w*old[body][node][a];x1[i][a]+=w*new[body][node][a];}}
   }
   let path=super::boundary_law::boundary_path::check_linear_feature_path(x0,x1,super::boundary_law::boundary_path::PathPolicy{minimum_barycentric:self.margin,..Default::default()}).map_err(fail)?;
   if path.status!=super::boundary_law::boundary_path::PathStatus::Admitted{return Err(fail(format!("contact quadrature {quadrature_index} changes feature along configuration path: {:?}, prefix {}, interval {:?}",path.domain,path.certified_prefix,path.blocked_interval)));}
   let law=compliant_energy_momentum_contact::vertex_face(x0,x1,q.weight*area.area_m2(),self.compliance,self.margin).map_err(fail)?;
   for i in 0..4 {let body=q.bodies[i];for (&node,&wi) in &weights[i]{let output=&self.nodes[body][node];
    for a in 0..3 {force[output.force_flux[a]]+=wi*law.force_n[3*i+a];
     for j in 0..4 {for (&other,&wj) in &weights[j]{let input=&self.nodes[q.bodies[j]][other];for axis in 0..3{
      let current=wi*law.current_jacobian_n_per_m[3*i+a][3*j+axis]*wj*input.inverse_state_scale[axis];
      let previous=wi*law.previous_jacobian_n_per_m[3*i+a][3*j+axis]*wj*input.inverse_state_scale[axis];
      if current!=0.{jc.push((output.force_flux[a],input.state[axis],current));}if previous!=0.{jp.push((output.force_flux[a],input.state[axis],previous));}
     }}}
    }
   }}
   let (_,area_density)=area.pullback(q.weight)?;
   let mut area_partials=BTreeMap::new();for (node,&v) in area_density.iter().enumerate(){if v!=0.{area_partials.insert((q.area_body,node),v);}}
   let mut density_nodes:BTreeSet<(usize,usize)>=area_partials.keys().copied().collect();
   for i in 0..4 {density_nodes.extend(partials[i].keys().map(|node|(q.bodies[i],*node)));}
   for (body,dnode) in density_nodes {
    let mut dq0=[0.;12];let mut dq1=[0.;12];
    for j in 0..4{if q.bodies[j]!=body{continue;}if let Some(row)=partials[j].get(&dnode){for (&node,&w) in row{for a in 0..3{dq0[3*j+a]+=w*old[body][node][a];dq1[3*j+a]+=w*new[body][node][a];}}}}
    let da=*area_partials.get(&(body,dnode)).unwrap_or(&0.);
    let df:[f64;12]=std::array::from_fn(|i|law.area_derivative_n_per_m2[i]*da+(0..12).map(|j|law.current_jacobian_n_per_m[i][j]*dq1[j]+law.previous_jacobian_n_per_m[i][j]*dq0[j]).sum::<f64>());
    let mut nodal_derivative:BTreeMap<usize,f64>=BTreeMap::new();
    for i in 0..4 {for (&node,&w) in &weights[i]{let output=&self.nodes[q.bodies[i]][node];for a in 0..3{*nodal_derivative.entry(output.force_flux[a]).or_default()+=w*df[3*i+a];}}
     if q.bodies[i]==body {if let Some(row)=partials[i].get(&dnode){for (&node,&dw) in row{let output=&self.nodes[body][node];for a in 0..3{*nodal_derivative.entry(output.force_flux[a]).or_default()+=dw*law.force_n[3*i+a];}}}}
    }
    let phase_row=if body==0{dnode}else{self.nodes[0].len()+dnode};let(columns,values)=self.phase.row(phase_row);
    for (row,value) in nodal_derivative{for (&col,&w) in columns.iter().zip(values){let v=value*w;if v!=0.{jd.push((row,col,v));}}}
   }
  }
  if force.iter().any(|x|!x.is_finite()){return Err(fail("native compliant force overflow"));}
  Ok(ContactContribution{force,constraints:vec![],force_current:matrix(self.fluxes,self.states,&jc)?,force_previous:matrix(self.fluxes,self.states,&jp)?,force_design:matrix(self.fluxes,self.phase.ncols(),&jd)?,force_time:vec![0.;self.fluxes],constraint_current:matrix(0,self.states,&vec![])?,constraint_previous:matrix(0,self.states,&vec![])?,constraint_design:matrix(0,self.phase.ncols(),&vec![])?,constraint_time:vec![]})
 }
}
impl NativeContactLaw for NativeCompliantLaw {
 fn multipliers(&self)->usize{0}
 fn evaluate(&self,_n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<ContactContribution>{self.contribution(current,previous,p.design)}
 fn check_state_domain(&self,_n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.contribution(current,previous,p.design).map(|_|())}
 fn check_derivative_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.check_state_domain(n,current,previous,p)}
 fn newton_constraints(&self,_n:usize,_current:&[f64],_previous:&[f64],_p:StepParameters<'_>,_direction:&[f64],_attempt:usize)->CaeResult<Option<Jacobian>>{Ok(None)}
 fn admitted_trial(&self,_n:usize,current:&[f64],direction:&[f64],previous:&[f64],p:StepParameters<'_>,trial:f64)->CaeResult<f64>{
  if current.len()!=self.states||direction.len()!=self.states||direction.iter().any(|v|!v.is_finite())||!trial.is_finite()||trial<=0.||trial>1. {return Err(fail("native compliant trial"));}
  let mut fraction=trial;for _ in 0..32 {let candidate:Vec<_>=current.iter().zip(direction).map(|(v,d)|v+fraction*d).collect();if self.contribution(&candidate,previous,p.design).is_ok(){return Ok(fraction);}fraction*=0.5;}Ok(0.)
 }
}
