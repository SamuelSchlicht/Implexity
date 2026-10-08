// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use implexity_ad::{Dual,Scalar};
use implexity_geometry::domain_sdf::{TriMesh,winding_number};
use std::collections::{BTreeMap,BTreeSet};
use super::surface_quadrature_ownership::closest_triangle;
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn sub(a:[f64;3],b:[f64;3])->[f64;3]{std::array::from_fn(|i|a[i]-b[i])}
fn dot(a:[f64;3],b:[f64;3])->f64{a.into_iter().zip(b).map(|(x,y)|x*y).sum()}
fn cross(a:[f64;3],b:[f64;3])->[f64;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
#[derive(Clone,Copy)]
pub struct DistancePolicy{pub minimum_area_ratio:f64,pub minimum_parameter:f64,pub tie_distance_m:f64,pub minimum_distance_m:f64,pub winding_tolerance:f64}
#[derive(Clone,Debug,PartialEq,Eq)]
pub enum DistanceFeature{Face(usize),Edge([usize;2]),Vertex(usize)}
#[derive(Clone,Debug)]
pub struct DistanceDerivative{pub point_gradient:[f64;3],pub vertex_gradient:Vec<(usize,[f64;3])>}
#[derive(Clone,Debug)]
pub struct SurfaceDistance{pub signed_distance_m:f64,pub winding_number:f64,pub closest_point_m:[f64;3],pub feature:DistanceFeature,pub coincident_facets:usize,pub derivative:Option<DistanceDerivative>}
pub struct ClosedSurfaceDistance{mesh:TriMesh,winding_mesh:TriMesh,input_winding_sign:Option<f64>,incident:Vec<Vec<usize>>,outward:Vec<f64>,policy:DistancePolicy}
impl ClosedSurfaceDistance{
 pub fn new(mesh:TriMesh,policy:DistancePolicy)->CaeResult<Self>{
  if mesh.v.is_empty()||mesh.f.is_empty()||mesh.v.iter().flatten().any(|v|!v.is_finite())||!policy.minimum_area_ratio.is_finite()||policy.minimum_area_ratio<=0.||policy.minimum_area_ratio>=1.||!policy.minimum_parameter.is_finite()||policy.minimum_parameter<=0.||policy.minimum_parameter>=1./3.||!policy.tie_distance_m.is_finite()||policy.tie_distance_m<0.||!policy.minimum_distance_m.is_finite()||policy.minimum_distance_m<=0.||!policy.winding_tolerance.is_finite()||policy.winding_tolerance<=0.||policy.winding_tolerance>=0.5{return Err(fail("closed surface distance policy"));}
  let mut edges:BTreeMap<[usize;2],(usize,i32)>=BTreeMap::new();let mut faces=BTreeSet::new();let mut incident=vec![vec![];mesh.v.len()];
  for (index,f) in mesh.f.iter().enumerate(){
   if f.iter().any(|v|*v>=mesh.v.len()){return Err(fail("closed surface vertex index"));}
   let mut key=*f;key.sort();if key[0]==key[1]||key[1]==key[2]||!faces.insert(key){return Err(fail("closed surface duplicate or collapsed facet"));}
   for &v in f{incident[v].push(index);}
   for (a,b) in [(f[0],f[1]),(f[1],f[2]),(f[2],f[0])]{let k=[a.min(b),a.max(b)];let e=edges.entry(k).or_default();e.0+=1;e.1+=if a<b{1}else{-1};}
   closest_triangle(mesh.v[f[0]],&mesh.v,&[*f],policy.minimum_area_ratio,policy.minimum_parameter,policy.tie_distance_m)?;
  }
  if edges.values().any(|(n,s)|*n!=2||*s!=0){return Err(fail("closed oriented two-manifold surface required"));}
  for (vertex,star) in incident.iter().enumerate(){
   if star.is_empty(){return Err(fail("closed surface unused vertex"));}
   let mut link:BTreeMap<usize,BTreeSet<usize>>=BTreeMap::new();
   for &index in star{let pair:Vec<_>=mesh.f[index].into_iter().filter(|v|*v!=vertex).collect();link.entry(pair[0]).or_default().insert(pair[1]);link.entry(pair[1]).or_default().insert(pair[0]);}
   if link.values().any(|v|v.len()!=2){return Err(fail("closed surface vertex link degree"));}
   let mut pending=vec![*link.keys().next().ok_or_else(||fail("closed surface vertex link absent"))?];let mut seen=BTreeSet::new();
   while let Some(v)=pending.pop(){if seen.insert(v){pending.extend(link[&v].iter().copied());}}
   if seen.len()!=link.len(){return Err(fail("closed surface disconnected vertex link"));}
  }
  let mut components=vec![];let mut assigned=vec![false;mesh.f.len()];
  for first in 0..mesh.f.len(){if assigned[first]{continue;}let origin=mesh.v[mesh.f[first][0]];let mut pending=vec![first];let mut visited=BTreeSet::new();let mut volume=0.;
   while let Some(index)=pending.pop(){if !visited.insert(index){continue;}let f=mesh.f[index];let a=sub(mesh.v[f[0]],origin);let b=sub(mesh.v[f[1]],origin);let c=sub(mesh.v[f[2]],origin);volume+=dot(a,cross(b,c))/6.;for v in f{pending.extend(incident[v].iter().copied());}}
   if !volume.is_finite()||volume==0.{return Err(fail("closed surface component volume"));}for &index in &visited{assigned[index]=true;}components.push((visited.into_iter().collect::<Vec<_>>(),origin,if volume>0.{1.}else{-1.}));
  }
  let mut outward=vec![0.;mesh.f.len()];let mut winding_mesh=mesh.clone();
  for (i,(facets,point,sign)) in components.iter().enumerate(){let mut depth=0;
   for (j,(other,_,_)) in components.iter().enumerate(){if i==j{continue;}let shell=TriMesh{v:mesh.v.clone(),f:other.iter().map(|f|mesh.f[*f]).collect()};let w=winding_number(&shell,&[*point])[0].abs();
    if !w.is_finite(){return Err(fail("closed surface nesting winding overflow"));}if (w-1.).abs()<=policy.winding_tolerance{depth+=1;}else if w>policy.winding_tolerance{return Err(fail("closed surface component nesting is ambiguous"));}
   }
   let normal_sign=sign*if depth%2==0{1.}else{-1.};for &index in facets{outward[index]=normal_sign;if normal_sign<0.{winding_mesh.f[index].swap(1,2);}}
  }
  let input_winding_sign=if outward.iter().all(|x|*x==1.){Some(1.)}else if outward.iter().all(|x|*x== -1.){Some(-1.)}else{None};
  Ok(Self{mesh,winding_mesh,input_winding_sign,incident,outward,policy})
 }
 pub fn query(&self,point:[f64;3])->CaeResult<SurfaceDistance>{
  let p=self.policy;let best=closest_triangle(point,&self.mesh.v,&self.mesh.f,p.minimum_area_ratio,p.minimum_parameter,p.tie_distance_m)?;let triangle=self.mesh.f[best.facet];
  let closest_point_m=std::array::from_fn(|a|(0..3).map(|i|best.barycentric[i]*self.mesh.v[triangle[i]][a]).sum());
  let mut support:Vec<_>=triangle.iter().zip(best.barycentric).filter_map(|(&v,w)|(w>0.).then_some(v)).collect();support.sort();
  let feature=match support.as_slice(){[a]=>DistanceFeature::Vertex(*a),[a,b]=>DistanceFeature::Edge([*a,*b]),[_,_,_]=>DistanceFeature::Face(best.facet),_=>return Err(fail("closest surface feature support"))};
  let mut coincident_facets=0;let mut unique=true;
  for f in &self.mesh.f{
   let lower:f64=(0..3).map(|a|{let lo=f.iter().map(|v|self.mesh.v[*v][a]).fold(f64::INFINITY,f64::min);let hi=f.iter().map(|v|self.mesh.v[*v][a]).fold(f64::NEG_INFINITY,f64::max);let d=if point[a]<lo{lo-point[a]}else if point[a]>hi{point[a]-hi}else{0.};d*d}).sum();
   if lower.sqrt()>best.distance_m+p.tie_distance_m{continue;}
   let c=closest_triangle(point,&self.mesh.v,&[*f],p.minimum_area_ratio,p.minimum_parameter,p.tie_distance_m)?;
   if (c.distance_m-best.distance_m).abs()<=p.tie_distance_m{coincident_facets+=1;let mut s:Vec<_>=f.iter().zip(c.barycentric).filter_map(|(&v,w)|(w>0.).then_some(v)).collect();s.sort();if s!=support{unique=false;}}
  }
  let winding=winding_number(&self.winding_mesh,&[point])[0];if !winding.is_finite(){return Err(fail("surface winding overflow"));}
  let inside=(winding.abs()-1.).abs()<=p.winding_tolerance;let outside=winding.abs()<=p.winding_tolerance;
  if !inside&&!outside&&best.distance_m>p.minimum_distance_m{return Err(fail("surface winding is not an unambiguous interior or exterior"));}
  let sign=if inside{-1.}else{1.};let mut ordinary=unique&&((best.distance_m>p.minimum_distance_m&&(inside||outside))||matches!(feature,DistanceFeature::Face(_)));
  match feature{
   DistanceFeature::Face(_)=>ordinary&=best.barycentric.iter().all(|v|*v>p.minimum_parameter),
   DistanceFeature::Edge([a,b])=>{
    ordinary&=best.barycentric.iter().filter(|v|**v>0.).all(|v|*v>p.minimum_parameter);
    for &index in &self.incident[a]{let f=self.mesh.f[index];if !f.contains(&b){continue;}
     let q=f.map(|i|self.mesh.v[i]);let u=sub(q[1],q[0]);let v=sub(q[2],q[0]);let w=sub(point,q[0]);let normal=cross(u,v);let den=dot(normal,normal);let beta=dot(cross(w,v),normal)/den;let gamma=dot(cross(u,w),normal)/den;let bary=[1.-beta-gamma,beta,gamma];let other=(0..3).find(|i|f[*i]!=a&&f[*i]!=b).ok_or_else(||fail("edge facet support"))?;ordinary&=bary[other]< -p.minimum_parameter;
    }
   },
   DistanceFeature::Vertex(a)=>for &index in &self.incident[a]{for b in self.mesh.f[index]{if b==a{continue;}let edge=sub(self.mesh.v[b],self.mesh.v[a]);ordinary&=dot(sub(point,self.mesh.v[a]),edge)/dot(edge,edge)< -p.minimum_parameter;}},
  }
  let mut signed_distance_m=sign*best.distance_m;
  let face_normal=if let DistanceFeature::Face(index)=feature{let f=self.mesh.f[index];let u=sub(self.mesh.v[f[1]],self.mesh.v[f[0]]);let v=sub(self.mesh.v[f[2]],self.mesh.v[f[0]]);let scale=u.iter().chain(&v).map(|x|x.abs()).fold(0.,f64::max);let n=cross(u.map(|x|x/scale),v.map(|x|x/scale));let length=dot(n,n).sqrt();let normal=n.map(|x|self.outward[index]*x/length);signed_distance_m=dot(sub(point,self.mesh.v[f[0]]),normal);Some(normal)}else{None};
  let derivative=if ordinary{let gradient=face_normal.unwrap_or_else(||sub(point,closest_point_m).map(|v|sign*v/best.distance_m));Some(DistanceDerivative{point_gradient:gradient,vertex_gradient:triangle.iter().zip(best.barycentric).filter_map(|(&node,w)|(w!=0.).then_some((node,gradient.map(|g|-w*g)))).collect()})}else{None};
  let input_winding=if let Some(sign)=self.input_winding_sign{sign*winding}else{winding_number(&self.mesh,&[point])[0]};if !input_winding.is_finite(){return Err(fail("input surface winding overflow"));}
  Ok(SurfaceDistance{signed_distance_m,winding_number:input_winding,closest_point_m,feature,coincident_facets,derivative})
 }
}
#[derive(Clone,Debug)]
pub struct DistanceDirectional{pub value:SurfaceDistance,pub distance_direction_m:Option<f64>,pub derivative_direction:Option<DistanceDerivative>}
fn dsub(a:[Dual<1>;3],b:[Dual<1>;3])->[Dual<1>;3]{std::array::from_fn(|i|a[i]-b[i])}
fn ddot(a:[Dual<1>;3],b:[Dual<1>;3])->Dual<1>{a.into_iter().zip(b).fold(Dual::constant(0.),|s,(x,y)|s+x*y)}
fn dcross(a:[Dual<1>;3],b:[Dual<1>;3])->[Dual<1>;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
impl ClosedSurfaceDistance{
 pub fn query_directional(&self,point:[f64;3],point_direction:[f64;3],vertex_direction:&[[f64;3]])->CaeResult<DistanceDirectional>{
  if vertex_direction.len()!=self.mesh.v.len()||point_direction.iter().chain(vertex_direction.iter().flatten()).any(|x|!x.is_finite()){return Err(fail("surface distance direction layout"));}
  let value=self.query(point)?;let Some(derivative)=&value.derivative else{return Ok(DistanceDirectional{value,distance_direction_m:None,derivative_direction:None});};
  let p=std::array::from_fn(|a|Dual::<1>{re:point[a],eps:[point_direction[a]]});
  let node=|i:usize|-> [Dual<1>;3] {std::array::from_fn(|a|Dual{re:self.mesh.v[i][a],eps:[vertex_direction[i][a]]})};
  let (q,weights):( [Dual<1>;3],Vec<(usize,Dual<1>)>)=match value.feature{
   DistanceFeature::Vertex(i)=>(node(i),vec![(i,Dual::constant(1.))]),
   DistanceFeature::Edge([a,b])=>{let x=node(a);let edge=dsub(node(b),x);let t=ddot(dsub(p,x),edge)/ddot(edge,edge);(std::array::from_fn(|i|x[i]+t*edge[i]),vec![(a,Dual::constant(1.)-t),(b,t)])},
   DistanceFeature::Face(index)=>{let f=self.mesh.f[index];let tri=f.map(node);let u=dsub(tri[1],tri[0]);let v=dsub(tri[2],tri[0]);let w=dsub(p,tri[0]);let scale=u.iter().chain(&v).map(|x|x.re.abs()).fold(0.,f64::max);let us=u.map(|x|x/Dual::constant(scale));let vs=v.map(|x|x/Dual::constant(scale));let ws=w.map(|x|x/Dual::constant(scale));let normal=dcross(us,vs);let den=ddot(normal,normal);let beta=ddot(dcross(ws,vs),normal)/den;let gamma=ddot(dcross(us,ws),normal)/den;let bary=[Dual::constant(1.)-beta-gamma,beta,gamma];(std::array::from_fn(|a|(0..3).fold(Dual::constant(0.),|s,i|s+bary[i]*tri[i][a])),f.into_iter().zip(bary).collect())},
  };
  let gradient=if let DistanceFeature::Face(index)=value.feature{let f=self.mesh.f[index];let tri=f.map(node);let u=dsub(tri[1],tri[0]);let v=dsub(tri[2],tri[0]);let scale=u.iter().chain(&v).map(|x|x.re.abs()).fold(0.,f64::max);let n=dcross(u.map(|x|x/Dual::constant(scale)),v.map(|x|x/Dual::constant(scale)));let length=ddot(n,n).sqrt();n.map(|x|Dual::constant(self.outward[index])*x/length)}else{let offset=dsub(p,q);let distance=ddot(offset,offset).sqrt();let sign=Dual::constant(if value.signed_distance_m<0.{-1.}else{1.});offset.map(|v|sign*v/distance)};
  let direction=DistanceDerivative{point_gradient:gradient.map(|v|v.eps[0]),vertex_gradient:weights.into_iter().map(|(i,w)|(i,gradient.map(|v|(-w*v).eps[0]))).collect()};
  let distance_direction_m=dot(derivative.point_gradient,point_direction)+derivative.vertex_gradient.iter().map(|(i,g)|dot(*g,vertex_direction[*i])).sum::<f64>();
  if !distance_direction_m.is_finite()||direction.point_gradient.iter().chain(direction.vertex_gradient.iter().flat_map(|(_,g)|g)).any(|v|!v.is_finite()){return Err(fail("surface distance directional derivative overflow"));}
  Ok(DistanceDirectional{value,distance_direction_m:Some(distance_direction_m),derivative_direction:Some(direction)})
 }
}

impl ClosedSurfaceDistance{pub(super) fn mesh(&self)->&TriMesh{&self.mesh} pub(super) fn outward(&self,facet:usize)->f64{self.outward[facet]}}
