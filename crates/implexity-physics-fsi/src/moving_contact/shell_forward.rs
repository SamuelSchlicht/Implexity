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

use implexity_core::CaeResult;
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::multirate_coupling::FluxDrivenField;
use super::{separate_body::MovingFsiPairModel,resources::{PairNativeResources,PersistentContactFields},native_surface_binding::NativeNodalBinding,event_step::EventAdvancePolicy,contact_set::native_velocity_trace_from_pair,boundary_law::{reference_embedding::AuthenticatedReferencePatch,plane_manifold::SupportingPlaneSelection,complete_shell_path::CompleteShellContact,primal_vf_family::{PrimalVfFamily,authenticated_native_shell_contact}},collection::MultipleContact};
pub type AuthenticatedShellContact=CompleteShellContact<MultipleContact<PrimalVfFamily>>;
pub struct PreparedShellForward<'a>{pub fields:PersistentContactFields<'a,AuthenticatedShellContact>,pub physical_design:Vec<f64>,pub physical_velocity:CsrMatrix,pub feature_identity:serde_json::Value}
pub fn prepare_authenticated_shell_forward<'a>(model:&'a MovingFsiPairModel,controls:[&[f64];2],event:EventAdvancePolicy,patches:[&AuthenticatedReferencePatch;2],feature_pairs:&[[usize;2]],gap_scale_m:f64,force_scale_n:f64,initial_plane:SupportingPlaneSelection)->CaeResult<PreparedShellForward<'a>>{
 let physical_design=model.event_physical_design(controls)?;let resources=PairNativeResources::new(model,event)?;let binding=NativeNodalBinding::new(model,&resources.solid,&physical_design)?;let mut initial=resources.solid.initial_state(&physical_design)?;initial.extend(vec![0.;feature_pairs.len()]);let physical_velocity=native_velocity_trace_from_pair(model,&resources.solid,&physical_design,1.,feature_pairs.len())?;let law=authenticated_native_shell_contact(&binding,patches,feature_pairs,&physical_design,&initial,gap_scale_m,force_scale_n,initial_plane)?;let feature_identity=serde_json::json!({"schema":"native-authenticated-shell-forward-family/1","model_identity":model.identity(),"native_binding":binding.identity(),"shells":[patches[0].identity(),patches[1].identity()],"feature_pairs":feature_pairs,"gap_scale_m":gap_scale_m,"force_scale_n":force_scale_n,"scope":"complete sampled shell separation guard and incident VF families; unsupported EE and feature-event transitions refuse; no ordinary closed gradient"});let fields=resources.into_contact(law)?;Ok(PreparedShellForward{fields,physical_design,physical_velocity,feature_identity})
}
