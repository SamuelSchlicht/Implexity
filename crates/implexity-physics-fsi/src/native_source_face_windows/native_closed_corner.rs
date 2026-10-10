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
#[derive(Clone,Copy,Debug)]
struct Interval{lo:f64,hi:f64}
fn next_up(v:f64)->CaeResult<f64>{
 if !v.is_finite(){return Err(fail("column interval overflow"));}
 let q=if v==0.{f64::from_bits(1)}else if v>0.{f64::from_bits(v.to_bits()+1)}else{f64::from_bits(v.to_bits()-1)};
 if !q.is_finite(){return Err(fail("column interval upper overflow"));}Ok(q)
}
fn next_down(v:f64)->CaeResult<f64>{Ok(-next_up(-v)?)}
struct Arithmetic{operations:usize}
impl Arithmetic{
 fn exact(&self,v:f64)->CaeResult<Interval>{if !v.is_finite(){return Err(fail("column interval input nonfinite"));}Ok(Interval{lo:v,hi:v})}
 fn add(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{self.operations+=2;Ok(Interval{lo:next_down(a.lo+b.lo)?,hi:next_up(a.hi+b.hi)?})}
 fn neg(&self,a:Interval)->Interval{Interval{lo:-a.hi,hi:-a.lo}}
 fn sub(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{self.add(a,self.neg(b))}
 fn mul(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{
 if a.lo==0.&&a.hi==0.||b.lo==0.&&b.hi==0.{return self.exact(0.);}
 self.operations+=4;let q=[a.lo*b.lo,a.lo*b.hi,a.hi*b.lo,a.hi*b.hi];if q.iter().any(|v|!v.is_finite()){return Err(fail("column interval product overflow"));}
 Ok(Interval{lo:next_down(q.into_iter().fold(f64::INFINITY,f64::min))?,hi:next_up(q.into_iter().fold(f64::NEG_INFINITY,f64::max))?})
 }
 fn div(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{
 if b.lo<=0.&&b.hi>=0.{return Err(fail("column interval denominator loses rank"));}
 self.operations+=4;let q=[a.lo/b.lo,a.lo/b.hi,a.hi/b.lo,a.hi/b.hi];if q.iter().any(|v|!v.is_finite()){return Err(fail("column interval quotient overflow"));}
 Ok(Interval{lo:next_down(q.into_iter().fold(f64::INFINITY,f64::min))?,hi:next_up(q.into_iter().fold(f64::NEG_INFINITY,f64::max))?})
 }
 fn affine(&mut self,lower:Interval,upper:Interval,fraction:Interval)->CaeResult<Interval>{let d=self.sub(upper,lower)?;let p=self.mul(d,fraction)?;self.add(lower,p)}
}
fn admits(i:Interval,v:f64)->bool{v.is_finite()&&v>=i.lo&&v<=i.hi}
fn channels<B:GeometryTimeBase>(v:GeometryTime<B>,n:usize)->CaeResult<Vec<f64>>{
 let mut x=v.value.geometry_first_order_directions().ok_or_else(||fail("column value directions opaque"))?;
 let mut t=v.rate.geometry_first_order_directions().ok_or_else(||fail("column rate directions opaque"))?;
 if x.len()>n||t.len()>n{return Err(fail("column direction width differs"));}x.resize(n,0.);t.resize(n,0.);
 let mut out=vec![v.value.value()];out.extend(x);out.push(v.rate.value());out.extend(t);if out.iter().any(|v|!v.is_finite()){return Err(fail("column channel nonfinite"));}Ok(out)
}
pub(crate) struct NativeClosedCornerUv<'a,'g,B:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,B>,window:usize,candidate:usize,corner:usize,target_corner:usize,chart:usize,point:[GeometryTime<B>;3],correspondence:Vec<Vec<f64>>}
pub(crate) struct NativeClosedCornerProof<'a,'g,B:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,B>,window:usize,candidate:usize,corner:usize,target_corner:usize,chart:usize,uv:[f64;2],point_bits:[Vec<u64>;3],rebuilt_bits:[Vec<u64>;3],bounds:[Vec<f64>;3],width:usize,operations:usize}
fn point_same<B:GeometryTimeBase>(a:[GeometryTime<B>;3],b:[GeometryTime<B>;3])->bool{(0..3).all(|k|same(a[k],b[k]))}
fn feature_matches(feature:&[usize],body:usize,actual:&crate::moving_contact::surface_map::SurfaceFeature)->bool{
 use crate::moving_contact::surface_map::SurfaceFeature;
 match actual{SurfaceFeature::Vertex(n)=>feature==[0,body,*n],SurfaceFeature::DensityEdge{nodes,..}=>feature==[1,body,nodes[0],nodes[1]],SurfaceFeature::FixedTetrahedron{..}=>false}
}
impl<'g,B:GeometryTimeBase> NativeSourceFaceWindows<'g,B>{
 pub(crate) fn native_closed_corner_uv<'a>(&'a self,window:usize,candidate:usize,corner:usize)->CaeResult<Option<NativeClosedCornerUv<'a,'g,B>>>{
 let w=self.windows.get(window).ok_or_else(||fail("closed native corner window absent"))?;let f=self.windows.get(candidate).ok_or_else(||fail("closed native corner candidate absent"))?;
 if corner>=3{return Err(fail("closed native corner candidate corner absent"));}
 if !matches!(w.kind,SourceWindowKind::NativePhysical)||!matches!(f.kind,SourceWindowKind::NativePhysical){return Ok(None);}
 
 for &chart in &self.maintained_charts{if !self.trace_recipe_charts.contains(&chart){continue;}let features=self.adapter.kinematics.chart_features(chart)?;
 for body in 0..2{for j in 0..3{if !feature_matches(&f.facet.vertex_features[corner],body,&features[body][j]){continue;}
 if f.facet.owner.get(1)!=Some(&body){return Err(fail("closed native corner candidate body differs"));}
 for target_corner in 0..3{if !feature_matches(&w.facet.vertex_features[target_corner],1-body,&features[1-body][j]){continue;}
 if w.facet.owner.get(1)!=Some(&(1-body)){return Err(fail("closed native corner window body differs"));}
 let point=f.facet.vertices[corner];let mapped=self.mapped_columns[chart];
 if !point_same(mapped[body*3+j],mapped[(1-body)*3+j])||!point_same(point,w.facet.vertices[target_corner]){continue;}
 let mut correspondence=self.validate_corner_correspondence(chart,body,j,point)?;correspondence.extend(self.validate_corner_correspondence(chart,1-body,j,w.facet.vertices[target_corner])?);return Ok(Some(NativeClosedCornerUv{source:self,window,candidate,corner,target_corner,chart,point,correspondence}));
 }}}
 }Ok(None)
 }
}
impl<'a,'g,B:GeometryTimeBase> NativeClosedCornerUv<'a,'g,B>{
 pub(crate) fn certify_recomposition(&self)->CaeResult<NativeClosedCornerProof<'a,'g,B>>{
 if self.correspondence.len()!=6||self.correspondence.iter().flatten().any(|v|!v.is_finite()||*v<0.){return Err(fail("closed native corner correspondence certificate invalid"));}let w=&self.source.windows[self.window].facet;let uv=[[0.,0.],[1.,0.],[0.,1.]][self.target_corner];
 let rebuilt=std::array::from_fn::<_,3,_>(|k|w.vertices[0][k]+(w.vertices[1][k]-w.vertices[0][k])*uv[0]+(w.vertices[2][k]-w.vertices[0][k])*uv[1]);
 let mut width=0;for v in w.vertices.iter().flatten().chain(&self.point).chain(&rebuilt){for x in [v.value,v.rate]{width=width.max(x.geometry_first_order_directions().ok_or_else(||fail("closed native corner opaque directions"))?.len());}}
 if width>48{return Err(fail("closed native corner direction width exceeds forty-eight"));}
 let mut arithmetic=Arithmetic{operations:0};let mut point_bits=std::array::from_fn(|_|Vec::new());let mut rebuilt_bits=std::array::from_fn(|_|Vec::new());let mut bounds=std::array::from_fn(|_|Vec::new());
 for k in 0..3{let wv=w.vertices.map(|p|channels(p[k],width)).into_iter().collect::<CaeResult<Vec<_>>>()?;let actual=channels(self.point[k],width)?;let output=channels(rebuilt[k],width)?;
 for j in 0..actual.len(){let a=arithmetic.exact(wv[0][j])?;let b=arithmetic.exact(wv[1][j])?;let c=arithmetic.exact(wv[2][j])?;let u=arithmetic.exact(uv[0])?;let v=arithmetic.exact(uv[1])?;let du=arithmetic.sub(b,a)?;let dv=arithmetic.sub(c,a)?;let pu=arithmetic.mul(du,u)?;let pv=arithmetic.mul(dv,v)?;let q=arithmetic.add(a,pu)?;let q=arithmetic.add(q,pv)?;
 if !admits(q,output[j]){return Err(fail("closed native corner lift outside counted construction"));}
 let target=arithmetic.exact(actual[j])?;let error=arithmetic.sub(q,target)?;if error.lo>0.||error.hi<0.{return Err(fail("closed native corner identity leaves counted interval"));}
 let bound=error.lo.abs().max(error.hi.abs());if !bound.is_finite()||(output[j]-actual[j]).abs()>bound{return Err(fail("closed native corner recomposition exceeds counted interval"));}bounds[k].push(bound);
 }point_bits[k]=actual.iter().map(|x|x.to_bits()).collect();rebuilt_bits[k]=output.iter().map(|x|x.to_bits()).collect();}
 Ok(NativeClosedCornerProof{source:self.source,window:self.window,candidate:self.candidate,corner:self.corner,target_corner:self.target_corner,chart:self.chart,uv,point_bits,rebuilt_bits,bounds,width,operations:arithmetic.operations})
 }
}
impl<B:GeometryTimeBase> NativeClosedCornerProof<'_,'_,B>{
 pub(crate) fn uv(&self)->[f64;2]{self.uv}
 pub(crate) fn validate_owner(&self,source:&NativeSourceFaceWindows<'_,B>,window:usize,candidate:usize,corner:usize)->CaeResult<()>{if !std::ptr::eq(self.source,source)||[self.window,self.candidate,self.corner]!=[window,candidate,corner]||!source.maintained_charts.contains(&self.chart){return Err(fail("closed native corner proof owner differs"));}Ok(())}
 pub(crate) fn validate_bindings(&self,point:[GeometryTime<B>;3],rebuilt:[GeometryTime<B>;3],uv:[f64;2])->CaeResult<()>{
 if uv.map(f64::to_bits)!=self.uv.map(f64::to_bits){return Err(fail("closed native corner proof UV differs"));}
 for k in 0..3{if channels(point[k],self.width)?.iter().map(|x|x.to_bits()).collect::<Vec<_>>()!=self.point_bits[k]||channels(rebuilt[k],self.width)?.iter().map(|x|x.to_bits()).collect::<Vec<_>>()!=self.rebuilt_bits[k]{return Err(fail("closed native corner proof fullpoint differs"));}}Ok(())
 }
}


