// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
