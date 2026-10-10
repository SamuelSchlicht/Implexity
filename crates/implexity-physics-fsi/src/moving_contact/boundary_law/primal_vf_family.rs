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
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::{matrix::Jacobian,time_stepper::StepParameters};
use crate::moving_contact::{contact_field::{NativeContactLaw,ContactContribution},contact_set_kinematics::ContactFeatureKinematics,linear_path::ApproximatePathCertificate};
use crate::moving_contact::boundary_law::{boundary_mapped_pair_law::{BoundaryMappedPairLaw,NodeBinding},reference_embedding::AuthenticatedReferencePatch};
fn fail(s:&str)->CaeError{CaeError::contract(s)}
pub struct PrimalVfFamily{candidates:Vec<(usize,BoundaryMappedPairLaw)>,native:usize,identity:String,physical_identity:String,unique_endpoint_normal:bool}
impl PrimalVfFamily{
 pub fn authenticated(nodes:[Vec<NodeBinding>;2],phase:CsrMatrix,native:usize,fluxes:usize,gap_scale:f64,force_scale:f64,source:&AuthenticatedReferencePatch,target:&AuthenticatedReferencePatch,source_vertex:usize,target_vertex:usize)->CaeResult<Self>{
  let mut candidates=vec![];for &facet in target.patch().incident_facets(target_vertex)?{candidates.push((facet,BoundaryMappedPairLaw::from_authenticated_patches(nodes.clone(),phase.clone(),native,fluxes,gap_scale,force_scale,source,target,source_vertex,target_vertex,facet)?));}
  if candidates.is_empty(){return Err(fail("surface contact family has no incident native feature"));}let identity=implexity_core::json::canonical_sha256(&serde_json::json!({"source":source.identity(),"target":target.identity(),"source_vertex":source_vertex,"target_vertex":target_vertex,"candidates":candidates.iter().map(|(id,l)|(*id,l.geometry_identity())).collect::<Vec<_>>(),"gap_scale_m":gap_scale,"force_scale_n":force_scale,"scope":"solver-only native incident VF family; unsupported edge/corner transition remains refusal"}));let physical_identity=implexity_core::json::canonical_sha256(&serde_json::json!({"source_patch":source.identity(),"target_patch":target.identity(),"source_vertex":source_vertex,"target_vertex":target_vertex,"kind":"authenticated native incident VF physical family"}));Ok(Self{candidates,native,identity,physical_identity,unique_endpoint_normal:false})
 }
 pub fn normal_cone_selection_record(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<serde_json::Value>{let i=self.choose(n,c,o,p)?;let law=&self.candidates[i].1;let previous=law.oriented_normal_cone_certificate(o,p.design)?;let current=law.oriented_normal_cone_certificate(c,p.design)?;Ok(serde_json::json!({"selected_owner":self.selection_record(n,c,o,p)?,"previous_normal_cone":previous.identity(),"current_normal_cone":current.identity(),"previous_native_axis":previous.axis(),"current_native_axis":current.axis(),"scope":"endpoint oriented cone uniqueness only; no swept uniqueness, feature-event or ordinary derivative admission"}))}
 pub fn with_unique_endpoint_normals(mut self)->Self{self.identity=implexity_core::json::canonical_sha256(&serde_json::json!({"family":self.identity,"admission":"unique combined authenticated endpoint supporting normal cones; swept cone uniqueness and feature event not granted"}));self.unique_endpoint_normal=true;self}
 pub fn identity(&self)->&str{&self.identity}
 pub fn physical_family_identity(&self)->&str{&self.physical_identity}
 fn choose(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<usize>{
  if current.len()!=self.native+1||previous.len()!=self.native+1||current.iter().chain(previous).any(|x|!x.is_finite())||p.design.iter().any(|x|!x.is_finite())||!p.time_scale.is_finite()||p.time_scale<=0.{return Err(fail("contact feature family complete state/parameter domain"));}let mut c=current.to_vec();let mut o=previous.to_vec();c[self.native]=0.;o[self.native]=0.;let mut choices=vec![];let mut minimum_old=f64::INFINITY;
  for (k,(_,law))in self.candidates.iter().enumerate(){if law.check_state_domain(n,&o,&o,p).is_ok(){let old=law.gap_current_matrix(&o,p.design)?.0;if !old.is_finite(){return Err(fail("incident feature previous gap is nonfinite"));}minimum_old=minimum_old.min(old.abs());}if law.check_state_domain(n,&c,&c,p).is_err(){continue;}let g=law.gap_current_matrix(&c,p.design)?.0;if g.is_finite(){choices.push((k,g.abs()));}}
  choices.sort_by(|a,b|a.1.total_cmp(&b.1).then_with(||self.candidates[a.0].0.cmp(&self.candidates[b.0].0)));let k=choices.first().ok_or_else(||fail("state-local incident VF cone has no admissible owner; EE/perimeter event required"))?.0;let law=&self.candidates[k].1;let old=law.gap_current_matrix(&o,p.design)?.0;if old.abs()!=minimum_old||law.check_state_domain(n,&c,&o,p).is_err(){return Err(fail("state-local VF owner lacks same-reference-gap certified history; feature event required"));}if self.unique_endpoint_normal{law.oriented_normal_cone_certificate(&o,p.design)?;law.oriented_normal_cone_certificate(&c,p.design)?;}Ok(k)
 }
 pub fn selection_record(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<serde_json::Value>{let i=self.choose(n,c,o,p)?;Ok(serde_json::json!({"family":self.identity,"facet":self.candidates[i].0,"geometry":self.candidates[i].1.geometry_identity(),"previous_bits":o.iter().map(|x|x.to_bits()).collect::<Vec<_>>(),"current_bits":c.iter().map(|x|x.to_bits()).collect::<Vec<_>>(),"design_bits":p.design.iter().map(|x|x.to_bits()).collect::<Vec<_>>(),"time_scale_bits":p.time_scale.to_bits(),"derivative_scope":"solver-only selected native feature; no ordinary contact-window gradient"}))}
 fn at_state(&self,x:&[f64],d:&[f64])->CaeResult<&BoundaryMappedPairLaw>{let i=self.choose(1,x,x,StepParameters{design:d,time_scale:1.})?;Ok(&self.candidates[i].1)}
}
impl NativeContactLaw for PrimalVfFamily{
 fn multipliers(&self)->usize{1}
 fn evaluate(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<ContactContribution>{self.candidates[self.choose(n,c,o,p)?].1.evaluate(n,c,o,p)}
 fn check_state_domain(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.candidates[self.choose(n,c,o,p)?].1.check_state_domain(n,c,o,p)}
 fn check_derivative_domain(&self,_n:usize,_c:&[f64],_o:&[f64],_p:StepParameters<'_>)->CaeResult<()>{Err(fail("feature family Newton rows are solver-only; ordinary selection derivative is not qualified"))}
 fn newton_constraints(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>,d:&[f64],attempt:usize)->CaeResult<Option<Jacobian>>{self.candidates[self.choose(n,c,o,p)?].1.newton_constraints(n,c,o,p,d,attempt)}
 fn admitted_trial(&self,n:usize,c:&[f64],d:&[f64],o:&[f64],p:StepParameters<'_>,trial:f64)->CaeResult<f64>{if c.len()!=d.len()||!trial.is_finite()||trial<=0.||trial>1.{return Err(fail("feature family trial domain"));}let mut best=0.0f64;for (_,law)in &self.candidates{if let Ok(f)=law.admitted_trial(n,c,d,o,p,trial){if !f.is_finite()||f<=0.||f>trial{return Err(fail("feature candidate invalid fraction"));}let candidate:Vec<_>=c.iter().zip(d).map(|(x,v)|x+f*v).collect();if self.check_state_domain(n,&candidate,o,p).is_ok(){best=best.max(f);}}}if best==0.{Err(fail("no physical feature family candidate admits Newton path"))}else{Ok(best)}}
}
impl ContactFeatureKinematics for PrimalVfFamily{
 fn check_selected_geometry_domain(&self,n:usize,c:&[f64],o:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.check_derivative_domain(n,c,o,p)}
 fn gap_current_matrix(&self,x:&[f64],d:&[f64])->CaeResult<(f64,Jacobian)>{self.at_state(x,d)?.gap_current_matrix(x,d)}
 fn gap_design_matrix(&self,x:&[f64],d:&[f64])->CaeResult<Jacobian>{self.at_state(x,d)?.gap_design_matrix(x,d)}
 fn instantaneous_row(&self,x:&[f64],d:&[f64])->CaeResult<(Vec<f64>,f64)>{self.at_state(x,d)?.instantaneous_row(x,d)}
 fn residual_path_certificate(&self,o:&[f64],c:&[f64],d:&[f64],tol:f64)->CaeResult<ApproximatePathCertificate>{let p=StepParameters{design:d,time_scale:1.};let i=self.choose(1,c,o,p)?;let v=self.candidates[i].1.residual_path_certificate(o,c,d,tol)?;Ok(ApproximatePathCertificate{previous_positions:v.previous_positions,current_positions:v.current_positions,lower_gap_bound_m:v.lower_gap_bound_m,allowance_m:v.allowance_m,worst_allowance_ratio:v.worst_allowance_ratio,intervals_examined:v.intervals_examined})}
 fn fixed_reference_geometry(&self)->bool{true}
}

pub fn authenticated_native_shell_contact(binding:&crate::moving_contact::native_surface_binding::NativeNodalBinding,patches:[&AuthenticatedReferencePatch;2],feature_pairs:&[[usize;2]],design:&[f64],initial:&[f64],gap_scale:f64,force_scale:f64,initial_plane:super::plane_manifold::SupportingPlaneSelection)->CaeResult<super::complete_shell_path::CompleteShellContact<crate::moving_contact::collection::MultipleContact<PrimalVfFamily>>>{
 binding.require_design(design)?;if feature_pairs.is_empty()||patches[0].patch().body()!=0||patches[1].patch().body()!=1{return Err(fail("native full shell body/feature ownership"));}let mut unique=std::collections::BTreeSet::new();if feature_pairs.iter().any(|p|!unique.insert(*p)){return Err(fail("duplicate native shell physical feature pair"));}
 let nodes=binding.boundary_nodes();let phase=binding.phase().clone();let native=binding.state_size();let fluxes=binding.flux_size();let surface=super::plane_manifold::CompleteSurfaceTrace::authenticated(nodes.clone(),phase.clone(),native,patches)?;
 let laws=feature_pairs.iter().map(|p|PrimalVfFamily::authenticated(nodes.clone(),phase.clone(),native,fluxes,gap_scale,force_scale,patches[1],patches[0],p[1],p[0])).collect::<CaeResult<Vec<_>>>()?;
 let law=crate::moving_contact::collection::MultipleContact::new(laws,native,fluxes,phase.ncols())?;
 super::complete_shell_path::CompleteShellContact::new(law,surface,initial,design,initial_plane)
}
