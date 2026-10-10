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

use implexity_core::{CaeError,CaeResult};
use implexity_ad::Dual;
use implexity_linalg::{sparse::CsrMatrix,lu::{SparseLu,LuSymbolic,Parallelism}};
use crate::{model::FsiModel,moving_contact::{separate_body::PairSolidField,impact::MassMetric}};
use implexity_solve::multirate_coupling::FluxDrivenField;
fn fail(e:impl std::fmt::Display)->CaeError{CaeError::contract(e.to_string())}
pub struct BorrowedPhysicalMass{matrix:CsrMatrix,fixed:Vec<bool>,free:Vec<usize>,factor:SparseLu}
impl BorrowedPhysicalMass{pub fn matrix(&self)->&CsrMatrix{&self.matrix}pub fn new(matrix:CsrMatrix,fixed:Vec<bool>)->CaeResult<Self>{if matrix.nrows()!=fixed.len()||matrix.ncols()!=fixed.len(){return Err(fail("physical mass shape"));}let free:Vec<_>=(0..fixed.len()).filter(|i|!fixed[*i]).collect();let mut map=vec![usize::MAX;fixed.len()];for(i,&j)in free.iter().enumerate(){map[j]=i;}let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);for(i,&r)in free.iter().enumerate(){let(js,v)=matrix.row(r);for(&c,&x)in js.iter().zip(v){if !x.is_finite(){return Err(fail("physical mass nonfinite"));}if map[c]!=usize::MAX{ri.push(i);ci.push(map[c]);vs.push(x);}}}let sub=CsrMatrix::from_triplets(free.len(),free.len(),&ri,&ci,&vs).map_err(fail)?.to_csc();let factor=LuSymbolic::analyze(&sub).and_then(|s|s.factor(&sub,Parallelism::Sequential)).map_err(fail)?;Ok(Self{matrix,fixed,free,factor})}}
impl MassMetric for BorrowedPhysicalMass{fn size(&self)->usize{self.fixed.len()}fn fixed(&self)->&[bool]{&self.fixed}fn apply(&self,v:&[f64])->CaeResult<Vec<f64>>{self.matrix.matvec(v).map_err(fail)}fn inverse_free(&self,f:&[f64])->CaeResult<Vec<f64>>{if f.len()!=self.size()||f.iter().any(|v|!v.is_finite()){return Err(fail("physical inverse mass input"));}let rhs:Vec<_>=self.free.iter().map(|i|f[*i]).collect();let x=self.factor.solve(&rhs).map_err(fail)?;let mut out=vec![0.;self.size()];for(&i,&v)in self.free.iter().zip(&x){out[i]=v;}Ok(out)}}
pub fn physical_mass_matrix(models:[&FsiModel;2],field:&PairSolidField<'_>,design:&[f64],direction:Option<&[f64]>)->CaeResult<(CsrMatrix,Vec<bool>)>{
if design.len()!=field.design_size()||direction.is_some_and(|d|d.len()!=design.len())||design.iter().chain(direction.into_iter().flatten()).any(|v|!v.is_finite()){return Err(fail("borrowed physical mass design shape"));}let mut matrices=vec![];let mut fixed=vec![];let mut start=0;for b in 0..2{let native=models[b];let body=field.body_field(b)?;let core=body.core();if !std::ptr::eq(core.model(),&native.soft){return Err(fail("borrowed physical mass body identity"));}let end=start+body.design_size();let parameters=core.params(&design[start..end])?;let soft=core.model();let removed=native.problem.solid.inertia_compensation*native.problem.fluid.density_kg_m3;let ratio:Vec<_>=(0..soft.ne()).map(|e|{let rho=soft.materials[soft.element_material[e]].density;(rho+removed)/rho}).collect();if ratio.iter().any(|r|!r.is_finite()||*r<1.){return Err(fail("two-body physical mass compensation domain"));}let factors=if let Some(d)=direction{let dp=core.params(&d[start..end])?;(0..soft.ne()).map(|i|soft.interpolation.mass(Dual::new(parameters[i],[dp[i]])).eps[0]*ratio[i]).collect::<Vec<_>>()}else{(0..soft.ne()).map(|i|soft.interpolation.mass(parameters[i])*ratio[i]).collect::<Vec<_>>()};matrices.push(soft.global_mass_and_reference(&factors,&vec![0.;soft.ne()])?.0);fixed.extend(&soft.fixed);start=end;}
let(mut ri,mut ci,mut vs)=(vec![],vec![],vec![]);for (block,offset)in [(&matrices[0],0),(&matrices[1],matrices[0].nrows())]{for i in 0..block.nrows(){let(js,values)=block.row(i);for(&j,&v)in js.iter().zip(values){ri.push(offset+i);ci.push(offset+j);vs.push(v);}}}Ok((CsrMatrix::from_triplets(fixed.len(),fixed.len(),&ri,&ci,&vs).map_err(fail)?,fixed))}
pub fn physical_mass(models:[&FsiModel;2],field:&PairSolidField<'_>,design:&[f64])->CaeResult<BorrowedPhysicalMass>{let(m,f)=physical_mass_matrix(models,field,design,None)?;BorrowedPhysicalMass::new(m,f)}
