// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use crate::contracts::{TOPOLOGY_COORDINATE, require_contract_bool};
use crate::error::{CaeError, CaeResult};
use crate::json::canonical_sha256;
use crate::py_repr::repr_str;
use crate::pyobj::{list_repr, py_eq, py_str, repr, truthy};

pub const STRICT_CONTRACT_VERSION: i64 = 2;

macro_rules! str_enum {
    ($(#[$doc:meta])* $name:ident, $label:literal, { $($(#[$vdoc:meta])* $variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name {
            $($(#[$vdoc])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            #[must_use]
            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some($name::$variant),)+ _ => None }
            }


            pub fn from_value(raw: &Value) -> CaeResult<Self> {
                Self::parse(&py_str(raw))
                    .ok_or_else(|| CaeError::contract(format!(concat!("unknown ", $label, " {}"), repr(raw))))
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(
    AddInCategory, "add-in category", {
        Field = "field",
        Constitutive = "constitutive",
        Interface = "interface",
        Evolution = "evolution",
        Network = "network",
        Process = "process",
        Material = "material",
    }
);

str_enum!(
    Fidelity, "fidelity", {
        Screening = "screening",
        Intermediate = "intermediate",
        High = "high",
        Qualification = "qualification",
    }
);

impl Fidelity {
    #[must_use]
    pub fn rank(self) -> u8 {
        match self {
            Self::Screening => 0,
            Self::Intermediate => 1,
            Self::High => 2,
            Self::Qualification => 3,
        }
    }
}

str_enum!(
    RuntimeRoute, "runtime route", {
        Composite = "composite",
        Array = "array",
        MatureJob = "mature_job",
        JobExtension = "job_extension",
    }
);

str_enum!(
    ExecutionKind, "execution kind", {
        Residual = "residual",
        Algebraic = "algebraic",
        Operation = "operation",
        Provider = "provider",
        LifecycleExtension = "lifecycle_extension",
        Legacy = "legacy",
    }
);

str_enum!(
    PlanStatus, "plan status", {
        Ready = "ready",
        NeedsAuthoring = "needs_authoring",
        Blocked = "blocked",
    }
);

const TEMPORAL: [&str; 4] = ["instantaneous", "lagged", "history", "steady"];
const AGGREGATION: [&str; 6] = ["single", "sum", "mean", "minimum", "maximum", "concatenate"];
const PORT_FIELDS: [&str; 9] = [
    "quantity",
    "unit",
    "domain",
    "interface",
    "temporal",
    "conserved",
    "port_id",
    "cardinality",
    "aggregation",
];

fn sorted_unknown(map: &Map<String, Value>, allowed: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = map.keys().filter(|k| !allowed.contains(&k.as_str())).cloned().collect();
    v.sort();
    v
}

fn type_error_missing(class: &str, name: &str) -> CaeError {
    CaeError::contract(format!(
        "{class}.__init__() missing 1 required positional argument: {}",
        repr_str(name)
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortSpec {
    pub quantity: String,
    pub unit: String,
    pub domain: String,
    pub interface: Option<String>,
    pub temporal: String,
    pub conserved: bool,
    pub port_id: String,
    pub cardinality: String,
    pub aggregation: String,
}

pub type PortKey = (String, String, String, Option<String>, String, bool, String, String, String);

impl PortSpec {
    pub fn new(quantity: impl Into<String>) -> Self {
        Self {
            quantity: quantity.into(),
            unit: "-".into(),
            domain: "*".into(),
            interface: None,
            temporal: "instantaneous".into(),
            conserved: false,
            port_id: String::new(),
            cardinality: "field".into(),
            aggregation: "single".into(),
        }
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.quantity.trim().is_empty() {
            return Err(CaeError::contract("physics port requires a quantity"));
        }
        if !TEMPORAL.contains(&self.temporal.as_str()) {
            return Err(CaeError::contract(format!(
                "unsupported port temporal semantics {}",
                repr_str(&self.temporal)
            )));
        }
        if !AGGREGATION.contains(&self.aggregation.as_str()) {
            return Err(CaeError::contract(format!(
                "unsupported port aggregation semantics {}",
                repr_str(&self.aggregation)
            )));
        }
        if self.cardinality.trim().is_empty() {
            return Err(CaeError::contract("physics port cardinality is required"));
        }
        Ok(())
    }

    #[must_use]
    pub fn key(&self) -> PortKey {
        (
            self.quantity.clone(),
            self.unit.clone(),
            self.domain.clone(),
            self.interface.clone(),
            self.temporal.clone(),
            self.conserved,
            self.cardinality.clone(),
            self.aggregation.clone(),
            self.port_id.clone(),
        )
    }

    #[must_use]
    pub fn key_repr(&self) -> String {
        format!(
            "({}, {}, {}, {}, {}, {}, {}, {}, {})",
            repr_str(&self.quantity),
            repr_str(&self.unit),
            repr_str(&self.domain),
            self.interface.as_deref().map_or_else(|| "None".to_string(), repr_str),
            repr_str(&self.temporal),
            if self.conserved { "True" } else { "False" },
            repr_str(&self.cardinality),
            repr_str(&self.aggregation),
            repr_str(&self.port_id),
        )
    }


    pub fn from_mapping(raw: &Value, strict: bool) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("physics port must be an object"));
        };
        let unknown = sorted_unknown(map, &PORT_FIELDS);
        if !unknown.is_empty() {
            return Err(CaeError::contract(format!("physics port has unknown keys {}", list_repr(&unknown))));
        }
        if strict {
            let mut missing: Vec<&str> =
                PORT_FIELDS.iter().copied().filter(|k| !map.contains_key(*k)).collect();
            missing.sort_unstable();
            if !missing.is_empty() {
                return Err(CaeError::contract(format!(
                    "strict physics port is missing {}",
                    list_repr(&missing)
                )));
            }
        }
        let Some(quantity) = map.get("quantity") else {
            return Err(type_error_missing("PortSpec", "quantity"));
        };
        let mut port = Self::new(String::new());
        match quantity.as_str() {
            Some(q) if !q.trim().is_empty() => port.quantity = q.to_string(),
            _ => return Err(CaeError::contract("physics port requires a quantity")),
        }
        for key in ["unit", "domain", "temporal", "port_id", "cardinality", "aggregation"] {
            if let Some(v) = map.get(key) {
                let Some(text) = v.as_str() else {
                    return Err(CaeError::contract(format!("physics port {key} must be text")));
                };
                let slot = match key {
                    "unit" => &mut port.unit,
                    "domain" => &mut port.domain,
                    "temporal" => &mut port.temporal,
                    "port_id" => &mut port.port_id,
                    "cardinality" => &mut port.cardinality,
                    _ => &mut port.aggregation,
                };
                *slot = text.to_string();
            }
        }
        match map.get("interface") {
            None | Some(Value::Null) => port.interface = None,
            Some(Value::String(s)) => port.interface = Some(s.clone()),
            Some(_) => return Err(CaeError::contract("physics port interface must be text or null")),
        }
        if let Some(v) = map.get("conserved") {
            port.conserved = require_contract_bool(v, "physics port conserved", false)?.unwrap_or(false);
        }
        port.validate()?;
        if strict
            && (port.unit.is_empty() || port.unit == "*" || port.domain.is_empty() || port.domain == "*")
        {
            return Err(CaeError::contract(
                "strict physics ports require explicit unit and domain; '-' is dimensionless",
            ));
        }
        Ok(port)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("quantity".into(), json!(self.quantity));
        m.insert("unit".into(), json!(self.unit));
        m.insert("domain".into(), json!(self.domain));
        m.insert("interface".into(), json!(self.interface));
        m.insert("temporal".into(), json!(self.temporal));
        m.insert("conserved".into(), json!(self.conserved));
        m.insert("port_id".into(), json!(self.port_id));
        m.insert("cardinality".into(), json!(self.cardinality));
        m.insert("aggregation".into(), json!(self.aggregation));
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseCapability {
    pub response: String,
    pub unit: String,
    pub differentiable: Option<bool>,
    pub topology_reachable: Option<bool>,
    pub depends_on: Vec<String>,
    pub label: String,
    pub description: String,
    pub family: String,
    pub design_reachable: Option<bool>,
}

impl ResponseCapability {
    pub fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            unit: "-".into(),
            differentiable: None,
            topology_reachable: None,
            depends_on: Vec::new(),
            label: String::new(),
            description: String::new(),
            family: String::new(),
            design_reachable: None,
        }
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.response.trim().is_empty() {
            return Err(CaeError::contract("response capability requires a response id"));
        }
        let r = repr_str(&self.response);
        if self.unit.is_empty() {
            return Err(CaeError::contract(format!("response {r}: unit must be explicit text")));
        }
        if self.depends_on.iter().any(String::is_empty) {
            return Err(CaeError::contract(format!("response {r}: depends_on must be a tuple of ids")));
        }
        let mut seen = std::collections::BTreeSet::new();
        if !self.depends_on.iter().all(|d| seen.insert(d)) {
            return Err(CaeError::contract(format!("response {r}: duplicate dependency ids")));
        }
        Ok(())
    }


    pub fn from_mapping(raw: &Value, strict: bool) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("response capability must be an object"));
        };
        let mut allowed = vec![
            "response",
            "unit",
            "differentiable",
            "design_reachable",
            "depends_on",
            "label",
            "description",
            "family",
        ];
        if !strict {
            allowed.push("topology_reachable");
        }
        let unknown = sorted_unknown(map, &allowed);
        if !unknown.is_empty() {
            return Err(CaeError::contract(format!(
                "response capability has unknown keys {}",
                list_repr(&unknown)
            )));
        }
        let depends_raw = match map.get("depends_on") {
            None => None,
            Some(Value::Array(items)) => Some(items),
            Some(_) => return Err(CaeError::contract("response depends_on must be a list of ids")),
        };
        let Some(response) = map.get("response") else {
            return Err(type_error_missing("ResponseCapability", "response"));
        };
        let response = match response.as_str() {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => return Err(CaeError::contract("response capability requires a response id")),
        };
        let r = repr_str(&response);
        let mut cap = Self::new(response.clone());
        if let Some(v) = map.get("unit") {
            match v.as_str() {
                Some(u) if !u.is_empty() => cap.unit = u.to_string(),
                _ => return Err(CaeError::contract(format!("response {r}: unit must be explicit text"))),
            }
        }
        for key in ["label", "description", "family"] {
            if let Some(v) = map.get(key) {
                let Some(text) = v.as_str() else {
                    return Err(CaeError::contract(format!("response {r}: {key} must be text")));
                };
                let slot = match key {
                    "label" => &mut cap.label,
                    "description" => &mut cap.description,
                    _ => &mut cap.family,
                };
                *slot = text.to_string();
            }
        }
        for key in ["differentiable", "topology_reachable", "design_reachable"] {
            if let Some(v) = map.get(key) {
                let claim = require_contract_bool(v, &format!("response {r}: {key}"), true)?;
                match key {
                    "differentiable" => cap.differentiable = claim,
                    "topology_reachable" => cap.topology_reachable = claim,
                    _ => cap.design_reachable = claim,
                }
            }
        }
        if let Some(items) = depends_raw {
            let mut deps = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) if !s.is_empty() => deps.push(s.to_string()),
                    _ => {
                        return Err(CaeError::contract(format!(
                            "response {r}: depends_on must be a tuple of ids"
                        )));
                    }
                }
            }
            cap.depends_on = deps;
        }
        cap.validate()?;
        if strict && (cap.differentiable.is_none() || cap.design_reachable.is_none()) {
            return Err(CaeError::contract(format!(
                "response {r}: strict derivative and reachability claims are required"
            )));
        }
        Ok(cap)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("response".into(), json!(self.response));
        m.insert("unit".into(), json!(self.unit));
        m.insert("differentiable".into(), json!(self.differentiable));
        m.insert("topology_reachable".into(), json!(self.topology_reachable));
        m.insert("depends_on".into(), json!(self.depends_on));
        m.insert("label".into(), json!(self.label));
        m.insert("description".into(), json!(self.description));
        m.insert("family".into(), json!(self.family));
        m.insert("design_reachable".into(), json!(self.design_reachable));
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExternalPortValue {
    pub port: PortSpec,
    pub value: Value,
    pub source_id: String,
}

impl ExternalPortValue {

    pub fn new(port: PortSpec, value: Value, source_id: impl Into<String>) -> CaeResult<Self> {
        let source_id = source_id.into();
        port.validate()?;
        if source_id.trim().is_empty() {
            return Err(CaeError::contract("external port value requires a source_id"));
        }
        Ok(Self { port, value, source_id })
    }


    pub fn from_mapping(raw: &Value) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("external port value must be an object"));
        };
        let mut allowed: Vec<&str> = vec!["port", "value", "source_id"];
        allowed.extend(PORT_FIELDS);
        let unknown = sorted_unknown(map, &allowed);
        if !unknown.is_empty() {
            return Err(CaeError::contract(format!(
                "external port value has unknown keys {}",
                list_repr(&unknown)
            )));
        }
        let Some(value) = map.get("value") else {
            return Err(CaeError::contract("external port value requires value"));
        };
        let has_flat = PORT_FIELDS.iter().any(|k| map.contains_key(*k));
        let port_raw = if let Some(Value::Object(p)) = map.get("port") {
            if has_flat {
                return Err(CaeError::contract("external port value cannot mix nested and flat port fields"));
            }
            Value::Object(p.clone())
        } else {
            {
                let mut flat = Map::new();
                for k in PORT_FIELDS {
                    if let Some(v) = map.get(k) {
                        flat.insert(k.into(), v.clone());
                    }
                }
                Value::Object(flat)
            }
        };
        let port = PortSpec::from_mapping(&port_raw, true)?;
        let source =
            map.get("source_id").filter(|v| truthy(v)).map_or_else(|| "authored".to_string(), py_str);
        Self::new(port, value.clone(), source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DesignCoordinateRef {
    pub coordinate: String,
    pub addin_id: String,
    pub port_id: String,
}

impl DesignCoordinateRef {
    pub fn new(coordinate: impl Into<String>, port_id: impl Into<String>) -> Self {
        Self { coordinate: coordinate.into(), addin_id: String::new(), port_id: port_id.into() }
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.coordinate.trim().is_empty() {
            return Err(CaeError::contract("design-coordinate reference requires a coordinate"));
        }
        Ok(())
    }


    pub fn from_mapping(raw: &Value) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("design-coordinate reference must be an object"));
        };
        let unknown = sorted_unknown(map, &["coordinate", "addin_id", "port_id"]);
        if !unknown.is_empty() {
            return Err(CaeError::contract(format!(
                "design-coordinate reference has unknown keys {}",
                list_repr(&unknown)
            )));
        }
        let Some(coordinate) = map.get("coordinate") else {
            return Err(type_error_missing("DesignCoordinateRef", "coordinate"));
        };
        let coordinate = match coordinate.as_str() {
            Some(c) if !c.trim().is_empty() => c.to_string(),
            _ => return Err(CaeError::contract("design-coordinate reference requires a coordinate")),
        };
        let text = |key: &str, message: &str| -> CaeResult<String> {
            match map.get(key) {
                None => Ok(String::new()),
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(CaeError::contract(message)),
            }
        };
        let addin_id = text("addin_id", "design-coordinate addin_id must be text")?;
        let port_id = text("port_id", "design-coordinate port_id must be text")?;
        Ok(Self { coordinate, addin_id, port_id })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("coordinate".into(), json!(self.coordinate));
        m.insert("addin_id".into(), json!(self.addin_id));
        m.insert("port_id".into(), json!(self.port_id));
        Value::Object(m)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoringRequirement {
    pub key: String,
    pub description: String,
    pub optional: bool,
}

impl AuthoringRequirement {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), description: String::new(), optional: false }
    }


    pub fn validate(&self) -> CaeResult<()> {
        if self.key.trim().is_empty() {
            return Err(CaeError::contract("authoring requirement key is required"));
        }
        Ok(())
    }


    pub fn from_mapping(raw: &Value) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("authoring requirement must be an object"));
        };
        if let Some(extra) = map.keys().find(|k| !["key", "description", "optional"].contains(&k.as_str())) {
            return Err(CaeError::contract(format!(
                "AuthoringRequirement.__init__() got an unexpected keyword argument {}",
                repr_str(extra)
            )));
        }
        let Some(key) = map.get("key") else { return Err(type_error_missing("AuthoringRequirement", "key")) };
        let key = match key.as_str() {
            Some(k) if !k.trim().is_empty() => k.to_string(),
            _ => return Err(CaeError::contract("authoring requirement key is required")),
        };
        let description = match map.get("description") {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => return Err(CaeError::contract("authoring requirement description must be text")),
        };
        let optional = match map.get("optional") {
            None => false,
            Some(v) => require_contract_bool(
                v,
                &format!("authoring requirement {}: optional", repr_str(&key)),
                false,
            )?
            .unwrap_or(false),
        };
        Ok(Self { key, description, optional })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("key".into(), json!(self.key));
        m.insert("description".into(), json!(self.description));
        m.insert("optional".into(), json!(self.optional));
        Value::Object(m)
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddInContract {
    pub addin_id: String,
    pub category: AddInCategory,
    pub provides: Vec<PortSpec>,
    pub consumes: Vec<PortSpec>,
    pub responses: Vec<ResponseCapability>,
    pub authoring: Vec<AuthoringRequirement>,
    pub scope: Vec<String>,
    pub fidelity: Fidelity,
    pub priority: i64,
    pub runtime_route: RuntimeRoute,
    pub lifecycle: Option<String>,
    pub topology_coordinate: String,
    pub exact_design_derivatives: Option<bool>,
    pub exact_state_transpose: Option<bool>,
    pub notes: Vec<String>,
    pub direct_topology_dependence: Option<bool>,
    pub contract_version: i64,
    pub compatibility_mode: bool,
    pub owner_id: String,
    pub execution_kind: Option<ExecutionKind>,
    pub supported_operations: Vec<String>,
    pub no_op_operations: Vec<String>,
    pub design_inputs: Vec<DesignCoordinateRef>,
}

impl AddInContract {
    pub fn new(addin_id: impl Into<String>) -> Self {
        Self {
            addin_id: addin_id.into(),
            category: AddInCategory::Field,
            provides: Vec::new(),
            consumes: Vec::new(),
            responses: Vec::new(),
            authoring: Vec::new(),
            scope: vec!["*".into()],
            fidelity: Fidelity::Intermediate,
            priority: 0,
            runtime_route: RuntimeRoute::Composite,
            lifecycle: None,
            topology_coordinate: TOPOLOGY_COORDINATE.into(),
            exact_design_derivatives: None,
            exact_state_transpose: None,
            notes: Vec::new(),
            direct_topology_dependence: None,
            contract_version: 1,
            compatibility_mode: false,
            owner_id: String::new(),
            execution_kind: None,
            supported_operations: Vec::new(),
            no_op_operations: Vec::new(),
            design_inputs: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_strict(&self) -> bool {
        self.contract_version == STRICT_CONTRACT_VERSION && !self.compatibility_mode
    }


    #[allow(clippy::too_many_lines)]
    pub fn validate(&self) -> CaeResult<()> {
        for p in self.provides.iter().chain(&self.consumes) {
            p.validate()?;
        }
        for r in &self.responses {
            r.validate()?;
        }
        for a in &self.authoring {
            a.validate()?;
        }
        for d in &self.design_inputs {
            d.validate()?;
        }
        let id = &self.addin_id;
        if id.trim().is_empty() {
            return Err(CaeError::contract("add-in id is required"));
        }
        for (key, values) in [
            ("supported_operations", &self.supported_operations),
            ("no_op_operations", &self.no_op_operations),
        ] {
            if values.iter().any(String::is_empty) {
                return Err(CaeError::contract(format!("{id}: {key} must be a tuple of operation ids")));
            }
            let mut seen = std::collections::BTreeSet::new();
            if !values.iter().all(|v| seen.insert(v)) {
                return Err(CaeError::contract(format!("{id}: duplicate {key}")));
            }
        }
        if !self.no_op_operations.iter().all(|o| self.supported_operations.contains(o)) {
            return Err(CaeError::contract(format!(
                "{id}: no_op_operations must be a subset of supported_operations"
            )));
        }
        let mut ports = std::collections::BTreeSet::new();
        if !self.design_inputs.iter().all(|d| ports.insert(&d.port_id)) {
            return Err(CaeError::contract(format!("{id}: duplicate design-input port id")));
        }
        if self.contract_version != 1 && self.contract_version != STRICT_CONTRACT_VERSION {
            return Err(CaeError::contract(format!(
                "{id}: unsupported add-in contract version {}",
                self.contract_version
            )));
        }
        if self.scope.is_empty() || self.scope.iter().any(String::is_empty) {
            return Err(CaeError::contract(format!("{id}: scope must be a tuple of ids")));
        }
        if self.lifecycle.as_deref() == Some("") {
            return Err(CaeError::contract(format!("{id}: lifecycle must be non-empty text or null")));
        }
        if self.topology_coordinate.is_empty() {
            return Err(CaeError::contract(format!(
                "{id}: legacy topology_coordinate must be non-empty text"
            )));
        }
        if matches!(self.runtime_route, RuntimeRoute::MatureJob | RuntimeRoute::JobExtension)
            && self.lifecycle.is_none()
        {
            return Err(CaeError::contract(format!(
                "{id}: {} requires a lifecycle id",
                self.runtime_route.as_str()
            )));
        }
        if self.is_strict() {
            let mut missing: Vec<String> = Vec::new();
            if self.exact_design_derivatives.is_none() {
                missing.push("exact_design_derivatives".into());
            }
            if self.exact_state_transpose.is_none() {
                missing.push("exact_state_transpose".into());
            }
            for r in &self.responses {
                if r.differentiable.is_none() {
                    missing.push(format!("response:{}:differentiable", r.response));
                }
                if r.design_reachable.is_none() {
                    missing.push(format!("response:{}:design_reachable", r.response));
                }
            }
            if !missing.is_empty() {
                return Err(CaeError::contract(format!(
                    "{id}: strict claims are missing {}",
                    list_repr(&missing)
                )));
            }
            if self.direct_topology_dependence.is_some() {
                return Err(CaeError::contract(format!(
                    "{id}: direct_topology_dependence is compatibility-only; strict direct dependence is declared solely by design_inputs"
                )));
            }
            let legacy: Vec<&str> = self
                .responses
                .iter()
                .filter(|r| r.topology_reachable.is_some())
                .map(|r| r.response.as_str())
                .collect();
            if !legacy.is_empty() {
                return Err(CaeError::contract(format!(
                    "{id}: topology_reachable is compatibility-only for responses {}; use design_reachable",
                    list_repr(&legacy)
                )));
            }
            if matches!(self.execution_kind, None | Some(ExecutionKind::Legacy)) {
                return Err(CaeError::contract(format!("{id}: strict execution_kind is required")));
            }
            if self.supported_operations.is_empty() {
                return Err(CaeError::contract(format!("{id}: strict supported_operations are required")));
            }
            for r in &self.design_inputs {
                if r.port_id.trim().is_empty() {
                    return Err(CaeError::contract(format!(
                        "{id}: strict design inputs require a stable port_id"
                    )));
                }
                if !r.addin_id.is_empty() && r.addin_id != *id {
                    return Err(CaeError::contract(format!(
                        "{id}: design-input owner {} does not match the add-in",
                        repr_str(&r.addin_id)
                    )));
                }
            }
            for p in self.provides.iter().chain(&self.consumes) {
                if p.unit.is_empty() || p.unit == "*" || p.domain.is_empty() || p.domain == "*" {
                    return Err(CaeError::contract(format!(
                        "{id}: strict ports require explicit unit and domain"
                    )));
                }
                if p.port_id.is_empty() {
                    return Err(CaeError::contract(format!("{id}: strict ports require stable port_id")));
                }
            }
        }
        let mut responses = std::collections::BTreeSet::new();
        if !self.responses.iter().all(|r| responses.insert(&r.response)) {
            return Err(CaeError::contract(format!("{id}: duplicate response capability")));
        }
        if self.contract_version == STRICT_CONTRACT_VERSION {
            for (group, label) in [(&self.provides, "provided"), (&self.consumes, "consumed")] {
                let mut keys = std::collections::BTreeSet::new();
                if !group.iter().all(|p| keys.insert(p.key())) {
                    return Err(CaeError::contract(format!("{id}: duplicate {label} port")));
                }
            }
        }
        Ok(())
    }


    pub fn checked(self) -> CaeResult<Self> {
        self.validate()?;
        Ok(self)
    }



    #[allow(clippy::too_many_lines)]
    pub fn from_mapping(raw: &Value, strict: bool) -> CaeResult<Self> {
        let Some(map) = raw.as_object() else {
            return Err(CaeError::contract("add-in contract must be an object"));
        };
        let mut allowed = vec![
            "addin_id",
            "id",
            "category",
            "provides",
            "consumes",
            "responses",
            "authoring",
            "scope",
            "fidelity",
            "priority",
            "runtime_route",
            "runtimeRoute",
            "lifecycle",
            "exact_design_derivatives",
            "exact_state_transpose",
            "notes",
            "owner_id",
            "execution_kind",
            "supported_operations",
            "no_op_operations",
            "design_inputs",
            "contract_version",
            "contractVersion",
            "compatibility_mode",
        ];
        if !strict {
            allowed.extend(["topology_coordinate", "direct_topology_dependence"]);
        }
        let unknown = sorted_unknown(map, &allowed);
        if !unknown.is_empty() {
            return Err(CaeError::contract(format!(
                "add-in contract has unknown keys {}",
                list_repr(&unknown)
            )));
        }
        let version = map.get("contract_version").or_else(|| map.get("contractVersion"));
        if strict && !version.is_some_and(|v| py_eq(v, &json!(STRICT_CONTRACT_VERSION))) {
            return Err(CaeError::contract(format!(
                "strict add-in contract requires contract_version={STRICT_CONTRACT_VERSION}"
            )));
        }
        if !strict
            && let Some(v) = version
            && !v.is_null()
            && !py_eq(v, &json!(1))
        {
            return Err(CaeError::contract(format!(
                "legacy add-in mapping cannot declare contract version {}",
                repr(v)
            )));
        }
        let compatibility = map.get("compatibility_mode").cloned().unwrap_or(Value::Bool(false));
        let compatibility =
            require_contract_bool(&compatibility, "add-in compatibility_mode", false)?.unwrap_or(false);
        if strict && compatibility {
            return Err(CaeError::contract("strict add-in mappings cannot declare compatibility_mode"));
        }
        let sequence = |key: &str, label: &str| -> CaeResult<Vec<Value>> {
            match map.get(key) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(items)) => Ok(items.clone()),
                Some(_) => Err(CaeError::contract(format!("add-in {label} must be a list"))),
            }
        };
        let provides = sequence("provides", "ports")?
            .iter()
            .map(|p| PortSpec::from_mapping(p, strict))
            .collect::<CaeResult<Vec<_>>>()?;
        let consumes = sequence("consumes", "ports")?
            .iter()
            .map(|p| PortSpec::from_mapping(p, strict))
            .collect::<CaeResult<Vec<_>>>()?;
        let responses = sequence("responses", "responses")?
            .iter()
            .map(|r| ResponseCapability::from_mapping(r, strict))
            .collect::<CaeResult<Vec<_>>>()?;
        let authoring = sequence("authoring", "authoring")?
            .iter()
            .map(AuthoringRequirement::from_mapping)
            .collect::<CaeResult<Vec<_>>>()?;
        let category = match map.get("category") {
            None => AddInCategory::Field,
            Some(v) => AddInCategory::from_value(v)?,
        };
        if strict && !map.contains_key("fidelity") {
            return Err(CaeError::contract("strict add-in contract requires fidelity"));
        }
        let fidelity = match map.get("fidelity") {
            None => Fidelity::Intermediate,
            Some(v) => Fidelity::from_value(v)?,
        };
        if strict && !map.contains_key("runtime_route") && !map.contains_key("runtimeRoute") {
            return Err(CaeError::contract("strict add-in contract requires runtime_route"));
        }
        let runtime_route = match map.get("runtime_route").or_else(|| map.get("runtimeRoute")) {
            None => RuntimeRoute::Composite,
            Some(v) => RuntimeRoute::from_value(v)?,
        };
        if strict && !map.contains_key("execution_kind") {
            return Err(CaeError::contract("strict add-in contract requires execution_kind"));
        }
        let execution_kind = match map.get("execution_kind") {
            None | Some(Value::Null) => None,
            Some(v) => Some(ExecutionKind::from_value(v)?),
        };
        if strict && !map.contains_key("supported_operations") {
            return Err(CaeError::contract("strict add-in contract requires supported_operations"));
        }
        let claim = |name: &str| -> CaeResult<Option<bool>> {
            match map.get(name) {
                None if strict => Err(CaeError::contract(format!("strict add-in contract requires {name}"))),
                None => Ok(None),
                Some(v) => require_contract_bool(v, name, false),
            }
        };
        let list_of = |key: &str, default: Vec<Value>| -> CaeResult<Vec<Value>> {
            match map.get(key) {
                None => Ok(default),
                Some(Value::Array(items)) => Ok(items.clone()),
                Some(_) => Err(CaeError::contract(format!("add-in {key} must be a list"))),
            }
        };
        let scope = list_of("scope", vec![json!("*")])?;
        let notes = list_of("notes", Vec::new())?;
        let operations = list_of("supported_operations", Vec::new())?;
        let no_ops = list_of("no_op_operations", Vec::new())?;
        let design_rows = list_of("design_inputs", Vec::new())?;
        if strict && (scope.iter().any(|x| !x.is_string()) || notes.iter().any(|x| !x.is_string())) {
            return Err(CaeError::contract("strict add-in scope and notes entries must be text"));
        }
        let owner_id = match map.get("owner_id").filter(|v| truthy(v)) {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => {
                return Err(CaeError::contract("add-in owner_id and topology_coordinate must be text"));
            }
        };
        let topology_coordinate = match map.get("topology_coordinate") {
            None => TOPOLOGY_COORDINATE.to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => {
                return Err(CaeError::contract("add-in owner_id and topology_coordinate must be text"));
            }
        };
        let priority = match map.get("priority") {
            None => 0,
            Some(Value::Number(n)) if n.as_i64().is_some() && !n.is_f64() => n.as_i64().unwrap_or(0),
            Some(_) => return Err(CaeError::contract("add-in priority must be an integer")),
        };
        let addin_id = match ["addin_id", "id"].iter().find_map(|k| map.get(*k).filter(|v| truthy(v))) {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => return Err(CaeError::contract("add-in id must be text")),
        };
        let exact_design_derivatives = claim("exact_design_derivatives")?;
        let exact_state_transpose = claim("exact_state_transpose")?;
        let direct_topology_dependence = if strict { None } else { claim("direct_topology_dependence")? };
        if addin_id.trim().is_empty() {
            return Err(CaeError::contract("add-in id is required"));
        }
        let lifecycle = match map.get("lifecycle") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(_) => {
                return Err(CaeError::contract(format!(
                    "{addin_id}: lifecycle must be non-empty text or null"
                )));
            }
        };
        let op_text = |rows: &[Value], key: &str| -> CaeResult<Vec<String>> {
            rows.iter()
                .map(|v| match v.as_str() {
                    Some(s) if !s.is_empty() => Ok(s.to_string()),
                    _ => {
                        Err(CaeError::contract(format!("{addin_id}: {key} must be a tuple of operation ids")))
                    }
                })
                .collect()
        };
        let supported_operations = op_text(&operations, "supported_operations")?;
        let no_op_operations = op_text(&no_ops, "no_op_operations")?;
        let design_inputs =
            design_rows.iter().map(DesignCoordinateRef::from_mapping).collect::<CaeResult<Vec<_>>>()?;
        let contract = Self {
            addin_id,
            category,
            provides,
            consumes,
            responses,
            authoring,
            scope: scope.iter().map(py_str).collect(),
            fidelity,
            priority,
            runtime_route,
            lifecycle,
            topology_coordinate,
            exact_design_derivatives,
            exact_state_transpose,
            notes: notes.iter().map(py_str).collect(),
            direct_topology_dependence,
            contract_version: if strict { STRICT_CONTRACT_VERSION } else { 1 },
            compatibility_mode: false,
            owner_id,
            execution_kind,
            supported_operations,
            no_op_operations,
            design_inputs,
        };
        contract.validate()?;
        Ok(contract)
    }

    #[must_use]
    pub fn asdict(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("addin_id".into(), json!(self.addin_id));
        m.insert("category".into(), json!(self.category.as_str()));
        m.insert("provides".into(), Value::Array(self.provides.iter().map(PortSpec::to_value).collect()));
        m.insert("consumes".into(), Value::Array(self.consumes.iter().map(PortSpec::to_value).collect()));
        m.insert(
            "responses".into(),
            Value::Array(self.responses.iter().map(ResponseCapability::to_value).collect()),
        );
        m.insert(
            "authoring".into(),
            Value::Array(self.authoring.iter().map(AuthoringRequirement::to_value).collect()),
        );
        m.insert("scope".into(), json!(self.scope));
        m.insert("fidelity".into(), json!(self.fidelity.as_str()));
        m.insert("priority".into(), json!(self.priority));
        m.insert("runtime_route".into(), json!(self.runtime_route.as_str()));
        m.insert("lifecycle".into(), json!(self.lifecycle));
        m.insert("topology_coordinate".into(), json!(self.topology_coordinate));
        m.insert("exact_design_derivatives".into(), json!(self.exact_design_derivatives));
        m.insert("exact_state_transpose".into(), json!(self.exact_state_transpose));
        m.insert("notes".into(), json!(self.notes));
        m.insert("direct_topology_dependence".into(), json!(self.direct_topology_dependence));
        m.insert("contract_version".into(), json!(self.contract_version));
        m.insert("compatibility_mode".into(), json!(self.compatibility_mode));
        m.insert("owner_id".into(), json!(self.owner_id));
        m.insert("execution_kind".into(), json!(self.execution_kind.map(ExecutionKind::as_str)));
        m.insert("supported_operations".into(), json!(self.supported_operations));
        m.insert("no_op_operations".into(), json!(self.no_op_operations));
        m.insert(
            "design_inputs".into(),
            Value::Array(self.design_inputs.iter().map(DesignCoordinateRef::to_value).collect()),
        );
        m
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut payload = self.asdict();
        if self.is_strict() {
            payload.shift_remove("topology_coordinate");
            payload.shift_remove("direct_topology_dependence");
            if let Some(Value::Array(rows)) = payload.get_mut("responses") {
                for row in rows {
                    if let Some(obj) = row.as_object_mut() {
                        obj.shift_remove("topology_reachable");
                    }
                }
            }
        }
        Value::Object(payload)
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        canonical_sha256(&self.to_value())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PublishedContract {
    Contract(Box<AddInContract>),
    Mapping(Value),
}

