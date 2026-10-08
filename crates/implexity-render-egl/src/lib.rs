// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END






#![deny(unsafe_code)]

#[cfg(feature = "egl")]
#[allow(unsafe_code)]
mod gl;
pub mod shaders;
pub mod worker;

use std::path::Path;

pub use worker::WorkerError;



pub fn render(packet: &Path, arrays: &Path, viewer: &Path, output: &Path) -> Result<(), WorkerError> {
    let inputs = worker::read_inputs(packet, arrays, viewer)?;
    let plan = worker::plan_inputs(&inputs)?;
    let (png, report) = execute(&plan, &inputs.sources)?;
    let io = |e: std::io::Error| WorkerError::Runtime(e.to_string());
    std::fs::write(output.join("image.png"), png).map_err(io)?;
    let text = serde_json::to_string_pretty(&report).map_err(|e| WorkerError::Runtime(e.to_string()))?;
    std::fs::write(output.join("graphics.json"), text).map_err(io)?;
    Ok(())
}



#[cfg(feature = "egl")]
pub fn execute(
    plan: &worker::RenderPlan,
    sources: &shaders::ShaderSources,
) -> Result<(Vec<u8>, serde_json::Value), WorkerError> {
    let back = gl::execute(plan, sources)?;
    worker::finish(plan, sources, &back)
}



#[cfg(not(feature = "egl"))]
pub fn execute(
    _plan: &worker::RenderPlan,
    _sources: &shaders::ShaderSources,
) -> Result<(Vec<u8>, serde_json::Value), WorkerError> {
    Err(WorkerError::Runtime("this implexity-egl-worker was built without the `egl` feature".into()))
}

pub const CRATE: &str = "implexity-render-egl";

pub const LAYER: &str = "kernel";
