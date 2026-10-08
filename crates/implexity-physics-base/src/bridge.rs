// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::addins::APPLICATION_DESIGN_COORDINATES;
use crate::contracts::AddinContract;
use crate::model_errors::PhysicsResult;
use crate::planner::{AddinPlanner, ObjectiveRequest};
use crate::provenance::build_physics_provenance;
use crate::registry::ensure_default_addins;
use crate::sufficiency::PhysicsSufficiencyAuditor;

pub const APPLICATION_MODULE_ID: &str = "engineering_physics_library";
pub const DEFAULT_ENABLED: bool = false;

fn compatibility() -> Value {
    json!({"topology_coordinate": "model:control", "contract": "engineering_physics_library_v33_single_array"})
}


pub fn addin_contracts() -> PhysicsResult<Vec<AddinContract>> {
    Ok(ensure_default_addins(None)?.list())
}


pub fn provider_factories() -> PhysicsResult<Map<String, Value>> {
    let mut out = Map::new();
    for c in addin_contracts()? {
        if let Some(f) = c.runtime_factory.filter(|f| !f.is_empty()) {
            out.insert(c.addin_id, json!(f));
        }
    }
    Ok(out)
}

pub const APPLICATION_MODULE_DESCRIPTOR: &str = include_str!("../data/application_module.json");


pub fn application_module_descriptor() -> PhysicsResult<Value> {
    serde_json::from_str(APPLICATION_MODULE_DESCRIPTOR)
        .map_err(|e| crate::PhysicsError::value(format!("application_module.json: {e}")))
}


pub fn application_module_manifest() -> PhysicsResult<Value> {
    let list = addin_contracts()?;
    let contracts: Vec<Value> = list.iter().map(AddinContract::as_dict).collect();
    let mut catalog = Map::new();
    for c in &list {
        let mut names: BTreeSet<&str> = c.objective_aliases.iter().map(String::as_str).collect();
        names.extend(c.produces.iter().map(|p| p.quantity.as_str()));
        for name in names {
            if let Value::Array(rows) = catalog.entry(name.to_string()).or_insert_with(|| json!([])) {
                rows.push(json!(c.addin_id));
            }
        }
    }
    Ok(json!({
        "schema": "implexity-application-module/2",
        "module_id": APPLICATION_MODULE_ID,
        "application_module_id": APPLICATION_MODULE_ID,
        "canonical_python_module": "implexity.physics_library",
        "python_module": "implexity.physics_library",
        "default_enabled": DEFAULT_ENABLED,
        "activation": "explicit_transactional_package_load",
        "addin_contracts": contracts,
        "physics_addins": contracts,
        "response_catalog": catalog,
        "provider_factories": provider_factories()?,
        "kernel_dependency": false,
        "design_coordinates": APPLICATION_DESIGN_COORDINATES,
        "compatibility": compatibility(),
        "authoring_namespace": "provider_local",
    }))
}

pub trait ApplicationModuleRegistry {
    fn register_application_module(&mut self, module_id: &str, manifest: &Value);
}

impl ApplicationModuleRegistry for Map<String, Value> {
    fn register_application_module(&mut self, module_id: &str, manifest: &Value) {
        self.insert(module_id.to_string(), manifest.clone());
    }
}


pub fn register_application_module(
    registry: Option<&mut dyn ApplicationModuleRegistry>,
) -> PhysicsResult<Value> {
    let manifest = application_module_manifest()?;
    if let Some(reg) = registry {
        reg.register_application_module(APPLICATION_MODULE_ID, &manifest);
    }
    Ok(manifest)
}


pub fn orchestrate_engineering_intent(
    objectives: &[ObjectiveRequest],
    activated: &[String],
    authoring: Option<&Map<String, Value>>,
    requested_fidelity: &str,
) -> PhysicsResult<Value> {
    let planner = AddinPlanner::new(None)?;
    let plan = planner.plan(objectives, activated, authoring)?;
    let suff = PhysicsSufficiencyAuditor::new(Some(planner.registry), None)?.audit(
        &plan,
        objectives,
        requested_fidelity,
    )?;
    let provenance = build_physics_provenance(&plan, objectives, Some(planner.registry), authoring)?;
    let status = if plan.ready() && suff.sufficient { "READY" } else { "BLOCKED" };
    let mut coordinates = BTreeSet::new();
    for id in &plan.selected_addins {
        coordinates.extend(planner.registry.get(id)?.design_coordinates);
    }
    let required: Map<String, Value> =
        plan.authoring_required.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    Ok(json!({
        "status": status,
        "plan": {
            "status": plan.status,
            "selected_addins": plan.selected_addins,
            "edges": plan.edges.iter().map(|(a, b, q)| json!([a, b, q])).collect::<Vec<_>>(),
            "missing_quantities": plan.missing_quantities,
            "issues": plan.issues.iter().map(crate::contracts::ValidationIssue::to_value).collect::<Vec<_>>(),
            "authoring_required": required,
        },
        "sufficiency": {
            "sufficient": suff.sufficient,
            "issues": suff.issues.iter().map(crate::contracts::ValidationIssue::to_value).collect::<Vec<_>>(),
            "evidence": suff.evidence,
        },
        "provenance": provenance,
        "design_coordinates": coordinates.into_iter().collect::<Vec<_>>(),
        "compatibility": compatibility(),
    }))
}

