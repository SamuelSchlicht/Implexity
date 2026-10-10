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
pub(crate) struct Interval{pub(crate) lo:f64,pub(crate) hi:f64}
fn next_up(v:f64)->CaeResult<f64>{
 if !v.is_finite(){return Err(fail("column interval overflow"));}
 let q=if v==0.{f64::from_bits(1)}else if v>0.{f64::from_bits(v.to_bits()+1)}else{f64::from_bits(v.to_bits()-1)};
 if !q.is_finite(){return Err(fail("column interval upper overflow"));}Ok(q)
}
fn next_down(v:f64)->CaeResult<f64>{Ok(-next_up(-v)?)}
pub(crate) struct Arithmetic{pub(crate) operations:usize}
impl Arithmetic{
 pub(crate) fn exact(&self,v:f64)->CaeResult<Interval>{if !v.is_finite(){return Err(fail("column interval input nonfinite"));}Ok(Interval{lo:v,hi:v})}
 pub(crate) fn add(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{self.operations+=2;Ok(Interval{lo:next_down(a.lo+b.lo)?,hi:next_up(a.hi+b.hi)?})}
 pub(crate) fn neg(&self,a:Interval)->Interval{Interval{lo:-a.hi,hi:-a.lo}}
 pub(crate) fn sub(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{self.add(a,self.neg(b))}
 pub(crate) fn mul(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{
 if a.lo==0.&&a.hi==0.||b.lo==0.&&b.hi==0.{return self.exact(0.);}
 self.operations+=4;let q=[a.lo*b.lo,a.lo*b.hi,a.hi*b.lo,a.hi*b.hi];if q.iter().any(|v|!v.is_finite()){return Err(fail("column interval product overflow"));}
 Ok(Interval{lo:next_down(q.into_iter().fold(f64::INFINITY,f64::min))?,hi:next_up(q.into_iter().fold(f64::NEG_INFINITY,f64::max))?})
 }
 pub(crate) fn div(&mut self,a:Interval,b:Interval)->CaeResult<Interval>{
 if b.lo<=0.&&b.hi>=0.{return Err(fail("column interval denominator loses rank"));}
 self.operations+=4;let q=[a.lo/b.lo,a.lo/b.hi,a.hi/b.lo,a.hi/b.hi];if q.iter().any(|v|!v.is_finite()){return Err(fail("column interval quotient overflow"));}
 Ok(Interval{lo:next_down(q.into_iter().fold(f64::INFINITY,f64::min))?,hi:next_up(q.into_iter().fold(f64::NEG_INFINITY,f64::max))?})
 }
 pub(crate) fn affine(&mut self,lower:Interval,upper:Interval,fraction:Interval)->CaeResult<Interval>{let d=self.sub(upper,lower)?;let p=self.mul(d,fraction)?;self.add(lower,p)}
}
pub(crate) fn admits(i:Interval,v:f64)->bool{v.is_finite()&&v>=i.lo&&v<=i.hi}
pub(crate) fn channels<B:GeometryTimeBase>(v:GeometryTime<B>,n:usize)->CaeResult<Vec<f64>>{
 let mut x=v.value.geometry_first_order_directions().ok_or_else(||fail("column value directions opaque"))?;
 let mut t=v.rate.geometry_first_order_directions().ok_or_else(||fail("column rate directions opaque"))?;
 if x.len()>n||t.len()>n{return Err(fail("column direction width differs"));}x.resize(n,0.);t.resize(n,0.);
 let mut out=vec![v.value.value()];out.extend(x);out.push(v.rate.value());out.extend(t);if out.iter().any(|v|!v.is_finite()){return Err(fail("column channel nonfinite"));}Ok(out)
}
pub(crate) struct ColumnRecompositionProof<'a,'g,B:GeometryTimeBase>{source:&'a NativeSourceFaceWindows<'g,B>,window:usize,candidate:usize,corner:usize,point_bits:[Vec<u64>;3],reconstructed_bits:[Vec<u64>;3],bounds:[Vec<f64>;3],uv:[f64;2],width:usize,operations:usize}
impl<B:GeometryTimeBase> ColumnRecompositionProof<'_,'_,B>{
 pub(crate) fn bounds(&self)->&[Vec<f64>;3]{&self.bounds}
 pub(crate) fn channel_width(&self)->usize{self.width}
 pub(crate) fn operation_count(&self)->usize{self.operations}
 pub(crate) fn uv(&self)->[f64;2]{self.uv}
 pub(crate) fn validate_owner(&self,source:&NativeSourceFaceWindows<'_,B>,window:usize,candidate:usize,corner:usize)->CaeResult<()>{if !std::ptr::eq(self.source,source)||self.window!=window||self.candidate!=candidate||self.corner!=corner{return Err(fail("column proof source binding differs"));}Ok(())}
 pub(crate) fn validate_bindings(&self,point:[GeometryTime<B>;3],reconstructed:[GeometryTime<B>;3],uv:[f64;2])->CaeResult<()>{
 if uv.map(f64::to_bits)!=self.uv.map(f64::to_bits){return Err(fail("column proof UV binding differs"));}
 for k in 0..3{if channels(point[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.point_bits[k]||channels(reconstructed[k],self.width)?.iter().map(|v|v.to_bits()).collect::<Vec<_>>()!=self.reconstructed_bits[k]{return Err(fail("column proof fullpoint binding differs"));}}Ok(())
 }
}
impl<'a,'g,B:GeometryTimeBase> SourceColumnUv<'a,'g,B>{
 pub(crate) fn certify_recomposition(&self)->CaeResult<ColumnRecompositionProof<'a,'g,B>>{
 let window=&self.source.windows[self.window].facet;let uv=self.uv;
 let reconstructed=std::array::from_fn::<_,3,_>(|k|window.vertices[0][k]+(window.vertices[1][k]-window.vertices[0][k])*uv[0]+(window.vertices[2][k]-window.vertices[0][k])*uv[1]);
 let values=self.lower.iter().chain(&self.upper).chain(window.vertices.iter().flatten()).chain(&self.point).chain(&reconstructed);
 let mut n=0;for v in values{for a in [v.value,v.rate]{let d=a.geometry_first_order_directions().ok_or_else(||fail("column geometry directions opaque"))?;n=n.max(d.len());}}
 if n>48{return Err(fail("column geometry scope exceeds forty-eight directions"));}
 let mut arithmetic=Arithmetic{operations:0};let f=self.layers.map(|l|l as f64/self.source.adapter.layers as f64);
 let fractions=f.map(|v|arithmetic.exact(v)).into_iter().collect::<CaeResult<Vec<_>>>()?;
 let numerator=arithmetic.sub(fractions[2],fractions[0])?;let denominator=arithmetic.sub(fractions[1],fractions[0])?;let lambda=arithmetic.div(numerator,denominator)?;
 let corners=[[0.,0.],[1.,0.],[0.,1.]];let [a,b]=self.window_corners;let mut uvi=[Interval{lo:0.,hi:0.};2];
 for k in 0..2{let av=arithmetic.exact(corners[a][k])?;let bv=arithmetic.exact(corners[b][k])?;let d=arithmetic.sub(bv,av)?;let p=arithmetic.mul(d,lambda)?;uvi[k]=arithmetic.add(av,p)?;if !admits(uvi[k],uv[k]){return Err(fail("column UV outside counted construction"));}}
 let mut point_bits:[Vec<u64>;3]=std::array::from_fn(|_|Vec::new());let mut reconstructed_bits:[Vec<u64>;3]=std::array::from_fn(|_|Vec::new());let mut bounds:[Vec<f64>;3]=std::array::from_fn(|_|Vec::new());
 for k in 0..3{
 let low=channels(self.lower[k],n)?;let high=channels(self.upper[k],n)?;let point=channels(self.point[k],n)?;let rebuilt=channels(reconstructed[k],n)?;let w=window.vertices.map(|p|channels(p[k],n)).into_iter().collect::<CaeResult<Vec<_>>>()?;
 for j in 0..point.len(){
 let lower=arithmetic.exact(low[j])?;let upper=arithmetic.exact(high[j])?;let source=arithmetic.affine(lower,upper,fractions[2])?;
 let mut endpoints=[arithmetic.exact(w[0][j])?,arithmetic.exact(w[1][j])?,arithmetic.exact(w[2][j])?];
 endpoints[a]=arithmetic.affine(lower,upper,fractions[0])?;endpoints[b]=arithmetic.affine(lower,upper,fractions[1])?;
 if !admits(endpoints[a],w[a][j])||!admits(endpoints[b],w[b][j])||!admits(source,point[j]){return Err(fail("column observed interpolation outside counted interval"));}
 let d0=arithmetic.sub(endpoints[1],endpoints[0])?;let d1=arithmetic.sub(endpoints[2],endpoints[0])?;let p0=arithmetic.mul(d0,uvi[0])?;let p1=arithmetic.mul(d1,uvi[1])?;let p=arithmetic.add(endpoints[0],p0)?;let lift=arithmetic.add(p,p1)?;
 if !admits(lift,rebuilt[j]){return Err(fail("column observed lift outside counted interval"));}
 let error=arithmetic.sub(lift,source)?;
 if error.lo>0.||error.hi<0.{return Err(fail("column affine identity leaves counted interval"));}
 let bound=error.lo.abs().max(error.hi.abs());if !bound.is_finite()||(rebuilt[j]-point[j]).abs()>bound{return Err(fail("column recomposition exceeds counted interval"));}bounds[k].push(bound);
 }
 point_bits[k]=point.iter().map(|v|v.to_bits()).collect();reconstructed_bits[k]=rebuilt.iter().map(|v|v.to_bits()).collect();
 }
 Ok(ColumnRecompositionProof{source:self.source,window:self.window,candidate:self.candidate,corner:self.corner,point_bits,reconstructed_bits,bounds,uv,width:n,operations:arithmetic.operations})
 }
}
