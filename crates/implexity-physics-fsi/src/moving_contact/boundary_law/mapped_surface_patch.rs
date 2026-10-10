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
use crate::moving_contact::surface_map::{SurfaceFeature,PointMap};
use std::collections::BTreeMap;
#[derive(Clone,Debug)]
pub struct MappedSurfacePatch {body:usize,features:Vec<SurfaceFeature>,facets:Vec<[usize;3]>,edges:Vec<[usize;2]>,boundary_edges:Vec<[usize;2]>,incident_facets:Vec<Vec<usize>>}
#[derive(Clone,Debug,PartialEq,Eq)]
pub enum ContactStratum {VertexFace{vertex:usize,facet:usize},EdgeEdge{first:[usize;2],second:[usize;2]},VertexEdge{vertex:usize,edge:[usize;2]},VertexVertex{first:usize,second:usize}}
#[derive(Clone,Debug)]
pub struct BoundaryContactCandidate {pub bodies:[usize;2],pub stratum:ContactStratum,pub incident_facets:[Vec<usize>;2]}
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn sub(a:[f64;3],b:[f64;3])->[f64;3]{std::array::from_fn(|i|a[i]-b[i])}
fn cross(a:[f64;3],b:[f64;3])->[f64;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
impl MappedSurfacePatch {
 pub fn new(body:usize,features:Vec<SurfaceFeature>,facets:Vec<[usize;3]>,nodes:&[[f64;3]],phase:&[f64])->CaeResult<Self>{
  if features.is_empty()||facets.is_empty()||nodes.is_empty()||nodes.len()!=phase.len()||nodes.iter().flatten().chain(phase).any(|v|!v.is_finite()){return Err(fail("mapped surface shape/finite contract"));}
  let mut incidence=vec![vec![];features.len()];let mut edges:BTreeMap<[usize;2],Vec<(usize,usize)>>=BTreeMap::new();let mut unique=BTreeMap::new();
  for(e,f)in facets.iter().enumerate(){if f.iter().any(|v|*v>=features.len())||f[0]==f[1]||f[1]==f[2]||f[0]==f[2]{return Err(fail("mapped surface connectivity"));}let mut key=*f;key.sort_unstable();if unique.insert(key,e).is_some(){return Err(fail("mapped surface duplicate facet"));}for n in f{incidence[*n].push(e);}for(a,b)in [(f[0],f[1]),(f[1],f[2]),(f[2],f[0])]{let mut key=[a,b];key.sort_unstable();edges.entry(key).or_default().push((a,b));}}
  if incidence.iter().any(|v|v.is_empty())||edges.values().any(|v|v.len()>2||(v.len()==2&&v[0]!=(v[1].1,v[1].0))){return Err(fail("mapped surface nonmanifold or inconsistent orientation"));}
  let out=Self{body,features,facets,edges:edges.keys().copied().collect(),boundary_edges:edges.iter().filter_map(|(e,owners)|(owners.len()==1).then_some(*e)).collect(),incident_facets:incidence};out.positions(nodes,phase)?;Ok(out)
 }
 pub fn body(&self)->usize{self.body}
 pub fn registered_vertex_boundary_candidates(&self,other:&Self,pairs:&[[usize;2]])->CaeResult<Vec<BoundaryContactCandidate>>{if self.body==other.body{return Err(fail("separate surface body ownership required"));}let mut seen=std::collections::BTreeSet::new();let mut out=vec![];for [a,b]in pairs{if !seen.insert([*a,*b]){return Err(fail("duplicate registered boundary candidate"));}out.push(BoundaryContactCandidate{bodies:[self.body,other.body],stratum:ContactStratum::VertexVertex{first:*a,second:*b},incident_facets:[self.incident_facets(*a)?.to_vec(),other.incident_facets(*b)?.to_vec()]});}Ok(out)}
 pub fn require_authored_shape_design_derivative(&self)->CaeResult<()>{Err(fail("frozen authored reference patch requires an independent shape/re-extraction owner"))}
 pub fn features(&self)->&[SurfaceFeature]{&self.features}
 pub fn facets(&self)->&[[usize;3]]{&self.facets}
 pub fn edges(&self)->&[[usize;2]]{&self.edges}
 pub fn boundary_edges(&self)->&[[usize;2]]{&self.boundary_edges}
 pub fn incident_facets(&self,vertex:usize)->CaeResult<&[usize]>{self.incident_facets.get(vertex).map(Vec::as_slice).ok_or_else(||fail("mapped surface vertex index"))}
 pub fn maps(&self,phase:&[f64])->CaeResult<Vec<PointMap>>{if phase.is_empty()||phase.iter().any(|v|!v.is_finite()){return Err(fail("mapped surface nonfinite phase"));}self.features.iter().map(|f|f.map(phase)).collect()}
 pub fn positions(&self,nodes:&[[f64;3]],phase:&[f64])->CaeResult<Vec<[f64;3]>>{if nodes.len()!=phase.len()||nodes.iter().flatten().chain(phase).any(|v|!v.is_finite()){return Err(fail("mapped surface coordinates"));}let p:Vec<_>=self.maps(phase)?.iter().map(|m|m.position(nodes)).collect::<CaeResult<_>>()?;for f in &self.facets{let n=cross(sub(p[f[1]],p[f[0]]),sub(p[f[2]],p[f[0]]));let area=n.iter().map(|v|v*v).sum::<f64>();if !area.is_finite()||area<=0.{return Err(fail("mapped surface zero/overflow area"));}}Ok(p)}
 pub fn direction(&self,nodes:&[[f64;3]],d_nodes:&[[f64;3]],phase:&[f64],d_phase:&[f64])->CaeResult<Vec<[f64;3]>>{self.positions(nodes,phase)?;self.maps(phase)?.iter().map(|m|m.direction(nodes,d_nodes,d_phase)).collect()}
 pub fn scatter(&self,phase:&[f64],force:&[[f64;3]],out:&mut[[f64;3]])->CaeResult<()>{if force.len()!=self.features.len()||out.len()!=phase.len()||force.iter().flatten().chain(out.iter().flatten()).any(|v|!v.is_finite()){return Err(fail("mapped surface force shape"));}for(m,f)in self.maps(phase)?.iter().zip(force){m.scatter(*f,out)?;}Ok(())}
 pub fn scatter_direction(&self,phase:&[f64],force:&[[f64;3]],d_force:&[[f64;3]],d_phase:&[f64],out:&mut[[f64;3]])->CaeResult<()>{if force.len()!=self.features.len()||d_force.len()!=force.len()||out.len()!=phase.len()||d_phase.len()!=phase.len()||force.iter().flatten().chain(d_force.iter().flatten()).chain(out.iter().flatten()).chain(d_phase).any(|v|!v.is_finite()){return Err(fail("mapped surface force direction shape"));}for((m,f),df)in self.maps(phase)?.iter().zip(force).zip(d_force){m.scatter_direction(*f,*df,d_phase,out)?;}Ok(())}
 pub fn require_interior_vertex_face(&self,source:&Self,vertex:usize,facet:usize,barycentric:[f64;3],minimum_barycentric:f64)->CaeResult<ContactStratum>{if self.body==source.body||vertex>=source.features.len()||facet>=self.facets.len()||!minimum_barycentric.is_finite()||minimum_barycentric<=0.||minimum_barycentric>=1./3.||barycentric.iter().any(|v|!v.is_finite()||*v<minimum_barycentric)||((barycentric.iter().sum::<f64>()-1.).abs()>8.*f64::EPSILON){return Err(fail("strict interior vertex-face contact required; boundary stratum must use its own owner"));}Ok(ContactStratum::VertexFace{vertex,facet})}
}
