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
use implexity_solve::{matrix::Jacobian,time_stepper::StepParameters};
use super::{contact_field::NativeContactLaw,linear_path::ApproximatePathCertificate};
pub trait ContactSetKinematics:NativeContactLaw{
 fn native_states(&self)->usize;
 fn check_selected_geometry_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>;
 fn contact_count(&self)->usize{self.multipliers()}
 fn scaled_gap_rows(&self,state:&[f64],design:&[f64])->CaeResult<(Vec<f64>,Jacobian)>;
 fn scaled_gap_design_rows(&self,state:&[f64],design:&[f64])->CaeResult<Jacobian>;
 fn inverse_force_scales(&self,state:&[f64],p:StepParameters<'_>)->CaeResult<Vec<f64>>;
 fn normal_velocities(&self,state:&[f64],design:&[f64],velocity:&[f64])->CaeResult<Vec<f64>>;
 fn instantaneous_rows(&self,state:&[f64],design:&[f64])->CaeResult<(Vec<Vec<f64>>,Vec<f64>)>;
 fn residual_path_certificates(&self,previous:&[f64],current:&[f64],design:&[f64],tolerance:f64)->CaeResult<Vec<ApproximatePathCertificate>>;
 fn fixed_reference_geometry(&self)->bool;
 fn bound_owner_features(&self,features:&serde_json::Value)->CaeResult<serde_json::Value>{Ok(features.clone())}
}
pub trait ContactFeatureKinematics:NativeContactLaw{
 fn check_selected_geometry_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>;
 fn gap_current_matrix(&self,state:&[f64],design:&[f64])->CaeResult<(f64,Jacobian)>;
 fn gap_design_matrix(&self,state:&[f64],design:&[f64])->CaeResult<Jacobian>;
 fn instantaneous_row(&self,state:&[f64],design:&[f64])->CaeResult<(Vec<f64>,f64)>;
 fn residual_path_certificate(&self,previous:&[f64],current:&[f64],design:&[f64],tolerance:f64)->CaeResult<ApproximatePathCertificate>;
 fn fixed_reference_geometry(&self)->bool;
}
impl ContactFeatureKinematics for super::mapped_pair_law::MappedPairLaw{
 fn check_selected_geometry_domain(&self,_n:usize,_current:&[f64],_previous:&[f64],_p:StepParameters<'_>)->CaeResult<()>{Ok(())}
 fn gap_current_matrix(&self,x:&[f64],d:&[f64])->CaeResult<(f64,Jacobian)>{self.gap_current_matrix(x,d)}
 fn gap_design_matrix(&self,x:&[f64],d:&[f64])->CaeResult<Jacobian>{self.gap_design_matrix(x,d)}
 fn instantaneous_row(&self,x:&[f64],d:&[f64])->CaeResult<(Vec<f64>,f64)>{self.instantaneous_row(x,d)}
 fn residual_path_certificate(&self,a:&[f64],b:&[f64],d:&[f64],t:f64)->CaeResult<ApproximatePathCertificate>{self.residual_path_certificate(a,b,d,t)}
 fn fixed_reference_geometry(&self)->bool{self.fixed_reference_geometry()}
}
impl ContactFeatureKinematics for super::boundary_law::boundary_mapped_pair_law::BoundaryMappedPairLaw{
 fn check_selected_geometry_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.check_derivative_domain(n,current,previous,p)}
 fn gap_current_matrix(&self,x:&[f64],d:&[f64])->CaeResult<(f64,Jacobian)>{self.gap_current_matrix(x,d)}
 fn gap_design_matrix(&self,x:&[f64],d:&[f64])->CaeResult<Jacobian>{self.gap_design_matrix(x,d)}
 fn instantaneous_row(&self,x:&[f64],d:&[f64])->CaeResult<(Vec<f64>,f64)>{self.instantaneous_row(x,d)}
 fn residual_path_certificate(&self,a:&[f64],b:&[f64],d:&[f64],t:f64)->CaeResult<ApproximatePathCertificate>{let c=self.residual_path_certificate(a,b,d,t)?;Ok(ApproximatePathCertificate{previous_positions:c.previous_positions,current_positions:c.current_positions,lower_gap_bound_m:c.lower_gap_bound_m,allowance_m:c.allowance_m,worst_allowance_ratio:c.worst_allowance_ratio,intervals_examined:c.intervals_examined})}
 fn fixed_reference_geometry(&self)->bool{self.fixed_reference_geometry()}
}
impl<L:ContactSetKinematics> ContactSetKinematics for super::boundary_law::plane_manifold::PlaneCertifiedContact<L>{
 fn native_states(&self)->usize{self.law().native_states()}
 fn check_selected_geometry_domain(&self,n:usize,current:&[f64],previous:&[f64],p:StepParameters<'_>)->CaeResult<()>{self.law().check_selected_geometry_domain(n,current,previous,p)?;if !self.certificate(current,previous,p.design)?.unresolved_candidates().is_empty(){return Err(CaeError::contract("closed supporting-plane selection requires a qualified directional KKT owner"));}Ok(())}
 fn scaled_gap_rows(&self,x:&[f64],d:&[f64])->CaeResult<(Vec<f64>,Jacobian)>{self.law().scaled_gap_rows(x,d)}
 fn scaled_gap_design_rows(&self,x:&[f64],d:&[f64])->CaeResult<Jacobian>{self.law().scaled_gap_design_rows(x,d)}
 fn inverse_force_scales(&self,x:&[f64],p:StepParameters<'_>)->CaeResult<Vec<f64>>{self.law().inverse_force_scales(x,p)}
 fn normal_velocities(&self,x:&[f64],d:&[f64],v:&[f64])->CaeResult<Vec<f64>>{self.law().normal_velocities(x,d,v)}
 fn instantaneous_rows(&self,x:&[f64],d:&[f64])->CaeResult<(Vec<Vec<f64>>,Vec<f64>)>{self.law().instantaneous_rows(x,d)}
 fn residual_path_certificates(&self,a:&[f64],b:&[f64],d:&[f64],t:f64)->CaeResult<Vec<ApproximatePathCertificate>>{self.certificate(b,a,d)?;self.law().residual_path_certificates(a,b,d,t)}
 fn fixed_reference_geometry(&self)->bool{self.law().fixed_reference_geometry()}
 fn bound_owner_features(&self,f:&serde_json::Value)->CaeResult<serde_json::Value>{Ok(serde_json::json!({"inner":self.law().bound_owner_features(f)?,"complete_surface_guard":self.kinematics_owner()}))}
}
