// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError,CaeResult};
use super::{boundary_geometry,exposed_surface::ExposedSurface};
use crate::moving_contact::native_compliant_law::SurfaceContactQuadrature;
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
fn sub(a:[f64;3],b:[f64;3])->[f64;3]{std::array::from_fn(|i|a[i]-b[i])}
fn dot(a:[f64;3],b:[f64;3])->f64{a[0]*b[0]+a[1]*b[1]+a[2]*b[2]}
fn cross(a:[f64;3],b:[f64;3])->[f64;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
#[derive(Clone,Debug)]
pub struct ClosestTriangle {pub facet:usize,pub barycentric:[f64;3],pub distance_m:f64,pub ordinary_face_derivative:bool,pub coincident_candidates:usize}
pub fn closest_triangle(point:[f64;3],positions:&[[f64;3]],facets:&[[usize;3]],minimum_area_ratio:f64,minimum_parameter:f64,tie_distance_m:f64)->CaeResult<ClosestTriangle>{
 if point.iter().chain(positions.iter().flatten()).any(|x|!x.is_finite())||facets.is_empty()||!minimum_area_ratio.is_finite()||minimum_area_ratio<=0.||minimum_area_ratio>=1.||!minimum_parameter.is_finite()||minimum_parameter<=0.||minimum_parameter>=1./3.||!tie_distance_m.is_finite()||tie_distance_m<0. {return Err(fail("closest triangle inputs"));}
 let mut best:Option<ClosestTriangle>=None;
 for (index,facet) in facets.iter().enumerate(){
  if facet.iter().any(|v|*v>=positions.len()){return Err(fail("closest triangle vertex index"));}
  let triangle=facet.map(|i|positions[i]);let mut lower=0.;
  for a in 0..3 {let lo=triangle.iter().map(|p|p[a]).fold(f64::INFINITY,f64::min);let hi=triangle.iter().map(|p|p[a]).fold(f64::NEG_INFINITY,f64::max);let d=if point[a]<lo{lo-point[a]}else if point[a]>hi{point[a]-hi}else{0.};lower+=d*d;}
  if let Some(old)=&best {if lower.sqrt()>old.distance_m+tie_distance_m {continue;}}
  let u=sub(triangle[1],triangle[0]);let v=sub(triangle[2],triangle[0]);let w=sub(point,triangle[0]);let scale=u.iter().chain(&v).map(|x|x.abs()).fold(0.,f64::max);
  if scale<=0. {return Err(fail("closest triangle degenerate edge scale"));}
  let us=u.map(|x|x/scale);let vs=v.map(|x|x/scale);let ws=w.map(|x|x/scale);let normal=cross(us,vs);let den=dot(normal,normal);
  if !den.is_finite()||den<=minimum_area_ratio*minimum_area_ratio{return Err(fail("closest triangle degenerate area"));}
  let beta=dot(cross(ws,vs),normal)/den;let gamma=dot(cross(us,ws),normal)/den;let interior=[1.-beta-gamma,beta,gamma];
  let mut candidates=vec![];
  if interior.iter().all(|v|*v>=0.){let q:[f64;3]=std::array::from_fn(|a|(0..3).map(|i|interior[i]*triangle[i][a]).sum());candidates.push((dot(sub(point,q),sub(point,q)).sqrt(),interior));}
  for (i,j) in [(0,1),(1,2),(2,0)] {let edge=sub(triangle[j],triangle[i]);let length=dot(edge,edge);if length<=0.||!length.is_finite(){return Err(fail("closest triangle collapsed edge"));}let t=(dot(sub(point,triangle[i]),edge)/length).clamp(0.,1.);let q=std::array::from_fn(|a|triangle[i][a]+t*edge[a]);let mut bary=[0.;3];bary[i]=1.-t;bary[j]=t;candidates.push((dot(sub(point,q),sub(point,q)).sqrt(),bary));}
  let (distance,bary)=candidates.into_iter().min_by(|a,b|a.0.total_cmp(&b.0)).ok_or_else(||fail("closest triangle candidates"))?;
  let margin=bary.into_iter().fold(1.,f64::min);let ordinary=margin>minimum_parameter;
  match &mut best {
   None=>best=Some(ClosestTriangle{facet:index,barycentric:bary,distance_m:distance,ordinary_face_derivative:ordinary,coincident_candidates:1}),
   Some(old)=>{
    if distance+tie_distance_m<old.distance_m {*old=ClosestTriangle{facet:index,barycentric:bary,distance_m:distance,ordinary_face_derivative:ordinary,coincident_candidates:1};}
    else if (distance-old.distance_m).abs()<=tie_distance_m {let count=old.coincident_candidates+1;let old_margin=old.barycentric.into_iter().fold(1.,f64::min);if margin>old_margin {*old=ClosestTriangle{facet:index,barycentric:bary,distance_m:distance,ordinary_face_derivative:ordinary,coincident_candidates:count};}else{old.coincident_candidates=count;}}
   }
  }
 }
 best.ok_or_else(||fail("closest triangle owner absent"))
}
pub struct QuadratureOwnership {pub source_facet:usize,pub closest:ClosestTriangle,pub quadrature:Option<SurfaceContactQuadrature>}
pub fn select_quadrature(source_body:usize,source_facet:usize,barycentric:[f64;3],weight:f64,surfaces:[&ExposedSurface;2],positions:[&[[f64;3]];2],minimum_area_ratio:f64,minimum_parameter:f64,tie_distance_m:f64)->CaeResult<QuadratureOwnership>{
 if source_body>1||!weight.is_finite()||weight<=0.||barycentric.iter().any(|x|!x.is_finite()||*x<0.)||(barycentric.iter().sum::<f64>()-1.).abs()>32.*f64::EPSILON{return Err(fail("contact quadrature selection"));}
 for b in 0..2{if positions[b].len()!=surfaces[b].features().len(){return Err(fail("contact surface position count"));}}
 let source=surfaces[source_body].facets().get(source_facet).ok_or_else(||fail("source contact facet index"))?;
 let point=std::array::from_fn(|a|(0..3).map(|i|barycentric[i]*positions[source_body][source[i]][a]).sum());let target_body=1-source_body;
 let closest=closest_triangle(point,positions[target_body],surfaces[target_body].facets(),minimum_area_ratio,minimum_parameter,tie_distance_m)?;
 let quadrature=if closest.ordinary_face_derivative {
  let target=surfaces[target_body].facets()[closest.facet];
  let traces=std::array::from_fn(|i|if i==0 {source.iter().enumerate().map(|(k,v)|(barycentric[k],surfaces[source_body].features()[*v].clone())).collect()}else{vec![(1.,surfaces[target_body].features()[target[i-1]].clone())]});
  let area_traces=source.map(|v|vec![(1.,surfaces[source_body].features()[v].clone())]);
  let triangle=target.map(|v|positions[target_body][v]);let geometry=boundary_geometry::vertex_face([point,triangle[0],triangle[1],triangle[2]],[[0.;3];4],minimum_area_ratio)?;geometry.require_ordinary_feature_derivative(minimum_parameter)?;
  Some(SurfaceContactQuadrature{traces,bodies:[source_body,target_body,target_body,target_body],area_traces,area_body:source_body,weight})
 }else{None};
 Ok(QuadratureOwnership{source_facet,closest,quadrature})
}

#[derive(Clone,Debug)]
pub struct OwnedProjectionSegment {pub interval:[f64;2],pub target_facet:usize}
#[derive(Clone,Debug)]
pub struct ProjectionOwnerTransfer {pub interval:[f64;2],pub from_facet:usize,pub to_facet:usize}
#[derive(Clone,Debug)]
pub struct ProjectionOwnershipTrace {pub segments:Vec<OwnedProjectionSegment>,pub transfers:Vec<ProjectionOwnerTransfer>,pub unresolved_interval:Option<[f64;2]>,pub closest_feature_dominance_qualified:bool}
pub fn trace_projection_owners(source_body:usize,source_facet:usize,barycentric:[f64;3],surfaces:[&ExposedSurface;2],old:[&[[f64;3]];2],new:[&[[f64;3]];2],minimum_area_ratio:f64,minimum_parameter:f64,tie_distance_m:f64,time_resolution:f64,maximum_transfers:usize)->CaeResult<ProjectionOwnershipTrace>{
 if source_body>1||!time_resolution.is_finite()||time_resolution<=0.||time_resolution>=1.||maximum_transfers==0{return Err(fail("projection ownership trace policy"));}
 for b in 0..2 {if old[b].len()!=new[b].len()||old[b].len()!=surfaces[b].features().len(){return Err(fail("projection ownership trace position count"));}}
 let source=*surfaces[source_body].facets().get(source_facet).ok_or_else(||fail("projection source facet"))?;let target=1-source_body;
 let positions=|fraction:f64|->[Vec<[f64;3]>;2]{std::array::from_fn(|b|old[b].iter().zip(new[b]).map(|(a,z)|std::array::from_fn(|axis|a[axis]+fraction*(z[axis]-a[axis]))).collect())};
 let point=|p:&[Vec<[f64;3]>;2]|-> [f64;3] {std::array::from_fn(|a|(0..3).map(|i|barycentric[i]*p[source_body][source[i]][a]).sum())};
 let mut result=ProjectionOwnershipTrace{segments:vec![],transfers:vec![],unresolved_interval:None,closest_feature_dominance_qualified:false};let mut fraction=0.;
 let initial=positions(0.);let first=select_quadrature(source_body,source_facet,barycentric,1.,surfaces,[&initial[0],&initial[1]],minimum_area_ratio,minimum_parameter,tie_distance_m)?;
 if !first.closest.ordinary_face_derivative{result.unresolved_interval=Some([0.,1.]);return Ok(result);}let mut owner=first.closest.facet;
 loop {
  let start=positions(fraction);let end=positions(1.);let facet=surfaces[target].facets()[owner];let a=[point(&start),start[target][facet[0]],start[target][facet[1]],start[target][facet[2]]];let z=[point(&end),end[target][facet[0]],end[target][facet[1]],end[target][facet[2]]];
  let path=super::boundary_path::check_linear_feature_path(a,z,super::boundary_path::PathPolicy{minimum_barycentric:0.,minimum_area_ratio,time_resolution,..Default::default()}).map_err(fail)?;
  if path.status==super::boundary_path::PathStatus::Admitted{result.segments.push(OwnedProjectionSegment{interval:[fraction,1.],target_facet:owner});return Ok(result);}
  if path.domain!=Some(super::boundary_path::PathDomain::ClosedFaceProjection){result.unresolved_interval=Some([fraction,1.]);return Ok(result);}
  let blocked=path.blocked_interval.ok_or_else(||fail("projection transition interval absent"))?;let lo=fraction+(1.-fraction)*blocked[0];let hi=fraction+(1.-fraction)*blocked[1];
  if lo>fraction{result.segments.push(OwnedProjectionSegment{interval:[fraction,lo],target_facet:owner});}
  if result.transfers.len()>=maximum_transfers{result.unresolved_interval=Some([lo,1.]);return Ok(result);}
  let mut distance=time_resolution.max(hi-lo);let mut selected=None;
  loop {
   let probe=(hi+distance).min(1.);if probe<=fraction{result.unresolved_interval=Some([lo,1.]);return Ok(result);}
   let p=positions(probe);let closest=closest_triangle(point(&p),&p[target],surfaces[target].facets(),minimum_area_ratio,minimum_parameter,tie_distance_m)?;
   if closest.ordinary_face_derivative&&closest.facet!=owner{selected=Some((probe,closest.facet));break;}
   if probe==1.{break;}distance*=2.;
  }
  let Some((next,next_owner))=selected else {result.unresolved_interval=Some([lo,1.]);return Ok(result);};
  result.transfers.push(ProjectionOwnerTransfer{interval:[lo,next],from_facet:owner,to_facet:next_owner});fraction=next;owner=next_owner;
  if fraction==1.{return Ok(result);}
 }
}
