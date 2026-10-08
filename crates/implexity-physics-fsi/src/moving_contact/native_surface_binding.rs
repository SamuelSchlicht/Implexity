// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::multirate_coupling::FluxDrivenField;
use implexity_physics_solid::soft_fsi::voxel::kuhn_tetrahedra;
use super::{separate_body::{MovingFsiPairModel,PairSolidField},mapped_pair_law::NodeBinding,boundary_law::{surface_admission::DensitySurfaceMap,exposed_surface::IsoSurfacePolicy}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct NativeSurfaceBinding{nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,surfaces:[DensitySurfaceMap;2],density:[Vec<f64>;2],state_size:usize,flux_size:usize,identity:String,design_identity:String}
impl NativeSurfaceBinding{
 pub fn new(model:&MovingFsiPairModel,pair:&PairSolidField<'_>,design:&[f64],policy:IsoSurfacePolicy)->CaeResult<Self>{
  let native=NativeNodalBinding::new(model,pair,design)?;let mut surfaces=vec![];for b in 0..2{surfaces.push(DensitySurfaceMap::new(b,&native.meshes[b].mesh.points,&native.meshes[b].mesh.elements,&native.density[b],policy)?);}
  let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"native_binding":native.identity(),"surface_meshes":[surfaces[0].surface().mesh_identity(),surfaces[1].surface().mesh_identity()],"surface_strata":[surfaces[0].surface().stratum_identity(),surfaces[1].surface().stratum_identity()]}));
  Ok(Self{nodes:native.nodes,phase:native.phase,surfaces:surfaces.try_into().map_err(|_|fail("native full-surface pair"))?,density:native.density,state_size:native.state_size,flux_size:native.flux_size,identity,design_identity:native.design_identity})
 }
 pub fn identity(&self)->&str{&self.identity}
 pub fn nodes(&self)->&[Vec<NodeBinding>;2]{&self.nodes}
 pub fn phase(&self)->&CsrMatrix{&self.phase}
 pub fn surfaces(&self)->&[DensitySurfaceMap;2]{&self.surfaces}
 pub fn density(&self)->&[Vec<f64>;2]{&self.density}
 pub fn density_direction(&self,design_direction:&[f64])->CaeResult<[Vec<f64>;2]>{if design_direction.len()!=self.phase.ncols()||design_direction.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface design direction"));}let mut out=vec![0.;self.phase.nrows()];for(r,x)in out.iter_mut().enumerate(){let(c,v)=self.phase.row(r);for(&i,&a)in c.iter().zip(v){*x+=a*design_direction[i];}}if out.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface phase overflow"));}let split=self.nodes[0].len();Ok([out[..split].to_vec(),out[split..].to_vec()])}
 pub fn density_pullback(&self,bars:[&[f64];2])->CaeResult<Vec<f64>>{if(0..2).any(|b|bars[b].len()!=self.nodes[b].len()||bars[b].iter().any(|x|!x.is_finite())){return Err(fail("native full-surface phase cotangent"));}let mut out=vec![0.;self.phase.ncols()];let mut row=0;for b in 0..2{for &bar in bars[b]{let(c,v)=self.phase.row(row);for(&i,&a)in c.iter().zip(v){out[i]+=a*bar;}row+=1;}}if out.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface phase cotangent overflow"));}Ok(out)}
 fn points(&self,state:&[f64])->CaeResult<[Vec<[f64;3]>;2]>{if state.len()<self.state_size||state.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface state shape"));}Ok(std::array::from_fn(|b|self.nodes[b].iter().map(|n|std::array::from_fn(|a|n.reference_m[a]+n.inverse_state_scale[a]*state[n.state[a]])).collect()))}
 pub fn positions(&self,state:&[f64])->CaeResult<[Vec<[f64;3]>;2]>{let points=self.points(state)?;Ok([self.surfaces[0].surface().positions(&points[0],&self.density[0])?,self.surfaces[1].surface().positions(&points[1],&self.density[1])?])}
 pub fn direction(&self,state:&[f64],state_direction:&[f64],design_direction:&[f64])->CaeResult<[Vec<[f64;3]>;2]>{let points=self.points(state)?;if state_direction.len()!=state.len()||state_direction.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface state direction"));}let dp:[Vec<[f64;3]>;2]=std::array::from_fn(|b|self.nodes[b].iter().map(|n|std::array::from_fn(|a|n.inverse_state_scale[a]*state_direction[n.state[a]])).collect());let dr=self.density_direction(design_direction)?;Ok([self.surfaces[0].surface().direction(&points[0],&dp[0],&self.density[0],&dr[0])?,self.surfaces[1].surface().direction(&points[1],&dp[1],&self.density[1],&dr[1])?])}
 pub fn pullback(&self,state:&[f64],bars:[&[[f64;3]];2])->CaeResult<(Vec<f64>,Vec<f64>)>{let points=self.points(state)?;let mut sb=vec![0.;state.len()];let mut db:[Vec<f64>;2]=std::array::from_fn(|b|vec![0.;self.nodes[b].len()]);for b in 0..2{let mut pb=vec![[0.;3];self.nodes[b].len()];self.surfaces[b].surface().pullback(&points[b],&self.density[b],bars[b],&mut pb,&mut db[b])?;for(n,v)in self.nodes[b].iter().zip(pb){for a in 0..3{sb[n.state[a]]+=n.inverse_state_scale[a]*v[a];}}}Ok((sb,self.density_pullback([&db[0],&db[1]])?))}
 pub fn scatter_force(&self,force:[&[[f64;3]];2])->CaeResult<Vec<f64>>{let mut out=vec![0.;self.flux_size];for b in 0..2{let mut nodal=vec![[0.;3];self.nodes[b].len()];self.surfaces[b].surface().scatter(&self.density[b],force[b],&mut nodal)?;for(n,v)in self.nodes[b].iter().zip(nodal){for a in 0..3{out[n.force_flux[a]]+=v[a];}}}Ok(out)}
 pub fn scatter_force_direction(&self,force:[&[[f64;3]];2],force_direction:[&[[f64;3]];2],design_direction:&[f64])->CaeResult<Vec<f64>>{let dr=self.density_direction(design_direction)?;let mut out=vec![0.;self.flux_size];for b in 0..2{let mut nodal=vec![[0.;3];self.nodes[b].len()];self.surfaces[b].surface().scatter_direction(&self.density[b],force[b],force_direction[b],&dr[b],&mut nodal)?;for(n,v)in self.nodes[b].iter().zip(nodal){for a in 0..3{out[n.force_flux[a]]+=v[a];}}}Ok(out)}
 pub fn scatter_force_pullback(&self,force:[&[[f64;3]];2],flux_bar:&[f64])->CaeResult<([Vec<[f64;3]>;2],Vec<f64>)>{if flux_bar.len()!=self.flux_size||flux_bar.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface force cotangent"));}let mut fb:[Vec<[f64;3]>;2]=std::array::from_fn(|_|vec![]);let mut db:[Vec<f64>;2]=std::array::from_fn(|b|vec![0.;self.nodes[b].len()]);for b in 0..2{let surface=self.surfaces[b].surface();surface.require_fixed_stratum(&self.density[b])?;if force[b].len()!=surface.features().len()||force[b].iter().flatten().any(|x|!x.is_finite()){return Err(fail("native full-surface force shape"));}let nodal:Vec<[f64;3]>=self.nodes[b].iter().map(|n|std::array::from_fn(|a|flux_bar[n.force_flux[a]])).collect();for(feature,f)in surface.features().iter().zip(force[b]){let map=feature.map(&self.density[b])?;fb[b].push(map.position(&nodal)?);map.density_pullback(&nodal,*f,&mut db[b])?;}}Ok((fb,self.density_pullback([&db[0],&db[1]])?))}
 pub fn control_pullback(&self,model:&MovingFsiPairModel,controls:[&[f64];2],design_bar:&[f64])->CaeResult<[Vec<f64>;2]>{let design=model.event_physical_design(controls)?;if design_bar.len()!=design.len()||design_bar.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface control cotangent"));}if implexity_core::json::canonical_sha256(&serde_json::json!(design))!=self.design_identity{return Err(fail("native full-surface control binding"));}let n=model.native_body(0)?.chain.len();Ok([model.native_body(0)?.chain.pullback(controls[0],&design_bar[..n])?,model.native_body(1)?.chain.pullback(controls[1],&design_bar[n..])?])}
}

#[derive(Clone)]
pub struct DensityBoundarySelection{pub source_body:usize,pub source_vertex:usize,pub target_vertex:usize,pub target_facet:usize,pub gap_scale_m:f64,pub force_scale_n:f64,pub path_policy:super::boundary_law::boundary_path::PathPolicy}
impl NativeSurfaceBinding{
 pub fn plane_contact_law(&self,design:&[f64],selections:&[DensityBoundarySelection],plane:super::boundary_law::plane_manifold::SupportingPlaneSelection,residual_tolerance:f64)->CaeResult<super::boundary_law::plane_manifold::PlaneCertifiedContact<super::collection::MultipleContact<super::boundary_law::boundary_mapped_pair_law::BoundaryMappedPairLaw>>>{
  if selections.is_empty()||!residual_tolerance.is_finite()||residual_tolerance<=0.||implexity_core::json::canonical_sha256(&serde_json::json!(design))!=self.design_identity{return Err(fail("native density contact collection design/policy binding"));}
  let boundary_nodes:[Vec<super::boundary_law::boundary_mapped_pair_law::NodeBinding>;2]=std::array::from_fn(|b|self.nodes[b].iter().map(|n|super::boundary_law::boundary_mapped_pair_law::NodeBinding{reference_m:n.reference_m,state:n.state,inverse_state_scale:n.inverse_state_scale,force_flux:n.force_flux}).collect());let mut laws=vec![];let mut selected=std::collections::BTreeSet::new();for s in selections{if s.source_body>1||!selected.insert((s.source_body,s.source_vertex,s.target_vertex,s.target_facet)){return Err(fail("native density contact duplicate selection"));}let law=super::boundary_law::boundary_mapped_pair_law::BoundaryMappedPairLaw::from_density_surface_vertex_pair(boundary_nodes.clone(),self.phase.clone(),self.state_size,self.flux_size,s.gap_scale_m,s.force_scale_n,[&self.surfaces[0],&self.surfaces[1]],design,s.source_body,s.source_vertex,s.target_vertex,s.target_facet)?.with_path_policy(s.path_policy)?.with_residual_path_allowance(residual_tolerance)?;laws.push(law);}
  let law=super::collection::MultipleContact::new(laws,self.state_size,self.flux_size,self.phase.ncols())?;let trace=super::boundary_law::plane_manifold::CompleteSurfaceTrace::density(boundary_nodes,self.phase.clone(),self.state_size,[&self.surfaces[0],&self.surfaces[1]])?;super::boundary_law::plane_manifold::PlaneCertifiedContact::new(law,trace,plane)
 }
}

pub struct NativeNodalBinding{nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,meshes:[implexity_physics_solid::soft_fsi::voxel::KuhnMesh;2],density:[Vec<f64>;2],state_size:usize,flux_size:usize,identity:String,design_identity:String}
impl NativeNodalBinding{
 pub fn new(model:&MovingFsiPairModel,pair:&PairSolidField<'_>,design:&[f64])->CaeResult<Self>{
  if design.len()!=pair.design_size()||design.iter().any(|x|!x.is_finite()){return Err(fail("native full-surface physical design shape"));}
  let mut nodes:[Vec<NodeBinding>;2]=std::array::from_fn(|_|vec![]);let(mut ri,mut ci,mut values)=(vec![],vec![],vec![]);let(mut so,mut fo,mut doff,mut row)=(0,0,0,0);let mut meshes=vec![];
  for b in 0..2{let grid=&model.native_body(b)?.problem.solid.grid;let field=pair.body_field(b)?;if !std::ptr::eq(field.core().model(),&model.native_body(b)?.soft){return Err(fail("native nodal borrowed body resource identity"));}if grid.voxel_count()>field.design_size(){return Err(fail("native full-surface material prefix"));}if design[doff..doff+grid.voxel_count()].iter().any(|x|!(0. ..=1.).contains(x)){return Err(fail("native nodal material density domain"));}let scale=field.core().scale();
   for id in 0..grid.node_count(){let mut state=[0;3];let mut inv=[0.;3];for a in 0..3{let(c,v)=field.trace_operator().row(3*id+a);if c.len()!=1||v!=[1.]||c[0]>=scale.len()||!scale[c[0]].is_finite()||scale[c[0]]<=0.{return Err(fail("native full-surface displacement trace"));}state[a]=so+c[0];inv[a]=1./scale[c[0]];}
    nodes[b].push(NodeBinding{reference_m:grid.node_position(id),state,inverse_state_scale:inv,force_flux:[fo+3*id,fo+3*id+1,fo+3*id+2]});let ijk=grid.node_ijk(id);let mut incident=vec![];
    for x in 0..2{for y in 0..2{for z in 0..2{let s=[x,y,z];let mut v=[0;3];let mut ok=true;for a in 0..3{if ijk[a]<s[a]{ok=false;break;}v[a]=ijk[a]-s[a];if v[a]>=grid.shape[a]{ok=false;break;}}if ok{incident.push(grid.voxel_index(v));}}}}
    if incident.is_empty(){return Err(fail("native full-surface incident phase"));}let w=1./incident.len()as f64;for v in incident{ri.push(row);ci.push(doff+v);values.push(w);}row+=1;
   }
   meshes.push(kuhn_tetrahedra(grid)?);so+=field.state_size();fo+=field.trace_operator().nrows();doff+=field.design_size();
  }
  if so!=pair.state_size()||fo!=pair.trace_operator().nrows()||doff!=design.len(){return Err(fail("native full-surface paired layout"));}
  let phase=CsrMatrix::from_triplets(row,design.len(),&ri,&ci,&values).map_err(|e|CaeError::contract(e.to_string()))?;let mut flat=vec![0.;row];for(r,x)in flat.iter_mut().enumerate(){let(c,v)=phase.row(r);for(&i,&a)in c.iter().zip(v){*x+=a*design[i];}}
  let split=nodes[0].len();let density=[flat[..split].to_vec(),flat[split..].to_vec()];let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"model_identity":model.identity(),"node_bindings":nodes.iter().map(|body|body.iter().map(|n|serde_json::json!({"reference_m":n.reference_m,"state":n.state,"inverse_state_scale":n.inverse_state_scale,"force_flux":n.force_flux})).collect::<Vec<_>>()).collect::<Vec<_>>(),"design":design,"state_size":so,"flux_size":fo}));
  Ok(Self{nodes,phase,meshes:meshes.try_into().map_err(|_|fail("native nodal mesh pair"))?,density,state_size:so,flux_size:fo,identity,design_identity:implexity_core::json::canonical_sha256(&serde_json::json!(design))})
 }
 pub fn identity(&self)->&str{&self.identity}
 pub fn nodes(&self)->&[Vec<NodeBinding>;2]{&self.nodes}
 pub fn phase(&self)->&CsrMatrix{&self.phase}
 pub fn meshes(&self)->&[implexity_physics_solid::soft_fsi::voxel::KuhnMesh;2]{&self.meshes}
 pub fn density(&self)->&[Vec<f64>;2]{&self.density}
 pub fn state_size(&self)->usize{self.state_size}
 pub fn flux_size(&self)->usize{self.flux_size}
 pub fn require_design(&self,design:&[f64])->CaeResult<()>{if design.len()!=self.phase.ncols()||design.iter().any(|x|!x.is_finite())||implexity_core::json::canonical_sha256(&serde_json::json!(design))!=self.design_identity{return Err(fail("native nodal design identity"));}Ok(())}
 pub fn boundary_nodes(&self)->[Vec<super::boundary_law::boundary_mapped_pair_law::NodeBinding>;2]{std::array::from_fn(|b|self.nodes[b].iter().map(|n|super::boundary_law::boundary_mapped_pair_law::NodeBinding{reference_m:n.reference_m,state:n.state,inverse_state_scale:n.inverse_state_scale,force_flux:n.force_flux}).collect())}
}
