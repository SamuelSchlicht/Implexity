// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use implexity_core::json::canonical;

#[derive(Debug, Clone, PartialEq)]
pub enum ReportValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl ReportValue {
    fn truthy(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::Int(i) => *i != 0,
            Self::Float(f) => *f != 0.0,
            Self::Text(s) => !s.is_empty(),
        }
    }

    fn evidence(&self) -> Value {
        match self {
            Self::Bool(b) => json!(b),
            Self::Int(i) => json!(i),
            Self::Float(f) if f.is_finite() => json!(f),
            Self::Float(f) => json!(if f.is_nan() {
                "nan"
            } else if *f > 0.0 {
                "inf"
            } else {
                "-inf"
            }),
            Self::Text(s) => json!(s),
        }
    }
}

pub type AdmissionReport = BTreeMap<String, ReportValue>;

const REPORT_KEYS: [&str; 15] = [
    "maximum_hydraulic_reynolds",
    "maximum_local_cell_reynolds",
    "maximum_cell_peclet",
    "legacy_reference_maximum_reynolds",
    "maximum_local_cell_reynolds_limit",
    "maximum_cell_peclet_limit",
    "max_cell_mass_balance_m3_s",
    "mass_balance_tolerance_m3_s",
    "physical_fluid_pressure_absolute_min_Pa",
    "all_cell_numerical_pressure_absolute_min_Pa",
    "hydraulic_diameter_m",
    "cell_characteristic_length_m",
    "fluid_regime_admission_cell_scope",
    "positive_physical_fluid_cell_count",
    "local_cell_reynolds_screen_applied",
];

#[must_use]
pub fn fluid_admission_failure_message(
    report: &AdmissionReport,
    history_step: i64,
    transport: &str,
) -> String {
    let temperature_key = if report.contains_key("temperature_evaluation_domain_screen_passed") {
        "temperature_evaluation_domain_screen_passed"
    } else {
        "temperature_material_interval_screen_passed"
    };
    let admission = [
        "finite_state_screen_passed",
        temperature_key,
        "positive_absolute_pressure_screen_passed",
        "mass_conservation_screen_passed",
    ];
    let reynolds_key = if report.get("local_cell_reynolds_screen_applied").is_some_and(ReportValue::truthy) {
        "local_cell_reynolds_screen_passed"
    } else {
        "legacy_reference_passed"
    };
    let regime = ["cell_peclet_screen_passed", reynolds_key];

    let policy = match report.get("regime_screen_policy") {
        Some(ReportValue::Text(s)) => s.clone(),
        Some(other) => match other.evidence() {
            Value::String(s) => s,
            v => v.to_string(),
        },
        None => "reject".into(),
    };
    let screens: Vec<&str> = admission
        .iter()
        .copied()
        .chain(if policy == "reject" { regime.to_vec() } else { Vec::new() })
        .collect();
    let mut values = Map::new();
    for key in admission.iter().chain(regime.iter()).copied().chain(REPORT_KEYS) {
        if let Some(v) = report.get(key) {
            values.insert(key.into(), v.evidence());
        }
    }
    let failed = |keys: &[&str]| -> Vec<Value> {
        keys.iter().filter(|k| report.get(**k).is_some_and(|v| !v.truthy())).map(|k| json!(k)).collect()
    };
    let evidence = json!({
        "schema": "implexity-fluid-admission-failure/1",
        "diagnostic_only": true,
        "history_step": history_step,
        "temperature_transport": transport,
        "regime_screen_policy": policy,
        "failed_screens": failed(&screens),
        "failed_reported_regime_screens": if policy == "reject" { Vec::new() } else { failed(&regime) },
        "report": Value::Object(values),
    });
    format!("fluid failed numerical admission: {}", canonical(&evidence))
}

#[must_use]
pub fn admission_report(value: &Value) -> AdmissionReport {
    let mut out = AdmissionReport::new();
    for (key, v) in value.as_object().into_iter().flatten() {
        let item = match v {
            Value::Bool(b) => ReportValue::Bool(*b),
            Value::Number(n) if n.is_i64() => ReportValue::Int(n.as_i64().unwrap_or(0)),
            Value::Number(n) => ReportValue::Float(n.as_f64().unwrap_or(f64::NAN)),
            Value::String(s) => ReportValue::Text(s.clone()),
            _ => continue,
        };
        out.insert(key.clone(), item);
    }
    out
}
