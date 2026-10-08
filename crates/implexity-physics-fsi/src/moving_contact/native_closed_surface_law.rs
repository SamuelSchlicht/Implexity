// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_geometry::domain_sdf::TriMesh;
use implexity_linalg::sparse::CsrMatrix;
use implexity_physics_solid::contact_compliance::UnilateralContactCompliance;
use implexity_solve::{matrix::Jacobian,time_stepper::StepParameters};
use std::collections::{BTreeMap,BTreeSet};
use std::sync::{Arc,Mutex};
use super::{contact_field::{NativeContactLaw,ContactContribution},mapped_pair_law::NodeBinding,native_surface_binding::NativeNodalBinding};
use super::boundary_law::{facet_area_trace::ReferenceFacetArea,exposed_surface::{ExposedSurface,IsoSurfacePolicy},closed_surface_contact_step::ClosedSurfaceContactStep,closed_surface_distance::DistancePolicy};
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
pub struct NativeClosedSurfaceLaw {
 nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,states:usize,fluxes:usize,tetrahedra:[Vec<[usize;4]>;2],
 compliance:UnilateralContactCompliance,surface_policy:IsoSurfacePolicy,distance_policy:DistancePolicy,
 reference_cache:Mutex<Option<Arc<ReferenceGeometry>>>,
}
struct CachedFacetArea{area_m2:f64,density_partials:BTreeMap<usize,f64>}
struct ReferenceGeometry{design_bits:Vec<u64>,density:[Vec<f64>;2],surfaces:[ExposedSurface;2],areas:[Vec<CachedFacetArea>;2]}
#[derive(Clone,Debug,Default)]
pub struct NativeClosedContactReport{pub quadrature:usize,pub separated_swept_bounds:usize,pub active_quadrature:usize,pub feature_transfers:usize,pub previous_energy_j:f64,pub current_energy_j:f64,pub maximum_local_work_defect_j:f64}
type Entries=Vec<(usize,usize,f64)>;
fn matrix(rows:usize,cols:usize,e:&Entries)->CaeResult<Jacobian>{
 if e.iter().any(|v|!v.2.is_finite()){return Err(fail("native compliant derivative overflow"));}
 let ri:Vec<_>=e.iter().map(|e|e.0).collect();let ci:Vec<_>=e.iter().map(|e|e.1).collect();let v:Vec<_>=e.iter().map(|e|e.2).collect();
 Ok(Jacobian::Csr(CsrMatrix::from_triplets(rows,cols,&ri,&ci,&v).map_err(fail)?))
}
impl NativeClosedSurfaceLaw {
 pub fn new(native:&NativeNodalBinding,compliance:UnilateralContactCompliance,surface_policy:IsoSurfacePolicy,distance_policy:DistancePolicy)->CaeResult<Self>{
  compliance.evaluate(0.).map_err(fail)?;
  Ok(Self{nodes:native.nodes().clone(),phase:native.phase().clone(),states:native.state_size(),fluxes:native.flux_size(),tetrahedra:std::array::from_fn(|b|native.meshes()[b].mesh.elements.clone()),compliance,surface_policy,distance_policy,reference_cache:Mutex::new(None)})
 }
 fn coordinates(&self,state:&[f64])->CaeResult<[Vec<[f64;3]>;2]>{
  if state.len()!=self.states||state.iter().any(|v|!v.is_finite()){return Err(fail("native closed contact state"));}
  Ok(std::array::from_fn(|b|self.nodes[b].iter().map(|n|std::array::from_fn(|a|n.reference_m[a]+n.inverse_state_scale[a]*state[n.state[a]])).collect()))
 }
 fn density(&self,design:&[f64])->CaeResult<[Vec<f64>;2]>{
  if design.len()!=self.phase.ncols()||design.iter().any(|v|!v.is_finite()){return Err(fail("native closed contact design"));}
  let mut rho=vec![0.;self.phase.nrows()];for (i,r) in rho.iter_mut().enumerate(){let(c,v)=self.phase.row(i);*r=c.iter().zip(v).map(|(j,w)|design[*j]*w).sum();if !r.is_finite()||!(0. ..=1.).contains(r){return Err(fail("native closed contact nodal density"));}}
  let other=rho.split_off(self.nodes[0].len());Ok([rho,other])
 }
 fn reference_geometry(&self,design:&[f64])->CaeResult<Arc<ReferenceGeometry>>{
  if design.len()!=self.phase.ncols()||design.iter().any(|v|!v.is_finite()){return Err(fail("native closed contact design"));}
  let bits:Vec<_>=design.iter().map(|v|v.to_bits()).collect();
  let mut cache=self.reference_cache.lock().map_err(|_|fail("native reference cache lock"))?;
  if let Some(cached)=cache.as_ref(){if cached.design_bits==bits{return Ok(Arc::clone(cached));}}
  let density=self.density(design)?;
  let reference:[Vec<[f64;3]>;2]=std::array::from_fn(|b|self.nodes[b].iter().map(|n|n.reference_m).collect());
  let surfaces=[ExposedSurface::extract(&reference[0],&self.tetrahedra[0],&density[0],self.surface_policy)?,ExposedSurface::extract(&reference[1],&self.tetrahedra[1],&density[1],self.surface_policy)?];
  let mut areas:[Vec<CachedFacetArea>;2]=std::array::from_fn(|_|vec![]);
  if !surfaces.iter().any(|s|s.facets().is_empty()){
   for surface in &surfaces{surface.require_closed_surface()?;}
   for body in 0..2{for facet in surfaces[body].facets(){
    let traces=facet.map(|v|vec![(1.,surfaces[body].features()[v].clone())]);
    let area=ReferenceFacetArea::new(&traces,&reference[body],&density[body])?;
    let (_,partials)=area.pullback(0.5)?;
    areas[body].push(CachedFacetArea{area_m2:area.area_m2(),density_partials:partials.into_iter().enumerate().filter(|(_,v)|*v!=0.).collect()});
   }}
  }
  let geometry=Arc::new(ReferenceGeometry{design_bits:bits,density,surfaces,areas});*cache=Some(Arc::clone(&geometry));Ok(geometry)
 }
 pub fn contribution(&self,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<ContactContribution>{self.contribution_with_report(current,previous,design).map(|x|x.0)}
 pub fn contribution_with_report(&self,current:&[f64],previous:&[f64],design:&[f64])->CaeResult<(ContactContribution,NativeClosedContactReport)>{
  let old=self.coordinates(previous)?;let new=self.coordinates(current)?;let geometry=self.reference_geometry(design)?;
  let density=&geometry.density;let surfaces=&geometry.surfaces;
  let mut force=vec![0.;self.fluxes];let mut jc=vec![];let mut jp=vec![];let mut jd=vec![];let mut report=NativeClosedContactReport::default();
  if !surfaces.iter().any(|s|s.facets().is_empty()){
   let x0=[surfaces[0].positions(&old[0],&density[0])?,surfaces[1].positions(&old[1],&density[1])?];
   let x1=[surfaces[0].positions(&new[0],&density[0])?,surfaces[1].positions(&new[1],&density[1])?];
   let steps=[ClosedSurfaceContactStep::new(TriMesh{v:x0[0].clone(),f:surfaces[0].facets().to_vec()},TriMesh{v:x1[0].clone(),f:surfaces[0].facets().to_vec()},self.distance_policy,self.compliance)?,ClosedSurfaceContactStep::new(TriMesh{v:x0[1].clone(),f:surfaces[1].facets().to_vec()},TriMesh{v:x1[1].clone(),f:surfaces[1].facets().to_vec()},self.distance_policy,self.compliance)?];
   let bounds:[[[f64;3];2];2]=std::array::from_fn(|b|[std::array::from_fn(|a|x0[b].iter().chain(&x1[b]).map(|p|p[a]).fold(f64::INFINITY,f64::min)),std::array::from_fn(|a|x0[b].iter().chain(&x1[b]).map(|p|p[a]).fold(f64::NEG_INFINITY,f64::max))]);
   for body in 0..2 {let target=1-body;for (facet_index,facet) in surfaces[body].facets().iter().enumerate(){
    let area=&geometry.areas[body][facet_index];
    let p0:[f64;3]=std::array::from_fn(|a|facet.iter().map(|v|x0[body][*v][a]/3.).sum());let p1:[f64;3]=std::array::from_fn(|a|facet.iter().map(|v|x1[body][*v][a]/3.).sum());
    report.quadrature+=1;if (0..3).any(|a|p0[a].max(p1[a])<bounds[target][0][a]||p0[a].min(p1[a])>bounds[target][1][a]){report.separated_swept_bounds+=1;continue;}
    let law=steps[target].evaluate(p0,p1,0.5*area.area_m2).map_err(|e|fail(format!("native closed contact body {body} facet {facet_index}: {e}")))?;
    report.previous_energy_j+=law.previous_energy_j;report.current_energy_j+=law.current_energy_j;report.maximum_local_work_defect_j=report.maximum_local_work_defect_j.max(law.work_defect_j.abs());if law.previous_energy_j>0.||law.current_energy_j>0.{report.active_quadrature+=1;}if law.previous_feature!=law.current_feature{report.feature_transfers+=1;}
    if law.force_n.iter().all(|v|*v==0.)&&law.current_jacobian_n_per_m.iter().flatten().chain(law.previous_jacobian_n_per_m.iter().flatten()).all(|v|*v==0.){continue;}
    let mut traces=vec![facet.iter().map(|v|(1./3.,surfaces[body].features()[*v].clone())).collect::<Vec<_>>()];traces.extend(law.target_nodes.iter().map(|v|vec![(1.,surfaces[target].features()[*v].clone())]));let mut bodies=vec![body];bodies.extend(law.target_nodes.iter().map(|_|target));
    let mut weights:Vec<BTreeMap<usize,f64>>=(0..traces.len()).map(|_|BTreeMap::new()).collect();let mut partials:Vec<BTreeMap<usize,BTreeMap<usize,f64>>>=(0..traces.len()).map(|_|BTreeMap::new()).collect();
    for i in 0..traces.len(){for (w,feature) in &traces[i]{let map=feature.map(&density[bodies[i]])?;for &(node,weight) in map.weights(){*weights[i].entry(node).or_default()+=w*weight;}for (dnode,row) in map.density_partials(){for &(node,dw) in row{*partials[i].entry(*dnode).or_default().entry(node).or_default()+=w*dw;}}}}
   for i in 0..weights.len() {let body=bodies[i];for (&node,&wi) in &weights[i]{let output=&self.nodes[body][node];
    for a in 0..3 {force[output.force_flux[a]]+=wi*law.force_n[3*i+a];
     for j in 0..weights.len() {for (&other,&wj) in &weights[j]{let input=&self.nodes[bodies[j]][other];for axis in 0..3{
      let current=wi*law.current_jacobian_n_per_m[3*i+a][3*j+axis]*wj*input.inverse_state_scale[axis];
      let previous=wi*law.previous_jacobian_n_per_m[3*i+a][3*j+axis]*wj*input.inverse_state_scale[axis];
      if current!=0.{jc.push((output.force_flux[a],input.state[axis],current));}if previous!=0.{jp.push((output.force_flux[a],input.state[axis],previous));}
     }}}
    }
   }}
   let area_partials:BTreeMap<_,_>=area.density_partials.iter().map(|(&node,&v)|((body,node),v)).collect();
   let mut density_nodes:BTreeSet<(usize,usize)>=area_partials.keys().copied().collect();
   for i in 0..weights.len() {density_nodes.extend(partials[i].keys().map(|node|(bodies[i],*node)));}
   for (density_body,dnode) in density_nodes {
    let mut dq0=vec![0.;law.force_n.len()];let mut dq1=vec![0.;law.force_n.len()];
    for j in 0..weights.len(){if bodies[j]!=density_body{continue;}if let Some(row)=partials[j].get(&dnode){for (&node,&w) in row{for a in 0..3{dq0[3*j+a]+=w*old[density_body][node][a];dq1[3*j+a]+=w*new[density_body][node][a];}}}}
    let da=*area_partials.get(&(density_body,dnode)).unwrap_or(&0.);
    let df:Vec<f64>=(0..law.force_n.len()).map(|i|law.area_derivative_n_per_m2[i]*da+(0..law.force_n.len()).map(|j|law.current_jacobian_n_per_m[i][j]*dq1[j]+law.previous_jacobian_n_per_m[i][j]*dq0[j]).sum::<f64>()).collect();
    let mut nodal_derivative:BTreeMap<usize,f64>=BTreeMap::new();
    for i in 0..weights.len() {for (&node,&w) in &weights[i]{let output=&self.nodes[bodies[i]][node];for a in 0..3{*nodal_derivative.entry(output.force_flux[a]).or_default()+=w*df[3*i+a];}}
     if bodies[i]==density_body {if let Some(row)=partials[i].get(&dnode){for (&node,&dw) in row{let output=&self.nodes[density_body][node];for a in 0..3{*nodal_derivative.entry(output.force_flux[a]).or_default()+=dw*law.force_n[3*i+a];}}}}
    }
    let phase_row=if density_body==0{dnode}else{self.nodes[0].len()+dnode};let(columns,values)=self.phase.row(phase_row);
    for (row,value) in nodal_derivative{for (&col,&w) in columns.iter().zip(values){let v=value*w;if v!=0.{jd.push((row,col,v));}}}
   }
   }}
  }
  if force.iter().any(|x|!x.is_finite()){return Err(fail("native closed contact force overflow"));}
  Ok((ContactContribution{force,constraints:vec![],force_current:matrix(self.fluxes,self.states,&jc)?,force_previous:matrix(self.fluxes,self.states,&jp)?,force_design:matrix(self.fluxes,self.phase.ncols(),&jd)?,force_time:vec![0.;self.fluxes],constraint_current:matrix(0,self.states,&vec![])?,constraint_previous:matrix(0,self.states,&vec![])?,constraint_design:matrix(0,self.phase.ncols(),&vec![])?,constraint_time:vec![]},report))
 }
}
impl NativeContactLaw for NativeClosedSurfaceLaw {
 fn step_ledger(&self,_n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<BTreeMap<String,f64>> {
  let (contact,report)=self.contribution_with_report(current,previous,p.design)?;
  let work:f64=self.nodes.iter().flatten().map(|node|(0..3).map(|a|contact.force[node.force_flux[a]]*node.inverse_state_scale[a]*(current[node.state[a]]-previous[node.state[a]])).sum::<f64>()).sum();
  let entries=[("contact_potential_previous_J",report.previous_energy_j),("contact_potential_current_J",report.current_energy_j),("contact_work_J",work),("contact_work_potential_defect_J",work+report.current_energy_j-report.previous_energy_j),("contact_quadrature_count",report.quadrature as f64),("contact_active_quadrature_count",report.active_quadrature as f64),("contact_feature_transfer_count",report.feature_transfers as f64)];
  if entries.iter().any(|(_,v)|!v.is_finite()){return Err(fail("native closed contact ledger overflow"));}Ok(entries.into_iter().map(|(k,v)|(k.to_string(),v)).collect())
 }
 fn multipliers(&self)->usize{0}
 fn evaluate(&self,_n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<ContactContribution>{self.contribution(current,previous,p.design)}
 fn check_state_domain(&self,_n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.contribution(current,previous,p.design).map(|_|())}
 fn check_derivative_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.check_state_domain(n,current,previous,p)}
 fn newton_constraints(&self,_n:usize,_current:&[f64],_previous:&[f64],_p:StepParameters<'_>,_direction:&[f64],_attempt:usize)->CaeResult<Option<Jacobian>>{Ok(None)}
 fn admitted_trial(&self,_n:usize,current:&[f64],direction:&[f64],previous:&[f64],p:StepParameters<'_>,trial:f64)->CaeResult<f64>{
  if current.len()!=self.states||direction.len()!=self.states||direction.iter().any(|v|!v.is_finite())||!trial.is_finite()||trial<=0.||trial>1. {return Err(fail("native closed contact trial"));}
  let mut fraction=trial;for _ in 0..32 {let candidate:Vec<_>=current.iter().zip(direction).map(|(v,d)|v+fraction*d).collect();if self.contribution(&candidate,previous,p.design).is_ok(){return Ok(fraction);}fraction*=0.5;}Ok(0.)
 }
}
