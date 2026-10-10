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
#[derive(Default)]
pub(crate) struct GeometryDirections{slots:Vec<usize>,values:Vec<f64>,dimension:usize}
impl GeometryDirections{
 pub(crate) fn reserve_nodes(&mut self,len:usize){self.slots.resize(len,usize::MAX)}
 pub(crate) fn set_dimension(&mut self,dimension:usize){self.dimension=dimension}
 pub(crate) fn get(&self,node:usize)->Option<&[f64]>{let start=*self.slots.get(node)?;if start==usize::MAX{return None}self.values.get(start..start.checked_add(self.dimension)?)}
 pub(crate) fn store(&mut self,node:usize,values:&[f64])->bool{if values.len()!=self.dimension{return false}if self.slots.len()<=node{self.reserve_nodes(node+1)}let start=self.values.len();self.values.extend_from_slice(values);self.slots[node]=start;true}
}
#[derive(Default)]
pub(crate) struct GeometryEdges{offsets:Vec<usize>,edges:Vec<(usize,f64)>}
impl GeometryEdges{
 pub(crate) fn ensure_node(&mut self,node:usize){if self.offsets.is_empty(){self.offsets.push(0)}while self.offsets.len()<=node+1{self.offsets.push(self.edges.len());}}
 pub(crate) fn append(&mut self,node:usize,edges:impl IntoIterator<Item=(usize,f64)>)->bool{if self.offsets.is_empty(){self.offsets.push(0)}while self.offsets.len()<=node{self.offsets.push(self.edges.len());}if self.offsets.len()!=node+1{return false}self.edges.extend(edges);self.offsets.push(self.edges.len());true}
 pub(crate) fn get(&self,node:usize)->Option<&[(usize,f64)]>{let start=*self.offsets.get(node)?;let end=*self.offsets.get(node.checked_add(1)?)?;self.edges.get(start..end)}
}
