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

use super::{NativeSourceFaceWindows,SourceWindowKind,GeometryTime,GeometryTimeBase,TransientGasScalar,fail};
use super::column_uv::roundoff::{Arithmetic,admits,channels};
use crate::moving_contact::surface_map::SurfaceFeature;
use implexity_ad::Scalar;
use implexity_core::CaeResult;

struct Recipe<S:GeometryTimeBase>{reference:[[f64;3];2],displacement:[[GeometryTime<S>;3];2],column:[[GeometryTime<S>;3];2],point:[GeometryTime<S>;3],layer:usize,quotient:super::layer_quotient_owner::LayerQuotientReceipt}
pub(crate) struct SourceRationalFourColumn<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,uv:[f64;2],recipes:[Recipe<S>;4]}
pub(crate) struct RationalFourColumnProof<'a,'g,S:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,S>,window:usize,candidate:usize,corner:usize,uv:[f64;2],point_bits:[Vec<u64>;3],rebuilt_bits:[Vec<u64>;3],bounds:[Vec<f64>;3],width:usize,operations:usize,integer_conversions:usize,layer_divisions:usize}
fn same<S:TransientGasScalar>(a:S,b:S)->bool{a.finite_first_order()&&b.finite_first_order()&&a.value().to_bits()==b.value().to_bits()&&match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(a),Some(b))=>a.iter().map(|x|x.to_bits()).eq(b.iter().map(|x|x.to_bits())),_=>false}}
fn compatible<S:TransientGasScalar>(a:S,b:S)->bool{a.finite_first_order()&&b.finite_first_order()&&a.value()==b.value()&&match(a.geometry_first_order_directions(),b.geometry_first_order_directions()){(Some(a),Some(b))=>a.len()==b.len()&&a.iter().zip(b).all(|(a,b)|*a==b),_=>false}}
fn same3<S:TransientGasScalar>(a:[S;3],b:[S;3])->bool{(0..3).all(|k|same(a[k],b[k]))}
fn exact_difference_equal(a:f64,b:f64,c:f64,d:f64)->bool{
 fn difference(a:f64,b:f64)->Option<[f64;2]>{let x=a-b;let bv=a-x;let av=x+bv;let br=bv-b;let ar=a-av;let y=ar+br;[a,b,x,bv,av,br,ar,y].iter().all(|v|v.is_finite()&&(*v==0.||v.abs()>=f64::MIN_POSITIVE)).then_some([x,y])}
 matches!((difference(a,b),difference(c,d)),(Some(a),Some(b))if a==b)
}
impl<S:GeometryTimeBase> RationalFourColumnProof<'_,'_,S>{
 pub(crate) fn uv(&self)->[f64;2]{self.uv}
 pub(crate) fn bounds(&self)->&[Vec<f64>;3]{&self.bounds}
 pub(crate) fn operation_count(&self)->usize{self.operations}
 pub(crate) fn integer_conversion_count(&self)->usize{self.integer_conversions}
 pub(crate) fn layer_division_count(&self)->usize{self.layer_divisions}
 pub(crate) fn validate_owner(&self,source:&NativeSourceFaceWindows<'_,S>,window:usize,candidate:usize,corner:usize)->CaeResult<()>{if !std::ptr::eq(source,self.source)||[window,candidate,corner]!=[self.window,self.candidate,self.corner]{return Err(fail("rational four-column owner differs"));}Ok(())}
 pub(crate) fn validate_bindings(&self,point:[GeometryTime<S>;3],rebuilt:[GeometryTime<S>;3],uv:[f64;2])->CaeResult<()>{if uv.map(f64::to_bits)!=self.uv.map(f64::to_bits){return Err(fail("rational four-column UV differs"));}for k in 0..3{if channels(point[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.point_bits[k]||channels(rebuilt[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.rebuilt_bits[k]{return Err(fail("rational four-column observed scalar bits differ"));}}Ok(())}
}
impl<'a,'g,S:GeometryTimeBase> SourceRationalFourColumn<'a,'g,S>{
 pub(crate) fn point(&self)->[GeometryTime<S>;3]{self.recipes[3].point}
 pub(crate) fn certify_recomposition(&self)->CaeResult<RationalFourColumnProof<'a,'g,S>>{
  let window=&self.source.windows[self.window].facet;let uv=self.uv;
  let rebuilt=std::array::from_fn::<_,3,_>(|k|window.vertices[0][k]+(window.vertices[1][k]-window.vertices[0][k])*uv[0]+(window.vertices[2][k]-window.vertices[0][k])*uv[1]);
  let mut width=0;for r in &self.recipes{for p in r.displacement.iter().chain(&r.column).chain(std::iter::once(&r.point)){for v in p{for q in[v.value,v.rate]{width=width.max(q.geometry_first_order_directions().ok_or_else(||fail("rational four-column geometry is opaque"))?.len());}}}}
  for v in rebuilt{for q in[v.value,v.rate]{width=width.max(q.geometry_first_order_directions().ok_or_else(||fail("rational four-column lift is opaque"))?.len());}}
  if width>48{return Err(fail("rational four-column direction scope exceeds forty-eight"));}
  let mut arithmetic=Arithmetic{operations:0};
  let mut fractions=Vec::new();for r in &self.recipes{if r.quotient.layer()!=r.layer||r.quotient.layers()!=self.source.adapter.layers{return Err(fail("rational source producer quotient identity differs"));}fractions.push(r.quotient.counted_interval(&mut arithmetic)?);}
  let mut point_bits=std::array::from_fn(|_|Vec::new());let mut rebuilt_bits=std::array::from_fn(|_|Vec::new());let mut bounds=std::array::from_fn(|_|Vec::new());
  for k in 0..3{
   let observed=self.recipes.iter().map(|r|channels(r.point[k],width)).collect::<CaeResult<Vec<_>>>()?;let lift=channels(rebuilt[k],width)?;
   let displacements=self.recipes.iter().map(|r|r.displacement.map(|p|channels(p[k],width)).into_iter().collect::<CaeResult<Vec<_>>>()).collect::<CaeResult<Vec<_>>>()?;
   let columns=self.recipes.iter().map(|r|r.column.map(|p|channels(p[k],width)).into_iter().collect::<CaeResult<Vec<_>>>()).collect::<CaeResult<Vec<_>>>()?;
   for channel in 0..lift.len(){let mut points=Vec::new();for i in 0..4{let mut endpoints=Vec::new();for body in 0..2{let d=arithmetic.exact(displacements[i][body][channel])?;let r=arithmetic.exact(if channel==0{self.recipes[i].reference[body][k]}else{0.})?;let value=arithmetic.add(d,r)?;if !admits(value,columns[i][body][channel]){return Err(fail("rational native addition exceeds counted interval"));}endpoints.push(value);}let point=arithmetic.affine(endpoints[0],endpoints[1],fractions[i])?;if !admits(point,observed[i][channel]){return Err(fail("rational source interpolation exceeds counted interval"));}points.push(point);}
    let d0=arithmetic.sub(points[1],points[0])?;let d1=arithmetic.sub(points[2],points[0])?;let u=arithmetic.exact(uv[0])?;let v=arithmetic.exact(uv[1])?;let first=arithmetic.mul(d0,u)?;let second=arithmetic.mul(d1,v)?;let first=arithmetic.add(points[0],first)?;let value=arithmetic.add(first,second)?;if !admits(value,lift[channel]){return Err(fail("rational source lift exceeds counted interval"));}let error=arithmetic.sub(value,points[3])?;if error.lo>0.||error.hi<0.{return Err(fail("rational source identity exceeds counted interval"));}let bound=error.lo.abs().max(error.hi.abs());if !bound.is_finite()||(lift[channel]-observed[3][channel]).abs()>bound{return Err(fail("rational source actual recomposition error exceeds interval"));}bounds[k].push(bound);
   }point_bits[k]=observed[3].iter().map(|v|v.to_bits()).collect();rebuilt_bits[k]=lift.iter().map(|v|v.to_bits()).collect();
  }
  Ok(RationalFourColumnProof{source:self.source,window:self.window,candidate:self.candidate,corner:self.corner,uv,point_bits,rebuilt_bits,bounds,width,operations:arithmetic.operations,integer_conversions:self.recipes.len()*2,layer_divisions:self.recipes.len()})
 }
}
impl<'g,S:GeometryTimeBase> NativeSourceFaceWindows<'g,S>{
 pub(crate) fn rational_four_column_uv<'a>(&'a self,window:usize,candidate:usize,corner:usize)->CaeResult<Option<SourceRationalFourColumn<'a,'g,S>>>{
  let w=self.windows.get(window).ok_or_else(||fail("rational source window is absent"))?;let f=self.windows.get(candidate).ok_or_else(||fail("rational source candidate is absent"))?;
  if corner>=3{return Err(fail("rational source corner is absent"));}if !matches!(w.kind,SourceWindowKind::Layered{..})||!matches!(f.kind,SourceWindowKind::Layered{..}){return Ok(None);}
  let layers=self.adapter.layers;if layers==0||layers as u128>(1u128<<53){return Err(fail("rational source layer count is outside exact integer range"));}
  let features=[&w.facet.vertex_features[0],&w.facet.vertex_features[1],&w.facet.vertex_features[2],&f.facet.vertex_features[corner]];
  if features.iter().any(|p|p.len()!=3||p[0]!=2||p[2]>layers){return Err(fail("rational source layer feature is invalid"));}
  if features.iter().enumerate().any(|(i,p)|features[i+1..].iter().any(|q|p[1]==q[1])){return Ok(None);}
  let Some(layer_owner)=self.layer_quotients.as_ref() else{return Ok(None);};
  let owner=self.adapter.owner.capacity_owner();self.adapter.kinematics.validate_native_owner_bindings(owner)?;
  let recipe=|feature:&[usize],point:[GeometryTime<S>;3]|->CaeResult<Option<Recipe<S>>>{
   for(chart,ids)in self.adapter.paired_vertex_ids.iter().enumerate(){for local in 0..3{if ids[local]!=feature[1]{continue;}let actual_features=self.adapter.kinematics.chart_features(chart)?;let mut reference=[[0.;3];2];let mut displacement=[[GeometryTime::<S>::zero();3];2];let mut column=[[GeometryTime::<S>::zero();3];2];let mut supported=true;
    for body in 0..2{let SurfaceFeature::Vertex(node)=actual_features[body][local] else{supported=false;break;};let Some(binding)=owner.nodes[body].get(node) else{return Err(fail("rational source native vertex owner is absent"));};reference[body]=binding.reference_m;displacement[body]=binding.force_flux.map(|i|self.live_trace[i]);column[body]=std::array::from_fn(|k|displacement[body][k]+reference[body][k]);if !same3(column[body],self.mapped_columns[chart][local+3*body]){supported=false;break;}}
    if !supported{continue;}let fraction=feature[2] as f64/layers as f64;let observed=std::array::from_fn(|k|column[0][k]+(column[1][k]-column[0][k])*fraction);if same3(observed,point){let quotient=layer_owner.receipt(feature[1],feature[2],point)?;if quotient.observed().to_bits()!=fraction.to_bits(){return Err(fail("rational source original producer quotient differs"));}return Ok(Some(Recipe{reference,displacement,column,point,layer:feature[2],quotient}));}
   }}Ok(None)
  };
  let mut recipes=Vec::new();for i in 0..4{let point=if i<3{w.facet.vertices[i]}else{f.facet.vertices[corner]};let Some(r)=recipe(features[i],point)? else{return Ok(None);};recipes.push(r);}
  let recipes:[Recipe<S>;4]=recipes.try_into().map_err(|_|fail("rational source recipe count differs"))?;
  for body in 0..2{for i in 1..4{for k in 0..3{if !compatible(recipes[0].displacement[body][k],recipes[i].displacement[body][k]){return Ok(None);}}}}
  for i in 1..4{for k in 0..3{if !exact_difference_equal(recipes[i].reference[1][k],recipes[i].reference[0][k],recipes[0].reference[1][k],recipes[0].reference[0][k]){return Ok(None);}}}
  for c in 0..3{let ab=(0..3).filter(|i|*i!=c).collect::<Vec<_>>();let(a,b)=(ab[0],ab[1]);
   if recipes[a].layer as i128+recipes[b].layer as i128-recipes[c].layer as i128!=recipes[3].layer as i128{continue;}
   if !(0..2).all(|body|(0..3).all(|k|exact_difference_equal(recipes[a].reference[body][k],recipes[c].reference[body][k],recipes[3].reference[body][k],recipes[b].reference[body][k]))){continue;}
   let corners=[[0.,0.],[1.,0.],[0.,1.]];let uv=std::array::from_fn(|k|corners[a][k]+corners[b][k]-corners[c][k]);return Ok(Some(SourceRationalFourColumn{source:self,window,candidate,corner,uv,recipes}));
  }Ok(None)
 }
}
