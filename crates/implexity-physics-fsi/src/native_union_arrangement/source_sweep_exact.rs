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
 let mut q=value;let mut next=Vec::with_capacity(expansion.len()+1);for &e in expansion.iter(){let [lo,hi]=two_sum(q,e)?;if lo!=0.{next.push(lo);}q=hi;}if q!=0.||next.is_empty(){next.push(q);}if next.len()>128{return Err("exact affine expansion capacity");}*expansion=next;Ok(())
}
pub(crate) struct ExactSweepPoint { frame:[f64;2], transformed:[f64;2], determinant:f64, emitted:[f64;2] }
impl ExactSweepPoint {
 fn from_actual_inverse(frame:[f64;2], transformed:[f64;2], emitted:[f64;2])->Result<Self,&'static str>{
  if frame.iter().chain(transformed.iter()).chain(emitted.iter()).any(|x|!x.is_finite()||(*x!=0.&&!x.is_normal())){return Err("source sweep input domain");}
  let[c,s]=frame;let[x,y]=transformed;let determinant=c*c+s*s;
  if !determinant.is_normal()||determinant<=0.{return Err("source sweep determinant domain");}
  let actual=[(x*c-y*s)/determinant,(x*s+y*c)/determinant];
  if actual.map(f64::to_bits)!=emitted.map(f64::to_bits){return Err("source sweep inverse output differs");}
  Ok(Self{frame,transformed,determinant,emitted})
 }
 fn numerator(&self,axis:usize)->Result<Vec<f64>,&'static str>{
  let[c,s]=self.frame;let[x,y]=self.transformed;let terms=if axis==0{[(x,c),(y,-s)]}else{[(x,s),(y,c)]};
  let mut out=Vec::new();for(a,b)in terms{for v in two_product(a,b)?{grow(&mut out,v)?;}}Ok(out)
 }
 fn barycentric_sign(&self,corner:usize)->Result<i8,&'static str>{
  let mut r=match corner{1=>self.numerator(0)?,2=>self.numerator(1)?,0=>{let mut e=vec![self.determinant];for k in 0..2{for t in self.numerator(k)?{grow(&mut e,-t)?;}}e},_=>return Err("source sweep corner domain")};
  sign(&mut r)
 }
 fn affine_sign(&self,corners:[f64;3],coordinate:f64)->Result<i8,&'static str>{
  if corners.iter().chain(std::iter::once(&coordinate)).any(|x|!x.is_finite()||(*x!=0.&&!x.is_normal())){return Err("source sweep affine domain");}
  let mut r=Vec::new();for coeff in two_sum(corners[0],-coordinate)?{for term in two_product(coeff,self.determinant)?{grow(&mut r,term)?;}}
  for k in 0..2{let numerator=self.numerator(k)?;for coeff in two_sum(corners[k+1],-corners[0])?{for &factor in &numerator{for term in two_product(coeff,factor)?{grow(&mut r,term)?;}}}}
  sign(&mut r)
 }
 fn emitted(&self)->[f64;2]{self.emitted}
}
fn sign(r:&mut Vec<f64>)->Result<i8,&'static str>{Ok(r.iter().rev().find(|x|**x!=0.).map_or(0,|x|if *x>0.{1}else{-1}))}
