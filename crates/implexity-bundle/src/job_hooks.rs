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



use std::sync::Arc;

use implexity_core::CaeResult;
use implexity_core::contracts::ProviderProblem;
use implexity_jobs::provider_hooks::{ProviderJobHooks, register};
use implexity_physics_cfd::CfdProblem;
#[cfg(feature = "dynamic")]
use implexity_physics_fsi::provider as fsi;
use implexity_physics_thermofluid::stokes_brinkman::{NAME as RESOLVED_STOKES, ResolvedStokesProvider};
use serde_json::Value;

struct ResolvedStokesHooks;

impl ProviderJobHooks for ResolvedStokesHooks {
    fn required_topology_registration(&self, problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        let p = problem.downcast_ref::<CfdProblem>()?;
        Some(ResolvedStokesProvider::required_topology_registration(p))
    }

    fn required_topology_semantics(&self, problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        let p = problem.downcast_ref::<CfdProblem>()?;
        Some(Ok(ResolvedStokesProvider::required_topology_semantics(p)))
    }
}

#[cfg(feature = "dynamic")]
struct FsiDynamicHooks;

#[cfg(feature = "dynamic")]
impl ProviderJobHooks for FsiDynamicHooks {
    fn required_topology_semantics(&self, _problem: &ProviderProblem) -> Option<CaeResult<Value>> {
        Some(Ok(fsi::required_topology_semantics()))
    }

    fn design_coordinate_shape(
        &self,
        problem: &ProviderProblem,
        coordinate: &str,
    ) -> Option<CaeResult<Option<Vec<usize>>>> {
        let document = problem.downcast_ref::<Value>()?;
        if coordinate != "model:control" {
            return Some(Ok(None));
        }
        Some(fsi::design_coordinate_shape(document).map(Some))
    }
}

#[cfg(feature = "dynamic")]
struct ContactSetHooks;
#[cfg(feature = "dynamic")]
impl ProviderJobHooks for ContactSetHooks {
    fn design_coordinate_shape(&self,problem:&ProviderProblem,coordinate:&str)->Option<CaeResult<Option<Vec<usize>>>>{
        let document=problem.downcast_ref::<Value>()?;
        Some(implexity_physics_fsi::moving_contact::macro_control::provider::design_coordinate_shape(document,coordinate))
    }
}

#[cfg(feature = "dynamic")]
struct AuthenticatedShellForwardHooks;
#[cfg(feature = "dynamic")]
impl ProviderJobHooks for AuthenticatedShellForwardHooks{
    fn design_coordinate_shape(&self,problem:&ProviderProblem,coordinate:&str)->Option<CaeResult<Option<Vec<usize>>>>{
        Some(implexity_physics_fsi::moving_contact::shell_forward_provider::normalized_design_coordinate_shape(problem,coordinate))
    }
}

#[cfg(feature = "dynamic")]
struct ClosedSurfaceHistoryHooks;
#[cfg(feature = "dynamic")]
impl ProviderJobHooks for ClosedSurfaceHistoryHooks{
    fn has_derived_model_output_refs(&self)->bool{true}
    fn has_derive_model_updates(&self)->bool{true}
    fn derived_model_hooks_enabled(&self,p:&ProviderProblem)->bool{implexity_physics_fsi::moving_contact::closed_surface_provider::derived_geometry_enabled(p)}
    fn derived_model_output_refs(&self,p:&ProviderProblem)->Option<CaeResult<Vec<String>>>{Some(implexity_physics_fsi::moving_contact::closed_surface_provider::derived_geometry_refs(p))}
    fn derive_model_updates(&self,p:&ProviderProblem,d:&implexity_jobs::provider_hooks::DerivedModelArrays)->Option<CaeResult<implexity_jobs::provider_hooks::DerivedModelArrays>>{Some(implexity_physics_fsi::moving_contact::closed_surface_provider::derived_geometry_updates(p,d))}

    fn design_coordinate_shape(&self,problem:&ProviderProblem,coordinate:&str)->Option<CaeResult<Option<Vec<usize>>>>{
        Some(implexity_physics_fsi::moving_contact::closed_surface_provider::normalized_design_coordinate_shape(problem,coordinate))
    }
}

#[cfg(feature = "dynamic")]
struct SurfaceGapHistoryHooks;
#[cfg(feature = "dynamic")]
impl ProviderJobHooks for SurfaceGapHistoryHooks{
    fn has_derived_model_output_refs(&self)->bool{true}
    fn has_derive_model_updates(&self)->bool{true}
    fn derived_model_hooks_enabled(&self,p:&ProviderProblem)->bool{implexity_physics_fsi::surface_gap_provider::derived_geometry_enabled(p)}
    fn derived_model_output_refs(&self,p:&ProviderProblem)->Option<CaeResult<Vec<String>>>{Some(implexity_physics_fsi::surface_gap_provider::derived_geometry_refs(p))}
    fn derive_model_updates(&self,p:&ProviderProblem,d:&implexity_jobs::provider_hooks::DerivedModelArrays)->Option<CaeResult<implexity_jobs::provider_hooks::DerivedModelArrays>>{Some(implexity_physics_fsi::surface_gap_provider::derived_geometry_updates(p,d))}

    fn design_coordinate_shape(&self,problem:&ProviderProblem,coordinate:&str)->Option<CaeResult<Option<Vec<usize>>>>{
        Some(implexity_physics_fsi::surface_gap_provider::normalized_design_coordinate_shape(problem,coordinate))
    }
}

#[cfg(feature = "dynamic")]
struct NativeContinuousHistoryHooks;
#[cfg(feature = "dynamic")]
impl ProviderJobHooks for NativeContinuousHistoryHooks {
    fn has_derived_model_output_refs(&self) -> bool { true }
    fn has_derive_model_updates(&self) -> bool { true }
    fn derived_model_hooks_enabled(&self, p: &ProviderProblem) -> bool {
        implexity_physics_fsi::native_continuous_provider::derived_geometry_enabled(p)
    }
    fn derived_model_output_refs(&self, p: &ProviderProblem) -> Option<CaeResult<Vec<String>>> {
        Some(implexity_physics_fsi::native_continuous_provider::derived_geometry_refs(p))
    }
    fn derive_model_updates(&self, p: &ProviderProblem, d: &implexity_jobs::provider_hooks::DerivedModelArrays) -> Option<CaeResult<implexity_jobs::provider_hooks::DerivedModelArrays>> {
        Some(implexity_physics_fsi::native_continuous_provider::derived_geometry_updates(p, d))
    }
    fn design_coordinate_shape(&self, p: &ProviderProblem, coordinate: &str) -> Option<CaeResult<Option<Vec<usize>>>> {
        Some(implexity_physics_fsi::native_continuous_provider::normalized_design_coordinate_shape(p, coordinate))
    }
}

pub fn register_job_hooks() {
    register(RESOLVED_STOKES, Arc::new(ResolvedStokesHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::native_continuous_provider::NAME, Arc::new(NativeContinuousHistoryHooks));
    #[cfg(feature = "dynamic")]
    register(fsi::NAME, Arc::new(FsiDynamicHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::moving_contact::closed_surface_provider::NAME, Arc::new(ClosedSurfaceHistoryHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::surface_gap_provider::NAME, Arc::new(SurfaceGapHistoryHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::moving_contact::macro_control::provider::NAME, Arc::new(ContactSetHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::moving_contact::shell_forward_provider::NAME, Arc::new(AuthenticatedShellForwardHooks));
}

