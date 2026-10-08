// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use crate::contracts::{CaeProvider, ProviderProblem};
use crate::error::{CaeError, CaeResult};
use crate::pyobj::{list_repr, py_str, truthy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quantity {
    pub name: String,
    pub role: String,
    pub units: String,
    pub conserved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    pub name: String,
    pub reads: Vec<String>,
    pub writes: Vec<String>,
    pub kind: String,
    pub temporal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConservationBalance {
    pub quantity: String,
    pub storage: Vec<String>,
    pub fluxes: Vec<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceExchange {
    pub quantity: String,
    pub side_a: String,
    pub side_b: String,
    pub conservative: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SemanticPhysicsContract {
    pub provider: String,
    pub quantities: Vec<Quantity>,
    pub operators: Vec<Operator>,
    pub balances: Vec<ConservationBalance>,
    pub interfaces: Vec<InterfaceExchange>,
    pub design_affects: Vec<Vec<String>>,
    pub active_responses: Vec<String>,
    pub exact_derivative_edges: Vec<Vec<String>>,
    pub coupled_groups: Vec<Vec<String>>,
}

fn strs(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().map(py_str).collect(),
        Some(Value::String(s)) => s.chars().map(|c| c.to_string()).collect(),
        _ => Vec::new(),
    }
}

fn rows(v: Option<&Value>) -> Vec<Vec<String>> {
    v.and_then(Value::as_array).map(|a| a.iter().map(|r| strs(Some(r))).collect()).unwrap_or_default()
}

fn kw_error(class: &str, key: &str) -> CaeError {
    CaeError::contract(format!(
        "{class}.__init__() got an unexpected keyword argument {}",
        crate::py_repr::repr_str(key)
    ))
}

fn required<'a>(m: &'a Map<String, Value>, class: &str, key: &str) -> CaeResult<&'a Value> {
    m.get(key).ok_or_else(|| {
        CaeError::contract(format!(
            "{class}.__init__() missing 1 required positional argument: {}",
            crate::py_repr::repr_str(key)
        ))
    })
}

impl SemanticPhysicsContract {

    pub fn from_value(value: &Value) -> CaeResult<Self> {
        let Some(m) = value.as_object() else {
            return Err(CaeError::contract("semantic physics contract must be a mapping"));
        };
        let mut quantities = Vec::new();
        for q in m.get("quantities").and_then(Value::as_array).into_iter().flatten() {
            let q = q.as_object().ok_or_else(|| CaeError::contract("quantity must be a mapping"))?;
            if let Some(k) = q.keys().find(|k| !["name", "role", "units", "conserved"].contains(&k.as_str()))
            {
                return Err(kw_error("Quantity", k));
            }
            quantities.push(Quantity {
                name: py_str(required(q, "Quantity", "name")?),
                role: q.get("role").map_or_else(|| "state".into(), py_str),
                units: q.get("units").map(py_str).unwrap_or_default(),
                conserved: q.get("conserved").is_some_and(truthy),
            });
        }
        let mut operators = Vec::new();
        for o in m.get("operators").and_then(Value::as_array).into_iter().flatten() {
            let o = o.as_object().ok_or_else(|| CaeError::contract("operator must be a mapping"))?;
            operators.push(Operator {
                name: py_str(o.get("name").ok_or_else(|| CaeError::contract("'name'"))?),
                reads: strs(o.get("reads")),
                writes: strs(o.get("writes")),
                kind: o.get("kind").map_or_else(|| "residual".into(), py_str),
                temporal: o.get("temporal").map_or_else(|| "instantaneous".into(), py_str),
            });
        }
        let mut balances = Vec::new();
        for b in m.get("balances").and_then(Value::as_array).into_iter().flatten() {
            let b = b.as_object().ok_or_else(|| CaeError::contract("balance must be a mapping"))?;
            balances.push(ConservationBalance {
                quantity: py_str(b.get("quantity").ok_or_else(|| CaeError::contract("'quantity'"))?),
                storage: strs(b.get("storage")),
                fluxes: strs(b.get("fluxes")),
                sources: strs(b.get("sources")),
            });
        }
        let mut interfaces = Vec::new();
        for i in m.get("interfaces").and_then(Value::as_array).into_iter().flatten() {
            let i = i.as_object().ok_or_else(|| CaeError::contract("interface must be a mapping"))?;
            if let Some(k) =
                i.keys().find(|k| !["quantity", "side_a", "side_b", "conservative"].contains(&k.as_str()))
            {
                return Err(kw_error("InterfaceExchange", k));
            }
            interfaces.push(InterfaceExchange {
                quantity: py_str(required(i, "InterfaceExchange", "quantity")?),
                side_a: py_str(required(i, "InterfaceExchange", "side_a")?),
                side_b: py_str(required(i, "InterfaceExchange", "side_b")?),
                conservative: i.get("conservative").is_none_or(truthy),
            });
        }
        let raw_affects = m.get("design_affects").or_else(|| m.get("designAffects"));
        let design_affects = match raw_affects {
            Some(Value::Object(map)) => map
                .iter()
                .flat_map(|(coordinate, qs)| {
                    let list = match qs {
                        Value::Array(a) => a.iter().map(py_str).collect::<Vec<_>>(),
                        other => vec![py_str(other)],
                    };
                    list.into_iter().map(move |q| vec![coordinate.clone(), q])
                })
                .collect(),
            other => rows(other),
        };
        Ok(Self {
            provider: m.get("provider").filter(|v| truthy(v)).map(py_str).unwrap_or_default(),
            quantities,
            operators,
            balances,
            interfaces,
            design_affects,
            active_responses: strs(m.get("active_responses").or_else(|| m.get("activeResponses"))),
            exact_derivative_edges: rows(
                m.get("exact_derivative_edges").or_else(|| m.get("exactDerivativeEdges")),
            ),
            coupled_groups: rows(m.get("coupled_groups").or_else(|| m.get("coupledGroups"))),
        })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "provider": self.provider,
            "quantities": self.quantities.iter().map(|q| json!({"name": q.name, "role": q.role, "units": q.units, "conserved": q.conserved})).collect::<Vec<_>>(),
            "operators": self.operators.iter().map(|o| json!({"name": o.name, "reads": o.reads, "writes": o.writes, "kind": o.kind, "temporal": o.temporal})).collect::<Vec<_>>(),
            "balances": self.balances.iter().map(|b| json!({"quantity": b.quantity, "storage": b.storage, "fluxes": b.fluxes, "sources": b.sources})).collect::<Vec<_>>(),
            "interfaces": self.interfaces.iter().map(|i| json!({"quantity": i.quantity, "side_a": i.side_a, "side_b": i.side_b, "conservative": i.conservative})).collect::<Vec<_>>(),
            "design_affects": self.design_affects,
            "active_responses": self.active_responses,
            "exact_derivative_edges": self.exact_derivative_edges,
            "coupled_groups": self.coupled_groups,
        })
    }
}

const ROLES: [&str; 7] = ["state", "flux", "source", "constitutive", "geometry", "control", "response"];

fn python_tuple_list(rows: &[&Vec<String>]) -> String {
    let inner: Vec<String> = rows
        .iter()
        .map(|r| {
            let items: Vec<String> = r.iter().map(|s| crate::py_repr::repr_str(s)).collect();
            if items.len() == 1 { format!("({},)", items[0]) } else { format!("({})", items.join(", ")) }
        })
        .collect();
    format!("[{}]", inner.join(", "))
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn validate_semantic_physics(d: &SemanticPhysicsContract, for_optimization: bool) -> Value {
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let err = |e: &mut Vec<Value>, code: &str, msg: String| e.push(json!({"code": code, "message": msg}));
    if d.provider.is_empty() {
        err(&mut errors, "SEMANTIC_PROVIDER_MISSING", "provider id is required".into());
    }
    let qnames: BTreeSet<&String> = d.quantities.iter().map(|q| &q.name).collect();
    if qnames.len() != d.quantities.len() {
        err(&mut errors, "DUPLICATE_QUANTITY", "quantity names must be unique".into());
    }
    let onames: BTreeSet<&String> = d.operators.iter().map(|o| &o.name).collect();
    if onames.len() != d.operators.len() {
        err(&mut errors, "DUPLICATE_OPERATOR", "operator names must be unique".into());
    }
    for q in &d.quantities {
        if !ROLES.contains(&q.role.as_str()) {
            err(&mut errors, "UNKNOWN_QUANTITY_ROLE", format!("{}: unknown role {}", q.name, q.role));
        }
    }
    for o in &d.operators {
        let missing: BTreeSet<&String> =
            o.reads.iter().chain(&o.writes).filter(|x| !qnames.contains(x)).collect();
        if !missing.is_empty() {
            let m: Vec<&&String> = missing.iter().collect();
            err(
                &mut errors,
                "OPERATOR_UNKNOWN_QUANTITY",
                format!("{} refers to unknown quantities {}", o.name, list_repr(&m)),
            );
        }
        if o.writes.is_empty() {
            err(&mut errors, "OPERATOR_WITHOUT_OUTPUT", format!("{} writes no quantity", o.name));
        }
        if !["instantaneous", "lagged", "history"].contains(&o.temporal.as_str()) {
            err(&mut errors, "UNKNOWN_TEMPORAL_SEMANTICS", format!("{}: {}", o.name, o.temporal));
        }
    }
    let qby: BTreeMap<&String, &Quantity> = d.quantities.iter().map(|q| (&q.name, q)).collect();
    for b in &d.balances {
        if !qnames.contains(&b.quantity) {
            err(&mut errors, "BALANCE_UNKNOWN_QUANTITY", format!("balance for unknown {}", b.quantity));
            continue;
        }
        if !qby[&b.quantity].conserved {
            warnings.push(json!({"code": "BALANCE_QUANTITY_NOT_MARKED_CONSERVED",
                                 "message": format!("{} has a balance but is not marked conserved", b.quantity)}));
        }
        let members: BTreeSet<&String> = b.storage.iter().chain(&b.fluxes).chain(&b.sources).collect();
        if members.is_empty() {
            err(
                &mut errors,
                "EMPTY_CONSERVATION_BALANCE",
                format!("{} balance has no storage, flux or source evidence", b.quantity),
            );
        }
        let unknown: Vec<&&String> = members.iter().filter(|m| !qnames.contains(**m)).collect();
        if !unknown.is_empty() {
            err(
                &mut errors,
                "BALANCE_UNKNOWN_TERM",
                format!("{} balance uses unknown terms {}", b.quantity, list_repr(&unknown)),
            );
        }
    }
    let balanced: BTreeSet<&String> = d.balances.iter().map(|b| &b.quantity).collect();
    for q in &d.quantities {
        if q.conserved && !balanced.contains(&q.name) {
            err(
                &mut errors,
                "CONSERVATION_EVIDENCE_MISSING",
                format!("conserved quantity {} has no declared balance", q.name),
            );
        }
    }
    for i in &d.interfaces {
        if !qnames.contains(&i.quantity) || !onames.contains(&i.side_a) || !onames.contains(&i.side_b) {
            err(
                &mut errors,
                "INTERFACE_DECLARATION_INVALID",
                format!("interface {} must reference a known quantity and two known operators", i.quantity),
            );
        }
        if i.side_a == i.side_b {
            err(
                &mut errors,
                "INTERFACE_SIDES_IDENTICAL",
                format!("interface {} requires distinct sides", i.quantity),
            );
        }
    }
    let mut graph: BTreeMap<&String, BTreeSet<&String>> =
        qnames.iter().map(|q| (*q, BTreeSet::new())).collect();
    let mut reverse: BTreeMap<&String, BTreeSet<&String>> =
        qnames.iter().map(|q| (*q, BTreeSet::new())).collect();
    for o in &d.operators {
        for a in &o.reads {
            for b in &o.writes {
                if let Some(s) = graph.get_mut(a) {
                    s.insert(b);
                }
                if let Some(s) = reverse.get_mut(b) {
                    s.insert(a);
                }
            }
        }
    }
    let malformed: Vec<&Vec<String>> =
        d.design_affects.iter().filter(|r| r.len() != 2 || r[0].is_empty() || r[1].is_empty()).collect();
    if !malformed.is_empty() {
        err(
            &mut errors,
            "DESIGN_AFFECT_EDGE_INVALID",
            format!(
                "design-affect edges must contain coordinate and quantity ids: {}",
                python_tuple_list(&malformed)
            ),
        );
    }
    let unique: BTreeSet<&Vec<String>> = d.design_affects.iter().collect();
    if unique.len() != d.design_affects.len() {
        err(&mut errors, "DUPLICATE_DESIGN_AFFECT_EDGE", "design-affect edges must be unique".into());
    }
    let mut coordinates: Vec<&String> = Vec::new();
    for r in d.design_affects.iter().filter(|r| r.len() == 2) {
        if !coordinates.contains(&&r[0]) {
            coordinates.push(&r[0]);
        }
    }
    let targets: BTreeSet<&String> = d
        .design_affects
        .iter()
        .filter(|r| r.len() == 2)
        .map(|r| &r[1])
        .filter(|q| !qnames.contains(q))
        .collect();
    if !targets.is_empty() {
        let t: Vec<&&String> = targets.iter().collect();
        err(
            &mut errors,
            "DESIGN_TARGET_UNKNOWN",
            format!("design coordinates affect unknown quantities {}", list_repr(&t)),
        );
    }
    let unknown_resp: BTreeSet<&String> = d.active_responses.iter().filter(|r| !qnames.contains(r)).collect();
    if !unknown_resp.is_empty() {
        let u: Vec<&&String> = unknown_resp.iter().collect();
        err(&mut errors, "ACTIVE_RESPONSE_UNKNOWN", format!("active responses unknown: {}", list_repr(&u)));
    }
    let mut reachable_by: Vec<(&String, BTreeSet<&String>)> = Vec::new();
    for coordinate in &coordinates {
        let mut reachable: BTreeSet<&String> =
            d.design_affects.iter().filter(|r| r.len() == 2 && r[0] == **coordinate).map(|r| &r[1]).collect();
        let mut frontier: Vec<&String> = reachable.iter().copied().collect();
        while let Some(a) = frontier.pop() {
            for b in graph.get(a).into_iter().flatten() {
                if reachable.insert(b) {
                    frontier.push(b);
                }
            }
        }
        for response in &d.active_responses {
            if !reachable.contains(response) {
                let msg = format!(
                    "active response {response} has no semantic path from design coordinate {}",
                    crate::py_repr::repr_str(coordinate)
                );
                let item = json!({"code": "ACTIVE_RESPONSE_NOT_DESIGN_REACHABLE", "message": msg});
                if for_optimization { errors.push(item) } else { warnings.push(item) }
            }
        }
        reachable_by.push((coordinate, reachable));
    }
    if for_optimization {
        let deriv: BTreeSet<(&str, &str)> = d
            .exact_derivative_edges
            .iter()
            .filter(|e| e.len() == 2)
            .map(|e| (e[0].as_str(), e[1].as_str()))
            .collect();
        let mut needed: BTreeSet<&String> = d.active_responses.iter().collect();
        let mut frontier: Vec<&String> = needed.iter().copied().collect();
        while let Some(b) = frontier.pop() {
            for a in reverse.get(b).into_iter().flatten() {
                if needed.insert(a) {
                    frontier.push(a);
                }
            }
        }
        for (coordinate, reachable) in &reachable_by {
            for r in d.design_affects.iter().filter(|r| r.len() == 2) {
                if r[0] == **coordinate
                    && needed.contains(&r[1])
                    && !deriv.contains(&(r[0].as_str(), r[1].as_str()))
                {
                    err(
                        &mut errors,
                        "EXACT_SEMANTIC_DERIVATIVE_MISSING",
                        format!("exact derivative edge missing for {}->{}", r[0], r[1]),
                    );
                }
            }
            for a in reachable.iter().filter(|a| needed.contains(**a)) {
                for b in graph.get(a).into_iter().flatten() {
                    if needed.contains(b) && !deriv.contains(&(a.as_str(), b.as_str())) {
                        err(
                            &mut errors,
                            "EXACT_SEMANTIC_DERIVATIVE_MISSING",
                            format!("exact derivative edge missing for {a}->{b}"),
                        );
                    }
                }
            }
        }
    }
    let mut adj: BTreeMap<&String, BTreeSet<&String>> =
        qnames.iter().map(|q| (*q, BTreeSet::new())).collect();
    for o in d.operators.iter().filter(|o| o.temporal == "instantaneous") {
        for a in &o.reads {
            for b in &o.writes {
                if let Some(s) = adj.get_mut(a) {
                    s.insert(b);
                }
            }
        }
    }
    let nodes: BTreeSet<String> = qnames.iter().map(|q| (*q).clone()).collect();
    let adj_owned: BTreeMap<String, BTreeSet<String>> =
        adj.iter().map(|(k, v)| ((*k).clone(), v.iter().map(|s| (*s).clone()).collect())).collect();
    let groups: Vec<BTreeSet<&String>> = d.coupled_groups.iter().map(|g| g.iter().collect()).collect();
    for scc in crate::graph::strongly_connected(&nodes, &adj_owned) {
        let refs: BTreeSet<&String> = scc.iter().collect();
        if !groups.iter().any(|g| refs.is_subset(g)) {
            let names: Vec<&String> = scc.iter().collect();
            let item = json!({"code": "INSTANTANEOUS_FEEDBACK_NOT_CLOSED",
                              "message": format!("instantaneous feedback {} lacks a coupled solve group", list_repr(&names))});
            if for_optimization { errors.push(item) } else { warnings.push(item) }
        }
    }
    let design_reachable: Map<String, Value> =
        reachable_by.iter().map(|(c, r)| ((*c).clone(), json!(r.iter().collect::<Vec<_>>()))).collect();
    json!({
        "schema": "implexity-semantic-physics-report/2",
        "provider": d.provider,
        "ok": errors.is_empty(),
        "errors": errors,
        "warnings": warnings,
        "designReachable": Value::Object(design_reachable),
        "activeResponses": d.active_responses,
        "contract": d.to_value(),
    })
}


pub fn validate_provider_semantics(
    provider: &dyn CaeProvider,
    problem: Option<&ProviderProblem>,
    for_optimization: bool,
) -> CaeResult<Option<Value>> {
    let Some(raw) = provider.semantic_physics_contract(problem) else { return Ok(None) };
    let d = SemanticPhysicsContract::from_value(&raw)?;
    Ok(Some(validate_semantic_physics(&d, for_optimization)))
}
