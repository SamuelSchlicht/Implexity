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

fn finite(x:f64)->Result<f64,&'static str>{if x.is_finite(){Ok(x)}else{Err("exact affine arithmetic overflow")}}
fn two_sum(a:f64,b:f64)->Result<[f64;2],&'static str>{
 let s=finite(a+b)?;let bv=finite(s-a)?;let av=finite(s-bv)?;let br=finite(b-bv)?;let ar=finite(a-av)?;let e=finite(ar+br)?;Ok([e,s])
}
fn least_bit(x:f64)->i32{let bits=x.abs().to_bits();let exponent=((bits>>52)&2047)as i32;let mantissa=(bits&((1u64<<52)-1))|(1u64<<52);exponent-1023-52+mantissa.trailing_zeros()as i32}
fn two_product(a:f64,b:f64)->Result<[f64;2],&'static str>{
 if a==0.||b==0.{return Ok([0.,a*b]);}
 if !a.is_normal()||!b.is_normal()||least_bit(a)+least_bit(b)< -1074{return Err("exact affine weighted product underflow scope");}
 let p=finite(a*b)?;if !p.is_normal(){return Err("exact affine product subnormal or underflow");}
 let e=finite(a.mul_add(b,-p))?;if e!=0.&&!e.is_normal(){return Err("exact affine product residual subnormal");}Ok([e,p])
}
fn grow(expansion:&mut Vec<f64>,value:f64)->Result<(),&'static str>{
 let mut q=value;let mut next=Vec::with_capacity(expansion.len()+1);for &e in expansion.iter(){let [lo,hi]=two_sum(q,e)?;if lo!=0.{next.push(lo);}q=hi;}if q!=0.||next.is_empty(){next.push(q);}if next.len()>32{return Err("exact affine expansion capacity");}*expansion=next;Ok(())
}
pub(crate) fn exact_sign(corners:[f64;3],uv:[f64;2],coordinate:f64)->Result<i8,&'static str>{
 if corners.iter().chain(uv.iter()).chain(std::iter::once(&coordinate)).any(|x|!x.is_finite()||(*x!=0.&&!x.is_normal())){return Err("exact affine input domain");}
 let mut r=Vec::new();for x in two_sum(corners[0],-coordinate)?{grow(&mut r,x)?;}
 for (corner,weight) in [(corners[1],uv[0]),(corners[2],uv[1])]{for coeff in two_sum(corner,-corners[0])?{for term in two_product(coeff,weight)?{grow(&mut r,term)?;}}}
 Ok(r.iter().rev().find(|x|**x!=0.).map_or(0,|x|if *x>0.{1}else{-1}))
}

fn input_domain(values:impl Iterator<Item=f64>)->Result<(),&'static str>{
 if values.into_iter().any(|x|!x.is_finite()||(x!=0.&&!x.is_normal())){return Err("exact affine input domain");}Ok(())
}
fn expansion_sign(r:&[f64])->i8{r.iter().rev().find(|x|**x!=0.).map_or(0,|x|if *x>0.{1}else{-1})}
fn add_product(r:&mut Vec<f64>,a:f64,b:f64)->Result<(),&'static str>{for x in two_product(a,b)?{grow(r,x)?;}Ok(())}
pub(crate) fn exact_dot_sign(a:[f64;3],b:[f64;3])->Result<i8,&'static str>{
 input_domain(a.into_iter().chain(b))?;let mut r=Vec::new();for k in 0..3{add_product(&mut r,a[k],b[k])?;}Ok(expansion_sign(&r))
}
pub(crate) fn exact_bilinear_sum_zero(terms:&[(f64,f64)])->Result<bool,&'static str>{
 input_domain(terms.iter().flat_map(|&(a,b)|[a,b]))?;
 let mut r=Vec::new();for &(a,b) in terms{add_product(&mut r,a,b)?;}Ok(expansion_sign(&r)==0)
}
pub(crate) fn exact_plane_sign(normal:[f64;3],point:[f64;3],origin:[f64;3],offset:f64)->Result<i8,&'static str>{
 exact_framed_plane_sign(normal,point,[0.;3],origin,[0.;3],offset)
}
pub(crate) fn exact_framed_plane_sign(normal:[f64;3],point:[f64;3],point_anchor:[f64;3],origin:[f64;3],origin_anchor:[f64;3],offset:f64)->Result<i8,&'static str>{
 input_domain(normal.into_iter().chain(point).chain(point_anchor).chain(origin).chain(origin_anchor).chain(std::iter::once(offset)))?;
 let mut r=vec![offset];for k in 0..3{for value in [point[k],point_anchor[k],-origin_anchor[k],-origin[k]]{add_product(&mut r,normal[k],value)?;}}Ok(expansion_sign(&r))
}
pub(crate) fn exact_framed_affine_plane_sign(normal:[f64;3],corners:[[f64;3];3],uv:[f64;2],corner_anchor:[f64;3],origin:[f64;3],origin_anchor:[f64;3],offset:f64)->Result<i8,&'static str>{
 input_domain(normal.into_iter().chain(corners.into_iter().flatten()).chain(uv).chain(corner_anchor).chain(origin).chain(origin_anchor).chain(std::iter::once(offset)))?;
 let mut r=vec![offset];for k in 0..3{
  for value in [corners[0][k],corner_anchor[k],-origin_anchor[k],-origin[k]]{add_product(&mut r,normal[k],value)?;}
  for (corner,weight) in [(corners[1][k],uv[0]),(corners[2][k],uv[1])]{for coefficient in two_sum(corner,-corners[0][k])?{for term in two_product(coefficient,weight)?{add_product(&mut r,term,normal[k])?;}}}
 }Ok(expansion_sign(&r))
}

pub(crate) fn exact_framed_tetra_sign(points:[[f64;3];4],anchors:[[f64;3];4])->Result<i8,&'static str>{
 input_domain(points.into_iter().flatten().chain(anchors.into_iter().flatten()))?;
 let mut edges:[[Vec<f64>;3];3]=std::array::from_fn(|_|std::array::from_fn(|_|Vec::new()));
 for i in 0..3{for k in 0..3{
  for value in [points[i+1][k],anchors[i+1][k],-points[0][k],-anchors[0][k]]{grow(&mut edges[i][k],value)?;}
 }}
 let mut r=Vec::new();
 for (axes,sign) in [([0,1,2],1.),([1,2,0],1.),([2,0,1],1.),([0,2,1],-1.),([1,0,2],-1.),([2,1,0],-1.)]{
  for &a in &edges[0][axes[0]]{for &b in &edges[1][axes[1]]{for &c in &edges[2][axes[2]]{
   for term in two_product(a,b)?{for value in two_product(term,c)?{grow(&mut r,sign*value)?;}}
  }}}
 }
 Ok(expansion_sign(&r))
}

fn expansion_round(r:&[f64])->Result<f64,&'static str>{
 let mut n=r.len();if n==0{return Ok(0.);}
 n-=1;let mut hi=r[n];let mut lo=0.;
 while n>0{
  let x=hi;n-=1;let y=r[n];hi=finite(x+y)?;let yr=finite(hi-x)?;lo=finite(y-yr)?;
  if lo!=0.{break;}
 }
 if n>0 && ((lo<0.&&r[n-1]<0.)||(lo>0.&&r[n-1]>0.)){
  let y=finite(lo*2.)?;let x=finite(hi+y)?;let yr=finite(x-hi)?;
  if y==yr{hi=x;}
 }
 finite(hi)
}
pub(crate) fn exact_grid_displacement_tetra_sign(indices:[[usize;3];4],spacing:f64,displacements:[[f64;3];4])->Result<i8,&'static str>{
 if indices.iter().flatten().any(|i|(*i as u128)>(1u128<<53))||spacing<=0.{return Err("exact grid tetrahedron domain");}
 input_domain(std::iter::once(spacing).chain(displacements.into_iter().flatten()))?;
 let mut edges:[[Vec<f64>;3];3]=std::array::from_fn(|_|std::array::from_fn(|_|Vec::new()));
 for i in 0..3{for k in 0..3{
  let delta=if indices[i+1][k]>=indices[0][k]{(indices[i+1][k]-indices[0][k])as f64}else{-((indices[0][k]-indices[i+1][k])as f64)};
  for term in two_product(delta,spacing)?{grow(&mut edges[i][k],term)?;}
  for value in [displacements[i+1][k],-displacements[0][k]]{grow(&mut edges[i][k],value)?;}
 }}
 let mut r=Vec::new();
 for (axes,sign) in [([0,1,2],1.),([1,2,0],1.),([2,0,1],1.),([0,2,1],-1.),([1,0,2],-1.),([2,1,0],-1.)]{
  for &a in &edges[0][axes[0]]{for &b in &edges[1][axes[1]]{for &c in &edges[2][axes[2]]{
   for term in two_product(a,b)?{for value in two_product(term,c)?{grow(&mut r,sign*value)?;}}
  }}}
 }
 Ok(expansion_sign(&r))
}
pub(crate) fn exact_framed_grid_coordinate(origin:f64,index:usize,spacing:f64,anchor:f64)->Result<f64,&'static str>{
 if (index as u128)>(1u128<<53)||spacing<=0.{return Err("exact grid coordinate domain");}
 input_domain([origin,index as f64,spacing,anchor].into_iter())?;
 let mut r=Vec::new();grow(&mut r,origin)?;grow(&mut r,-anchor)?;
 for term in two_product(index as f64,spacing)?{grow(&mut r,term)?;}
 expansion_round(&r)
}
