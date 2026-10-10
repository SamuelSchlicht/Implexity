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

use super::*;
use crate::moving_contact::surface_map::SurfaceFeature;
use super::column_uv::roundoff::{Arithmetic,Interval,admits,channels};
fn same_point<S:TransientGasScalar>(a:S,b:S)->bool{a.value().to_bits()==b.value().to_bits()&&match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(x),Some(y))=>x.iter().map(|v|v.to_bits()).eq(y.iter().map(|v|v.to_bits())),_=>false}}
fn same3<S:TransientGasScalar>(a:[S;3],b:[S;3])->bool{(0..3).all(|k|same_point(a[k],b[k]))}
pub(crate) struct SourceReferenceFibreUv<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,anchor:usize,reference:[usize;2],fractions:[f64;4],candidate_column:[[GeometryTime<S>;3];2],reference_column:[[GeometryTime<S>;3];2],point:[GeometryTime<S>;3],uv:[f64;2],native_reference:[[f64;3];4],native_displacement:[[GeometryTime<S>;3];4]}
pub(crate) struct ReferenceFibreProof<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,point_bits:[Vec<u64>;3],reconstructed_bits:[Vec<u64>;3],uv:[f64;2],bounds:[Vec<f64>;3],width:usize,operations:usize}
impl<S:GeometryTimeBase> ReferenceFibreProof<'_,'_,S>{
 pub(crate) fn uv(&self)->[f64;2]{self.uv}
 pub(crate) fn bounds(&self)->&[Vec<f64>;3]{&self.bounds}
 pub(crate) fn operation_count(&self)->usize{self.operations}
 pub(crate) fn validate_owner(&self,s:&NativeSourceFaceWindows<'_,S>,w:usize,c:usize,k:usize)->CaeResult<()>{if !std::ptr::eq(self.source,s)||[self.window,self.candidate,self.corner]!=[w,c,k]{return Err(fail("reference fibre proof owner differs"));}Ok(())}
 pub(crate) fn validate_bindings(&self,point:[GeometryTime<S>;3],reconstructed:[GeometryTime<S>;3],uv:[f64;2])->CaeResult<()>{if uv.map(f64::to_bits)!=self.uv.map(f64::to_bits){return Err(fail("reference fibre proof UV differs"));}for k in 0..3{if channels(point[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.point_bits[k]||channels(reconstructed[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.reconstructed_bits[k]{return Err(fail("reference fibre proof fullpoint differs"));}}Ok(())}
}
impl<'a,'g,S:GeometryTimeBase> SourceReferenceFibreUv<'a,'g,S>{
 pub(crate) fn point(&self)->[GeometryTime<S>;3]{self.point}
 pub(crate) fn uv(&self)->[f64;2]{self.uv}
 pub(crate) fn certify_recomposition(&self)->CaeResult<ReferenceFibreProof<'a,'g,S>>{
 let w=&self.source.windows[self.window].facet;let uv=self.uv;let reconstructed=std::array::from_fn::<_,3,_>(|k|w.vertices[0][k]+(w.vertices[1][k]-w.vertices[0][k])*uv[0]+(w.vertices[2][k]-w.vertices[0][k])*uv[1]);
 let mut width=0;for p in self.candidate_column.iter().chain(&self.reference_column).chain(&w.vertices).chain(&[self.point,reconstructed]){for v in p{for q in[v.value,v.rate]{width=width.max(q.geometry_first_order_directions().ok_or_else(||fail("reference fibre opaque geometry"))?.len());}}}if width>48{return Err(fail("reference fibre scope exceeds forty-eight directions"));}
 let mut a=Arithmetic{operations:0};let f=self.fractions.map(|v|a.exact(v)).into_iter().collect::<CaeResult<Vec<_>>>()?;let num=a.sub(f[3],f[0])?;let den=a.sub(f[2],f[1])?;let lambda=a.div(num,den)?;let corners=[[0.,0.],[1.,0.],[0.,1.]];let mut ui=[Interval{lo:0.,hi:0.};2];for k in 0..2{let d=a.sub(a.exact(corners[self.reference[1]][k])?,a.exact(corners[self.reference[0]][k])?)?;let p=a.mul(d,lambda)?;ui[k]=a.add(a.exact(corners[self.anchor][k])?,p)?;if !admits(ui[k],uv[k]){return Err(fail("reference fibre UV outside counted interval"));}}
 let mut point_bits=std::array::from_fn(|_|Vec::new());let mut reconstructed_bits=std::array::from_fn(|_|Vec::new());let mut bounds=std::array::from_fn(|_|Vec::new());
 for k in 0..3{let cl=channels(self.candidate_column[0][k],width)?;let cu=channels(self.candidate_column[1][k],width)?;let rl=channels(self.reference_column[0][k],width)?;let ru=channels(self.reference_column[1][k],width)?;let p=channels(self.point[k],width)?;let r=channels(reconstructed[k],width)?;let observed=w.vertices.map(|v|channels(v[k],width)).into_iter().collect::<CaeResult<Vec<_>>>()?;
 for j in 0..p.len(){let mut native=[a.exact(0.)?;4];for i in 0..4{let d=channels(self.native_displacement[i][k],width)?;native[i]=a.add(a.exact(d[j])?,a.exact(if j==0{self.native_reference[i][k]}else{0.})?)?;if !admits(native[i],[cl[j],cu[j],rl[j],ru[j]][i]){return Err(fail("native reference addition outside counted interval"));}}let mut endpoints=[a.exact(0.)?;3];endpoints[self.anchor]=a.affine(native[0],native[1],f[0])?;endpoints[self.reference[0]]=a.affine(native[2],native[3],f[1])?;endpoints[self.reference[1]]=a.affine(native[2],native[3],f[2])?;for i in 0..3{if !admits(endpoints[i],observed[i][j]){return Err(fail("reference fibre observed endpoint outside counted interval"));}}
 let actual=a.affine(native[0],native[1],f[3])?;if !admits(actual,p[j]){return Err(fail("reference fibre point outside counted interval"));}let d0=a.sub(endpoints[1],endpoints[0])?;let d1=a.sub(endpoints[2],endpoints[0])?;let t0=a.mul(d0,ui[0])?;let t1=a.mul(d1,ui[1])?;let z=a.add(endpoints[0],t0)?;let lift=a.add(z,t1)?;if !admits(lift,r[j]){return Err(fail("reference fibre recomposition outside counted interval"));}let error=a.sub(lift,actual)?;if error.lo>0.||error.hi<0.{return Err(fail("reference fibre affine identity leaves counted interval"));}let bound=error.lo.abs().max(error.hi.abs());if !bound.is_finite()||(r[j]-p[j]).abs()>bound{return Err(fail("reference fibre recomposition error exceeds interval"));}bounds[k].push(bound);
 }point_bits[k]=p.iter().map(|v|v.to_bits()).collect();reconstructed_bits[k]=r.iter().map(|v|v.to_bits()).collect();}
 Ok(ReferenceFibreProof{source:self.source,window:self.window,candidate:self.candidate,corner:self.corner,point_bits,reconstructed_bits,uv,bounds,width,operations:a.operations})
 }
}
impl<'g,S:GeometryTimeBase> NativeSourceFaceWindows<'g,S>{
 pub(crate) fn reference_fibre_uv<'a>(&'a self,window:usize,candidate:usize,corner:usize)->CaeResult<Option<SourceReferenceFibreUv<'a,'g,S>>>{
 let w=self.windows.get(window).ok_or_else(||fail("reference fibre window absent"))?;let f=self.windows.get(candidate).ok_or_else(||fail("reference fibre candidate absent"))?;if corner>=3{return Err(fail("reference fibre corner absent"));}if !matches!(w.kind,SourceWindowKind::Layered{..})||!matches!(f.kind,SourceWindowKind::Layered{..}){return Ok(None);}
 let p=&f.facet.vertex_features[corner];if p.len()!=3||p[0]!=2{return Err(fail("reference fibre feature malformed"));}let own=w.facet.vertex_features.iter().enumerate().filter(|(_,q)|q.len()==3&&q[0]==2&&q[1]==p[1]).collect::<Vec<_>>();if own.len()!=1{return Ok(None);}let anchor=own[0].0;let others=(0..3).filter(|&i|i!=anchor).collect::<Vec<_>>();let a=&w.facet.vertex_features[others[0]];let b=&w.facet.vertex_features[others[1]];if a.len()!=3||b.len()!=3||a[0]!=2||b[0]!=2||a[1]!=b[1]||a[2]==b[2]{return Ok(None);}
 if self.adapter.layers==0||self.adapter.layers as u128>(1u128<<53)||[own[0].1[2],a[2],b[2],p[2]].iter().any(|&l|l>self.adapter.layers||l as u128>(1u128<<53)){return Err(fail("reference fibre layers outside exact mesh"));}
 for(chart,ids)in self.adapter.paired_vertex_ids.iter().enumerate(){let Some(c)=ids.iter().position(|&x|x==p[1])else{continue;};let Some(r)=ids.iter().position(|&x|x==a[1])else{continue;};let cols=self.mapped_columns[chart];let candidate_column=[cols[c],cols[c+3]];let reference_column=[cols[r],cols[r+3]];
 let feature=self.adapter.kinematics.chart_features(chart)?;let owner=self.adapter.owner.capacity_owner();self.adapter.kinematics.validate_native_owner_bindings(owner)?;let mut refs=[[0.;3];4];let mut displacement=[[GeometryTime::<S>::zero();3];4];let mut genuine=true;
 for(body,local,index)in[(0,c,0),(1,c,1),(0,r,2),(1,r,3)]{let SurfaceFeature::Vertex(node)=feature[body][local] else{genuine=false;break;};let binding=&owner.nodes[body][node];refs[index]=binding.reference_m;displacement[index]=binding.force_flux.map(|i|self.live_trace[i]);let actual=std::array::from_fn(|k|displacement[index][k]+refs[index][k]);let expected=if index<2{candidate_column[index]}else{reference_column[index-2]};if !same3(actual,expected){genuine=false;break;}}
 if !genuine||(0..3).any(|k|!same_point(displacement[0][k],displacement[2][k])||!same_point(displacement[1][k],displacement[3][k])||!reference_difference_equal(refs[1][k],refs[0][k],refs[3][k],refs[2][k])){continue;}

 let fractions=[own[0].1[2],a[2],b[2],p[2]].map(|l|l as f64/self.adapter.layers as f64);let at=|col:[[GeometryTime<S>;3];2],v:f64|std::array::from_fn(|k|col[0][k]+(col[1][k]-col[0][k])*v);
 if !same3(at(candidate_column,fractions[0]),w.facet.vertices[anchor])||!same3(at(reference_column,fractions[1]),w.facet.vertices[others[0]])||!same3(at(reference_column,fractions[2]),w.facet.vertices[others[1]])||!same3(at(candidate_column,fractions[3]),f.facet.vertices[corner]){continue;}
 let d=fractions[2]-fractions[1];if d==0.||!d.is_finite(){return Err(fail("reference fibre rounded fractions lose rank"));}let lambda=(fractions[3]-fractions[0])/d;let corners=[[0.,0.],[1.,0.],[0.,1.]];let uv=std::array::from_fn(|k|corners[anchor][k]+(corners[others[1]][k]-corners[others[0]][k])*lambda);if uv.iter().any(|v|!v.is_finite()){return Err(fail("reference fibre UV nonfinite"));}
 return Ok(Some(SourceReferenceFibreUv{source:self,window,candidate,corner,anchor,reference:[others[0],others[1]],fractions,candidate_column,reference_column,point:f.facet.vertices[corner],uv,native_reference:refs,native_displacement:displacement}));
 }Ok(None)
 }
}

fn reference_difference_equal(a:f64,b:f64,c:f64,d:f64)->bool{
 fn pair(a:f64,b:f64)->Option<[f64;2]>{let x=a-b;let bv=a-x;let av=x+bv;let br=bv-b;let ar=a-av;let y=ar+br;[x,bv,av,br,ar,y].iter().all(|v|v.is_finite()).then_some([x,y])}
 matches!((pair(a,b),pair(c,d)),(Some(x),Some(y))if x==y)
}
