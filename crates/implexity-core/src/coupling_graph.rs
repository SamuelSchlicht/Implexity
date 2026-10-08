// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::contracts::{CaeProvider, ProviderProblem, provider_key};
use crate::extensions::{CouplingRule, ExtensionRegistry};
use crate::pyobj::{py_str, truthy};

pub const COUPLING_MODES: [&str; 4] = ["one_way", "iterative", "monolithic", "equality"];
pub const CLOSED_COUPLING_MODES: [&str; 3] = ["iterative", "monolithic", "equality"];
pub const EQUALITY_MODE: &str = "equality";
pub const REPORT_SCHEMA: &str = "implexity-physics-coupling-report/1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CouplingContractError(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PhysicsPort {
    pub name: String,
    pub owner: String,
    pub direction: String,
    pub conserved: bool,
    pub units: String,
}

impl PhysicsPort {
    fn from_value(v: &Value) -> Result<Self, CouplingContractError> {
        let m =
            v.as_object().ok_or_else(|| CouplingContractError("physics port must be a mapping".into()))?;
        for k in m.keys() {
            if !["name", "owner", "direction", "conserved", "units"].contains(&k.as_str()) {
                return Err(CouplingContractError(format!(
                    "PhysicsPort.__init__() got an unexpected keyword argument {}",
                    crate::py_repr::repr_str(k)
                )));
            }
        }
        let text = |k: &str| -> Result<String, CouplingContractError> {
            m.get(k).map(py_str).ok_or_else(|| {
                CouplingContractError(format!(
                    "PhysicsPort.__init__() missing 1 required positional argument: {}",
                    crate::py_repr::repr_str(k)
                ))
            })
        };
        Ok(Self {
            name: text("name")?,
            owner: text("owner")?,
            direction: text("direction")?,
            conserved: m.get("conserved").is_some_and(truthy),
            units: m.get("units").map(py_str).unwrap_or_default(),
        })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"name": self.name, "owner": self.owner, "direction": self.direction,
               "conserved": self.conserved, "units": self.units})
    }
}


#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CouplingEdge {
    pub source: String,
    pub target: String,
    pub quantity: String,
    pub mode: String,
    pub required: bool,
    pub reason: String,
}

impl CouplingEdge {

    pub fn new(
        source: &str,
        target: &str,
        quantity: &str,
        mode: &str,
        required: bool,
        reason: &str,
    ) -> Result<Self, CouplingContractError> {
        if !COUPLING_MODES.contains(&mode) {
            return Err(CouplingContractError(format!(
                "coupling edge {source}->{target}:{quantity} has unknown mode {}; admitted modes are {}",
                crate::py_repr::repr_str(mode),
                crate::pyobj::list_repr(&COUPLING_MODES)
            )));
        }
        Ok(Self {
            source: source.into(),
            target: target.into(),
            quantity: quantity.into(),
            mode: mode.into(),
            required,
            reason: reason.into(),
        })
    }

    #[must_use]
    pub fn closes_loop(&self) -> bool {
        CLOSED_COUPLING_MODES.contains(&self.mode.as_str())
    }

    #[must_use]
    pub fn is_equality(&self) -> bool {
        self.mode == EQUALITY_MODE
    }

    fn from_value(v: &Value) -> Result<Self, CouplingContractError> {
        let m =
            v.as_object().ok_or_else(|| CouplingContractError("coupling edge must be a mapping".into()))?;
        for k in m.keys() {
            if !["source", "target", "quantity", "mode", "required", "reason"].contains(&k.as_str()) {
                return Err(CouplingContractError(format!(
                    "CouplingEdge.__init__() got an unexpected keyword argument {}",
                    crate::py_repr::repr_str(k)
                )));
            }
        }
        let text = |k: &str| -> Result<String, CouplingContractError> {
            m.get(k).map(py_str).ok_or_else(|| {
                CouplingContractError(format!(
                    "CouplingEdge.__init__() missing 1 required positional argument: {}",
                    crate::py_repr::repr_str(k)
                ))
            })
        };
        Self::new(
            &text("source")?,
            &text("target")?,
            &text("quantity")?,
            &m.get("mode").map_or_else(|| "one_way".to_string(), py_str),
            m.get("required").is_none_or(truthy),
            &m.get("reason").map(py_str).unwrap_or_default(),
        )
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"source": self.source, "target": self.target, "quantity": self.quantity,
               "mode": self.mode, "required": self.required, "reason": self.reason})
    }

    fn key(&self) -> (String, String, String) {
        (self.source.clone(), self.target.clone(), self.quantity.clone())
    }

    fn freeze_key(&self) -> String {
        format!("{}->{}:{}", self.source, self.target, self.quantity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CouplingDeclaration {
    pub provider: String,
    pub active_physics: Vec<String>,
    pub ports: Vec<PhysicsPort>,
    pub edges: Vec<CouplingEdge>,
    pub closed_loops: Vec<Vec<String>>,
    pub intentionally_frozen: Vec<String>,
    pub notes: Vec<String>,
}

fn strs(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items.iter().map(py_str).collect(),
        Some(Value::String(s)) => s.chars().map(|c| c.to_string()).collect(),
        _ => Vec::new(),
    }
}

impl CouplingDeclaration {

    pub fn from_value(value: &Value) -> Result<Self, CouplingContractError> {
        let Some(m) = value.as_object() else {
            return Err(CouplingContractError("coupling declaration must be a mapping".into()));
        };
        let ports = m
            .get("ports")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(PhysicsPort::from_value).collect::<Result<Vec<_>, _>>())
            .transpose()?
            .unwrap_or_default();
        let edges = m
            .get("edges")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(CouplingEdge::from_value).collect::<Result<Vec<_>, _>>())
            .transpose()?
            .unwrap_or_default();
        let loops = m
            .get("closed_loops")
            .or_else(|| m.get("closedLoops"))
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|l| strs(Some(l))).collect())
            .unwrap_or_default();
        Ok(Self {
            provider: m.get("provider").filter(|v| truthy(v)).map(py_str).unwrap_or_default(),
            active_physics: strs(m.get("active_physics").or_else(|| m.get("activePhysics"))),
            ports,
            edges,
            closed_loops: loops,
            intentionally_frozen: strs(
                m.get("intentionally_frozen").or_else(|| m.get("intentionallyFrozen")),
            ),
            notes: strs(m.get("notes")),
        })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "provider": self.provider,
            "active_physics": self.active_physics,
            "ports": self.ports.iter().map(PhysicsPort::to_value).collect::<Vec<_>>(),
            "edges": self.edges.iter().map(CouplingEdge::to_value).collect::<Vec<_>>(),
            "closed_loops": self.closed_loops,
            "intentionally_frozen": self.intentionally_frozen,
            "notes": self.notes,
        })
    }
}

#[must_use]
pub fn expected_edges(rules: &[CouplingRule], active_physics: &BTreeSet<String>) -> Vec<CouplingEdge> {
    let mut out: Vec<CouplingEdge> = Vec::new();
    for rule in rules {
        if rule.trigger.is_subset(active_physics) {
            for e in &rule.edges {
                if !out.contains(e) {
                    out.push(e.clone());
                }
            }
        }
    }
    out
}

fn issue(code: &str, message: &str, extra: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    m.insert("code".into(), json!(code));
    m.insert("message".into(), json!(message));
    for (k, v) in extra {
        m.insert((*k).into(), v.clone());
    }
    Value::Object(m)
}

fn dfs(
    node: &str,
    path: &mut Vec<String>,
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    if visiting.contains(node) {
        let i = path.iter().position(|p| p == node).unwrap_or(0);
        let mut cyc: Vec<String> = path[i..].to_vec();
        cyc.push(node.to_string());
        if !cycles.contains(&cyc) {
            cycles.push(cyc);
        }
        return;
    }
    if visited.contains(node) {
        return;
    }
    visiting.insert(node.to_string());
    path.push(node.to_string());
    if let Some(next) = adjacency.get(node) {
        for n in next {
            dfs(n, path, adjacency, visiting, visited, cycles);
        }
    }
    path.pop();
    visiting.remove(node);
    visited.insert(node.to_string());
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn validate_declaration(
    d: &CouplingDeclaration,
    rules: &[CouplingRule],
    for_optimization: bool,
) -> Value {
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    if d.provider.is_empty() {
        errors.push(issue("COUPLING_PROVIDER_MISSING", "coupling declaration has no provider id", &[]));
    }
    let active: BTreeSet<String> = d.active_physics.iter().cloned().collect();
    let mut supplied: BTreeMap<(String, String, String), &CouplingEdge> = BTreeMap::new();
    for e in &d.edges {
        supplied.insert(e.key(), e);
    }
    let expected = expected_edges(rules, &active);
    let frozen: BTreeSet<&String> = d.intentionally_frozen.iter().collect();
    for edge in &expected {
        if !edge.required {
            continue;
        }
        let fk = edge.freeze_key();
        let push = |item: Value, errors: &mut Vec<Value>, warnings: &mut Vec<Value>| {
            if for_optimization { errors.push(item) } else { warnings.push(item) }
        };
        match supplied.get(&edge.key()) {
            None => {
                if frozen.contains(&fk) {
                    warnings.push(issue(
                        "PHYSICS_COUPLING_FROZEN",
                        &format!(
                            "Required interaction {fk} is intentionally frozen; result is reduced fidelity."
                        ),
                        &[("edge", json!(fk))],
                    ));
                    if for_optimization {
                        errors.push(issue(
                            "OPTIMIZATION_REQUIRES_CLOSED_COUPLING",
                            &format!("Optimization may not vary a model with frozen required coupling {fk}."),
                            &[],
                        ));
                    }
                } else {
                    let item = issue(
                        "MISSING_PHYSICS_COUPLING",
                        &format!("Missing required coupling {fk}"),
                        &[("edge", json!(fk)), ("reason", json!(edge.reason))],
                    );
                    push(item, &mut errors, &mut warnings);
                }
            }
            Some(got) if got.mode == "one_way" && edge.closes_loop() => {
                let item = issue(
                    "ONE_WAY_FEEDBACK_LOOP",
                    &format!(
                        "{fk} is declared one-way but registered application policy requires {} coupling.",
                        edge.mode
                    ),
                    &[("edge", json!(fk))],
                );
                push(item, &mut errors, &mut warnings);
            }
            Some(got) if edge.is_equality() && got.mode != EQUALITY_MODE => {
                let item = issue(
                    "EQUALITY_COUPLING_REQUIRED",
                    &format!(
                        "{fk} is declared {} but registered application policy requires an equality constraint.",
                        got.mode
                    ),
                    &[("edge", json!(fk))],
                );
                push(item, &mut errors, &mut warnings);
            }
            Some(_) => {}
        }
    }
    let mut adjacency: BTreeMap<String, BTreeSet<String>> =
        active.iter().map(|p| (p.clone(), BTreeSet::new())).collect();
    for e in &d.edges {
        if e.required && active.contains(&e.source) && active.contains(&e.target) {
            adjacency.entry(e.source.clone()).or_default().insert(e.target.clone());
            if e.is_equality() {
                adjacency.entry(e.target.clone()).or_default().insert(e.source.clone());
            }
        }
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut cycles: Vec<Vec<String>> = Vec::new();
    for node in &active {
        dfs(node, &mut Vec::new(), &adjacency, &mut visiting, &mut visited, &mut cycles);
    }
    let declared: Vec<BTreeSet<&String>> = d.closed_loops.iter().map(|l| l.iter().collect()).collect();
    for cyc in &cycles {
        let nodes: BTreeSet<&String> = cyc.iter().collect();
        let all_closed = d
            .edges
            .iter()
            .filter(|e| nodes.contains(&e.source) && nodes.contains(&e.target) && e.required)
            .all(CouplingEdge::closes_loop);
        if all_closed && !declared.iter().any(|s| nodes.is_subset(s)) {
            let names: Vec<&String> = nodes.iter().copied().collect();
            let item = issue(
                "UNDECLARED_FEEDBACK_CLOSURE",
                &format!(
                    "Feedback loop {} exists but is not declared as a closed coupled solve.",
                    crate::pyobj::list_repr(&names)
                ),
                &[("physics", json!(names))],
            );
            if for_optimization { errors.push(item) } else { warnings.push(item) }
        }
    }
    let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    for e in d.edges.iter().filter(|e| e.is_equality()) {
        let (a, b) = if e.source <= e.target { (&e.source, &e.target) } else { (&e.target, &e.source) };
        let unordered = (a.clone(), b.clone(), e.quantity.clone());
        if seen.contains(&unordered) {
            let item = issue(
                "EQUALITY_CONSTRAINT_REDUNDANT",
                &format!(
                    "Equality constraint {}=={}:{} is declared more than once; the constraint Jacobian is rank deficient.",
                    e.source, e.target, e.quantity
                ),
                &[("edge", json!(e.freeze_key()))],
            );
            if for_optimization { errors.push(item) } else { warnings.push(item) }
        }
        seen.insert(unordered);
    }
    json!({
        "ok": errors.is_empty(),
        "schema": REPORT_SCHEMA,
        "provider": d.provider,
        "activePhysics": active.iter().collect::<Vec<_>>(),
        "requiredEdges": expected.iter().filter(|e| e.required).map(CouplingEdge::to_value).collect::<Vec<_>>(),
        "declaredEdges": d.edges.iter().map(CouplingEdge::to_value).collect::<Vec<_>>(),
        "closedLoops": d.closed_loops,
        "errors": errors,
        "warnings": warnings,
    })
}


pub fn declaration_for_provider(
    provider: &dyn CaeProvider,
    problem: Option<&ProviderProblem>,
) -> Result<CouplingDeclaration, CouplingContractError> {
    if let Some(d) = provider.coupling_declaration(problem) {
        let d = d.map_err(|e| CouplingContractError(e.message().to_string()))?;
        return CouplingDeclaration::from_value(&d);
    }
    let caps = provider.capabilities().map_err(|e| CouplingContractError(e.message().to_string()))?;
    let row = caps.to_map();
    if let Some(d) = row.get("couplingDeclaration").filter(|v| truthy(v))
        && matches!(caps, crate::contracts::ProviderCapabilities::Mapping(_))
    {
        return CouplingDeclaration::from_value(d);
    }
    let isolated = match &caps {
        crate::contracts::ProviderCapabilities::Mapping(m) => m.get("isolatedPhysics").is_some_and(truthy),
        _ => row.get("isolated_physics").is_some_and(truthy),
    };
    if isolated {
        let name = provider_key(provider);
        let name = if name.is_empty() { "provider".to_string() } else { name };
        return Ok(CouplingDeclaration {
            provider: name.clone(),
            active_physics: vec![name],
            notes: vec!["provider declares isolated physics".into()],
            ..CouplingDeclaration::default()
        });
    }
    Err(CouplingContractError("provider does not declare multiphysics coupling semantics".into()))
}

fn failure(provider: &dyn CaeProvider, code: &str, message: &str) -> Value {
    let name = provider_key(provider);
    json!({"ok": false, "schema": REPORT_SCHEMA, "provider": if name.is_empty() { "provider".to_string() } else { name },
           "activePhysics": [], "requiredEdges": [], "declaredEdges": [], "closedLoops": [],
           "errors": [{"code": code, "message": message}], "warnings": []})
}

#[must_use]
pub fn validate_provider_couplings(
    provider: &dyn CaeProvider,
    problem: Option<&ProviderProblem>,
    extensions: &ExtensionRegistry,
    for_optimization: bool,
) -> Value {
    if let Some(custom) = provider.coupling_validation(problem, for_optimization) {
        return match custom {
            Ok(Value::Object(mut out)) => {
                let name = provider_key(provider);
                let defaults = [
                    ("schema", json!(REPORT_SCHEMA)),
                    ("provider", json!(if name.is_empty() { "provider".to_string() } else { name })),
                    ("activePhysics", json!([])),
                    ("requiredEdges", json!([])),
                    ("declaredEdges", json!([])),
                    ("closedLoops", json!([])),
                    ("errors", json!([])),
                    ("warnings", json!([])),
                ];
                for (k, v) in defaults {
                    if !out.contains_key(k) {
                        out.insert(k.into(), v);
                    }
                }
                let has_errors = out.get("errors").is_some_and(truthy);
                let ok = out.get("ok").map_or(!has_errors, truthy);
                out.insert("ok".into(), json!(ok && !has_errors));
                Value::Object(out)
            }
            Ok(_) => failure(
                provider,
                "PHYSICS_COUPLING_CONTRACT_INVALID",
                "custom coupling_validation must return a mapping",
            ),
            Err(message) => failure(provider, "PHYSICS_COUPLING_CONTRACT_INVALID", &message),
        };
    }
    match declaration_for_provider(provider, problem) {
        Ok(d) => validate_declaration(&d, &extensions.coupling_rules(), for_optimization),
        Err(e) => failure(provider, "PHYSICS_COUPLING_CONTRACT_MISSING", &e.0),
    }
}

