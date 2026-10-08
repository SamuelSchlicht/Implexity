// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_core::json::sha256_hex;
use crate::{problem::normalise,moving_contact::surface_map::{SurfaceFeature,PointMap}};
use implexity_physics_solid::soft_fsi::voxel::kuhn_tetrahedra;
use super::mapped_surface_patch::MappedSurfacePatch;
use serde_json::Value;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct AuthenticatedReferencePatch{patch:MappedSurfacePatch,references:Vec<[f64;3]>,nodes:Vec<[f64;3]>,shell_sha:String,body_sha:String}
impl AuthenticatedReferencePatch{
 pub fn read(path:&std::path::Path,shell_sha:&str,body_sha:&str)->CaeResult<Self>{
  if [shell_sha,body_sha].iter().any(|s|s.len()!=64||!s.bytes().all(|b|b.is_ascii_hexdigit())){return Err(fail("reference SHA shape"));}
  let bytes=std::fs::read(path).map_err(|_|fail("reference bytes"))?;if sha256_hex(&bytes)!=shell_sha{return Err(fail("reference SHA mismatch"));}
  let doc:Value=serde_json::from_slice(&bytes).map_err(|_|fail("reference JSON"))?;if doc["schema"]!="native-closed-reference-surface/1"&&doc["schema"]!="native-closed-reference-tissue-shell/1"{return Err(fail("reference schema"));}
  let body=doc["body"].as_u64().ok_or_else(||fail("reference body"))? as usize;if body>1{return Err(fail("reference body index"));}
  let body_path=doc["native_body_path"].as_str().ok_or_else(||fail("reference native body path"))?;let body_bytes=std::fs::read(body_path).map_err(|_|fail("reference native body bytes"))?;if sha256_hex(&body_bytes)!=body_sha{return Err(fail("reference native body SHA mismatch"));}
  let body_doc:Value=serde_json::from_slice(&body_bytes).map_err(|_|fail("reference native body JSON"))?;let nodes=kuhn_tetrahedra(&normalise(&body_doc)?.solid.grid)?.mesh.points;
  let vertices=doc["vertices"].as_array().ok_or_else(||fail("reference vertices"))?;let mut references=vec![];let mut features=vec![];
  for v in vertices{let q:[f64;3]=serde_json::from_value(v["reference_position_m"].clone()).map_err(|_|fail("reference point shape"))?;if q.iter().any(|v|!v.is_finite()){return Err(fail("reference point finite"));}let f=&v["feature"];if f["kind"]!="fixed_tetrahedron"{return Err(fail("fixed reference feature owner required"));}let feature=SurfaceFeature::FixedTetrahedron{nodes:serde_json::from_value(f["nodes"].clone()).map_err(|_|fail("reference native indices"))?,weights:serde_json::from_value(f["weights"].clone()).map_err(|_|fail("reference native weights"))?,weight_tolerance:f["weight_tolerance"].as_f64().ok_or_else(||fail("reference weight tolerance"))?};feature.map(&vec![0.;nodes.len()])?.position(&nodes)?;references.push(q);features.push(feature);}
  let facets=serde_json::from_value(doc["facets"].clone()).map_err(|_|fail("reference facets"))?;let patch=MappedSurfacePatch::new(body,features,facets,&nodes,&vec![0.;nodes.len()])?;if !patch.boundary_edges().is_empty(){return Err(fail("authenticated closed reference surface has boundary edges"));}Ok(Self{patch,references,nodes,shell_sha:shell_sha.into(),body_sha:body_sha.into()})
 }
 pub fn patch(&self)->&MappedSurfacePatch{&self.patch}
 pub fn reference(&self,i:usize)->CaeResult<[f64;3]>{self.references.get(i).copied().ok_or_else(||fail("reference vertex index"))}
 pub fn nodes(&self)->&[[f64;3]]{&self.nodes}
 pub fn identity(&self)->String{sha256_hex(format!("{}:{}",self.shell_sha,self.body_sha).as_bytes())}
 pub fn require_shape_derivative(&self)->CaeResult<()>{Err(fail("fixed authored reference embedding has no shape/re-extraction derivative owner"))}
}
pub fn position(map:&PointMap,reference:[f64;3],displacement:&[[f64;3]])->CaeResult<[f64;3]>{let u=map.position(displacement)?;let q=std::array::from_fn(|i|reference[i]+u[i]);if q.iter().any(|v|!v.is_finite()){return Err(fail("embedded reference position finite"));}Ok(q)}
