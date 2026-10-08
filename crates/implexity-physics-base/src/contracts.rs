// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::PyNum;

use crate::model_errors::{PhysicsError, PhysicsResult};

const ALLOWED_AGGREGATIONS: [&str; 7] =
    ["single", "sum", "product", "minimum", "maximum", "stack", "mixture"];
const ALLOWED_SUPPORTS: [&str; 7] =
    ["volume", "surface", "interface", "network", "point", "global", "history"];
const VALUE_KINDS: [&str; 6] = ["number", "integer", "array", "text", "boolean", "mapping"];
const QUALIFICATION_LEVELS: [&str; 4] = ["screening", "intermediate", "verification", "production"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    pub blocking: bool,
    pub field: Option<String>,
}

impl ValidationIssue {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into(), blocking: true, field: None }
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"code": self.code, "message": self.message, "blocking": self.blocking, "field": self.field})
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSpec {
    pub name: String,
    pub quantity: String,
    pub units: String,
    pub support: String,
    pub required: bool,
    pub aggregation: String,
    pub description: String,
    pub reverse_quantity: Option<String>,
}

impl PortSpec {
    #[must_use]
    pub fn new(name: &str, quantity: &str, units: &str, support: &str) -> Self {
        Self {
            name: name.into(),
            quantity: quantity.into(),
            units: units.into(),
            support: support.into(),
            required: true,
            aggregation: "single".into(),
            description: String::new(),
            reverse_quantity: None,
        }
    }

    #[must_use]
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    #[must_use]
    pub fn aggregation(mut self, aggregation: &str) -> Self {
        self.aggregation = aggregation.into();
        self
    }

    #[must_use]
    pub fn reverse(mut self, quantity: &str) -> Self {
        self.reverse_quantity = Some(quantity.into());
        self
    }

    #[must_use]
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let name = repr_str(&self.name);
        if self.name.trim().is_empty() {
            issues.push(ValidationIssue::new("EMPTY_PORT_NAME", "Port name must not be empty."));
        }
        if self.quantity.trim().is_empty() {
            issues.push(ValidationIssue::new(
                "EMPTY_QUANTITY",
                format!("Port {name} has no physical quantity."),
            ));
        }
        if self.units.trim().is_empty() {
            issues.push(ValidationIssue::new("MISSING_UNITS", format!("Port {name} must declare units.")));
        }
        if !ALLOWED_SUPPORTS.contains(&self.support.as_str()) {
            issues.push(ValidationIssue::new(
                "INVALID_SUPPORT",
                format!("Port {name} uses unsupported support {}.", repr_str(&self.support)),
            ));
        }
        if !ALLOWED_AGGREGATIONS.contains(&self.aggregation.as_str()) {
            issues.push(ValidationIssue::new(
                "INVALID_AGGREGATION",
                format!("Port {name} uses unsupported aggregation {}.", repr_str(&self.aggregation)),
            ));
        }
        issues
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "name": self.name, "quantity": self.quantity, "units": self.units, "support": self.support,
            "required": self.required, "aggregation": self.aggregation, "description": self.description,
            "reverse_quantity": self.reverse_quantity,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthoringField {
    pub name: String,
    pub units: String,
    pub required: bool,
    pub minimum: Option<PyNum>,
    pub maximum: Option<PyNum>,
    pub choices: Vec<String>,
    pub description: String,
    pub default: Value,
    pub value_kind: String,
    pub shape: Option<Vec<Option<usize>>>,
}

fn tuple_repr_usize(items: &[Option<usize>]) -> String {
    let parts: Vec<String> =
        items.iter().map(|v| v.map_or_else(|| "None".to_string(), |n| n.to_string())).collect();
    if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
}

fn tuple_repr_str(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| repr_str(s)).collect();
    if parts.len() == 1 { format!("({},)", parts[0]) } else { format!("({})", parts.join(", ")) }
}

enum ArrayProbe {
    Numeric(Vec<usize>, Vec<Value>),
    Ragged,
    Other,
}

fn probe_array(value: &Value) -> ArrayProbe {
    fn shape_of(v: &Value) -> Option<Vec<usize>> {
        match v {
            Value::Array(items) => {
                let mut inner: Option<Vec<usize>> = None;
                for item in items {
                    let s = shape_of(item)?;
                    match &inner {
                        None => inner = Some(s),
                        Some(prev) if *prev == s => {}
                        Some(_) => return None,
                    }
                }
                let mut out = vec![items.len()];
                out.extend(inner.unwrap_or_default());
                Some(out)
            }
            _ => Some(Vec::new()),
        }
    }
    fn flatten<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        match v {
            Value::Array(items) => items.iter().for_each(|i| flatten(i, out)),
            other => out.push(other),
        }
    }
    let Some(shape) = shape_of(value) else { return ArrayProbe::Ragged };
    let mut leaves = Vec::new();
    flatten(value, &mut leaves);
    if leaves.iter().all(|v| v.is_number()) {
        ArrayProbe::Numeric(shape, leaves.into_iter().cloned().collect())
    } else {
        ArrayProbe::Other
    }
}

fn has_boolean(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::Array(items) => items.iter().any(has_boolean),
        _ => false,
    }
}

impl AuthoringField {
    #[must_use]
    pub fn new(name: &str, units: &str) -> Self {
        Self {
            name: name.into(),
            units: units.into(),
            required: true,
            minimum: None,
            maximum: None,
            choices: Vec::new(),
            description: String::new(),
            default: Value::Null,
            value_kind: "number".into(),
            shape: None,
        }
    }

    #[must_use]
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    #[must_use]
    pub fn min(mut self, minimum: impl Into<PyNum>) -> Self {
        self.minimum = Some(minimum.into());
        self
    }

    #[must_use]
    pub fn max(mut self, maximum: impl Into<PyNum>) -> Self {
        self.maximum = Some(maximum.into());
        self
    }

    #[must_use]
    pub fn choices(mut self, choices: &[&str]) -> Self {
        self.choices = choices.iter().map(|c| (*c).to_string()).collect();
        self
    }

    #[must_use]
    pub fn default_value(mut self, default: Value) -> Self {
        self.default = default;
        self
    }

    #[must_use]
    pub fn array(mut self, shape: &[Option<usize>]) -> Self {
        self.value_kind = "array".into();
        self.shape = Some(shape.to_vec());
        self
    }

    #[must_use]
    pub fn kind(mut self, value_kind: &str) -> Self {
        self.value_kind = value_kind.into();
        self
    }


    pub fn check(&self) -> PhysicsResult<()> {
        if self.name.trim().is_empty() || self.units.trim().is_empty() {
            return Err(PhysicsError::value("Authoring fields require a name and explicit units"));
        }
        if !VALUE_KINDS.contains(&self.value_kind.as_str()) {
            return Err(PhysicsError::value("Unknown authoring value_kind"));
        }
        if let Some(shape) = &self.shape
            && (self.value_kind != "array" || shape.contains(&Some(0)))
        {
            return Err(PhysicsError::value("Array shapes require positive dimensions or None wildcards"));
        }
        for bound in [self.minimum, self.maximum].into_iter().flatten() {
            if !bound.is_finite() {
                return Err(PhysicsError::value("Authoring bounds must be finite numbers"));
            }
        }
        if let (Some(lo), Some(hi)) = (self.minimum, self.maximum)
            && lo.as_f64() > hi.as_f64()
        {
            return Err(PhysicsError::value("Authoring bounds must be ordered"));
        }
        if (self.minimum.is_some() || self.maximum.is_some())
            && !["number", "integer", "array"].contains(&self.value_kind.as_str())
        {
            return Err(PhysicsError::value("Numeric bounds cannot be attached to nonnumeric fields"));
        }
        Ok(())
    }

    fn issue(&self, code: &str, message: &str) -> Vec<ValidationIssue> {
        vec![ValidationIssue {
            code: code.into(),
            message: format!("{}: {message}", self.name),
            blocking: true,
            field: Some(self.name.clone()),
        }]
    }

    fn array_entries(&self, value: &Value) -> Result<Vec<Value>, Vec<ValidationIssue>> {
        if has_boolean(value) {
            return Err(self.issue("AUTHORING_TYPE", "boolean entries are not physical numbers"));
        }
        match probe_array(value) {
            ArrayProbe::Ragged => Err(self.issue("AUTHORING_TYPE", "expected a rectangular numeric array")),
            ArrayProbe::Other => Err(self.issue("AUTHORING_TYPE", "expected a nonempty real numeric array")),
            ArrayProbe::Numeric(shape, flat) => {
                if shape.is_empty() || flat.is_empty() {
                    return Err(self.issue("AUTHORING_TYPE", "expected a nonempty real numeric array"));
                }
                if let Some(wanted) = &self.shape
                    && (wanted.len() != shape.len()
                        || wanted.iter().zip(&shape).any(|(w, a)| w.is_some_and(|w| w != *a)))
                {
                    let got: Vec<Option<usize>> = shape.iter().map(|s| Some(*s)).collect();
                    return Err(self.issue(
                        "AUTHORING_SHAPE",
                        &format!(
                            "expected shape {}, got {}",
                            tuple_repr_usize(wanted),
                            tuple_repr_usize(&got)
                        ),
                    ));
                }
                Ok(flat)
            }
        }
    }

    #[must_use]
    pub fn validate_value(&self, value: &Value) -> Vec<ValidationIssue> {
        let value = if value.is_null() {
            if self.default.is_null() {
                return if self.required {
                    self.issue("MISSING_AUTHORING", "required field is missing")
                } else {
                    Vec::new()
                };
            }
            &self.default
        } else {
            value
        };
        if !self.choices.is_empty() {
            return match value.as_str() {
                Some(s) if self.choices.iter().any(|c| c == s) => Vec::new(),
                _ => self
                    .issue("INVALID_CHOICE", &format!("expected one of {}", tuple_repr_str(&self.choices))),
            };
        }
        let values: Vec<Value> = match self.value_kind.as_str() {
            "text" => {
                return if value.is_string() {
                    Vec::new()
                } else {
                    self.issue("AUTHORING_TYPE", "expected text")
                };
            }
            "boolean" => {
                return if value.is_boolean() {
                    Vec::new()
                } else {
                    self.issue("AUTHORING_TYPE", "expected boolean")
                };
            }
            "mapping" => {
                return if value.is_object() {
                    Vec::new()
                } else {
                    self.issue("AUTHORING_TYPE", "expected mapping")
                };
            }
            kind @ ("number" | "integer") => {
                let ok = match value {
                    Value::Number(n) => kind == "number" || n.is_i64() || n.is_u64(),
                    _ => false,
                };
                if !ok {
                    return self.issue("AUTHORING_TYPE", &format!("expected a {kind} scalar"));
                }
                vec![value.clone()]
            }
            _ => match self.array_entries(value) {
                Ok(flat) => flat,
                Err(issues) => return issues,
            },
        };
        for number in &values {
            let x = number.as_f64().unwrap_or(f64::NAN);
            if !x.is_finite() {
                return self.issue("NONFINITE_AUTHORING", "all entries must be finite");
            }
            if let Some(lo) = self.minimum
                && x < lo.as_f64()
            {
                return self.issue("AUTHORING_BELOW_MINIMUM", &format!("entry below {}", lo.repr()));
            }
            if let Some(hi) = self.maximum
                && x > hi.as_f64()
            {
                return self.issue("AUTHORING_ABOVE_MAXIMUM", &format!("entry above {}", hi.repr()));
            }
        }
        Vec::new()
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "name": self.name, "units": self.units, "required": self.required,
            "minimum": self.minimum.map(PyNum::to_value), "maximum": self.maximum.map(PyNum::to_value),
            "choices": self.choices, "description": self.description, "default": self.default,
            "value_kind": self.value_kind, "shape": self.shape,
        })
    }
}

pub const DEFAULT_SOLVE_STRATEGIES: [&str; 5] =
    ["monolithic", "exact_partitioned", "fixed_point", "one_way", "monolithic_or_exact_partitioned"];

#[derive(Debug, Clone, PartialEq)]
pub struct AddinContract {
    pub addin_id: String,
    pub family: String,
    pub version: String,
    pub fidelity: String,
    pub produces: Vec<PortSpec>,
    pub consumes: Vec<PortSpec>,
    pub authoring: Vec<AuthoringField>,
    pub objective_aliases: Vec<String>,
    pub design_coordinates: Vec<String>,
    pub coupled_group: Option<String>,
    pub solve_strategy: Option<String>,
    pub path_dependent: bool,
    pub conservative_balances: Vec<String>,
    pub validity_notes: Vec<String>,
    pub runtime_factory: Option<String>,
    pub supported_solve_strategies: Vec<String>,
    pub exact_coupled_derivatives: bool,
    pub exact_partial_derivatives: bool,
    pub qualification_level: String,
    pub metadata: Map<String, Value>,
}

impl AddinContract {
    #[must_use]
    pub fn new(addin_id: &str, family: &str, version: &str, fidelity: &str) -> Self {
        Self {
            addin_id: addin_id.into(),
            family: family.into(),
            version: version.into(),
            fidelity: fidelity.into(),
            produces: Vec::new(),
            consumes: Vec::new(),
            authoring: Vec::new(),
            objective_aliases: Vec::new(),
            design_coordinates: Vec::new(),
            coupled_group: None,
            solve_strategy: None,
            path_dependent: false,
            conservative_balances: Vec::new(),
            validity_notes: Vec::new(),
            runtime_factory: None,
            supported_solve_strategies: DEFAULT_SOLVE_STRATEGIES.iter().map(|s| (*s).to_string()).collect(),
            exact_coupled_derivatives: true,
            exact_partial_derivatives: false,
            qualification_level: "screening".into(),
            metadata: Map::new(),
        }
    }

    #[must_use]
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        let id = &self.addin_id;
        if id.trim().is_empty() {
            issues.push(ValidationIssue::new("EMPTY_ADDIN_ID", "Add-in ID must not be empty."));
        }
        if self.family.trim().is_empty() {
            issues.push(ValidationIssue::new("EMPTY_FAMILY", format!("{id}: family is required.")));
        }
        let mut seen: Vec<(&str, &str, &str)> = Vec::new();
        for p in self.produces.iter().chain(&self.consumes) {
            issues.extend(p.validate());
            let key = (p.name.as_str(), p.quantity.as_str(), p.support.as_str());
            if seen.contains(&key) {
                issues.push(ValidationIssue::new(
                    "DUPLICATE_PORT",
                    format!(
                        "{id}: duplicate port ({}, {}, {}).",
                        repr_str(key.0),
                        repr_str(key.1),
                        repr_str(key.2)
                    ),
                ));
            }
            seen.push(key);
        }
        if self.produces.is_empty() {
            issues.push(ValidationIssue::new(
                "NO_OUTPUT",
                format!("{id}: an add-in must produce at least one physical quantity."),
            ));
        }
        let unique: std::collections::BTreeSet<&String> = self.design_coordinates.iter().collect();
        if self.design_coordinates.is_empty()
            || self.design_coordinates.iter().any(|v| v.trim().is_empty())
            || unique.len() != self.design_coordinates.len()
        {
            issues.push(ValidationIssue::new(
                "NO_DESIGN_PATH",
                format!("{id}: optimizable add-ins must explicitly expose unique design coordinates."),
            ));
        }
        if self.coupled_group.as_deref().is_some_and(|g| !g.is_empty())
            && self.solve_strategy.as_deref().is_none_or(str::is_empty)
        {
            issues.push(ValidationIssue::new(
                "MISSING_SOLVE_STRATEGY",
                format!("{id}: coupled group requires a solve strategy."),
            ));
        }
        if let Some(strategy) = self.solve_strategy.as_deref().filter(|s| !s.is_empty())
            && !self.supported_solve_strategies.iter().any(|s| s == strategy)
        {
            issues.push(ValidationIssue::new(
                "UNSUPPORTED_SOLVE_STRATEGY",
                format!("{id}: solve strategy {} is not declared supported.", repr_str(strategy)),
            ));
        }
        if !QUALIFICATION_LEVELS.contains(&self.qualification_level.as_str()) {
            issues.push(ValidationIssue::new(
                "INVALID_QUALIFICATION_LEVEL",
                format!("{id}: invalid qualification level {}.", repr_str(&self.qualification_level)),
            ));
        }
        let names: Vec<&String> = self.authoring.iter().map(|a| &a.name).collect();
        let unique_names: std::collections::BTreeSet<&&String> = names.iter().collect();
        if unique_names.len() != names.len() {
            issues.push(ValidationIssue::new(
                "DUPLICATE_AUTHORING_FIELD",
                format!("{id}: duplicate authoring field."),
            ));
        }
        issues
    }

    #[must_use]
    pub fn validate_authoring(&self, namespace: Option<&Value>) -> Vec<ValidationIssue> {
        let data = match namespace {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                return vec![ValidationIssue::new(
                    "AUTHORING_NAMESPACE_TYPE",
                    format!("{}: authoring must be a mapping.", self.addin_id),
                )];
            }
        };
        let mut issues = Vec::new();
        for field in &self.authoring {
            let value = data.get(&field.name).cloned().unwrap_or_else(|| field.default.clone());
            issues.extend(field.validate_value(&value));
        }
        let mut leaked: Vec<&String> =
            data.keys().filter(|k| !self.authoring.iter().any(|a| &a.name == *k)).collect();
        if !leaked.is_empty() {
            leaked.sort();
            let names: Vec<String> = leaked.iter().map(|s| repr_str(s)).collect();
            issues.push(ValidationIssue::new(
                "UNDECLARED_AUTHORING_FIELD",
                format!("{}: undeclared provider-local fields [{}].", self.addin_id, names.join(", ")),
            ));
        }
        issues
    }

    #[must_use]
    pub fn asdict(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("addin_id".into(), json!(self.addin_id));
        m.insert("family".into(), json!(self.family));
        m.insert("version".into(), json!(self.version));
        m.insert("fidelity".into(), json!(self.fidelity));
        m.insert("produces".into(), Value::Array(self.produces.iter().map(PortSpec::to_value).collect()));
        m.insert("consumes".into(), Value::Array(self.consumes.iter().map(PortSpec::to_value).collect()));
        m.insert(
            "authoring".into(),
            Value::Array(self.authoring.iter().map(AuthoringField::to_value).collect()),
        );
        m.insert("objective_aliases".into(), json!(self.objective_aliases));
        m.insert("design_coordinates".into(), json!(self.design_coordinates));
        m.insert("coupled_group".into(), json!(self.coupled_group));
        m.insert("solve_strategy".into(), json!(self.solve_strategy));
        m.insert("path_dependent".into(), json!(self.path_dependent));
        m.insert("conservative_balances".into(), json!(self.conservative_balances));
        m.insert("validity_notes".into(), json!(self.validity_notes));
        m.insert("runtime_factory".into(), json!(self.runtime_factory));
        m.insert("supported_solve_strategies".into(), json!(self.supported_solve_strategies));
        m.insert("exact_coupled_derivatives".into(), json!(self.exact_coupled_derivatives));
        m.insert("exact_partial_derivatives".into(), json!(self.exact_partial_derivatives));
        m.insert("qualification_level".into(), json!(self.qualification_level));
        m.insert("metadata".into(), Value::Object(self.metadata.clone()));
        m
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        let mut data = self.asdict();
        let mut capabilities: std::collections::BTreeSet<String> =
            self.objective_aliases.iter().cloned().collect();
        capabilities.extend(self.produces.iter().map(|p| p.quantity.clone()));
        data.insert("id".into(), json!(self.addin_id));
        data.insert("provider_id".into(), json!(self.addin_id));
        data.insert("capabilities".into(), json!(capabilities.into_iter().collect::<Vec<_>>()));
        data.insert("outputs".into(), Value::Array(self.produces.iter().map(PortSpec::to_value).collect()));
        data.insert("inputs".into(), Value::Array(self.consumes.iter().map(PortSpec::to_value).collect()));
        let names = |required: bool| -> Vec<String> {
            self.consumes.iter().filter(|p| p.required == required).map(|p| p.quantity.clone()).collect()
        };
        data.insert("required_inputs".into(), json!(names(true)));
        data.insert("optional_inputs".into(), json!(names(false)));
        let fields = |required: bool| -> Vec<String> {
            self.authoring.iter().filter(|a| a.required == required).map(|a| a.name.clone()).collect()
        };
        data.insert("required_authoring".into(), json!(fields(true)));
        data.insert("optional_authoring".into(), json!(fields(false)));
        let exact = if self.exact_partial_derivatives && self.exact_coupled_derivatives {
            self.design_coordinates.clone()
        } else {
            Vec::new()
        };
        data.insert("exact_sensitivities".into(), json!(exact));
        data.insert(
            "derivative_declaration".into(),
            json!({
                "local_partials_declared_exact": self.exact_partial_derivatives,
                "coupled_derivatives_declared_exact": self.exact_coupled_derivatives,
                "coordinates": self.design_coordinates,
                "qualification": "declaration_only_not_evaluation_evidence",
            }),
        );
        data.insert(
            "coupling".into(),
            json!({
                "group": self.coupled_group,
                "declared_strategy": self.solve_strategy,
                "supported_strategies": self.supported_solve_strategies,
                "exact_coupled_derivatives": self.exact_coupled_derivatives,
            }),
        );
        Value::Object(data)
    }
}
