// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::any::Any;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use implexity_core::history_field_sources::{HistorySourceComponent, HistorySourceInterface};
use implexity_core::orchestration::AddInAdapter;

use crate::host::FieldSourceAuthoring;

pub struct FieldSourceAdapter {
    implementation: &'static str,
    data: Value,
    interface: HistorySourceInterface,
    authoring: Box<dyn FieldSourceAuthoring>,
}

impl FieldSourceAdapter {
    #[must_use]
    pub fn new<C>(implementation: &'static str, data: Value, component: C) -> Self
    where
        C: HistorySourceComponent + FieldSourceAuthoring + Clone + 'static,
    {
        Self {
            implementation,
            data,
            interface: HistorySourceInterface(Box::new(component.clone())),
            authoring: Box::new(component),
        }
    }

    #[must_use]
    pub fn authoring(&self) -> &dyn FieldSourceAuthoring {
        self.authoring.as_ref()
    }

    #[must_use]
    pub fn editor_label(&self) -> Option<&str> {
        self.data.get("editor_label").and_then(Value::as_str)
    }
}

impl AddInAdapter for FieldSourceAdapter {
    fn implementation(&self) -> String {
        self.implementation.into()
    }

    fn runtime_support(&self) -> Option<Map<String, Value>> {
        self.data["runtime_support"].as_object().cloned()
    }

    fn component_kind(&self) -> Option<String> {
        Some("coupled_history_source".into())
    }

    fn authoring_contract(&self) -> Option<Map<String, Value>> {
        self.data["authoring_contract"].as_object().cloned()
    }

    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        self.data["response_units"]
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect())
    }

    fn state_contract(&self) -> Option<String> {
        self.data["state_contract"].as_str().map(str::to_string)
    }

    fn interface(&self, name: &str) -> Option<&(dyn Any + Send + Sync)> {
        match name {
            implexity_core::history_field_sources::COMPONENT_INTERFACE => Some(&self.interface),
            crate::host::AUTHORING_INTERFACE => Some(self),
            _ => None,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
