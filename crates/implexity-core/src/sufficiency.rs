// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use crate::error::{CaeError, CaeResult};
use crate::json::{canonical_sha256, sha256_hex};
use crate::orchestration::{EngineeringIntent, OrchestrationPlan, RegisteredAddIn, RegistryBindingToken};
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;
use crate::sync::{ReLock, lock};

fn fidelity_rank(name: &str) -> Option<i64> {
    match name {
        "screening" => Some(0),
        "intermediate" => Some(1),
        "high" => Some(2),
        "qualification" => Some(3),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SufficiencyRule {
    pub response: String,
    pub minimum_fidelity: String,
    pub required_categories: Vec<String>,
    pub required_companion_responses: Vec<String>,
    pub required_quantities: Vec<String>,
    pub rationale: String,
    pub module_id: String,
    pub owner_addin_id: String,
    pub response_unit: String,
    pub rule_version: i64,
}

impl SufficiencyRule {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            minimum_fidelity: "screening".into(),
            required_categories: Vec::new(),
            required_companion_responses: Vec::new(),
            required_quantities: Vec::new(),
            rationale: String::new(),
            module_id: String::new(),
            owner_addin_id: String::new(),
            response_unit: String::new(),
            rule_version: 1,
        }
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.response.trim().is_empty() {
            return Err(CaeError::contract("sufficiency response is required"));
        }
        if fidelity_rank(&self.minimum_fidelity).is_none() {
            return Err(CaeError::contract(format!(
                "unknown sufficiency fidelity {}",
                repr_str(&self.minimum_fidelity)
            )));
        }
        for (key, values) in [
            ("required_categories", &self.required_categories),
            ("required_companion_responses", &self.required_companion_responses),
            ("required_quantities", &self.required_quantities),
        ] {
            if values.iter().any(String::is_empty) {
                return Err(CaeError::contract(format!("sufficiency {key} must be a tuple of ids")));
            }
            let mut seen = BTreeSet::new();
            if !values.iter().all(|v| seen.insert(v)) {
                return Err(CaeError::contract(format!("sufficiency {key} contains duplicate ids")));
            }
        }
        if self.rule_version < 1 {
            return Err(CaeError::contract("sufficiency rule_version must be a positive integer"));
        }
        Ok(())
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("response".into(), json!(self.response));
        m.insert("minimum_fidelity".into(), json!(self.minimum_fidelity));
        m.insert("required_categories".into(), json!(self.required_categories));
        m.insert("required_companion_responses".into(), json!(self.required_companion_responses));
        m.insert("required_quantities".into(), json!(self.required_quantities));
        m.insert("rationale".into(), json!(self.rationale));
        m.insert("module_id".into(), json!(self.module_id));
        m.insert("owner_addin_id".into(), json!(self.owner_addin_id));
        m.insert("response_unit".into(), json!(self.response_unit));
        m.insert("rule_version".into(), json!(self.rule_version));
        Value::Object(m)
    }

    #[must_use]
    pub fn identity(&self) -> String {
        canonical_sha256(&self.to_value())
    }

    fn key(&self) -> (String, String, String, i64) {
        (self.owner_addin_id.clone(), self.module_id.clone(), self.response.clone(), self.rule_version)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SufficiencyRegistryToken {
    pub generation: u64,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct SufficiencyRegistrySnapshot {
    pub rules: Vec<Arc<SufficiencyRule>>,
    pub token: SufficiencyRegistryToken,
}

#[derive(Default)]
struct Data {
    rules: BTreeMap<String, Vec<Arc<SufficiencyRule>>>,
    generation: u64,
}

fn token_of(data: &Data) -> SufficiencyRegistryToken {
    let joined: String = data.rules.values().flatten().map(|r| r.identity()).collect();
    SufficiencyRegistryToken { generation: data.generation, fingerprint: sha256_hex(joined.as_bytes()) }
}

#[derive(Default)]
pub struct SufficiencyRegistry {
    relock: ReLock,
    data: Mutex<Data>,
}

impl std::fmt::Debug for SufficiencyRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SufficiencyRegistry").field("generation", &self.generation()).finish()
    }
}

impl SufficiencyRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        let _g = self.relock.lock();
        lock(&self.data).generation
    }


    pub fn register(&self, rule: SufficiencyRule) -> CaeResult<Arc<SufficiencyRule>> {
        rule.validate()?;
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        let rows = data.rules.entry(rule.response.clone()).or_default();
        let key = rule.key();
        if let Some(existing) = rows.iter().find(|e| e.key() == key) {
            if **existing == rule {
                return Ok(Arc::clone(existing));
            }
            return Err(CaeError::contract(format!(
                "sufficiency rule identity collision for ({}, {}, {}, {})",
                repr_str(&key.0),
                repr_str(&key.1),
                repr_str(&key.2),
                key.3
            )));
        }
        let rule = Arc::new(rule);
        rows.push(Arc::clone(&rule));
        rows.sort_by(|a, b| {
            (a.owner_addin_id.as_str(), a.module_id.as_str(), a.rule_version, a.identity()).cmp(&(
                b.owner_addin_id.as_str(),
                b.module_id.as_str(),
                b.rule_version,
                b.identity(),
            ))
        });
        data.generation += 1;
        Ok(rule)
    }

    #[must_use]
    pub fn rules_for(&self, response: &str, owner_addin_id: Option<&str>) -> Vec<Arc<SufficiencyRule>> {
        let _g = self.relock.lock();
        let rows = lock(&self.data).rules.get(response).cloned().unwrap_or_default();
        match owner_addin_id {
            None => rows,
            Some(owner) => rows.into_iter().filter(|r| r.owner_addin_id == owner).collect(),
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> SufficiencyRegistrySnapshot {
        let _g = self.relock.lock();
        let data = lock(&self.data);
        SufficiencyRegistrySnapshot {
            rules: data.rules.values().flatten().cloned().collect(),
            token: token_of(&data),
        }
    }

    #[must_use]
    pub fn binding_token(&self) -> SufficiencyRegistryToken {
        let _g = self.relock.lock();
        token_of(&lock(&self.data))
    }

    pub fn restore(&self, state: &SufficiencyRegistrySnapshot) {
        let mut rows: BTreeMap<String, Vec<Arc<SufficiencyRule>>> = BTreeMap::new();
        for rule in &state.rules {
            rows.entry(rule.response.clone()).or_default().push(Arc::clone(rule));
        }
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        data.rules = rows;
        data.generation += 1;
    }


    pub fn transaction<T, E>(&self, body: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let _g = self.relock.lock();
        let before = lock(&self.data).rules.clone();
        let result = body();
        if result.is_err() {
            let mut data = lock(&self.data);
            data.rules = before;
            data.generation += 1;
        }
        result
    }

    pub fn unregister_owner(&self, owner_addin_id: &str) {
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        let mut changed = false;
        let keys: Vec<String> = data.rules.keys().cloned().collect();
        for response in keys {
            if let Some(rows) = data.rules.get_mut(&response) {
                let before = rows.len();
                rows.retain(|r| r.owner_addin_id != owner_addin_id);
                changed |= rows.len() != before;
                if rows.is_empty() {
                    data.rules.remove(&response);
                }
            }
        }
        if changed {
            data.generation += 1;
        }
    }


    pub fn unregister(
        &self,
        identity: &str,
        expected_owner: Option<&str>,
        expected_module: Option<&str>,
    ) -> CaeResult<Arc<SufficiencyRule>> {
        if identity.is_empty() {
            return Err(CaeError::contract("sufficiency unregister requires a rule identity"));
        }
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        let found = data.rules.iter().find_map(|(resp, rows)| {
            rows.iter().position(|r| r.identity() == identity).map(|i| (resp.clone(), i))
        });
        let Some((response, index)) = found else {
            return Err(CaeError::contract(format!(
                "unknown sufficiency rule identity {}",
                repr_str(identity)
            )));
        };
        let rule = Arc::clone(&data.rules[&response][index]);
        if expected_owner.is_some_and(|o| rule.owner_addin_id != o) {
            return Err(CaeError::contract(format!(
                "sufficiency rule {} owner mismatch",
                repr_str(identity)
            )));
        }
        if expected_module.is_some_and(|m| rule.module_id != m) {
            return Err(CaeError::contract(format!(
                "sufficiency rule {} module mismatch",
                repr_str(identity)
            )));
        }
        if let Some(rows) = data.rules.get_mut(&response) {
            rows.remove(index);
            if rows.is_empty() {
                data.rules.remove(&response);
            }
        }
        data.generation += 1;
        Ok(rule)
    }

    pub fn clear(&self) {
        let _g = self.relock.lock();
        let mut data = lock(&self.data);
        if !data.rules.is_empty() {
            data.rules.clear();
            data.generation += 1;
        }
    }

    pub fn hold(&self) -> crate::sync::ReLockGuard<'_> {
        self.relock.lock()
    }
}

#[must_use]
pub fn audit_sufficiency(
    intent: &EngineeringIntent,
    plan: &OrchestrationPlan,
    entries: &BTreeMap<String, Arc<RegisteredAddIn>>,
    registry: &SufficiencyRegistry,
    snapshot_token: Option<&RegistryBindingToken>,
) -> Value {
    let selected: Vec<&Arc<RegisteredAddIn>> =
        plan.selected_addins.iter().filter_map(|a| entries.get(a)).collect();
    let categories: BTreeSet<&str> = selected.iter().map(|e| e.contract.category.as_str()).collect();
    let responses: BTreeSet<&str> =
        selected.iter().flat_map(|e| e.contract.responses.iter().map(|r| r.response.as_str())).collect();
    let quantities: BTreeSet<&str> =
        selected.iter().flat_map(|e| e.contract.provides.iter().map(|p| p.quantity.as_str())).collect();
    let mut issues: Vec<String> = Vec::new();
    let mut evidence = Map::new();
    let requested_rank = i64::from(intent.fidelity.rank());
    for response in intent.requested_responses() {
        let owner = plan.response_providers.get(&response).and_then(Value::as_str).map(str::to_string);
        let rules = owner.as_deref().map(|o| registry.rules_for(&response, Some(o))).unwrap_or_default();
        let providers: Vec<&&Arc<RegisteredAddIn>> =
            selected.iter().filter(|e| Some(e.contract.addin_id.as_str()) == owner.as_deref()).collect();
        let available = providers.iter().map(|e| i64::from(e.contract.fidelity.rank())).max().unwrap_or(-1);
        if available < requested_rank {
            issues.push(format!(
                "{response}: selected provider fidelity rank {available} is below requested rank {requested_rank}"
            ));
        }
        if rules.is_empty() {
            if requested_rank >= 3 {
                issues.push(format!(
                    "{response}: qualification requires an explicit sufficiency rule and evidence"
                ));
            }
            evidence.insert(
                response.clone(),
                json!({"status": "no_registered_sufficiency_rule", "assertion": "not_made",
                       "requested_fidelity_satisfied": available >= requested_rank}),
            );
            continue;
        }
        let mut rows = Vec::new();
        let mut all_ok = true;
        for rule in &rules {
            let min_rank = requested_rank.max(fidelity_rank(&rule.minimum_fidelity).unwrap_or(0));
            let best_rank = available;
            let missing_categories: Vec<&String> =
                rule.required_categories.iter().filter(|x| !categories.contains(x.as_str())).collect();
            let missing_responses: Vec<&String> = rule
                .required_companion_responses
                .iter()
                .filter(|x| !responses.contains(x.as_str()))
                .collect();
            let missing_quantities: Vec<&String> =
                rule.required_quantities.iter().filter(|x| !quantities.contains(x.as_str())).collect();
            let mut local: Vec<String> = Vec::new();
            if best_rank < min_rank {
                local.push(format!(
                    "response {} requires fidelity >= {} (requested rank {requested_rank}, available rank {best_rank})",
                    repr_str(&response),
                    repr_str(&rule.minimum_fidelity)
                ));
            }
            if !missing_categories.is_empty() {
                local.push(format!("missing add-in categories {}", list_repr(&missing_categories)));
            }
            if !missing_responses.is_empty() {
                local.push(format!("missing companion responses {}", list_repr(&missing_responses)));
            }
            if !missing_quantities.is_empty() {
                local.push(format!("missing physical quantities {}", list_repr(&missing_quantities)));
            }
            let cap =
                providers.iter().flat_map(|e| e.contract.responses.iter()).find(|c| c.response == response);
            if !rule.response_unit.is_empty() && cap.is_none_or(|c| c.unit != rule.response_unit) {
                local.push(format!(
                    "response unit does not match rule unit {}",
                    repr_str(&rule.response_unit)
                ));
            }
            for x in &local {
                issues.push(format!("{response}: {x}"));
            }
            all_ok &= local.is_empty();
            rows.push(json!({"rule": rule.to_value(), "rule_identity": rule.identity(),
                             "satisfied": local.is_empty(), "details": local}));
        }
        evidence.insert(
            response.clone(),
            json!({"status": if all_ok { "sufficient" } else { "insufficient" }, "rules": rows}),
        );
    }
    let token = snapshot_token.map_or(Value::Null, RegistryBindingToken::to_value);
    let rule_token = registry.binding_token();
    json!({
        "ok": issues.is_empty(),
        "issues": issues,
        "evidence": Value::Object(evidence),
        "addin_registry": token,
        "rule_registry": {"generation": rule_token.generation, "fingerprint": rule_token.fingerprint},
    })
}

#[must_use]
pub fn build_plan_provenance(
    intent: &EngineeringIntent,
    plan: &OrchestrationPlan,
    entries: &BTreeMap<String, Arc<RegisteredAddIn>>,
    snapshot_token: Option<&RegistryBindingToken>,
) -> Value {
    let mut nodes = Map::new();
    for aid in &plan.selected_addins {
        let Some(e) = entries.get(aid) else { continue };
        let c = &e.contract;
        let response_contracts: Vec<Value> = c
            .responses
            .iter()
            .map(|r| {
                let mut v = r.to_value();
                if let Some(o) = v.as_object_mut() {
                    o.shift_remove("topology_reachable");
                }
                v
            })
            .collect();
        nodes.insert(
            aid.clone(),
            json!({
                "category": c.category.as_str(),
                "fidelity": c.fidelity.as_str(),
                "runtime_route": c.runtime_route.as_str(),
                "execution_kind": c.execution_kind.map(crate::orchestration::ExecutionKind::as_str),
                "exact_design_derivatives": c.exact_design_derivatives,
                "exact_state_transpose": c.exact_state_transpose,
                "contract_fingerprint": e.contract_fingerprint,
                "compatibility_mode": e.compatibility_mode,
                "owner_identity": e.owner_identity,
                "supported_operations": c.supported_operations,
                "no_op_operations": c.no_op_operations,
                "design_inputs": c.design_inputs.iter().map(crate::orchestration::DesignCoordinateRef::to_value).collect::<Vec<_>>(),
                "provides": c.provides.iter().map(crate::orchestration::PortSpec::to_value).collect::<Vec<_>>(),
                "consumes": c.consumes.iter().map(crate::orchestration::PortSpec::to_value).collect::<Vec<_>>(),
                "responses": c.responses.iter().map(|r| r.response.clone()).collect::<Vec<_>>(),
                "response_contracts": response_contracts,
                "notes": c.notes,
            }),
        );
    }
    json!({
        "schema": "implexity-physics-provenance/2",
        "active_design_coordinates": plan.active_design_coordinates,
        "requested_responses": intent.requested_responses(),
        "response_providers": Value::Object(plan.response_providers.clone()),
        "nodes": Value::Object(nodes),
        "edges": plan.coupling_edges.iter().map(crate::orchestration::CouplingEdge::to_value).collect::<Vec<_>>(),
        "selection_reasons": Value::Object(plan.selection_reasons.clone()),
        "provider_candidates": Value::Object(plan.provider_candidates.clone()),
        "registry_binding": snapshot_token.map_or(Value::Null, RegistryBindingToken::to_value),
    })
}
