// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_core::orchestration::{
    AddInAdapter, AddInCategory, AddInContract, ContractInput, Fidelity, PortSpec,
};
use implexity_core::packages::InstallContext;

use crate::region_temperature_extrema::step;

pub const IMPLEMENTATION: &str = "implexity_rust_extension.cyclic_plastic_strain";
pub const COMPONENT_ID: &str = "cyclic_plastic_strain";
pub const PLASTIC_STRAIN_SAMPLE: &str = "solid_cell_equivalent_plastic_strain";
pub const CELL_REGION_SAMPLE: &str = "cell_region_fractions";
pub const REQUIRES: [&str; 2] = [PLASTIC_STRAIN_SAMPLE, CELL_REGION_SAMPLE];
pub const REGIONS: [&str; 2] = ["endmember_0", "endmember_1"];
pub const FATIGUE_KEY: &str = "fatigue";

const LIMITATIONS: [&str; 5] = [
    "Accumulated equivalent plastic strain per resolved cycle from the host's constitutive state; cell means of the six tetrahedra, no stress-concentration factor.",
    "Fatigue usage is Miner's rule on a Coffin-Manson life of the resolved cycle's plastic strain range: no elastic (Basquin) term, mean-stress, creep-fatigue or irradiation effect; the authored coefficients and their provenance are the study's statement.",
    "The usage extrapolates the resolved cycle to the service cycle count (cycle jump); it presumes shakedown or a stabilised loop.",
    "Region memberships from the cell occupancy and phase; a diffuse bond is represented to one cell.",
    "Weighted log-sum-exp is a differentiable surrogate; it can underestimate the literal maximum for partial memberships.",
];

#[must_use]
pub fn response_names() -> Vec<String> {
    let strain = REGIONS
        .iter()
        .flat_map(|r| [format!("cycle_plastic_strain_{r}_bound"), format!("cycle_plastic_strain_{r}_max")]);
    let usage = REGIONS
        .iter()
        .flat_map(|r| [format!("cycle_fatigue_usage_{r}_bound"), format!("cycle_fatigue_usage_{r}_max")]);
    strain.chain(usage).collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoffinManson {
    pub ductility_coefficient: f64,
    pub exponent: f64,
}

impl CoffinManson {
    pub fn usage<S: Scalar>(&self, dp: S, cycles: f64) -> S {
        if dp.value() <= 0.0 {
            return S::zero();
        }
        (dp * (0.25 / self.ductility_coefficient)).powf(-1.0 / self.exponent) * (2.0 * cycles)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FatigueSettings {
    pub service_cycles: f64,
    pub coffin_manson: [CoffinManson; 2],
    pub provenance: String,
}

fn fatigue(value: &Value) -> Option<FatigueSettings> {
    let f = value.as_object().filter(|f| f.len() == 3)?;
    let number =
        |v: Option<&Value>| v.filter(|v| v.is_number()).and_then(Value::as_f64).filter(Scalar::is_finite);
    let service_cycles = number(f.get("service_cycles")).filter(|n| *n > 0.0)?;
    let table = f.get("coffin_manson")?.as_object().filter(|t| t.len() == REGIONS.len())?;
    let mut coffin_manson = [CoffinManson { ductility_coefficient: 0.0, exponent: 0.0 }; 2];
    for (slot, region) in coffin_manson.iter_mut().zip(REGIONS) {
        let row = table.get(region)?.as_object().filter(|r| r.len() == 2)?;
        *slot = CoffinManson {
            ductility_coefficient: number(row.get("ductility_coefficient")).filter(|e| *e > 0.0)?,
            exponent: number(row.get("exponent")).filter(|c| *c > -1.0 && *c < 0.0)?,
        };
    }
    let provenance = f.get("provenance")?.as_str().filter(|p| !p.trim().is_empty())?.to_string();
    Some(FatigueSettings { service_cycles, coffin_manson, provenance })
}

#[derive(Debug, Clone, PartialEq)]
pub struct CyclicSettings {
    pub start: usize,
    pub end: usize,
    pub width: f64,
    pub membership_threshold: f64,
    pub membership_half_width: f64,
    pub fatigue: FatigueSettings,
    pub provenance: String,
    raw: Value,
}

impl CyclicSettings {
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}


pub fn validate(settings: &Value) -> Result<CyclicSettings, CaeError> {
    let err = || {
        CaeError::contract(
            "cyclic plastic strain requires integer cycle_start_state < cycle_end_state, positive width, membership_threshold in (0, 1), membership_half_width in (0, min(threshold, 1 - threshold)], provenance and fatigue {service_cycles > 0, coffin_manson {endmember_0, endmember_1: {ductility_coefficient > 0, exponent in (-1, 0)}}, provenance}",
        )
    };
    let s = settings.as_object().filter(|s| s.len() == 7).ok_or_else(err)?;
    let index = |k: &str| s.get(k).and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok());
    let number =
        |k: &str| s.get(k).filter(|v| v.is_number()).and_then(Value::as_f64).filter(Scalar::is_finite);
    let start = index("cycle_start_state").ok_or_else(err)?;
    let end = index("cycle_end_state").filter(|e| *e > start).ok_or_else(err)?;
    let width = number("width").filter(|w| *w > 0.0).ok_or_else(err)?;
    let eta = number("membership_threshold").filter(|v| *v > 0.0 && *v < 1.0).ok_or_else(err)?;
    let delta =
        number("membership_half_width").filter(|v| *v > 0.0 && *v <= eta.min(1.0 - eta)).ok_or_else(err)?;
    let provenance = s
        .get("provenance")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(err)?
        .to_string();
    let fatigue = s.get(FATIGUE_KEY).and_then(fatigue).ok_or_else(err)?;
    Ok(CyclicSettings {
        start,
        end,
        width,
        membership_threshold: eta,
        membership_half_width: delta,
        fatigue,
        provenance,
        raw: settings.clone(),
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CyclicPlasticStrain;

impl CyclicPlasticStrain {
    pub const COMPONENT_KIND: &'static str = "history_response_observer";

    #[must_use]
    pub fn response_units() -> Vec<(String, String)> {
        response_names().into_iter().map(|r| (r, "1".to_string())).collect()
    }


    pub fn bind(&self, settings: &Value) -> Result<BoundCyclic, CaeError> {
        Ok(BoundCyclic { settings: validate(settings)? })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundCyclic {
    pub settings: CyclicSettings,
}

impl BoundCyclic {

    pub fn selected_states(&self, len: usize) -> Result<[usize; 2], CaeError> {
        if self.settings.end >= len {
            return Err(CaeError::contract(format!(
                "cyclic plastic strain: cycle_end_state {} is outside the {len} stored states",
                self.settings.end
            )));
        }
        Ok([self.settings.start, self.settings.end])
    }


    pub fn values<S: Scalar>(&self, plastic: [&[S]; 2], fractions: &[S]) -> Result<Vec<S>, CaeError> {
        let [a, b] = plastic;
        if a.len() != b.len() || fractions.len() != 3 * a.len() {
            return Err(CaeError::contract("cyclic plastic strain samples disagree in size"));
        }
        let (eta, delta, w) =
            (self.settings.membership_threshold, self.settings.membership_half_width, self.settings.width);
        let mut out = Vec::with_capacity(8);
        for (r, name) in REGIONS.iter().enumerate() {
            let rows: Vec<(S, S)> = (0..a.len())
                .map(|c| (b[c] - a[c], step(fractions[3 * c + r], eta, delta)))
                .filter(|(_, m)| m.value() > 0.0)
                .collect();
            if rows.is_empty() {
                return Err(CaeError::contract(format!(
                    "cyclic plastic strain: region {name} has no member cell"
                )));
            }
            let shift = rows.iter().fold(f64::NEG_INFINITY, |acc, (d, _)| acc.max(d.value()));
            let mut total = S::zero();
            for (d, m) in &rows {
                total += *m * ((*d - shift) * (1.0 / w)).exp();
            }
            out.push(total.ln() * w + shift);
            let strong: Vec<S> = rows.iter().filter(|(_, m)| m.value() >= 0.5).map(|(d, _)| *d).collect();
            let pool: Vec<S> =
                if strong.is_empty() { rows.iter().map(|(d, _)| *d).collect() } else { strong };
            let top = pool.iter().fold(f64::NEG_INFINITY, |acc, d| acc.max(d.value()));
            #[allow(clippy::float_cmp)]                                                    
            let ties = pool.iter().filter(|d| d.value() == top).count().max(1);
            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            let grad: Vec<f64> =
                pool.iter().map(|d| if d.value() == top { 1.0 / ties as f64 } else { 0.0 }).collect();
            out.push(S::lift(top, &pool, &grad, &[]));
        }
        let fatigue = &self.settings.fatigue;
        let usage: Vec<S> = (0..REGIONS.len())
            .flat_map(|r| {
                let law = fatigue.coffin_manson[r];
                [
                    law.usage(out[2 * r], fatigue.service_cycles),
                    law.usage(out[2 * r + 1], fatigue.service_cycles),
                ]
            })
            .collect();
        out.extend(usage);
        Ok(out)
    }


    pub fn check(&self, plastic: [&[f64]; 2], fractions: &[f64]) -> Result<Value, CaeError> {
        let [a, b] = plastic;
        if a.is_empty()
            || a.len() != b.len()
            || fractions.len() != 3 * a.len()
            || !a.iter().chain(b).chain(fractions).all(Scalar::is_finite)
        {
            return Err(CaeError::convergence("nonfinite or inconsistent cyclic plastic strain sample"));
        }
        let decreasing = a.iter().zip(b).filter(|(x, y)| **y < **x - 1e-12).count();
        Ok(json!({"component": COMPONENT_ID, "all_samples_finite": true,
                  "cycle_states": [self.settings.start, self.settings.end],
                  "cells_with_decreasing_accumulated_plastic_strain": decreasing,
                  "fatigue_service_cycles": self.settings.fatigue.service_cycles,
                  "fatigue_provenance": self.settings.fatigue.provenance,
                  "engineering_limits_checked": false, "weighted_surrogate": true,
                  "unconditional_literal_upper_bound": false}))
    }

    #[must_use]
    pub fn field_metadata(&self) -> Map<String, Value> {
        Map::new()
    }
}

impl AddInAdapter for CyclicPlasticStrain {
    fn implementation(&self) -> String {
        IMPLEMENTATION.into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        json!({"status": "field_component", "history": true, "rust_extension": true, "limitations": LIMITATIONS})
            .as_object()
            .cloned()
    }
    fn component_kind(&self) -> Option<String> {
        Some(CyclicPlasticStrain::COMPONENT_KIND.into())
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        Some(CyclicPlasticStrain::response_units().into_iter().collect())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}


pub fn register(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    let mut c = AddInContract::new(COMPONENT_ID);
    c.category = AddInCategory::Constitutive;
    c.provides = vec![PortSpec::new("plastic_strain_history_metrics")];
    c.fidelity = Fidelity::Intermediate;
    c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
    ctx.register_addin(ContractInput::Typed(Box::new(c)), Some(Arc::new(CyclicPlasticStrain)))
}

