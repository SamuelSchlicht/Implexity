// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::LazyLock;

use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::list_repr;
use serde_json::{Map, Value, json};

use crate::error::{AgentError, AgentResult};
use crate::pyval::{is_int, is_number, number, type_is_dict};

pub const SCHEMA: &str = "implexity-engineering-agent/1";
pub const INTENT_SCHEMA: &str = "implexity-engineering-intent/1";
pub const POLICY_SCHEMA: &str = "implexity-agent-policy/1";
pub const COMPUTATION_EFFORT_REQUEST_SCHEMA: &str = "implexity-computation-effort-request/1";
const MAX_SAFE_JSON_INTEGER: i64 = (1 << 53) - 1;
const MAX_PREVIEW_TRUST_RADIUS: f64 = 1.0;
const MAX_PREVIEW_CORRECTION_CADENCE: i64 = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct ActionContract {
    pub name: String,
    pub label: String,
    pub permission: String,
    pub mutates: bool,
    pub requires_model: bool,
    pub description: String,
    pub input_schema: Value,
}

impl ActionContract {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({"name": self.name, "label": self.label, "permission": self.permission,
               "mutates": self.mutates, "requires_model": self.requires_model,
               "description": self.description})
    }
}

#[derive(Debug)]
pub struct Contracts {
    actions: Vec<ActionContract>,
    extension: Vec<ActionContract>,
    presets: Vec<(String, Vec<String>)>,
    manual: Value,
}

static EMBEDDED: &str = include_str!("../resources/contracts.json");
static EMBEDDED_EXTENSION: &str = include_str!("../resources/rust_extension_actions.json");
pub const EXTENSION: &str = "implexity-rust-extension/1";

fn parse_actions(doc: &Value) -> Result<Vec<ActionContract>, String> {
    let text = |v: &Value, k: &str| -> Result<String, String> {
        v.get(k).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("contract field {k}"))
    };
    let flag = |v: &Value, k: &str| -> Result<bool, String> {
        v.get(k).and_then(Value::as_bool).ok_or_else(|| format!("contract flag {k}"))
    };
    let mut actions = Vec::new();
    for row in doc.get("actions").and_then(Value::as_array).ok_or("contract actions")? {
        actions.push(ActionContract {
            name: text(row, "name")?,
            label: text(row, "label")?,
            permission: text(row, "permission")?,
            mutates: flag(row, "mutates")?,
            requires_model: flag(row, "requires_model")?,
            description: text(row, "description")?,
            input_schema: row.get("input_schema").cloned().ok_or("contract input schema")?,
        });
    }
    Ok(actions)
}

fn parse_embedded() -> Result<Contracts, String> {
    let doc: Value = serde_json::from_str(EMBEDDED).map_err(|e| e.to_string())?;
    let actions = parse_actions(&doc)?;
    let ext_doc: Value = serde_json::from_str(EMBEDDED_EXTENSION).map_err(|e| e.to_string())?;
    if ext_doc.get("extension").and_then(Value::as_str) != Some(EXTENSION) {
        return Err("the Rust-extension action catalogue names another extension".into());
    }
    let extension = parse_actions(&ext_doc)?;
    if extension.iter().any(|e| actions.iter().any(|a| a.name == e.name)) {
        return Err("a Rust-extension action repeats a golden action".into());
    }
    let presets = doc
        .get("autonomy_presets")
        .and_then(Value::as_object)
        .ok_or("autonomy presets")?
        .iter()
        .map(|(k, v)| {
            let classes =
                v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect());
            classes.map(|c| (k.clone(), c)).ok_or_else(|| format!("autonomy preset {k}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let manual = doc.get("manual").cloned().ok_or("manual rows")?;
    Ok(Contracts { actions, extension, presets, manual })
}

static CONTRACTS: LazyLock<Result<Contracts, String>> = LazyLock::new(parse_embedded);



pub fn contracts() -> AgentResult<&'static Contracts> {
    CONTRACTS.as_ref().map_err(|e| AgentError::failed(format!("embedded agent contracts are invalid: {e}")))
}

impl Contracts {
    #[must_use]
    pub fn actions(&self) -> &[ActionContract] {
        &self.actions
    }

    #[must_use]
    pub fn extension_actions(&self) -> &[ActionContract] {
        &self.extension
    }

    #[must_use]
    pub fn is_extension(&self, name: &str) -> bool {
        self.extension.iter().any(|a| a.name == name)
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ActionContract> {
        self.actions.iter().chain(&self.extension).find(|a| a.name == name)
    }

    #[must_use]
    pub fn presets(&self) -> &[(String, Vec<String>)] {
        &self.presets
    }

    #[must_use]
    pub fn preset(&self, preset: &str) -> Option<&[String]> {
        self.presets.iter().find(|(k, _)| k == preset).map(|(_, v)| v.as_slice())
    }

    #[must_use]
    pub fn manual_rows(&self) -> &Value {
        &self.manual
    }
}

fn public_effort_number(name: &str, value: Option<&Value>, positive: bool) -> AgentResult<Option<f64>> {
    let qualifier = if positive { "positive" } else { "non-negative" };
    let refuse = || AgentError::contract(format!("{name} must be a {qualifier} finite number or null"));
    let Some(value) = value.filter(|v| !v.is_null()) else { return Ok(None) };
    if !is_number(value) {
        return Err(refuse());
    }
    let n = number(value).ok_or_else(refuse)?;
    if !n.is_finite() || (if positive { n <= 0.0 } else { n < 0.0 }) {
        return Err(refuse());
    }
    Ok(Some(if n == 0.0 { 0.0 } else { n }))
}

fn public_effort_object(name: &str, value: &Value, allowed: &[&str]) -> AgentResult<Map<String, Value>> {
    let Some(m) = value.as_object().filter(|_| type_is_dict(value)) else {
        return Err(AgentError::contract(format!("{name} must be an object")));
    };
    let mut unknown: Vec<&String> = m.keys().filter(|k| !allowed.contains(&k.as_str())).collect();
    unknown.sort();
    if !unknown.is_empty() {
        return Err(AgentError::contract(format!("{name} has unknown fields {}", list_repr(&unknown))));
    }
    Ok(m.clone())
}

fn opt_json(v: Option<f64>) -> Value {
    v.map_or(Value::Null, |x| json!(x))
}

fn truthy(v: Option<f64>) -> bool {
    v.is_some_and(|x| x != 0.0)
}

fn none_compare(what: &str) -> AgentError {
    AgentError::failed(format!("'{what}' not supported between instances of 'NoneType' and 'float'"))
}



#[allow(clippy::too_many_lines)]
pub fn normalise_computation_effort_request(raw: Option<&Value>) -> AgentResult<Value> {
    let empty = json!({});
    let raw = raw.filter(|v| !v.is_null()).unwrap_or(&empty);
    let row = public_effort_object(
        "computation_effort",
        raw,
        &[
            "schema",
            "preset",
            "mode",
            "hard_budgets",
            "target_update_rate_hz",
            "error_limits",
            "trust_radius",
            "exact_correction",
            "ood_policy",
            "coupling_approximation",
        ],
    )?;
    let schema = row.get("schema").cloned().unwrap_or_else(|| json!(COMPUTATION_EFFORT_REQUEST_SCHEMA));
    if schema != COMPUTATION_EFFORT_REQUEST_SCHEMA {
        return Err(AgentError::contract("unsupported computation_effort schema"));
    }
    let preset = row.get("preset").filter(|v| !v.is_null());
    if let Some(p) = preset {
        if !p.as_str().is_some_and(|s| ["exact", "fast", "staged", "interactive"].contains(&s)) {
            return Err(AgentError::contract(
                "computation_effort.preset must be exact, staged, or interactive",
            ));
        }
        let mut conflicting: Vec<&str> = [
            "mode",
            "target_update_rate_hz",
            "error_limits",
            "trust_radius",
            "exact_correction",
            "ood_policy",
        ]
        .into_iter()
        .filter(|k| row.contains_key(*k))
        .collect();
        conflicting.sort_unstable();
        if !conflicting.is_empty() {
            return Err(AgentError::contract(format!(
                "computation_effort.preset cannot be combined with advanced fields {}",
                list_repr(&conflicting)
            )));
        }
    }
    let preset = preset.and_then(Value::as_str);
    let interactive = matches!(preset, Some("fast" | "interactive"));
    let staged = preset == Some("staged");
    let preview = interactive || staged;
    let default_mode = if interactive {
        "interactive_preview"
    } else if staged {
        "verified_preview"
    } else {
        "exact"
    };
    let mode = row.get("mode").cloned().unwrap_or_else(|| json!(default_mode));
    let Some(mode) =
        mode.as_str().filter(|m| ["exact", "verified_preview", "interactive_preview"].contains(m))
    else {
        return Err(AgentError::contract(
            "computation_effort.mode must be exact, verified_preview, or interactive_preview",
        ));
    };
    let budgets = public_effort_object(
        "computation_effort.hard_budgets",
        row.get("hard_budgets").unwrap_or(&empty),
        &["wall_time_s", "memory_bytes", "wall_time_mode"],
    )?;
    let wall_time = public_effort_number(
        "computation_effort.hard_budgets.wall_time_s",
        budgets.get("wall_time_s"),
        true,
    )?;
    let memory = budgets.get("memory_bytes").filter(|v| !v.is_null());
    let wall_time_mode = budgets.get("wall_time_mode").cloned().unwrap_or_else(|| json!("default"));
    let Some(wall_time_mode) = wall_time_mode.as_str().filter(|m| ["default", "unlimited"].contains(m))
    else {
        return Err(AgentError::contract(
            "computation_effort.hard_budgets.wall_time_mode must be default or unlimited",
        ));
    };
    if wall_time_mode == "unlimited"
        && (!budgets.contains_key("wall_time_s") || wall_time.is_some() || memory.is_none())
    {
        return Err(AgentError::contract(
            "unlimited wall time requires explicit null wall_time_s and a positive memory_bytes budget",
        ));
    }
    if let Some(m) = memory
        && !(is_int(m) && m.as_i64().is_some_and(|n| n > 0 && n <= MAX_SAFE_JSON_INTEGER))
    {
        return Err(AgentError::contract(
            "computation_effort.hard_budgets.memory_bytes must be a positive interoperable integer or null",
        ));
    }
    let default_rate = if interactive {
        json!(20.0)
    } else if staged {
        json!(5.0)
    } else {
        Value::Null
    };
    let update_rate = public_effort_number(
        "computation_effort.target_update_rate_hz",
        Some(row.get("target_update_rate_hz").unwrap_or(&default_rate)),
        true,
    )?;
    let limits = public_effort_object(
        "computation_effort.error_limits",
        row.get("error_limits").unwrap_or(&empty),
        &["response", "state", "gradient"],
    )?;
    let default_response = json!(if interactive {
        0.2
    } else if staged {
        0.05
    } else {
        0.0
    });
    let zero = json!(0.0);
    let response_error = public_effort_number(
        "computation_effort.error_limits.response",
        Some(limits.get("response").unwrap_or(&default_response)),
        false,
    )?;
    let state_error = public_effort_number(
        "computation_effort.error_limits.state",
        Some(limits.get("state").unwrap_or(&zero)),
        false,
    )?;
    let gradient_error = public_effort_number(
        "computation_effort.error_limits.gradient",
        Some(limits.get("gradient").unwrap_or(&zero)),
        false,
    )?;
    let default_trust = json!(if interactive {
        0.15
    } else if staged {
        0.08
    } else {
        0.0
    });
    let trust_radius = public_effort_number(
        "computation_effort.trust_radius",
        Some(row.get("trust_radius").unwrap_or(&default_trust)),
        false,
    )?;
    match trust_radius {
        None => return Err(none_compare(">")),
        Some(t) if t > MAX_PREVIEW_TRUST_RADIUS => {
            return Err(AgentError::contract("computation_effort.trust_radius must be between 0 and 1"));
        }
        Some(_) => {}
    }
    let correction = public_effort_object(
        "computation_effort.exact_correction",
        row.get("exact_correction").unwrap_or(&empty),
        &["cadence_updates", "deadline_s"],
    )?;
    let default_cadence = json!(if interactive {
        5
    } else if staged {
        2
    } else {
        1
    });
    let cadence = correction.get("cadence_updates").unwrap_or(&default_cadence).clone();
    let cadence_n =
        cadence.as_i64().filter(|n| is_int(&cadence) && (1..=MAX_PREVIEW_CORRECTION_CADENCE).contains(n));
    let Some(cadence_n) = cadence_n else {
        return Err(AgentError::contract(
            "computation_effort.exact_correction.cadence_updates must be an integer between 1 and 64",
        ));
    };
    let default_deadline = json!(if preview { 10800.0 } else { 0.0 });
    let deadline = public_effort_number(
        "computation_effort.exact_correction.deadline_s",
        Some(correction.get("deadline_s").unwrap_or(&default_deadline)),
        false,
    )?;
    let default_ood = json!(if preview { "require_exact" } else { "refuse" });
    let ood = row.get("ood_policy").unwrap_or(&default_ood).clone();
    let Some(ood) = ood.as_str().filter(|o| ["refuse", "require_exact", "hold_exact_anchor"].contains(o))
    else {
        return Err(AgentError::contract(
            "computation_effort.ood_policy must be refuse, require_exact, or hold_exact_anchor",
        ));
    };
    if mode == "exact" {
        if truthy(response_error)
            || truthy(state_error)
            || truthy(gradient_error)
            || truthy(trust_radius)
            || truthy(deadline)
        {
            return Err(AgentError::contract(
                "exact computation_effort requires zero error limits, trust radius, and correction deadline",
            ));
        }
        if cadence_n != 1 {
            return Err(AgentError::contract("exact computation_effort requires correction cadence one"));
        }
    } else {
        let (Some(t), Some(d)) = (trust_radius, deadline) else { return Err(none_compare("<=")) };
        if t <= 0.0 || d <= 0.0 {
            return Err(AgentError::contract(
                "preview computation_effort requires positive trust_radius and correction deadline_s",
            ));
        }
    }
    let coupling = public_effort_object(
        "computation_effort.coupling_approximation",
        row.get("coupling_approximation").unwrap_or(&empty),
        &["preset", "lagged_coupling_ids"],
    )?;
    let default_coupling = json!(match mode {
        "exact" => "exact",
        "verified_preview" => "staged",
        _ => "interactive",
    });
    let coupling_preset = coupling.get("preset").unwrap_or(&default_coupling).clone();
    let Some(coupling_preset) =
        coupling_preset.as_str().filter(|p| ["exact", "staged", "interactive", "explicit"].contains(p))
    else {
        return Err(AgentError::contract(
            "computation_effort.coupling_approximation.preset must be exact, staged, interactive, or explicit",
        ));
    };
    let empty_list = json!([]);
    let raw_ids = coupling.get("lagged_coupling_ids").unwrap_or(&empty_list);
    let ids_ok = raw_ids.as_array().is_some_and(|ids| {
        let mut seen = std::collections::BTreeSet::new();
        ids.len() <= 32
            && ids.iter().all(|v| {
                v.as_str().is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 160 && seen.insert(s))
            })
    });
    if !ids_ok {
        return Err(AgentError::contract(
            "computation_effort.coupling_approximation.lagged_coupling_ids must be a unique list of at most 32 nonempty identifiers",
        ));
    }
    let ids = raw_ids.as_array().cloned().unwrap_or_default();
    if mode == "exact" {
        if coupling_preset != "exact" || !ids.is_empty() {
            return Err(AgentError::contract("exact computation_effort cannot lag or omit couplings"));
        }
    } else if coupling_preset == "exact" {
        if !ids.is_empty() {
            return Err(AgentError::contract("exact coupling initialization cannot list lagged couplings"));
        }
    } else if coupling_preset == "explicit" {
        if ids.is_empty() {
            return Err(AgentError::contract(
                "explicit coupling approximation requires at least one lagged coupling id",
            ));
        }
    } else if !ids.is_empty() {
        return Err(AgentError::contract(
            "lagged_coupling_ids are accepted only with coupling_approximation.preset=explicit",
        ));
    }
    let mut hard = Map::new();
    hard.insert("wall_time_s".into(), opt_json(wall_time));
    hard.insert("memory_bytes".into(), memory.cloned().unwrap_or(Value::Null));
    if wall_time_mode == "unlimited" {
        hard.insert("wall_time_mode".into(), json!("unlimited"));
    }
    Ok(json!({
        "schema": COMPUTATION_EFFORT_REQUEST_SCHEMA,
        "mode": mode,
        "hard_budgets": hard,
        "target_update_rate_hz": opt_json(update_rate),
        "error_limits": {"response": opt_json(response_error), "state": opt_json(state_error),
                         "gradient": opt_json(gradient_error)},
        "trust_radius": opt_json(trust_radius),
        "exact_correction": {"cadence_updates": cadence_n, "deadline_s": opt_json(deadline)},
        "ood_policy": ood,
        "coupling_approximation": {"preset": coupling_preset, "lagged_coupling_ids": ids},
    }))
}

const SEMANTICS: [&str; 8] =
    ["minimize", "maximize", "upper", "lower", "target", "less_equal", "greater_equal", "equal"];



pub fn normalise_intent(raw: &Value) -> AgentResult<Value> {
    let Some(m) = raw.as_object() else {
        return Err(AgentError::contract("engineering intent must be an object"));
    };
    if let Some(s) = m.get("schema").filter(|v| !v.is_null())
        && s != INTENT_SCHEMA
    {
        return Err(AgentError::contract(format!(
            "engineering intent schema must be {}",
            repr_str(INTENT_SCHEMA)
        )));
    }
    let mut out = m.clone();
    let version = out.get("contract_version").cloned().unwrap_or_else(|| json!(2));
    let version = version.as_i64().filter(|v| is_int(&version) && (*v == 1 || *v == 2));
    let Some(version) = version else {
        return Err(AgentError::contract("engineering intent contract_version must be 1 or 2"));
    };
    out.insert("contract_version".into(), json!(version));
    if version == 2 {
        out.shift_remove("schema");
        out.shift_remove("compatibility");
        if let Some(c) = out.get("active_design_coordinates") {
            let ok = c.as_array().is_some_and(|a| {
                let mut seen = std::collections::BTreeSet::new();
                !a.is_empty()
                    && a.iter().all(|v| v.as_str().is_some_and(|s| !s.trim().is_empty() && seen.insert(s)))
            });
            if !ok {
                return Err(AgentError::contract(
                    "active_design_coordinates, when supplied, must be a unique nonempty explicit subset",
                ));
            }
        }
    } else {
        out.insert("schema".into(), json!(INTENT_SCHEMA));
    }
    let list_or_empty = |k: &str| -> Option<Value> {
        match out.get(k) {
            Some(v) if crate::pyval::truthy(v) => Some(v.clone()),
            _ => Some(json!([])),
        }
    };
    let goals = list_or_empty("goals").unwrap_or_default();
    let constraints = list_or_empty("constraints").unwrap_or_default();
    let (Some(g), Some(c)) = (goals.as_array(), constraints.as_array()) else {
        return Err(AgentError::contract(
            "engineering intent requires at least one goal or strict-v2 constraint",
        ));
    };
    if g.is_empty() && (version == 1 || c.is_empty()) {
        return Err(AgentError::contract(
            "engineering intent requires at least one goal or strict-v2 constraint",
        ));
    }
    let response_of = |row: &Value| -> bool {
        row.as_object()
            .and_then(|r| r.get("response"))
            .filter(|v| crate::pyval::truthy(v))
            .is_some_and(|v| !crate::pyval::py_str(v).trim().is_empty())
    };
    for (i, row) in g.iter().enumerate() {
        if !response_of(row) {
            return Err(AgentError::contract(format!("goal {i} requires a response")));
        }
        let pick = |k: &str| row.get(k).filter(|v| crate::pyval::truthy(v));
        let semantic = pick("relation")
            .or_else(|| pick("sense"))
            .map_or_else(|| "minimize".to_owned(), crate::pyval::py_str);
        if !SEMANTICS.contains(&semantic.as_str()) {
            return Err(AgentError::contract(format!("goal {i} has an unsupported relation/sense")));
        }
    }
    for (i, row) in c.iter().enumerate() {
        if !response_of(row) {
            return Err(AgentError::contract(format!("constraint {i} requires a response")));
        }
    }
    if let Some(a) = out.get("autonomy") {
        let name = crate::pyval::py_str(a);
        if contracts()?.preset(&name).is_none() {
            return Err(AgentError::contract("unsupported autonomy preset"));
        }
    }
    Ok(Value::Object(out))
}

