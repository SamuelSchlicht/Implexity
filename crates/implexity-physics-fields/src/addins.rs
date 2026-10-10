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

use serde_json::Value;

use implexity_core::CaeResult;
use implexity_core::orchestration::{AddInCategory, AddInContract, ContractInput, Fidelity, PortSpec};
use implexity_core::packages::{InstallContext, link_installer};

use crate::adapter::FieldSourceAdapter;

pub const PRESCRIBED_JOULE_HEAT: &str = "implexity.addins.prescribed_joule_heat";
pub const RESOLVED_ELECTROTHERMAL: &str = "implexity.addins.resolved_electrothermal";
pub const ELECTROMAGNETIC_LOADS: &str = "implexity.addins.electromagnetic_loads";
pub const PRESCRIBED_VOLUMETRIC_HEATING: &str = "implexity.addins.prescribed_volumetric_heating";
pub const NEUTRAL_PARTICLE_TRANSPORT: &str = "implexity.addins.neutral_particle_transport";

fn register(
    ctx: &InstallContext<'_>,
    name: &str,
    notes: Vec<String>,
    adapter: FieldSourceAdapter,
) -> CaeResult<()> {
    let mut c = AddInContract::new(name);
    c.category = AddInCategory::Field;
    c.provides = vec![PortSpec::new("coupled_history_source")];
    c.fidelity = Fidelity::Intermediate;
    c.notes = notes;
    ctx.register_addin(ContractInput::Typed(Box::new(c)), Some(Arc::new(adapter)))?;
    Ok(())
}

fn data(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}


pub fn install_prescribed_joule_heat(ctx: &InstallContext<'_>) -> CaeResult<()> {
    use crate::prescribed_joule as m;
    register(
        ctx,
        m::NAME,
        m::limitations(),
        FieldSourceAdapter::new(
            "implexity.physics_library.prescribed_joule.PrescribedJoule",
            data(include_str!("data/prescribed_joule.json")),
            m::PrescribedJoule,
        ),
    )
}


pub fn install_prescribed_volumetric_heating(ctx: &InstallContext<'_>) -> CaeResult<()> {
    use crate::prescribed_volumetric_heating as m;
    register(
        ctx,
        m::NAME,
        m::limitations(),
        FieldSourceAdapter::new(
            "implexity.physics_library.prescribed_volumetric_heating.PrescribedVolumetricHeating",
            data(include_str!("data/prescribed_volumetric_heating.json")),
            m::PrescribedVolumetricHeating,
        ),
    )
}


pub fn install_resolved_electrothermal(ctx: &InstallContext<'_>) -> CaeResult<()> {
    use crate::electrothermal as m;
    register(
        ctx,
        m::NAME,
        m::limitations(),
        FieldSourceAdapter::new(
            "implexity.physics_library.electrothermal_element.NativeElectrothermalSource",
            data(include_str!("data/electrothermal.json")),
            m::NativeElectrothermalSource,
        ),
    )
}


pub fn install_electromagnetic_loads(ctx: &InstallContext<'_>) -> CaeResult<()> {
    use crate::electromagnetic_loads as m;
    register(
        ctx,
        m::NAME,
        m::limitations(),
        FieldSourceAdapter::new(
            "implexity.physics_library.electromagnetic_loads.ElectromagneticLoads",
            data(include_str!("data/electromagnetic_loads.json")),
            m::ElectromagneticLoads,
        ),
    )
}


pub fn install_neutral_particle_transport(ctx: &InstallContext<'_>) -> CaeResult<()> {
    use crate::neutral as m;
    register(
        ctx,
        m::NAME,
        m::limitations(),
        FieldSourceAdapter::new(
            "implexity.physics_library.neutral_deposition.NeutralDeposition",
            data(include_str!("data/neutral_deposition.json")),
            m::NeutralDeposition,
        ),
    )
}

pub fn link() {
    link_installer(PRESCRIBED_JOULE_HEAT, install_prescribed_joule_heat);
    link_installer(PRESCRIBED_VOLUMETRIC_HEATING, install_prescribed_volumetric_heating);
    link_installer(RESOLVED_ELECTROTHERMAL, install_resolved_electrothermal);
    link_installer(ELECTROMAGNETIC_LOADS, install_electromagnetic_loads);
    link_installer(NEUTRAL_PARTICLE_TRANSPORT, install_neutral_particle_transport);
}
