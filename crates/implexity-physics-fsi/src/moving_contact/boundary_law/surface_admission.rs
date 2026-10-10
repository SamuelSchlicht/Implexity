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
use super::{exposed_surface::{ExposedSurface,IsoSurfacePolicy},reference_embedding::AuthenticatedReferencePatch,swept_coverage::{SweptCoverage,SweptCandidate,exhaustive_swept_candidates}};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct DensitySurfaceMap{body:usize,reference_nodes:Vec<[f64;3]>,surface:ExposedSurface}
impl DensitySurfaceMap{
 pub fn new(body:usize,reference_nodes:&[[f64;3]],tetrahedra:&[[usize;4]],density:&[f64],policy:IsoSurfacePolicy)->CaeResult<Self>{if body>1{return Err(fail("two-body surface ownership"));}let surface=ExposedSurface::extract(reference_nodes,tetrahedra,density,policy)?;surface.require_closed_surface()?;Ok(Self{body,reference_nodes:reference_nodes.to_vec(),surface})}
 pub fn body(&self)->usize{self.body}
 pub fn surface(&self)->&ExposedSurface{&self.surface}
 pub fn reference_nodes(&self)->&[[f64;3]]{&self.reference_nodes}
}
pub enum SurfaceEvolution<'a>{AuthenticatedFixedReference(&'a AuthenticatedReferencePatch),NativeDensityIsosurface(&'a DensitySurfaceMap)}
impl SurfaceEvolution<'_>{
 pub fn body(&self)->usize{match self{Self::AuthenticatedFixedReference(p)=>p.patch().body(),Self::NativeDensityIsosurface(p)=>p.body}}
 pub fn identity(&self)->String{match self{Self::AuthenticatedFixedReference(p)=>p.identity(),Self::NativeDensityIsosurface(p)=>implexity_core::json::canonical_sha256(&serde_json::json!({"body":p.body,"mesh":p.surface.mesh_identity(),"stratum":p.surface.stratum_identity()}))}}
 pub fn facets(&self)->&[[usize;3]]{match self{Self::AuthenticatedFixedReference(p)=>p.patch().facets(),Self::NativeDensityIsosurface(p)=>p.surface.facets()}}
 pub fn positions(&self,displacement:&[[f64;3]],density:&[f64])->CaeResult<Vec<[f64;3]>>{match self{Self::AuthenticatedFixedReference(p)=>{if displacement.len()!=p.nodes().len()||density.len()!=displacement.len()||displacement.iter().flatten().chain(density).any(|x|!x.is_finite()){return Err(fail("fixed-reference native displacement/density shape"));}p.patch().maps(density)?.iter().enumerate().map(|(i,map)|super::reference_embedding::position(map,p.reference(i)?,displacement)).collect()},Self::NativeDensityIsosurface(p)=>{if displacement.len()!=p.reference_nodes.len()||displacement.iter().flatten().any(|x|!x.is_finite()){return Err(fail("density-surface native displacement shape"));}let points:Vec<_>=p.reference_nodes.iter().zip(displacement).map(|(x,u)|std::array::from_fn(|k|x[k]+u[k])).collect();p.surface.positions(&points,density)}}}
 pub fn require_surface_design_direction(&self,density:&[f64])->CaeResult<()>{match self{Self::AuthenticatedFixedReference(p)=>p.require_shape_derivative(),Self::NativeDensityIsosurface(p)=>p.surface.require_fixed_stratum(density)}}
}
pub struct GlobalSurfaceAdmission{report:SweptCoverage,surface_identities:[String;2],density_identity:String}
impl GlobalSurfaceAdmission{
 pub fn check(surfaces:[SurfaceEvolution<'_>;2],old_displacements:[&[[f64;3]];2],new_displacements:[&[[f64;3]];2],density:[&[f64];2],maximum_pairs:usize)->CaeResult<Self>{if surfaces[0].body()!=0||surfaces[1].body()!=1{return Err(fail("ordered complete two-body surface ownership"));}let old=[surfaces[0].positions(old_displacements[0],density[0])?,surfaces[1].positions(old_displacements[1],density[1])?];let new=[surfaces[0].positions(new_displacements[0],density[0])?,surfaces[1].positions(new_displacements[1],density[1])?];let report=exhaustive_swept_candidates([&old[0],&old[1]],[&new[0],&new[1]],[surfaces[0].facets(),surfaces[1].facets()],maximum_pairs)?;Ok(Self{report,surface_identities:[surfaces[0].identity(),surfaces[1].identity()],density_identity:implexity_core::json::canonical_sha256(&serde_json::json!(density))})}
 pub fn unresolved_candidates(&self)->&[SweptCandidate]{&self.report.unresolved}
 pub fn require_separated_phase(&self)->CaeResult<()>{self.report.require_swept_boundary_separated()}
 pub fn require_surface_design_direction(&self,surfaces:[SurfaceEvolution<'_>;2],density:[&[f64];2])->CaeResult<()>{if self.surface_identities!=[surfaces[0].identity(),surfaces[1].identity()]||self.density_identity!=implexity_core::json::canonical_sha256(&serde_json::json!(density)){return Err(fail("surface design admission source/stratum/density identity"));}for b in 0..2{surfaces[b].require_surface_design_direction(density[b])?;}self.require_separated_phase()}
 pub fn identity(&self)->String{implexity_core::json::canonical_sha256(&serde_json::json!({"surfaces":self.surface_identities,"density":self.density_identity,"endpoints":self.report.endpoint_identity}))}
}
