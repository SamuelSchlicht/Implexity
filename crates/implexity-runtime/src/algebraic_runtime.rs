// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use implexity_ad::tape::{Tape, Var};
use implexity_core::contracts::{
    CaeProvider, Evaluation, LegacySingleArrayProviderCapabilities, ProviderCapabilities, ProviderDescriptor,
    ProviderProblem, Sensitivity, TOPOLOGY_COORDINATE,
};
use implexity_core::orchestration::{DesignCoordinateRef, OrchestrationPlan, PortSpec, RegisteredAddIn};
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::{DesignLayout, NamedArrays};
use implexity_optim::numeric::str_list_repr;
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::LifecycleDeclaration;
use implexity_optim::provider_ops::{
    DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, design_interface,
};
use ndarray::ArrayD;
use serde_json::{Map, Value};

use crate::addin::{AlgebraicAddIn, AlgebraicValue, JsonMap, addin_operations};
use crate::orchestration_runtime::{DesignPortBindings, ExecutionContext, route_key_repr};
use crate::results::json_problem;

pub const ALGEBRAIC_NAME: &str = "orchestrated_algebraic";

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message.into())
}

fn ad(e: &implexity_ad::error::AdError) -> CaeError {
    contract(e.to_string())
}

fn port_token(port: &PortSpec, compatibility: bool) -> String {
    if compatibility || port.port_id.is_empty() { port.quantity.clone() } else { port.port_id.clone() }
}

fn json_numbers(value: &Value, out: &mut Vec<f64>, shape: &mut Vec<usize>, depth: usize) -> CaeResult<()> {
    match value {
        Value::Number(n) => {
            out.push(n.as_f64().ok_or_else(|| contract("authored algebraic value is not a real number"))?);
            Ok(())
        }
        Value::Bool(b) => {
            out.push(if *b { 1.0 } else { 0.0 });
            Ok(())
        }
        Value::Array(items) => {
            if shape.len() == depth {
                shape.push(items.len());
            } else if shape[depth] != items.len() {
                return Err(contract("authored algebraic array is ragged"));
            }
            for item in items {
                json_numbers(item, out, shape, depth + 1)?;
            }
            Ok(())
        }
        _ => Err(contract("authored algebraic value is not numeric")),
    }
}

impl AlgebraicValue {

    pub fn tensor(&self, tape: &mut Tape) -> CaeResult<(Var, Vec<usize>)> {
        match self {
            Self::Tensor { var, shape } => Ok((*var, shape.clone())),
            Self::Json(v) => {
                let mut data = Vec::new();
                let mut shape = Vec::new();
                json_numbers(v, &mut data, &mut shape, 0)?;
                Ok((tape.constant(data), shape))
            }
        }
    }
}

fn aggregate(
    tape: &mut Tape,
    values: &[AlgebraicValue],
    semantics: &str,
    label: &str,
) -> CaeResult<AlgebraicValue> {
    let Some(first) = values.first() else {
        return Err(contract(format!("{label}: aggregation has no inputs")));
    };
    match semantics {
        "single" => {
            if values.len() != 1 {
                return Err(contract(format!("{label}: multiple producers require an explicit aggregator")));
            }
            Ok(first.clone())
        }
        "sum" | "mean" | "minimum" | "maximum" => {
            let (mut acc, mut shape) = first.tensor(tape)?;
            for other in &values[1..] {
                let (v, s) = other.tensor(tape)?;
                if s.iter().product::<usize>() > shape.iter().product::<usize>() {
                    shape = s;
                }
                acc = match semantics {
                    "minimum" => tape.minimum(acc, v),
                    "maximum" => tape.maximum(acc, v),
                    _ => tape.add(acc, v),
                }
                .map_err(|e| ad(&e))?;
            }
            if semantics == "mean" {
                #[allow(clippy::cast_precision_loss)]
                let n = values.len() as f64;
                acc = tape.scale(acc, 1.0 / n).map_err(|e| ad(&e))?;
            }
            Ok(AlgebraicValue::Tensor { var: acc, shape })
        }
        "concatenate" => {
            let mut parts = Vec::new();
            let mut rows = 0usize;
            let mut tail: Option<Vec<usize>> = None;
            for v in values {
                let (var, shape) = v.tensor(tape)?;
                if shape.is_empty() {
                    return Err(contract(format!("{label}: zero-dimensional arrays cannot be concatenated")));
                }
                if tail.as_ref().is_some_and(|t| t.as_slice() != &shape[1..]) {
                    return Err(contract(format!(
                        "{label}: all the input array dimensions except for the concatenation axis must match exactly"
                    )));
                }
                tail = Some(shape[1..].to_vec());
                rows += shape[0];
                parts.push(var);
            }
            let var = tape.concat(&parts).map_err(|e| ad(&e))?;
            let mut shape = vec![rows];
            shape.extend(tail.unwrap_or_default());
            Ok(AlgebraicValue::Tensor { var, shape })
        }
        other => Err(contract(format!("{label}: unsupported aggregation semantics {}", repr_str(other)))),
    }
}

type Key = (String, String, String, String, Option<String>, String, bool, String, String);
type Outputs = BTreeMap<String, BTreeMap<String, AlgebraicValue>>;
type Validity = Vec<(String, Vec<(String, Vec<f64>)>)>;

pub struct AlgebraicOrchestratedProvider {
    plan: Arc<OrchestrationPlan>,
    entries: BTreeMap<String, Arc<RegisteredAddIn>>,
    context: ExecutionContext,
    compatibility: BTreeMap<String, bool>,
    design_coordinates: Vec<String>,
    order: Vec<String>,
}

impl std::fmt::Debug for AlgebraicOrchestratedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlgebraicOrchestratedProvider")
            .field("design_coordinates", &self.design_coordinates)
            .field("order", &self.order)
            .finish_non_exhaustive()
    }
}

impl AlgebraicOrchestratedProvider {

    #[allow(clippy::too_many_lines)]
    pub fn new(
        plan: Arc<OrchestrationPlan>,
        entries: BTreeMap<String, Arc<RegisteredAddIn>>,
        context: ExecutionContext,
    ) -> CaeResult<Self> {
        let mut compatibility = BTreeMap::new();
        let mut union: BTreeSet<String> = BTreeSet::new();
        for aid in &plan.selected_addins {
            let entry = &entries[aid];
            let compat = entry.compatibility_mode || entry.contract.compatibility_mode;
            compatibility.insert(aid.clone(), compat);
            let alg = entry.adapter.as_deref().and_then(addin_operations).and_then(|o| o.algebraic());
            let Some(alg) = alg else {
                return Err(contract(format!("{aid}: no algebraic evaluation contract")));
            };
            let names: Vec<String> = if compat {
                if alg.named() {
                    alg.design_coordinates().unwrap_or_else(|| vec![TOPOLOGY_COORDINATE.to_string()])
                } else {
                    vec![TOPOLOGY_COORDINATE.to_string()]
                }
            } else {
                if !alg.named() {
                    return Err(contract(format!(
                        "{aid}: canonical algebraic execution requires algebraic_evaluate_design"
                    )));
                }
                let mut names: Vec<String> = Vec::new();
                for r in &entry.contract.design_inputs {
                    if !names.contains(&r.coordinate) {
                        names.push(r.coordinate.clone());
                    }
                }
                names
            };
            let unique: BTreeSet<&String> = names.iter().collect();
            if unique.len() != names.len() {
                return Err(contract(format!("{aid}: invalid algebraic coordinate declaration")));
            }
            union.extend(names);
        }
        let strict = !compatibility.values().any(|c| *c);
        let planned = plan.active_design_coordinates.clone();
        let design_coordinates = if strict {
            if planned.iter().cloned().collect::<BTreeSet<_>>() != union || planned.len() != union.len() {
                return Err(contract(format!(
                    "algebraic graph design-coordinate mismatch; plan={}, bound={}",
                    str_list_repr(&planned),
                    str_list_repr(&union.iter().cloned().collect::<Vec<_>>())
                )));
            }
            planned
        } else {
            union.into_iter().collect()
        };
        if design_coordinates.is_empty() {
            return Err(contract("algebraic graph has no explicitly declared design coordinate"));
        }
        let mut incoming: BTreeMap<String, BTreeSet<String>> =
            plan.selected_addins.iter().map(|a| (a.clone(), BTreeSet::new())).collect();
        for edge in &plan.coupling_edges {
            if incoming.contains_key(&edge.source) && incoming.contains_key(&edge.target) {
                incoming.get_mut(&edge.target).map(|s| s.insert(edge.source.clone()));
            }
        }
        let mut order = Vec::new();
        while !incoming.is_empty() {
            let ready: Vec<String> =
                incoming.iter().filter(|(_, v)| v.is_empty()).map(|(k, _)| k.clone()).collect();
            if ready.is_empty() {
                return Err(contract("cyclic algebraic graph requires a residual/adjoint implementation"));
            }
            for aid in ready {
                incoming.remove(&aid);
                for v in incoming.values_mut() {
                    v.remove(&aid);
                }
                order.push(aid);
            }
        }
        let key = |port: &PortSpec, compat: bool| -> Key {
            (
                if compat { String::new() } else { port.port_id.clone() },
                port.quantity.clone(),
                port.unit.clone(),
                port.domain.clone(),
                port.interface.clone(),
                port.temporal.clone(),
                port.conserved,
                port.cardinality.clone(),
                port.aggregation.clone(),
            )
        };
        let mut ownership: Vec<(Key, Vec<String>)> = Vec::new();
        for aid in &plan.selected_addins {
            for port in &entries[aid].contract.provides {
                let k = key(port, compatibility[aid]);
                match ownership.iter_mut().find(|(x, _)| *x == k) {
                    Some(row) => row.1.push(aid.clone()),
                    None => ownership.push((k, vec![aid.clone()])),
                }
            }
        }
        for (k, owners) in &ownership {
            if owners.len() < 2 {
                continue;
            }
            let owner_set: BTreeSet<&String> = owners.iter().collect();
            let mut admitted = false;
            for target in &plan.selected_addins {
                for port in &entries[target].contract.consumes {
                    let target_key = key(port, compatibility[target]);
                    let incoming: BTreeSet<&String> = plan
                        .coupling_edges
                        .iter()
                        .filter(|e| {
                            &e.target == target
                                && e.quantity == port.quantity
                                && owner_set.contains(&e.source)
                        })
                        .map(|e| &e.source)
                        .collect();
                    if &target_key == k && incoming == owner_set && port.aggregation != "single" {
                        admitted = true;
                    }
                }
            }
            if !admitted {
                let mut sorted = owners.clone();
                sorted.sort();
                return Err(contract(format!(
                    "algebraic provided-port ownership collision for {}: {}; an explicit consumer aggregator is required",
                    route_key_repr(k),
                    str_list_repr(&sorted)
                )));
            }
        }
        Ok(Self { plan, entries, context, compatibility, design_coordinates, order })
    }

    fn strict(&self) -> bool {
        !self.compatibility.values().any(|c| *c)
    }

    fn adapter(&self, aid: &str) -> CaeResult<&dyn AlgebraicAddIn> {
        self.entries
            .get(aid)
            .and_then(|e| e.adapter.as_deref())
            .and_then(addin_operations)
            .and_then(|o| o.algebraic())
            .ok_or_else(|| contract(format!("{aid}: no algebraic evaluation contract")))
    }

    fn layout(&self, design: &NamedArrays) -> CaeResult<DesignLayout> {
        let layout = DesignLayout::from_values(design)?;
        let names = design.names();
        let missing: Vec<String> =
            self.design_coordinates.iter().filter(|n| !names.contains(n)).cloned().collect();
        let extra: Vec<String> =
            names.iter().filter(|n| !self.design_coordinates.contains(n)).cloned().collect();
        if !missing.is_empty() || !extra.is_empty() {
            return Err(contract(format!(
                "algebraic graph requires exact named design keys; missing={}, extra={}",
                str_list_repr(&missing),
                str_list_repr(&extra)
            )));
        }
        Ok(layout)
    }

    fn authored(&self, port: &PortSpec, aid: &str) -> CaeResult<AlgebraicValue> {
        let keys: Vec<String> = if port.domain.is_empty() || port.domain == "*" {
            vec![port.quantity.clone()]
        } else {
            vec![format!("{}@{}", port.quantity, port.domain), port.quantity.clone()]
        };
        if !self.compatibility[aid] {
            let candidates: Vec<&Value> = self
                .context
                .external_ports
                .iter()
                .flatten()
                .filter(|item| item.port.key() == port.key())
                .map(|item| &item.value)
                .collect();
            if candidates.len() != 1 {
                return Err(contract(format!(
                    "{aid}: strict consumed port {} requires exactly one typed ExternalPortValue",
                    repr_str(&port_token(port, false))
                )));
            }
            return Ok(AlgebraicValue::Json(candidates[0].clone()));
        }
        let ctx = &self.context.values;
        let authoring = ctx.get("authoring").and_then(Value::as_object);
        let empty = Map::new();
        for root in [ctx, authoring.unwrap_or(&empty)] {
            for pool_key in ["quantities", "boundary_conditions", "states", "external_quantities"] {
                if let Some(Value::Object(pool)) = root.get(pool_key) {
                    for token in &keys {
                        if let Some(v) = pool.get(token) {
                            return Ok(AlgebraicValue::Json(v.clone()));
                        }
                    }
                }
            }
        }
        if let Some(authored) = authoring {
            for token in &keys {
                if let Some(v) = authored.get(token) {
                    return Ok(AlgebraicValue::Json(v.clone()));
                }
            }
        }
        Err(contract(format!(
            "no authored value or add-in source for physical quantity {} on domain {}",
            repr_str(&port.quantity),
            repr_str(&port.domain)
        )))
    }

    #[allow(clippy::too_many_lines)]
    fn design_binding(
        &self,
        aid: &str,
        port: &PortSpec,
        design: &BTreeMap<String, AlgebraicValue>,
    ) -> CaeResult<Option<AlgebraicValue>> {
        let compat = self.compatibility[aid];
        let token = port_token(port, compat);
        let lookup = |coordinate: &str| -> CaeResult<AlgebraicValue> {
            design.get(coordinate).cloned().ok_or_else(|| {
                contract(format!(
                    "{aid}: design-port binding names absent coordinate {}",
                    repr_str(coordinate)
                ))
            })
        };
        let declared: Vec<&DesignCoordinateRef> =
            self.entries[aid].contract.design_inputs.iter().filter(|r| r.port_id == port.port_id).collect();
        if declared.len() > 1 {
            return Err(contract(format!(
                "{aid}: ambiguous declared design binding for {}",
                repr_str(&token)
            )));
        }
        if let Some(r) = declared.first() {
            if !r.addin_id.is_empty() && r.addin_id != aid {
                return Err(contract(format!("{aid}: declared design binding has the wrong owner")));
            }
            return lookup(&r.coordinate).map(Some);
        }
        let check_ref = |owner: &str, port_id: &str| -> CaeResult<()> {
            if !(owner.is_empty() || owner == aid)
                || !(port_id.is_empty() || port_id == token || port_id == port.port_id)
            {
                return Err(contract(format!(
                    "{aid}: design-port binding identity disagrees with consumed port"
                )));
            }
            Ok(())
        };
        match &self.context.design_port_bindings {
            Some(DesignPortBindings::Keyed(rows)) => {
                let find = |k: &str| rows.iter().find(|(key, _)| key == k).map(|(_, r)| r);
                if let Some(r) = find(&format!("{aid}::{token}")).or_else(|| find(&token)) {
                    check_ref(&r.addin_id, &r.port_id)?;
                    return lookup(&r.coordinate).map(Some);
                }
            }
            Some(DesignPortBindings::Listed(rows)) => {
                let matches: Vec<&DesignCoordinateRef> = rows
                    .iter()
                    .filter(|r| r.addin_id.is_empty() || r.addin_id == aid)
                    .filter(|r| r.port_id.is_empty() || r.port_id == token || r.port_id == port.port_id)
                    .collect();
                if matches.len() > 1 {
                    return Err(contract(format!(
                        "{aid}: ambiguous design-port binding for {}",
                        repr_str(&token)
                    )));
                }
                if let Some(r) = matches.first() {
                    return lookup(&r.coordinate).map(Some);
                }
            }
            None => match self.context.get("design_port_bindings") {
                Some(Value::Object(bindings)) => {
                    let value = bindings.get(&format!("{aid}::{token}")).or_else(|| bindings.get(&token));
                    if let Some(value) = value.filter(|v| !v.is_null()) {
                        if !compat {
                            return Err(contract(format!(
                                "{aid}: strict design-port binding must be a DesignCoordinateRef"
                            )));
                        }
                        let coordinate = crate::pyval::py_str(value);
                        return lookup(&coordinate).map(Some);
                    }
                }
                Some(Value::Array(items)) => {
                    let mut matches = 0usize;
                    for _ in items {
                        if !compat {
                            return Err(contract(format!(
                                "{aid}: strict design-port bindings must be DesignCoordinateRef values"
                            )));
                        }
                        matches += 1;
                    }
                    if matches > 1 {
                        return Err(contract(format!(
                            "{aid}: ambiguous design-port binding for {}",
                            repr_str(&token)
                        )));
                    }
                    if matches == 1 {
                        return lookup("").map(Some);
                    }
                }
                Some(other) if crate::pyval::truthy(Some(other)) => {
                    return Err(contract(format!(
                        "{aid}: design-port bindings must be a mapping or sequence"
                    )));
                }
                _ => {}
            },
        }
        if compat && port.quantity == "topology_density" {
            return lookup(TOPOLOGY_COORDINATE).map(Some);
        }
        Ok(None)
    }

    #[allow(clippy::too_many_lines)]
    fn forward(
        &self,
        tape: &mut Tape,
        design: &BTreeMap<String, AlgebraicValue>,
        validity: &mut Validity,
    ) -> CaeResult<Outputs> {
        let mut outputs: Outputs = BTreeMap::new();
        let ctx = &self.context.values;
        for aid in &self.order {
            let contract_ = &self.entries[aid].contract;
            let adapter = self.adapter(aid)?;
            let compat = self.compatibility[aid];
            let mut inputs: BTreeMap<String, AlgebraicValue> = BTreeMap::new();
            for need in &contract_.consumes {
                let token = port_token(need, compat);
                if inputs.contains_key(&token) {
                    return Err(contract(format!(
                        "{aid}: ambiguous consumed port token {}",
                        repr_str(&token)
                    )));
                }
                let mut rows = Vec::new();
                for edge in self.plan.coupling_edges.iter().filter(|e| {
                    &e.target == aid
                        && e.quantity == need.quantity
                        && e.unit == need.unit
                        && e.target_domain == need.domain
                        && e.interface == need.interface
                        && e.temporal == need.temporal
                        && e.conserved == need.conserved
                        && e.cardinality == need.cardinality
                        && e.aggregation == need.aggregation
                        && (e.target_port_id.is_empty() || e.target_port_id == need.port_id)
                }) {
                    let source_contract = &self.entries[&edge.source].contract;
                    let ports: Vec<&PortSpec> = source_contract
                        .provides
                        .iter()
                        .filter(|p| {
                            p.quantity == edge.quantity
                                && p.unit == edge.unit
                                && p.domain == edge.source_domain
                                && p.interface == edge.interface
                                && p.temporal == edge.temporal
                                && p.conserved == need.conserved
                                && p.cardinality == need.cardinality
                                && p.aggregation == edge.aggregation
                                && (edge.source_port_id.is_empty() || p.port_id == edge.source_port_id)
                        })
                        .collect();
                    if ports.len() != 1 {
                        return Err(contract(format!(
                            "{}->{aid}:{}: edge does not resolve one typed source port",
                            edge.source, need.quantity
                        )));
                    }
                    let source_token = port_token(ports[0], self.compatibility[&edge.source]);
                    let Some(v) = outputs.get(&edge.source).and_then(|m| m.get(&source_token)) else {
                        return Err(contract(format!(
                            "{} did not produce declared port {}",
                            edge.source,
                            repr_str(&source_token)
                        )));
                    };
                    rows.push(v.clone());
                }
                let value = if rows.is_empty() {
                    match self.design_binding(aid, need, design)? {
                        Some(v) => v,
                        None => self.authored(need, aid)?,
                    }
                } else {
                    aggregate(tape, &rows, &need.aggregation, &format!("{aid}:{token}"))?
                };
                inputs.insert(token, value);
            }
            let raw = if adapter.named() {
                let result = adapter.evaluate(tape, &inputs, design, ctx)?;
                if adapter.reports_validity() {
                    let Some(margins) = result.validity.clone() else {
                        return Err(contract(format!("{aid}: expected (outputs, validity margins)")));
                    };
                    validity.push((aid.clone(), margins));
                }
                result.outputs
            } else {
                if !compat {
                    return Err(contract(format!(
                        "{aid}: strict algebraic execution requires algebraic_evaluate_design"
                    )));
                }
                let topology: BTreeMap<String, AlgebraicValue> = design
                    .get(TOPOLOGY_COORDINATE)
                    .map(|v| (TOPOLOGY_COORDINATE.to_string(), v.clone()))
                    .into_iter()
                    .collect();
                adapter.evaluate(tape, &inputs, &topology, ctx)?.outputs
            };
            let mut out: BTreeMap<String, AlgebraicValue> = BTreeMap::new();
            for (k, v) in raw {
                out.insert(k, v);
            }
            if !compat {
                let expected: BTreeSet<String> =
                    contract_.provides.iter().map(|p| port_token(p, false)).collect();
                if expected.len() != contract_.provides.len() {
                    return Err(contract(format!("{aid}: ambiguous provided port tokens")));
                }
                let actual: BTreeSet<String> = out.keys().cloned().collect();
                if actual != expected {
                    return Err(contract(format!(
                        "{aid}: algebraic outputs do not exactly match declared provides; missing={}, extra={}",
                        str_list_repr(&expected.difference(&actual).cloned().collect::<Vec<_>>()),
                        str_list_repr(&actual.difference(&expected).cloned().collect::<Vec<_>>())
                    )));
                }
            }
            outputs.insert(aid.clone(), out);
        }
        Ok(outputs)
    }

    fn reduce(
        &self,
        tape: &mut Tape,
        design: &BTreeMap<String, AlgebraicValue>,
        outputs: &Outputs,
        response: &str,
    ) -> CaeResult<Var> {
        let aid = crate::pyval::py_str(self.plan.response_providers.get(response).unwrap_or(&Value::Null));
        let adapter = self.adapter(&aid)?;
        let own = outputs.get(&aid).cloned().unwrap_or_default();
        let value = adapter.response_value(
            tape,
            response,
            &own,
            if adapter.response_uses_design() { Some(design) } else { None },
            &self.context.values,
        )?;
        let (var, _) = value.tensor(tape)?;
        if var.len() != 1 {
            return Err(contract("algebraic responses must be finite scalar reductions"));
        }
        Ok(var)
    }

    fn admit_validity(&self, validity: &Validity) -> CaeResult<()> {
        for (aid, margins) in validity {
            let nonnegative = self.adapter(aid)?.nonnegative_validity_margins();
            for (name, arr) in margins {
                let zero_ok = nonnegative.contains(name);
                let outside = arr.iter().any(|v| if zero_ok { *v < 0.0 } else { *v <= 0.0 });
                if arr.is_empty() || arr.iter().any(|v| !v.is_finite()) || outside {
                    return Err(contract(format!(
                        "{aid}: validity boundary crossed or invalid margin {name}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn inputs(
        tape: &mut Tape,
        design: &NamedArrays,
    ) -> (BTreeMap<String, AlgebraicValue>, Vec<(String, Var)>) {
        let mut values = BTreeMap::new();
        let mut vars = Vec::new();
        for (name, array) in design.iter() {
            let var = tape.input(array.iter().copied().collect());
            vars.push((name.to_string(), var));
            values.insert(name.to_string(), AlgebraicValue::Tensor { var, shape: array.shape().to_vec() });
        }
        (values, vars)
    }


    pub fn evaluate_named(&self, design: &NamedArrays) -> CaeResult<Evaluation> {
        self.layout(design)?;
        let mut tape = Tape::new();
        let (values, _) = Self::inputs(&mut tape, design);
        let mut validity = Vec::new();
        let outputs = self.forward(&mut tape, &values, &mut validity)?;
        self.admit_validity(&validity)?;
        let mut responses = BTreeMap::new();
        let names: Vec<String> = self.plan.response_providers.keys().cloned().collect();
        for r in &names {
            let var = self.reduce(&mut tape, &values, &outputs, r)?;
            responses.insert(r.clone(), tape.value(var).map_err(|e| ad(&e))?[0]);
        }
        if responses.values().any(|v| !v.is_finite()) {
            return Err(contract("algebraic response returned nonfinite values"));
        }
        let mut diagnostics = Map::new();
        diagnostics.insert("orchestrationPlan".into(), self.plan.as_dict());
        diagnostics.insert(
            "algebraic_outputs".into(),
            Value::Object(
                outputs
                    .iter()
                    .map(|(k, v)| (k.clone(), Value::Array(v.keys().cloned().map(Value::String).collect())))
                    .collect(),
            ),
        );
        diagnostics.insert("precision".into(), Value::String("float64".into()));
        Ok(Evaluation { provider: ALGEBRAIC_NAME.into(), responses, diagnostics, fields: BTreeMap::new() })
    }


    pub fn sensitivities_named(
        &self,
        design: &NamedArrays,
        names: &[String],
    ) -> CaeResult<DesignSensitivities> {
        let layout = self.layout(design)?;
        let unique: BTreeSet<&String> = names.iter().collect();
        if names.is_empty()
            || unique.len() != names.len()
            || names.iter().any(|n| !self.plan.response_providers.contains_key(n))
        {
            return Err(contract("batched algebraic sensitivities require unique known response names"));
        }
        let mut tape = Tape::new();
        let (values, vars) = Self::inputs(&mut tape, design);
        let mut validity = Vec::new();
        let outputs = self.forward(&mut tape, &values, &mut validity)?;
        let mut reduced = Vec::new();
        for r in names {
            reduced.push(self.reduce(&mut tape, &values, &outputs, r)?);
        }
        let stacked = tape.concat(&reduced).map_err(|e| ad(&e))?;
        self.admit_validity(&validity)?;
        let wrt: Vec<Var> = vars.iter().map(|(_, v)| *v).collect();
        let jac = tape.jacrev(stacked, &wrt).map_err(|e| ad(&e))?;
        let vals = tape.value(stacked).map_err(|e| ad(&e))?.to_vec();
        if vals.len() != names.len() || vals.iter().any(|v| !v.is_finite()) {
            return Err(contract("algebraic responses must be finite scalar reductions"));
        }
        let mut gradients = BTreeMap::new();
        for (j, name) in names.iter().enumerate() {
            let mut blocks = NamedArrays::new();
            for (k, (coord, var)) in vars.iter().enumerate() {
                let n = var.len();
                let row = jac[k][j * n..(j + 1) * n].to_vec();
                let shape = design.get(coord).map(|a| a.shape().to_vec()).unwrap_or_default();
                blocks.insert(
                    coord.clone(),
                    ArrayD::from_shape_vec(shape, row).map_err(|e| contract(e.to_string()))?,
                );
            }
            layout.pack(&blocks, &format!("algebraic derivative {name}"))?;
            gradients.insert(name.clone(), blocks);
        }
        let mut diagnostics = Map::new();
        diagnostics.insert("orchestrationPlan".into(), self.plan.as_dict());
        diagnostics.insert("exact_ad".into(), Value::String("reverse_tape_named_design".into()));
        diagnostics.insert("precision".into(), Value::String("float64".into()));
        diagnostics.insert("shared_forward_passes".into(), Value::from(1));
        diagnostics.insert("response_count".into(), Value::from(names.len()));
        Ok(DesignSensitivities {
            responses: names.iter().cloned().zip(vals).collect(),
            gradients,
            diagnostics,
        })
    }

    fn preflight_report(&self) -> JsonMap {
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        out.insert("issues".into(), Value::Array(Vec::new()));
        out.insert(
            "design_coordinates".into(),
            Value::Array(self.design_coordinates.iter().cloned().map(Value::String).collect()),
        );
        out.insert("orchestrationPlan".into(), self.plan.as_dict());
        out
    }


    pub fn coupling_report(&self, for_optimization: bool) -> CaeResult<JsonMap> {
        crate::orchestration_coupling::validate_orchestration_couplings(
            ALGEBRAIC_NAME,
            &self.plan,
            &self.entries,
            &self.context.values,
            for_optimization,
            Some(&crate::orchestration_coupling::RuntimeEvidence::Algebraic),
        )
    }
}

impl CaeProvider for AlgebraicOrchestratedProvider {
    fn name(&self) -> &str {
        ALGEBRAIC_NAME
    }
    fn provider_id(&self) -> Option<&str> {
        Some(ALGEBRAIC_NAME)
    }
    fn implementation(&self) -> &'static str {
        "implexity.cae.algebraic_runtime.AlgebraicOrchestratedProvider"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let responses: Vec<String> =
            self.plan.response_providers.keys().cloned().collect::<BTreeSet<_>>().into_iter().collect();
        let analyses = vec!["intent-composed algebraic physics".to_string()];
        if self.strict() {
            let mut d = ProviderDescriptor::new(ALGEBRAIC_NAME, analyses, responses);
            d.sensitivities = true;
            d.design_coordinates.clone_from(&self.design_coordinates);
            d.nonlinear = true;
            Ok(ProviderCapabilities::Descriptor(Box::new(d.checked()?)))
        } else {
            let mut c = LegacySingleArrayProviderCapabilities::new(ALGEBRAIC_NAME, analyses, responses);
            c.base.sensitivities = true;
            c.base.design_coordinates.clone_from(&self.design_coordinates);
            c.base.nonlinear = true;
            Ok(ProviderCapabilities::Legacy(Box::new(c.checked()?)))
        }
    }
    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Ok(json_problem(if problem.is_null() { Value::Object(Map::new()) } else { problem.clone() }))
    }
    fn preflight(
        &self,
        _problem: &ProviderProblem,
        _topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        Ok(self.preflight_report())
    }
    fn evaluate(&self, _problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        if !self.compatibility.values().all(|c| *c) {
            return Err(contract("bare-array algebraic evaluation is compatibility-only"));
        }
        self.evaluate_named(&NamedArrays::single(TOPOLOGY_COORDINATE, topology.clone()))
    }
    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        if !self.compatibility.values().all(|c| *c) {
            return Err(contract("bare-array algebraic sensitivity is compatibility-only"));
        }
        let mut out = self.sensitivities_named(
            &NamedArrays::single(TOPOLOGY_COORDINATE, topology.clone()),
            &[response.to_string()],
        )?;
        Ok(Sensitivity {
            provider: ALGEBRAIC_NAME.into(),
            response: response.into(),
            value: out.responses[response],
            gradient: out
                .gradients
                .remove(response)
                .and_then(|g| g.get(TOPOLOGY_COORDINATE).cloned())
                .unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }
    fn coupling_validation(
        &self,
        _problem: Option<&ProviderProblem>,
        for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        Some(self.coupling_report(for_optimization).map(Value::Object).map_err(|e| e.to_string()))
    }
    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        design_interface::<Self>(name)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl DesignOperations for AlgebraicOrchestratedProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::Evaluate
                | DesignOp::Sensitivity
                | DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::OptimizerLifecycle
        )
    }
    fn evaluate_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        _op: usize,
    ) -> CaeResult<Evaluation> {
        self.evaluate_named(design)
    }
    fn preflight_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        self.layout(design)?;
        Ok(self.preflight_report())
    }
    fn sensitivity_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _op: usize,
    ) -> CaeResult<DesignSensitivity> {
        let mut out = self.sensitivities_named(design, &[response.to_string()])?;
        Ok(DesignSensitivity {
            value: out.responses[response],
            gradients: out.gradients.remove(response).unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }
    fn sensitivities_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _op: usize,
    ) -> CaeResult<DesignSensitivities> {
        self.sensitivities_named(design, responses)
    }
    fn optimizer_lifecycle(&self, _problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        let strict = self.strict();
        OptimizerLifecycleConfig::new(
            self.design_coordinates.clone(),
            "sensitivity_design",
            "evaluate_design",
            None,
            None,
            strict,
            !strict,
        )
        .map(LifecycleDeclaration::Typed)
    }
}
