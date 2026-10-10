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
