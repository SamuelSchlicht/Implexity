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
