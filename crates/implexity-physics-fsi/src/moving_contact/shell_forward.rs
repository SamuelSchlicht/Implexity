// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::CaeResult;
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::multirate_coupling::FluxDrivenField;
use super::{separate_body::MovingFsiPairModel,resources::{PairNativeResources,PersistentContactFields},native_surface_binding::NativeNodalBinding,event_step::EventAdvancePolicy,contact_set::native_velocity_trace_from_pair,boundary_law::{reference_embedding::AuthenticatedReferencePatch,plane_manifold::SupportingPlaneSelection,complete_shell_path::CompleteShellContact,primal_vf_family::{PrimalVfFamily,authenticated_native_shell_contact}},collection::MultipleContact};
pub type AuthenticatedShellContact=CompleteShellContact<MultipleContact<PrimalVfFamily>>;
pub struct PreparedShellForward<'a>{pub fields:PersistentContactFields<'a,AuthenticatedShellContact>,pub physical_design:Vec<f64>,pub physical_velocity:CsrMatrix,pub feature_identity:serde_json::Value}
pub fn prepare_authenticated_shell_forward<'a>(model:&'a MovingFsiPairModel,controls:[&[f64];2],event:EventAdvancePolicy,patches:[&AuthenticatedReferencePatch;2],feature_pairs:&[[usize;2]],gap_scale_m:f64,force_scale_n:f64,initial_plane:SupportingPlaneSelection)->CaeResult<PreparedShellForward<'a>>{
 let physical_design=model.event_physical_design(controls)?;let resources=PairNativeResources::new(model,event)?;let binding=NativeNodalBinding::new(model,&resources.solid,&physical_design)?;let mut initial=resources.solid.initial_state(&physical_design)?;initial.extend(vec![0.;feature_pairs.len()]);let physical_velocity=native_velocity_trace_from_pair(model,&resources.solid,&physical_design,1.,feature_pairs.len())?;let law=authenticated_native_shell_contact(&binding,patches,feature_pairs,&physical_design,&initial,gap_scale_m,force_scale_n,initial_plane)?;let feature_identity=serde_json::json!({"schema":"native-authenticated-shell-forward-family/1","model_identity":model.identity(),"native_binding":binding.identity(),"shells":[patches[0].identity(),patches[1].identity()],"feature_pairs":feature_pairs,"gap_scale_m":gap_scale_m,"force_scale_n":force_scale_n,"scope":"complete sampled shell separation guard and incident VF families; unsupported EE and feature-event transitions refuse; no ordinary closed gradient"});let fields=resources.into_contact(law)?;Ok(PreparedShellForward{fields,physical_design,physical_velocity,feature_identity})
}
