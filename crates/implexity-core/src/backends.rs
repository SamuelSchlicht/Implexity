// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::Value;

use crate::contributions::{ContributionError, ContributionRegistry, ContributionValue};
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;
use crate::sync::lock;

#[derive(Debug, Clone, PartialEq)]
pub struct GeometryBuildOptions {
    pub design_path: Option<PathBuf>,
    pub domain_mm: [f64; 3],
    pub design_grid: [usize; 3],
    pub period_mm: f64,
    pub use_jit: bool,
}

impl Default for GeometryBuildOptions {
    fn default() -> Self {
        Self {
            design_path: None,
            domain_mm: [8.0, 16.0, 32.0],
            design_grid: [32, 64, 128],
            period_mm: 4.0,
            use_jit: true,
        }
    }
}

pub struct GeometryBuild {
    pub evaluator: Box<dyn Any + Send + Sync>,
    pub design: Box<dyn Any + Send + Sync>,
}

pub trait GeometryBackend: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i64 {
        0
    }
    fn implementation(&self) -> &str;
    fn available(&self, design_path: Option<&Path>) -> (bool, String);

    fn build(&self, options: &GeometryBuildOptions) -> Result<GeometryBuild, String>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    Key(String),
    #[error("{0}")]
    Runtime(String),
}

static GEOMETRY: LazyLock<Mutex<Vec<Arc<dyn GeometryBackend>>>> = LazyLock::new(|| Mutex::new(Vec::new()));


pub fn register_geometry(
    backend: Arc<dyn GeometryBackend>,
) -> Result<Arc<dyn GeometryBackend>, BackendError> {
    let name = backend.name().to_string();
    if name.is_empty() {
        return Err(BackendError::Value("geometry backend: needs a non-empty string 'name'".into()));
    }
    let mut g = lock(&GEOMETRY);
    if let Some(existing) = g.iter().find(|b| b.name() == name) {
        let module = existing.implementation().rsplit_once('.').map_or(existing.implementation(), |(m, _)| m);
        return Err(BackendError::Value(format!(
            "geometry backend {} is already registered (by {module}); pick another name -- a registered name is never silently replaced",
            repr_str(&name)
        )));
    }
    g.push(Arc::clone(&backend));
    Ok(backend)
}

#[must_use]
pub fn geometry_names() -> Vec<String> {
    let mut n: Vec<String> = lock(&GEOMETRY).iter().map(|b| b.name().to_string()).collect();
    n.sort();
    n
}


pub fn geometry(
    name: Option<&str>,
    design_path: Option<&Path>,
) -> Result<Arc<dyn GeometryBackend>, BackendError> {
    let env = std::env::var("IMPLEXITY_BACKEND").ok().filter(|s| !s.is_empty());
    let name = name.filter(|s| !s.is_empty()).map(str::to_string).or(env).unwrap_or_else(|| "auto".into());
    let backends = lock(&GEOMETRY).clone();
    let names = || {
        let mut n: Vec<String> = backends.iter().map(|b| b.name().to_string()).collect();
        n.sort();
        list_repr(&n)
    };
    if name != "auto" {
        return backends.iter().find(|b| b.name() == name).cloned().ok_or_else(|| {
            BackendError::Key(format!("no geometry backend {}; registered: {}", repr_str(&name), names()))
        });
    }
    let mut ordered: Vec<&Arc<dyn GeometryBackend>> = backends.iter().collect();
    ordered.sort_by_key(|b| -b.priority());
    for b in ordered {
        if b.available(design_path).0 {
            return Ok(Arc::clone(b));
        }
    }
    Err(BackendError::Runtime(format!(
        "no geometry backend reports itself available; registered: {}",
        names()
    )))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultFieldRow {
    pub id: String,
    pub source: String,
    pub rank: String,
    pub location: String,
    pub units: String,
    pub doc: String,
}

pub trait PhysicsBackend: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> &str {
        self.name()
    }
    fn implementation(&self) -> &str;
    fn available(&self) -> (bool, String);
    fn result_fields(&self) -> Vec<ResultFieldRow> {
        Vec::new()
    }
    fn problem(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
    fn current_case(&self, _svc: &dyn Any) -> Option<Value> {
        None
    }
    fn part_domain(&self, _svc: &dyn Any) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

pub struct PhysicsBackendHandle(pub Arc<dyn PhysicsBackend>);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PhysicsBackendUnset(pub String);


pub fn register_physics(
    reg: &ContributionRegistry,
    backend: Arc<dyn PhysicsBackend>,
    owner_id: &str,
) -> Result<(), ContributionError> {
    let name = backend.name().to_string();
    if name.is_empty() {
        return Err(ContributionError("physics backend: needs a non-empty string 'name'".into()));
    }
    let identity = Arc::as_ptr(&backend).cast::<()>() as usize;
    let implementation = backend.implementation().to_string();
    let value =
        ContributionValue::with_identity(Arc::new(PhysicsBackendHandle(backend)), identity, implementation);
    reg.register("physics_backends", &name, value, owner_id)?;
    Ok(())
}

#[must_use]
pub fn physics_backends(reg: &ContributionRegistry) -> Vec<(String, Arc<dyn PhysicsBackend>)> {
    reg.entries("physics_backends")
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.downcast::<PhysicsBackendHandle>().map(|h| (k, Arc::clone(&h.0))))
        .collect()
}


pub fn physics(
    reg: &ContributionRegistry,
    name: Option<&str>,
) -> Result<Arc<dyn PhysicsBackend>, PhysicsBackendUnset> {
    let active = physics_backends(reg);
    let env = std::env::var("IMPLEXITY_PHYSICS_BACKEND").ok().filter(|s| !s.is_empty());
    let name = name.filter(|s| !s.is_empty()).map(str::to_string).or(env);
    let mut names: Vec<String> = active.iter().map(|(k, _)| k.clone()).collect();
    names.sort();
    if let Some(name) = name {
        return active.iter().find(|(k, _)| *k == name).map(|(_, b)| Arc::clone(b)).ok_or_else(|| {
            PhysicsBackendUnset(format!(
                "physics backend {} is not active; active backends: {} (load the package that provides it)",
                repr_str(&name),
                if names.is_empty() { "none".to_string() } else { list_repr(&names) }
            ))
        });
    }
    match active.len() {
        1 => Ok(Arc::clone(&active[0].1)),
        0 => Err(PhysicsBackendUnset(
            "no physics backend is active: load a physics package that contributes one".into(),
        )),
        _ => Err(PhysicsBackendUnset(format!(
            "several physics backends are active ({}); select one with IMPLEXITY_PHYSICS_BACKEND",
            list_repr(&names)
        ))),
    }
}

#[must_use]
pub fn selected_physics(reg: &ContributionRegistry, name: Option<&str>) -> Option<Arc<dyn PhysicsBackend>> {
    physics(reg, name).ok()
}

#[must_use]
pub fn selected_physics_name(reg: &ContributionRegistry, name: Option<&str>) -> Option<String> {
    selected_physics(reg, name).map(|b| b.name().to_string())
}

#[must_use]
pub fn current_case(reg: &ContributionRegistry, svc: &dyn Any, name: Option<&str>) -> Option<Value> {
    selected_physics(reg, name).and_then(|b| b.current_case(svc))
}

#[must_use]
pub fn part_domain(
    reg: &ContributionRegistry,
    svc: &dyn Any,
    name: Option<&str>,
) -> Option<Arc<dyn Any + Send + Sync>> {
    selected_physics(reg, name).and_then(|b| b.part_domain(svc))
}
