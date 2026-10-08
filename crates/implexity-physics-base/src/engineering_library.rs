// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::CaeResult;
use implexity_core::packages::{InstallContext, link_installer};

pub const INSTALLER_MODULE: &str = "implexity.addins.engineering_library";


pub fn install(ctx: &InstallContext<'_>) -> CaeResult<()> {
    crate::coupling_policy::install(ctx, INSTALLER_MODULE)?;
    crate::core_bridge::register_into_core(ctx)?;
    crate::core_sufficiency::register_rules(ctx)?;
    Ok(())
}

pub fn link() {
    link_installer(INSTALLER_MODULE, install);
}
