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
