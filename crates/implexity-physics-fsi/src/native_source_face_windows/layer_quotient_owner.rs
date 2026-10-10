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

use super::{GeometryTime,GeometryTimeBase,TransientGasScalar,fail};
use super::column_uv::roundoff::{Arithmetic,Interval,admits};
use crate::gas_layered_tetra_mesh::LayeredGasMesh;
use implexity_ad::Scalar;
use implexity_core::CaeResult;
use std::collections::BTreeMap;
#[derive(Clone)]
pub(super) struct LayerQuotientReceipt{layer:usize,layers:usize,observed_bits:u64}
impl LayerQuotientReceipt{
 pub(super) fn layer(&self)->usize{self.layer}
 pub(super) fn layers(&self)->usize{self.layers}
 pub(super) fn observed(&self)->f64{f64::from_bits(self.observed_bits)}
 pub(super) fn counted_interval(&self,arithmetic:&mut Arithmetic)->CaeResult<Interval>{
  let numerator=arithmetic.exact(self.layer as f64)?;let denominator=arithmetic.exact(self.layers as f64)?;let fraction=arithmetic.div(numerator,denominator)?;
  if !admits(fraction,self.observed()){return Err(fail("producer layer quotient leaves counted division interval"));}Ok(fraction)
 }
}
struct NodeReceipt{quotient:LayerQuotientReceipt,point_bits:[Vec<u64>;3]}
pub(super) struct LayerQuotientOwner{nodes:BTreeMap<(usize,usize),NodeReceipt>}
fn bits<S:TransientGasScalar>(point:[S;3])->CaeResult<[Vec<u64>;3]>{
 point.map(|v|{if !v.finite_first_order(){return Err(fail("producer layer node is nonfinite"));}let directions=v.geometry_first_order_directions().ok_or_else(||fail("producer layer node channels are opaque"))?;if directions.iter().any(|d|!d.is_finite()){return Err(fail("producer layer node directions are nonfinite"));}Ok(std::iter::once(v.value().to_bits()).chain(directions.into_iter().map(f64::to_bits)).collect::<Vec<_>>())}).into_iter().collect::<CaeResult<Vec<_>>>()?.try_into().map_err(|_|fail("producer layer point width differs"))
}
impl LayerQuotientOwner{
 pub(super) fn issue<S:GeometryTimeBase>(mesh:&LayeredGasMesh<GeometryTime<S>>,mapped:&[[[GeometryTime<S>;3];6]],paired:&[[usize;3]],layers:usize)->CaeResult<Self>{
  if layers==0||layers as u128>(1u128<<53)||mapped.len()!=paired.len(){return Err(fail("producer layer owner metadata differs"));}
  let mut nodes=BTreeMap::new();
  for node in &mesh.nodes{
   if node.layer>layers{return Err(fail("producer actual node layer exceeds owned mesh"));}
   let fraction=node.layer as f64/layers as f64;let actual=bits(node.start_m)?;
   if actual!=bits(node.end_m)?{return Err(fail("producer stage endpoint layer nodes differ"));}
   let mut authenticated=false;
   for(chart,ids)in paired.iter().enumerate(){for corner in 0..3{if ids[corner]!=node.paired_vertex{continue;}
    let point=std::array::from_fn(|k|mapped[chart][corner][k]+(mapped[chart][corner+3][k]-mapped[chart][corner][k])*fraction);
    if bits(point)?!=actual{return Err(fail("producer paired chart layer node reconstruction differs"));}authenticated=true;
   }}
   if !authenticated{return Err(fail("producer layer node has no actual paired chart owner"));}
   let receipt=NodeReceipt{quotient:LayerQuotientReceipt{layer:node.layer,layers,observed_bits:fraction.to_bits()},point_bits:actual};
   if nodes.insert((node.paired_vertex,node.layer),receipt).is_some(){return Err(fail("producer layer node identity is duplicated"));}
  }Ok(Self{nodes})
 }
 pub(super) fn receipt<S:TransientGasScalar>(&self,pair:usize,layer:usize,actual:[S;3])->CaeResult<LayerQuotientReceipt>{
  let node=self.nodes.get(&(pair,layer)).ok_or_else(||fail("producer actual layer node is absent"))?;
  if bits(actual)?!=node.point_bits{return Err(fail("producer layer node full scalar binding differs"));}Ok(node.quotient.clone())
 }
}
