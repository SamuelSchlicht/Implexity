// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::error::{CaeError, CaeResult};
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;
use crate::sufficiency::{SufficiencyRegistry, audit_sufficiency, build_plan_provenance};

use super::compat;
use super::intent::EngineeringIntent;
use super::registry::{AddInRegistrySnapshot, RegisteredAddIn, RegistryBindingToken};
use super::types::{
    AddInCategory, AddInContract, Fidelity, PlanStatus, PortKey, PortSpec, RuntimeRoute,
    STRICT_CONTRACT_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CouplingEdge {
    pub source: String,
    pub target: String,
    pub quantity: String,
    pub unit: String,
    pub source_domain: String,
    pub target_domain: String,
    pub interface: Option<String>,
    pub temporal: String,
    pub conserved: bool,
    pub cardinality: String,
    pub aggregation: String,
    pub source_port_id: String,
    pub target_port_id: String,
}

impl CouplingEdge {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("source".into(), json!(self.source));
        m.insert("target".into(), json!(self.target));
        m.insert("quantity".into(), json!(self.quantity));
        m.insert("unit".into(), json!(self.unit));
        m.insert("source_domain".into(), json!(self.source_domain));
        m.insert("target_domain".into(), json!(self.target_domain));
        m.insert("interface".into(), json!(self.interface));
        m.insert("temporal".into(), json!(self.temporal));
        m.insert("conserved".into(), json!(self.conserved));
        m.insert("cardinality".into(), json!(self.cardinality));
        m.insert("aggregation".into(), json!(self.aggregation));
        m.insert("source_port_id".into(), json!(self.source_port_id));
        m.insert("target_port_id".into(), json!(self.target_port_id));
        Value::Object(m)
    }

    fn connect(source: &str, target: &str, provided: &PortSpec, need: &PortSpec) -> Self {
        Self {
            source: source.to_string(),
            target: target.to_string(),
            quantity: need.quantity.clone(),
            unit: need.unit.clone(),
            source_domain: provided.domain.clone(),
            target_domain: need.domain.clone(),
            interface: need.interface.clone(),
            temporal: need.temporal.clone(),
            conserved: need.conserved,
            cardinality: need.cardinality.clone(),
            aggregation: need.aggregation.clone(),
            source_port_id: provided.port_id.clone(),
            target_port_id: need.port_id.clone(),
        }
    }
}


#[derive(Debug, Clone)]
pub struct OrchestrationPlan {
    pub status: PlanStatus,
    pub selected_addins: Vec<String>,
    pub response_providers: Map<String, Value>,
    pub coupling_edges: Vec<CouplingEdge>,
    pub missing_physics: Vec<String>,
    pub missing_authoring: Vec<String>,
    pub blocked_reasons: Vec<String>,
    pub warnings: Vec<String>,
    pub runtime_route: String,
    pub active_design_coordinates: Vec<String>,
    pub inactive_design_coordinates: Vec<String>,
    pub fidelity: String,
    pub provenance: Value,
    pub provider_candidates: Map<String, Value>,
    pub selection_reasons: Map<String, Value>,
    pub sufficiency: Value,
    pub physics_provenance: Value,
    pub binding_token: Option<RegistryBindingToken>,
    pub delivered_fidelity: Fidelity,
}

impl PartialEq for OrchestrationPlan {
    fn eq(&self, other: &Self) -> bool {
        self.status == other.status
            && self.selected_addins == other.selected_addins
            && self.response_providers == other.response_providers
            && self.coupling_edges == other.coupling_edges
            && self.missing_physics == other.missing_physics
            && self.missing_authoring == other.missing_authoring
            && self.blocked_reasons == other.blocked_reasons
            && self.warnings == other.warnings
            && self.runtime_route == other.runtime_route
            && self.active_design_coordinates == other.active_design_coordinates
            && self.inactive_design_coordinates == other.inactive_design_coordinates
            && self.fidelity == other.fidelity
            && self.provenance == other.provenance
            && self.provider_candidates == other.provider_candidates
            && self.selection_reasons == other.selection_reasons
            && self.sufficiency == other.sufficiency
            && self.physics_provenance == other.physics_provenance
    }
}

fn default_provenance() -> Value {
    json!({"origin": "authoritative_model", "requested_by": "human_gui", "orchestration_authority": "implexity_kernel"})
}

impl OrchestrationPlan {

    pub fn validate(&self) -> CaeResult<()> {
        for (coordinates, label) in
            [(&self.active_design_coordinates, "active"), (&self.inactive_design_coordinates, "inactive")]
        {
            if coordinates.iter().any(String::is_empty) {
                return Err(CaeError::contract(format!(
                    "orchestration plan {label}_design_coordinates must be a tuple of ids"
                )));
            }
            let mut seen = BTreeSet::new();
            if !coordinates.iter().all(|c| seen.insert(c)) {
                return Err(CaeError::contract(format!(
                    "orchestration plan {label}_design_coordinates must be unique"
                )));
            }
        }
        let mut overlap: Vec<&String> = self
            .active_design_coordinates
            .iter()
            .filter(|c| self.inactive_design_coordinates.contains(c))
            .collect();
        overlap.sort();
        if !overlap.is_empty() {
            return Err(CaeError::contract(format!(
                "orchestration plan design coordinates are both active and inactive: {}",
                list_repr(&overlap)
            )));
        }
        Ok(())
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("status".into(), json!(self.status.as_str()));
        m.insert("selected_addins".into(), json!(self.selected_addins));
        m.insert("response_providers".into(), Value::Object(self.response_providers.clone()));
        m.insert(
            "coupling_edges".into(),
            Value::Array(self.coupling_edges.iter().map(CouplingEdge::to_value).collect()),
        );
        m.insert("missing_physics".into(), json!(self.missing_physics));
        m.insert("missing_authoring".into(), json!(self.missing_authoring));
        m.insert("blocked_reasons".into(), json!(self.blocked_reasons));
        m.insert("warnings".into(), json!(self.warnings));
        m.insert("runtime_route".into(), json!(self.runtime_route));
        m.insert("active_design_coordinates".into(), json!(self.active_design_coordinates));
        m.insert("inactive_design_coordinates".into(), json!(self.inactive_design_coordinates));
        m.insert("fidelity".into(), json!(self.fidelity));
        m.insert("provenance".into(), self.provenance.clone());
        m.insert("provider_candidates".into(), Value::Object(self.provider_candidates.clone()));
        m.insert("selection_reasons".into(), Value::Object(self.selection_reasons.clone()));
        m.insert("sufficiency".into(), self.sufficiency.clone());
        m.insert("physics_provenance".into(), self.physics_provenance.clone());
        m.insert("delivered_fidelity".into(), json!(self.delivered_fidelity.as_str()));
        Value::Object(m)
    }


    pub fn require_current_binding(
        &self,
        current: Option<&RegistryBindingToken>,
    ) -> CaeResult<RegistryBindingToken> {
        let Some(token) = &self.binding_token else {
            return Err(CaeError::contract(
                "orchestration plan invalidated: plan carries no registry binding token; plan again",
            ));
        };
        match current {
            Some(c) if c == token => Ok(token.clone()),
            _ => Err(CaeError::contract(format!(
                "orchestration plan invalidated: registry binding token stale (planned generation {}, current generation {}); plan again",
                token.generation,
                current.map_or_else(|| "?".to_string(), |c| c.generation.to_string())
            ))),
        }
    }
}

#[must_use]
pub fn direct_port_match(source: &PortSpec, target: &PortSpec, compatibility: bool) -> bool {
    if compatibility {
        let wildcard = |s: &str| s.is_empty() || s == "*";
        let unit = wildcard(&source.unit) || wildcard(&target.unit) || source.unit == target.unit;
        let domain = wildcard(&target.domain) || wildcard(&source.domain) || source.domain == target.domain;
        let interface = target.interface.is_none() || source.interface == target.interface;
        return source.quantity == target.quantity && unit && domain && interface;
    }
    source.quantity == target.quantity
        && source.unit == target.unit
        && source.domain == target.domain
        && source.interface == target.interface
        && source.temporal == target.temporal
        && source.conserved == target.conserved
        && source.cardinality == target.cardinality
        && source.aggregation == target.aggregation
}

#[must_use]
pub fn provider_score(contract: &AddInContract, intent: &EngineeringIntent, response: &str) -> i64 {
    let mut score: i64 = 0;
    if let Some(o) = intent.provider_overrides.get(response).filter(|o| !o.is_empty()) {
        score += if *o == contract.addin_id { 1_000_000_000 } else { -1_000_000_000 };
    }
    let scopes: BTreeSet<&str> = contract.scope.iter().map(String::as_str).collect();
    let apps: BTreeSet<&str> = intent.application.iter().map(String::as_str).collect();
    if apps.is_empty() {
        if scopes.contains("*") {
            score += 1_000_000;
        }
    } else if !scopes.is_disjoint(&apps) {
        score += 10_000_000;
    } else if scopes.contains("*") {
        score += 1_000_000;
    } else {
        score -= 100_000_000;
    }
    let want = i64::from(intent.fidelity.rank());
    let have = i64::from(contract.fidelity.rank());
    if have >= want {
        score += 1_000_000 - (have - want) * 100_000;
    } else {
        score -= 1_000_000 + (want - have) * 100_000;
    }
    score += contract.priority * 1_000;
    score -= 10 * i64::try_from(contract.authoring.iter().filter(|a| !a.optional).count()).unwrap_or(0);
    score -= 5 * i64::try_from(contract.consumes.len()).unwrap_or(0);
    score
}

fn below_intent(contract: &AddInContract, intent: &EngineeringIntent) -> bool {
    contract.fidelity.rank() < intent.fidelity.rank()
}

fn fidelity_eligible(contract: &AddInContract, intent: &EngineeringIntent) -> bool {
    !intent.strict_fidelity || !below_intent(contract, intent)
}

fn upstream(
    aid: &str,
    allowed: Option<&BTreeSet<String>>,
    seen: &mut BTreeSet<(String, Option<Vec<String>>)>,
    entries: &BTreeMap<String, Arc<RegisteredAddIn>>,
    incoming: &BTreeMap<String, Vec<CouplingEdge>>,
) -> BTreeSet<String> {
    let token = (aid.to_string(), allowed.map(|a| a.iter().cloned().collect::<Vec<_>>()));
    if !seen.insert(token) {
        return BTreeSet::new();
    }
    let mut reachable = BTreeSet::new();
    let Some(entry) = entries.get(aid) else { return reachable };
    let c = &entry.contract;
    for r in &c.design_inputs {
        let hit = allowed.is_none_or(|a| a.contains(&r.coordinate) || a.contains(&r.port_id));
        if c.exact_design_derivatives == Some(true) && hit {
            reachable.insert(r.coordinate.clone());
        }
    }
    for edge in incoming.get(aid).map(Vec::as_slice).unwrap_or_default() {
        if let Some(a) = allowed
            && !a.contains(&edge.quantity)
            && !a.contains(&edge.target_port_id)
        {
            continue;
        }
        reachable.extend(upstream(&edge.source, None, seen, entries, incoming));
    }
    reachable
}

#[derive(Debug, Clone, Copy, Default)]
pub struct OrchestrationPlanner;

struct Planning<'a> {
    intent: &'a EngineeringIntent,
    entries: BTreeMap<String, Arc<RegisteredAddIn>>,
    selected: BTreeSet<String>,
    edges: Vec<CouplingEdge>,
    blocked: Vec<String>,
    warnings: Vec<String>,
    provider_candidates: Map<String, Value>,
    resolving: BTreeSet<(String, PortKey)>,
}

impl Planning<'_> {
    fn authored_quantity(&mut self, need: &PortSpec) -> CaeResult<bool> {
        let key = need.key();
        let exact = self.intent.external_ports.iter().filter(|r| r.port.key() == key).count();
        if exact > 1 {
            return Err(CaeError::contract(format!(
                "ambiguous external values for port {}",
                need.key_repr()
            )));
        }
        if exact == 1 {
            return Ok(true);
        }
        if self.intent.contract_version == 1
            && compat::legacy_external_port_value(need, &self.intent.authoring).is_some()
        {
            self.warnings
                .push("legacy authored quantities were compatibility-bound to consumer ports".into());
            return Ok(true);
        }
        Ok(false)
    }

    fn satisfy(&mut self, target_addin: &str, need: &PortSpec) -> CaeResult<bool> {
        let Some(target_entry) = self.entries.get(target_addin).cloned() else { return Ok(false) };
        if target_entry.contract.design_inputs.iter().any(|r| r.port_id == need.port_id) {
            return Ok(true);
        }
        if target_entry.compatibility_mode && compat::legacy_design_port_matches(&target_entry.contract, need)
        {
            return Ok(true);
        }
        if self.authored_quantity(need)? {
            return Ok(true);
        }
        let token = (target_addin.to_string(), need.key());
        if self.resolving.contains(&token) {
            return Ok(true);
        }
        self.resolving.insert(token.clone());
        let result = self.satisfy_sources(target_addin, &target_entry, need);
        self.resolving.remove(&token);
        result
    }

    fn satisfy_sources(
        &mut self,
        target_addin: &str,
        target_entry: &Arc<RegisteredAddIn>,
        need: &PortSpec,
    ) -> CaeResult<bool> {
        let intent = self.intent;
        let upstream_override = intent
            .provider_overrides
            .get(&need.port_id)
            .filter(|s| !s.is_empty())
            .or_else(|| intent.provider_overrides.get(&need.quantity).filter(|s| !s.is_empty()))
            .cloned();
        let mut candidates: Vec<(i64, String, PortSpec)> = Vec::new();
        let mut available: BTreeSet<String> = BTreeSet::new();
        let mut fidelity_rejected: Vec<String> = Vec::new();
        for (aid, e) in &self.entries {
            if aid == target_addin {
                continue;
            }
            for p in &e.contract.provides {
                let compat_mode = e.compatibility_mode && target_entry.compatibility_mode;
                if direct_port_match(p, need, compat_mode) {
                    available.insert(aid.clone());
                    if !fidelity_eligible(&e.contract, intent) {
                        fidelity_rejected.push(aid.clone());
                        continue;
                    }
                    if upstream_override.as_ref().is_none_or(|o| o == aid)
                        && (!e.adapter.as_ref().and_then(|a| a.provider()).is_some_and(|p| p.requires_explicit_selection())
                            || upstream_override.as_ref().is_some_and(|o| o == aid)) {
                        candidates.push((provider_score(&e.contract, intent, ""), aid.clone(), p.clone()));
                    }
                }
            }
        }
        let key = if need.port_id.is_empty() { need.quantity.clone() } else { need.port_id.clone() };
        self.provider_candidates.insert(key, json!(available.iter().collect::<Vec<_>>()));
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        if self.try_sources(target_addin, need, &candidates)? {
            return Ok(true);
        }
        let mut bridges: Vec<(i64, String, PortSpec)> = Vec::new();
        for (aid, e) in &self.entries {
            if aid == target_addin || e.contract.category != AddInCategory::Interface {
                continue;
            }
            if upstream_override.as_ref().is_some_and(|o| o != aid) {
                continue;
            }
            if !fidelity_eligible(&e.contract, intent) {
                if !fidelity_rejected.contains(aid) {
                    fidelity_rejected.push(aid.clone());
                }
                continue;
            }
            for p in &e.contract.provides {
                let compat_mode = e.compatibility_mode && target_entry.compatibility_mode;
                if direct_port_match(p, need, compat_mode) {
                    bridges.push((provider_score(&e.contract, intent, ""), aid.clone(), p.clone()));
                }
            }
        }
        bridges.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        if self.try_sources(target_addin, need, &bridges)? {
            return Ok(true);
        }
        if !fidelity_rejected.is_empty() {
            let rejected: BTreeSet<&String> = fidelity_rejected.iter().collect();
            let rejected: Vec<&&String> = rejected.iter().collect();
            self.blocked.push(format!(
                "port {}[{}] for {}: no source at or above requested fidelity {}; below-intent source(s) {} were not eligible (strict_fidelity)",
                need.quantity,
                need.domain,
                repr_str(target_addin),
                repr_str(intent.fidelity.as_str()),
                list_repr(&rejected)
            ));
        }
        Ok(false)
    }

    fn try_sources(
        &mut self,
        target_addin: &str,
        need: &PortSpec,
        sources: &[(i64, String, PortSpec)],
    ) -> CaeResult<bool> {
        for (_, aid, p) in sources {
            self.selected.insert(aid.clone());
            let consumes = self.entries.get(aid).map(|e| e.contract.consumes.clone()).unwrap_or_default();
            let mut ok = true;
            for upstream in &consumes {
                if !self.satisfy(aid, upstream)? {
                    ok = false;
                    break;
                }
            }
            if ok {
                self.edges.push(CouplingEdge::connect(aid, target_addin, p, need));
                return Ok(true);
            }
            self.selected.remove(aid);
        }
        Ok(false)
    }
}

impl OrchestrationPlanner {

    pub fn plan_snapshot(
        &self,
        intent: &EngineeringIntent,
        snapshot: &AddInRegistrySnapshot,
        sufficiency: &SufficiencyRegistry,
    ) -> CaeResult<OrchestrationPlan> {
        self.plan(intent, &snapshot.entries, Some(&snapshot.token), sufficiency)
    }



    #[allow(clippy::too_many_lines)]
    pub fn plan(
        &self,
        intent: &EngineeringIntent,
        rows: &[Arc<RegisteredAddIn>],
        token: Option<&RegistryBindingToken>,
        sufficiency: &SufficiencyRegistry,
    ) -> CaeResult<OrchestrationPlan> {
        let mut active_coordinates = intent.active_design_coordinates.clone();
        let mut explicit_limit = !active_coordinates.is_empty();
        if intent.contract_version == 1 {
            active_coordinates = compat::legacy_intent_active_design_coordinates(intent)?;
            explicit_limit = true;
        }
        if intent.contract_version == STRICT_CONTRACT_VERSION && token.is_none() {
            return Err(CaeError::contract(
                "strict orchestration requires an immutable registry snapshot token",
            ));
        }
        let entries: BTreeMap<String, Arc<RegisteredAddIn>> = rows
            .iter()
            .filter(|e| !intent.excluded_addins.contains(&e.contract.addin_id))
            .map(|e| (e.contract.addin_id.clone(), Arc::clone(e)))
            .collect();
        let mut p = Planning {
            intent,
            entries,
            selected: BTreeSet::new(),
            edges: Vec::new(),
            blocked: Vec::new(),
            warnings: Vec::new(),
            provider_candidates: Map::new(),
            resolving: BTreeSet::new(),
        };
        let mut response_providers = Map::new();
        let mut missing_physics: Vec<String> = Vec::new();
        let mut selection_reasons = Map::new();
        let mut root_deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

        for response in intent.requested_responses() {
            let mut candidates: Vec<(i64, String)> = Vec::new();
            let mut fidelity_rejected: Vec<String> = Vec::new();
            let mut optional_candidates: Vec<String> = Vec::new();
            for e in p.entries.values() {
                if !e.contract.responses.iter().any(|r| r.response == response) {
                    continue;
                }
                if e.adapter.as_ref().and_then(|a| a.provider()).is_some_and(|p| p.requires_explicit_selection()) {
                    optional_candidates.push(e.contract.addin_id.clone());
                }
                if !fidelity_eligible(&e.contract, intent) {
                    fidelity_rejected.push(e.contract.addin_id.clone());
                    continue;
                }
                candidates
                    .push((if e.adapter.as_ref().and_then(|a| a.provider()).is_some_and(|p| p.requires_explicit_selection())
                        && intent.provider_overrides.get(&response) != Some(&e.contract.addin_id) { -2_000_000_000 }
                        else { provider_score(&e.contract, intent, &response) }, e.contract.addin_id.clone()));
            }
            candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let mut presented: Vec<String> = candidates.iter().map(|c|c.1.clone()).collect();
            for id in optional_candidates { if !presented.contains(&id) { presented.push(id); } }
            p.provider_candidates.insert(response.clone(),json!(presented));
            if candidates.is_empty() || candidates[0].0 < -50_000_000 {
                missing_physics.push(format!("response:{response}"));
                if !fidelity_rejected.is_empty() {
                    let mut rejected = fidelity_rejected.clone();
                    rejected.sort();
                    p.blocked.push(format!(
                        "response {}: no provider at or above requested fidelity {}; below-intent provider(s) {} were not eligible (strict_fidelity)",
                        repr_str(&response),
                        repr_str(intent.fidelity.as_str()),
                        list_repr(&rejected)
                    ));
                }
                continue;
            }
            let aid = candidates[0].1.clone();
            let entry = Arc::clone(&p.entries[&aid]);
            let Some(cap) = entry.contract.responses.iter().find(|r| r.response == response).cloned() else {
                continue;
            };
            selection_reasons.insert(
                response.clone(),
                json!(format!(
                    "selected {aid} from {} compatible provider(s) by scope, requested fidelity, priority and any explicit expert override",
                    candidates.len()
                )),
            );
            if cap.differentiable != Some(true) {
                p.blocked.push(format!(
                    "response {} from {} has no explicit differentiability evidence",
                    repr_str(&response),
                    repr_str(&aid)
                ));
            }
            let reachable_claim =
                if entry.compatibility_mode { cap.topology_reachable } else { cap.design_reachable };
            if reachable_claim != Some(true) {
                p.blocked.push(format!(
                    "response {} from {} has no explicit design-reachability evidence",
                    repr_str(&response),
                    repr_str(&aid)
                ));
            }
            response_providers.insert(response.clone(), json!(aid));
            p.selected.insert(aid.clone());
            if !cap.depends_on.is_empty() {
                root_deps.entry(aid.clone()).or_default().extend(cap.depends_on.iter().cloned());
            }
            if !entry.compatibility_mode {
                let c = &entry.contract;
                let mut declared: BTreeSet<&str> = BTreeSet::new();
                declared.extend(c.consumes.iter().map(|x| x.port_id.as_str()));
                declared.extend(c.consumes.iter().map(|x| x.quantity.as_str()));
                declared.extend(c.design_inputs.iter().map(|x| x.port_id.as_str()));
                declared.extend(c.design_inputs.iter().map(|x| x.coordinate.as_str()));
                let unknown: BTreeSet<&str> =
                    cap.depends_on.iter().map(String::as_str).filter(|d| !declared.contains(d)).collect();
                if !unknown.is_empty() {
                    let unknown: Vec<&str> = unknown.into_iter().collect();
                    p.blocked.push(format!(
                        "response {} from {} has undeclared dependencies {}",
                        repr_str(&response),
                        repr_str(&aid),
                        list_repr(&unknown)
                    ));
                }
            }
        }

        if missing_physics.is_empty() {
            let mut cursor = 0;
            loop {
                let ids: Vec<String> = p.selected.iter().cloned().collect();
                if cursor >= ids.len() {
                    break;
                }
                let aid = ids[cursor].clone();
                cursor += 1;
                let active = root_deps.get(&aid).cloned();
                let consumes = p.entries.get(&aid).map(|e| e.contract.consumes.clone()).unwrap_or_default();
                for need in &consumes {
                    if let Some(deps) = &active
                        && !deps.contains(&need.quantity)
                        && !deps.contains(&need.port_id)
                    {
                        continue;
                    }
                    if !p.satisfy(&aid, need)? {
                        missing_physics.push(format!("port:{}[{}]", need.quantity, need.domain));
                    }
                }
            }
        }

        let mut seen_edges = BTreeSet::new();
        let mut edges: Vec<CouplingEdge> = Vec::new();
        for e in p.edges.drain(..) {
            if seen_edges.insert(e.clone().to_value().to_string()) {
                edges.push(e);
            }
        }
        edges.sort_by(|a, b| {
            (
                &a.source,
                &a.target,
                &a.quantity,
                &a.source_domain,
                &a.target_domain,
                a.interface.clone().unwrap_or_else(|| "None".into()),
                &a.temporal,
                a.conserved,
                &a.source_port_id,
                &a.target_port_id,
            )
                .cmp(&(
                    &b.source,
                    &b.target,
                    &b.quantity,
                    &b.source_domain,
                    &b.target_domain,
                    b.interface.clone().unwrap_or_else(|| "None".into()),
                    &b.temporal,
                    b.conserved,
                    &b.source_port_id,
                    &b.target_port_id,
                ))
        });

        let selected = p.selected.clone();
        let entries = p.entries.clone();
        let mut blocked = std::mem::take(&mut p.blocked);
        let mut warnings = std::mem::take(&mut p.warnings);

        let mut instant: BTreeMap<String, BTreeSet<String>> =
            selected.iter().map(|a| (a.clone(), BTreeSet::new())).collect();
        for e in &edges {
            if e.temporal == "instantaneous" && selected.contains(&e.source) && selected.contains(&e.target) {
                instant.entry(e.source.clone()).or_default().insert(e.target.clone());
            }
        }
        for comp in crate::graph::strongly_connected(&selected, &instant) {
            let residual_closed = comp
                .iter()
                .all(|a| entries[a].adapter.as_ref().is_some_and(|ad| ad.has_residual_contributions()));
            let lifecycles: BTreeSet<Option<String>> =
                comp.iter().map(|a| entries[a].contract.lifecycle.clone()).collect();
            let lifecycle_closed = lifecycles.len() == 1
                && !lifecycles.contains(&None)
                && comp.iter().any(|a| entries[a].contract.runtime_route == RuntimeRoute::MatureJob);
            if !(residual_closed || lifecycle_closed) {
                let names: Vec<&String> = comp.iter().collect();
                blocked.push(format!(
                    "instantaneous feedback loop {} lacks a common residual/adjoint or authoritative lifecycle implementation",
                    list_repr(&names)
                ));
            }
        }

        for aid in &selected {
            let e = &entries[aid];
            let c = &e.contract;
            if c.exact_design_derivatives != Some(true) || c.exact_state_transpose != Some(true) {
                blocked.push(format!(
                    "add-in {} cannot participate in exact direct-gradient optimization",
                    repr_str(aid)
                ));
            }
            if e.compatibility_mode {
                warnings.push(format!(
                    "add-in {} uses an explicitly labeled legacy compatibility contract",
                    repr_str(aid)
                ));
                blocked.push(format!(
                    "add-in {} compatibility metadata cannot establish authoritative exactness; publish a strict v2 contract",
                    repr_str(aid)
                ));
            }
        }

        let mut incoming: BTreeMap<String, Vec<CouplingEdge>> =
            selected.iter().map(|a| (a.clone(), Vec::new())).collect();
        for e in &edges {
            if let Some(v) = incoming.get_mut(&e.target) {
                v.push(e.clone());
            }
        }
        let deps_of = |response: &str, aid: &str| -> Option<BTreeSet<String>> {
            entries[aid]
                .contract
                .responses
                .iter()
                .find(|r| r.response == response)
                .filter(|r| !r.depends_on.is_empty())
                .map(|r| r.depends_on.iter().cloned().collect())
        };
        let mut reachability: Vec<(String, BTreeSet<String>)> = Vec::new();
        for (response, aid) in &response_providers {
            let aid = aid.as_str().unwrap_or_default();
            let deps = deps_of(response, aid);
            let mut seen = BTreeSet::new();
            reachability
                .push((response.clone(), upstream(aid, deps.as_ref(), &mut seen, &entries, &incoming)));
        }
        let union: BTreeSet<String> = reachability.iter().flat_map(|(_, r)| r.iter().cloned()).collect();
        let mut ordered: Vec<String> = Vec::new();
        for aid in &selected {
            for r in &entries[aid].contract.design_inputs {
                if union.contains(&r.coordinate) && !ordered.contains(&r.coordinate) {
                    ordered.push(r.coordinate.clone());
                }
            }
        }
        if !explicit_limit {
            active_coordinates.clone_from(&ordered);
            if !active_coordinates.is_empty() {
                warnings.push(format!(
                    "active_design_coordinates omitted; activated all {} provider-declared reachable coordinates",
                    active_coordinates.len()
                ));
            } else if !response_providers.is_empty() {
                blocked.push(
                    "selected differentiable provider graph declares no reachable design coordinates".into(),
                );
            }
        }
        let inactive: Vec<String> = if explicit_limit {
            ordered.iter().filter(|c| !active_coordinates.contains(c)).cloned().collect()
        } else {
            Vec::new()
        };
        if !inactive.is_empty() {
            warnings.push(format!(
                "explicit design-coordinate limitation keeps {} provider-declared reachable coordinate(s) fixed: {}",
                inactive.len(),
                inactive.join(", ")
            ));
        }
        for (response, reachable) in &reachability {
            let aid = response_providers[response].as_str().unwrap_or_default().to_string();
            if !selected.contains(&aid) {
                continue;
            }
            let missing: Vec<&String> =
                active_coordinates.iter().filter(|c| !reachable.contains(*c)).collect();
            if explicit_limit && !missing.is_empty() {
                blocked.push(format!(
                    "response {} from {} has no physical derivative path from active design coordinates {} through its declared response dependencies",
                    repr_str(response),
                    repr_str(&aid),
                    list_repr(&missing)
                ));
            }
        }

        let mut missing_authoring: Vec<String> = Vec::new();
        if missing_physics.is_empty() {
            for aid in &selected {
                for req in &entries[aid].contract.authoring {
                    if req.optional {
                        continue;
                    }
                    let mut cur = Value::Object(intent.authoring.clone());
                    let mut present = true;
                    for part in req.key.split('.') {
                        let Some(next) = cur.as_object().and_then(|m| m.get(part)).cloned() else {
                            present = false;
                            break;
                        };
                        cur = next;
                    }
                    if !present {
                        missing_authoring.push(format!("{aid}:{}", req.key));
                    }
                }
            }
        }

        let roots: Vec<&AddInContract> = selected
            .iter()
            .map(|a| &entries[a].contract)
            .filter(|c| c.runtime_route == RuntimeRoute::MatureJob)
            .collect();
        let extensions: Vec<&AddInContract> = selected
            .iter()
            .map(|a| &entries[a].contract)
            .filter(|c| c.runtime_route == RuntimeRoute::JobExtension)
            .collect();
        let runtime_route = if let Some(first) = roots.first() {
            let life = first.lifecycle.clone();
            if roots.len() > 1 && roots.iter().any(|r| r.lifecycle != life) {
                blocked.push("selected mature-job add-ins have incompatible lifecycle owners".into());
            }
            if extensions.iter().any(|e| e.lifecycle != life) {
                blocked.push("job extension lifecycle does not match mature-job owner".into());
            }
            format!("mature_job:{}", life.unwrap_or_else(|| "None".into()))
        } else if selected.iter().any(|a| entries[a].contract.runtime_route == RuntimeRoute::Array) {
            "array-composite".into()
        } else {
            "composite".into()
        };

        let below: Vec<&String> =
            selected.iter().filter(|a| below_intent(&entries[*a].contract, intent)).collect();
        let delivered = selected
            .iter()
            .map(|a| entries[a].contract.fidelity)
            .min_by_key(|f| f.rank())
            .unwrap_or(Fidelity::Screening);
        if !below.is_empty() {
            if intent.strict_fidelity {
                blocked.push(format!(
                    "fidelity firewall: requested {} but selected add-in(s) {} deliver at most {}",
                    repr_str(intent.fidelity.as_str()),
                    list_repr(&below),
                    repr_str(delivered.as_str())
                ));
            } else {
                for aid in &below {
                    warnings.push(format!(
                        "fidelity downgrade accepted (strict_fidelity=False): requested {}, add-in {} delivers {}",
                        repr_str(intent.fidelity.as_str()),
                        repr_str(aid),
                        repr_str(entries[*aid].contract.fidelity.as_str())
                    ));
                }
            }
        }

        let mut status = if !missing_physics.is_empty() || !blocked.is_empty() {
            PlanStatus::Blocked
        } else if !missing_authoring.is_empty() {
            PlanStatus::NeedsAuthoring
        } else {
            PlanStatus::Ready
        };
        let missing_physics: Vec<String> =
            missing_physics.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        let missing_authoring: Vec<String> =
            missing_authoring.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        let mut plan = OrchestrationPlan {
            status,
            selected_addins: selected.iter().cloned().collect(),
            response_providers,
            coupling_edges: edges,
            missing_physics,
            missing_authoring,
            blocked_reasons: blocked.clone(),
            warnings,
            runtime_route,
            active_design_coordinates: active_coordinates,
            inactive_design_coordinates: inactive,
            fidelity: intent.fidelity.as_str().to_string(),
            provenance: default_provenance(),
            provider_candidates: p.provider_candidates,
            selection_reasons,
            sufficiency: json!({"ok": true, "issues": [], "evidence": {}}),
            physics_provenance: json!({}),
            binding_token: token.cloned(),
            delivered_fidelity: delivered,
        };
        plan.validate()?;
        let suff = audit_sufficiency(intent, &plan, &entries, sufficiency, token);
        let provenance = build_plan_provenance(intent, &plan, &entries, token);
        if status == PlanStatus::Ready && suff.get("ok") != Some(&Value::Bool(true)) {
            status = PlanStatus::Blocked;
            if let Some(issues) = suff.get("issues").and_then(Value::as_array) {
                for issue in issues {
                    blocked.push(format!("physics sufficiency: {}", issue.as_str().unwrap_or_default()));
                }
            }
        }
        plan.status = status;
        plan.blocked_reasons = blocked;
        plan.sufficiency = suff;
        plan.physics_provenance = provenance;
        Ok(plan)
    }


    pub fn plan_fidelity_ladder(
        &self,
        intent: &EngineeringIntent,
        snapshot: &AddInRegistrySnapshot,
        sufficiency: &SufficiencyRegistry,
    ) -> CaeResult<Map<String, Value>> {
        let mut rows = Map::new();
        for fid in Fidelity::ALL {
            let mut rung = intent.clone();
            rung.fidelity = *fid;
            rows.insert(fid.as_str().into(), self.plan_snapshot(&rung, snapshot, sufficiency)?.as_dict());
        }
        Ok(rows)
    }
}
