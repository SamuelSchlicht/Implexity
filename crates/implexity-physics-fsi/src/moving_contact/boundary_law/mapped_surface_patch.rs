// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
