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
