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


use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use implexity_core::orchestration::{PortSpec as RuntimePort, direct_port_match};
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::py_str;

use crate::contracts::{AddinContract, PortSpec, ValidationIssue};
use crate::model_errors::PhysicsResult;
use crate::registry::{PhysicsAddinRegistry, ensure_default_addins};

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveRequest {
    pub quantity: String,
    pub sense: String,
    pub target: Option<f64>,
    pub units: Option<String>,
}

impl ObjectiveRequest {
    pub fn new(quantity: impl Into<String>) -> Self {
        Self { quantity: quantity.into(), sense: "minimise".into(), target: None, units: None }
    }
}

impl From<&str> for ObjectiveRequest {
    fn from(q: &str) -> Self {
        Self::new(q)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicsPlan {
    pub status: String,
    pub selected_addins: Vec<String>,
    pub edges: Vec<(String, String, String)>,
    pub missing_quantities: Vec<String>,
    pub issues: Vec<ValidationIssue>,
    pub authoring_required: BTreeMap<String, Vec<String>>,
    pub validation_scope: String,
    pub numerical_execution_verified: bool,
}

impl PhysicsPlan {
    #[must_use]
    pub fn ready(&self) -> bool {
        self.status == "READY" && !self.issues.iter().any(|i| i.blocking)
    }
}

pub const BOUNDARY_OR_AUTHORED_QUANTITIES: [&str; 27] = [
    "ambient_temperature",
    "surface_temperature",
    "temperature",
    "pressure",
    "surface_pressure",
    "surface_normal",
    "slip_velocity",
    "gap",
    "strain",
    "strain_increment",
    "electric_potential",
    "overpotential",
    "species_concentration",
    "phase_temperature",
    "structural_velocity",
    "acoustic_pressure",
    "mesh_geometry",
    "time",
    "topology_density",
    "external_heat_flux",
    "fluid_velocity",
    "fluid_temperature",
    "heat_source",
    "displacement",
    "stress",
    "frequency",
    "time_increment",
];

fn is_boundary(q: &str) -> bool {
    BOUNDARY_OR_AUTHORED_QUANTITIES.contains(&q)
}

fn fidelity_rank(f: &str) -> u8 {
    match f {
        "intermediate" => 1,
        "high" => 2,
        _ => 0,
    }
}

fn namespace_contains(namespace: Option<&Value>, name: &str) -> bool {
    match namespace {
        Some(Value::Object(m)) => m.contains_key(name),
        Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some(name)),
        Some(Value::String(s)) => s.contains(name),
        _ => false,
    }
}

fn runtime_port(local: &PortSpec) -> Option<RuntimePort> {
    let aggregation = if local.aggregation == "stack" { "concatenate" } else { local.aggregation.as_str() };
    if !["single", "sum", "mean", "minimum", "maximum", "concatenate"].contains(&aggregation) {
        return None;
    }
    let mut port = RuntimePort::new(local.quantity.clone());
    port.unit.clone_from(&local.units);
    port.domain.clone_from(&local.support);
    port.temporal = if local.support == "history" { "history" } else { "instantaneous" }.into();
    port.aggregation = aggregation.into();
    Some(port)
}

pub struct AddinPlanner<'a> {
    pub registry: &'a PhysicsAddinRegistry,
}

impl<'a> AddinPlanner<'a> {

    pub fn new(registry: Option<&'a PhysicsAddinRegistry>) -> PhysicsResult<Self> {
        Ok(Self { registry: ensure_default_addins(registry)? })
    }


    #[allow(clippy::too_many_lines)]
    pub fn plan(
        &self,
        objectives: &[ObjectiveRequest],
        activated: &[String],
        authoring: Option<&Map<String, Value>>,
    ) -> PhysicsResult<PhysicsPlan> {
        let catalogue = self.registry.list();
        let mut selected: BTreeSet<String> = activated.iter().cloned().collect();
        let mut missing: Vec<String> = Vec::new();
        let mut issues: Vec<ValidationIssue> = Vec::new();
        let mut edges: Vec<(String, String, String)> = Vec::new();
        let mut queue: std::collections::VecDeque<String> =
            objectives.iter().map(|o| o.quantity.clone()).collect();
        let mut aliases: BTreeMap<&str, Vec<&AddinContract>> = BTreeMap::new();
        for c in &catalogue {
            for a in &c.objective_aliases {
                aliases.entry(a.as_str()).or_default().push(c);
            }
        }
        let producers = |q: &str| -> Vec<&AddinContract> {
            let mut direct: Vec<&AddinContract> = catalogue
                .iter()
                .filter(|c| c.produces.iter().any(|p| p.quantity == q || p.name == q))
                .collect();
            for x in aliases.get(q).into_iter().flatten() {
                if !direct.iter().any(|d| d.addin_id == x.addin_id) {
                    direct.push(x);
                }
            }
            direct
        };
        while let Some(q) = queue.pop_front() {
            if is_boundary(&q) {
                continue;
            }
            let mut ps = producers(&q);
            if ps.is_empty() {
                missing.push(q);
                continue;
            }
            ps.sort_by(|a, b| {
                (!selected.contains(&a.addin_id), fidelity_rank(&a.fidelity), &a.addin_id).cmp(&(
                    !selected.contains(&b.addin_id),
                    fidelity_rank(&b.fidelity),
                    &b.addin_id,
                ))
            });
            let chosen = ps[0];
            if !selected.contains(&chosen.addin_id) {
                selected.insert(chosen.addin_id.clone());
                for port in &chosen.consumes {
                    if port.required {
                        queue.push_back(port.quantity.clone());
                    }
                }
            }
        }

        let contract_of = |cid: &str| self.registry.get(cid);
        for cid in &selected {
            let c = contract_of(cid)?;
            for port in &c.consumes {
                let candidates: Vec<&AddinContract> = producers(&port.quantity)
                    .into_iter()
                    .filter(|p| selected.contains(&p.addin_id) && &p.addin_id != cid)
                    .collect();
                if candidates.is_empty() {
                    if port.required && !is_boundary(&port.quantity) {
                        missing.push(port.quantity.clone());
                    }
                    continue;
                }
                if candidates.len() > 1 && port.aggregation == "single" {
                    issues.push(ValidationIssue::new(
                        "AMBIGUOUS_SOURCE",
                        format!(
                            "{cid}.{} has multiple active sources for {} without aggregation.",
                            port.name, port.quantity
                        ),
                    ));
                }
                let targets = [runtime_port(port)];
                for src in candidates {
                    let sources: Vec<Option<RuntimePort>> = src
                        .produces
                        .iter()
                        .filter(|p| p.quantity == port.quantity)
                        .map(runtime_port)
                        .collect();
                    let compatible = targets.iter().all(|b| {
                        b.as_ref().is_some_and(|b| {
                            sources.iter().any(|a| a.as_ref().is_some_and(|a| direct_port_match(a, b, false)))
                        })
                    });
                    if compatible {
                        edges.push((src.addin_id.clone(), cid.clone(), port.quantity.clone()));
                    } else {
                        issues.push(ValidationIssue::new(
                            "INCOMPATIBLE_PORT",
                            format!(
                                "{}->{cid}:{} has incompatible units, support or temporal semantics.",
                                src.addin_id, port.quantity
                            ),
                        ));
                    }
                }
            }
        }

        let empty = Map::new();
        let auth = authoring.unwrap_or(&empty);
        let mut required: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for cid in &selected {
            let c = contract_of(cid)?;
            let ns = auth.get(cid);
            required.insert(
                cid.clone(),
                c.authoring
                    .iter()
                    .filter(|a| a.required && !namespace_contains(ns, &a.name))
                    .map(|a| a.name.clone())
                    .collect(),
            );
            let namespace = ns.cloned().unwrap_or_else(|| Value::Object(Map::new()));
            issues.extend(c.validate_authoring(Some(&namespace)));
        }

        for cid in &selected {
            let contract = contract_of(cid)?;
            let coords = &contract.design_coordinates;
            let unique: BTreeSet<&String> = coords.iter().collect();
            if coords.is_empty() || unique.len() != coords.len() || coords.iter().any(String::is_empty) {
                issues.push(ValidationIssue::new(
                    "NO_DESIGN_PATH",
                    format!("{cid} has no explicit exact named-design path."),
                ));
            }
            let condition =
                contract.metadata.get("requires_reverse_addin_when").map(py_str).unwrap_or_default();
            if let Some((key, expected)) = condition.split_once('=') {
                let actual =
                    auth.get(cid).and_then(|ns| ns.get(key)).map_or_else(|| "None".to_string(), py_str);
                if actual == expected {
                    let reverse = contract.metadata.get("reverse_quantity").map(py_str).unwrap_or_default();
                    if !reverse.is_empty() {
                        let mut found = false;
                        for other in selected.iter().filter(|o| *o != cid) {
                            if contract_of(other)?.produces.iter().any(|p| p.quantity == reverse) {
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            issues.push(ValidationIssue::new(
                                "MISSING_REVERSE_COUPLING",
                                format!(
                                    "{cid} requires a provider for {} when {condition}.",
                                    repr_str(&reverse)
                                ),
                            ));
                        }
                    }
                }
            }
        }

        let mut graph: BTreeMap<String, BTreeSet<String>> =
            selected.iter().map(|c| (c.clone(), BTreeSet::new())).collect();
        for (src, dst, _) in &edges {
            graph.entry(src.clone()).or_default().insert(dst.clone());
        }
        for comp in tarjan(&graph) {
            let self_loop = comp.len() == 1 && graph.get(&comp[0]).is_some_and(|s| s.contains(&comp[0]));
            if comp.len() > 1 || self_loop {
                let mut groups = BTreeSet::new();
                let mut strategies = BTreeSet::new();
                for x in &comp {
                    let c = contract_of(x)?;
                    groups.insert(c.coupled_group.clone());
                    strategies.insert(c.solve_strategy.clone());
                }
                if groups.contains(&None) || groups.len() != 1 || strategies.contains(&None) {
                    let mut sorted = comp.clone();
                    sorted.sort();
                    let names: Vec<String> = sorted.iter().map(|s| repr_str(s)).collect();
                    issues.push(ValidationIssue::new(
                        "UNDECLARED_COUPLED_LOOP",
                        format!(
                            "Feedback loop [{}] lacks a common coupled group and solve strategy.",
                            names.join(", ")
                        ),
                    ));
                }
            }
        }
        let missing: Vec<String> = missing.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        let blocking = !missing.is_empty() || issues.iter().any(|i| i.blocking);
        let edges: Vec<(String, String, String)> =
            edges.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        Ok(PhysicsPlan {
            status: if blocking { "BLOCKED" } else { "READY" }.into(),
            selected_addins: selected.into_iter().collect(),
            edges,
            missing_quantities: missing,
            issues,
            authoring_required: required,
            validation_scope: "static_authoring_and_semantic_ports".into(),
            numerical_execution_verified: false,
        })
    }
}

fn tarjan(graph: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    struct State<'g> {
        graph: &'g BTreeMap<String, BTreeSet<String>>,
        index: usize,
        stack: Vec<&'g str>,
        on: BTreeSet<&'g str>,
        idx: BTreeMap<&'g str, usize>,
        low: BTreeMap<&'g str, usize>,
        components: Vec<Vec<String>>,
    }
    fn visit<'g>(s: &mut State<'g>, v: &'g str) {
        s.idx.insert(v, s.index);
        s.low.insert(v, s.index);
        s.index += 1;
        s.stack.push(v);
        s.on.insert(v);
        if let Some(targets) = s.graph.get(v) {
            for w in targets {
                let w = w.as_str();
                if !s.idx.contains_key(w) {
                    visit(s, w);
                    let lw = s.low[w];
                    let lv = s.low[v];
                    s.low.insert(v, lv.min(lw));
                } else if s.on.contains(w) {
                    let iw = s.idx[w];
                    let lv = s.low[v];
                    s.low.insert(v, lv.min(iw));
                }
            }
        }
        if s.low[v] == s.idx[v] {
            let mut comp = Vec::new();
            while let Some(w) = s.stack.pop() {
                s.on.remove(w);
                comp.push(w.to_string());
                if w == v {
                    break;
                }
            }
            s.components.push(comp);
        }
    }
    let mut state = State {
        graph,
        index: 0,
        stack: Vec::new(),
        on: BTreeSet::new(),
        idx: BTreeMap::new(),
        low: BTreeMap::new(),
        components: Vec::new(),
    };
    for v in graph.keys() {
        if !state.idx.contains_key(v.as_str()) {
            visit(&mut state, v);
        }
    }
    state.components
}
