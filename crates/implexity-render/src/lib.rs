// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




#![forbid(unsafe_code)]

pub mod artifact;
pub mod capture;
#[cfg(feature = "dynamic")]
pub mod dynamic;
pub mod mesh_scene;
pub mod model_fields;
pub mod render3d;
pub mod rendering;
pub mod viewer_scene;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("{0}")]
    Invalid(String),
}

impl From<implexity_mesh::MeshError> for RenderError {
    fn from(e: implexity_mesh::MeshError) -> Self {
        Self::Invalid(e.to_string())
    }
}

pub const CRATE: &str = "implexity-render";

pub const LAYER: &str = "kernel";
