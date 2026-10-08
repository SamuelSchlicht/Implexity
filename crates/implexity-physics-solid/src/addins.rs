// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::CaeResult;
use implexity_core::packages::{InstallContext, link_installer};

pub const SOLID_MECHANICS: &str = "implexity.addins.solid_mechanics";
pub const INELASTIC_MATERIALS: &str = "implexity.addins.inelastic_materials";
pub const CYCLIC_PLASTICITY: &str = "implexity.addins.cyclic_plasticity";
pub const TABULATED_MATERIALS: &str = "implexity.addins.tabulated_materials";
pub const MATERIAL_EVOLUTION: &str = "implexity.addins.material_evolution";
pub const MOVING_MATERIAL_INTERFACES: &str = "implexity.addins.moving_material_interfaces";


pub fn install_solid_mechanics(ctx: &InstallContext<'_>) -> CaeResult<()> {
    implexity_physics_base::coupling_policy::install(ctx, SOLID_MECHANICS)?;
    ctx.register_provider(Arc::new(crate::solid_history::NativeSolidHistoryProvider::new()))?;
    crate::solid_history::register_history_component(ctx)?;
    Ok(())
}


pub fn install_inelastic_materials(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::components::register_inelastic_components(ctx)
}


pub fn install_cyclic_plasticity(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::components::register_chaboche(ctx)
}


pub fn install_tabulated_materials(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::components::register_tabulated(ctx)
}


pub fn install_material_evolution(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::components::register_material_evolution(ctx)
}


pub fn install_moving_material_interfaces(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::components::register_moving_material(ctx)
}

pub const HYPERELASTIC_FIELD: &str = "implexity.addins.hyperelastic_field";


pub fn install_hyperelastic_field(ctx: &InstallContext<'_>) -> CaeResult<()> {
    for kind in crate::hyperelastic::providers::ALL {
        ctx.register_provider(Arc::new(crate::hyperelastic::providers::HyperelasticProvider(kind)))?;
    }
    Ok(())
}

pub const SOFT_MATTER_MECHANICS: &str = crate::soft::providers::INSTALLER;


pub fn install_soft_matter_mechanics(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::soft::providers::install(ctx)
}

pub fn link() {
    implexity_physics_base::engineering_library::link();
    link_installer(SOLID_MECHANICS, install_solid_mechanics);
    link_installer(INELASTIC_MATERIALS, install_inelastic_materials);
    link_installer(CYCLIC_PLASTICITY, install_cyclic_plasticity);
    link_installer(TABULATED_MATERIALS, install_tabulated_materials);
    link_installer(MATERIAL_EVOLUTION, install_material_evolution);
    link_installer(MOVING_MATERIAL_INTERFACES, install_moving_material_interfaces);
    link_installer(HYPERELASTIC_FIELD, install_hyperelastic_field);
    link_installer(SOFT_MATTER_MECHANICS, install_soft_matter_mechanics);
}
