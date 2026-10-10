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

use crate::gas_geometry_time::GeometryTimeBase;
use crate::native_source_face_windows::NativeSourceFaceWindows;
use implexity_ad::Scalar;
use implexity_core::{CaeError,CaeResult};
type Bounds=[[f64;3];2];
fn overlaps(a:Bounds,b:Bounds)->bool{(0..3).all(|k|!(a[1][k]<b[0][k]||b[1][k]<a[0][k]))}
fn union(a:Bounds,b:Bounds)->Bounds{[std::array::from_fn(|k|a[0][k].min(b[0][k])),std::array::from_fn(|k|a[1][k].max(b[1][k]))]}
enum Branch{Leaf(Vec<usize>),Split(Box<Node>,Box<Node>)}
struct Node{bounds:Bounds,branch:Branch}
impl Node{
 fn build(indices:&mut[usize],bounds:&[Bounds])->Self{let all=indices.iter().skip(1).fold(bounds[indices[0]],|a,&i|union(a,bounds[i]));if indices.len()<=8{return Self{bounds:all,branch:Branch::Leaf(indices.to_vec())};}let axis=(0..3).max_by(|&a,&b|(all[1][a]-all[0][a]).total_cmp(&(all[1][b]-all[0][b]))).unwrap();indices.sort_by(|&a,&b|bounds[a][0][axis].total_cmp(&bounds[b][0][axis]).then(a.cmp(&b)));let mid=indices.len()/2;let(left,right)=indices.split_at_mut(mid);Self{bounds:all,branch:Branch::Split(Box::new(Self::build(left,bounds)),Box::new(Self::build(right,bounds)))}}
 fn query(&self,window:Bounds,bounds:&[Bounds],out:&mut Vec<usize>){if !overlaps(self.bounds,window){return;}match &self.branch{Branch::Leaf(indices)=>{out.extend(indices.iter().copied().filter(|&i|overlaps(bounds[i],window)));},Branch::Split(left,right)=>{left.query(window,bounds,out);right.query(window,bounds,out);}}}
}
pub(crate) struct SourceFacetIndex<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,bounds:Vec<Bounds>,root:Option<Node>}
impl<'a,'g,S:GeometryTimeBase> SourceFacetIndex<'a,'g,S>{
 pub(crate) fn new(source:&'a NativeSourceFaceWindows<'g,S>)->CaeResult<Self>{let mut bounds=Vec::with_capacity(source.windows().len());for w in source.windows(){let p=w.facet().vertices.map(|p|p.map(|v|v.value()));if p.iter().flatten().any(|v|!v.is_finite()){return Err(CaeError::contract("source facet bound is non-finite"));}let b=[std::array::from_fn(|k|p[0][k].min(p[1][k]).min(p[2][k])),std::array::from_fn(|k|p[0][k].max(p[1][k]).max(p[2][k]))];bounds.push(b);}let root=if bounds.is_empty(){None}else{let mut indices=(0..bounds.len()).collect::<Vec<_>>();Some(Node::build(&mut indices,&bounds))};Ok(Self{source,bounds,root})}
 pub(crate) fn candidate_indices(&self,window:usize)->CaeResult<Vec<usize>>{if window>=self.source.windows().len(){return Err(CaeError::contract("source facet index is out of range"));}let mut out=Vec::new();if let Some(root)=&self.root{root.query(self.bounds[window],&self.bounds,&mut out);}out.sort_unstable();if out.windows(2).any(|p|p[0]>=p[1]){return Err(CaeError::contract("source facet index duplicates an owner"));}Ok(out)}
 pub(crate) fn source(&self)->&'a NativeSourceFaceWindows<'g,S>{self.source}
}

