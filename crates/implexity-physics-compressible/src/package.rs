// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::Arc;
use implexity_core::{CaeResult,contracts::CaeProvider,packages::{InstallContext,link_installer}};
pub const INSTALLER_MODULE:&str="implexity.addins.compressible_transport";
pub fn providers()->Vec<Arc<dyn CaeProvider>>{vec![
 Arc::new(crate::providers::quasi1d_euler::Quasi1DEulerProvider),
 Arc::new(crate::providers::euler3d::Euler3DProvider),
 Arc::new(crate::providers::euler3d_ad::Euler3DTopologyProvider::first_order()),
 Arc::new(crate::providers::euler3d_ad::Euler3DTopologyProvider::muscl()),
 Arc::new(crate::providers::euler3d_structure::EulerStructureProvider),
]}
pub fn install(ctx:&InstallContext<'_>)->CaeResult<()>{
 implexity_physics_base::coupling_policy::install(ctx,INSTALLER_MODULE)?;
 for p in providers(){let id=p.provider_id().unwrap_or_else(||p.name());if ctx.registries().providers.get(id).is_err(){ctx.register_provider(p)?;}}
 Ok(())
}
pub fn link(){link_installer(INSTALLER_MODULE,install);}
