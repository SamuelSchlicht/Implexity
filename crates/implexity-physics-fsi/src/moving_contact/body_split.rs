// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use serde_json::{Value,json};
use crate::problem::{FsiProblem,normalise};
use crate::json::{mask_of,encode_mask};
use implexity_physics_solid::soft_fsi::voxel::VoxelGrid;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct SplitBodyDocuments{pub grids:[VoxelGrid;2],pub original_grid:VoxelGrid,pub starts:[[usize;3];2],pub bodies:[Value;2],pub source_voxels:[Vec<usize>;2],pub original_identity:String,pub original_responses:Value,pub original_observables:Value,pub original_mirror_axis:Option<usize>,pub original_design_size:usize}
impl SplitBodyDocuments{
 pub fn gather_design(&self,source:&[f64])->CaeResult<Vec<f64>>{if source.len()!=self.original_design_size||source.iter().any(|v|!v.is_finite()){return Err(fail("split design source shape"));}Ok(self.source_voxels.iter().flatten().map(|i|source[*i]).collect())}
 pub fn scatter_design_cotangent(&self,bar:&[f64])->CaeResult<Vec<f64>>{if bar.len()!=self.original_design_size||bar.iter().any(|v|!v.is_finite()){return Err(fail("split design cotangent shape"));}let mut out=vec![0.;self.original_design_size];for(i,v)in self.source_voxels.iter().flatten().zip(bar){out[*i]+=v;}Ok(out)}
}
pub fn split_native_grid(problem:&FsiProblem,axis:usize,index:usize)->CaeResult<SplitBodyDocuments>{
 let old=&problem.solid.grid;if axis>=3||index==0||index>=old.shape[axis]{return Err(fail("split must be an internal native voxel plane"));}
 if problem.design.removal.is_some()||problem.solid.removal_modifier.is_some(){return Err(fail("removal/scar split requires reference-depth chain preservation"));}
 if !problem.solid.contact_planes.is_empty(){return Err(fail("split requires an explicit plane-free original problem"));}
 let mut docs=vec![];let mut maps=vec![];let mut grids=vec![];let mut starts=[[0;3];2];starts[1][axis]=index;
 for body in 0..2{
  let mut shape=old.shape;shape[axis]=if body==0{index}else{old.shape[axis]-index};let mut origin=old.origin_m;if body==1{origin[axis]+=index as f64*old.element_size_m;}
  let grid=VoxelGrid{origin_m:origin,shape,element_size_m:old.element_size_m};let mut map=vec![];
  for x in 0..shape[0]{for y in 0..shape[1]{for z in 0..shape[2]{let mut ijk=[x,y,z];if body==1{ijk[axis]+=index;}map.push(old.voxel_index(ijk));}}}
  let mut doc=problem.normal_form().clone();doc["solid"]["reference_grid"]=json!({"origin_m":origin,"shape":shape,"element_size_m":old.element_size_m});
  doc["design"]["region"]=json!(map.iter().map(|i|problem.design.region[*i]).collect::<Vec<_>>());doc["design"]["initial_density"]=json!(map.iter().map(|i|problem.design.initial_density[*i]).collect::<Vec<_>>());doc["design"]["protected_density"]=json!(map.iter().map(|i|problem.design.protected_density[*i]).collect::<Vec<_>>());doc["design"]["protected_solid"]=Value::Null;doc["design"]["symmetry"]=Value::Null;
  let material=&problem.solid.material_map;let mut entries=vec![];for m in 1..material.materials.len(){let mask:Vec<_>=map.iter().map(|i|material.voxel_material[*i]==m).collect();if mask.iter().any(|v|*v){entries.push(json!({"label":material.labels[m-1],"region":encode_mask(&mask,shape),"material":material.law_objects[m-1]}));}}doc["solid"]["material_map"]=json!(entries);
  if let Some(supports)=doc["solid"]["supports"].as_array_mut(){for support in supports{if let Some(region)=support.get("region"){let mask=mask_of(region,"split support",old.shape)?;support["region"]=encode_mask(&map.iter().map(|i|mask[*i]).collect::<Vec<_>>(),shape);}}}
  if let Some(supports)=doc["solid"]["supports"].as_array_mut(){let mut retained=vec![];for support in supports.iter(){let selected=if let Some(v)=support.get("box_m"){let bounds: [[f64;3];2]=serde_json::from_value(v.clone()).map_err(|_|fail("split support box"))?;(0..grid.node_count()).any(|i|crate::json::in_box(&bounds,grid.node_position(i),grid.element_size_m))}else if let Some(v)=support.get("region"){let mask=mask_of(v,"split support",grid.shape)?;!grid.voxel_nodes(&mask)?.is_empty()}else{return Err(fail("split support selection"));};if selected{retained.push(support.clone());}}*supports=retained;}
  let mut observations=vec![];for obs in problem.normal_form()["observables"].as_array().ok_or_else(||fail("split observable list"))?{
   let kind=obs["kind"].as_str().unwrap_or("");if kind=="probe_separation"{continue;}
   if kind=="probe_displacement"||kind=="probe_velocity"{let p=obs["point_m"].as_array().ok_or_else(||fail("split physical probe"))?;let v=p[axis].as_f64().ok_or_else(||fail("split physical probe coordinate"))?;let cut=old.origin_m[axis]+index as f64*old.element_size_m;if(body==0&&v>=cut)||(body==1&&v<cut){continue;}}
   observations.push(obs.clone());
  }
  doc["observables"]=json!(observations);let mut response=problem.normal_form()["responses"].clone();let terms=response["terms"].as_array().ok_or_else(||fail("split response terms"))?;let retained:Vec<_>=terms.iter().filter(|v|v["functional"]["kind"]=="design_volume_fraction").cloned().collect();if retained.is_empty(){return Err(fail("split diagnostic needs an authored design volume response"));}response["terms"]=json!(retained);doc["responses"]=response;let normalized=normalise(&doc)?;
  if normalized.solid.grid.shape!=grid.shape||normalized.solid.grid.origin_m!=grid.origin_m{return Err(fail("split native grid normalization changed"));}
  grids.push(grid);docs.push(doc);maps.push(map);
 }
 Ok(SplitBodyDocuments{grids:grids.try_into().map_err(|_|fail("split grid count"))?,original_grid:old.clone(),starts,bodies:docs.try_into().map_err(|_|fail("split body count"))?,source_voxels:maps.try_into().map_err(|_|fail("split map count"))?,original_identity:problem.identity().to_string(),original_responses:problem.normal_form()["responses"].clone(),original_observables:problem.normal_form()["observables"].clone(),original_mirror_axis:problem.design.mirror_axis,original_design_size:old.voxel_count()})
}

impl SplitBodyDocuments{
 pub fn fixed_reference_feature(&self,body:usize,source_nodes:[usize;4],weights:[f64;4],weight_tolerance:f64)->CaeResult<super::surface_map::SurfaceFeature>{
  if body>=2||source_nodes.iter().any(|i|*i>=self.original_grid.node_count()){return Err(fail("reference trace source/body index"));}
  let mut nodes=[0;4];for(i,node)in source_nodes.into_iter().enumerate(){let old=self.original_grid.node_ijk(node);let mut local=[0;3];for a in 0..3{if old[a]<self.starts[body][a]{return Err(fail("reference trace belongs to another body"));}local[a]=old[a]-self.starts[body][a];if local[a]>self.grids[body].shape[a]{return Err(fail("reference trace belongs to another body"));}}nodes[i]=self.grids[body].node_index(local);}
  if !weight_tolerance.is_finite()||!(0. ..=32.*f64::EPSILON).contains(&weight_tolerance)||weights.iter().any(|v|!v.is_finite()||*v< -weight_tolerance)||(weights.iter().sum::<f64>()-1.).abs()>weight_tolerance||nodes.iter().enumerate().any(|(i,v)|nodes[..i].contains(v)){return Err(fail("reference tetrahedron weights/nodes"));}Ok(super::surface_map::SurfaceFeature::FixedTetrahedron{nodes,weights,weight_tolerance})
 }
}
