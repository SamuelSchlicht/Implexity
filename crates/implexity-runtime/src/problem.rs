// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_core::contracts::{CaeProvider, ProviderProblem, ResponseSpec};
use implexity_core::orchestration::ExecutionKind;
use implexity_core::py_repr::repr_str;
use implexity_core::registries::Registries;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::numeric::str_list_repr;
use implexity_optim::provider_ops::design_operations;
use serde_json::{Map, Value};

pub const SCHEMA: &str = "implexity-implicit-cae/1";

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message.into())
}

#[derive(Clone)]
pub struct PreparedProblem {
    pub name: String,
    pub provider_name: String,
    pub provider: Arc<dyn CaeProvider>,
    pub problem: ProviderProblem,
    pub problem_raw: Value,
    pub design_coordinates: Vec<String>,
    pub inactive_design_coordinates: Vec<String>,
    pub design_coordinate_selection: Value,
    pub default_design_bindings: Vec<Value>,
    pub responses: Vec<ResponseSpec>,
    pub metadata: Map<String, Value>,
}

impl std::fmt::Debug for PreparedProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedProblem")
            .field("name", &self.name)
            .field("provider_name", &self.provider_name)
            .field("design_coordinates", &self.design_coordinates)
            .finish_non_exhaustive()
    }
}

impl PreparedProblem {
    #[must_use]
    pub fn coordinate_selection(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert(
            "design_coordinates".into(),
            Value::Array(self.design_coordinates.iter().cloned().map(Value::String).collect()),
        );
        out.insert(
            "inactive_design_coordinates".into(),
            Value::Array(self.inactive_design_coordinates.iter().cloned().map(Value::String).collect()),
        );
        out.insert("design_coordinate_selection".into(), self.design_coordinate_selection.clone());
        out
    }
}

fn declared_coordinates(
    registries: &Registries,
    provider: &Arc<dyn CaeProvider>,
    provider_name: &str,
    supported: Vec<String>,
) -> CaeResult<Vec<String>> {
    if supported.is_empty() {
        let entry = registries.addins.get(provider_name)?;
        let c = &entry.contract;
        let ops: std::collections::BTreeSet<&str> =
            c.supported_operations.iter().map(String::as_str).collect();
        let same_provider =
            entry.adapter.as_ref().and_then(|a| a.provider()).is_some_and(|p| Arc::ptr_eq(&p, provider));
        if c.contract_version != 2
            || c.compatibility_mode
            || c.execution_kind != Some(ExecutionKind::Provider)
            || !c.design_inputs.is_empty()
            || ops != ["evaluate", "preflight"].into_iter().collect()
            || c.responses.iter().any(|r| r.design_reachable == Some(true) || r.differentiable == Some(true))
            || !same_provider
        {
            return Err(contract(
                "empty design space requires an owned strict evaluation-only provider contract",
            ));
        }
        return Ok(Vec::new());
    }
    let unique: std::collections::BTreeSet<&String> = supported.iter().collect();
    if supported.iter().any(String::is_empty) || unique.len() != supported.len() {
        return Err(contract(format!(
            "provider {} has no valid design-coordinate declaration",
            repr_str(provider_name)
        )));
    }
    Ok(supported)
}

fn default_binding_metadata(
    provider: &dyn CaeProvider,
    problem: &ProviderProblem,
    caps_name: &str,
    traits: Option<&Value>,
    supported: &[String],
) -> CaeResult<Vec<Value>> {
    let mut raw: Option<Value> = None;
    let mut source = "";
    if let Some(v) = design_operations(provider).and_then(|o| o.default_design_bindings(problem)) {
        raw = Some(v?).filter(|v| !v.is_null());
        source = "provider";
    }
    if raw.is_none()
        && let Some(Value::Object(declaration)) = traits.and_then(|t| t.get("design_variable_default"))
    {
        raw = declaration.get("bindings").cloned().filter(|v| !v.is_null());
        source = "capability";
    }
    let Some(raw) = raw else { return Ok(Vec::new()) };
    let Value::Array(items) = raw else {
        return Err(contract(format!(
            "provider {} default design bindings must be a list",
            repr_str(caps_name)
        )));
    };
    let mut rows = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, value) in items.iter().enumerate() {
        let Value::Object(m) = value else {
            return Err(contract(format!("provider default design binding {index} must be an object")));
        };
        let mut row = m.clone();
        let coordinate = [row.get("coordinate"), row.get("name")]
            .into_iter()
            .flatten()
            .find(|v| crate::pyval::truthy(Some(v)))
            .map_or_else(String::new, |v| crate::pyval::py_str(v).trim().to_string());
        if coordinate.is_empty() || seen.contains(&coordinate) {
            return Err(contract("provider default design bindings require unique nonempty coordinates"));
        }
        if !supported.contains(&coordinate) {
            return Err(contract(format!(
                "provider default binding names undeclared coordinate {}",
                repr_str(&coordinate)
            )));
        }
        row.insert("coordinate".into(), Value::String(coordinate.clone()));
        row.shift_remove("name");
        row.insert("source".into(), Value::String(source.into()));
        rows.push(Value::Object(row));
        seen.push(coordinate);
    }
    Ok(rows)
}

fn first_truthy<'a>(candidates: &[Option<&'a Value>]) -> Option<&'a Value> {
    candidates.iter().flatten().copied().find(|v| crate::pyval::truthy(Some(v)))
}


#[allow(clippy::too_many_lines)]
pub fn normalise(value: &Value, registries: &Registries) -> CaeResult<PreparedProblem> {
    let Some(d) = value.as_object() else {
        return Err(contract("CAE problem must be a JSON object"));
    };
    let physics = match d.get("physics") {
        Some(Value::Object(m)) => m.clone(),
        Some(v) if crate::pyval::truthy(Some(v)) => {
            return Err(contract("CAE problem physics must be a JSON object"));
        }
        _ => Map::new(),
    };
    let provider_name = first_truthy(&[physics.get("provider"), d.get("provider")])
        .map_or_else(String::new, |v| crate::pyval::py_str(v).trim().to_string());
    if provider_name.is_empty() {
        return Err(contract("CAE problem requires physics.provider"));
    }
    let provider = registries.providers.get(&provider_name)?;
    let problem_raw = first_truthy(&[physics.get("problem"), d.get("problem")])
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let problem = provider.normalise_problem(&problem_raw)?;
    let caps = provider.capabilities()?;
    let mut supported =
        declared_coordinates(registries, &provider, &provider_name, caps.design_coordinates())?;
    let ops = design_operations(provider.as_ref());
    if let Some(resolved) = ops.and_then(|o| o.authoring_design_coordinates(&problem)) {
        let resolved = resolved?;
        let unique: std::collections::BTreeSet<&String> = resolved.iter().collect();
        if resolved.is_empty()
            || unique.len() != resolved.len()
            || resolved.iter().any(|c| !supported.contains(c))
        {
            return Err(contract("provider returned invalid authoring coordinate contract"));
        }
        supported = resolved;
    }
    let (raw_coords, source, explicit) = if let Some(v) = d.get("design_coordinates") {
        (v.clone(), "explicit_request", true)
    } else if let Some(v) = problem_raw.as_object().and_then(|m| m.get("active_design_coordinates")) {
        (v.clone(), "explicit_provider_problem", true)
    } else {
        (Value::Array(supported.iter().cloned().map(Value::String).collect()), "all_provider_declared", false)
    };
    let Value::Array(items) = raw_coords else {
        return Err(contract("design_coordinates must be a list when explicitly supplied"));
    };
    let unique: std::collections::BTreeSet<String> = items.iter().map(Value::to_string).collect();
    if (items.is_empty() && !supported.is_empty())
        || items.iter().any(|v| v.as_str().is_none_or(str::is_empty))
        || unique.len() != items.len()
    {
        return Err(contract("design_coordinates must contain unique nonempty ids"));
    }
    let coords: Vec<String> = items.iter().map(crate::pyval::py_str).collect();
    let unsupported: Vec<String> = coords.iter().filter(|c| !supported.contains(c)).cloned().collect();
    if !unsupported.is_empty() {
        return Err(contract(format!(
            "provider {} does not supply design-coordinate derivatives for {}; available: {}",
            repr_str(&provider_name),
            str_list_repr(&unsupported),
            str_list_repr(&supported)
        )));
    }
    let inactive: Vec<String> = supported.iter().filter(|c| !coords.contains(c)).cloned().collect();
    let caps_map = caps.to_map();
    let caps_name = caps_map.get("name").map_or_else(String::new, crate::pyval::py_str);
    let default_bindings = default_binding_metadata(
        provider.as_ref(),
        &problem,
        &caps_name,
        caps_map.get("traits"),
        &supported,
    )?;
    let responses_raw: Vec<Value> = match d.get("responses") {
        Some(Value::Array(items)) if !items.is_empty() => items.clone(),
        Some(v) if crate::pyval::truthy(Some(v)) => return Err(contract("responses must be a list")),
        _ => ops.map(|o| o.problem_objectives(&problem)).unwrap_or_default(),
    };
    let responses: Vec<ResponseSpec> =
        responses_raw.iter().map(ResponseSpec::from_dict).collect::<CaeResult<_>>()?;
    let available: Vec<String> = caps_map
        .get("responses")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(crate::pyval::py_str).collect())
        .unwrap_or_default();

    let available = implexity_optim::admitted_responses(provider.as_ref(), &problem, &available)?;
    let unsupported: Vec<String> =
        responses.iter().filter(|r| !available.contains(&r.name)).map(|r| r.name.clone()).collect();
    if !unsupported.is_empty() {
        return Err(contract(format!(
            "provider {} does not supply responses {}; available: {}",
            repr_str(&provider_name),
            str_list_repr(&unsupported),
            str_list_repr(&available)
        )));
    }
    if let Some(check) = ops.and_then(|o| {
        o.validate_response_selection(&problem, &responses.iter().map(|r| r.name.clone()).collect::<Vec<_>>())
    }) {
        check?;
    }
    let mut selection = Map::new();
    selection.insert("source".into(), Value::String(source.into()));
    selection.insert("explicit".into(), Value::Bool(explicit));
    selection.insert(
        "provider_declared".into(),
        Value::Array(supported.iter().cloned().map(Value::String).collect()),
    );
    selection.insert("active".into(), Value::Array(coords.iter().cloned().map(Value::String).collect()));
    selection.insert("inactive".into(), Value::Array(inactive.iter().cloned().map(Value::String).collect()));
    let name = first_truthy(&[d.get("name")])
        .map_or_else(|| "implicit CAE problem".to_string(), crate::pyval::py_str);
    Ok(PreparedProblem {
        name,
        provider_name,
        provider,
        problem,
        problem_raw,
        design_coordinates: coords,
        inactive_design_coordinates: inactive,
        design_coordinate_selection: Value::Object(selection),
        default_design_bindings: default_bindings,
        responses,
        metadata: crate::pyval::mapping_or_empty(d.get("metadata")),
    })
}
