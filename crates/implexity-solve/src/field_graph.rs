// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};

use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::error::{CaeError, CaeResult};
use implexity_core::py_repr::repr_str;
use serde_json::{Map, Value, json};

use crate::convergence::repr_name_list;

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident, $py:literal, { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant),+
        }

        impl $name {
            #[must_use]
            pub fn value(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }



            pub fn parse(value: &str) -> CaeResult<Self> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    other => Err(err(format!("{} is not a valid {}", repr_str(other), $py))),
                }
            }
        }
    };
}

string_enum!(
    FieldRole, "FieldRole", {
        State => "state",
        Coefficient => "coefficient",
        Source => "source",
        Flux => "flux",
        Storage => "storage",
        Geometry => "geometry",
        Control => "control",
        Response => "response",
    }
);

string_enum!(
    AggregationLaw, "AggregationLaw", {
        Single => "single",
        Sum => "sum",
    }
);

string_enum!(
    CoupledSolvePolicy, "CoupledSolvePolicy", {
        Monolithic => "monolithic",
        ExactPartitioned => "exact_partitioned",
        Sequential => "sequential",
    }
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldPort {
    pub quantity: String,
    pub role: FieldRole,
    pub units: String,
    pub aggregation: AggregationLaw,
    pub optional: bool,
}

impl FieldPort {


    pub fn new(
        quantity: &str,
        role: FieldRole,
        units: &str,
        aggregation: AggregationLaw,
        optional: bool,
    ) -> CaeResult<Self> {
        let quantity = quantity.trim().to_string();
        if quantity.is_empty() {
            return Err(err("field quantity must be non-empty"));
        }
        Ok(Self { quantity, role, units: units.into(), aggregation, optional })
    }



    pub fn from_json(value: &Value) -> CaeResult<Self> {
        let mut row =
            value.as_object().cloned().ok_or_else(|| err("field port must be a FieldPort or mapping"))?;
        if row.contains_key("name") && !row.contains_key("quantity") {
            let name = row.shift_remove("name").unwrap_or(Value::Null);
            row.insert("quantity".into(), name);
        }
        for key in row.keys() {
            if !["quantity", "role", "units", "aggregation", "optional"].contains(&key.as_str()) {
                return Err(err(format!(
                    "FieldPort.__init__() got an unexpected keyword argument {}",
                    repr_str(key)
                )));
            }
        }
        let quantity = row
            .get("quantity")
            .map(text)
            .ok_or_else(|| err("FieldPort.__init__() missing required argument: 'quantity'"))?;
        let role = FieldRole::parse(
            &row.get("role")
                .map(text)
                .ok_or_else(|| err("FieldPort.__init__() missing required argument: 'role'"))?,
        )?;
        let aggregation =
            row.get("aggregation").map_or(Ok(AggregationLaw::Single), |v| AggregationLaw::parse(&text(v)))?;
        let optional = row.get("optional").is_some_and(truthy);
        Self::new(&quantity, role, &row.get("units").map(text).unwrap_or_default(), aggregation, optional)
    }
}

fn text(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_string)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FieldAddinContract {
    pub addin_id: String,
    pub consumes: Vec<FieldPort>,
    pub produces: Vec<FieldPort>,
    pub solved_states: Vec<String>,
    pub topology_inputs: Vec<String>,
    pub response_dependencies: Vec<(String, Vec<String>)>,
    pub coupled_solve_policy: Option<CoupledSolvePolicy>,
    pub notes: Vec<String>,
}

impl FieldAddinContract {


    pub fn from_json(value: &Value) -> CaeResult<Self> {
        let mut row =
            value.as_object().cloned().ok_or_else(|| err("field add-in contract must be a mapping"))?;
        if row.contains_key("id") && !row.contains_key("addin_id") {
            let id = row.shift_remove("id").unwrap_or(Value::Null);
            row.insert("addin_id".into(), id);
        }
        let known = [
            "addin_id",
            "consumes",
            "produces",
            "solved_states",
            "topology_inputs",
            "response_dependencies",
            "coupled_solve_policy",
            "notes",
        ];
        for key in row.keys() {
            if !known.contains(&key.as_str()) {
                return Err(err(format!(
                    "FieldAddinContract.__init__() got an unexpected keyword argument {}",
                    repr_str(key)
                )));
            }
        }
        let addin_id = row.get("addin_id").map(text).unwrap_or_default().trim().to_string();
        if addin_id.is_empty() {
            return Err(err("add-in id must be non-empty"));
        }
        let list =
            |k: &str| -> Vec<Value> { row.get(k).and_then(Value::as_array).cloned().unwrap_or_default() };
        let strings = |k: &str| -> Vec<String> { list(k).iter().map(text).collect() };
        let ports =
            |k: &str| -> CaeResult<Vec<FieldPort>> { list(k).iter().map(FieldPort::from_json).collect() };
        let response_dependencies = row
            .get("response_dependencies")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        (k.clone(), v.as_array().map(|a| a.iter().map(text).collect()).unwrap_or_default())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let coupled_solve_policy = match row.get("coupled_solve_policy") {
            None | Some(Value::Null) => None,
            Some(v) => Some(CoupledSolvePolicy::parse(&text(v))?),
        };
        Ok(Self {
            addin_id,
            consumes: ports("consumes")?,
            produces: ports("produces")?,
            solved_states: strings("solved_states"),
            topology_inputs: strings("topology_inputs"),
            response_dependencies,
            coupled_solve_policy,
            notes: strings("notes"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldDependencyEdge {
    pub source: String,
    pub target: String,
    pub quantity: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSolveGroup {
    pub members: Vec<String>,
    pub policy: CoupledSolvePolicy,
    pub cyclic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldAssemblyIssue {
    pub code: String,
    pub message: String,
    pub addins: Vec<String>,
    pub quantities: Vec<String>,
}

impl FieldAssemblyIssue {
    fn new(code: &str, message: String, addins: Vec<String>, quantities: Vec<String>) -> Self {
        Self { code: code.into(), message, addins, quantities }
    }

    fn to_json(&self) -> Value {
        json!({"code": self.code, "message": self.message, "addins": self.addins, "quantities": self.quantities})
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldAssemblyPlan {
    pub requested_responses: Vec<String>,
    pub ready: bool,
    pub errors: Vec<FieldAssemblyIssue>,
    pub warnings: Vec<FieldAssemblyIssue>,
    pub edges: Vec<FieldDependencyEdge>,
    pub solve_groups: Vec<FieldSolveGroup>,
    pub state_owners: BTreeMap<String, String>,
    pub response_owners: BTreeMap<String, String>,
    pub source_aggregation: BTreeMap<String, AggregationLaw>,
    pub response_topology_paths: Vec<(String, Vec<String>)>,
}

impl FieldAssemblyPlan {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        let mut paths = Map::new();
        for (k, v) in &self.response_topology_paths {
            paths.insert(k.clone(), json!(v));
        }
        json!({
            "schema": "implexity-field-assembly-plan/36",
            "topology_coordinate": TOPOLOGY_COORDINATE,
            "requested_responses": self.requested_responses,
            "ready": self.ready,
            "errors": self.errors.iter().map(FieldAssemblyIssue::to_json).collect::<Vec<_>>(),
            "warnings": self.warnings.iter().map(FieldAssemblyIssue::to_json).collect::<Vec<_>>(),
            "edges": self.edges.iter().map(|e| json!({"source": e.source, "target": e.target, "quantity": e.quantity})).collect::<Vec<_>>(),
            "solve_groups": self.solve_groups.iter().map(|g| json!({"members": g.members, "policy": g.policy.value(), "cyclic": g.cyclic})).collect::<Vec<_>>(),
            "state_owners": self.state_owners,
            "response_owners": self.response_owners,
            "source_aggregation": self.source_aggregation.iter().map(|(k, v)| (k.clone(), json!(v.value()))).collect::<Map<_, _>>(),
            "response_topology_paths": paths,
        })
    }
}

fn tarjan(nodes: &[String], adjacency: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    struct State<'a> {
        adjacency: &'a BTreeMap<String, BTreeSet<String>>,
        index: usize,
        indices: BTreeMap<String, usize>,
        low: BTreeMap<String, usize>,
        stack: Vec<String>,
        on_stack: BTreeSet<String>,
        components: Vec<Vec<String>>,
    }
    fn strong(s: &mut State<'_>, v: &str) {
        s.indices.insert(v.to_string(), s.index);
        s.low.insert(v.to_string(), s.index);
        s.index += 1;
        s.stack.push(v.to_string());
        s.on_stack.insert(v.to_string());
        let targets: Vec<String> =
            s.adjacency.get(v).map(|t| t.iter().cloned().collect()).unwrap_or_default();
        for w in targets {
            if !s.indices.contains_key(&w) {
                strong(s, &w);
                let lw = s.low[&w];
                let lv = s.low[v].min(lw);
                s.low.insert(v.to_string(), lv);
            } else if s.on_stack.contains(&w) {
                let lv = s.low[v].min(s.indices[&w]);
                s.low.insert(v.to_string(), lv);
            }
        }
        if s.low[v] == s.indices[v] {
            let mut comp = Vec::new();
            while let Some(w) = s.stack.pop() {
                s.on_stack.remove(&w);
                let done = w == v;
                comp.push(w);
                if done {
                    break;
                }
            }
            comp.sort();
            s.components.push(comp);
        }
    }
    let mut state = State {
        adjacency,
        index: 0,
        indices: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        components: Vec::new(),
    };
    let mut sorted = nodes.to_vec();
    sorted.sort();
    for node in &sorted {
        if !state.indices.contains_key(node) {
            strong(&mut state, node);
        }
    }
    state.components
}

fn ordered_groups(
    components: &[Vec<String>],
    adjacency: &BTreeMap<String, BTreeSet<String>>,
) -> CaeResult<Vec<Vec<String>>> {
    let owner: BTreeMap<&String, usize> =
        components.iter().enumerate().flat_map(|(i, c)| c.iter().map(move |n| (n, i))).collect();
    let mut outgoing: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); components.len()];
    let mut indegree = vec![0usize; components.len()];
    for (source, targets) in adjacency {
        let a = owner[source];
        for target in targets {
            let b = owner[target];
            if a != b && outgoing[a].insert(b) {
                indegree[b] += 1;
            }
        }
    }
    let mut ready: Vec<usize> = (0..components.len()).filter(|&i| indegree[i] == 0).collect();
    ready.sort_by(|a, b| components[*a].cmp(&components[*b]));
    let mut order = Vec::new();
    while !ready.is_empty() {
        let i = ready.remove(0);
        order.push(i);
        let mut next: Vec<usize> = outgoing[i].iter().copied().collect();
        next.sort_by(|a, b| components[*a].cmp(&components[*b]));
        for j in next {
            indegree[j] -= 1;
            if indegree[j] == 0 {
                ready.push(j);
                ready.sort_by(|a, b| components[*a].cmp(&components[*b]));
            }
        }
    }
    if order.len() != components.len() {
        return Err(err("internal error while ordering coupled solve groups"));
    }
    Ok(order.into_iter().map(|i| components[i].clone()).collect())
}

fn sorted_pair(a: &str, b: &str) -> Vec<String> {
    let mut v = vec![a.to_string(), b.to_string()];
    v.sort();
    v
}



#[allow(clippy::too_many_lines)]
pub fn build_field_assembly_plan(
    rows: &[FieldAddinContract],
    requested_responses: &[String],
) -> CaeResult<FieldAssemblyPlan> {
    let mut requested: Vec<String> = Vec::new();
    for r in requested_responses {
        if !requested.contains(r) {
            requested.push(r.clone());
        }
    }
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    let mut by_id: BTreeMap<String, &FieldAddinContract> = BTreeMap::new();
    let mut id_order: Vec<String> = Vec::new();
    for row in rows {
        if by_id.contains_key(&row.addin_id) {
            errors.push(FieldAssemblyIssue::new(
                "DUPLICATE_ADDIN_ID",
                format!("duplicate add-in id {}", repr_str(&row.addin_id)),
                vec![row.addin_id.clone()],
                vec![],
            ));
        } else {
            by_id.insert(row.addin_id.clone(), row);
            id_order.push(row.addin_id.clone());
        }
    }
    let mut produced: BTreeMap<String, Vec<(String, &FieldPort)>> = BTreeMap::new();
    for row in rows {
        let mut seen: BTreeSet<(String, FieldRole)> = BTreeSet::new();
        for port in &row.produces {
            if !seen.insert((port.quantity.clone(), port.role)) {
                errors.push(FieldAssemblyIssue::new(
                    "DUPLICATE_LOCAL_PRODUCTION",
                    format!(
                        "add-in {} declares {} more than once",
                        repr_str(&row.addin_id),
                        repr_str(&port.quantity)
                    ),
                    vec![row.addin_id.clone()],
                    vec![port.quantity.clone()],
                ));
            }
            produced.entry(port.quantity.clone()).or_default().push((row.addin_id.clone(), port));
        }
    }
    let mut state_owners: BTreeMap<String, String> = BTreeMap::new();
    for row in rows {
        let states: BTreeSet<&String> =
            row.produces.iter().filter(|p| p.role == FieldRole::State).map(|p| &p.quantity).collect();
        for state in &row.solved_states {
            if !states.contains(state) {
                errors.push(FieldAssemblyIssue::new(
                    "SOLVED_STATE_NOT_PRODUCED",
                    format!(
                        "add-in {} owns residual state {} but does not produce it as a state",
                        repr_str(&row.addin_id),
                        repr_str(state)
                    ),
                    vec![row.addin_id.clone()],
                    vec![state.clone()],
                ));
            }
            if let Some(previous) = state_owners.get(state)
                && *previous != row.addin_id
            {
                errors.push(FieldAssemblyIssue::new(
                    "COMPETING_STATE_OWNERS",
                    format!(
                        "state {} has competing residual owners {} and {}",
                        repr_str(state),
                        repr_str(previous),
                        repr_str(&row.addin_id)
                    ),
                    sorted_pair(previous, &row.addin_id),
                    vec![state.clone()],
                ));
            }
            state_owners.insert(state.clone(), row.addin_id.clone());
        }
        let solved: BTreeSet<&String> = row.solved_states.iter().collect();
        for state in states.iter().filter(|s| !solved.contains(*s)) {
            errors.push(FieldAssemblyIssue::new(
                "STATE_WITHOUT_RESIDUAL_OWNER",
                format!(
                    "state {} is produced by {} without residual ownership",
                    repr_str(state),
                    repr_str(&row.addin_id)
                ),
                vec![row.addin_id.clone()],
                vec![(*state).clone()],
            ));
        }
    }
    let mut aggregation = BTreeMap::new();
    for (quantity, owners) in &produced {
        if owners.len() == 1 {
            aggregation.insert(quantity.clone(), owners[0].1.aggregation);
            continue;
        }
        let additive = owners.iter().all(|(_, p)| {
            matches!(p.role, FieldRole::Source | FieldRole::Flux) && p.aggregation == AggregationLaw::Sum
        });
        if additive {
            aggregation.insert(quantity.clone(), AggregationLaw::Sum);
        } else {
            let mut ids: Vec<String> = owners.iter().map(|(a, _)| a.clone()).collect();
            ids.sort();
            errors.push(FieldAssemblyIssue::new(
                "AMBIGUOUS_FIELD_PRODUCERS",
                format!(
                    "field {} has multiple producers without one compatible additive source/flux law",
                    repr_str(quantity)
                ),
                ids,
                vec![quantity.clone()],
            ));
        }
    }
    let mut response_owners: BTreeMap<String, String> = BTreeMap::new();
    let mut response_quantities: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        for (response, quantities) in &row.response_dependencies {
            if let Some(previous) = response_owners.get(response)
                && *previous != row.addin_id
            {
                errors.push(FieldAssemblyIssue::new(
                    "COMPETING_RESPONSE_OWNERS",
                    format!(
                        "response {} is declared by both {} and {}",
                        repr_str(response),
                        repr_str(previous),
                        repr_str(&row.addin_id)
                    ),
                    sorted_pair(previous, &row.addin_id),
                    quantities.clone(),
                ));
            }
            response_owners.insert(response.clone(), row.addin_id.clone());
            response_quantities.insert(response.clone(), quantities.clone());
        }
    }
    let mut edges: BTreeSet<FieldDependencyEdge> = BTreeSet::new();
    let mut adjacency: BTreeMap<String, BTreeSet<String>> =
        by_id.keys().map(|k| (k.clone(), BTreeSet::new())).collect();
    let mut reverse = adjacency.clone();
    for row in rows {
        for port in &row.consumes {
            let Some(owners) = produced.get(&port.quantity) else {
                if !port.optional {
                    errors.push(FieldAssemblyIssue::new(
                        "REQUIRED_FIELD_UNPRODUCED",
                        format!(
                            "add-in {} consumes unproduced field {}",
                            repr_str(&row.addin_id),
                            repr_str(&port.quantity)
                        ),
                        vec![row.addin_id.clone()],
                        vec![port.quantity.clone()],
                    ));
                }
                continue;
            };
            for (owner, _) in owners {
                edges.insert(FieldDependencyEdge {
                    source: owner.clone(),
                    target: row.addin_id.clone(),
                    quantity: port.quantity.clone(),
                });
                adjacency.entry(owner.clone()).or_default().insert(row.addin_id.clone());
                reverse.entry(row.addin_id.clone()).or_default().insert(owner.clone());
            }
        }
    }
    let seeds: BTreeSet<String> = rows
        .iter()
        .filter(|r| r.topology_inputs.iter().any(|t| t == TOPOLOGY_COORDINATE))
        .map(|r| r.addin_id.clone())
        .collect();
    for row in rows {
        let foreign: Vec<String> = row
            .topology_inputs
            .iter()
            .filter(|t| *t != TOPOLOGY_COORDINATE)
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !foreign.is_empty() && !row.topology_inputs.iter().any(|t| t == TOPOLOGY_COORDINATE) {
            warnings.push(FieldAssemblyIssue::new(
                "NON_AUTHORITATIVE_DESIGN_INPUT_ONLY",
                format!(
                    "add-in {} declares design inputs {} but no {}",
                    repr_str(&row.addin_id),
                    repr_name_list(&foreign),
                    repr_str(TOPOLOGY_COORDINATE)
                ),
                vec![row.addin_id.clone()],
                foreign,
            ));
        }
    }
    let mut forward = seeds.clone();
    let mut frontier: Vec<String> = seeds.iter().cloned().collect();
    while let Some(node) = frontier.pop() {
        for next in adjacency.get(&node).into_iter().flatten() {
            if forward.insert(next.clone()) {
                frontier.push(next.clone());
            }
        }
    }
    let mut response_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for response in &requested {
        let Some(owner) = response_owners.get(response) else {
            errors.push(FieldAssemblyIssue::new(
                "REQUESTED_RESPONSE_UNOWNED",
                format!("requested response {} has no declaring add-in", repr_str(response)),
                vec![],
                vec![response.clone()],
            ));
            response_paths.insert(response.clone(), vec![]);
            continue;
        };
        let quantities = response_quantities.get(response).cloned().unwrap_or_default();
        for quantity in &quantities {
            if !produced.contains_key(quantity) {
                errors.push(FieldAssemblyIssue::new(
                    "RESPONSE_DEPENDENCY_UNPRODUCED",
                    format!(
                        "response {} depends on unproduced field {}",
                        repr_str(response),
                        repr_str(quantity)
                    ),
                    vec![owner.clone()],
                    vec![quantity.clone()],
                ));
            }
        }
        let mut ancestors = BTreeSet::from([owner.clone()]);
        let mut stack = vec![owner.clone()];
        while let Some(node) = stack.pop() {
            for prev in reverse.get(&node).into_iter().flatten() {
                if ancestors.insert(prev.clone()) {
                    stack.push(prev.clone());
                }
            }
        }
        let path: Vec<String> = ancestors.intersection(&forward).cloned().collect();
        if path.is_empty() || !path.contains(owner) {
            errors.push(FieldAssemblyIssue::new(
                "RESPONSE_NOT_TOPOLOGY_REACHABLE",
                format!(
                    "requested response {} has no field path from {TOPOLOGY_COORDINATE}",
                    repr_str(response)
                ),
                vec![owner.clone()],
                quantities,
            ));
        }
        response_paths.insert(response.clone(), path);
    }
    let components = ordered_groups(&tarjan(&id_order, &adjacency), &adjacency)?;
    let mut solve_groups = Vec::new();
    for comp in components {
        let cyclic = comp.len() > 1 || comp.iter().any(|n| adjacency.get(n).is_some_and(|t| t.contains(n)));
        if !cyclic {
            solve_groups.push(FieldSolveGroup {
                members: comp,
                policy: CoupledSolvePolicy::Sequential,
                cyclic: false,
            });
            continue;
        }
        let all: BTreeSet<Option<CoupledSolvePolicy>> =
            comp.iter().map(|n| by_id[n].coupled_solve_policy).collect();
        let declared: BTreeSet<CoupledSolvePolicy> = all.iter().flatten().copied().collect();
        let listed = format!("[{}]", comp.iter().map(|m| repr_str(m)).collect::<Vec<_>>().join(", "));
        let policy = if declared.len() != 1 || declared.len() != all.len() {
            errors.push(FieldAssemblyIssue::new(
                "COUPLED_SOLVE_POLICY_INCOMPATIBLE",
                format!("cyclic field group {listed} does not declare one compatible solve policy"),
                comp.clone(),
                vec![],
            ));
            CoupledSolvePolicy::Sequential
        } else {
            let policy = declared.into_iter().next().unwrap_or(CoupledSolvePolicy::Sequential);
            if policy == CoupledSolvePolicy::Sequential {
                errors.push(FieldAssemblyIssue::new(
                    "SEQUENTIAL_CYCLIC_SOLVE_FORBIDDEN",
                    format!("cyclic field group {listed} cannot use sequential evaluation"),
                    comp.clone(),
                    vec![],
                ));
            }
            policy
        };
        solve_groups.push(FieldSolveGroup { members: comp, policy, cyclic: true });
    }
    Ok(FieldAssemblyPlan {
        ready: errors.is_empty(),
        response_topology_paths: requested
            .iter()
            .map(|k| (k.clone(), response_paths.get(k).cloned().unwrap_or_default()))
            .collect(),
        requested_responses: requested,
        errors,
        warnings,
        edges: edges.into_iter().collect(),
        solve_groups,
        state_owners,
        response_owners,
        source_aggregation: aggregation,
    })
}

