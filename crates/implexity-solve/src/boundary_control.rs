// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeSet;
use std::sync::Arc;

use implexity_core::contracts::{CaeProvider, ProviderCapabilities};
use implexity_core::error::{CaeError, CaeResult};
use implexity_core::json::canonical;
use implexity_core::py_repr::repr_str;
use serde_json::{Map, Value, json};

use crate::convergence::repr_name_list;

pub const CATALOG_SCHEMA: &str = "implexity-boundary-control-catalog/1";
pub const PROVIDER_SCHEMA: &str = "implexity-provider-boundary-control/1";
pub const BOUNDARY_CONTROL_INTERFACE: &str = "boundary_control";

const CONTROL_TYPES: [&str; 10] = [
    "boolean",
    "enum",
    "identifier",
    "integer",
    "interval",
    "number",
    "object",
    "region_reference",
    "selector",
    "text",
];

pub struct BoundaryControlHook(pub BoundaryControlFn);

pub type BoundaryControlFn = Box<dyn Fn(Option<&Value>) -> CaeResult<Option<Value>> + Send + Sync>;

impl std::fmt::Debug for BoundaryControlHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BoundaryControlHook")
    }
}

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn text(value: Option<&Value>, path: &str, maximum: usize) -> CaeResult<String> {
    match value {
        Some(Value::String(s)) if !s.trim().is_empty() && s == s.trim() && s.chars().count() <= maximum => {
            Ok(s.clone())
        }
        _ => Err(err(format!("{path} must be nonempty canonical text"))),
    }
}

fn boolean(value: Option<&Value>, path: &str) -> CaeResult<bool> {
    value.and_then(Value::as_bool).ok_or_else(|| err(format!("{path} must be boolean")))
}

fn finite(value: &Value, path: &str) -> CaeResult<f64> {
    value.as_f64().filter(|v| v.is_finite()).ok_or_else(|| err(format!("{path} must be a finite number")))
}

fn float_value(x: f64) -> Value {
    serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
}

fn sorted_copy(value: &Value) -> Value {
    match value {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|k| (k.clone(), sorted_copy(&m[k]))).collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted_copy).collect()),
        other => other.clone(),
    }
}

fn json_copy(value: &Value, path: &str) -> CaeResult<Value> {
    if canonical(value).len() > 262_144 {
        return Err(err(format!("{path} is too large")));
    }
    Ok(sorted_copy(value))
}

fn string_array(value: Option<&Value>, path: &str, maximum: usize) -> CaeResult<Vec<String>> {
    let bad = || err(format!("{path} must be an array of canonical identifiers"));
    let items = value.and_then(Value::as_array).ok_or_else(bad)?;
    if items.len() > maximum {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::String(s) if !s.trim().is_empty() && s == s.trim() => out.push(s.clone()),
            _ => return Err(bad()),
        }
    }
    if out.iter().collect::<BTreeSet<_>>().len() != out.len() {
        return Err(err(format!("{path} contains duplicate identifiers")));
    }
    Ok(out)
}

fn object<'a>(value: Option<&'a Value>, path: &str) -> CaeResult<&'a Map<String, Value>> {
    value.and_then(Value::as_object).ok_or_else(|| err(format!("{path} must be an object")))
}

fn require(raw: &Map<String, Value>, required: &[&str], path: &str) -> CaeResult<()> {
    let mut missing: Vec<String> =
        required.iter().filter(|k| !raw.contains_key(**k)).map(|k| (*k).to_string()).collect();
    missing.sort();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(err(format!("{path} omitted fields {}", repr_name_list(&missing))))
    }
}

fn sorted_difference(items: &[String], known: &BTreeSet<String>) -> Vec<String> {
    items.iter().filter(|x| !known.contains(*x)).cloned().collect::<BTreeSet<_>>().into_iter().collect()
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

fn stable_id(raw: Option<&Value>, provider: &str) -> CaeResult<Value> {
    let path = format!("provider {provider}.stable_id");
    let raw = object(raw, &path)?;
    require(
        raw,
        &["field", "label", "description", "required", "editable", "regeneratable", "pattern"],
        &path,
    )?;
    Ok(json!({
        "field": text(raw.get("field"), &format!("{path}.field"), 160)?,
        "label": text(raw.get("label"), &format!("{path}.label"), 320)?,
        "description": text(raw.get("description"), &format!("{path}.description"), 4096)?,
        "required": boolean(raw.get("required"), &format!("{path}.required"))?,
        "editable": boolean(raw.get("editable"), &format!("{path}.editable"))?,
        "regeneratable": boolean(raw.get("regeneratable"), &format!("{path}.regeneratable"))?,
        "pattern": text(raw.get("pattern"), &format!("{path}.pattern"), 320)?,
    }))
}

fn selector(raw: Option<&Value>, provider: &str) -> CaeResult<Value> {
    let path = format!("provider {provider}.selector");
    let raw = object(raw, &path)?;
    require(
        raw,
        &[
            "field",
            "region_field",
            "label",
            "description",
            "required",
            "semantics",
            "coordinate_system",
            "registration",
            "exact_before_commit",
            "allowed_kinds",
        ],
        &path,
    )?;
    Ok(json!({
        "field": text(raw.get("field"), &format!("{path}.field"), 160)?,
        "region_field": text(raw.get("region_field"), &format!("{path}.region_field"), 160)?,
        "label": text(raw.get("label"), &format!("{path}.label"), 320)?,
        "description": text(raw.get("description"), &format!("{path}.description"), 4096)?,
        "required": boolean(raw.get("required"), &format!("{path}.required"))?,
        "semantics": text(raw.get("semantics"), &format!("{path}.semantics"), 4096)?,
        "coordinate_system": text(raw.get("coordinate_system"), &format!("{path}.coordinate_system"), 320)?,
        "registration": text(raw.get("registration"), &format!("{path}.registration"), 320)?,
        "exact_before_commit": boolean(raw.get("exact_before_commit"), &format!("{path}.exact_before_commit"))?,
        "allowed_kinds": string_array(raw.get("allowed_kinds"), &format!("{path}.allowed_kinds"), 512)?,
    }))
}

fn is_number(v: &Value) -> bool {
    v.is_number()
}

#[allow(clippy::too_many_lines)]
fn field(raw: &Value, provider: &str, condition: &str, index: usize) -> CaeResult<Value> {
    let path = format!("provider {provider} condition {} field {index}", repr_str(condition));
    let raw = object(Some(raw), &path)?;
    require(raw, &["path", "label", "description", "control", "required", "unit_si", "editor_step"], &path)?;
    let control = text(raw.get("control"), &format!("{path}.control"), 64)?;
    if !CONTROL_TYPES.contains(&control.as_str()) {
        let names: Vec<String> = CONTROL_TYPES.iter().map(|s| (*s).to_string()).collect();
        return Err(err(format!("{path}.control must be one of {}", repr_name_list(&names))));
    }
    let boundary = Value::String("boundary".into());
    let scope = text(Some(raw.get("scope").unwrap_or(&boundary)), &format!("{path}.scope"), 64)?;
    if scope != "boundary" && scope != "problem" {
        return Err(err(format!("{path}.scope must be boundary or problem")));
    }
    let minimum = match raw.get("minimum") {
        None | Some(Value::Null) => None,
        Some(v) => Some(finite(v, &format!("{path}.minimum"))?),
    };
    let maximum = match raw.get("maximum") {
        None | Some(Value::Null) => None,
        Some(v) => Some(finite(v, &format!("{path}.maximum"))?),
    };
    if let (Some(lo), Some(hi)) = (minimum, maximum)
        && lo > hi
    {
        return Err(err(format!("{path} minimum exceeds maximum")));
    }
    let choices = match raw.get("choices") {
        Some(v) if truthy(v) => string_array(Some(v), &format!("{path}.choices"), 512)?,
        _ => Vec::new(),
    };
    if control == "enum" && choices.is_empty() {
        return Err(err(format!("{path}.choices is required for an enum")));
    }
    let mut default = json_copy(raw.get("default").unwrap_or(&Value::Null), &format!("{path}.default"))?;
    if control == "interval" {
        let pair = default
            .as_array()
            .filter(|a| a.len() == 2 && a.iter().all(is_number))
            .cloned()
            .ok_or_else(|| err(format!("{path}.default must be a two-number [lower, upper] interval")))?;
        let lower = finite(&pair[0], &format!("{path}.default[0]"))?;
        let upper = finite(&pair[1], &format!("{path}.default[1]"))?;
        if lower >= upper {
            return Err(err(format!("{path}.default interval must satisfy lower < upper")));
        }
        if minimum.is_some_and(|m| lower < m) {
            return Err(err(format!("{path}.default is below minimum")));
        }
        if maximum.is_some_and(|m| upper > m) {
            return Err(err(format!("{path}.default exceeds maximum")));
        }
        default = json!([float_value(lower), float_value(upper)]);
    }
    if default.is_number() {
        let numeric = finite(&default, &format!("{path}.default"))?;
        if minimum.is_some_and(|m| numeric < m) {
            return Err(err(format!("{path}.default is below minimum")));
        }
        if maximum.is_some_and(|m| numeric > m) {
            return Err(err(format!("{path}.default exceeds maximum")));
        }
    }
    if !choices.is_empty()
        && !default.is_null()
        && !default.as_str().is_some_and(|d| choices.iter().any(|c| c == d))
    {
        return Err(err(format!("{path}.default is not an allowed choice")));
    }
    Ok(json!({
        "path": text(raw.get("path"), &format!("{path}.path"), 512)?,
        "scope": scope,
        "label": text(raw.get("label"), &format!("{path}.label"), 320)?,
        "description": text(raw.get("description"), &format!("{path}.description"), 4096)?,
        "control": control,
        "required": boolean(raw.get("required"), &format!("{path}.required"))?,
        "unit_si": text(raw.get("unit_si"), &format!("{path}.unit_si"), 160)?,
        "minimum": minimum.map_or(Value::Null, float_value),
        "maximum": maximum.map_or(Value::Null, float_value),
        "default": default,
        "choices": choices,
        "editor_step": text(raw.get("editor_step"), &format!("{path}.editor_step"), 160)?,
    }))
}

fn bounded_array<'a>(raw: Option<&'a Value>, path: &str, limit: usize) -> CaeResult<&'a Vec<Value>> {
    raw.and_then(Value::as_array)
        .filter(|a| a.len() <= limit)
        .ok_or_else(|| err(format!("{path} must be an array of at most {limit} entries")))
}

fn unique_ids(rows: &[Value], path: &str) -> CaeResult<()> {
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    if ids.iter().collect::<BTreeSet<_>>().len() == ids.len() {
        Ok(())
    } else {
        Err(err(format!("{path} contains duplicate IDs")))
    }
}

fn roles(raw: Option<&Value>, provider: &str) -> CaeResult<Vec<Value>> {
    let path = format!("provider {provider}.roles");
    let mut out = Vec::new();
    for (index, value) in bounded_array(raw, &path, 512)?.iter().enumerate() {
        let item = format!("{path}[{index}]");
        let value = object(Some(value), &item)?;
        require(
            value,
            &[
                "id",
                "label",
                "description",
                "repeatable",
                "allowed_condition_types",
                "default_condition_type",
            ],
            &item,
        )?;
        let allowed = string_array(
            value.get("allowed_condition_types"),
            &format!("{item}.allowed_condition_types"),
            512,
        )?;
        let default =
            text(value.get("default_condition_type"), &format!("{item}.default_condition_type"), 160)?;
        if !allowed.contains(&default) {
            return Err(err(format!("{item}.default_condition_type must be allowed")));
        }
        let no = Value::Bool(false);
        out.push(json!({
            "id": text(value.get("id"), &format!("{item}.id"), 160)?,
            "label": text(value.get("label"), &format!("{item}.label"), 320)?,
            "description": text(value.get("description"), &format!("{item}.description"), 4096)?,
            "repeatable": boolean(value.get("repeatable"), &format!("{item}.repeatable"))?,
            "required_by_default": boolean(Some(value.get("required_by_default").unwrap_or(&no)), &format!("{item}.required_by_default"))?,
            "selector_required": boolean(Some(value.get("selector_required").unwrap_or(&no)), &format!("{item}.selector_required"))?,
            "allowed_condition_types": allowed,
            "default_condition_type": default,
        }));
    }
    unique_ids(&out, &path)?;
    Ok(out)
}

fn conditions(raw: Option<&Value>, provider: &str, role_ids: &BTreeSet<String>) -> CaeResult<Vec<Value>> {
    let path = format!("provider {provider}.condition_types");
    let mut out = Vec::new();
    for (index, value) in bounded_array(raw, &path, 512)?.iter().enumerate() {
        let item = format!("{path}[{index}]");
        let value = object(Some(value), &item)?;
        require(value, &["id", "label", "description", "allowed_roles", "required_fields"], &item)?;
        let identifier = text(value.get("id"), &format!("{item}.id"), 160)?;
        let allowed = string_array(value.get("allowed_roles"), &format!("{item}.allowed_roles"), 512)?;
        let unknown = sorted_difference(&allowed, role_ids);
        if !unknown.is_empty() {
            return Err(err(format!(
                "{item}.allowed_roles names unknown roles {}",
                repr_name_list(&unknown)
            )));
        }
        let fields = value
            .get("required_fields")
            .and_then(Value::as_array)
            .filter(|a| a.len() <= 128)
            .ok_or_else(|| err(format!("{item}.required_fields must contain at most 128 fields")))?;
        let rows: Vec<Value> = fields
            .iter()
            .enumerate()
            .map(|(i, f)| field(f, provider, &identifier, i))
            .collect::<CaeResult<_>>()?;
        let paths: Vec<&str> = rows.iter().filter_map(|r| r["path"].as_str()).collect();
        if paths.iter().collect::<BTreeSet<_>>().len() != paths.len() {
            return Err(err(format!("{item}.required_fields contains duplicate paths")));
        }
        out.push(json!({
            "id": identifier,
            "label": text(value.get("label"), &format!("{item}.label"), 320)?,
            "description": text(value.get("description"), &format!("{item}.description"), 4096)?,
            "allowed_roles": allowed,
            "required_fields": rows,
        }));
    }
    unique_ids(&out, &path)?;
    Ok(out)
}

fn configuration_groups(
    raw: Option<&Value>,
    provider: &str,
    condition_ids: &BTreeSet<String>,
) -> CaeResult<Vec<Value>> {
    let path = format!("provider {provider}.configuration_groups");
    if raw.is_none_or(Value::is_null) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (index, value) in bounded_array(raw, &path, 64)?.iter().enumerate() {
        let item = format!("{path}[{index}]");
        let value = object(Some(value), &item)?;
        require(value, &["id", "path", "label", "description", "selected", "options"], &item)?;
        let options_raw = value
            .get("options")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty() && a.len() <= 64)
            .ok_or_else(|| err(format!("{item}.options must contain 1..64 entries")))?;
        let mut options = Vec::new();
        for (oi, option) in options_raw.iter().enumerate() {
            let op = format!("{item}.options[{oi}]");
            let option = object(Some(option), &op)?;
            require(option, &["id", "label", "description", "condition_types"], &op)?;
            let linked = string_array(option.get("condition_types"), &format!("{op}.condition_types"), 512)?;
            let unknown = sorted_difference(&linked, condition_ids);
            if !unknown.is_empty() {
                return Err(err(format!(
                    "{op}.condition_types names unknown conditions {}",
                    repr_name_list(&unknown)
                )));
            }
            options.push(json!({
                "id": text(option.get("id"), &format!("{op}.id"), 160)?,
                "label": text(option.get("label"), &format!("{op}.label"), 320)?,
                "description": text(option.get("description"), &format!("{op}.description"), 4096)?,
                "condition_types": linked,
            }));
        }
        let option_ids: Vec<&str> = options.iter().filter_map(|o| o["id"].as_str()).collect();
        if option_ids.iter().collect::<BTreeSet<_>>().len() != option_ids.len() {
            return Err(err(format!("{item}.options contains duplicate IDs")));
        }
        let selected = text(value.get("selected"), &format!("{item}.selected"), 160)?;
        if !option_ids.contains(&selected.as_str()) {
            return Err(err(format!("{item}.selected is not an available option")));
        }
        let yes = Value::Bool(true);
        out.push(json!({
            "id": text(value.get("id"), &format!("{item}.id"), 160)?,
            "path": text(value.get("path"), &format!("{item}.path"), 512)?,
            "label": text(value.get("label"), &format!("{item}.label"), 320)?,
            "description": text(value.get("description"), &format!("{item}.description"), 4096)?,
            "exclusive": boolean(Some(value.get("exclusive").unwrap_or(&yes)), &format!("{item}.exclusive"))?,
            "selected": selected,
            "options": options,
        }));
    }
    unique_ids(&out, &path)?;
    Ok(out)
}

fn legacy_identifier_control(condition_types: &[String]) -> Value {
    let roles: Vec<Value> = condition_types
        .iter()
        .map(|id| {
            json!({
                "id": id, "label": id,
                "description": "Provider-declared boundary role; this compatibility provider did not publish a descriptive control schema.",
                "repeatable": true, "required_by_default": false, "selector_required": false,
                "allowed_condition_types": [id], "default_condition_type": id,
            })
        })
        .collect();
    let conditions: Vec<Value> = condition_types
        .iter()
        .map(|id| {
            json!({
                "id": id, "label": id,
                "description": "Provider-declared condition identifier. Additional fields and selector meaning remain provider-specific.",
                "allowed_roles": [id], "required_fields": [],
            })
        })
        .collect();
    json!({
        "schema": PROVIDER_SCHEMA,
        "boundary_array_path": "boundary_conditions",
        "stable_id": {
            "field": "id", "label": "Stable boundary ID",
            "description": "Stable identifier retained across edits.",
            "required": false, "editable": true, "regeneratable": true,
            "pattern": "^[A-Za-z][A-Za-z0-9_.-]*$",
        },
        "selector": {
            "field": "selector", "region_field": "region",
            "label": "Provider-owned spatial selector",
            "description": "This compatibility provider did not publish a selector schema.",
            "required": false,
            "semantics": "Opaque provider-owned selector data.",
            "coordinate_system": "provider_owned",
            "registration": "provider_owned",
            "exact_before_commit": true,
            "allowed_kinds": [],
        },
        "roles": roles,
        "condition_types": conditions,
        "compatibility": "identifier_only",
    })
}

fn capability_condition_types(caps: &ProviderCapabilities) -> CaeResult<Vec<String>> {
    let value = match caps {
        ProviderCapabilities::Mapping(m) => {
            if m.contains_key("condition_types") {
                m.get("condition_types").cloned()
            } else {
                m.get("conditionTypes").cloned()
            }
        }
        other => other.get("condition_types"),
    };
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(v) => string_array(Some(&v), "provider condition_types", 512),
    }
}


pub fn normalise_provider_boundary_control(
    name: &str,
    caps: &ProviderCapabilities,
    provider: &Arc<dyn CaeProvider>,
) -> CaeResult<Value> {
    normalise_provider_boundary_control_with(name, caps, Some(provider.as_ref()), None)
}


pub fn normalise_provider_boundary_control_with(
    provider_id: &str,
    caps: &ProviderCapabilities,
    provider: Option<&dyn CaeProvider>,
    problem: Option<&Value>,
) -> CaeResult<Value> {
    let provider_id = text(Some(&Value::String(provider_id.to_string())), "provider_id", 160)?;
    let quoted = repr_str(&provider_id);
    let traits = caps.get("traits").and_then(|t| t.as_object().cloned()).unwrap_or_default();
    let mut raw = None;
    if let Some(hook) = provider
        .and_then(|p| p.interface(BOUNDARY_CONTROL_INTERFACE))
        .and_then(|any| any.downcast_ref::<BoundaryControlHook>())
    {
        raw = (hook.0)(problem)?;
    }
    if raw.as_ref().is_none_or(Value::is_null) {
        raw = traits.get("boundary_control").cloned().filter(|v| !v.is_null());
    }
    let mut compatibility = None;
    let raw = if let Some(raw) = raw {
        raw
    } else {
        let identifiers = capability_condition_types(caps)?;
        if identifiers.is_empty() {
            return Ok(json!({
                "provider_id": provider_id,
                "available": false,
                "reason": "provider_did_not_publish_boundary_controls",
                "roles": [],
                "condition_types": [],
            }));
        }
        compatibility = Some("identifier_only".to_string());
        legacy_identifier_control(&identifiers)
    };
    let Some(map) =
        raw.as_object().filter(|m| m.get("schema").and_then(Value::as_str) == Some(PROVIDER_SCHEMA))
    else {
        return Err(err(format!("provider {quoted} boundary-control declaration is malformed")));
    };
    let boundary_array_path =
        text(map.get("boundary_array_path"), &format!("provider {quoted}.boundary_array_path"), 320)?;
    let stable = stable_id(map.get("stable_id"), &quoted)?;
    let select = selector(map.get("selector"), &quoted)?;
    let role_rows = roles(map.get("roles"), &quoted)?;
    let role_ids: BTreeSet<String> =
        role_rows.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    let condition_rows = conditions(map.get("condition_types"), &quoted, &role_ids)?;
    let condition_ids: BTreeSet<String> =
        condition_rows.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    let groups = configuration_groups(map.get("configuration_groups"), &quoted, &condition_ids)?;
    for (index, role) in role_rows.iter().enumerate() {
        let allowed: Vec<String> = role["allowed_condition_types"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let unknown = sorted_difference(&allowed, &condition_ids);
        if !unknown.is_empty() {
            return Err(err(format!(
                "provider {quoted}.roles[{index}] names unknown conditions {}",
                repr_name_list(&unknown)
            )));
        }
    }
    let compatibility = compatibility.map_or_else(
        || map.get("compatibility").cloned().unwrap_or_else(|| Value::String("native_catalogue".into())),
        Value::String,
    );
    let result = json!({
        "provider_id": provider_id,
        "available": true,
        "schema": PROVIDER_SCHEMA,
        "boundary_array_path": boundary_array_path,
        "stable_id": stable,
        "selector": select,
        "roles": role_rows,
        "condition_types": condition_rows,
        "configuration_groups": groups,
        "compatibility": compatibility,
    });
    json_copy(&result, &format!("provider {quoted} boundary control"))
}

fn problem_for_provider<'a>(provider_id: &str, context: Option<&'a Value>) -> Option<&'a Value> {
    let context = context?.as_object()?;
    if let Some(found) =
        context.get("provider_problems").and_then(|d| d.get(provider_id)).filter(|v| v.is_object())
    {
        return Some(found);
    }
    if context.get("provider").and_then(Value::as_str) == Some(provider_id)
        && let Some(problem) = context.get("problem").filter(|v| v.is_object())
    {
        return Some(problem);
    }
    for key in ["physics", "problem", "authoring", "intent"] {
        if let Some(nested) = context.get(key).filter(|v| v.is_object())
            && let Some(found) = problem_for_provider(provider_id, Some(nested))
        {
            return Some(found);
        }
    }
    None
}


pub fn boundary_catalogue(
    provider_id: Option<&str>,
    problem_context: Option<&Value>,
    context_source: &str,
) -> CaeResult<Value> {
    let registries = implexity_core::registries::global();
    let snapshot = registries.providers.snapshot();
    let mut rows = Vec::new();
    for (name, provider) in &snapshot.entries {
        if provider_id.is_some_and(|p| p != name) {
            continue;
        }
        let caps = provider.capabilities()?;
        let direct = provider_id == Some(name.as_str());
        let provider_problem = problem_for_provider(name, problem_context)
            .or_else(|| if direct { problem_context.filter(|v| v.is_object()) } else { None });
        let mut row =
            normalise_provider_boundary_control_with(name, &caps, Some(provider.as_ref()), provider_problem)?;
        if let Value::Object(m) = &mut row {
            m.insert("problem_context_applied".into(), Value::Bool(provider_problem.is_some()));
        }
        rows.push(row);
    }
    if let Some(p) = provider_id
        && rows.is_empty()
    {
        return Err(err(format!("unknown CAE provider {}", repr_str(p))));
    }
    let token = registries.providers.binding_token();
    Ok(json!({
        "schema": CATALOG_SCHEMA,
        "provider_registry": {"generation": token.generation, "fingerprint": token.fingerprint},
        "providers": rows,
        "problem_context": {"source": context_source, "supplied": problem_context.is_some()},
        "request_contract": {
            "inspection_only": true,
            "provider_owns_meaning": true,
            "client_must_not_infer_missing_fields": true,
            "exact_selector_required_before_commit_when_declared": true,
        },
    }))
}

