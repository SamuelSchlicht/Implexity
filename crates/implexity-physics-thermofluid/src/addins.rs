// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::CaeResult;
use implexity_core::orchestration::AddInCategory;
use implexity_core::packages::{InstallContext, link_installer};
use implexity_physics_base::core_bridge::{ComponentOptions, PhysicsComponent, register_strict_component};

use crate::stokes_brinkman::{NAME as STOKES_PROVIDER, ResolvedStokesProvider};

pub const CFD_STOKES: &str = "implexity.addins.cfd_stokes";
pub const THERMAL_EXCHANGE: &str = "implexity.addins.thermal_exchange";
pub const PHASE_EQUILIBRIA: &str = "implexity.addins.phase_equilibria";
pub const UNIFIED_HEAT_MECHANICS: &str = "implexity.addins.unified_heat_mechanics";
pub const CONJUGATE_HEAT_MECHANICS: &str = "implexity.addins.conjugate_heat_mechanics";
pub const CONSERVATIVE_INTERFACES: &str = "implexity.addins.conservative_interfaces";
pub const INCOMPRESSIBLE_TRANSPORT: &str = "implexity.addins.incompressible_transport";
pub const REGION_HISTORY_RESPONSES: &str = "implexity.addins.region_history_responses";


pub fn install_cfd_stokes(ctx: &InstallContext<'_>) -> CaeResult<()> {
    implexity_physics_base::coupling_policy::install(ctx, CFD_STOKES)?;
    if ctx.registries().providers.get(STOKES_PROVIDER).is_err() {
        ctx.register_provider(Arc::new(ResolvedStokesProvider::default()))?;
    }
    Ok(())
}


pub fn install_thermal_exchange(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::thermal_exchange::register_components(ctx)
}


pub fn install_phase_equilibria(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::water_saturation::register(ctx)?;
    crate::liquid_admissibility::register(ctx)?;
    crate::liquid_pressure_margin::register(ctx)?;
    Ok(())
}


pub fn install_incompressible_transport(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::incompressible_transport::register_components(ctx)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DensityJumpStressFactory;

impl PhysicsComponent for DensityJumpStressFactory {
    fn implementation(&self) -> String {
        "implexity.physics_library.phase_stress_transfer.register.<locals>.Factory".into()
    }
    fn component_kind(&self) -> Option<String> {
        Some(implexity_physics_solid::phase_stress_transfer::COMPONENT_KIND.into())
    }
    fn runtime_support(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        implexity_physics_solid::phase_stress_transfer::runtime_support().as_object().cloned()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}


pub fn install_unified_heat_mechanics(ctx: &InstallContext<'_>) -> CaeResult<()> {
    implexity_physics_base::coupling_policy::install(ctx, UNIFIED_HEAT_MECHANICS)?;
    register_strict_component(
        ctx,
        implexity_physics_solid::phase_stress_transfer::COMPONENT_ID,
        Arc::new(DensityJumpStressFactory),
        implexity_physics_solid::phase_stress_transfer::COMPONENT_KIND,
        &ComponentOptions {
            category: AddInCategory::Field,
            domain: "shared_domain".into(),
            notes: implexity_physics_solid::phase_stress_transfer::LIMITATIONS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            ..ComponentOptions::default()
        },
    )?;
    implexity_physics_base::temperature_extrema::register(ctx)?;
    ctx.register_provider(Arc::new(crate::unified_history::NativeUnifiedHistoryProvider::new()))?;
    Ok(())
}


pub fn install_conservative_interfaces(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::conservative_interfaces::register_components(ctx)
}


pub fn install_region_history_responses(ctx: &InstallContext<'_>) -> CaeResult<()> {
    implexity_physics_base::region_temperature_extrema::register(ctx)?;
    implexity_physics_base::cyclic_plastic_strain::register(ctx)?;
    implexity_physics_base::wall_film_temperature::register(ctx)?;
    implexity_physics_base::occupancy_grayness::register(ctx)?;
    Ok(())
}

pub fn link() {
    link_installer(CFD_STOKES, install_cfd_stokes);
    link_installer(THERMAL_EXCHANGE, install_thermal_exchange);
    link_installer(PHASE_EQUILIBRIA, install_phase_equilibria);
    link_installer(INCOMPRESSIBLE_TRANSPORT, install_incompressible_transport);
    link_installer(UNIFIED_HEAT_MECHANICS, install_unified_heat_mechanics);
    link_installer(CONJUGATE_HEAT_MECHANICS, crate::conjugate_history::install_conjugate_heat_mechanics);
    link_installer(CONSERVATIVE_INTERFACES, install_conservative_interfaces);
    link_installer(REGION_HISTORY_RESPONSES, install_region_history_responses);
}
