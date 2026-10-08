// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
