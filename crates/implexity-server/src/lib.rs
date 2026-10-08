// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





#![forbid(unsafe_code)]

pub mod admission;
pub mod agent;
pub mod body;
pub mod contributed;
#[cfg(feature = "dynamic-results")]
pub mod dynamic_results;
pub mod geometry;
pub mod http;
pub mod jobs;
pub mod kernel_routes;
pub mod lod;
pub mod managers;
pub mod packages;
pub mod preview;
pub mod routes;
pub mod rust_extension;
pub mod service;
pub mod startup;
pub mod viewer;
mod viewer_assets;
pub mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

pub use http::{Reply, Request, RequestBody, RouteError};
pub use preview::{PreviewBackend, PreviewResult};
pub use routes::{BodyPolicy, Registry, RouteTables};
pub use service::{Service, ServiceConfig};
pub use viewer::ViewerSource;

pub const CRATE: &str = "implexity-server";

pub const LAYER: &str = "kernel";

pub const COMPATIBILITY_VERSION: &str = "24.0.0rc12.dev9";

pub const RUST_VERSION: &str = env!("CARGO_PKG_VERSION");



pub fn build_service(
    config: &ServiceConfig,
    viewer: ViewerSource,
    backend: Option<Arc<dyn PreviewBackend>>,
) -> std::io::Result<Arc<Service>> {
    let kernel = kernel_routes::kernel_registry().map_err(std::io::Error::other)?;
    let service = Service::new(config, kernel, viewer, backend)?;
    startup::install_kernel_endpoints(&service).map_err(std::io::Error::other)?;
    let service = Arc::new(service);
    startup::install_host_hooks(&service);
    #[cfg(feature = "dynamic-results")]
    dynamic_results::install(&service).map_err(std::io::Error::other)?;
    Ok(service)
}



pub fn build_geometry_service(
    config: &ServiceConfig,
    viewer: ViewerSource,
    geometry: Arc<geometry::GeometryPreview>,
) -> std::io::Result<Arc<Service>> {
    let kernel = kernel_routes::kernel_registry().map_err(std::io::Error::other)?;
    let service = Service::with_geometry(config, kernel, viewer, geometry)?;
    startup::install_kernel_endpoints(&service).map_err(std::io::Error::other)?;
    let service = Arc::new(service);
    startup::install_host_hooks(&service);
    #[cfg(feature = "dynamic-results")]
    dynamic_results::install(&service).map_err(std::io::Error::other)?;
    Ok(service)
}




pub fn run<B>(service: &Arc<Service>, host: &str, port: u16, on_bound: B) -> std::io::Result<()>
where
    B: FnOnce(SocketAddr),
{
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = runtime.block_on(http::serve(Arc::clone(service), host, port, on_bound, async {
        let _ = tokio::signal::ctrl_c().await;
    }));
    service.pool.stop();
    result
}
