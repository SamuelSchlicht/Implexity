// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::component_manifests::{
    HistoryResponseManifest, load_history_catalog, validate_history_manifest,
};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_base::array::Tensor;
use implexity_physics_base::cyclic_plastic_strain::{self as cyclic, BoundCyclic, CyclicPlasticStrain};
use implexity_physics_base::occupancy_grayness::{self as grayness, BoundGrayness, OccupancyGrayness};
use implexity_physics_base::region_temperature_extrema::{
    self as region, BoundRegionExtrema, REGION_FRACTION_SAMPLE, RegionTemperatureExtrema,
};
use implexity_physics_base::temperature_extrema::{BoundTemperatureExtrema, TemperatureExtrema};
use implexity_physics_base::wall_film_temperature::{
    self as film, BoundFilm, FilmSample, WallFilmTemperature,
};
use implexity_physics_solid::history::monitor::{self, BoundMonitor, MonitorSample};

use crate::liquid_admissibility::{BoundLiquid, LiquidOnlyHistory, ObserverSample};
use crate::liquid_pressure_margin::{BoundLiquidPressureMargin, LiquidPressureMargin};

use super::kernel::UnifiedKernel;
use super::{BASE_HISTORY_SAMPLES, MATERIAL_HISTORY_SAMPLES};

const COMPONENT_KIND: &str = "history_response_observer";

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Debug, Clone)]
pub struct HistorySample<S> {
    pub pressure: Vec<S>,
    pub fluid_temperature: Vec<S>,
    pub upper: Vec<S>,
    pub fluid_fraction: Vec<S>,
    pub nodal_temperature: Vec<S>,
    pub material: Option<[Vec<S>; 4]>,
    pub nodal_region_fractions: Option<Vec<S>>,
    pub cell_equivalent_plastic_strain: Option<Vec<S>>,
    pub cell_region_fractions: Option<Vec<S>>,
    pub nodal_solid_heat_flux: Option<Vec<S>>,
    pub nodal_fluid_speed: Option<Vec<S>>,
    pub nodal_fluid_temperature: Option<Vec<S>>,
}

impl<S: Scalar> HistorySample<S> {
    fn observer(&self) -> ObserverSample<S> {
        ObserverSample {
            pressure_absolute_pa: self.pressure.clone(),
            fluid_temperature_k: self.fluid_temperature.clone(),
            phase_cell_temperature_upper_k: self.upper.clone(),
            fluid_fraction: self.fluid_fraction.clone(),
        }
    }

    #[must_use]
    pub fn named(&self) -> BTreeMap<&'static str, Vec<f64>> {
        let v = |a: &[S]| a.iter().map(Scalar::value).collect::<Vec<f64>>();
        let mut out = BTreeMap::from([
            ("pressure_absolute_Pa", v(&self.pressure)),
            ("fluid_temperature_K", v(&self.fluid_temperature)),
            ("phase_cell_temperature_upper_K", v(&self.upper)),
            ("fluid_fraction", v(&self.fluid_fraction)),
            ("shared_nodal_temperature_K", v(&self.nodal_temperature)),
        ]);
        if let Some(m) = &self.material {
            for (name, a) in MATERIAL_HISTORY_SAMPLES.iter().zip(m) {
                out.insert(name, v(a));
            }
        }
        if let Some(f) = &self.nodal_region_fractions {
            out.insert(REGION_FRACTION_SAMPLE, v(f));
        }
        if let Some(f) = &self.cell_equivalent_plastic_strain {
            out.insert(cyclic::PLASTIC_STRAIN_SAMPLE, v(f));
        }
        if let Some(f) = &self.cell_region_fractions {
            out.insert(cyclic::CELL_REGION_SAMPLE, v(f));
        }
        if let Some(f) = &self.nodal_solid_heat_flux {
            out.insert(film::WALL_FLUX_SAMPLE, v(f));
        }
        if let Some(f) = &self.nodal_fluid_speed {
            out.insert(film::SPEED_SAMPLE, v(f));
        }
        out
    }
}

pub enum ObserverKind {
    Extrema(BoundTemperatureExtrema),
    Liquid(BoundLiquid),
    Pressure(BoundLiquidPressureMargin),
    Monitor(BoundMonitor),
    Region(BoundRegionExtrema),
    Cyclic(BoundCyclic),
    Film(BoundFilm),
    Grayness(BoundGrayness),
}

pub struct UnifiedObserver {
    pub component: String,
    pub required_sample_names: Vec<String>,
    pub kind: ObserverKind,
}

impl std::fmt::Debug for UnifiedObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnifiedObserver").field("component", &self.component).finish_non_exhaustive()
    }
}

enum Factory {
    Extrema,
    Liquid,
    Pressure,
    Monitor,
    Region,
    Cyclic,
    Film,
    Grayness,
}

impl Factory {
    fn response_units(&self) -> Vec<(String, String)> {
        let rows: Vec<(&str, &str)> = match self {
            Self::Extrema => {
                vec![("shared_temperature_max_K", "K"), ("shared_temperature_upper_bound_K", "K")]
            }
            Self::Liquid => {
                vec![("fluid_subcooling_bound_K", "K"), ("phase_cell_nodal_subcooling_bound_K", "K")]
            }
            Self::Pressure => crate::liquid_pressure_margin::RESPONSES.iter().map(|r| (*r, "Pa")).collect(),
            Self::Monitor => monitor::RESPONSE_UNITS.to_vec(),
            Self::Region => return RegionTemperatureExtrema::response_units(),
            Self::Cyclic => return CyclicPlasticStrain::response_units(),
            Self::Film => return WallFilmTemperature::response_units(),
            Self::Grayness => return OccupancyGrayness::response_units(),
        };
        rows.into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    fn requires(&self) -> Vec<String> {
        let rows: Vec<&str> = match self {
            Self::Extrema => TemperatureExtrema::REQUIRES.to_vec(),
            Self::Liquid | Self::Pressure => crate::liquid_admissibility::REQUIRES.to_vec(),
            Self::Monitor => monitor::REQUIRES.to_vec(),
            Self::Region => region::REQUIRES.to_vec(),
            Self::Cyclic => cyclic::REQUIRES.to_vec(),
            Self::Film => film::REQUIRES.to_vec(),
            Self::Grayness => grayness::REQUIRES.to_vec(),
        };
        rows.into_iter().map(str::to_string).collect()
    }

    fn validate(&self, settings: &Value) -> CaeResult<Value> {
        let addins = &implexity_core::registries::global().addins;
        match self {
            Self::Extrema => {
                TemperatureExtrema.validate(settings)?;
                Ok(settings.clone())
            }
            Self::Liquid => LiquidOnlyHistory.validate(addins, settings),
            Self::Pressure => Ok(LiquidPressureMargin.validate(addins, settings)?.raw().clone()),
            Self::Monitor => monitor::validate(settings),
            Self::Region => Ok(region::validate(settings)?.raw().clone()),
            Self::Cyclic => Ok(cyclic::validate(settings)?.raw().clone()),
            Self::Film => Ok(film::validate(settings)?.raw().clone()),
            Self::Grayness => Ok(grayness::validate(settings)?.raw().clone()),
        }
    }
}

fn selected_observer(name: &str) -> CaeResult<Factory> {
    let addins = &implexity_core::registries::global().addins;
    let entry = addins.get(name)?;
    let not_observer = || err(format!("{name}: not a native history response/admissibility component"));
    let adapter = entry.adapter.as_ref().ok_or_else(not_observer)?;
    if adapter.component_kind().as_deref() != Some(COMPONENT_KIND) {
        return Err(not_observer());
    }
    let implementation = adapter.implementation();
    let factory = if implementation == implexity_physics_base::temperature_extrema::IMPLEMENTATION {
        Factory::Extrema
    } else if implementation == crate::liquid_admissibility::IMPLEMENTATION {
        Factory::Liquid
    } else if implementation == crate::liquid_pressure_margin::IMPLEMENTATION {
        Factory::Pressure
    } else if implementation == monitor::IMPLEMENTATION {
        Factory::Monitor
    } else if implementation == region::IMPLEMENTATION {
        Factory::Region
    } else if implementation == cyclic::IMPLEMENTATION {
        Factory::Cyclic
    } else if implementation == film::IMPLEMENTATION {
        Factory::Film
    } else if implementation == grayness::IMPLEMENTATION {
        Factory::Grayness
    } else {
        return Err(not_observer());
    };
    let manifests = load_history_catalog(implexity_core::distributions::global())?;
    let declared =
        HistoryResponseManifest { response_units: factory.response_units(), requires: factory.requires() };
    validate_history_manifest(&manifests, name, &declared)?;
    Ok(factory)
}

fn repr_names(names: &[String]) -> String {
    implexity_core::pyobj::list_repr(&names.iter().map(String::as_str).collect::<Vec<_>>())
}


pub fn declarations(rows: Option<&Value>) -> CaeResult<Vec<Value>> {
    let Some(rows) = rows.filter(|r| !r.is_null()) else {
        return Ok(Vec::new());
    };
    let rows = rows.as_array().ok_or_else(|| err("history_observers must be an explicit list"))?;
    let (mut out, mut ids, mut responses) = (Vec::new(), BTreeSet::new(), BTreeSet::new());
    for row in rows {
        let m = row
            .as_object()
            .filter(|m| {
                m.len() == 2 && m.contains_key("settings") && m.get("component").is_some_and(Value::is_string)
            })
            .ok_or_else(|| err("history observer requires component and settings"))?;
        let name = m["component"].as_str().unwrap_or_default().to_string();
        let factory = selected_observer(&name)?;
        if ids.contains(&name) {
            return Err(err("duplicate history observer"));
        }
        let config = factory.validate(&m["settings"])?;
        ids.insert(name.clone());
        let units: BTreeSet<String> = factory.response_units().into_iter().map(|(k, _)| k).collect();
        let collisions: Vec<String> = responses.intersection(&units).cloned().collect();
        if !collisions.is_empty() {
            return Err(err(format!("ambiguous observer responses: {}", repr_names(&collisions))));
        }
        responses.extend(units);
        out.push(json!({"component": name, "settings": config}));
    }
    Ok(out)
}


pub fn selected_response_units(rows: Option<&Value>) -> CaeResult<Vec<(String, String)>> {
    let mut out = Vec::new();
    for row in declarations(rows)? {
        out.extend(selected_observer(row["component"].as_str().unwrap_or_default())?.response_units());
    }
    Ok(out)
}


pub fn bind(kernel: &UnifiedKernel) -> CaeResult<Vec<UnifiedObserver>> {
    let mut available: BTreeSet<&str> = BASE_HISTORY_SAMPLES.iter().copied().collect();
    if kernel.s.model.history.is_some() {
        available.extend(MATERIAL_HISTORY_SAMPLES);
    }

    available.insert(REGION_FRACTION_SAMPLE);
    available.insert(cyclic::CELL_REGION_SAMPLE);
    available.insert(cyclic::PLASTIC_STRAIN_SAMPLE);
    available.insert(film::WALL_FLUX_SAMPLE);
    available.insert(film::SPEED_SAMPLE);
    let addins = &implexity_core::registries::global().addins;
    let mut out = Vec::new();
    for row in declarations(kernel.p.get("history_observers"))? {
        let name = row["component"].as_str().unwrap_or_default().to_string();
        let factory = selected_observer(&name)?;
        let requires = factory.requires();
        let absent: Vec<String> = requires
            .iter()
            .filter(|r| !available.contains(r.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !absent.is_empty() {
            return Err(err(format!("{name}: host omits required history samples {}", repr_names(&absent))));
        }
        let settings = &row["settings"];
        let kind = match factory {
            Factory::Extrema => ObserverKind::Extrema(TemperatureExtrema.bind(settings)?),
            Factory::Liquid => ObserverKind::Liquid(LiquidOnlyHistory.bind(addins, settings)?),
            Factory::Pressure => {
                ObserverKind::Pressure(LiquidPressureMargin.bind(addins, settings, &kernel.s.times)?)
            }
            Factory::Monitor => ObserverKind::Monitor(BoundMonitor::new(settings.clone())),
            Factory::Region => ObserverKind::Region(RegionTemperatureExtrema.bind(settings)?),
            Factory::Cyclic => ObserverKind::Cyclic(CyclicPlasticStrain.bind(settings)?),
            Factory::Film => ObserverKind::Film(WallFilmTemperature.bind(settings)?),
            Factory::Grayness => ObserverKind::Grayness(OccupancyGrayness.bind(settings)?),
        };
        out.push(UnifiedObserver { component: name, required_sample_names: requires, kind });
    }
    Ok(out)
}

fn tensor<S: Scalar>(v: &[S]) -> Tensor<S> {
    Tensor::vector(v.to_vec())
}

fn monitor_values<S: Scalar>(m: &[Vec<S>; 4]) -> [S; 3] {
    let [energy, k, y, w] = m;
    let mut den = S::zero();
    let (mut e, mut kk, mut yy) = (S::zero(), S::zero(), S::zero());
    for i in 0..w.len() {
        den += w[i];
        e += w[i] * energy[i];
        kk += w[i] * k[i];
        yy += w[i] * y[i];
    }
    [e, kk / den, yy / den]
}

fn cyclic_sample<S: Scalar>(s: &HistorySample<S>) -> CaeResult<(&[S], &[S])> {
    Ok((
        s.cell_equivalent_plastic_strain.as_deref().ok_or_else(|| err("host omitted cell plastic strain"))?,
        s.cell_region_fractions.as_deref().ok_or_else(|| err("host omitted cell region fractions"))?,
    ))
}

fn film_samples<S: Scalar>(samples: &[HistorySample<S>]) -> CaeResult<Vec<FilmSample<'_, S>>> {
    samples
        .iter()
        .map(|s| {
            Ok(FilmSample {
                temperature: &s.nodal_temperature,
                fractions: s
                    .nodal_region_fractions
                    .as_deref()
                    .ok_or_else(|| err("host omitted nodal region fractions"))?,
                heat_flux: s
                    .nodal_solid_heat_flux
                    .as_deref()
                    .ok_or_else(|| err("host omitted nodal solid heat flux"))?,
                speed: s.nodal_fluid_speed.as_deref().ok_or_else(|| err("host omitted nodal fluid speed"))?,
            })
        })
        .collect()
}

fn cyclic_samples<S: Scalar>(samples: &[HistorySample<S>]) -> CaeResult<[(&[S], &[S]); 2]> {
    match samples {
        [a, e] => Ok([cyclic_sample(a)?, cyclic_sample(e)?]),
        _ => Err(err("cyclic plastic strain requires the samples of its two cycle states")),
    }
}

impl UnifiedObserver {
    #[must_use]
    pub fn response_units(&self) -> Vec<(String, String)> {
        match &self.kind {
            ObserverKind::Extrema(_) => Factory::Extrema.response_units(),
            ObserverKind::Liquid(_) => Factory::Liquid.response_units(),
            ObserverKind::Pressure(_) => Factory::Pressure.response_units(),
            ObserverKind::Monitor(_) => Factory::Monitor.response_units(),
            ObserverKind::Region(_) => Factory::Region.response_units(),
            ObserverKind::Cyclic(_) => Factory::Cyclic.response_units(),
            ObserverKind::Film(_) => Factory::Film.response_units(),
            ObserverKind::Grayness(_) => Factory::Grayness.response_units(),
        }
    }

    #[must_use]
    pub fn field_metadata(&self) -> Map<String, Value> {
        match &self.kind {
            ObserverKind::Extrema(b) => b.fields(),
            ObserverKind::Liquid(b) => b.field_metadata(),
            ObserverKind::Pressure(b) => b.field_metadata(),
            ObserverKind::Monitor(b) => b.field_metadata().as_object().cloned().unwrap_or_default(),
            ObserverKind::Region(b) => b.field_metadata(),
            ObserverKind::Cyclic(b) => b.field_metadata(),
            ObserverKind::Film(_) => Map::new(),
            ObserverKind::Grayness(b) => b.field_metadata(),
        }
    }

    #[must_use]
    pub fn sampled_states(&self, len: usize) -> Vec<usize> {
        match &self.kind {
            ObserverKind::Extrema(b) => b.selected_states(len).collect(),
            ObserverKind::Liquid(_) | ObserverKind::Pressure(_) => (0..len).collect(),
            ObserverKind::Monitor(_) => vec![len - 1],
            ObserverKind::Region(b) => b.selected_states(len).collect(),
            ObserverKind::Cyclic(b) => b.selected_states(len).map_or_else(|_| Vec::new(), |s| s.to_vec()),
            ObserverKind::Film(b) => b.selected_states(len).collect(),
            ObserverKind::Grayness(b) => b.selected_states(len),
        }
    }


    pub fn values<S: Scalar>(&self, samples: &[HistorySample<S>]) -> CaeResult<Vec<S>> {
        match &self.kind {
            ObserverKind::Extrema(b) => {
                let t: Vec<Tensor<S>> = samples.iter().map(|s| tensor(&s.nodal_temperature)).collect();
                Ok(b.values(&t)?.to_vec())
            }
            ObserverKind::Liquid(b) => {
                let o: Vec<ObserverSample<S>> = samples.iter().map(HistorySample::observer).collect();
                Ok(b.values(&o).to_vec())
            }
            ObserverKind::Pressure(b) => {
                let o: Vec<ObserverSample<S>> = samples.iter().map(HistorySample::observer).collect();
                Ok(b.values(&o).to_vec())
            }
            ObserverKind::Monitor(_) => {
                let last = samples
                    .last()
                    .and_then(|s| s.material.as_ref())
                    .ok_or_else(|| err("host omitted actual material-state samples"))?;
                Ok(monitor_values(last).to_vec())
            }
            ObserverKind::Region(b) => {
                let t: Vec<&[S]> = samples.iter().map(|s| s.nodal_temperature.as_slice()).collect();
                let coolant: Vec<&[S]> = samples
                    .iter()
                    .map(|s| s.nodal_fluid_temperature.as_deref().unwrap_or(&s.nodal_temperature))
                    .collect();
                let f: Vec<&[S]> = samples
                    .iter()
                    .map(|s| {
                        s.nodal_region_fractions
                            .as_deref()
                            .ok_or_else(|| err("host omitted nodal region fractions"))
                    })
                    .collect::<CaeResult<_>>()?;
                b.values_two_temperature(&t, &coolant, &f)
            }
            ObserverKind::Cyclic(b) => {
                let [a, e] = cyclic_samples(samples)?;
                b.values([a.0, e.0], e.1)
            }
            ObserverKind::Film(b) => b.values(&film_samples(samples)?),
            ObserverKind::Grayness(b) => {
                let phi: Vec<&[S]> = samples.iter().map(|s| s.fluid_fraction.as_slice()).collect();
                b.values(&phi)
            }
        }
    }


    pub fn check(
        &self,
        samples: &[HistorySample<f64>],
        boundaries: &[ObserverSample<f64>],
    ) -> CaeResult<Value> {
        match &self.kind {
            ObserverKind::Extrema(b) => {
                let t: Vec<Tensor<f64>> = samples.iter().map(|s| tensor(&s.nodal_temperature)).collect();
                b.check(&t)
            }
            ObserverKind::Liquid(b) => {
                let mut o: Vec<ObserverSample<f64>> = samples.iter().map(HistorySample::observer).collect();
                o.extend(boundaries.iter().cloned());
                b.check(&o)
            }
            ObserverKind::Pressure(b) => {
                let mut o: Vec<ObserverSample<f64>> = samples.iter().map(HistorySample::observer).collect();
                o.extend(boundaries.iter().cloned());
                b.check(&o)
            }
            ObserverKind::Monitor(b) => {
                let m: Vec<MonitorSample> = samples
                    .iter()
                    .map(|s| {
                        s.material
                            .as_ref()
                            .map(|[e, k, y, w]| MonitorSample {
                                stored_energy: e.clone(),
                                conductivity: k.clone(),
                                yield_stress: y.clone(),
                                volume: w.clone(),
                            })
                            .ok_or_else(|| err("host omitted actual material-state samples"))
                    })
                    .collect::<CaeResult<_>>()?;
                b.check(&m)
            }
            ObserverKind::Region(b) => {
                let t: Vec<&[f64]> = samples.iter().map(|s| s.nodal_temperature.as_slice()).collect();
                let f: Vec<&[f64]> = samples
                    .iter()
                    .map(|s| {
                        s.nodal_region_fractions
                            .as_deref()
                            .ok_or_else(|| err("host omitted nodal region fractions"))
                    })
                    .collect::<CaeResult<_>>()?;
                b.check(&t, &f)
            }
            ObserverKind::Cyclic(b) => {
                let states = b.selected_states(samples.len())?;
                let picked: Vec<HistorySample<f64>> = states.iter().map(|n| samples[*n].clone()).collect();
                let [a, e] = cyclic_samples(&picked)?;
                b.check([a.0, e.0], e.1)
            }
            ObserverKind::Film(b) => {
                let picked: Vec<HistorySample<f64>> =
                    b.selected_states(samples.len()).map(|n| samples[n].clone()).collect();
                b.check(&film_samples(&picked)?)
            }
            ObserverKind::Grayness(b) => {
                let phi: Vec<&[f64]> = b
                    .selected_states(samples.len())
                    .iter()
                    .map(|n| samples[*n].fluid_fraction.as_slice())
                    .collect();
                b.check(&phi)
            }
        }
    }


    pub fn fields(&self, sample: &HistorySample<f64>) -> CaeResult<BTreeMap<String, Vec<f64>>> {
        match &self.kind {
            ObserverKind::Extrema(_) | ObserverKind::Cyclic(_) | ObserverKind::Film(_) => Ok(BTreeMap::new()),
            ObserverKind::Liquid(b) => b.fields(&sample.observer()),
            ObserverKind::Pressure(b) => b.fields(&sample.observer()),
            ObserverKind::Monitor(b) => {
                let [e, k, y, w] = sample
                    .material
                    .clone()
                    .ok_or_else(|| err("host omitted actual material-state samples"))?;
                Ok(b.fields(&MonitorSample { stored_energy: e, conductivity: k, yield_stress: y, volume: w }))
            }
            ObserverKind::Region(b) => Ok(b.fields(
                sample
                    .nodal_region_fractions
                    .as_deref()
                    .ok_or_else(|| err("host omitted nodal region fractions"))?,
            )),
            ObserverKind::Grayness(b) => Ok(b.fields(&sample.fluid_fraction)),
        }
    }

    #[must_use]
    pub fn accepts_boundary_samples(&self) -> bool {
        let boundary = [
            "pressure_absolute_Pa",
            "fluid_temperature_K",
            "phase_cell_temperature_upper_K",
            "fluid_fraction",
        ];
        self.required_sample_names.iter().all(|r| boundary.contains(&r.as_str()))
    }
}
