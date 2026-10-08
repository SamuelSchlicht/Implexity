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

use crate::region_temperature_extrema::{FRACTION_WIDTH, REGION_FRACTION_SAMPLE, step};

pub const IMPLEMENTATION: &str = "implexity_rust_extension.wall_film_temperature";
pub const COMPONENT_ID: &str = "wall_film_temperature";
pub const WALL_FLUX_SAMPLE: &str = "shared_nodal_solid_heat_flux_W_m2";
pub const SPEED_SAMPLE: &str = "shared_nodal_fluid_speed_m_s";
pub const REQUIRES: [&str; 4] =
    ["shared_nodal_temperature_K", REGION_FRACTION_SAMPLE, WALL_FLUX_SAMPLE, SPEED_SAMPLE];
pub const LAMINAR_NU: f64 = 3.66;
pub const RE_LAMINAR: f64 = 2300.0;
pub const RE_TURBULENT: f64 = 1.0e4;

const LIMITATIONS: [&str; 4] = [
    "Response-level correction: the solved shared temperature field keeps its local-equilibrium wall; the film difference is added to the reported values, not fed back.",
    "Gnielinski channel correlation at the bulk (fluid-volume mean) coolant speed of each state and an authored hydraulic diameter: one film coefficient per state, no local variation of the coefficient (stagnation, separation); entrance effects, roughness, secondary flows and wall-temperature viscosity corrections are not included; enhancement factors are authored.",
    "First-order region-plus-film weighted surrogate; partial memberships can invalidate a literal peak upper-bound guarantee.",
    "No boiling: the corrected wall temperature must stay below saturation for the single-phase correlation to hold; subcooled boiling and CHF are not modelled.",
];

#[must_use]
pub fn response_names() -> Vec<String> {
    [
        "wall_film_temperature_rise_bound_K",
        "wall_film_temperature_rise_max_K",
        "wall_film_wall_temperature_bound_K",
        "wall_film_wall_temperature_max_K",
        "wall_film_endmember_0_temperature_bound_K",
        "wall_film_endmember_1_temperature_bound_K",
        "wall_film_loaded_surface_temperature_bound_K",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilmSettings {
    pub include_initial: bool,
    pub width_k: f64,
    pub membership_threshold: f64,
    pub membership_half_width: f64,
    pub hydraulic_diameter_m: f64,
    pub density: f64,
    pub viscosity: f64,
    pub conductivity: f64,
    pub heat_capacity: f64,
    pub enhancement: f64,
    pub provenance: String,
    raw: Value,
}

impl FilmSettings {
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw
    }

    #[must_use]
    pub fn prandtl(&self) -> f64 {
        self.heat_capacity * self.viscosity / self.conductivity
    }
}


pub fn validate(settings: &Value) -> Result<FilmSettings, CaeError> {
    let err = || {
        CaeError::contract(
            "wall film temperature requires include_initial, positive width_K, membership_threshold in (0, 1), membership_half_width in (0, min(threshold, 1 - threshold)], positive hydraulic_diameter_m, fluid {density_kg_m3, mu_Pa_s, k_W_mK, cp_J_kgK} (positive), correlation 'gnielinski', enhancement_factor >= 1 and provenance",
        )
    };
    let s = settings.as_object().filter(|s| s.len() == 9).ok_or_else(err)?;
    let positive = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|v| v.is_finite() && *v > 0.0);
    let include_initial = s.get("include_initial").and_then(Value::as_bool).ok_or_else(err)?;
    let width_k = positive(s.get("width_K")).ok_or_else(err)?;
    let eta = positive(s.get("membership_threshold")).filter(|v| *v < 1.0).ok_or_else(err)?;
    let delta =
        positive(s.get("membership_half_width")).filter(|v| *v <= eta.min(1.0 - eta)).ok_or_else(err)?;
    let dh = positive(s.get("hydraulic_diameter_m")).ok_or_else(err)?;
    let fluid = s.get("fluid").and_then(Value::as_object).filter(|f| f.len() == 4).ok_or_else(err)?;
    let density = positive(fluid.get("density_kg_m3")).ok_or_else(err)?;
    let viscosity = positive(fluid.get("mu_Pa_s")).ok_or_else(err)?;
    let conductivity = positive(fluid.get("k_W_mK")).ok_or_else(err)?;
    let heat_capacity = positive(fluid.get("cp_J_kgK")).ok_or_else(err)?;
    if s.get("correlation").and_then(Value::as_str) != Some("gnielinski") {
        return Err(err());
    }
    let enhancement = positive(s.get("enhancement_factor")).filter(|e| *e >= 1.0).ok_or_else(err)?;
    let provenance = s
        .get("provenance")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(err)?
        .to_string();
    Ok(FilmSettings {
        include_initial,
        width_k,
        membership_threshold: eta,
        membership_half_width: delta,
        hydraulic_diameter_m: dh,
        density,
        viscosity,
        conductivity,
        heat_capacity,
        enhancement,
        provenance,
        raw: settings.clone(),
    })
}

fn gnielinski<S: Scalar>(re: S, pr: f64) -> S {
    let f = (re.ln() * 0.790 - 1.64).powf(-2.0);
    (f * 0.125) * (re - 1000.0) * pr / ((f * 0.125).sqrt() * (12.7 * (pr.powf(2.0 / 3.0) - 1.0)) + 1.0)
}

pub fn nusselt<S: Scalar>(re: S, pr: f64) -> S {
    if re.value() <= RE_LAMINAR {
        return S::from_f64(LAMINAR_NU);
    }
    if re.value() >= RE_TURBULENT {
        return gnielinski(re, pr);
    }
    let upper = gnielinski(S::from_f64(RE_TURBULENT), pr).value();
    let gamma = (re - RE_LAMINAR) * (1.0 / (RE_TURBULENT - RE_LAMINAR));
    (-gamma + 1.0) * LAMINAR_NU + gamma * upper
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WallFilmTemperature;

impl WallFilmTemperature {
    pub const COMPONENT_KIND: &'static str = "history_response_observer";

    #[must_use]
    pub fn response_units() -> Vec<(String, String)> {
        response_names().into_iter().map(|r| (r, "K".to_string())).collect()
    }


    pub fn bind(&self, settings: &Value) -> Result<BoundFilm, CaeError> {
        Ok(BoundFilm { settings: validate(settings)? })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundFilm {
    pub settings: FilmSettings,
}

pub struct FilmSample<'a, S> {
    pub temperature: &'a [S],
    pub fractions: &'a [S],
    pub heat_flux: &'a [S],
    pub speed: &'a [S],
}

fn log_sum_exp<S: Scalar>(rows: &[(S, S)], w: f64) -> Option<S> {
    if rows.is_empty() {
        return None;
    }
    let shift = rows.iter().fold(f64::NEG_INFINITY, |a, (t, _)| a.max(t.value()));
    let mut total = S::zero();
    for (t, m) in rows {
        total += *m * ((*t - shift) * (1.0 / w)).exp();
    }
    Some(total.ln() * w + shift)
}

fn literal_max<S: Scalar>(rows: &[(S, S)]) -> Option<S> {
    let strong: Vec<S> = rows.iter().filter(|(_, m)| m.value() >= 0.5).map(|(t, _)| *t).collect();
    let pool: Vec<S> = if strong.is_empty() { rows.iter().map(|(t, _)| *t).collect() } else { strong };
    if pool.is_empty() {
        return None;
    }
    let top = pool.iter().fold(f64::NEG_INFINITY, |a, t| a.max(t.value()));
    #[allow(clippy::float_cmp)]                             
    let ties = pool.iter().filter(|t| t.value() == top).count().max(1);
    #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
    let grad: Vec<f64> =
        pool.iter().map(|t| if t.value() == top { 1.0 / ties as f64 } else { 0.0 }).collect();
    Some(S::lift(top, &pool, &grad, &[]))
}

impl BoundFilm {
    #[must_use]
    pub fn selected_states(&self, len: usize) -> std::ops::Range<usize> {
        (usize::from(!self.settings.include_initial))..len
    }

    pub fn coefficient<S: Scalar>(&self, speed: S) -> S {
        let st = &self.settings;
        let re = speed * (st.density * st.hydraulic_diameter_m / st.viscosity);
        nusselt(re, st.prandtl()) * (st.enhancement * st.conductivity / st.hydraulic_diameter_m)
    }


    pub fn values<S: Scalar>(&self, samples: &[FilmSample<'_, S>]) -> Result<Vec<S>, CaeError> {
        let st = &self.settings;
        let (eta, delta, w) = (st.membership_threshold, st.membership_half_width, st.width_k);
        let mut rise: Vec<(S, S)> = Vec::new();
        let mut wall: Vec<(S, S)> = Vec::new();
        let mut solid: [Vec<(S, S)>; 3] = Default::default();
        for s in samples {
            let n = s.temperature.len();
            if s.fractions.len() != FRACTION_WIDTH * n || s.heat_flux.len() != n || s.speed.len() != n {
                return Err(CaeError::contract("wall film samples disagree in size"));
            }
            let (mut flow, mut volume) = (S::zero(), S::zero());
            for i in 0..n {
                let f = s.fractions[FRACTION_WIDTH * i + 2];
                flow += f * s.speed[i];
                volume += f;
            }
            if volume.value() <= 0.0 {
                return Err(CaeError::contract("wall film temperature: no coolant node"));
            }
            let coefficient = self.coefficient(flow / volume);
            for i in 0..n {
                let phi = &s.fractions[FRACTION_WIDTH * i..FRACTION_WIDTH * (i + 1)];
                let (e0, e1, f, loaded) = (phi[0], phi[1], phi[2], phi[3]);
                let t = s.temperature[i];
                let m_wall = (e0 + e1) * f * 4.0;
                if m_wall.value() > 0.0 {
                    let d_t = s.heat_flux[i] / coefficient;
                    rise.push((d_t, m_wall));
                    wall.push((t + d_t, m_wall));
                }
                for (r, m) in [step(e0, eta, delta), step(e1, eta, delta), loaded * step(e0 + e1, eta, delta)]
                    .into_iter()
                    .enumerate()
                {
                    if m.value() > 0.0 {
                        solid[r].push((t, m));
                    }
                }
            }
        }
        let missing = || CaeError::contract("wall film temperature: no wetted-wall or solid region node");
        let rise_bound = log_sum_exp(&rise, w).ok_or_else(missing)?;
        let mut out = vec![
            rise_bound,
            literal_max(&rise).ok_or_else(missing)?,
            log_sum_exp(&wall, w).ok_or_else(missing)?,
            literal_max(&wall).ok_or_else(missing)?,
        ];
        for rows in &solid {
            out.push(log_sum_exp(rows, w).ok_or_else(missing)? + rise_bound);
        }
        Ok(out)
    }


    pub fn check(&self, samples: &[FilmSample<'_, f64>]) -> Result<Value, CaeError> {
        for s in samples {
            let ok = s
                .temperature
                .iter()
                .chain(s.fractions)
                .chain(s.heat_flux)
                .chain(s.speed)
                .all(Scalar::is_finite)
                && s.heat_flux.iter().chain(s.speed).all(|v| *v >= 0.0);
            if !ok {
                return Err(CaeError::convergence("nonfinite or negative wall film sample"));
            }
        }
        Ok(json!({"component": COMPONENT_ID, "all_samples_finite": true, "correlation": "gnielinski",
                  "feedback_into_field": false, "engineering_limits_checked": false, "weighted_surrogate": true,
                  "unconditional_literal_upper_bound": false}))
    }
}

impl AddInAdapter for WallFilmTemperature {
    fn implementation(&self) -> String {
        IMPLEMENTATION.into()
    }
    fn runtime_support(&self) -> Option<Map<String, Value>> {
        json!({"status": "field_component", "history": true, "rust_extension": true, "limitations": LIMITATIONS})
            .as_object()
            .cloned()
    }
    fn component_kind(&self) -> Option<String> {
        Some(WallFilmTemperature::COMPONENT_KIND.into())
    }
    fn response_units(&self) -> Option<BTreeMap<String, String>> {
        Some(WallFilmTemperature::response_units().into_iter().collect())
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
    ctx.register_addin(ContractInput::Typed(Box::new(c)), Some(Arc::new(WallFilmTemperature)))
}

