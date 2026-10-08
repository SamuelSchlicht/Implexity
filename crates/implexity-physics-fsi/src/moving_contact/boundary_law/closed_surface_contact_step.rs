// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::{Dual,Scalar};
use implexity_core::{CaeError,CaeResult};
use implexity_geometry::domain_sdf::TriMesh;
use implexity_physics_solid::contact_compliance::UnilateralContactCompliance;
use std::collections::BTreeSet;
use super::closed_surface_distance::{ClosedSurfaceDistance,DistancePolicy,DistanceFeature,DistanceDerivative};
fn fail(s:impl std::fmt::Display)->CaeError{CaeError::contract(s.to_string())}
type D=Dual<60>;
fn sub<S:Scalar>(a:[S;3],b:[S;3])->[S;3]{std::array::from_fn(|i|a[i]-b[i])}
fn dot<S:Scalar>(a:[S;3],b:[S;3])->S{a.into_iter().zip(b).fold(S::from_f64(0.),|s,(x,y)|s+x*y)}
fn cross<S:Scalar>(a:[S;3],b:[S;3])->[S;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
pub struct ClosedSurfaceContactStep{old:ClosedSurfaceDistance,mid:ClosedSurfaceDistance,new:ClosedSurfaceDistance,law:UnilateralContactCompliance,policy:DistancePolicy}
#[derive(Clone,Debug)]
pub struct ClosedSurfaceStepForce{pub target_nodes:Vec<usize>,pub force_n:Vec<f64>,pub current_jacobian_n_per_m:Vec<Vec<f64>>,pub previous_jacobian_n_per_m:Vec<Vec<f64>>,pub area_derivative_n_per_m2:Vec<f64>,pub previous_energy_j:f64,pub current_energy_j:f64,pub work_defect_j:f64,pub net_force_n:[f64;3],pub midpoint_torque_nm:[f64;3],pub previous_feature:DistanceFeature,pub current_feature:DistanceFeature}
fn gradient(g:&DistanceDerivative,nodes:&[usize])->Vec<f64>{let mut result=g.point_gradient.to_vec();for node in nodes{result.extend(g.vertex_gradient.iter().find(|(i,_)|i==node).map(|(_,g)|*g).unwrap_or([0.;3]));}result}
impl ClosedSurfaceContactStep{
 pub fn new(old:TriMesh,new:TriMesh,policy:DistancePolicy,law:UnilateralContactCompliance)->CaeResult<Self>{
  if old.f!=new.f||old.v.len()!=new.v.len(){return Err(fail("contact surface topology changes within a step"));}law.evaluate(0.).map_err(fail)?;
  let mid=TriMesh{v:old.v.iter().zip(&new.v).map(|(a,b)|std::array::from_fn(|i|0.5*(a[i]+b[i]))).collect(),f:old.f.clone()};
  Ok(Self{old:ClosedSurfaceDistance::new(old,policy)?,mid:ClosedSurfaceDistance::new(mid,policy)?,new:ClosedSurfaceDistance::new(new,policy)?,law,policy})
 }
 pub fn evaluate(&self,old_point:[f64;3],new_point:[f64;3],area:f64)->CaeResult<ClosedSurfaceStepForce>{
  if !area.is_finite()||area<=0.{return Err(fail("contact reference area must be finite and positive"));}
  let mid_point=std::array::from_fn(|i|0.5*(old_point[i]+new_point[i]));let a=self.old.query(old_point)?;let b=self.new.query(new_point)?;let m=self.mid.query(mid_point)?;
  let e0=self.law.evaluate(a.signed_distance_m).map_err(fail)?;let e1=self.law.evaluate(b.signed_distance_m).map_err(fail)?;let sec=self.law.secant(a.signed_distance_m,b.signed_distance_m).map_err(fail)?;
  let mut union=BTreeSet::new();for (surface,value) in [(&self.old,&a),(&self.mid,&m),(&self.new,&b)]{match value.feature{DistanceFeature::Face(i)=>union.extend(surface.mesh().f[i]),DistanceFeature::Edge(v)=>union.extend(v),DistanceFeature::Vertex(v)=>{union.insert(v);}}}
  let nodes:Vec<_>=union.into_iter().collect();let count=nodes.len()+1;let size=3*count;if count>10{return Err(fail("contact feature union exceeds coordinate capacity"));}
  let mut old=vec![old_point];old.extend(nodes.iter().map(|i|self.old.mesh().v[*i]));let mut new=vec![new_point];new.extend(nodes.iter().map(|i|self.new.mesh().v[*i]));let mid:Vec<[f64;3]>=old.iter().zip(&new).map(|(a,b)|std::array::from_fn(|i|0.5*(a[i]+b[i]))).collect();
  let mut force=vec![0.;size];let mut previous=vec![vec![0.;size];size];let mut current=previous.clone();let mut unit=vec![0.;size];
  if sec.pressure_pa!=0.||sec.previous_gap_derivative_pa_per_m!=0.||sec.current_gap_derivative_pa_per_m!=0.{
   let ga=gradient(a.derivative.as_ref().ok_or_else(||fail("previous contact distance derivative is ambiguous"))?,&nodes);let gb=gradient(b.derivative.as_ref().ok_or_else(||fail("current contact distance derivative is ambiguous"))?,&nodes);
   if let (DistanceFeature::Face(i),DistanceFeature::Face(j),DistanceFeature::Face(k))=(&a.feature,&b.feature,&m.feature){if i==j&&j==k{
    if self.old.outward(*i)!=self.mid.outward(*i)||self.old.outward(*i)!=self.new.outward(*i){return Err(fail("contact component orientation changes within a step"));}
    let mut target=self.old.mesh().f[*i];if self.old.outward(*i)<0.{target.swap(1,2);}let index=[0,1+nodes.binary_search(&target[0]).map_err(|_|fail("contact face node"))?,1+nodes.binary_search(&target[1]).map_err(|_|fail("contact face node"))?,1+nodes.binary_search(&target[2]).map_err(|_|fail("contact face node"))?];
    let z=super::compliant_energy_momentum_contact::vertex_face(index.map(|v|old[v]),index.map(|v|new[v]),area,self.law,self.policy.minimum_parameter).map_err(fail)?;
    for i in 0..12{let row=3*index[i/3]+i%3;force[row]=z.force_n[i];unit[row]=z.area_derivative_n_per_m2[i];for j in 0..12{let col=3*index[j/3]+j%3;previous[row][col]=z.previous_jacobian_n_per_m[i][j];current[row][col]=z.current_jacobian_n_per_m[i][j];}}
    return self.finish(nodes,old,new,force,current,previous,unit,area*e0.energy_j_per_m2,area*e1.energy_j_per_m2,a.feature,b.feature);
   }}
   let x0:Vec<[D;3]>=old.iter().enumerate().map(|(i,p)|std::array::from_fn(|j|D::variable(p[j],3*i+j))).collect();let x1:Vec<[D;3]>=new.iter().enumerate().map(|(i,p)|std::array::from_fn(|j|D::variable(p[j],size+3*i+j))).collect();
   let g0=D{re:a.signed_distance_m,eps:std::array::from_fn(|i|if i<size{ga[i]}else{0.})};let g1=D{re:b.signed_distance_m,eps:std::array::from_fn(|i|if i>=size&&i<2*size{gb[i-size]}else{0.})};let pressure=D{re:sec.pressure_pa,eps:std::array::from_fn(|i|sec.previous_gap_derivative_pa_per_m*g0.eps[i]+sec.current_gap_derivative_pa_per_m*g1.eps[i])};let c=D::constant;
   let pm:Vec<[D;3]>=x0.iter().zip(&x1).map(|(a,b)|std::array::from_fn(|j|(a[j]+b[j])*c(0.5))).collect();let d:Vec<[D;3]>=x1.iter().zip(&x0).map(|(a,b)|sub(*a,*b)).collect();let mut u=vec![c(0.);size];
   match (&a.feature,&b.feature,&m.feature){
    (DistanceFeature::Vertex(i),DistanceFeature::Vertex(j),DistanceFeature::Vertex(k)) if i==j&&j==k&&g0.re*g1.re>0.=>{let node=1+nodes.binary_search(i).map_err(|_|fail("contact vertex node"))?;let r0=sub(x0[0],x0[node]);let r1=sub(x1[0],x1[node]);for axis in 0..3{u[axis]=(r0[axis]+r1[axis])/(g0+g1);u[3*node+axis]= -u[axis];}},
    (DistanceFeature::Edge(i),DistanceFeature::Edge(j),DistanceFeature::Edge(k)) if i==j&&j==k&&g0.re*g1.re>0.=>{let first=1+nodes.binary_search(&i[0]).map_err(|_|fail("contact edge node"))?;let last=1+nodes.binary_search(&i[1]).map_err(|_|fail("contact edge node"))?;let u0=sub(x0[last],x0[first]);let u1=sub(x1[last],x1[first]);let w0=sub(x0[0],x0[first]);let w1=sub(x1[0],x1[first]);let uu=(dot(u0,u0)+dot(u1,u1))*c(0.5);let ww=(dot(w0,w0)+dot(w1,w1))*c(0.5);let uw=(dot(u0,w0)+dot(u1,w1))*c(0.5);let h=(g0*g0+g1*g1)*c(0.5);let gu=(ww-h)/uu;let gc= -c(2.)*uw/uu;let um=sub(pm[last],pm[first]);let wm=sub(pm[0],pm[first]);for axis in 0..3{u[axis]=(c(2.)*wm[axis]+gc*um[axis])/(g0+g1);u[3*last+axis]=(c(2.)*gu*um[axis]+gc*wm[axis])/(g0+g1);u[3*first+axis]= -u[axis]-u[3*last+axis];}},
    _=>{
     let gm=gradient(m.derivative.as_ref().ok_or_else(||fail("midpoint contact distance derivative is ambiguous"))?,&nodes);let mut hessian=vec![vec![0.;size];size];let mut direction=vec![[0.;3];self.mid.mesh().v.len()];
     for j in 0..size{let mut point_direction=[0.;3];if j<3{point_direction[j]=1.;}else{direction[nodes[j/3-1]][j%3]=1.;}let z=self.mid.query_directional(mid_point,point_direction,&direction)?;let col=gradient(z.derivative_direction.as_ref().ok_or_else(||fail("midpoint contact Hessian is ambiguous"))?,&nodes);for i in 0..size{hessian[i][j]=col[i];}if j>=3{direction[nodes[j/3-1]][j%3]=0.;}}
     let gradient:Vec<D>=(0..size).map(|i|D{re:gm[i],eps:std::array::from_fn(|j|if j<2*size{0.5*hessian[i][j%size]}else{0.})}).collect();
     let center:[D;3]=std::array::from_fn(|a|pm.iter().fold(c(0.),|s,p|s+p[a])/c(count as f64));let mean:[D;3]=std::array::from_fn(|a|d.iter().fold(c(0.),|s,p|s+p[a])/c(count as f64));let r:Vec<_>=pm.iter().map(|p|sub(*p,center)).collect();let v:Vec<_>=d.iter().map(|p|sub(*p,mean)).collect();let inertia:[[D;3];3]=std::array::from_fn(|i|std::array::from_fn(|j|r.iter().fold(c(0.),|s,p|s+(if i==j{dot(*p,*p)}else{c(0.)})-p[i]*p[j])));let moment:[D;3]=std::array::from_fn(|i|r.iter().zip(&v).fold(c(0.),|s,(p,w)|s+cross(*p,*w)[i]));let det=dot(inertia[0],cross(inertia[1],inertia[2]));if !det.re.is_finite()||det.re<=0.{return Err(fail("contact feature-transfer rigid projection is degenerate"));}let cols=[cross(inertia[1],inertia[2]),cross(inertia[2],inertia[0]),cross(inertia[0],inertia[1])];let omega:[D;3]=std::array::from_fn(|i|(0..3).fold(c(0.),|s,j|s+cols[j][i]*moment[j])/det);let projected:Vec<D>=(0..size).map(|i|sub(v[i/3],cross(omega,r[i/3]))[i%3]).collect();let denominator=projected.iter().fold(c(0.),|s,x|s+*x * *x);if !denominator.re.is_finite()||denominator.re<=0.{return Err(fail("contact feature-transfer displacement is degenerate"));}let remainder=g1-g0-(0..size).fold(c(0.),|s,i|s+gradient[i]*d[i/3][i%3]);for i in 0..size{u[i]=gradient[i]+remainder*projected[i]/denominator;}
    }
   }
   for i in 0..size{let f=pressure*u[i];if !f.re.is_finite()||f.eps.iter().any(|v|!v.is_finite()){return Err(fail("closed surface contact force derivative overflow"));}force[i]=area*f.re;unit[i]=f.re;for j in 0..size{previous[i][j]=area*f.eps[j];current[i][j]=area*f.eps[size+j];}}
  }
  self.finish(nodes,old,new,force,current,previous,unit,area*e0.energy_j_per_m2,area*e1.energy_j_per_m2,a.feature,b.feature)
 }
 fn finish(&self,nodes:Vec<usize>,old:Vec<[f64;3]>,new:Vec<[f64;3]>,force:Vec<f64>,current:Vec<Vec<f64>>,previous:Vec<Vec<f64>>,unit:Vec<f64>,e0:f64,e1:f64,previous_feature:DistanceFeature,current_feature:DistanceFeature)->CaeResult<ClosedSurfaceStepForce>{
  let work:f64=old.iter().zip(&new).enumerate().map(|(i,(a,b))|(0..3).map(|j|force[3*i+j]*(b[j]-a[j])).sum::<f64>()).sum();let net=std::array::from_fn(|a|(0..old.len()).map(|i|force[3*i+a]).sum());let mut torque=[0.;3];for (i,(a,b)) in old.iter().zip(&new).enumerate(){let mid=std::array::from_fn(|j|0.5*(a[j]+b[j]));let f=std::array::from_fn(|j|force[3*i+j]);let t=cross(mid,f);for j in 0..3{torque[j]+=t[j];}}
  if force.iter().chain(current.iter().flatten()).chain(previous.iter().flatten()).chain(&unit).chain(&net).chain(&torque).chain([work,e0,e1].iter()).any(|v|!v.is_finite()){return Err(fail("closed surface contact output overflow"));}
  Ok(ClosedSurfaceStepForce{target_nodes:nodes,force_n:force,current_jacobian_n_per_m:current,previous_jacobian_n_per_m:previous,area_derivative_n_per_m2:unit,previous_energy_j:e0,current_energy_j:e1,work_defect_j:work+e1-e0,net_force_n:net,midpoint_torque_nm:torque,previous_feature,current_feature})
 }
}
