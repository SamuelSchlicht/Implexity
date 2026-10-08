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

pub const IMPLEMENTATION: &str = "implexity_rust_extension.phase_region_temperature_extrema";
pub const COMPONENT_ID: &str = "phase_region_temperature_extrema";
pub const REGION_FRACTION_SAMPLE: &str = "shared_nodal_region_fractions";
pub const REQUIRES: [&str; 2] = ["shared_nodal_temperature_K", REGION_FRACTION_SAMPLE];
pub const REGIONS: [&str; 6] =
    ["endmember_0", "endmember_1", "endmember_interface", "wetted_wall", "fluid", "loaded_surface"];
pub const FRACTION_WIDTH: usize = 4;

const LIMITATIONS: [&str; 4] = [
    "Region memberships come from adjacent-cell means of the diffuse occupancy and phase fields; a sharp interface is represented to one cell.",
    "Weighted log-sum-exp is a differentiable surrogate; it can underestimate the literal maximum for partial memberships.",
    "Shared (local-equilibrium) nodal temperatures: a wetted-wall value is the host's wall-node temperature, not a resolved boundary-layer value.",
    "Checks sampled values only; engineering limits are authored as response bounds by the study.",
];

#[must_use]
pub fn response_names() -> Vec<String> {
    REGIONS
        .iter()
        .flat_map(|r| {
            [format!("phase_region_{r}_temperature_bound_K"), format!("phase_region_{r}_temperature_max_K")]
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegionSettings {
    pub include_initial: bool,
    pub width_k: f64,
    pub membership_threshold: f64,
    pub membership_half_width: f64,
    pub provenance: String,
    raw: Value,
}

impl RegionSettings {
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }
}


pub fn validate(settings: &Value) -> Result<RegionSettings, CaeError> {
    let err = || {
        CaeError::contract(
            "phase-region temperature extrema require include_initial, positive width_K, membership_threshold in (0, 1), membership_half_width in (0, min(threshold, 1 - threshold)] and provenance",
        )
    };
    let s = settings.as_object().filter(|s| s.len() == 5).ok_or_else(err)?;
    let number =
        |k: &str| s.get(k).filter(|v| v.is_number()).and_then(Value::as_f64).filter(Scalar::is_finite);
    let include_initial = s.get("include_initial").and_then(Value::as_bool).ok_or_else(err)?;
    let width_k = number("width_K").filter(|w| *w > 0.0).ok_or_else(err)?;
    let eta = number("membership_threshold").filter(|v| *v > 0.0 && *v < 1.0).ok_or_else(err)?;
    let delta =
        number("membership_half_width").filter(|v| *v > 0.0 && *v <= eta.min(1.0 - eta)).ok_or_else(err)?;
    let provenance = s
        .get("provenance")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(err)?
        .to_string();
    Ok(RegionSettings {
        include_initial,
        width_k,
        membership_threshold: eta,
        membership_half_width: delta,
        provenance,
        raw: settings.clone(),
    })
}

pub fn step<S: Scalar>(phi: S, eta: f64, delta: f64) -> S {
    let t = (phi.value() - (eta - delta)) / (2.0 * delta);
    if t <= 0.0 {
        S::zero()
    } else if t >= 1.0 {
        S::from_f64(1.0)
    } else {
        let tt = (phi - (eta - delta)) * (0.5 / delta);
        tt * tt * (-tt * 2.0 + 3.0)
    }
}

fn memberships<S: Scalar>(phi: &[S], eta: f64, delta: f64) -> [S; 6] {
    let (e0, e1, f, loaded) = (phi[0], phi[1], phi[2], phi[3]);
    [
        step(e0, eta, delta),
        step(e1, eta, delta),
        e0 * e1 * 4.0,
        (e0 + e1) * f * 4.0,
        step(f, eta, delta),
        loaded * step(e0 + e1, eta, delta),
    ]
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundRegionExtrema {
    pub settings: RegionSettings,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RegionTemperatureExtrema;

impl RegionTemperatureExtrema {
    pub const COMPONENT_KIND: &'static str = "history_response_observer";

    #[must_use]
    pub fn response_units() -> Vec<(String, String)> {
        response_names().into_iter().map(|r| (r, "K".to_string())).collect()
    }


    pub fn bind(&self, settings: &Value) -> Result<BoundRegionExtrema, CaeError> {
        Ok(BoundRegionExtrema { settings: validate(settings)? })
    }
}

impl BoundRegionExtrema {
    #[must_use]
    pub fn selected_states(&self, len: usize) -> std::ops::Range<usize> {
        (usize::from(!self.settings.include_initial))..len
    }


    pub fn values<S: Scalar>(&self, temperatures: &[&[S]], fractions: &[&[S]]) -> Result<Vec<S>, CaeError> {
        self.values_two_temperature(temperatures, temperatures, fractions)
    }


    pub fn values_two_temperature<S: Scalar>(
        &self,
        temperatures: &[&[S]],
        coolant: &[&[S]],
        fractions: &[&[S]],
    ) -> Result<Vec<S>, CaeError> {
        if temperatures.is_empty()
            || temperatures.len() != fractions.len()
            || coolant.len() != temperatures.len()
        {
            return Err(CaeError::contract(
                "phase-region temperature extrema require selected stored states",
            ));
        }
        let (eta, delta, w) =
            (self.settings.membership_threshold, self.settings.membership_half_width, self.settings.width_k);
        let mut members: [Vec<(S, S)>; 6] = Default::default();
        let fluid = REGIONS.iter().position(|r| *r == "fluid").unwrap_or(usize::MAX);
        for ((t, tc), phi) in temperatures.iter().zip(coolant).zip(fractions) {
            if phi.len() != FRACTION_WIDTH * t.len() || tc.len() != t.len() {
                return Err(CaeError::contract("region fractions must hold four values per sampled node"));
            }
            for (i, ti) in t.iter().enumerate() {
                let m = memberships(&phi[FRACTION_WIDTH * i..FRACTION_WIDTH * (i + 1)], eta, delta);
                for (r, mr) in m.into_iter().enumerate() {
                    if mr.value() > 0.0 {
                        members[r].push((if r == fluid { tc[i] } else { *ti }, mr));
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(10);
        for (r, rows) in members.iter().enumerate() {
            if rows.is_empty() {
                return Err(CaeError::contract(format!(
                    "phase-region temperature extrema: region {} has no member node",
                    REGIONS[r]
                )));
            }
            let shift = rows.iter().fold(f64::NEG_INFINITY, |a, (t, _)| a.max(t.value()));
            let mut total = S::zero();
            for (t, m) in rows {
                total += *m * ((*t - shift) * (1.0 / w)).exp();
            }
            out.push(total.ln() * w + shift);
            let strong: Vec<S> = rows.iter().filter(|(_, m)| m.value() >= 0.5).map(|(t, _)| *t).collect();
            let pool: Vec<S> =
                if strong.is_empty() { rows.iter().map(|(t, _)| *t).collect() } else { strong };
            let top = pool.iter().fold(f64::NEG_INFINITY, |a, t| a.max(t.value()));
            #[allow(clippy::float_cmp)]                                                    
            let ties = pool.iter().filter(|t| t.value() == top).count().max(1);
            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            let grad: Vec<f64> =
                pool.iter().map(|t| if t.value() == top { 1.0 / ties as f64 } else { 0.0 }).collect();
            out.push(S::lift(top, &pool, &grad, &[]));
        }
        Ok(out)
    }


    pub fn check(&self, temperatures: &[&[f64]], fractions: &[&[f64]]) -> Result<Value, CaeError> {
        if temperatures.is_empty() {
            return Err(CaeError::contract("phase-region temperature extrema require samples"));
        }
        for (t, phi) in temperatures.iter().zip(fractions) {
            if t.is_empty()
                || phi.len() != FRACTION_WIDTH * t.len()
                || !t.iter().chain(phi.iter()).all(Scalar::is_finite)
            {
                return Err(CaeError::convergence(
                    "nonfinite or inconsistent phase-region temperature sample",
                ));
            }
        }
        Ok(json!({"component": COMPONENT_ID, "all_samples_finite": true,
                  "scope": "nodal_memberships_from_adjacent_cell_region_fractions",
                  "engineering_limits_checked": false, "weighted_surrogate": true,
                  "unconditional_literal_upper_bound": false}))
    }

    #[must_use]
    pub fn fields(&self, fractions: &[f64]) -> BTreeMap<String, Vec<f64>> {
        let (eta, delta) = (self.settings.membership_threshold, self.settings.membership_half_width);
        let mut out: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        for phi in fractions.chunks(FRACTION_WIDTH) {
            let m = memberships(phi, eta, delta);
            for (r, v) in m.iter().enumerate() {
                out.entry(format!("phase_region_{}_membership", REGIONS[r])).or_default().push(*v);
            }
        }
        out
    }

    #[must_use]
    pub fn field_metadata(&self) -> Map<String, Value> {
        REGIONS
            .iter()
            .map(|r| {
                (
                    format!("phase_region_{r}_membership"),
                    json!({"units": "1", "association": "node", "rank": 0, "source": COMPONENT_ID,
                           "description": format!("Nodal {r} membership of the phase-region temperature extrema (solid-mesh node order)")}),
                )
            })
            .collect()
    }
}

impl AddInAdapter for RegionTemperatureExtrema {
    fn implementation(&self) -> String {
        IMPLEMENTATION.into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        json!({"status": "field_component", "history": true, "rust_extension": true, "limitations": LIMITATIONS})
            .as_object()
            .cloned()
    }
    fn component_kind(&self) -> Option<String> {
        Some(RegionTemperatureExtrema::COMPONENT_KIND.into())
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        Some(RegionTemperatureExtrema::response_units().into_iter().collect())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}


pub fn register(ctx: &InstallContext<'_>) -> Result<AddInContract, CaeError> {
    let mut c = AddInContract::new(COMPONENT_ID);
    c.category = AddInCategory::Constitutive;
    c.provides = vec![PortSpec::new("temperature_history_metrics")];
    c.fidelity = Fidelity::Intermediate;
    c.notes = LIMITATIONS.iter().map(|s| (*s).to_string()).collect();
    ctx.register_addin(ContractInput::Typed(Box::new(c)), Some(Arc::new(RegionTemperatureExtrema)))
}

