// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::{Dual, Scalar};
use implexity_physics_solid::contact_compliance::UnilateralContactCompliance;
use super::{boundary_geometry, contact_step_force::ContactStepForce};
use super::contact_invariants::{sub,dot,invariants,invariant_gradient,coordinate_gradient};
fn cross<S:Scalar>(a:[S;3],b:[S;3])->[S;3]{[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]]}
fn sum<S:Scalar>(v:impl Iterator<Item=S>)->S{v.fold(S::from_f64(0.),|a,b|a+b)}
pub fn vertex_face(old:[[f64;3];4],new:[[f64;3];4],area:f64,law:UnilateralContactCompliance,margin:f64)->Result<ContactStepForce,String>{
 if !area.is_finite()||area<=0. {return Err("finite positive contact area required".into());}
 let a=boundary_geometry::vertex_face(old,[[0.;3];4],1e-8).map_err(|e|e.to_string())?;
 let b=boundary_geometry::vertex_face(new,[[0.;3];4],1e-8).map_err(|e|e.to_string())?;
 let e0=law.evaluate(a.signed_gap_m).map_err(str::to_string)?;let e1=law.evaluate(b.signed_gap_m).map_err(str::to_string)?;
 let sec=law.secant(a.signed_gap_m,b.signed_gap_m).map_err(str::to_string)?;
 let x0=std::array::from_fn(|i|std::array::from_fn(|j|Dual::<24>::variable(old[i][j],3*i+j)));
 let x1=std::array::from_fn(|i|std::array::from_fn(|j|Dual::<24>::variable(new[i][j],12+3*i+j)));
 let mid=std::array::from_fn(|i|std::array::from_fn(|j|(x0[i][j]+x1[i][j])*Dual::constant(0.5)));
 let middle=std::array::from_fn(|i|std::array::from_fn(|j|0.5*(old[i][j]+new[i][j])));
 let geometry=boundary_geometry::vertex_face(middle,[[0.;3];4],1e-8).map_err(|e|e.to_string())?;
 if sec.pressure_pa>0. {for g in [&a,&b,&geometry]{g.require_ordinary_feature_derivative(margin).map_err(|e|e.to_string())?;}}
 let g0=Dual::<24>{re:a.signed_gap_m,eps:std::array::from_fn(|j|if j<12 {a.gradient[j/3][j%3]}else{0.})};
 let g1=Dual::<24>{re:b.signed_gap_m,eps:std::array::from_fn(|j|if j>=12 {b.gradient[(j-12)/3][j%3]}else{0.})};
 let pressure=Dual::<24>{re:sec.pressure_pa,eps:std::array::from_fn(|j|sec.previous_gap_derivative_pa_per_m*g0.eps[j]+sec.current_gap_derivative_pa_per_m*g1.eps[j])};
 let mut hessian=[[0.;12];12];
 for j in 0..12 {let mut direction=[[0.;3];4];direction[j/3][j%3]=1.;let z=boundary_geometry::vertex_face(middle,direction,1e-8).map_err(|e|e.to_string())?;
  for i in 0..12 {hessian[i][j]=z.gradient_direction[i/3][i%3];}}
 let gradient:[Dual<24>;12]=std::array::from_fn(|i|Dual{re:geometry.gradient[i/3][i%3],eps:std::array::from_fn(|j|0.5*hessian[i][j%12])});
 let c=Dual::<24>::constant;let d: [[Dual<24>;3];4]=std::array::from_fn(|i|sub(x1[i],x0[i]));
 let center:[Dual<24>;3]=std::array::from_fn(|j|sum(mid.iter().map(|p|p[j]))/c(4.));
 let mean:[Dual<24>;3]=std::array::from_fn(|j|sum(d.iter().map(|p|p[j]))/c(4.));
 let r=mid.map(|p|sub(p,center));let v=d.map(|p|sub(p,mean));
 let inertia:[[Dual<24>;3];3]=std::array::from_fn(|i|std::array::from_fn(|j|sum(r.iter().map(|p|(if i==j{dot(*p,*p)}else{c(0.)})-p[i]*p[j]))));
 let moment:[Dual<24>;3]=std::array::from_fn(|i|sum(r.iter().zip(v).map(|(p,w)|cross(*p,w)[i])));
 let det=dot(inertia[0],cross(inertia[1],inertia[2]));if !det.re.is_finite()||det.re<=0. {return Err("contact rigid projection is degenerate".into());}
 let cols=[cross(inertia[1],inertia[2]),cross(inertia[2],inertia[0]),cross(inertia[0],inertia[1])];
 let omega:[Dual<24>;3]=std::array::from_fn(|i|sum((0..3).map(|j|cols[j][i]*moment[j]))/det);
 let projected:[Dual<24>;12]=std::array::from_fn(|i|sub(v[i/3],cross(omega,r[i/3]))[i%3]);
 let denominator=sum(projected.iter().map(|x|*x * *x));let flat:[Dual<24>;12]=std::array::from_fn(|i|d[i/3][i%3]);
 let total=g0+g1;let change=g1-g0;let blend=total*total+change*change;
 let unit=if pressure.re==0.&&pressure.eps.iter().all(|x|*x==0.){[c(0.);12]}else{
  if blend.re<=0.||!blend.re.is_finite(){return Err("contact gap blend is degenerate".into());}
  let h=invariant_gradient(invariants(x0),invariants(x1),g0*g0,g1*g1);let invariant=coordinate_gradient(mid,h);
  let remainder=change-sum((0..12).map(|i|gradient[i]*flat[i]));
  std::array::from_fn(|i|{
   let correction=if denominator.re==0.{c(0.)}else{change*change*remainder*projected[i]/denominator};
   pressure*(total*invariant[i]+change*change*gradient[i]+correction)/blend
  })
 };
 let force=unit.map(|x|area*x.re);let previous=std::array::from_fn(|i|std::array::from_fn(|j|area*unit[i].eps[j]));let current=std::array::from_fn(|i|std::array::from_fn(|j|area*unit[i].eps[12+j]));
 let work:f64=(0..12).map(|i|force[i]*(new[i/3][i%3]-old[i/3][i%3])).sum();let net=std::array::from_fn(|a|(0..4).map(|i|force[3*i+a]).sum());let mut torque=[0.;3];
 for i in 0..4 {let q=middle[i];let f=[force[3*i],force[3*i+1],force[3*i+2]];let t=cross(q,f);for a in 0..3{torque[a]+=t[a];}}
 if unit.iter().any(|v|!v.re.is_finite()||v.eps.iter().any(|x|!x.is_finite())) {return Err("contact force derivative overflow".into());}
 Ok(ContactStepForce{force_n:force,current_jacobian_n_per_m:current,previous_jacobian_n_per_m:previous,area_derivative_n_per_m2:unit.map(|x|x.re),previous_energy_j:area*e0.energy_j_per_m2,current_energy_j:area*e1.energy_j_per_m2,work_defect_j:work+area*(e1.energy_j_per_m2-e0.energy_j_per_m2),net_force_n:net,midpoint_torque_nm:torque})
}
