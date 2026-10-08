// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use serde_json::{Map, Value};

use crate::contracts::{
    CaeProvider, ProviderCapabilities, TOPOLOGY_COORDINATE, provider_key, require_contract_bool,
};
use crate::error::{CaeError, CaeResult};
use crate::py_repr::repr_str;
use crate::pyobj::py_str;

use super::intent::EngineeringIntent;
use super::registry::{AddInAdapter, AddInRegistry, ContractInput, LegacyProviderAdapter};
use super::types::{
    AddInCategory, AddInContract, DesignCoordinateRef, ExecutionKind, ExternalPortValue, Fidelity, PortSpec,
    ResponseCapability, RuntimeRoute, STRICT_CONTRACT_VERSION,
};

pub const TRANSLATION_NOTE: &str =
    "Compatibility-translated v1 add-in contract; not evidence for a new canonical declaration.";


pub fn translate_legacy_addin_contract(contract: &AddInContract) -> CaeResult<AddInContract> {
    if contract.contract_version != 1 {
        if contract.contract_version == STRICT_CONTRACT_VERSION {
            return Ok(contract.clone());
        }
        return Err(CaeError::contract(format!(
            "unsupported legacy add-in contract version {}",
            contract.contract_version
        )));
    }
    let responses: Vec<ResponseCapability> = contract
        .responses
        .iter()
        .map(|row| {
            let mut r = row.clone();
            r.differentiable = Some(row.differentiable.unwrap_or(false));
            r.topology_reachable = Some(row.topology_reachable.unwrap_or(false));
            r.design_reachable = Some(row.topology_reachable.unwrap_or(false));
            r
        })
        .collect();
    let mut notes = contract.notes.clone();
    if !notes.iter().any(|n| n == TRANSLATION_NOTE) {
        notes.push(TRANSLATION_NOTE.to_string());
    }
    let ports = |rows: &[PortSpec], kind: &str| -> Vec<PortSpec> {
        let mut used: Vec<String> = Vec::new();
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                let base = if row.port_id.is_empty() {
                    format!("{}.{kind}.{index}:{}", contract.addin_id, row.quantity)
                } else {
                    row.port_id.clone()
                };
                let mut endpoint = base.clone();
                let mut suffix = 1;
                while used.contains(&endpoint) {
                    endpoint = format!("{base}#{suffix}");
                    suffix += 1;
                }
                used.push(endpoint.clone());
                let mut p = row.clone();
                p.port_id = endpoint;
                p
            })
            .collect()
    };
    let design_inputs = if contract.design_inputs.is_empty() {
        if contract.direct_topology_dependence == Some(true) {
            vec![DesignCoordinateRef {
                coordinate: contract.topology_coordinate.clone(),
                addin_id: String::new(),
                port_id: "topology_density".into(),
            }]
        } else {
            Vec::new()
        }
    } else {
        contract.design_inputs.clone()
    };
    let translated = AddInContract {
        provides: ports(&contract.provides, "provides"),
        consumes: ports(&contract.consumes, "consumes"),
        responses,
        exact_design_derivatives: Some(contract.exact_design_derivatives.unwrap_or(false)),
        exact_state_transpose: Some(contract.exact_state_transpose.unwrap_or(false)),
        direct_topology_dependence: Some(contract.direct_topology_dependence.unwrap_or(false)),
        notes,
        contract_version: STRICT_CONTRACT_VERSION,
        compatibility_mode: true,
        owner_id: if contract.owner_id.is_empty() {
            format!("legacy-addin:{}", contract.addin_id)
        } else {
            contract.owner_id.clone()
        },
        execution_kind: Some(ExecutionKind::Legacy),
        supported_operations: vec!["preflight".into(), "evaluate".into(), "report".into()],
        no_op_operations: Vec::new(),
        design_inputs,
        ..contract.clone()
    };
    translated.validate()?;
    Ok(translated)
}

#[must_use]
pub fn legacy_external_port_value(
    need: &PortSpec,
    authoring: &Map<String, Value>,
) -> Option<ExternalPortValue> {
    let mut pools: Vec<&Map<String, Value>> = Vec::new();
    for key in ["quantities", "boundary_conditions", "states", "external_quantities"] {
        if let Some(Value::Object(m)) = authoring.get(key) {
            pools.push(m);
        }
    }
    pools.push(authoring);
    for pool in pools {
        if let Some(v) = pool.get(&need.quantity) {
            return ExternalPortValue::new(need.clone(), v.clone(), "legacy-authoring").ok();
        }
        let qualified = format!("{}@{}", need.quantity, need.domain);
        if let Some(v) = pool.get(&qualified) {
            return ExternalPortValue::new(need.clone(), v.clone(), "legacy-authoring").ok();
        }
    }
    None
}

#[must_use]
pub fn legacy_design_port_matches(contract: &AddInContract, need: &PortSpec) -> bool {
    need.quantity == "topology_density"
        && contract
            .design_inputs
            .iter()
            .any(|r| r.coordinate == TOPOLOGY_COORDINATE && r.port_id == "topology_density")
}


pub fn legacy_intent_active_design_coordinates(intent: &EngineeringIntent) -> CaeResult<Vec<String>> {
    if intent.contract_version != 1 {
        return Err(CaeError::contract(
            "legacy design-coordinate inference requires a v1 engineering intent",
        ));
    }
    if !intent.active_design_coordinates.is_empty() {
        return Err(CaeError::contract(
            "v1 engineering intent cannot override its compatibility design coordinate",
        ));
    }
    Ok(vec![TOPOLOGY_COORDINATE.to_string()])
}

fn capget(caps: &Map<String, Value>, name: &str, default: Value) -> Value {
    caps.get(name).cloned().unwrap_or(default)
}

fn legacy_fidelity(aid: &str, caps: &Map<String, Value>) -> CaeResult<Fidelity> {
    if caps.get("traits").and_then(|v| v.get("approximate")).and_then(Value::as_bool) == Some(true) { return Ok(Fidelity::Screening); }
    let mut flags = [false; 3];
    for (i, name) in
        ["resolvedReactingFlow", "productionPhysics", "exactScreeningGradient"].iter().enumerate()
    {
        let v = capget(caps, name, Value::Bool(false));
        flags[i] = require_contract_bool(&v, &format!("legacy provider {}: {name}", repr_str(aid)), false)?
            .unwrap_or(false);
    }
    let lower = aid.to_lowercase();
    Ok(if flags[0] || flags[1] {
        Fidelity::High
    } else if flags[2] || lower.contains("screening") {
        Fidelity::Screening
    } else if lower.contains("resolved") || lower.contains("implicit") {
        Fidelity::High
    } else {
        Fidelity::Intermediate
    })
}


#[allow(clippy::too_many_lines)]
pub fn legacy_provider_contract(provider: &dyn CaeProvider) -> CaeResult<AddInContract> {
    let caps: ProviderCapabilities = provider.capabilities()?;
    let caps = caps.to_map();
    let mut aid = provider_key(provider);
    if aid.is_empty() {
        aid = caps.get("name").map(py_str).unwrap_or_default();
    }
    if aid.is_empty() {
        return Err(CaeError::contract("legacy provider bundle requires an id"));
    }
    let raid = repr_str(&aid);
    let sensitivities = require_contract_bool(
        &capget(&caps, "sensitivities", Value::Bool(true)),
        &format!("legacy provider {raid}: sensitivities"),
        false,
    )?
    .unwrap_or(true);
    let execution = capget(&caps, "execution", Value::String("array".into()));
    let execution = execution.as_str().unwrap_or("");
    if execution != "array" && execution != "implicit_job" {
        return Err(CaeError::contract(format!(
            "legacy provider {raid}: unsupported execution {}",
            crate::pyobj::repr(&capget(&caps, "execution", Value::Null))
        )));
    }
    let route = if execution == "implicit_job" { RuntimeRoute::MatureJob } else { RuntimeRoute::Array };
    let metadata = match caps.get("response_metadata") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => {
            return Err(CaeError::contract(format!("{aid}: provider response_metadata must be a mapping")));
        }
    };
    let mut responses = Vec::new();
    let raw_responses = caps.get("responses").and_then(Value::as_array).cloned().unwrap_or_default();
    for raw in &raw_responses {
        let response = py_str(raw);
        let row = match metadata.get(&response) {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                return Err(CaeError::contract(format!(
                    "{aid}: metadata for response {} must be a mapping",
                    repr_str(&response)
                )));
            }
        };
        let differentiable = require_contract_bool(
            &row.get("differentiable").cloned().unwrap_or(Value::Bool(sensitivities)),
            &format!("{aid}:{response}: differentiable"),
            false,
        )?;
        let topology_reachable = require_contract_bool(
            &row.get("topology_reachable").cloned().unwrap_or(Value::Bool(true)),
            &format!("{aid}:{response}: topology_reachable"),
            false,
        )?;
        let depends = match row.get("depends_on") {
            None => Vec::new(),
            Some(v) if !crate::pyobj::truthy(v) => Vec::new(),
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
                items.iter().map(py_str).collect()
            }
            Some(_) => {
                return Err(CaeError::contract(format!(
                    "{aid}:{response}: depends_on must contain text ids"
                )));
            }
        };
        let text = |key: &str, default: &str| -> String {
            match row.get(key) {
                None => default.to_string(),
                Some(v) if !crate::pyobj::truthy(v) => String::new(),
                Some(v) => py_str(v),
            }
        };
        responses.push(ResponseCapability {
            response: response.clone(),
            unit: row.get("unit").map_or_else(|| "-".to_string(), py_str),
            differentiable,
            topology_reachable,
            depends_on: depends,
            label: text("label", ""),
            description: text("description", ""),
            family: text("family", ""),
            design_reachable: topology_reachable,
        });
    }
    let contract = AddInContract {
        addin_id: aid.clone(),
        category: AddInCategory::Field,
        responses,
        scope: provider.application_scope(),
        fidelity: legacy_fidelity(&aid, &caps)?,
        runtime_route: route,
        lifecycle: if route == RuntimeRoute::MatureJob { Some(aid.clone()) } else { None },
        topology_coordinate: TOPOLOGY_COORDINATE.into(),
        exact_design_derivatives: Some(sensitivities),
        exact_state_transpose: Some(sensitivities),
        direct_topology_dependence: Some(true),
        notes: vec![
            "Legacy provider bundle; application-specific interpretation is quarantined in cae.compat."
                .into(),
        ],
        contract_version: STRICT_CONTRACT_VERSION,
        compatibility_mode: true,
        owner_id: format!("provider:{aid}"),
        execution_kind: Some(ExecutionKind::Legacy),
        supported_operations: vec!["preflight".into(), "evaluate".into(), "report".into()],
        design_inputs: vec![DesignCoordinateRef::new(TOPOLOGY_COORDINATE, "topology_density")],
        ..AddInContract::new(aid.clone())
    };
    contract.validate()?;
    Ok(contract)
}


pub fn register_legacy_provider_bundle(
    provider: &Arc<dyn CaeProvider>,
    registry: &AddInRegistry,
) -> CaeResult<AddInContract> {
    let contract = legacy_provider_contract(provider.as_ref())?;
    let existing = registry.get(&contract.addin_id).ok();
    let adapter: Arc<dyn AddInAdapter> = Arc::new(LegacyProviderAdapter::new(Arc::clone(provider)));
    let owner = contract.owner_id.clone();
    let Some(existing) = existing else {
        return registry.register(ContractInput::Typed(Box::new(contract)), Some(adapter), Some(&owner));
    };
    let current = existing.adapter.as_ref().and_then(|a| a.provider());
    if !current.as_ref().is_some_and(|p| Arc::ptr_eq(p, provider)) {
        return Err(CaeError::contract(format!(
            "provider/add-in identity collision for {}: add-in is owned by {}",
            repr_str(&contract.addin_id),
            repr_str(&existing.owner_identity)
        )));
    }
    if existing.contract == contract {
        return Ok(existing.contract.clone());
    }
    let aid = contract.addin_id.clone();
    registry.replace(&aid, contract, Some(adapter), &registry.binding_token(), Some(&owner))
}


pub fn register_legacy_published_contract(
    provider: &Arc<dyn CaeProvider>,
    contract: &AddInContract,
    registry: &AddInRegistry,
) -> CaeResult<AddInContract> {
    if contract.contract_version != 1 {
        return Err(CaeError::contract("legacy published-contract adapter requires an AddInContract v1"));
    }
    let aid = provider_key(provider.as_ref());
    if aid.is_empty() || aid != contract.addin_id {
        return Err(CaeError::contract(format!(
            "provider/add-in identity mismatch: provider {}, contract {}",
            repr_str(&aid),
            repr_str(&contract.addin_id)
        )));
    }
    let mut translated = translate_legacy_addin_contract(contract)?;
    translated.owner_id = format!("provider:{aid}");
    let adapter: Arc<dyn AddInAdapter> = Arc::new(LegacyProviderAdapter::new(Arc::clone(provider)));
    let owner = translated.owner_id.clone();
    let Some(existing) = registry.get(&aid).ok() else {
        return registry.register(ContractInput::Typed(Box::new(translated)), Some(adapter), Some(&owner));
    };
    let current = existing.adapter.as_ref().and_then(|a| a.provider());
    if !current.as_ref().is_some_and(|p| Arc::ptr_eq(p, provider)) {
        return Err(CaeError::contract(format!("provider/add-in identity collision for {}", repr_str(&aid))));
    }
    if existing.contract == translated {
        return Ok(existing.contract.clone());
    }
    registry.replace(&aid, translated, Some(adapter), &registry.binding_token(), Some(&owner))
}
