// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, CoordinateOptimizationSettings, Evaluation, FieldValue,
    LegacySingleArrayOptimizationSettings, LegacySingleArrayProviderCapabilities, MatchingTimeNewtonGuess,
    ProviderCapabilities, ProviderDescriptor, ProviderProblem, ResponseSpec, Sensitivity,
    TOPOLOGY_COORDINATE,
};
use implexity_core::numeric_contract::real_array;
use implexity_core::orchestration::{
    AddInAdapter, AddInRegistry, CouplingEdge, DesignCoordinateRef, ExecutionKind, ExternalPortValue,
    OrchestrationPlan, PlanStatus, PortSpec, RegisteredAddIn, RuntimeRoute,
};
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::PyNum;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::{DesignLayout, NamedArrays, design_identity};
use implexity_optim::native_design::batch_sensitivities;
use implexity_optim::numeric::{array_to_value, str_list_repr};
use implexity_optim::optimizer::{
    DirectGradientRun, OptimizerLifecycleConfig, ProgressCallback, optimise, optimise_design,
};
use implexity_optim::provider_ops::LifecycleDeclaration;
use implexity_optim::provider_ops::{
    CachedEvaluation, DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, design_interface,
    design_operations,
};
use ndarray::ArrayD;
use serde_json::{Map, Value};

use crate::addin::{AddInOperations, JsonMap, addin_operations};
use crate::results::{ExecutionOutput, json_problem, problem_json};

#[derive(Debug, Clone, PartialEq)]
pub enum DesignPortBindings {
    Keyed(Vec<(String, DesignCoordinateRef)>),
    Listed(Vec<DesignCoordinateRef>),
}


#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExecutionContext {
    pub values: JsonMap,
    pub external_ports: Option<Vec<ExternalPortValue>>,
    pub design_port_bindings: Option<DesignPortBindings>,
}

impl ExecutionContext {
    #[must_use]
    pub fn from_json(values: JsonMap) -> Self {
        Self { values, external_ports: None, design_port_bindings: None }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum DesignArg<'a> {
    Json(&'a Value),
    Named(&'a NamedArrays),
}

impl DesignArg<'_> {
    fn is_named(&self) -> bool {
        match self {
            Self::Json(v) => v.is_object(),
            Self::Named(_) => true,
        }
    }

    fn keys(&self) -> Vec<String> {
        match self {
            Self::Json(Value::Object(m)) => m.keys().cloned().collect(),
            Self::Json(_) => Vec::new(),
            Self::Named(n) => n.names(),
        }
    }

    fn array(&self, name: &str) -> CaeResult<ArrayD<f64>> {
        let label = format!("design coordinate {}", repr_str(name));
        match self {
            Self::Json(Value::Object(m)) => real_array(m.get(name).unwrap_or(&Value::Null), &label),
            Self::Json(_) => Err(CaeError::contract("named design must be a mapping")),
            Self::Named(n) => {
                let a = n.get(name).ok_or_else(|| CaeError::contract(format!("{label} is missing")))?;
                if a.iter().any(|v| !v.is_finite()) {
                    return Err(CaeError::contract(format!("{label} must contain only finite real values")));
                }
                Ok(a.clone())
            }
        }
    }

    fn bare(&self) -> CaeResult<ArrayD<f64>> {
        match self {
            Self::Json(v) => real_array(v, "topology"),
            Self::Named(_) => Err(CaeError::contract("expected a bare design array")),
        }
    }
}

pub struct ExecuteRequest<'a> {
    pub operation: &'a str,
    pub topology: Option<DesignArg<'a>>,
    pub design: Option<DesignArg<'a>>,
    pub responses: Option<Vec<ResponseSpec>>,
    pub settings: Option<&'a Value>,
    pub callback: Option<ProgressCallback<'a>>,
    pub matching_time_guess: Option<&'a MatchingTimeNewtonGuess>,
    pub require_accepted: bool,
    pub result_artifacts: bool,
}

impl<'a> ExecuteRequest<'a> {
    #[must_use]
    pub fn new(operation: &'a str) -> Self {
        Self {
            operation,
            topology: None,
            design: None,
            responses: None,
            settings: None,
            callback: None,
            matching_time_guess: None,
            require_accepted: false,
            result_artifacts: false,
        }
    }
}

fn is_compat(entry: &RegisteredAddIn) -> bool {
    entry.compatibility_mode || entry.contract.compatibility_mode
}

fn is_legacy_or_compat(entry: &RegisteredAddIn) -> bool {
    entry.compatibility_mode || entry.contract.execution_kind == Some(ExecutionKind::Legacy)
}

#[must_use]
pub fn port_token(port: &PortSpec) -> String {
    if port.port_id.is_empty() { port.quantity.clone() } else { port.port_id.clone() }
}

fn entry_port_token(entry: &RegisteredAddIn, port: &PortSpec) -> String {
    if is_legacy_or_compat(entry) { port.quantity.clone() } else { port_token(port) }
}

fn declared_output_tokens(entry: &RegisteredAddIn) -> CaeResult<Vec<String>> {
    let tokens: Vec<String> = entry.contract.provides.iter().map(port_token).collect();
    let unique: BTreeSet<&String> = tokens.iter().collect();
    if unique.len() != tokens.len() {
        return Err(CaeError::contract(format!(
            "{}: provided ports have ambiguous output tokens {}",
            entry.contract.addin_id,
            py_tuple_repr(&tokens)
        )));
    }
    Ok(tokens)
}

#[must_use]
pub fn py_tuple_repr(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| repr_str(s)).collect();
    if inner.len() == 1 { format!("({},)", inner[0]) } else { format!("({})", inner.join(", ")) }
}

fn sorted_diff(a: &BTreeSet<String>, b: &BTreeSet<String>) -> Vec<String> {
    a.difference(b).cloned().collect()
}

fn validate_operation_output(
    entry: &RegisteredAddIn,
    output: JsonMap,
    operation: &str,
) -> CaeResult<JsonMap> {
    if is_legacy_or_compat(entry) {
        return Ok(output);
    }
    let expected: BTreeSet<String> = declared_output_tokens(entry)?.into_iter().collect();
    let actual: BTreeSet<String> = output.keys().cloned().collect();
    if actual != expected {
        return Err(CaeError::contract(format!(
            "{}:{operation}: outputs do not exactly match declared provides; missing={}, extra={}",
            entry.contract.addin_id,
            str_list_repr(&sorted_diff(&expected, &actual)),
            str_list_repr(&sorted_diff(&actual, &expected))
        )));
    }
    Ok(output)
}


pub fn require_operation(entry: &RegisteredAddIn, operation: &str) -> CaeResult<bool> {
    let contract = &entry.contract;
    if !contract.supported_operations.iter().any(|o| o == operation) {
        return Err(CaeError::contract(format!(
            "{}: selected execution contract does not support operation {}",
            contract.addin_id,
            repr_str(operation)
        )));
    }
    let no_op = contract.no_op_operations.iter().any(|o| o == operation);
    if no_op && !contract.provides.is_empty() {
        return Err(CaeError::contract(format!(
            "{}:{operation}: a declared no-op cannot own provided outputs",
            contract.addin_id
        )));
    }
    Ok(no_op)
}

fn ensure_direct_invokable(entry: &RegisteredAddIn, operation: &str, extension: bool) -> CaeResult<()> {
    if entry.contract.no_op_operations.iter().any(|o| o == operation) {
        return Ok(());
    }
    let aid = &entry.contract.addin_id;
    let Some(adapter) = &entry.adapter else {
        return Err(CaeError::contract(format!("{aid}:{operation}: selected adapter is absent")));
    };
    let ops = addin_operations(adapter.as_ref());
    if extension && ops.is_some_and(AddInOperations::has_extend) {
        return Ok(());
    }
    if ops.is_some_and(|o| o.has_operation(operation) || o.has_execute()) {
        return Ok(());
    }
    Err(CaeError::contract(format!("{aid}:{operation}: declared operation has no executable callback")))
}

fn ensure_numerical_factories(selected: &[Arc<RegisteredAddIn>], kind: ExecutionKind) -> CaeResult<()> {
    for entry in selected {
        let aid = &entry.contract.addin_id;
        let Some(adapter) = &entry.adapter else {
            return Err(CaeError::contract(format!("{aid}: selected numerical adapter is absent")));
        };
        let ops = addin_operations(adapter.as_ref());
        match kind {
            ExecutionKind::Residual => {
                let any = ops.is_some_and(|o| {
                    ["residual_contributions", "response_contributions", "field_contributions"]
                        .iter()
                        .any(|h| o.provides(h))
                });
                if !any {
                    return Err(CaeError::contract(format!(
                        "{aid}: selected residual add-in supplies no executable contribution factory"
                    )));
                }
            }
            ExecutionKind::Algebraic => {
                let alg = ops.and_then(AddInOperations::algebraic);
                let ok = alg.is_some_and(|a| a.named() || entry.compatibility_mode);
                if !ok {
                    return Err(CaeError::contract(format!(
                        "{aid}: selected algebraic add-in has no executable evaluator"
                    )));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn invoke(
    entry: &RegisteredAddIn,
    operation: &str,
    context: &JsonMap,
    plan: &OrchestrationPlan,
    result: Option<&JsonMap>,
) -> CaeResult<JsonMap> {
    let no_op = require_operation(entry, operation)?;
    let aid = &entry.contract.addin_id;
    let Some(adapter) = &entry.adapter else {
        if no_op {
            return Ok(Map::new());
        }
        return Err(CaeError::contract(format!("{aid}:{operation}: selected adapter is absent")));
    };
    let ops = addin_operations(adapter.as_ref());
    if let (Some(result), Some(o)) = (result, ops)
        && o.has_extend()
    {
        return Ok(o.extend(result, operation, context, plan)?.unwrap_or_default());
    }
    if let Some(o) = ops {
        if o.has_operation(operation) {
            return Ok(o.call_operation(operation, context, plan)?.unwrap_or_default());
        }
        if o.has_execute() {
            return Ok(o.execute(operation, context, plan)?.unwrap_or_default());
        }
    }
    if no_op {
        return Ok(Map::new());
    }
    Err(CaeError::contract(format!("{aid}:{operation}: declared operation has no executable callback")))
}


pub fn execution_kind(entry: &RegisteredAddIn) -> CaeResult<ExecutionKind> {
    let aid = &entry.contract.addin_id;
    match entry.contract.execution_kind {
        None => Err(CaeError::contract(format!("{aid}: execution kind is undeclared"))),
        Some(ExecutionKind::Legacy) => {
            if !entry.compatibility_mode {
                return Err(CaeError::contract(format!("{aid}: legacy execution kind is not canonical")));
            }
            let adapter = entry.adapter.as_deref();
            let ops = adapter.and_then(addin_operations);
            let residual = ops.is_some_and(|o| o.provides("residual_contributions"));
            let algebraic = ops.and_then(AddInOperations::algebraic).is_some();
            let provider = adapter.and_then(AddInAdapter::provider).is_some();
            let present = usize::from(residual) + usize::from(algebraic) + usize::from(provider);
            if present > 1 {
                return Err(CaeError::contract(format!(
                    "{aid}: compatibility adapter exposes ambiguous numerical execution forms"
                )));
            }
            Ok(if residual {
                ExecutionKind::Residual
            } else if algebraic {
                ExecutionKind::Algebraic
            } else if provider {
                ExecutionKind::Provider
            } else {
                ExecutionKind::Operation
            })
        }
        Some(kind) => Ok(kind),
    }
}

type RouteKey = (String, String, String, String, Option<String>, String, bool, String, String);

fn port_route_key(port: &PortSpec, compatibility: bool) -> RouteKey {
    (
        if compatibility { String::new() } else { port.port_id.clone() },
        port.quantity.clone(),
        port.unit.clone(),
        port.domain.clone(),
        port.interface.clone(),
        port.temporal.clone(),
        port.conserved,
        port.cardinality.clone(),
        port.aggregation.clone(),
    )
}

#[must_use]
pub fn route_key_repr(key: &RouteKey) -> String {
    format!(
        "({}, {}, {}, {}, {}, {}, {}, {}, {})",
        repr_str(&key.0),
        repr_str(&key.1),
        repr_str(&key.2),
        repr_str(&key.3),
        key.4.as_deref().map_or_else(|| "None".to_string(), repr_str),
        repr_str(&key.5),
        if key.6 { "True" } else { "False" },
        repr_str(&key.7),
        repr_str(&key.8)
    )
}

fn edge_matches(port: &PortSpec, edge: &CouplingEdge, domain: &str) -> bool {
    port.quantity == edge.quantity
        && port.unit == edge.unit
        && port.domain == domain
        && port.interface == edge.interface
        && port.temporal == edge.temporal
        && port.conserved == edge.conserved
        && port.cardinality == edge.cardinality
        && port.aggregation == edge.aggregation
}


pub fn edge_ports<'a>(
    edge: &CouplingEdge,
    source: &'a RegisteredAddIn,
    target: &'a RegisteredAddIn,
) -> CaeResult<(&'a PortSpec, &'a PortSpec)> {
    let sources: Vec<&PortSpec> = source
        .contract
        .provides
        .iter()
        .filter(|p| {
            edge_matches(p, edge, &edge.source_domain)
                && (edge.source_port_id.is_empty() || p.port_id == edge.source_port_id)
        })
        .collect();
    let targets: Vec<&PortSpec> = target
        .contract
        .consumes
        .iter()
        .filter(|p| {
            edge_matches(p, edge, &edge.target_domain)
                && (edge.target_port_id.is_empty() || p.port_id == edge.target_port_id)
        })
        .collect();
    if sources.len() != 1 || targets.len() != 1 {
        return Err(CaeError::contract(format!(
            "coupling edge {}->{}:{} does not resolve to exactly one typed source and target port (sources={}, targets={})",
            edge.source,
            edge.target,
            edge.quantity,
            sources.len(),
            targets.len()
        )));
    }
    let (s, t) = (sources[0], targets[0]);
    if s.conserved != t.conserved {
        return Err(CaeError::contract(format!(
            "coupling edge {}->{}:{} has incompatible conserved",
            edge.source, edge.target, edge.quantity
        )));
    }
    if s.cardinality != t.cardinality {
        return Err(CaeError::contract(format!(
            "coupling edge {}->{}:{} has incompatible cardinality",
            edge.source, edge.target, edge.quantity
        )));
    }
    Ok((s, t))
}

fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn unsupported_operand(op: &str, a: &Value, b: &Value) -> CaeError {
    CaeError::contract(format!(
        "unsupported operand type(s) for {op}: '{}' and '{}'",
        py_type_name(a),
        py_type_name(b)
    ))
}

fn json_add(a: &Value, b: &Value) -> CaeResult<Value> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if let (Some(i), Some(j)) = (x.as_i64(), y.as_i64())
                && x.is_i64()
                && y.is_i64()
            {
                return Ok(i.checked_add(j).map_or_else(
                    || implexity_optim::numeric::float_value(to_f64(a) + to_f64(b)),
                    Value::from,
                ));
            }
            Ok(implexity_optim::numeric::float_value(to_f64(a) + to_f64(b)))
        }
        (Value::Array(x), Value::Array(y)) => Ok(Value::Array(x.iter().chain(y.iter()).cloned().collect())),
        (Value::String(x), Value::String(y)) => Ok(Value::String(format!("{x}{y}"))),
        _ => Err(unsupported_operand("+", a, b)),
    }
}

#[allow(clippy::cast_precision_loss)]
fn to_f64(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

fn elementwise(a: &Value, b: &Value, f: &dyn Fn(f64, f64) -> f64, label: &str) -> CaeResult<Value> {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => {
            Ok(implexity_optim::numeric::float_value(f(to_f64(a), to_f64(b))))
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() == y.len() {
                Ok(Value::Array(
                    x.iter().zip(y).map(|(p, q)| elementwise(p, q, f, label)).collect::<CaeResult<_>>()?,
                ))
            } else if x.len() == 1 {
                Ok(Value::Array(y.iter().map(|q| elementwise(&x[0], q, f, label)).collect::<CaeResult<_>>()?))
            } else if y.len() == 1 {
                Ok(Value::Array(x.iter().map(|p| elementwise(p, &y[0], f, label)).collect::<CaeResult<_>>()?))
            } else {
                Err(CaeError::contract(format!(
                    "{label}: operands could not be broadcast together with shapes ({},) ({},)",
                    x.len(),
                    y.len()
                )))
            }
        }
        (Value::Array(x), Value::Number(_)) => {
            Ok(Value::Array(x.iter().map(|p| elementwise(p, b, f, label)).collect::<CaeResult<_>>()?))
        }
        (Value::Number(_), Value::Array(y)) => {
            Ok(Value::Array(y.iter().map(|q| elementwise(a, q, f, label)).collect::<CaeResult<_>>()?))
        }
        _ => Err(CaeError::contract(format!("{label}: aggregation requires numeric values"))),
    }
}


pub fn aggregate(values: &[Value], semantics: &str, label: &str) -> CaeResult<Value> {
    let Some(first) = values.first() else {
        return Err(CaeError::contract(format!("{label}: aggregation has no inputs")));
    };
    match semantics {
        "single" => {
            if values.len() != 1 {
                return Err(CaeError::contract(format!(
                    "{label}: multiple producers require an explicit aggregator"
                )));
            }
            Ok(first.clone())
        }
        "sum" | "mean" => {
            let mut value = first.clone();
            for other in &values[1..] {
                value = json_add(&value, other)?;
            }
            if semantics == "sum" {
                return Ok(value);
            }
            #[allow(clippy::cast_precision_loss)]
            let n = values.len() as f64;
            match &value {
                Value::Number(_) => Ok(implexity_optim::numeric::float_value(to_f64(&value) / n)),
                other => Err(CaeError::contract(format!(
                    "unsupported operand type(s) for /: '{}' and 'int'",
                    py_type_name(other)
                ))),
            }
        }
        "minimum" | "maximum" => {
            let f: &dyn Fn(f64, f64) -> f64 = if semantics == "minimum" {
                &|x: f64, y: f64| if x.is_nan() || y.is_nan() { f64::NAN } else { x.min(y) }
            } else {
                &|x: f64, y: f64| if x.is_nan() || y.is_nan() { f64::NAN } else { x.max(y) }
            };
            let mut value = first.clone();
            for other in &values[1..] {
                value = elementwise(&value, other, f, label)?;
            }
            Ok(value)
        }
        "concatenate" => {
            let mut out = Vec::new();
            for v in values {
                match v {
                    Value::Array(items) => out.extend(items.iter().cloned()),
                    _ => {
                        return Err(CaeError::contract(format!(
                            "{label}: zero-dimensional arrays cannot be concatenated"
                        )));
                    }
                }
            }
            Ok(Value::Array(out))
        }
        other => {
            Err(CaeError::contract(format!("{label}: unsupported aggregation semantics {}", repr_str(other))))
        }
    }
}

fn compatibility_marker(addins: &[String]) -> JsonMap {
    let mut sorted = addins.to_vec();
    sorted.sort();
    let mut m = Map::new();
    m.insert("truth_status".into(), Value::String("compatibility_nonauthoritative".into()));
    m.insert("compatibility_addins".into(), Value::Array(sorted.into_iter().map(Value::String).collect()));
    m
}


pub fn mark_compatibility_evaluation(
    value: ExecutionOutput,
    addins: &[String],
) -> CaeResult<ExecutionOutput> {
    if addins.is_empty() {
        return Ok(value);
    }
    let marker = compatibility_marker(addins);
    match value {
        ExecutionOutput::Evaluation(mut e) => {
            for (k, v) in marker {
                e.diagnostics.insert(k, v);
            }
            Ok(ExecutionOutput::Evaluation(e))
        }
        ExecutionOutput::Json(mut out) => {
            for (k, v) in &marker {
                out.insert(k.clone(), v.clone());
            }
            if let Some(Value::Object(d)) = out.get_mut("diagnostics") {
                for (k, v) in marker {
                    d.insert(k, v);
                }
            }
            Ok(ExecutionOutput::Json(out))
        }
        _ => Err(CaeError::contract("compatibility evaluation returned an unsupported result type")),
    }
}

#[must_use]
pub fn namespaced_field_metadata(
    provider: &str,
    metadata: &Value,
    fields: &BTreeMap<String, FieldValue>,
) -> Value {
    let Value::Object(result) = metadata else { return metadata.clone() };
    let mut result = result.clone();
    let reference = |name: &Value| -> Value {
        match name {
            Value::String(s) => {
                let candidate = format!("{provider}::{s}");
                if fields.contains_key(&candidate) { Value::String(candidate) } else { name.clone() }
            }
            other => other.clone(),
        }
    };
    for key in ["node_field", "reference_coordinate_field"] {
        if let Some(v) = result.get(key).cloned() {
            result.insert(key.into(), reference(&v));
        }
    }
    if let Some(Value::Array(items)) = result.get("source_fields").cloned() {
        result.insert("source_fields".into(), Value::Array(items.iter().map(reference).collect()));
    }
    Value::Object(result)
}

fn plan_owner_ids(plan: &OrchestrationPlan) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in plan.response_providers.values() {
        let s = crate::pyval::py_str(v);
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

fn default_specs(plan: &OrchestrationPlan) -> CaeResult<Vec<ResponseSpec>> {
    plan.response_providers
        .keys()
        .map(|n| ResponseSpec::new(n, PyNum::Float(1.0), "minimise", None, PyNum::Float(1.0)))
        .collect()
}

fn coordinate_settings(settings: Option<&Value>, strict: bool) -> CaeResult<CoordinateOptimizationSettings> {
    if strict {
        CoordinateOptimizationSettings::from_dict(settings)
    } else {
        Ok(LegacySingleArrayOptimizationSettings::from_dict(settings)?.as_coordinate_settings())
    }
}

fn provider_id_of(provider: &dyn CaeProvider) -> String {
    provider.provider_id().map_or_else(|| provider.name().to_string(), str::to_string)
}

fn missing_extra(expected: &[String], actual: &[String]) -> (Vec<String>, Vec<String>) {
    let missing = expected.iter().filter(|n| !actual.contains(n)).cloned().collect();
    let extra = actual.iter().filter(|n| !expected.contains(n)).cloned().collect();
    (missing, extra)
}

fn check_scalar(value: f64, pid: &str, name: &str) -> CaeResult<()> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(CaeError::contract(format!(
            "provider {} returned nonfinite/non-scalar response {}",
            repr_str(pid),
            repr_str(name)
        )))
    }
}


pub struct IndependentProviderEnsemble {
    plan: Arc<OrchestrationPlan>,
    providers: Vec<(String, Arc<dyn CaeProvider>)>,
    context: ExecutionContext,
    strict: bool,
    route: Vec<(String, usize)>,
}

impl std::fmt::Debug for IndependentProviderEnsemble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndependentProviderEnsemble")
            .field("providers", &self.providers.iter().map(|(k, _)| k).collect::<Vec<_>>())
            .field("strict", &self.strict)
            .finish_non_exhaustive()
    }
}

pub const ENSEMBLE_NAME: &str = "orchestrated_provider_ensemble";

impl IndependentProviderEnsemble {

    pub fn new(
        plan: Arc<OrchestrationPlan>,
        providers: Vec<(String, Arc<dyn CaeProvider>)>,
        context: ExecutionContext,
        strict: bool,
    ) -> CaeResult<Self> {
        for (aid, provider) in &providers {
            let declared = provider_id_of(provider.as_ref());
            if &declared != aid {
                return Err(CaeError::contract(format!(
                    "independent provider identity mismatch: selected {}, got {}",
                    repr_str(aid),
                    repr_str(&declared)
                )));
            }
        }
        let mut route = Vec::new();
        for (response, aid) in &plan.response_providers {
            let aid = crate::pyval::py_str(aid);
            let Some(index) = providers.iter().position(|(k, _)| *k == aid) else {
                return Err(CaeError::contract(format!(
                    "orchestration response {} selects unavailable provider {}",
                    repr_str(response),
                    repr_str(&aid)
                )));
            };
            route.push((response.clone(), index));
        }
        Ok(Self { plan, providers, context, strict, route })
    }

    fn route_index(&self, response: &str) -> Option<usize> {
        self.route.iter().find(|(r, _)| r == response).map(|(_, i)| *i)
    }

    fn used(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for (_, i) in &self.route {
            let ptr = Arc::as_ptr(&self.providers[*i].1).cast::<()>();
            if !out.iter().any(|j| Arc::as_ptr(&self.providers[*j].1).cast::<()>() == ptr) {
                out.push(*i);
            }
        }
        out
    }

    fn expected_for(&self, index: usize) -> BTreeSet<String> {
        let ptr = Arc::as_ptr(&self.providers[index].1).cast::<()>();
        self.route
            .iter()
            .filter(|(_, i)| Arc::as_ptr(&self.providers[*i].1).cast::<()>() == ptr)
            .map(|(r, _)| r.clone())
            .collect()
    }

    fn pid(&self, index: usize) -> String {
        provider_id_of(self.providers[index].1.as_ref())
    }

    fn child_problem(&self, index: usize) -> CaeResult<ProviderProblem> {
        let pid = self.pid(index);
        let raw = self
            .context
            .get("provider_problems")
            .and_then(Value::as_object)
            .and_then(|m| m.get(&pid))
            .or_else(|| self.context.get("problem"))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        self.providers[index].1.normalise_problem(&raw)
    }


    pub fn ensemble_capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let mut sets: Vec<Vec<String>> = Vec::new();
        for i in self.used() {
            let pid = self.pid(i);
            let caps = self.providers[i].1.capabilities()?;
            let declared = caps.design_coordinates();
            let named_ok = match &caps {
                ProviderCapabilities::Mapping(m) => {
                    m.get("design_coordinates").is_some_and(|v| v.as_array().is_some())
                }
                _ => true,
            };
            if !named_ok {
                return Err(CaeError::contract(format!(
                    "provider {} omitted named design_coordinates",
                    repr_str(&pid)
                )));
            }
            let unique: BTreeSet<&String> = declared.iter().collect();
            if declared.is_empty() || declared.iter().any(String::is_empty) || unique.len() != declared.len()
            {
                return Err(CaeError::contract(format!(
                    "provider {} has invalid named design_coordinates",
                    repr_str(&pid)
                )));
            }
            sets.push(declared);
        }
        let mut responses: Vec<String> = self.route.iter().map(|(r, _)| r.clone()).collect();
        responses.sort();
        if self.strict {
            let ordered = self.plan.active_design_coordinates.clone();
            if ordered.is_empty() {
                return Err(CaeError::contract(
                    "strict independent ensemble requires active design coordinates",
                ));
            }
            if sets.iter().any(|row| !ordered.iter().all(|c| row.contains(c))) {
                return Err(CaeError::contract(format!(
                    "independent providers do not all declare the requested active design subset {}",
                    str_list_repr(&ordered)
                )));
            }
            let mut d = ProviderDescriptor::new(
                ENSEMBLE_NAME,
                vec!["intent-composed independent physics".into()],
                responses,
            );
            d.sensitivities = true;
            d.design_coordinates = ordered;
            return Ok(ProviderCapabilities::Descriptor(Box::new(d.checked()?)));
        }
        let mut common: BTreeSet<String> = sets.first().map_or_else(
            || std::iter::once(TOPOLOGY_COORDINATE.to_string()).collect(),
            |s| s.iter().cloned().collect(),
        );
        for row in &sets[1.min(sets.len())..] {
            common.retain(|c| row.contains(c));
        }
        if !common.contains(TOPOLOGY_COORDINATE) {
            return Err(CaeError::contract(format!(
                "legacy independent providers do not share {}",
                repr_str(TOPOLOGY_COORDINATE)
            )));
        }
        let mut ordered = vec![TOPOLOGY_COORDINATE.to_string()];
        ordered.extend(common.into_iter().filter(|c| c != TOPOLOGY_COORDINATE));
        let mut caps = LegacySingleArrayProviderCapabilities::new(
            ENSEMBLE_NAME,
            vec!["compatibility independent physics".into()],
            responses,
        );
        caps.base.design_coordinates = ordered;
        Ok(ProviderCapabilities::Legacy(Box::new(caps.checked()?)))
    }

    fn exact_design(&self, design: &NamedArrays) -> CaeResult<NamedArrays> {
        if design.is_empty() {
            return Err(CaeError::contract("named design must be a nonempty mapping"));
        }
        if design.names().iter().any(String::is_empty) {
            return Err(CaeError::contract("named-design coordinate ids must be nonempty text"));
        }
        for (k, v) in design.iter() {
            if v.iter().any(|x| !x.is_finite()) {
                return Err(CaeError::contract(format!(
                    "design coordinate {} must contain only finite real values",
                    repr_str(k)
                )));
            }
        }
        let expected = if self.strict {
            self.plan.active_design_coordinates.clone()
        } else {
            self.ensemble_capabilities()?.design_coordinates()
        };
        let (missing, extra) = missing_extra(&expected, &design.names());
        if !missing.is_empty() || !extra.is_empty() {
            return Err(CaeError::contract(format!(
                "independent ensemble requires exact named design keys; missing={}, extra={}",
                str_list_repr(&missing),
                str_list_repr(&extra)
            )));
        }
        Ok(expected.iter().map(|n| (n.clone(), design.get(n).cloned().unwrap_or_default())).collect())
    }

    fn matching_time_guess_owner(&self, operation: DesignOp) -> CaeResult<(String, usize)> {
        let owners = plan_owner_ids(&self.plan);
        if owners.len() != 1 {
            return Err(CaeError::contract(format!(
                "matching-time guess lifecycle requires exactly one selected response owner; got {}",
                str_list_repr(&owners)
            )));
        }
        let aid = owners[0].clone();
        if !self.plan.selected_addins.contains(&aid) {
            return Err(CaeError::contract(format!(
                "matching-time guess response provider {} is not selected",
                repr_str(&aid)
            )));
        }
        let Some(index) = self.providers.iter().position(|(k, _)| *k == aid) else {
            return Err(CaeError::contract(format!(
                "matching-time guess selected provider {} is unavailable or mismatched",
                repr_str(&aid)
            )));
        };
        if self.pid(index) != aid {
            return Err(CaeError::contract(format!(
                "matching-time guess selected provider {} is unavailable or mismatched",
                repr_str(&aid)
            )));
        }
        if !design_operations(self.providers[index].1.as_ref()).is_some_and(|o| o.provides(operation)) {
            return Err(CaeError::contract(format!(
                "provider {} has no {} lifecycle operation",
                repr_str(&aid),
                operation.name()
            )));
        }
        Ok((aid, index))
    }


    pub fn install_guess(&self, design: &NamedArrays, guess: &MatchingTimeNewtonGuess) -> CaeResult<JsonMap> {
        let canonical = self.exact_design(design)?;
        let (aid, index) = self.matching_time_guess_owner(DesignOp::InstallMatchingTimeGuess)?;
        let expected = design_identity(&canonical)?;
        let provider = self.providers[index].1.as_ref();
        let ops =
            design_operations(provider).ok_or_else(|| CaeError::contract("provider hooks unavailable"))?;
        let result = ops.install_matching_time_guess(&self.child_problem(index)?, &canonical, guess)?;
        if result.get("design_state_id") != Some(&Value::String(expected.clone()))
            || result.get("canonical_cache_admission") != Some(&Value::Bool(false))
        {
            return Err(CaeError::contract(format!(
                "provider {}: matching-time guess acknowledgement is stale or unsafe",
                repr_str(&aid)
            )));
        }
        let mut out = Map::new();
        out.insert("schema".into(), Value::String("implexity-matching-time-guess-installation/1".into()));
        out.insert("lifecycle_owner".into(), Value::String(aid));
        out.insert("design_state_id".into(), Value::String(expected));
        out.insert("canonical_cache_admission".into(), Value::Bool(false));
        out.insert("provider_acknowledgement".into(), Value::Object(result));
        Ok(out)
    }


    pub fn export_guess(
        &self,
        design: &NamedArrays,
        require_accepted: bool,
    ) -> CaeResult<MatchingTimeNewtonGuess> {
        let canonical = self.exact_design(design)?;
        let (_aid, index) = self.matching_time_guess_owner(DesignOp::ExportMatchingTimeGuess)?;
        let provider = self.providers[index].1.as_ref();
        let ops =
            design_operations(provider).ok_or_else(|| CaeError::contract("provider hooks unavailable"))?;
        ops.export_matching_time_guess(&self.child_problem(index)?, &canonical, require_accepted)
    }


    pub fn accept(&self, design: &NamedArrays) -> CaeResult<JsonMap> {
        let canonical = self.exact_design(design)?;
        DesignLayout::from_values(&canonical)?;
        let expected_identity = design_identity(&canonical)?;
        let owners = plan_owner_ids(&self.plan);
        if owners.is_empty() {
            return Err(CaeError::contract("accepted-design forwarding has no selected response provider"));
        }
        let mut stateful: Vec<(String, usize)> = Vec::new();
        for aid in &owners {
            if !self.plan.selected_addins.contains(aid) {
                return Err(CaeError::contract(format!(
                    "accepted-design response provider {} is not selected by the orchestration plan",
                    repr_str(aid)
                )));
            }
            let Some(index) = self.providers.iter().position(|(k, _)| k == aid) else {
                return Err(CaeError::contract(format!(
                    "accepted-design selected provider {} is unavailable",
                    repr_str(aid)
                )));
            };
            let declared = self.pid(index);
            if &declared != aid {
                return Err(CaeError::contract(format!(
                    "accepted-design provider identity mismatch: selected {}, got {}",
                    repr_str(aid),
                    repr_str(&declared)
                )));
            }
            if design_operations(self.providers[index].1.as_ref())
                .is_some_and(|o| o.provides(DesignOp::AcceptDesign))
            {
                stateful.push((aid.clone(), index));
            }
        }
        if stateful.len() > 1 {
            let names: Vec<String> = stateful.iter().map(|(a, _)| a.clone()).collect();
            return Err(CaeError::contract(format!(
                "accepted-design forwarding has multiple stateful providers and no atomic commit protocol: {}",
                str_list_repr(&names)
            )));
        }
        if self.strict && stateful.is_empty() {
            return Err(CaeError::contract(
                "strict accepted-design forwarding requires one explicitly implemented lifecycle owner",
            ));
        }
        let mut forwarded = Vec::new();
        let mut acknowledgement = Map::new();
        if let Some((aid, index)) = stateful.first() {
            let provider = self.providers[*index].1.as_ref();
            let ops = design_operations(provider)
                .ok_or_else(|| CaeError::contract("provider hooks unavailable"))?;
            let raw = ops.accept_design(&self.child_problem(*index)?, &canonical)?;
            if self.strict {
                let Some(ack) = raw else {
                    return Err(CaeError::contract(format!(
                        "provider {}: strict accept_design must return an acknowledgement mapping",
                        repr_str(aid)
                    )));
                };
                if ack.contains_key("designStateId") {
                    return Err(CaeError::contract(format!(
                        "provider {}: designStateId is compatibility-only; use design_state_id",
                        repr_str(aid)
                    )));
                }
                if ack.get("design_state_id") != Some(&Value::String(expected_identity.clone())) {
                    return Err(CaeError::contract(format!(
                        "provider {}: accepted-design acknowledgement identity is stale or missing",
                        repr_str(aid)
                    )));
                }
                acknowledgement = ack;
            }
            forwarded.push(Value::String(aid.clone()));
        }
        let mut out = Map::new();
        out.insert("schema".into(), Value::String("implexity-accepted-design-forwarding/1".into()));
        out.insert(
            "selected_response_providers".into(),
            Value::Array(owners.into_iter().map(Value::String).collect()),
        );
        out.insert("forwarded_providers".into(), Value::Array(forwarded));
        out.insert("design_state_id".into(), Value::String(expected_identity));
        out.insert("provider_acknowledgement".into(), Value::Object(acknowledgement));
        Ok(out)
    }


    pub fn ensemble_preflight(&self, topology: Option<&ArrayD<f64>>) -> CaeResult<JsonMap> {
        if self.strict && topology.is_some() {
            return Err(CaeError::contract("bare-array preflight is compatibility-only"));
        }
        let mut issues = Vec::new();
        for (index, (aid, provider)) in self.providers.iter().enumerate() {
            let problem = self.child_problem(index)?;
            let rep = provider.preflight(&problem, topology)?;
            if rep.get("ok") != Some(&Value::Bool(true)) && !crate::pyval::truthy(rep.get("ok")) {
                let mut row = Map::new();
                row.insert("addin".into(), Value::String(aid.clone()));
                row.insert("report".into(), Value::Object(rep));
                issues.push(Value::Object(row));
            }
        }
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(issues.is_empty()));
        out.insert("issues".into(), Value::Array(issues));
        out.insert("orchestrationPlan".into(), self.plan.as_dict());
        Ok(out)
    }

    fn merged_evaluation(
        &self,
        problem: Option<&Value>,
        mut per: Vec<(usize, Evaluation)>,
        certify: bool,
    ) -> CaeResult<Evaluation> {
        let mut responses = BTreeMap::new();
        let mut diagnostics = Map::new();
        let mut fields: BTreeMap<String, FieldValue> = BTreeMap::new();
        for (index, e) in &mut per {
            let pid = self.pid(*index);
            diagnostics.insert(pid.clone(), Value::Object(std::mem::take(&mut e.diagnostics)));
            let expected = self.expected_for(*index);
            let missing: Vec<String> =
                expected.iter().filter(|n| !e.responses.contains_key(*n)).cloned().collect();
            if !missing.is_empty() && self.strict {
                return Err(CaeError::contract(format!(
                    "provider {} omitted selected responses {}",
                    repr_str(&pid),
                    str_list_repr(&missing)
                )));
            }
            for name in &expected {
                if let Some(v) = e.responses.get(name) {
                    check_scalar(*v, &pid, name)?;
                }
            }
            for (k, v) in std::mem::take(&mut e.fields) {
                fields.insert(format!("{pid}::{k}"), v);
            }
            for name in &expected {
                if let Some(v) = e.responses.get(name) {
                    responses.insert(name.clone(), *v);
                }
            }
        }
        let route: BTreeSet<&String> = self.route.iter().map(|(r, _)| r).collect();
        if self.strict && responses.keys().collect::<BTreeSet<_>>() != route {
            return Err(CaeError::contract(
                "independent ensemble did not produce every selected response exactly once",
            ));
        }
        if responses.is_empty() {
            return Err(CaeError::contract("independent ensemble produced no declared response"));
        }
        let mut out = Map::new();
        out.insert("providers".into(), Value::Object(diagnostics.clone()));
        out.insert("orchestrationPlan".into(), self.plan.as_dict());
        if certify {
            out.insert(
                "physical_certification".into(),
                Value::Object(self.certification(problem, &diagnostics)?),
            );
        }
        let mut meta = Map::new();
        for (pid, diag) in &diagnostics {
            if let Some(Value::Object(rows)) = diag.get("field_metadata") {
                for (name, m) in rows {
                    meta.insert(format!("{pid}::{name}"), namespaced_field_metadata(pid, m, &fields));
                }
            }
        }
        out.insert("field_metadata".into(), Value::Object(meta));
        Ok(Evaluation { provider: ENSEMBLE_NAME.into(), responses, diagnostics: out, fields })
    }


    pub fn evaluate_array(&self, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        if self.strict {
            return Err(CaeError::contract("bare-array provider evaluation is compatibility-only"));
        }
        let mut per = Vec::new();
        for i in self.used() {
            let problem = self.child_problem(i)?;
            per.push((i, self.providers[i].1.evaluate(&problem, topology)?));
        }
        self.merged_evaluation(None, per, false)
    }


    pub fn sensitivity_array(&self, topology: &ArrayD<f64>, response: &str) -> CaeResult<Sensitivity> {
        if self.strict {
            return Err(CaeError::contract("bare-array provider sensitivity is compatibility-only"));
        }
        let Some(index) = self.route_index(response) else {
            return Err(CaeError::contract(format!("unknown orchestrated response {}", repr_str(response))));
        };
        let pid = self.pid(index);
        let problem = self.child_problem(index)?;
        let raw = self.providers[index].1.sensitivity(&problem, topology, response)?;
        if !raw.value.is_finite() {
            return Err(CaeError::contract(format!(
                "provider {} returned nonfinite sensitivity value",
                repr_str(&pid)
            )));
        }
        if raw.gradient.shape() != topology.shape() || raw.gradient.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(format!(
                "provider {} returned invalid sensitivity gradient for {}: expected {}, got {}",
                repr_str(&pid),
                repr_str(response),
                implexity_optim::numeric::shape_repr(topology.shape()),
                implexity_optim::numeric::shape_repr(raw.gradient.shape())
            )));
        }
        let mut diagnostics = Map::new();
        diagnostics.insert("source_provider".into(), Value::String(pid));
        diagnostics.insert("source_diagnostics".into(), Value::Object(raw.diagnostics));
        diagnostics.insert("orchestrationPlan".into(), self.plan.as_dict());
        Ok(Sensitivity {
            provider: ENSEMBLE_NAME.into(),
            response: response.into(),
            value: raw.value,
            gradient: raw.gradient,
            diagnostics,
        })
    }

    fn certification(&self, problem: Option<&Value>, evidence: &JsonMap) -> CaeResult<JsonMap> {
        let mut reports = Map::new();
        let owners = self.used();
        for &i in &owners {
            let provider = self.providers[i].1.as_ref();
            let Some(ops) =
                design_operations(provider).filter(|o| o.provides(DesignOp::PhysicalCertification))
            else {
                continue;
            };
            let mut report = ops.physical_certification(&self.child_problem(i)?)?;
            let valid = |m: &JsonMap| matches!(m.get("certified"), None | Some(Value::Bool(_) | Value::Null));
            if !valid(&report) {
                return Err(CaeError::contract("invalid provider physical-certification capability"));
            }
            let pid = self.pid(i);
            let current = evidence.get(&pid).and_then(|d| d.get("physical_certification"));
            if report.get("certified") != Some(&Value::Bool(false))
                && let Some(current) = current
                && !current.is_null()
            {
                let Value::Object(c) = current else {
                    return Err(CaeError::contract("invalid provider physical-certification evidence"));
                };
                if !valid(c) {
                    return Err(CaeError::contract("invalid provider physical-certification evidence"));
                }
                report.clone_from(c);
            }
            reports.insert(pid, Value::Object(report));
        }
        let denied = reports.values().any(|r| r.get("certified") == Some(&Value::Bool(false)));
        let passed = !reports.is_empty()
            && reports.len() == owners.len()
            && reports.values().all(|r| r.get("certified") == Some(&Value::Bool(true)));
        let mut out = Map::new();
        out.insert(
            "certified".into(),
            if passed {
                Value::Bool(true)
            } else if denied {
                Value::Bool(false)
            } else {
                Value::Null
            },
        );
        out.insert(
            "authority".into(),
            Value::String(
                if passed {
                    "coupled_equilibrium_certified"
                } else if denied {
                    "exploratory_non_authoritative"
                } else {
                    "not_established"
                }
                .into(),
            ),
        );
        out.insert("scope".into(), Value::String("provider_declared_certification_scopes_only".into()));
        out.insert("providers".into(), Value::Object(reports));
        if problem.and_then(|p| p.get("physical_certification_required")) == Some(&Value::Bool(true))
            && !passed
        {
            return Err(CaeError::contract("ensemble physical certification is not established"));
        }
        Ok(out)
    }

    fn evaluate_named(
        &self,
        problem: Option<&Value>,
        design: &NamedArrays,
        result_artifacts: bool,
    ) -> CaeResult<Evaluation> {
        let values = self.exact_design(design)?;
        let mut per = Vec::new();
        for i in self.used() {
            let provider = self.providers[i].1.as_ref();
            let pid = self.pid(i);
            let problem = self.child_problem(i)?;
            let ops = design_operations(provider);
            let e = if let Some(o) =
                ops.filter(|o| result_artifacts && o.provides(DesignOp::EvaluateResultsDesign))
            {
                o.evaluate_results_design(&problem, &values)?
            } else if let Some(o) = ops.filter(|o| o.provides(DesignOp::EvaluateDesign)) {
                o.evaluate_design(&problem, &values, 0)?
            } else if !self.strict && values.names() == [TOPOLOGY_COORDINATE.to_string()] {
                provider
                    .evaluate(&problem, values.get(TOPOLOGY_COORDINATE).unwrap_or(&ArrayD::zeros(vec![0])))?
            } else {
                return Err(CaeError::contract(format!(
                    "provider {} has no evaluate_design() for coordinates {}",
                    repr_str(&pid),
                    str_list_repr(&values.names())
                )));
            };
            per.push((i, e));
        }
        self.merged_evaluation(problem, per, true)
    }

    fn preflight_named(&self, design: &NamedArrays) -> CaeResult<JsonMap> {
        let values = self.exact_design(design)?;
        DesignLayout::from_values(&values)?;
        let mut reports = Map::new();
        let mut issues = Vec::new();
        for i in self.used() {
            let provider = self.providers[i].1.as_ref();
            let pid = self.pid(i);
            let problem = self.child_problem(i)?;
            let report = if let Some(o) =
                design_operations(provider).filter(|o| o.provides(DesignOp::PreflightDesign))
            {
                o.preflight_design(&problem, &values)?
            } else if !self.strict {
                provider.preflight(&problem, values.get(TOPOLOGY_COORDINATE))?
            } else {
                return Err(CaeError::contract(format!(
                    "provider {} has no strict preflight_design operation",
                    repr_str(&pid)
                )));
            };
            reports.insert(pid.clone(), Value::Object(report.clone()));
            if !crate::pyval::truthy(report.get("ok")) {
                let mut row = Map::new();
                row.insert("provider".into(), Value::String(pid));
                row.insert("report".into(), Value::Object(report));
                issues.push(Value::Object(row));
            }
        }
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(issues.is_empty()));
        out.insert("issues".into(), Value::Array(issues));
        out.insert("providers".into(), Value::Object(reports));
        out.insert("orchestrationPlan".into(), self.plan.as_dict());
        Ok(out)
    }

    fn sensitivity_named(
        &self,
        problem: Option<&Value>,
        design: &NamedArrays,
        response: &str,
    ) -> CaeResult<DesignSensitivity> {
        let Some(index) = self.route_index(response) else {
            return Err(CaeError::contract(format!("unknown orchestrated response {}", repr_str(response))));
        };
        let values = self.exact_design(design)?;
        let provider = self.providers[index].1.as_ref();
        let pid = self.pid(index);
        let (value, grads, diag) = if let Some(o) =
            design_operations(provider).filter(|o| o.provides(DesignOp::SensitivityDesign))
        {
            let raw = o.sensitivity_design(&self.child_problem(index)?, &values, response, 0)?;
            (raw.value, raw.gradients, raw.diagnostics)
        } else if !self.strict && values.names() == [TOPOLOGY_COORDINATE.to_string()] {
            let topo = values.get(TOPOLOGY_COORDINATE).cloned().unwrap_or_default();
            let s = self.sensitivity_array(&topo, response)?;
            (s.value, NamedArrays::single(TOPOLOGY_COORDINATE, s.gradient), s.diagnostics)
        } else {
            return Err(CaeError::contract(format!(
                "provider {} has no sensitivity_design() for coordinates {}",
                repr_str(&pid),
                str_list_repr(&values.names())
            )));
        };
        let (missing, extra) = missing_extra(&values.names(), &grads.names());
        if !missing.is_empty() || !extra.is_empty() {
            return Err(CaeError::contract(format!(
                "provider {} derivative {} coordinate mismatch; missing={}, extra={}",
                repr_str(&pid),
                repr_str(response),
                str_list_repr(&missing),
                str_list_repr(&extra)
            )));
        }
        if !value.is_finite() {
            return Err(CaeError::contract(format!(
                "provider {} returned nonfinite sensitivity value",
                repr_str(&pid)
            )));
        }
        for (name, v) in values.iter() {
            let block = grads.get(name).cloned().unwrap_or_default();
            if block.shape() != v.shape() || block.iter().any(|x| !x.is_finite()) {
                return Err(CaeError::contract(format!(
                    "provider {} returned invalid derivative {}/{}: expected {}, got {}",
                    repr_str(&pid),
                    repr_str(response),
                    repr_str(name),
                    implexity_optim::numeric::shape_repr(v.shape()),
                    implexity_optim::numeric::shape_repr(block.shape())
                )));
            }
        }
        let mut evidence = Map::new();
        evidence.insert(pid.clone(), Value::Object(diag.clone()));
        let mut diagnostics = Map::new();
        diagnostics.insert("source_provider".into(), Value::String(pid));
        diagnostics.insert("source_diagnostics".into(), Value::Object(diag));
        diagnostics.insert("orchestrationPlan".into(), self.plan.as_dict());
        diagnostics
            .insert("physical_certification".into(), Value::Object(self.certification(problem, &evidence)?));
        Ok(DesignSensitivity { value, gradients: grads, diagnostics })
    }

    #[allow(clippy::too_many_lines)]
    fn sensitivities_named(
        &self,
        problem: Option<&Value>,
        design: &NamedArrays,
        responses: &[String],
    ) -> CaeResult<DesignSensitivities> {
        let design = self.exact_design(design)?;
        let unique: BTreeSet<&String> = responses.iter().collect();
        if responses.is_empty()
            || unique.len() != responses.len()
            || responses.iter().any(|n| self.route_index(n).is_none())
        {
            return Err(CaeError::contract("unknown, empty or duplicate ensemble response request"));
        }
        let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
        for name in responses {
            let index = self.route_index(name).unwrap_or(0);
            let ptr = Arc::as_ptr(&self.providers[index].1).cast::<()>();
            if let Some(g) =
                groups.iter_mut().find(|(i, _)| Arc::as_ptr(&self.providers[*i].1).cast::<()>() == ptr)
            {
                g.1.push(name.clone());
            } else {
                groups.push((index, vec![name.clone()]));
            }
        }
        let mut values = BTreeMap::new();
        let mut gradients = BTreeMap::new();
        let mut diagnostics = Map::new();
        let mut batching = Map::new();
        let layout = DesignLayout::from_values(&design)?;
        for (index, requested) in &groups {
            let provider = self.providers[*index].1.as_ref();
            let pid = self.pid(*index);
            let ops = design_operations(provider);
            let native = ops.is_some_and(|o| {
                o.provides(DesignOp::SensitivitiesDesign) || o.provides(DesignOp::SensitivityDesign)
            });
            let out = if native {
                batching.insert(
                    pid.clone(),
                    Value::Bool(ops.is_some_and(|o| o.provides(DesignOp::SensitivitiesDesign))),
                );
                batch_sensitivities(provider, &self.child_problem(*index)?, &design, requested, 0)?
            } else {
                let mut out = DesignSensitivities::default();
                let mut per = Map::new();
                for n in requested {
                    let row = self.sensitivity_named(problem, &design, n)?;
                    out.responses.insert(n.clone(), row.value);
                    out.gradients.insert(n.clone(), row.gradients);
                    per.insert(n.clone(), Value::Object(row.diagnostics));
                }
                out.diagnostics.insert("responses".into(), Value::Object(per));
                out.diagnostics.insert("shared_state".into(), Value::Bool(false));
                batching.insert(pid.clone(), Value::Bool(false));
                out
            };
            let got_values: Vec<String> = out.responses.keys().cloned().collect();
            let got_grads: Vec<String> = out.gradients.keys().cloned().collect();
            let req: BTreeSet<&String> = requested.iter().collect();
            if got_values.iter().collect::<BTreeSet<_>>() != req
                || got_grads.iter().collect::<BTreeSet<_>>() != req
            {
                let mut sorted_req = requested.clone();
                sorted_req.sort();
                return Err(CaeError::contract(format!(
                    "provider {} batched sensitivities do not exactly match requested responses; values={}, gradients={}, requested={}",
                    repr_str(&pid),
                    str_list_repr(&got_values),
                    str_list_repr(&got_grads),
                    str_list_repr(&sorted_req)
                )));
            }
            for name in requested {
                let v = out.responses[name];
                if !v.is_finite() {
                    return Err(CaeError::contract(format!(
                        "provider {} returned nonfinite/non-scalar value for {}",
                        repr_str(&pid),
                        repr_str(name)
                    )));
                }
                let label = format!("provider {} derivative {name}", repr_str(&pid));
                layout.check_keys(&out.gradients[name], &label)?;
                layout.pack(&out.gradients[name], &label)?;
            }
            for name in requested {
                values.insert(name.clone(), out.responses[name]);
            }
            gradients.extend(out.gradients);
            diagnostics.insert(pid, Value::Object(out.diagnostics));
        }
        let mut merged = Map::new();
        if diagnostics.len() == 1
            && let Some(Value::Object(owner)) = diagnostics.values().next()
        {
            for k in ["field_registration", "design_field_registrations"] {
                if let Some(v) = owner.get(k) {
                    merged.insert(k.into(), v.clone());
                }
            }
        }
        let shared = groups.len() == 1 && batching.values().all(|v| v == &Value::Bool(true));
        let mut diag = Map::new();
        diag.insert("providers".into(), Value::Object(diagnostics.clone()));
        diag.insert(
            "physical_certification".into(),
            Value::Object(self.certification(problem, &diagnostics)?),
        );
        diag.insert("shared_state".into(), Value::Bool(shared));
        diag.insert("batched_providers".into(), Value::Object(batching));
        diag.insert("orchestrationPlan".into(), self.plan.as_dict());
        for (k, v) in merged {
            diag.insert(k, v);
        }
        Ok(DesignSensitivities { responses: values, gradients, diagnostics: diag })
    }

    fn lifecycle_config(&self) -> CaeResult<OptimizerLifecycleConfig> {
        let accept = self
            .providers
            .iter()
            .any(|(_, p)| design_operations(p.as_ref()).is_some_and(|o| o.provides(DesignOp::AcceptDesign)));
        OptimizerLifecycleConfig::new(
            self.plan.active_design_coordinates.clone(),
            "sensitivity_design",
            "evaluate_design",
            None,
            accept.then_some("accept_design"),
            self.strict,
            !self.strict,
        )
    }


    pub fn evaluate_with_context(
        &self,
        design: &NamedArrays,
        result_artifacts: bool,
    ) -> CaeResult<Evaluation> {
        let ctx = Value::Object(self.context.values.clone());
        self.evaluate_named(Some(&ctx), design, result_artifacts)
    }

    fn cached(
        &self,
        design: &NamedArrays,
        operating_point: usize,
        owner: &str,
    ) -> CaeResult<ExecutionOutput> {
        let values = self.exact_design(design)?;
        let index = 0;
        let provider = self.providers[index].1.as_ref();
        let problem = self.child_problem(index)?;
        let hook = design_operations(provider)
            .and_then(|o| o.cached_evaluation_design(&problem, &values, operating_point));
        let Some(result) = hook else {
            let mut m = Map::new();
            m.insert("available".into(), Value::Bool(false));
            m.insert("reason".into(), Value::String("cache_hook_missing".into()));
            return Ok(ExecutionOutput::Json(m));
        };
        match result? {
            CachedEvaluation::Available(e) => {
                if e.provider != owner {
                    return Err(CaeError::contract("cached evaluation result owner mismatch"));
                }
                Ok(ExecutionOutput::Cached(CachedEvaluation::Available(e)))
            }
            CachedEvaluation::Unavailable(m) => {
                if m.get("available") == Some(&Value::Bool(false))
                    && m.get("reason").and_then(Value::as_str).is_some_and(|r| !r.is_empty())
                {
                    Ok(ExecutionOutput::Json(m))
                } else {
                    Err(CaeError::contract(
                        "cached evaluation must return Evaluation or explicit unavailable",
                    ))
                }
            }
        }
    }
}

impl CaeProvider for IndependentProviderEnsemble {
    fn name(&self) -> &str {
        ENSEMBLE_NAME
    }
    fn provider_id(&self) -> Option<&str> {
        Some(ENSEMBLE_NAME)
    }
    fn implementation(&self) -> &'static str {
        "implexity.cae.orchestration_runtime.IndependentProviderEnsemble"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        self.ensemble_capabilities()
    }
    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Ok(json_problem(match problem {
            Value::Null => Value::Object(Map::new()),
            other => other.clone(),
        }))
    }
    fn preflight(
        &self,
        _problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        self.ensemble_preflight(topology)
    }
    fn evaluate(&self, _problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        self.evaluate_array(topology)
    }
    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        self.sensitivity_array(topology, response)
    }
    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        design_interface::<Self>(name)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl DesignOperations for IndependentProviderEnsemble {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::Evaluate
                | DesignOp::Sensitivity
                | DesignOp::EvaluateDesign
                | DesignOp::EvaluateResultsDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::AcceptDesign
                | DesignOp::OptimizerLifecycle
                | DesignOp::InstallMatchingTimeGuess
                | DesignOp::ExportMatchingTimeGuess
                | DesignOp::PhysicalCertification
        )
    }
    fn evaluate_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        _op: usize,
    ) -> CaeResult<Evaluation> {
        self.evaluate_named(problem_json(problem), design, false)
    }
    fn evaluate_results_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Evaluation> {
        self.evaluate_named(problem_json(problem), design, true)
    }
    fn preflight_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        self.preflight_named(design)
    }
    fn sensitivity_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _op: usize,
    ) -> CaeResult<DesignSensitivity> {
        self.sensitivity_named(problem_json(problem), design, response)
    }
    fn sensitivities_design(
        &self,
        problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _op: usize,
    ) -> CaeResult<DesignSensitivities> {
        self.sensitivities_named(problem_json(problem), design, responses)
    }
    fn accept_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Option<Map<String, Value>>> {
        self.accept(design).map(Some)
    }
    fn optimizer_lifecycle(&self, _problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        self.lifecycle_config().map(LifecycleDeclaration::Typed)
    }
    fn install_matching_time_guess(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        guess: &MatchingTimeNewtonGuess,
    ) -> CaeResult<Map<String, Value>> {
        self.install_guess(design, guess)
    }
    fn export_matching_time_guess(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        require_accepted: bool,
    ) -> CaeResult<MatchingTimeNewtonGuess> {
        self.export_guess(design, require_accepted)
    }
    fn physical_certification(&self, problem: &ProviderProblem) -> CaeResult<Map<String, Value>> {
        self.certification(problem_json(problem), &Map::new())
    }
}

#[derive(Debug, Clone)]
pub struct OrchestrationRuntime<'r> {
    registry: &'r AddInRegistry,
}

fn entries_of(
    registry: &AddInRegistry,
) -> (BTreeMap<String, Arc<RegisteredAddIn>>, implexity_core::orchestration::RegistryBindingToken) {
    let snapshot = registry.snapshot();
    let entries = snapshot.entries.iter().map(|e| (e.contract.addin_id.clone(), Arc::clone(e))).collect();
    (entries, snapshot.token)
}

fn adapter_provider(entry: &RegisteredAddIn) -> Option<Arc<dyn CaeProvider>> {
    entry.adapter.as_ref().and_then(|a| a.provider())
}

fn named_values(active: &[String], design: DesignArg<'_>) -> CaeResult<NamedArrays> {
    let keys = design.keys();
    let (missing, extra) = missing_extra(active, &keys);
    if !missing.is_empty() || !extra.is_empty() {
        return Err(CaeError::contract(format!(
            "execution requires exact named design keys; missing={}, extra={}",
            str_list_repr(&missing),
            str_list_repr(&extra)
        )));
    }
    active.iter().map(|n| Ok((n.clone(), design.array(n)?))).collect()
}

impl<'r> OrchestrationRuntime<'r> {
    #[must_use]
    pub fn new(registry: &'r AddInRegistry) -> Self {
        Self { registry }
    }


    pub fn cached_evaluation_design(
        &self,
        plan: &Arc<OrchestrationPlan>,
        context: &ExecutionContext,
        design: DesignArg<'_>,
        operating_point: usize,
    ) -> CaeResult<ExecutionOutput> {
        if plan.status != PlanStatus::Ready {
            return Err(CaeError::contract("cached evaluation requires a ready plan"));
        }
        let (entries, token) = entries_of(self.registry);
        plan.require_current_binding(Some(&token))?;
        if plan.selected_addins.iter().any(|a| !entries.contains_key(a)) {
            return Err(CaeError::contract("cached evaluation selected provider unavailable"));
        }
        let owners = plan_owner_ids(plan);
        let unsupported = || {
            let mut m = Map::new();
            m.insert("available".into(), Value::Bool(false));
            m.insert("reason".into(), Value::String("unsupported_cache_composition".into()));
            Ok(ExecutionOutput::Json(m))
        };
        if owners.len() != 1 || plan.selected_addins != owners || !plan.coupling_edges.is_empty() {
            return unsupported();
        }
        let entry = &entries[&owners[0]];
        if execution_kind(entry)? != ExecutionKind::Provider {
            return unsupported();
        }
        let Some(provider) = adapter_provider(entry) else {
            return Err(CaeError::contract("cached evaluation owner identity mismatch"));
        };
        if provider_id_of(provider.as_ref()) != owners[0] {
            return Err(CaeError::contract("cached evaluation owner identity mismatch"));
        }
        let ensemble = IndependentProviderEnsemble::new(
            Arc::clone(plan),
            vec![(owners[0].clone(), provider)],
            context.clone(),
            !is_compat(entry),
        )?;
        let values: NamedArrays = match design {
            DesignArg::Named(n) => n.clone(),
            DesignArg::Json(Value::Object(m)) => {
                m.keys().map(|k| Ok((k.clone(), design.array(k)?))).collect::<CaeResult<_>>()?
            }
            DesignArg::Json(_) => return Err(CaeError::contract("named design must be a nonempty mapping")),
        };
        ensemble.cached(&values, operating_point, &owners[0])
    }


    pub fn validate_matching_time_guess_lifecycle(
        &self,
        plan: &OrchestrationPlan,
        consume: bool,
        produce: bool,
    ) -> CaeResult<JsonMap> {
        if plan.status != PlanStatus::Ready {
            return Err(CaeError::contract(format!(
                "orchestration plan is not executable: {}",
                plan.status.as_str()
            )));
        }
        let (entries, token) = entries_of(self.registry);
        plan.require_current_binding(Some(&token))?;
        let missing: Vec<String> =
            plan.selected_addins.iter().filter(|a| !entries.contains_key(*a)).cloned().collect();
        if !missing.is_empty() {
            return Err(CaeError::contract(format!(
                "selected providers are unavailable at execution (add-ins): {}",
                str_list_repr(&missing)
            )));
        }
        let owners = plan_owner_ids(plan);
        if owners.len() != 1 {
            return Err(CaeError::contract(format!(
                "matching-time guess lifecycle requires exactly one selected response owner; got {}",
                str_list_repr(&owners)
            )));
        }
        let owner = &owners[0];
        if !plan.selected_addins.contains(owner) {
            return Err(CaeError::contract(format!(
                "matching-time guess response provider {} is not selected",
                repr_str(owner)
            )));
        }
        let entry = &entries[owner];
        if is_compat(entry) || entry.contract.execution_kind == Some(ExecutionKind::Legacy) {
            return Err(CaeError::contract("matching-time guess lifecycle requires a strict v2 state owner"));
        }
        let Some(provider) = adapter_provider(entry) else {
            return Err(CaeError::contract(format!(
                "matching-time guess state owner {} is unavailable",
                repr_str(owner)
            )));
        };
        let declared = provider_id_of(provider.as_ref());
        if &declared != owner {
            return Err(CaeError::contract(format!(
                "matching-time guess state owner identity mismatch: selected {}, got {}",
                repr_str(owner),
                repr_str(&declared)
            )));
        }
        let mut operations = Vec::new();
        if consume {
            operations.push(DesignOp::InstallMatchingTimeGuess);
        }
        if produce {
            operations.push(DesignOp::ExportMatchingTimeGuess);
        }
        for op in operations {
            require_operation(entry, op.name())?;
            if !design_operations(provider.as_ref()).is_some_and(|o| o.provides(op)) {
                return Err(CaeError::contract(format!(
                    "provider {} has no {} lifecycle operation",
                    repr_str(owner),
                    op.name()
                )));
            }
        }
        let mut out = Map::new();
        out.insert("schema".into(), Value::String("implexity-matching-time-lifecycle-validation/1".into()));
        out.insert("lifecycle_owner".into(), Value::String(owner.clone()));
        out.insert("consume".into(), Value::Bool(consume));
        out.insert("produce".into(), Value::Bool(produce));
        out.insert("mutation_performed".into(), Value::Bool(false));
        Ok(out)
    }


    pub fn execute(
        &self,
        plan: &Arc<OrchestrationPlan>,
        context: &ExecutionContext,
        request: ExecuteRequest<'_>,
    ) -> CaeResult<ExecutionOutput> {
        plan.require_current_binding(Some(&self.registry.binding_token()))?;
        let _scope =
            crate::provider_job_authority::selected_provider_computation_effort_scope(plan, self.registry)?;
        self.execute_inner(plan, context, request)
    }

    #[allow(clippy::too_many_lines)]
    fn execute_inner(
        &self,
        plan: &Arc<OrchestrationPlan>,
        context: &ExecutionContext,
        mut request: ExecuteRequest<'_>,
    ) -> CaeResult<ExecutionOutput> {
        let operation = request.operation;
        if plan.status != PlanStatus::Ready {
            let reasons = if !plan.blocked_reasons.is_empty() {
                &plan.blocked_reasons
            } else if !plan.missing_physics.is_empty() {
                &plan.missing_physics
            } else {
                &plan.missing_authoring
            };
            return Err(CaeError::contract(format!(
                "orchestration plan is not executable: {}; {}",
                plan.status.as_str(),
                str_list_repr(reasons)
            )));
        }
        if request.topology.is_some() && request.design.is_some() {
            return Err(CaeError::contract("execution cannot receive both a bare array and named design"));
        }
        let (entries, token) = entries_of(self.registry);
        plan.require_current_binding(Some(&token))?;
        let ctx = context.clone();
        let missing: Vec<String> =
            plan.selected_addins.iter().filter(|a| !entries.contains_key(*a)).cloned().collect();
        if !missing.is_empty() {
            return Err(CaeError::contract(format!(
                "selected providers are unavailable at execution (add-ins): {}",
                str_list_repr(&missing)
            )));
        }
        let selected: Vec<Arc<RegisteredAddIn>> =
            plan.selected_addins.iter().map(|a| Arc::clone(&entries[a])).collect();
        let authoritative = [
            "sensitivity",
            "sensitivities",
            "optimize",
            "accept_design",
            "install_matching_time_guess",
            "export_matching_time_guess",
        ];
        let compatibility: Vec<String> = selected
            .iter()
            .filter(|e| is_compat(e) || e.contract.execution_kind == Some(ExecutionKind::Legacy))
            .map(|e| e.contract.addin_id.clone())
            .collect();
        let strict = compatibility.is_empty();
        let active = plan.active_design_coordinates.clone();
        if strict && active.is_empty() {
            return Err(CaeError::contract(
                "strict v2 execution requires nonempty active_design_coordinates",
            ));
        }
        if authoritative.contains(&operation) && !compatibility.is_empty() {
            let mut sorted = compatibility.clone();
            sorted.sort();
            return Err(CaeError::contract(format!(
                "operation {} requires strict v2 execution evidence; compatibility add-ins are evaluation-only and cannot establish exactness: {}",
                repr_str(operation),
                str_list_repr(&sorted)
            )));
        }
        if matches!(operation, "install_matching_time_guess" | "export_matching_time_guess") {
            let owners = plan_owner_ids(plan);
            if owners.len() != 1 {
                return Err(CaeError::contract(format!(
                    "matching-time guess lifecycle requires exactly one selected response owner; got {}",
                    str_list_repr(&owners)
                )));
            }
            let owner = &owners[0];
            if !entries.contains_key(owner) || !plan.selected_addins.contains(owner) {
                return Err(CaeError::contract(format!(
                    "matching-time guess response provider {} is not selected",
                    repr_str(owner)
                )));
            }
            require_operation(&entries[owner], operation)?;
        } else {
            for entry in &selected {
                require_operation(entry, operation)?;
            }
        }
        if operation == "accept_design" {
            let owners = plan_owner_ids(plan);
            if owners.is_empty() {
                return Err(CaeError::contract(
                    "accepted-design forwarding has no selected response provider",
                ));
            }
            let unselected: Vec<String> =
                owners.iter().filter(|a| !plan.selected_addins.contains(*a)).cloned().collect();
            if !unselected.is_empty() {
                return Err(CaeError::contract(format!(
                    "accepted-design response providers are not selected: {}",
                    str_list_repr(&unselected)
                )));
            }
        }
        if plan.runtime_route.starts_with("mature_job:") {
            return Self::execute_mature_job(plan, &entries, &ctx, operation, &request, &compatibility);
        }
        let mut kinds: BTreeSet<&'static str> = BTreeSet::new();
        let mut kind_values: Vec<ExecutionKind> = Vec::new();
        for entry in &selected {
            let k = execution_kind(entry)?;
            if kinds.insert(k.as_str()) {
                kind_values.push(k);
            }
        }
        let numerical: Vec<ExecutionKind> = kind_values
            .iter()
            .copied()
            .filter(|k| matches!(k, ExecutionKind::Residual | ExecutionKind::Algebraic))
            .collect();
        if numerical.len() > 1 {
            return Err(CaeError::contract(
                "unsupported mixed residual/algebraic composition; select an explicitly declared bridge that owns one numerical execution kind",
            ));
        }
        if !numerical.is_empty() && kind_values.len() > numerical.len() {
            let names: Vec<String> = kinds.iter().map(|s| (*s).to_string()).collect();
            return Err(CaeError::contract(format!(
                "unsupported mixed numerical/operation composition {}; an explicit composer is required",
                str_list_repr(&names)
            )));
        }
        let native_ops =
            ["preflight", "evaluate", "sensitivity", "sensitivities", "optimize", "preflight_design"];
        if native_ops.contains(&operation)
            && let Some(kind) = numerical.first().copied()
        {
            ensure_numerical_factories(&selected, kind)?;
            let provider: Arc<dyn CaeProvider> = if kind == ExecutionKind::Residual {
                Arc::new(crate::residual_runtime::CompositeOrchestratedProvider::new(
                    Arc::clone(plan),
                    entries.clone(),
                    ctx.clone(),
                )?)
            } else {
                Arc::new(crate::algebraic_runtime::AlgebraicOrchestratedProvider::new(
                    Arc::clone(plan),
                    entries.clone(),
                    ctx.clone(),
                )?)
            };
            return Self::execute_numerical(
                provider.as_ref(),
                plan,
                &ctx,
                &active,
                strict,
                &compatibility,
                &mut request,
            );
        }
        let provider_entries: Vec<&Arc<RegisteredAddIn>> = selected
            .iter()
            .filter(|e| execution_kind(e).is_ok_and(|k| k == ExecutionKind::Provider))
            .collect();
        let mut providers: Vec<(String, Arc<dyn CaeProvider>)> = Vec::new();
        for entry in &provider_entries {
            if let Some(p) = adapter_provider(entry) {
                providers.push((entry.contract.addin_id.clone(), p));
            }
        }
        let provider_ops = [
            "preflight",
            "evaluate",
            "sensitivity",
            "sensitivities",
            "optimize",
            "preflight_design",
            "accept_design",
            "install_matching_time_guess",
            "export_matching_time_guess",
        ];
        if provider_ops.contains(&operation)
            && !providers.is_empty()
            && providers.len() == plan.selected_addins.len()
            && plan.coupling_edges.is_empty()
        {
            let ens_strict = provider_entries.iter().all(|e| !is_compat(e));
            let ensemble =
                IndependentProviderEnsemble::new(Arc::clone(plan), providers, ctx.clone(), ens_strict)?;
            return Self::execute_ensemble(
                &ensemble,
                &ctx,
                &active,
                strict,
                &compatibility,
                plan,
                &mut request,
            );
        }
        if operation == "accept_design" {
            return Err(CaeError::contract(
                "accepted-design forwarding requires an unambiguous uncoupled selected-provider route",
            ));
        }
        if !numerical.is_empty() || !provider_entries.is_empty() {
            return Err(CaeError::contract(format!(
                "operation {} is not implemented by the selected numerical execution contract",
                repr_str(operation)
            )));
        }
        for entry in &selected {
            ensure_direct_invokable(entry, operation, false)?;
        }
        Self::execute_operation_graph(plan, &entries, &selected, &ctx, operation, &compatibility)
    }

    fn execute_mature_job(
        plan: &Arc<OrchestrationPlan>,
        entries: &BTreeMap<String, Arc<RegisteredAddIn>>,
        ctx: &ExecutionContext,
        operation: &str,
        request: &ExecuteRequest<'_>,
        compatibility: &[String],
    ) -> CaeResult<ExecutionOutput> {
        if operation == "accept_design" {
            return Err(CaeError::contract(
                "accepted-design forwarding is not defined for a mature-job lifecycle owner",
            ));
        }
        if request.design.is_some()
            && ["evaluate", "sensitivity", "sensitivities", "optimize", "preflight_design"]
                .contains(&operation)
        {
            return Err(CaeError::contract(
                "mature-job orchestration owns its authoritative design inside the implicit model; detached design arrays are not accepted",
            ));
        }
        let roots: Vec<&Arc<RegisteredAddIn>> = plan
            .selected_addins
            .iter()
            .map(|a| &entries[a])
            .filter(|e| e.contract.runtime_route == RuntimeRoute::MatureJob)
            .collect();
        if roots.len() != 1 {
            return Err(CaeError::contract("mature-job orchestration requires exactly one lifecycle owner"));
        }
        let extensions: Vec<&Arc<RegisteredAddIn>> = plan
            .selected_addins
            .iter()
            .map(|a| &entries[a])
            .filter(|e| e.contract.runtime_route == RuntimeRoute::JobExtension)
            .collect();
        ensure_direct_invokable(roots[0], operation, false)?;
        for extension in &extensions {
            ensure_direct_invokable(extension, operation, true)?;
        }
        let mut result = validate_operation_output(
            roots[0],
            invoke(roots[0], operation, &ctx.values, plan, None)?,
            operation,
        )?;
        for aid in &plan.selected_addins {
            let e = &entries[aid];
            if e.contract.runtime_route == RuntimeRoute::JobExtension {
                let ext = validate_operation_output(
                    e,
                    invoke(e, operation, &ctx.values, plan, Some(&result))?,
                    operation,
                )?;
                if !ext.is_empty() {
                    let slot = result.entry("extensions").or_insert_with(|| Value::Object(Map::new()));
                    if let Value::Object(m) = slot {
                        m.insert(aid.clone(), Value::Object(ext));
                    } else {
                        return Err(CaeError::contract(
                            "mature-job result 'extensions' must be a mapping to receive extension outputs",
                        ));
                    }
                }
            }
        }
        let out = ExecutionOutput::Json(result);
        if operation == "evaluate" { mark_compatibility_evaluation(out, compatibility) } else { Ok(out) }
    }

    fn execute_numerical(
        provider: &dyn CaeProvider,
        plan: &OrchestrationPlan,
        ctx: &ExecutionContext,
        active: &[String],
        strict: bool,
        compatibility: &[String],
        request: &mut ExecuteRequest<'_>,
    ) -> CaeResult<ExecutionOutput> {
        let operation = request.operation;
        let problem = json_problem(Value::Object(ctx.values.clone()));
        let ops = design_operations(provider)
            .ok_or_else(|| CaeError::contract("numerical provider has no hooks"))?;
        if operation == "preflight" {
            if strict && request.topology.is_some() {
                return Err(CaeError::contract("strict preflight does not accept a bare design array"));
            }
            let topo = request.topology.map(|t| t.bare()).transpose()?;
            return Ok(ExecutionOutput::Json(provider.preflight(&problem, topo.as_ref())?));
        }
        let Some(native) = request.design.or(request.topology) else {
            return Err(CaeError::contract(format!("{operation} requires an authoritative design")));
        };
        let is_named = native.is_named();
        if strict && !is_named {
            return Err(CaeError::contract(
                "strict v2 execution requires a named design mapping; bare arrays are compatibility-only",
            ));
        }
        let values = if is_named {
            named_values(active, native)?
        } else {
            NamedArrays::single(TOPOLOGY_COORDINATE, native.bare()?)
        };
        match operation {
            "preflight_design" => Ok(ExecutionOutput::Json(ops.preflight_design(&problem, &values)?)),
            "evaluate" => {
                let e = if request.result_artifacts && ops.provides(DesignOp::EvaluateResultsDesign) {
                    ops.evaluate_results_design(&problem, &values)?
                } else {
                    ops.evaluate_design(&problem, &values, 0)?
                };
                mark_compatibility_evaluation(ExecutionOutput::Evaluation(e), compatibility)
            }
            "sensitivities" => {
                let names = response_list(ctx.get("responses"));
                Ok(ExecutionOutput::Sensitivities(ops.sensitivities_design(&problem, &values, &names, 0)?))
            }
            "sensitivity" => {
                let response = crate::pyval::text_or(ctx.get("response"), "");
                if response.is_empty() {
                    return Err(CaeError::contract("sensitivity operation requires context.response"));
                }
                if is_named {
                    Ok(ExecutionOutput::DesignSensitivity(
                        ops.sensitivity_design(&problem, &values, &response, 0)?,
                    ))
                } else {
                    let topo = values.get(TOPOLOGY_COORDINATE).cloned().unwrap_or_default();
                    Ok(ExecutionOutput::Sensitivity(provider.sensitivity(&problem, &topo, &response)?))
                }
            }
            _ => {
                let specs = match request.responses.take() {
                    Some(s) if !s.is_empty() => s,
                    _ => default_specs(plan)?,
                };
                let callback = request.callback.take();
                if is_named {
                    let config = coordinate_settings(request.settings, strict)?;
                    let names = values.names();
                    let run = optimise_design(
                        provider,
                        &problem,
                        &values.to_wire(),
                        &specs,
                        &config,
                        Some(&names),
                        None,
                        callback,
                    )?;
                    Ok(ExecutionOutput::Run(run))
                } else {
                    let settings = LegacySingleArrayOptimizationSettings::from_dict(request.settings)?;
                    let topo = values.get(TOPOLOGY_COORDINATE).cloned().unwrap_or_default();
                    Ok(ExecutionOutput::Run(optimise(
                        provider, &problem, &topo, &specs, &settings, callback,
                    )?))
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute_ensemble(
        ensemble: &IndependentProviderEnsemble,
        ctx: &ExecutionContext,
        active: &[String],
        strict: bool,
        compatibility: &[String],
        plan: &OrchestrationPlan,
        request: &mut ExecuteRequest<'_>,
    ) -> CaeResult<ExecutionOutput> {
        let operation = request.operation;
        let problem = json_problem(Value::Object(ctx.values.clone()));
        if operation == "preflight" {
            if strict && request.topology.is_some() {
                return Err(CaeError::contract("strict preflight does not accept a bare design array"));
            }
            let topo = request.topology.map(|t| t.bare()).transpose()?;
            return Ok(ExecutionOutput::Json(ensemble.ensemble_preflight(topo.as_ref())?));
        }
        let Some(native) = request.design.or(request.topology) else {
            return Err(CaeError::contract(format!("{operation} requires an authoritative design")));
        };
        if strict && !native.is_named() {
            return Err(CaeError::contract(
                "strict v2 execution requires a named design mapping; bare arrays are compatibility-only",
            ));
        }
        if native.is_named() {
            let design = named_values(active, native)?;
            return match operation {
                "preflight_design" => Ok(ExecutionOutput::Json(ensemble.preflight_named(&design)?)),
                "sensitivities" => {
                    let names = response_list(ctx.get("responses"));
                    Ok(ExecutionOutput::Sensitivities(ensemble.sensitivities_named(
                        problem_json(&problem),
                        &design,
                        &names,
                    )?))
                }
                "accept_design" => Ok(ExecutionOutput::Json(ensemble.accept(&design)?)),
                "install_matching_time_guess" => {
                    let Some(guess) = request.matching_time_guess else {
                        return Err(CaeError::contract(
                            "matching-time guess installation requires a typed guess",
                        ));
                    };
                    Ok(ExecutionOutput::Json(ensemble.install_guess(&design, guess)?))
                }
                "export_matching_time_guess" => {
                    Ok(ExecutionOutput::Guess(ensemble.export_guess(&design, request.require_accepted)?))
                }
                "evaluate" => {
                    let e =
                        ensemble.evaluate_named(problem_json(&problem), &design, request.result_artifacts)?;
                    mark_compatibility_evaluation(ExecutionOutput::Evaluation(e), compatibility)
                }
                "sensitivity" => {
                    let response = crate::pyval::text_or(ctx.get("response"), "");
                    if response.is_empty() {
                        return Err(CaeError::contract("sensitivity operation requires context.response"));
                    }
                    Ok(ExecutionOutput::DesignSensitivity(ensemble.sensitivity_named(
                        problem_json(&problem),
                        &design,
                        &response,
                    )?))
                }
                _ => {
                    let specs = match request.responses.take() {
                        Some(s) if !s.is_empty() => s,
                        _ => default_specs(plan)?,
                    };
                    let config = coordinate_settings(request.settings, strict)?;
                    let names = design.names();
                    let run = optimise_design(
                        ensemble,
                        &problem,
                        &design.to_wire(),
                        &specs,
                        &config,
                        Some(&names),
                        None,
                        request.callback.take(),
                    )?;
                    Ok(ExecutionOutput::Run(run))
                }
            };
        }
        let topo = native.bare()?;
        match operation {
            "evaluate" => mark_compatibility_evaluation(
                ExecutionOutput::Evaluation(ensemble.evaluate_array(&topo)?),
                compatibility,
            ),
            "sensitivity" => {
                let response = crate::pyval::text_or(ctx.get("response"), "");
                if response.is_empty() {
                    return Err(CaeError::contract("sensitivity operation requires context.response"));
                }
                Ok(ExecutionOutput::Sensitivity(ensemble.sensitivity_array(&topo, &response)?))
            }
            "optimize" => {
                let specs = match request.responses.take() {
                    Some(s) if !s.is_empty() => s,
                    _ => default_specs(plan)?,
                };
                let pre = ensemble.ensemble_preflight(Some(&topo))?;
                if !crate::pyval::truthy(pre.get("ok")) {
                    return Err(CaeError::contract(format!(
                        "orchestrated provider preflight failed: {}",
                        crate::pyval::py_repr_value(pre.get("issues").unwrap_or(&Value::Null))
                    )));
                }
                let settings = LegacySingleArrayOptimizationSettings::from_dict(request.settings)?;
                Ok(ExecutionOutput::Run(optimise(
                    ensemble,
                    &problem,
                    &topo,
                    &specs,
                    &settings,
                    request.callback.take(),
                )?))
            }
            other => Err(CaeError::contract(format!(
                "operation {} requires a named design mapping",
                repr_str(other)
            ))),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute_operation_graph(
        plan: &OrchestrationPlan,
        entries: &BTreeMap<String, Arc<RegisteredAddIn>>,
        selected: &[Arc<RegisteredAddIn>],
        ctx: &ExecutionContext,
        operation: &str,
        compatibility: &[String],
    ) -> CaeResult<ExecutionOutput> {
        let mut ownership: Vec<(RouteKey, Vec<String>)> = Vec::new();
        for entry in selected {
            let compat = is_legacy_or_compat(entry);
            for port in &entry.contract.provides {
                let key = port_route_key(port, compat);
                if let Some(row) = ownership.iter_mut().find(|(k, _)| *k == key) {
                    row.1.push(entry.contract.addin_id.clone());
                } else {
                    ownership.push((key, vec![entry.contract.addin_id.clone()]));
                }
            }
        }
        for (key, owners) in &ownership {
            if owners.len() < 2 {
                continue;
            }
            let owner_set: BTreeSet<&String> = owners.iter().collect();
            let mut aggregators = 0usize;
            for target in selected {
                for consume in &target.contract.consumes {
                    let tail = (
                        consume.quantity.clone(),
                        consume.unit.clone(),
                        consume.domain.clone(),
                        consume.interface.clone(),
                        consume.temporal.clone(),
                        consume.conserved,
                        consume.cardinality.clone(),
                        consume.aggregation.clone(),
                    );
                    let key_tail = (
                        key.1.clone(),
                        key.2.clone(),
                        key.3.clone(),
                        key.4.clone(),
                        key.5.clone(),
                        key.6,
                        key.7.clone(),
                        key.8.clone(),
                    );
                    if tail == key_tail {
                        let incoming: BTreeSet<&String> = plan
                            .coupling_edges
                            .iter()
                            .filter(|e| {
                                e.target == target.contract.addin_id
                                    && owner_set.contains(&e.source)
                                    && e.quantity == consume.quantity
                            })
                            .map(|e| &e.source)
                            .collect();
                        if incoming == owner_set && consume.aggregation != "single" {
                            aggregators += 1;
                        }
                    }
                }
            }
            if aggregators == 0 {
                let mut sorted = owners.clone();
                sorted.sort();
                return Err(CaeError::contract(format!(
                    "provided-port ownership collision for {}: {}; an explicit consumer aggregator is required",
                    route_key_repr(key),
                    str_list_repr(&sorted)
                )));
            }
        }
        let mut shared = ctx.values.clone();
        let mut outputs = Map::new();
        let mut flat_owners: BTreeMap<String, String> = BTreeMap::new();
        let mut port_outputs: BTreeMap<String, JsonMap> = BTreeMap::new();
        let mut port_outputs_order: Vec<String> = Vec::new();
        let mut done: BTreeSet<String> = BTreeSet::new();
        while done.len() < plan.selected_addins.len() {
            let mut progressed = false;
            for aid in &plan.selected_addins {
                if done.contains(aid) {
                    continue;
                }
                let deps: BTreeSet<&String> =
                    plan.coupling_edges.iter().filter(|e| &e.target == aid).map(|e| &e.source).collect();
                if !deps.iter().all(|d| done.contains(*d)) {
                    continue;
                }
                let entry = &entries[aid];
                let mut inputs = Map::new();
                let mut input_meta = Map::new();
                let mut grouped: Vec<Grouped<'_>> = Vec::new();
                for edge in plan.coupling_edges.iter().filter(|e| &e.target == aid) {
                    let (source_port, target_port) = edge_ports(edge, &entries[&edge.source], entry)?;
                    let source_token = entry_port_token(&entries[&edge.source], source_port);
                    let target_token = entry_port_token(entry, target_port);
                    let Some(value) = port_outputs.get(&edge.source).and_then(|m| m.get(&source_token))
                    else {
                        return Err(CaeError::contract(format!(
                            "{} did not produce declared port {} for {aid}",
                            edge.source,
                            repr_str(&source_token)
                        )));
                    };
                    if let Some(g) = grouped.iter_mut().find(|(t, _, _)| *t == target_token) {
                        g.2.push((edge.source.clone(), value.clone()));
                    } else {
                        grouped.push((target_token, target_port, vec![(edge.source.clone(), value.clone())]));
                    }
                }
                for (token, target_port, rows) in grouped {
                    if inputs.contains_key(&token) {
                        return Err(CaeError::contract(format!(
                            "{aid}: ambiguous consumed port token {}",
                            repr_str(&token)
                        )));
                    }
                    let values: Vec<Value> = rows.iter().map(|r| r.1.clone()).collect();
                    inputs.insert(
                        token.clone(),
                        aggregate(&values, &target_port.aggregation, &format!("{aid}:{token}"))?,
                    );
                    let mut meta = Map::new();
                    meta.insert("port".into(), port_key_value(target_port));
                    meta.insert(
                        "sources".into(),
                        Value::Array(rows.iter().map(|r| Value::String(r.0.clone())).collect()),
                    );
                    meta.insert("aggregation".into(), Value::String(target_port.aggregation.clone()));
                    input_meta.insert(token, Value::Object(meta));
                }
                let mut call_context = shared.clone();
                call_context.insert("inputs".into(), Value::Object(inputs));
                call_context.insert("input_ports".into(), Value::Object(input_meta));
                call_context.insert("outputs_by_addin".into(), Value::Object(outputs.clone()));
                let out = validate_operation_output(
                    entry,
                    invoke(entry, operation, &call_context, plan, None)?,
                    operation,
                )?;
                outputs.insert(aid.clone(), Value::Object(out.clone()));
                port_outputs.insert(aid.clone(), out.clone());
                port_outputs_order.push(aid.clone());
                if is_legacy_or_compat(entry) {
                    for (key, value) in &out {
                        if let Some(owner) = flat_owners.get(key) {
                            return Err(CaeError::contract(format!(
                                "legacy operation output {} collides between {} and {}",
                                repr_str(key),
                                repr_str(owner),
                                repr_str(aid)
                            )));
                        }
                        if ctx.values.contains_key(key) {
                            return Err(CaeError::contract(format!(
                                "legacy operation output {} collides with input context",
                                repr_str(key)
                            )));
                        }
                        flat_owners.insert(key.clone(), aid.clone());
                        shared.insert(key.clone(), value.clone());
                    }
                }
                shared.insert("outputs_by_addin".into(), Value::Object(outputs.clone()));
                done.insert(aid.clone());
                progressed = true;
            }
            if !progressed {
                return Err(CaeError::contract(
                    "cyclic add-in plan has no common residual/adjoint implementation",
                ));
            }
        }
        let mut result = Map::new();
        result.insert("schema".into(), Value::String("implexity-orchestration-execution/1".into()));
        result.insert("outputs".into(), Value::Object(outputs));
        result.insert("context".into(), Value::Object(shared));
        result.insert(
            "port_outputs".into(),
            Value::Object(
                port_outputs_order
                    .iter()
                    .map(|a| (a.clone(), Value::Object(port_outputs[a].clone())))
                    .collect(),
            ),
        );
        result.insert("plan".into(), plan.as_dict());
        let out = ExecutionOutput::Json(result);
        if operation == "evaluate" { mark_compatibility_evaluation(out, compatibility) } else { Ok(out) }
    }
}

type Grouped<'p> = (String, &'p PortSpec, Vec<(String, Value)>);

#[must_use]
pub fn port_key_value(port: &PortSpec) -> Value {
    Value::Array(vec![
        Value::String(port.quantity.clone()),
        Value::String(port.unit.clone()),
        Value::String(port.domain.clone()),
        port.interface.clone().map_or(Value::Null, Value::String),
        Value::String(port.temporal.clone()),
        Value::Bool(port.conserved),
        Value::String(port.cardinality.clone()),
        Value::String(port.aggregation.clone()),
        Value::String(port.port_id.clone()),
    ])
}

fn response_list(raw: Option<&Value>) -> Vec<String> {
    match raw {
        Some(Value::Array(items)) => items.iter().map(crate::pyval::py_str).collect(),
        Some(Value::String(s)) => s.chars().map(|c| c.to_string()).collect(),
        _ => Vec::new(),
    }
}

#[must_use]
pub fn provider_problem_raw(context: &JsonMap, provider_id: &str) -> Value {
    context
        .get("provider_problems")
        .and_then(Value::as_object)
        .and_then(|m| m.get(provider_id))
        .or_else(|| context.get("problem"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

#[must_use]
pub fn run_value(run: &DirectGradientRun) -> Value {
    let mut record = run.record.clone();
    if !record.contains_key("design") {
        record.insert("design".into(), run.design.to_wire());
    }
    if let Some(t) = run.design.get(TOPOLOGY_COORDINATE)
        && record.contains_key("topology")
    {
        record.insert("topology".into(), array_to_value(t));
    }
    Value::Object(record)
}
