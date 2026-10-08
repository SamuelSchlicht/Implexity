// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::error::{CaeError, CaeResult};
use crate::field_source_catalog::FieldSourceCatalog;
use crate::orchestration::{AddInAdapter, AddInRegistry};

pub const COMPONENT_INTERFACE: &str = "coupled_history_source";

pub trait HistorySourceComponent: Send + Sync {

    fn validate(&self, settings: &Value, context: &dyn Any) -> CaeResult<Value>;


    fn create(&self, settings: &Value, host: &dyn Any) -> CaeResult<Box<dyn Any + Send + Sync>>;


    fn coupling(&self, base: Value, settings: &Value) -> CaeResult<Value>;

    fn owns_material_forcing(&self, settings: &Value) -> bool;
}

pub struct HistorySourceInterface(pub Box<dyn HistorySourceComponent>);

pub struct SelectedSource {
    pub adapter: Arc<dyn AddInAdapter>,
}

impl SelectedSource {

    pub fn component(&self) -> CaeResult<&dyn HistorySourceComponent> {
        self.adapter
            .interface(COMPONENT_INTERFACE)
            .and_then(|i| i.downcast_ref::<HistorySourceInterface>())
            .map(|i| i.0.as_ref())
            .ok_or_else(|| CaeError::contract("incomplete coupled history-source component"))
    }
}


pub fn advertised_responses(manifests: &FieldSourceCatalog) -> CaeResult<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for row in manifests.values() {
        if row.keys().any(|k| out.contains_key(k)) {
            return Err(CaeError::contract("duplicate field-source response manifest"));
        }
        out.extend(row.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Ok(out)
}


pub fn selected(
    addins: &AddInRegistry,
    manifests: &FieldSourceCatalog,
    name: &str,
) -> CaeResult<SelectedSource> {
    let row = addins.get(name)?;
    let Some(adapter) = row.adapter.clone() else {
        return Err(CaeError::contract(format!("{name}: inactive/incompatible coupled field source")));
    };
    if adapter.component_kind().as_deref() != Some("coupled_history_source") {
        return Err(CaeError::contract(format!("{name}: inactive/incompatible coupled field source")));
    }
    if manifests.get(name) != adapter.response_units().as_ref() {
        return Err(CaeError::contract(format!("{name}: field-source response manifest mismatch")));
    }
    let source = SelectedSource { adapter };
    source.component()?;
    Ok(source)
}

#[derive(Debug, Clone, PartialEq)]
pub struct SourceDeclaration {
    pub component: String,
    pub settings: Value,
}

impl SourceDeclaration {
    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({"component": self.component, "settings": self.settings})
    }
}


pub fn declarations(
    addins: &AddInRegistry,
    manifests: &FieldSourceCatalog,
    rows: Option<&Value>,
    context: &dyn Any,
) -> CaeResult<Vec<SourceDeclaration>> {
    let Some(rows) = rows.filter(|r| !r.is_null()) else { return Ok(Vec::new()) };
    let Some(rows) = rows.as_array() else {
        return Err(CaeError::contract("field_sources must be an explicit list"));
    };
    let mut out = Vec::new();
    let mut ids = BTreeSet::new();
    let mut forcing_owner = false;
    for row in rows {
        let Some(r) = row.as_object().filter(|r| {
            r.len() == 2 && r.contains_key("settings") && r.get("component").is_some_and(Value::is_string)
        }) else {
            return Err(CaeError::contract("field source requires component and settings"));
        };
        let name = r["component"].as_str().unwrap_or_default().to_string();
        let source = selected(addins, manifests, &name)?;
        if ids.contains(&name) {
            return Err(CaeError::contract("duplicate field source instance"));
        }
        let component = source.component()?;
        let config = component.validate(&r["settings"], context)?;
        let takes = component.owns_material_forcing(&config);
        if takes && forcing_owner {
            return Err(CaeError::contract("multiple fields cannot replace the same material forcing port"));
        }
        forcing_owner |= takes;
        ids.insert(name.clone());
        out.push(SourceDeclaration { component: name, settings: config });
    }
    Ok(out)
}


pub fn response_units(
    addins: &AddInRegistry,
    manifests: &FieldSourceCatalog,
    rows: Option<&Value>,
    context: &dyn Any,
) -> CaeResult<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for row in declarations(addins, manifests, rows, context)? {
        let source = selected(addins, manifests, &row.component)?;
        out.extend(source.adapter.response_units().unwrap_or_default());
    }
    Ok(out)
}

pub struct BoundSource {
    pub component: String,
    pub settings: Value,
    pub owns_material_forcing: bool,
    pub response_units: BTreeMap<String, String>,
    pub state_contract: Option<String>,
    pub source: Box<dyn Any + Send + Sync>,
}


pub fn bind(
    addins: &AddInRegistry,
    manifests: &FieldSourceCatalog,
    rows: Option<&Value>,
    context: &dyn Any,
    host: &dyn Any,
) -> CaeResult<Vec<BoundSource>> {
    let mut out = Vec::new();
    for row in declarations(addins, manifests, rows, context)? {
        let source = selected(addins, manifests, &row.component)?;
        let component = source.component()?;
        let bound = component.create(&row.settings, host)?;
        out.push(BoundSource {
            owns_material_forcing: component.owns_material_forcing(&row.settings),
            response_units: source.adapter.response_units().unwrap_or_default(),
            state_contract: source.adapter.state_contract(),
            component: row.component,
            settings: row.settings,
            source: bound,
        });
    }
    Ok(out)
}


pub fn with_source_couplings(
    addins: &AddInRegistry,
    manifests: &FieldSourceCatalog,
    base: Value,
    rows: Option<&Value>,
    context: &dyn Any,
) -> CaeResult<Value> {
    let mut base = base;
    for row in declarations(addins, manifests, rows, context)? {
        let source = selected(addins, manifests, &row.component)?;
        base = source.component()?.coupling(base, &row.settings)?;
    }
    Ok(base)
}
