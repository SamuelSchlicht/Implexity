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

use crate::native_geometry_construction_error::{ConstructionJet,JetLayout};
use implexity_core::{CaeError,CaeResult};
pub(crate) fn sign(corners:[f64;3],uv:[f64;2],coordinate:f64)->CaeResult<i8>{
 let compute=||->Result<i8,String>{
  let input=|v|ConstructionJet::input(JetLayout::FirstOrder(0),&[v]);
  let o=input(corners[0])?;let c=input(coordinate)?;let a=input(corners[1])?.sub(&o)?;let b=input(corners[2])?.sub(&o)?;
  let r=o.sub(&c)?.add(&a.mul(&input(uv[0])?)?)?.add(&b.mul(&input(uv[1])?)?)?;
  let value=f64::from_bits(r.observed_bits()[0]);let error=r.errors()[0];
  if value>error{Ok(1)}else if value< -error{Ok(-1)}else if value==0.&&error==0.{Ok(0)}else{crate::native_exact_affine_predicate::exact_sign(corners,uv,coordinate).map_err(|e|format!("affine grid exact expansion sign refused after uncertain interval: {e}; corners={corners:?} uv={uv:?} coordinate={coordinate:.17e} residual={value:.17e} error={error:.17e}"))}
 };
 compute().map_err(CaeError::contract)
}

pub(crate) fn barycentric_sign(uv:[f64;2],corner:usize)->CaeResult<i8>{
 let compute=||->Result<i8,String>{
  let input=|v|ConstructionJet::input(JetLayout::FirstOrder(0),&[v]);
  let r=match corner{0=>input(uv[0])?,1=>input(uv[1])?,2=>input(1.)?.sub(&input(uv[0])?)?.sub(&input(uv[1])?)?,_=>return Err("affine barycentric corner is invalid".into())};
  let value=f64::from_bits(r.observed_bits()[0]);let error=r.errors()[0];
  if value>error{Ok(1)}else if value< -error{Ok(-1)}else if value==0.&&error==0.{Ok(0)}else{crate::native_exact_affine_predicate::exact_sign(match corner{0=>[0.,1.,0.],1=>[0.,0.,1.],_=>[1.,0.,0.]},uv,0.).map_err(|e|format!("affine barycentric exact expansion sign refused: {e}; corner={corner} uv={uv:?} residual={value:.17e} error={error:.17e}"))}
 };compute().map_err(CaeError::contract)
}
