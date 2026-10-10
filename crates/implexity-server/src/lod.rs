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

pub const FEATURE_PERIOD_MM_MIN: f64 = 1.16;
pub const FEATURE_PERIOD_MM_MED: f64 = 1.84;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lod {
    pub name: &'static str,
    pub samples_per_feature: f64,
    pub geff_stride: i64,
    pub rdp_tol_frac: f64,
    pub slab_samples: i64,
}

#[must_use]
pub fn py_round(x: f64, ndigits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.ndigits$}").parse().unwrap_or(x)
}

impl Lod {
    #[must_use]
    pub fn h_mm(&self) -> f64 {
        FEATURE_PERIOD_MM_MIN / self.samples_per_feature
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({
            "name": self.name,
            "h_mm": py_round(self.h_mm(), 4),
            "samples_per_feature": self.samples_per_feature,
            "geff_stride": self.geff_stride,
            "slab_samples": self.slab_samples,
        })
    }
}

pub const LEVELS: [Lod; 5] = [
    Lod { name: "drag", samples_per_feature: 3.0, geff_stride: 3, rdp_tol_frac: 0.35, slab_samples: 48 },
    Lod { name: "move", samples_per_feature: 4.0, geff_stride: 3, rdp_tol_frac: 0.30, slab_samples: 56 },
    Lod { name: "settle", samples_per_feature: 5.0, geff_stride: 2, rdp_tol_frac: 0.25, slab_samples: 72 },
    Lod { name: "read", samples_per_feature: 8.0, geff_stride: 2, rdp_tol_frac: 0.20, slab_samples: 96 },
    Lod { name: "exact", samples_per_feature: 8.0, geff_stride: 1, rdp_tol_frac: 0.10, slab_samples: 96 },
];

pub const ORDER: [&str; 5] = ["drag", "move", "settle", "read", "exact"];

pub const DEFAULT: &str = "settle";

fn index_of(name: &str) -> Option<usize> {
    ORDER.iter().position(|n| *n == name)
}

#[must_use]
pub fn get(name: &str) -> &'static Lod {
    let i = index_of(name).or_else(|| index_of(DEFAULT)).unwrap_or(2);
    &LEVELS[i]
}

#[must_use]
pub fn levels_json() -> Value {
    Value::Array(LEVELS.iter().map(Lod::as_dict).collect())
}

#[derive(Clone, Debug, PartialEq)]
pub struct Budget {
    sps: BTreeMap<i64, f64>,
    alpha: f64,
    samples: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownLevel(pub String);

impl std::fmt::Display for UnknownLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} is not in list", self.0)
    }
}

impl std::error::Error for UnknownLevel {}

impl Budget {
    #[must_use]
    pub fn new(initial_sps: Option<BTreeMap<i64, f64>>) -> Self {
        let sps = initial_sps.unwrap_or_else(|| {
            [(1, 2.0e4), (2, 7.5e4), (3, 1.6e5), (4, 2.4e5), (6, 3.5e5)].into_iter().collect()
        });
        Self { sps, alpha: 0.3, samples: 0 }
    }

    pub fn observe(&mut self, stride: i64, n_core: f64, seconds: f64) {
        if seconds <= 0.0 || n_core <= 0.0 {
            return;
        }
        let obs = n_core / seconds;
        let cur = self.sps.get(&stride).copied().unwrap_or(obs);
        self.sps.insert(stride, (1.0 - self.alpha) * cur + self.alpha * obs);
        self.samples += 1;
    }

    #[must_use]
    pub fn predict_ms(&self, stride: i64, n_core: f64) -> f64 {
        1000.0 * n_core / self.sps.get(&stride).copied().unwrap_or(2.0e4).max(1.0)
    }



    pub fn choose(
        &self,
        extent_mm: &[f64],
        target_ms: f64,
        thickness_samples: i64,
        floor: &str,
        ceiling: &str,
    ) -> Result<&'static Lod, UnknownLevel> {
        let lo = index_of(floor).ok_or_else(|| UnknownLevel(floor.to_owned()))?;
        let hi = index_of(ceiling).ok_or_else(|| UnknownLevel(ceiling.to_owned()))?;
        let mut best = lo;
        for (i, level) in LEVELS.iter().enumerate().take(hi + 1).skip(lo) {
            let mut n = 1.0_f64;
            for &e in extent_mm {
                n *= ((e / level.h_mm()).round_ties_even() + 1.0).max(2.0);
            }
            n *= thickness_samples.max(1) as f64;
            if self.predict_ms(level.geff_stride, n) <= target_ms {
                best = i;
            } else {
                break;
            }
        }
        Ok(&LEVELS[best])
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        let by_stride: Map<String, Value> =
            self.sps.iter().map(|(k, v)| (k.to_string(), json!(py_round(*v, 1)))).collect();
        json!({"samples_per_second_by_stride": by_stride, "observations": self.samples})
    }
}

