// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::route_tables::RouteTable;

use crate::api::{CfdBackend, CfdWorkspaceController, route_table};
use crate::error::{CfdError, CfdResult};
use crate::optimizer_bridge::UniversalOptimizerManager;
use crate::resolved_stokes::ResolvedStokesBrinkmanBackend;

pub struct Attached {
    pub controller: Arc<CfdWorkspaceController>,
    pub backend: Arc<dyn CfdBackend>,
    pub routes: RouteTable,
}

impl std::fmt::Debug for Attached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attached").field("routes", &self.routes.endpoints()).finish_non_exhaustive()
    }
}


pub fn attach(
    backend: Option<Arc<dyn CfdBackend>>,
    optimizer_manager: Option<Arc<dyn UniversalOptimizerManager>>,
) -> CfdResult<Attached> {
    let backend: Arc<dyn CfdBackend> =
        backend.unwrap_or_else(|| Arc::new(ResolvedStokesBrinkmanBackend::default()) as Arc<dyn CfdBackend>);
    let controller = Arc::new(CfdWorkspaceController::new(Some(Arc::clone(&backend)), optimizer_manager));
    let routes = route_table(&controller).map_err(|e| {
        CfdError::Integration(format!(
            "active service host exposes no supported CFD route-registration interface: {e}"
        ))
    })?;
    Ok(Attached { controller, backend, routes })
}
