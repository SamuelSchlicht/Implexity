// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

