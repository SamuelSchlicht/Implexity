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

use crate::gas_transient_kinetic::TransientGasScalar;
use crate::native_union_arrangement::PhysicalSegment2;
use crate::native_union_quadrature::UnionNode;
use implexity_ad::Scalar;
use implexity_core::{CaeError,CaeResult};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
fn same<S:TransientGasScalar>(a:S,b:S)->bool{let d=a-b;d.value()==0.&&d.geometry_first_order_directions().is_some_and(|v|v.iter().all(|x|x.is_finite()&&*x==0.))}
fn cross<S:Scalar>(a:[S;2],b:[S;2])->S{a[0]*b[1]-a[1]*b[0]}
fn sub<S:Scalar>(a:[S;2],b:[S;2])->[S;2]{[a[0]-b[0],a[1]-b[1]]}
fn key(kind:usize,parts:&[Vec<usize>])->Vec<usize>{let mut r=vec![kind];for p in parts{r.push(p.len());r.extend(p);}r}
pub(crate) fn interval_overlay<S:TransientGasScalar>(input:&[PhysicalSegment2<S>])->CaeResult<Vec<PhysicalSegment2<S>>>{
 let mut groups:Vec<Vec<usize>>=Vec::new();let mut parent=(0..input.len()).collect::<Vec<_>>();
 fn root(p:&[usize],mut i:usize)->usize{while p[i]!=i{i=p[i];}i}
 for i in 0..input.len(){let [a,b]=input[i].endpoints.clone().map(|p|p.point);let r=sub(b,a);if r.iter().all(|v|v.value()==0.){return Err(fail("maintained mask source segment has zero primal length"));}
  for j in i+1..input.len(){let [c,d]=input[j].endpoints.clone().map(|p|p.point);let t=sub(d,c);let det=cross(r,t);let line=cross(r,sub(c,a));
   if det.value()!=0.||line.value()!=0.{continue;}
   let axis=usize::from(r[1].value().abs()>r[0].value().abs());let lo=a[axis].value().min(b[axis].value()).max(c[axis].value().min(d[axis].value()));let hi=a[axis].value().max(b[axis].value()).min(c[axis].value().max(d[axis].value()));if hi<lo{continue;}
   if hi==lo {
    let first=[a,b].into_iter().find(|p|p[axis].value()==lo).ok_or_else(||fail("maintained mask touch has no first source endpoint"))?;
    let second=[c,d].into_iter().find(|p|p[axis].value()==lo).ok_or_else(||fail("maintained mask touch has no second source endpoint"))?;
    if (0..2).any(|k|!same(first[k],second[k])){return Err(fail("maintained mask touched endpoint has unequal full motion"));}
    continue;
   }
   if !same(det,S::zero())||!same(line,S::zero()){return Err(CaeError::contract(format!("maintained mask collinear source has nonmaintained cross-line motion i={} j={} owner_i={:?} owner_j={:?} overlap={:.17e} first={:?} second={:?} det={:.17e} det_jets={:?} line={:.17e} line_jets={:?}",i,j,input[i].owner,input[j].owner,hi-lo,[a,b].map(|p|p.map(|v|(v.value(),v.geometry_first_order_directions()))),[c,d].map(|p|p.map(|v|(v.value(),v.geometry_first_order_directions()))),det.value(),det.geometry_first_order_directions(),line.value(),line.geometry_first_order_directions())));}
   let x=root(&parent,i);let y=root(&parent,j);if x!=y{parent[y]=x;}
  }
 }
 for i in 0..input.len(){let r=root(&parent,i);if let Some(g)=groups.iter_mut().find(|g|root(&parent,g[0])==r){g.push(i);}else{groups.push(vec![i]);}}
 let mut out=Vec::new();
 for g in groups{let [a,b]=input[g[0]].endpoints.clone().map(|p|p.point);let r=sub(b,a);let axis=usize::from(r[1].value().abs()>r[0].value().abs());let mut points=g.iter().flat_map(|&i|input[i].endpoints.clone()).collect::<Vec<_>>();points.sort_by(|a,b|a.point[axis].value().total_cmp(&b.point[axis].value()).then(a.feature.cmp(&b.feature)));
  let mut nodes:Vec<UnionNode<S>>=Vec::new();for p in points{if let Some(q)=nodes.last_mut(){if p.point[axis].value()==q.point[axis].value(){if (0..2).any(|k|!same(p.point[k],q.point[k])){return Err(fail("maintained mask tied endpoint has unequal full motion"));}let mut aliases=vec![q.feature.clone(),p.feature.clone()];aliases.sort();q.feature=key(31,&aliases);continue;}}nodes.push(p);}
  let mut sources=g.iter().map(|&i|input[i].owner.clone()).collect::<Vec<_>>();sources.sort();sources.dedup();let owner=key(32,&sources);
  for pair in nodes.windows(2){let lo=pair[0].point[axis].value();let hi=pair[1].point[axis].value();if g.iter().any(|&i|{let x=input[i].endpoints[0].point[axis].value();let y=input[i].endpoints[1].point[axis].value();x.min(y)<=lo&&hi<=x.max(y)}){out.push(PhysicalSegment2{owner:owner.clone(),endpoints:[pair[0].clone(),pair[1].clone()]});}}
 }
 Ok(out)
}
