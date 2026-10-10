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
