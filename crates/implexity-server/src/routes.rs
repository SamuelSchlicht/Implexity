// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::http::{Reply, Request, RouteError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

pub type Handler = Arc<dyn Fn(&Request) -> Result<Reply, RouteError> + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDeclarationError(pub String);

impl fmt::Display for RouteDeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RouteDeclarationError {}

#[derive(Clone)]
pub struct Route {
    method: String,
    pattern: String,
    prefix: String,
    capture: Option<String>,
    suffix: String,
    body: BodyPolicy,
    doc: String,
    module: String,
    handler: Handler,
}

impl fmt::Debug for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<Route {}>", self.spec())
    }
}

impl Route {



    pub fn new(
        method: &str,
        pattern: &str,
        body: BodyPolicy,
        doc: &str,
        module: &str,
        handler: Handler,
    ) -> Result<Self, RouteDeclarationError> {
        let (prefix, capture, suffix) = if let Some((head, rest)) = pattern.split_once('<') {
            let (capture, tail) = rest.split_once('>').unwrap_or((rest, ""));
            if capture.is_empty() || !rest.contains('>') || tail.contains('<') {
                return Err(RouteDeclarationError(format!(
                    "route pattern {pattern:?}: exactly one <name> capture"
                )));
            }
            (head.to_owned(), Some(capture.to_owned()), tail.to_owned())
        } else {
            (pattern.to_owned(), None, String::new())
        };
        Ok(Self {
            method: method.to_owned(),
            pattern: pattern.to_owned(),
            prefix,
            capture,
            suffix,
            body,
            doc: doc.to_owned(),
            module: module.to_owned(),
            handler,
        })
    }

    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    #[must_use]
    pub fn capture(&self) -> Option<&str> {
        self.capture.as_deref()
    }

    #[must_use]
    pub fn body(&self) -> BodyPolicy {
        self.body
    }

    #[must_use]
    pub fn doc(&self) -> &str {
        &self.doc
    }

    #[must_use]
    pub fn module(&self) -> &str {
        &self.module
    }

    #[must_use]
    pub fn handler(&self) -> &Handler {
        &self.handler
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
    pub fn matches<'p>(&self, path: &'p str) -> Option<Option<&'p str>> {
        if self.capture.is_none() {
            return (path == self.prefix).then_some(None);
        }
        let rest = path.strip_prefix(self.prefix.as_str())?;
        if self.suffix.is_empty() {
            return Some(Some(rest));
        }
        if !path.ends_with(self.suffix.as_str()) {
            return None;
        }

        let end = path.len() - self.suffix.len();
        let start = self.prefix.len();
        Some(Some(if end > start { &path[start..end] } else { "" }))
    }

    fn specificity(&self) -> (bool, usize) {
        (!self.suffix.is_empty(), self.prefix.chars().count())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    exact: BTreeMap<(String, String), usize>,
    by_pattern: BTreeMap<(String, String), usize>,
    patterns: Vec<usize>,
    all: Vec<Route>,
}

#[derive(Clone, Debug)]
pub struct RouteMatch {
    pub route: Route,
    pub ident: Option<String>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }



    pub fn add(&mut self, route: Route) -> Result<(), RouteDeclarationError> {
        let key = (route.method.clone(), route.pattern.clone());
        if let Some(&i) = self.by_pattern.get(&key) {
            return Err(RouteDeclarationError(format!(
                "route {} is already registered (by {}); a registered route is never silently replaced",
                route.spec(),
                self.all[i].module
            )));
        }
        let index = self.all.len();
        self.by_pattern.insert(key, index);
        if route.kind() == RouteKind::Exact {
            self.exact.insert((route.method.clone(), route.prefix.clone()), index);
        } else {
            self.patterns.push(index);
        }
        self.all.push(route);
        let all = &self.all;

        self.patterns.sort_by(|&a, &b| all[b].specificity().cmp(&all[a].specificity()));
        Ok(())
    }



    pub fn replace(&mut self, route: Route) -> Result<Route, RouteDeclarationError> {
        let key = (route.method.clone(), route.pattern.clone());
        let Some(&i) = self.by_pattern.get(&key) else {
            return Err(RouteDeclarationError(format!("route {} is not registered", route.spec())));
        };
        Ok(std::mem::replace(&mut self.all[i], route))
    }



    pub fn route(
        &mut self,
        method: &str,
        pattern: &str,
        body: BodyPolicy,
        doc: &str,
        module: &str,
        handler: Handler,
    ) -> Result<(), RouteDeclarationError> {
        self.add(Route::new(method, pattern, body, doc, module, handler)?)
    }

    #[must_use]
    pub fn find(&self, method: &str, path: &str) -> Option<RouteMatch> {
        if let Some(&i) = self.exact.get(&(method.to_owned(), path.to_owned())) {
            return Some(RouteMatch { route: self.all[i].clone(), ident: None });
        }
        for &i in &self.patterns {
            let r = &self.all[i];
            if r.method != method {
                continue;
            }
            if let Some(ident) = r.matches(path) {
                return Some(RouteMatch { route: r.clone(), ident: ident.map(str::to_owned) });
            }
        }
        None
    }

    #[must_use]
    pub fn all(&self) -> &[Route] {
        &self.all
    }

    #[must_use]
    pub fn endpoints(&self) -> Vec<String> {
        let mut out: Vec<String> = self.all.iter().map(Route::spec).collect();
        out.sort();
        out
    }

    #[must_use]
    pub fn patterns_for(&self, method: &str) -> Vec<String> {
        self.all.iter().filter(|r| r.method == method).map(|r| r.pattern.clone()).collect()
    }

    #[must_use]
    pub fn kinds(&self) -> BTreeMap<String, RouteKind> {
        self.all.iter().map(|r| (r.spec(), r.kind())).collect()
    }
}

#[derive(Clone)]
pub struct RouteGate(Arc<dyn Fn() -> bool + Send + Sync>);

impl RouteGate {
    pub fn new(open: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(open))
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        (self.0)()
    }
}

impl fmt::Debug for RouteGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RouteGate")
    }
}

#[derive(Clone, Debug, Default)]
pub struct RouteTables {
    kernel: Registry,
    pending: std::collections::BTreeSet<String>,
    contributed: Vec<ContributedTable>,
}

#[derive(Clone, Debug)]
pub struct ContributedTable {
    pub key: String,
    pub owner_id: String,
    pub table: Registry,
    pub gate: Option<RouteGate>,
}

impl RouteTables {
    #[must_use]
    pub fn new(kernel: Registry) -> Self {
        Self::with_pending(kernel, std::collections::BTreeSet::new())
    }

    #[must_use]
    pub fn with_pending(kernel: Registry, pending: std::collections::BTreeSet<String>) -> Self {
        let declared: std::collections::BTreeSet<String> = kernel.endpoints().into_iter().collect();
        let pending = pending.into_iter().filter(|s| declared.contains(s)).collect();
        Self { kernel, pending, contributed: Vec::new() }
    }

    #[must_use]
    pub fn pending(&self) -> Vec<String> {
        self.pending.iter().cloned().collect()
    }



    pub fn install_kernel_route(&mut self, route: Route) -> Result<(), RouteDeclarationError> {
        let spec = route.spec();
        if self.pending.remove(&spec) {
            self.kernel.replace(route)?;
            return Ok(());
        }
        if self.contributed.iter().any(|c| c.table.endpoints().contains(&spec)) {
            return Err(RouteDeclarationError(format!(
                "route {spec} is declared by a contributed table and cannot become a kernel route"
            )));
        }
        self.kernel.add(route)
    }

    #[must_use]
    pub fn kernel(&self) -> &Registry {
        &self.kernel
    }

    #[must_use]
    pub fn contributed(&self) -> &[ContributedTable] {
        &self.contributed
    }

    fn tables(&self) -> impl Iterator<Item = &Registry> {
        std::iter::once(&self.kernel).chain(
            self.contributed
                .iter()
                .filter(|c| c.gate.as_ref().is_none_or(RouteGate::is_open))
                .map(|c| &c.table),
        )
    }

    fn reserved(&self) -> std::collections::BTreeSet<String> {
        std::iter::once(&self.kernel)
            .chain(self.contributed.iter().map(|c| &c.table))
            .flat_map(Registry::endpoints)
            .collect()
    }

    #[must_use]
    pub fn find(&self, method: &str, path: &str) -> Option<RouteMatch> {
        self.tables().find_map(|t| t.find(method, path))
    }

    #[must_use]
    pub fn patterns_for(&self, method: &str) -> Vec<String> {
        self.tables().flat_map(|t| t.patterns_for(method)).collect()
    }

    #[must_use]
    pub fn endpoints(&self) -> Vec<String> {
        let mut out: Vec<String> = self.tables().flat_map(Registry::endpoints).collect();
        out.sort();
        out
    }

    #[must_use]
    pub fn routes(&self) -> Vec<Route> {
        self.tables().flat_map(|t| t.all().iter().cloned()).collect()
    }



    pub fn register_table(
        &mut self,
        key: &str,
        table: Registry,
        owner_id: &str,
    ) -> Result<(), RouteDeclarationError> {
        if key.is_empty() || owner_id.is_empty() {
            return Err(RouteDeclarationError(
                "a contributed route table needs a key and an owner".to_owned(),
            ));
        }
        if self.contributed.iter().any(|c| c.key == key) {
            return Err(RouteDeclarationError(format!("route table {key:?} is already contributed")));
        }
        let active = self.reserved();
        let clash: Vec<String> = table.endpoints().into_iter().filter(|s| active.contains(s)).collect();
        if !clash.is_empty() {
            return Err(RouteDeclarationError(format!(
                "route table {key:?} redeclares active endpoints {clash:?}"
            )));
        }
        self.contributed.push(ContributedTable {
            key: key.to_owned(),
            owner_id: owner_id.to_owned(),
            table,
            gate: None,
        });
        Ok(())
    }



    pub fn register_gated_table(
        &mut self,
        key: &str,
        table: Registry,
        owner_id: &str,
        gate: RouteGate,
    ) -> Result<(), RouteDeclarationError> {
        self.register_table(key, table, owner_id)?;
        if let Some(last) = self.contributed.last_mut() {
            last.gate = Some(gate);
        }
        Ok(())
    }



    pub fn unregister_table(&mut self, key: &str, owner_id: &str) -> Result<(), RouteDeclarationError> {
        let Some(i) = self.contributed.iter().position(|c| c.key == key) else {
            return Err(RouteDeclarationError(format!("no contributed route table {key:?}")));
        };
        if self.contributed[i].owner_id != owner_id {
            return Err(RouteDeclarationError(format!(
                "route table {key:?} is owned by {}, not {owner_id}",
                self.contributed[i].owner_id
            )));
        }
        self.contributed.remove(i);
        Ok(())
    }
}

#[must_use]
pub fn prose_list(items: &[String]) -> String {
    match items {
        [] => "nothing".to_owned(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

