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
use crate::native_geometry_construction_error::{ConstructionScope,JetLayout,TrackedScalar};
pub(crate) struct CoordinateCorrespondence {native_bits:Vec<u64>,mapped_bits:Vec<u64>,bounds:Vec<f64>,width:usize}
fn channels<B:GeometryTimeBase>(q:GeometryTime<B>,width:usize)->CaeResult<Vec<f64>>{
 let mut d=q.value.geometry_first_order_directions().ok_or_else(||fail("corner correspondence value directions opaque"))?;
 let mut t=q.rate.geometry_first_order_directions().ok_or_else(||fail("corner correspondence time directions opaque"))?;
 if d.len()>width||t.len()>width{return Err(fail("corner correspondence direction width differs"));}d.resize(width,0.);t.resize(width,0.);let mut v=vec![q.value.value()];v.extend(d);v.push(q.rate.value());v.extend(t);if v.iter().any(|q|!q.is_finite()){return Err(fail("corner correspondence nonfinite channel"));}Ok(v)
}
fn native_point<S:TransientGasScalar>(source:&NativeSourceFaceWindows<'_,impl GeometryTimeBase>,body:usize,feature:&crate::moving_contact::surface_map::SurfaceFeature,trace:&[S],design:&[S])->CaeResult<[S;3]>{
 use crate::moving_contact::surface_map::SurfaceFeature;
 let owner=source.adapter.owner.capacity_owner();
 let p=|i:usize|std::array::from_fn(|k|trace[owner.nodes[body][i].force_flux[k]]+owner.nodes[body][i].reference_m[k]);
 match *feature {
 SurfaceFeature::Vertex(i)=>Ok(p(i)),
 SurfaceFeature::DensityEdge{nodes,iso,..}=>{
 if nodes[0]>=nodes[1]||iso.to_bits()!=owner.iso.to_bits(){return Err(fail("corner correspondence edge ownership differs"));}
 let phase=|i:usize|{let row=i+if body==0{0}else{owner.nodes[0].len()};let(c,w)=owner.phase.row(row);c.iter().zip(w).map(|(&j,&w)|design[j]*w).fold(S::zero(),|a,b|a+b)};
 let a=phase(nodes[0]);let b=phase(nodes[1]);let t=(S::from_f64(iso)-a)/(b-a);let x=p(nodes[0]);let y=p(nodes[1]);Ok(std::array::from_fn(|k|x[k]+(y[k]-x[k])*t))
 },SurfaceFeature::FixedTetrahedron{..}=>Err(fail("corner correspondence fixed tetra unsupported"))}
}
impl<'g,B:GeometryTimeBase> NativeSourceFaceWindows<'g,B>{
 fn record_corner_correspondence(&self,chart:usize)->CaeResult<()>{
 if self.corner_correspondence.borrow().contains_key(&chart){return Ok(());}
 if !self.trace_recipe_charts.contains(&chart){return Err(fail("corner correspondence trace recipe is not algebraically native"));}
 let mut width=0;for q in self.live_trace.iter().chain(&self.live_design){width=width.max(q.value.geometry_first_order_directions().ok_or_else(||fail("corner correspondence opaque value directions"))?.len()).max(q.rate.geometry_first_order_directions().ok_or_else(||fail("corner correspondence opaque time directions"))?.len());}
 let _scope=ConstructionScope::new(JetLayout::TimeMixed(width))?;
 let t=self.live_trace.iter().copied().map(TrackedScalar::from_control).collect::<Vec<_>>();let d=self.live_design.iter().copied().map(TrackedScalar::from_control).collect::<Vec<_>>();
 let mapped=self.adapter.kinematics.mapped_chart_from_trace(chart,&t,&d,|p,_|Ok(p))?;
 let features=self.adapter.kinematics.chart_features(chart)?;let mut output=Vec::new();
 for body in 0..2{for j in 0..3{let native=native_point(self,body,&features[body][j],&t,&d)?;for k in 0..3{
 let actual_mapped=channels(mapped[body*3+j][k].actual(),width)?;let retained=channels(self.mapped_columns[chart][body*3+j][k],width)?;
 if actual_mapped.iter().zip(&retained).any(|(a,b)|a.to_bits()!=b.to_bits()){return Err(fail("corner correspondence mapped replay differs"));}
 let delta=native[k]-mapped[body*3+j][k];let proof=delta.construction_proof()?;if !proof.certifies_zero(){return Err(fail("corner correspondence algebraic zero exceeds counted construction"));}
 output.push(CoordinateCorrespondence{native_bits:channels(native[k].actual(),width)?.iter().map(|q|q.to_bits()).collect(),mapped_bits:retained.iter().map(|q|q.to_bits()).collect(),bounds:proof.errors(),width});
 }}}
 self.corner_correspondence.borrow_mut().insert(chart,output);Ok(())
 }
 pub(crate) fn validate_corner_correspondence(&self,chart:usize,body:usize,j:usize,point:[GeometryTime<B>;3])->CaeResult<Vec<Vec<f64>>>{
 self.record_corner_correspondence(chart)?;let storage=self.corner_correspondence.borrow();let rows=storage.get(&chart).ok_or_else(||fail("corner correspondence chart absent"))?;let mut out=Vec::new();
 for k in 0..3{let r=&rows[(body*3+j)*3+k];let actual=channels(point[k],r.width)?;if actual.iter().map(|q|q.to_bits()).collect::<Vec<_>>()!=r.native_bits||channels(self.mapped_columns[chart][body*3+j][k],r.width)?.iter().map(|q|q.to_bits()).collect::<Vec<_>>()!=r.mapped_bits{return Err(fail("corner correspondence actual source replay differs"));}out.push(r.bounds.clone());}Ok(out)
 }
}
