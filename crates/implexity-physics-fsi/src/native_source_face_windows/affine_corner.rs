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
use super::column_uv::roundoff::{Arithmetic,Interval,admits,channels};
fn same<S:TransientGasScalar>(a:S,b:S)->bool{a.value().to_bits()==b.value().to_bits()&&match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(x),Some(y))=>x.iter().map(|v|v.to_bits()).eq(y.iter().map(|v|v.to_bits())),_=>false}}
fn same3<S:TransientGasScalar>(a:[S;3],b:[S;3])->bool{(0..3).all(|k|same(a[k],b[k]))}
struct Recipe<S:GeometryTimeBase>{lower:[GeometryTime<S>;3],upper:[GeometryTime<S>;3],fraction:f64,point:[GeometryTime<S>;3]}
pub(crate) struct SourceAffineCorner<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,uv:[f64;2],recipes:[Recipe<S>;4]}
pub(crate) struct AffineCornerProof<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,uv:[f64;2],width:usize,point_bits:[Vec<u64>;3],rebuilt_bits:[Vec<u64>;3],bounds:[Vec<f64>;3],operations:usize}
impl<S:GeometryTimeBase> AffineCornerProof<'_,'_,S>{
 pub(crate) fn uv(&self)->[f64;2]{self.uv}
 pub(crate) fn bounds(&self)->&[Vec<f64>;3]{&self.bounds}
 pub(crate) fn operation_count(&self)->usize{self.operations}
 pub(crate) fn validate_owner(&self,source:&NativeSourceFaceWindows<'_,S>,window:usize,candidate:usize,corner:usize)->CaeResult<()>{if !std::ptr::eq(source,self.source)||[window,candidate,corner]!=[self.window,self.candidate,self.corner]{return Err(fail("affine corner owner differs"));}Ok(())}
 pub(crate) fn validate_bindings(&self,point:[GeometryTime<S>;3],rebuilt:[GeometryTime<S>;3],uv:[f64;2])->CaeResult<()>{if self.uv.map(f64::to_bits)!=uv.map(f64::to_bits){return Err(fail("affine corner UV differs"));}for k in 0..3{if channels(point[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.point_bits[k]||channels(rebuilt[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.rebuilt_bits[k]{return Err(fail("affine corner fullpoint differs"));}}Ok(())}
}
impl<'a,'g,S:GeometryTimeBase> SourceAffineCorner<'a,'g,S>{
 pub(crate) fn point(&self)->[GeometryTime<S>;3]{self.recipes[3].point}
 pub(crate) fn certify_recomposition(&self)->CaeResult<AffineCornerProof<'a,'g,S>>{
  let w=&self.source.windows[self.window].facet;let uv=self.uv;let rebuilt=std::array::from_fn::<_,3,_>(|k|w.vertices[0][k]+(w.vertices[1][k]-w.vertices[0][k])*uv[0]+(w.vertices[2][k]-w.vertices[0][k])*uv[1]);
  let mut width=0;for r in &self.recipes{for p in [r.lower,r.upper,r.point]{for v in p{for q in [v.value,v.rate]{width=width.max(q.geometry_first_order_directions().ok_or_else(||fail("affine corner geometry opaque"))?.len());}}}}for v in rebuilt{for q in[v.value,v.rate]{width=width.max(q.geometry_first_order_directions().ok_or_else(||fail("affine corner rebuilt geometry opaque"))?.len());}}if width>48{return Err(fail("affine corner scope exceeds forty-eight"));}
  let mut a=Arithmetic{operations:0};let u=[a.exact(uv[0])?,a.exact(uv[1])?];let mut point_bits=std::array::from_fn(|_|Vec::new());let mut rebuilt_bits=std::array::from_fn(|_|Vec::new());let mut bounds=std::array::from_fn(|_|Vec::new());
  for k in 0..3{let lower=self.recipes.iter().map(|r|channels(r.lower[k],width)).collect::<CaeResult<Vec<_>>>()?;let upper=self.recipes.iter().map(|r|channels(r.upper[k],width)).collect::<CaeResult<Vec<_>>>()?;let observed=self.recipes.iter().map(|r|channels(r.point[k],width)).collect::<CaeResult<Vec<_>>>()?;let q=channels(rebuilt[k],width)?;
   for j in 0..q.len(){let mut points=[Interval{lo:0.,hi:0.};4];for i in 0..4{points[i]=a.affine(a.exact(lower[i][j])?,a.exact(upper[i][j])?,a.exact(self.recipes[i].fraction)?)?;if !admits(points[i],observed[i][j]){return Err(fail("affine source recipe leaves counted interval"));}}
    let d0=a.sub(points[1],points[0])?;let d1=a.sub(points[2],points[0])?;let t0=a.mul(d0,u[0])?;let t1=a.mul(d1,u[1])?;let first=a.add(points[0],t0)?;let lift=a.add(first,t1)?;if !admits(lift,q[j]){return Err(fail("affine corner rebuilt point leaves counted interval"));}let e=a.sub(lift,points[3])?;if e.lo>0.||e.hi<0.{return Err(fail("affine corner identity leaves counted interval"));}let bound=e.lo.abs().max(e.hi.abs());if !bound.is_finite()||(q[j]-observed[3][j]).abs()>bound{return Err(fail("affine corner error exceeds counted interval"));}bounds[k].push(bound);
   }point_bits[k]=observed[3].iter().map(|v|v.to_bits()).collect();rebuilt_bits[k]=q.iter().map(|v|v.to_bits()).collect();
  }
  Ok(AffineCornerProof{source:self.source,window:self.window,candidate:self.candidate,corner:self.corner,uv,width,point_bits,rebuilt_bits,bounds,operations:a.operations})
 }
}
impl<'g,S:GeometryTimeBase> NativeSourceFaceWindows<'g,S>{
 pub(crate) fn affine_corner_uv<'a>(&'a self,window:usize,candidate:usize,corner:usize)->CaeResult<Option<SourceAffineCorner<'a,'g,S>>>{
  let w=self.windows.get(window).ok_or_else(||fail("affine window absent"))?;let f=self.windows.get(candidate).ok_or_else(||fail("affine candidate absent"))?;if corner>=3{return Err(fail("affine corner absent"));}if !matches!(w.kind,SourceWindowKind::Layered{..})||!matches!(f.kind,SourceWindowKind::Layered{..}){return Ok(None);}
  let point=f.facet.vertices[corner];let mut coefficients=None;for a in 0..3{for b in a+1..3{let c=3-a-b;if(0..3).all(|k|(same(point[k],w.facet.vertices[a][k])&&same(w.facet.vertices[b][k],w.facet.vertices[c][k]))||(same(point[k],w.facet.vertices[b][k])&&same(w.facet.vertices[a][k],w.facet.vertices[c][k]))){coefficients=Some([a,b,c]);break;}}if coefficients.is_some(){break;}}
  let Some([a,b,c])=coefficients else{return Ok(None)};
  if self.adapter.layers==0||self.adapter.layers as u128>(1u128<<53){return Err(fail("affine source layer range differs"));}
  let recipe=|feature:&[usize],actual:[GeometryTime<S>;3]|->CaeResult<Recipe<S>>{if feature.len()!=3||feature[0]!=2||feature[2]>self.adapter.layers{return Err(fail("affine source feature differs"));}let fraction=feature[2] as f64/self.adapter.layers as f64;for(chart,ids)in self.adapter.paired_vertex_ids.iter().enumerate(){for j in 0..3{if ids[j]!=feature[1]{continue;}let lower=self.mapped_columns[chart][j];let upper=self.mapped_columns[chart][j+3];let p=std::array::from_fn(|k|lower[k]+(upper[k]-lower[k])*fraction);if same3(p,actual){return Ok(Recipe{lower,upper,fraction,point:actual});}}}Err(fail("affine corner has no authentic column recipe"))};
  let recipes=[recipe(&w.facet.vertex_features[0],w.facet.vertices[0])?,recipe(&w.facet.vertex_features[1],w.facet.vertices[1])?,recipe(&w.facet.vertex_features[2],w.facet.vertices[2])?,recipe(&f.facet.vertex_features[corner],point)?];
  let corners=[[0.,0.],[1.,0.],[0.,1.]];let uv=std::array::from_fn(|k|corners[a][k]+corners[b][k]-corners[c][k]);Ok(Some(SourceAffineCorner{source:self,window,candidate,corner,uv,recipes}))
 }
}
