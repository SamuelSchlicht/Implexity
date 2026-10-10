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



use std::any::Any;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::contributions::{ContributionError, ContributionRegistry, ContributionValue};
use crate::error::CaseError;
use crate::py_repr::repr_str;
use crate::pyobj::list_repr;

pub const KIND: &str = "http_routes";
pub const IMPLEMENTATION: &str = "implexity.routes.Registry";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyPolicy {
    None,
    Json,
    Raw,
}

impl BodyPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Json => "json",
            Self::Raw => "raw",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteKind {
    Exact,
    Prefix,
    PrefixSuffix,
}

impl RouteKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Prefix => "prefix",
            Self::PrefixSuffix => "prefix_suffix",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RouteBody {
    Json(Value),
    Raw(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub ident: Option<String>,
    pub body: RouteBody,
    pub headers: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteReply {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl RouteReply {
    #[must_use]
    pub fn json(status: u16, value: &Value) -> Self {
        Self {
            status,
            content_type: "application/json".into(),
            body: crate::json::dumps(value, &crate::json::DumpOptions::default()).into_bytes(),
            headers: Vec::new(),
        }
    }

    #[must_use]
    pub fn error(status: u16, message: &str, detail: Option<&Value>) -> Self {
        let mut m = serde_json::Map::new();
        m.insert("error".into(), Value::String(message.into()));
        m.insert("detail".into(), detail.cloned().unwrap_or(Value::Null));
        Self::json(status, &Value::Object(m))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteFailure {
    Contract(String),
    Case(CaseError),
    Status(u16, String, Option<String>),
    Internal(String),
}

pub trait RouteService: Send + Sync {
    fn state_dir(&self) -> PathBuf;
    fn broadcast(&self, message: &Value);
    fn extension_state(
        &self,
        key: &str,
        factory: &dyn Fn() -> Arc<dyn Any + Send + Sync>,
    ) -> Arc<dyn Any + Send + Sync>;
    fn as_any(&self) -> &dyn Any;
}

pub type RouteHandler =
    Arc<dyn Fn(&RouteRequest, &dyn RouteService) -> Result<RouteReply, RouteFailure> + Send + Sync>;

#[derive(Clone)]
pub struct RouteDecl {
    pub method: String,
    pub pattern: String,
    pub prefix: String,
    pub capture: Option<String>,
    pub suffix: String,
    pub body: BodyPolicy,
    pub doc: String,
    pub module: String,
    pub handler: RouteHandler,
}

impl std::fmt::Debug for RouteDecl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Route {}>", self.spec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RouteTableError(pub String);

impl RouteDecl {

    pub fn new(
        method: &str,
        pattern: &str,
        body: BodyPolicy,
        doc: &str,
        module: &str,
        handler: RouteHandler,
    ) -> Result<Self, RouteTableError> {
        let (prefix, capture, suffix) = if let Some((head, rest)) = pattern.split_once('<') {
            let Some((capture, tail)) = rest.split_once('>') else {
                return Err(RouteTableError(format!(
                    "route pattern {}: exactly one <name> capture",
                    repr_str(pattern)
                )));
            };
            if capture.is_empty() || tail.contains('<') {
                return Err(RouteTableError(format!(
                    "route pattern {}: exactly one <name> capture",
                    repr_str(pattern)
                )));
            }
            (head.to_string(), Some(capture.to_string()), tail.to_string())
        } else {
            (pattern.to_string(), None, String::new())
        };
        Ok(Self {
            method: method.into(),
            pattern: pattern.into(),
            prefix,
            capture,
            suffix,
            body,
            doc: doc.into(),
            module: module.into(),
            handler,
        })
    }

    #[must_use]
    pub fn kind(&self) -> RouteKind {
        match (&self.capture, self.suffix.is_empty()) {
            (None, _) => RouteKind::Exact,
            (Some(_), true) => RouteKind::Prefix,
            (Some(_), false) => RouteKind::PrefixSuffix,
        }
    }

    #[must_use]
    pub fn spec(&self) -> String {
        format!("{} {}", self.method, self.pattern)
    }

    #[must_use]
    pub fn matches(&self, path: &str) -> Option<Option<String>> {
        if self.capture.is_none() {
            return (path == self.prefix).then_some(None);
        }
        let rest = path.strip_prefix(&self.prefix)?;
        if self.suffix.is_empty() {
            return Some(Some(rest.to_string()));
        }
        let middle = rest.strip_suffix(&self.suffix)?;
        Some(Some(middle.to_string()))
    }
}

#[derive(Clone, Debug, Default)]
pub struct RouteTable {
    routes: Vec<RouteDecl>,
}

impl RouteTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }


    pub fn add(&mut self, route: RouteDecl) -> Result<(), RouteTableError> {
        if let Some(existing) =
            self.routes.iter().find(|r| r.method == route.method && r.pattern == route.pattern)
        {
            return Err(RouteTableError(format!(
                "route {} is already registered (by {}); a registered route is never silently replaced",
                route.spec(),
                existing.module
            )));
        }
        self.routes.push(route);
        Ok(())
    }

    #[must_use]
    pub fn all(&self) -> &[RouteDecl] {
        &self.routes
    }

    #[must_use]
    pub fn endpoints(&self) -> Vec<String> {
        let mut v: Vec<String> = self.routes.iter().map(RouteDecl::spec).collect();
        v.sort();
        v
    }

    #[must_use]
    pub fn matches(&self, method: &str, path: &str) -> Option<(&RouteDecl, Option<String>)> {
        if let Some(r) = self
            .routes
            .iter()
            .find(|r| r.kind() == RouteKind::Exact && r.method == method && r.prefix == path)
        {
            return Some((r, None));
        }
        let mut patterns: Vec<&RouteDecl> =
            self.routes.iter().filter(|r| r.kind() != RouteKind::Exact).collect();
        patterns.sort_by(|a, b| {
            (!b.suffix.is_empty(), b.prefix.len()).cmp(&(!a.suffix.is_empty(), a.prefix.len()))
        });
        for r in patterns {
            if r.method != method {
                continue;
            }
            if let Some(ident) = r.matches(path) {
                return Some((r, ident));
            }
        }
        None
    }
}

pub struct RouteTableHandle(pub Arc<RouteTable>);


pub fn register_table(
    reg: &ContributionRegistry,
    key: &str,
    table: Arc<RouteTable>,
    owner_id: &str,
    active_endpoints: &BTreeSet<String>,
) -> Result<(), ContributionError> {
    let mut active = active_endpoints.clone();
    for (_, t) in contributed_tables(reg) {
        active.extend(t.endpoints());
    }
    let clash: Vec<String> = table.endpoints().into_iter().filter(|e| active.contains(e)).collect();
    if !clash.is_empty() {
        return Err(ContributionError(format!(
            "route table {} redeclares active endpoints {}",
            repr_str(key),
            list_repr(&clash)
        )));
    }
    let identity = Arc::as_ptr(&table).cast::<()>() as usize;
    reg.register(
        KIND,
        key,
        ContributionValue::with_identity(Arc::new(RouteTableHandle(table)), identity, IMPLEMENTATION),
        owner_id,
    )?;
    Ok(())
}

#[must_use]
pub fn contributed_tables(reg: &ContributionRegistry) -> Vec<(String, Arc<RouteTable>)> {
    reg.entries(KIND)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.downcast::<RouteTableHandle>().map(|h| (k, Arc::clone(&h.0))))
        .collect()
}

