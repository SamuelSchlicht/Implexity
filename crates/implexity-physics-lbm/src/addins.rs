// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_core::CaeResult;
use implexity_core::packages::{InstallContext, link_installer};

use crate::compressible::provider::CompressibleLbmProvider;
use crate::porous::provider::PorousNativeProvider;
use crate::provider::LatticeBoltzmannProvider;
use crate::structure_provider::ThermalStructureLbmProvider;
use crate::thermal_provider::ThermalLbmProvider;

pub const LATTICE_BOLTZMANN: &str = "implexity.lbm";


pub fn install_lattice_boltzmann(ctx: &InstallContext<'_>) -> CaeResult<()> {
    ctx.register_provider(Arc::new(LatticeBoltzmannProvider))?;
    ctx.register_provider(Arc::new(ThermalLbmProvider))?;
    ctx.register_provider(Arc::new(ThermalStructureLbmProvider))?;
    ctx.register_provider(Arc::new(CompressibleLbmProvider))?;
    ctx.register_provider(Arc::new(PorousNativeProvider::default()))?;
    Ok(())
}

pub fn link() {
    link_installer(LATTICE_BOLTZMANN, install_lattice_boltzmann);
}
