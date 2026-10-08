// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use implexity_core::CaeError;

use crate::util::{contract, obj, text};

pub const RESPONSE_UNITS: [(&str, &str); 3] = [
    ("material_stored_energy_J", "J"),
    ("material_conductivity_mean_W_mK", "W/(m K)"),
    ("material_yield_mean_Pa", "Pa"),
];
pub const REQUIRES: [&str; 4] = [
    "material_stored_energy_J_m3",
    "material_conductivity_W_mK",
    "material_yield_stress_Pa",
    "solid_cell_volume_m3",
];
pub const IMPLEMENTATION: &str = "implexity.physics_library.material_history_monitor.MaterialHistoryMonitor";
pub const COMPONENT_KIND: &str = "history_response_observer";
pub const LIMITATIONS: [&str; 1] = [
    "Solid-volume-weighted final properties and stored energy; not fatigue lifetime, melting or irradiation safety margins.",
];

#[must_use]
pub fn runtime_support() -> Map<String, Value> {
    obj(json!({"status": "field_component", "history": true, "data": "selected evolving constitutive state",
        "limitations": LIMITATIONS}))
}

#[must_use]
pub fn response_units() -> BTreeMap<String, String> {
    RESPONSE_UNITS.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}


pub fn validate(p: &Value) -> Result<Value, CaeError> {
    if !crate::util::has_exact_keys(p, &["provenance"]) || !text(&p["provenance"]) {
        return contract("material history monitor requires explicit provenance");
    }
    Ok(p.clone())
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MonitorSample {
    pub stored_energy: Vec<f64>,
    pub conductivity: Vec<f64>,
    pub yield_stress: Vec<f64>,
    pub volume: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundMonitor {
    pub settings: Value,
}

impl BoundMonitor {
    #[must_use]
    pub fn new(settings: Value) -> Self {
        Self { settings }
    }

    #[must_use]
    pub fn field_metadata(&self) -> Value {
        let mut m = Map::new();
        for (name, unit) in [
            ("evolved_material_stored_energy_J_m3", "J/m^3"),
            ("evolved_material_conductivity_W_mK", "W/(m K)"),
            ("evolved_material_yield_stress_Pa", "Pa"),
        ] {
            m.insert(
                name.into(),
                json!({"units": unit, "association": "cell", "rank": "scalar", "phase_mask": "solid_reporting_mask",
                    "source": "current_solved_constitutive_history_T4_volume_average"}),
            );
        }
        Value::Object(m)
    }

    #[must_use]
    pub fn values(&self, s: &MonitorSample) -> [f64; 3] {
        let den: f64 = s.volume.iter().sum();
        let dot = |a: &[f64]| s.volume.iter().zip(a).map(|(w, v)| w * v).sum::<f64>();
        [dot(&s.stored_energy), dot(&s.conductivity) / den, dot(&s.yield_stress) / den]
    }


    pub fn check(&self, samples: &[MonitorSample]) -> Result<Value, CaeError> {
        let mut rows = Vec::new();
        for (i, s) in samples.iter().enumerate() {
            let n = s.volume.len();
            let fields = [&s.stored_energy, &s.conductivity, &s.yield_stress, &s.volume];
            if fields.iter().any(|f| f.len() != n || f.iter().any(|v| !v.is_finite()))
                || s.volume.iter().any(|w| *w < 0.0)
                || s.volume.iter().sum::<f64>() <= 0.0
            {
                return contract("invalid sampled material-point fields/volumes");
            }
            if s.conductivity.iter().any(|v| *v <= 0.0) || s.yield_stress.iter().any(|v| *v <= 0.0) {
                return contract("evolved conductivity/yield must remain positive");
            }
            let stored: f64 = s.volume.iter().zip(&s.stored_energy).map(|(w, e)| w * e).sum();
            rows.push(json!({"sample": i, "material_stored_energy_J": stored}));
        }
        Ok(json!({"component": "material_history_monitor", "samples": rows, "qualification": false}))
    }

    #[must_use]
    pub fn fields(&self, s: &MonitorSample) -> BTreeMap<String, Vec<f64>> {
        BTreeMap::from([
            ("evolved_material_stored_energy_J_m3".to_string(), s.stored_energy.clone()),
            ("evolved_material_conductivity_W_mK".to_string(), s.conductivity.clone()),
            ("evolved_material_yield_stress_Pa".to_string(), s.yield_stress.clone()),
        ])
    }
}
