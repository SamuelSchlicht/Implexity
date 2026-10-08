// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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

pub fn register_job_hooks() {
    register(RESOLVED_STOKES, Arc::new(ResolvedStokesHooks));
    #[cfg(feature = "dynamic")]
    register(fsi::NAME, Arc::new(FsiDynamicHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::moving_contact::closed_surface_provider::NAME, Arc::new(ClosedSurfaceHistoryHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::moving_contact::macro_control::provider::NAME, Arc::new(ContactSetHooks));
    #[cfg(feature = "dynamic")]
    register(implexity_physics_fsi::moving_contact::shell_forward_provider::NAME, Arc::new(AuthenticatedShellForwardHooks));
}

