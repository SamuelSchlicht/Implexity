// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

pub mod ageing;
pub mod composite;
pub mod defect;
pub mod local_advance;
pub mod local_snapshot;
pub mod mechanical_ageing;
pub mod monitor;
pub mod species;

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_physics_base::history_numerical_extension::{
    self as extension, CONTRACT as EXTENSION_CONTRACT, EndpointNumericalExtension,
};

use crate::material::Props;
use crate::material::idx;
use crate::util::{contract, has_exact_keys, obj, real_array};

pub use composite::Member;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HistoryEnergy<S> {
    pub stored: S,
    pub sensible_heat: S,
    pub external: S,
}

impl<S: Scalar> HistoryEnergy<S> {
    #[must_use]
    pub fn zero() -> Self {
        Self {
            stored: S::zero(),
            sensible_heat: S::zero(),
            external: S::zero(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryComponent {
    Defect,
    Ageing,
    Rupture,
    Oxidation,
    Species,
    Composite,
}

impl HistoryComponent {
    pub const ALL: [Self; 6] = [
        Self::Defect,
        Self::Ageing,
        Self::Species,
        Self::Composite,
        Self::Rupture,
        Self::Oxidation,
    ];

    #[must_use]
    pub fn component_id(self) -> &'static str {
        match self {
            Self::Defect => "saturating_defect_kinetics",
            Self::Ageing => "environmental_ageing",
            Self::Rupture => "creep_rupture_history",
            Self::Oxidation => "parabolic_oxidation_history",
            Self::Species => "saturating_species_retention",
            Self::Composite => "composite_material_history",
        }
    }

    #[must_use]
    pub fn implementation(self) -> &'static str {
        match self {
            Self::Defect => "implexity.physics_library.defect_kinetics.SaturatingDefectKinetics",
            Self::Ageing => "implexity.physics_library.environmental_ageing.EnvironmentalAgeing",
            Self::Rupture => "implexity.physics_library.mechanical_ageing.CreepRuptureHistory",
            Self::Oxidation => {
                "implexity.physics_library.mechanical_ageing.ParabolicOxidationHistory"
            }
            Self::Species => {
                "implexity.physics_library.species_retention.SaturatingSpeciesRetention"
            }
            Self::Composite => {
                "implexity.physics_library.composite_material_history.CompositeMaterialHistory"
            }
        }
    }

    #[must_use]
    pub fn limitations(self) -> &'static [&'static str] {
        match self {
            Self::Defect => &defect::LIMITATIONS,
            Self::Ageing => &ageing::LIMITATIONS,
            Self::Rupture | Self::Oxidation => &mechanical_ageing::LIMITATIONS,
            Self::Species => &species::LIMITATIONS,
            Self::Composite => &composite::LIMITATIONS,
        }
    }

    #[must_use]
    pub fn runtime_support(self) -> Map<String, Value> {
        obj(
            json!({"status": "field_component", "history": true, "data": "user_required",
            "limitations": self.limitations()}),
        )
    }

    #[must_use]
    pub fn authoring_contract(self) -> Map<String, Value> {
        obj(match self {
            Self::Defect => defect::authoring_contract(),
            Self::Ageing => ageing::authoring_contract(),
            Self::Rupture => mechanical_ageing::Kind::Rupture.authoring(),
            Self::Oxidation => mechanical_ageing::Kind::Oxidation.authoring(),
            Self::Species => species::authoring_contract(),
            Self::Composite => composite::authoring_contract(),
        })
    }

    #[must_use]
    pub fn editor_label(self) -> &'static str {
        match self {
            Self::Defect => "Defect production and recovery",
            Self::Ageing => "Thermal and environmental ageing",
            Self::Rupture => "Creep rupture life",
            Self::Oxidation => "Surface oxidation",
            Self::Species => "Local species retention and thermal release",
            Self::Composite => "Simultaneous independent material histories",
        }
    }

    pub fn editor_schema(self, settings: &Value, context: &Value) -> Result<Value, CaeError> {
        Ok(match self {
            Self::Defect => defect::editor_schema(settings, context),
            Self::Ageing => ageing::editor_schema(settings, context),
            Self::Rupture => mechanical_ageing::Kind::Rupture.editor(),
            Self::Oxidation => mechanical_ageing::Kind::Oxidation.editor(),
            Self::Species => species::editor_schema(settings, context),
            Self::Composite => composite::editor_schema(settings, context)?,
        })
    }

    #[must_use]
    pub fn property_effects(self) -> [&'static str; 3] {
        ["k", "yield_stress", "creep_rate_ref"]
    }

    #[must_use]
    pub fn composition_contract(self) -> Option<&'static str> {
        match self {
            Self::Composite => None,
            _ => Some(composite::CONTRACT),
        }
    }

    #[must_use]
    pub fn numerical_extension_contract_id(self) -> Option<&'static str> {
        match self {
            Self::Ageing | Self::Rupture | Self::Oxidation => None,
            _ => Some(EXTENSION_CONTRACT),
        }
    }

    #[must_use]
    pub fn external_forcing_contract(self) -> Option<Value> {
        (self == Self::Defect).then(defect::external_forcing_contract)
    }

    #[must_use]
    pub fn state_dependencies(self) -> &'static [&'static str] {
        match self {
            Self::Composite | Self::Rupture => &["temperature", "stress"],
            _ => &["temperature"],
        }
    }

    pub fn state_dependencies_for(self, settings: &Value) -> Result<Vec<String>, CaeError> {
        match self {
            Self::Composite => composite::state_dependencies_for(settings),
            _ => Ok(self
                .state_dependencies()
                .iter()
                .map(|s| (*s).to_string())
                .collect()),
        }
    }

    pub fn validate(self, settings: &Value, context: &Value) -> Result<Value, CaeError> {
        match self {
            Self::Defect => defect::validate(settings, context),
            Self::Ageing => ageing::validate(settings, context),
            Self::Rupture => mechanical_ageing::Kind::Rupture.validate(settings, context),
            Self::Oxidation => mechanical_ageing::Kind::Oxidation.validate(settings, context),
            Self::Species => species::validate(settings, context),
            Self::Composite => composite::validate(settings, context),
        }
    }

    pub fn bind_law(self, settings: &Value) -> Result<HistoryLaw, CaeError> {
        Ok(match self {
            Self::Defect => HistoryLaw::Defect(defect::DefectKinetics::bind(settings)),
            Self::Ageing => HistoryLaw::Ageing(ageing::EnvironmentalAgeing::bind(settings)),
            Self::Rupture => HistoryLaw::Mechanical(mechanical_ageing::MechanicalAgeing::bind(
                mechanical_ageing::Kind::Rupture,
                settings,
            )),
            Self::Oxidation => HistoryLaw::Mechanical(mechanical_ageing::MechanicalAgeing::bind(
                mechanical_ageing::Kind::Oxidation,
                settings,
            )),
            Self::Species => HistoryLaw::Species(species::SpeciesRetention::bind(settings)),
            Self::Composite => HistoryLaw::Composite(composite::bind(settings)?),
        })
    }

    #[must_use]
    pub fn solid_study_templates(self, solid: &Value) -> Vec<Value> {
        match self {
            Self::Species => species::solid_study_templates(solid),
            _ => Vec::new(),
        }
    }
}

impl EndpointNumericalExtension for HistoryComponent {
    fn numerical_extension_contract(&self) -> Option<&str> {
        self.numerical_extension_contract_id()
    }
    fn declares_endpoint_continuation(&self) -> bool {
        !matches!(self, Self::Ageing | Self::Rupture | Self::Oxidation)
    }
    fn validate_numerical_extension(
        &self,
        settings: &Value,
        context: &Value,
    ) -> Result<(), CaeError> {
        self.bind_law(settings)?
            .validate_numerical_extension(settings, context)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HistoryLaw {
    Defect(defect::DefectKinetics),
    Ageing(ageing::EnvironmentalAgeing),
    Mechanical(mechanical_ageing::MechanicalAgeing),
    Species(species::SpeciesRetention),
    Composite(Vec<Member>),
}

impl HistoryLaw {
    #[must_use]
    pub fn state_metadata(&self) -> Vec<Value> {
        match self {
            Self::Defect(l) => l.state_metadata(),
            Self::Ageing(l) => l.state_metadata(),
            Self::Mechanical(l) => l.state_metadata(),
            Self::Species(l) => l.state_metadata(),
            Self::Composite(members) => composite::state_metadata(members),
        }
    }

    #[must_use]
    pub fn forcing(&self, settings: &Value) -> (Vec<usize>, Vec<f64>) {
        match self {
            Self::Defect(_) => real_array(&settings["dose_rate_dpa_s"]).unwrap_or_default(),
            Self::Ageing(_) => real_array(&settings["activities"]).unwrap_or_default(),
            Self::Mechanical(_) => real_array(&settings["exposure"]).unwrap_or_default(),
            Self::Species(_) => {
                real_array(&settings["source_atomic_fraction_s_inv"]).unwrap_or_default()
            }
            Self::Composite(members) => composite::forcing(members, settings),
        }
    }

    #[must_use]
    pub fn state_endpoints(&self) -> Option<Vec<usize>> {
        match self {
            Self::Defect(_) | Self::Species(_) | Self::Mechanical(_) => Some(vec![0, 1]),
            Self::Ageing(_) => None,
            Self::Composite(members) => {
                let mut out = Vec::new();
                for m in members {
                    out.extend(m.law.state_endpoints()?);
                }
                Some(out)
            }
        }
    }

    pub fn validate_numerical_extension(
        &self,
        settings: &Value,
        context: &Value,
    ) -> Result<(), CaeError> {
        match self {
            Self::Defect(l) => l.validate_numerical_extension(settings, context),
            Self::Species(l) => l.validate_numerical_extension(settings, context),
            Self::Ageing(_) | Self::Mechanical(_) => {
                contract("this history declares no endpoint numerical extension")
            }
            Self::Composite(members) => {
                for (m, row) in members
                    .iter()
                    .zip(settings["members"].as_array().into_iter().flatten())
                {
                    if m.component.numerical_extension_contract_id() != Some(EXTENSION_CONTRACT) {
                        return contract(
                            "composite member lacks endpoint-local numerical extension",
                        );
                    }
                    m.law
                        .validate_numerical_extension(&row["settings"], context)?;
                }
                Ok(())
            }
        }
    }

    pub fn supports_closed_inventory_step(&self) -> bool {
        match self {
            Self::Defect(_) | Self::Species(_) => true,
            Self::Composite(members) => members
                .iter()
                .all(|member| member.law.supports_closed_inventory_step()),
            Self::Ageing(_) | Self::Mechanical(_) => false,
        }
    }

    pub fn properties_at_temperature<S: Scalar>(
        &self,
        endpoint: usize,
        state: &[S],
        base: [S; 3],
        temperature: S,
    ) -> [S; 3] {
        match self {
            Self::Defect(law) => law.properties_at_temperature(endpoint, state, base, temperature),
            Self::Composite(members) => {
                let mut out = base;
                for member in members {
                    let values = member.law.properties_at_temperature(
                        endpoint,
                        &state[member.state.clone()],
                        base,
                        temperature,
                    );
                    for i in 0..3 {
                        out[i] *= if base[i].value() == 0.0 {
                            S::one()
                        } else {
                            values[i] / base[i]
                        };
                    }
                }
                out
            }
            _ => self.properties(endpoint, state, base),
        }
    }

    pub fn properties<S: Scalar>(&self, endpoint: usize, state: &[S], base: [S; 3]) -> [S; 3] {
        match self {
            Self::Defect(l) => l.properties(endpoint, state, base),
            Self::Ageing(l) => l.properties(endpoint, state, base),
            Self::Mechanical(l) => l.properties(endpoint, state, base),
            Self::Species(l) => l.properties(endpoint, state, base),
            Self::Composite(members) => composite::properties(members, endpoint, state, base),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn residual<S: Scalar>(
        &self,
        state: &[S],
        previous: &[S],
        temps: [S; 2],
        stress: &crate::mandel::Mandel<S>,
        dt: S,
        forcing: &[S],
        out: &mut [S],
    ) {
        match self {
            Self::Defect(l) => l.residual(state, previous, temps, dt, forcing, out),
            Self::Ageing(l) => l.residual(state, previous, temps[0], dt, forcing, out),
            Self::Mechanical(l) => l.residual(state, previous, temps, stress, dt, forcing, out),
            Self::Species(l) => l.residual(state, previous, temps, dt, forcing, out),
            Self::Composite(members) => {
                composite::residual(members, state, previous, temps, stress, dt, forcing, out);
            }
        }
    }

    pub fn energy<S: Scalar>(
        &self,
        state: &[S],
        temps: [S; 2],
        forcing: &[S],
        c: S,
        densities: [f64; 2],
    ) -> HistoryEnergy<S> {
        match self {
            Self::Defect(l) => l.energy(state, temps, forcing, c, densities),
            Self::Ageing(l) => l.energy(state, temps[0], forcing, c, densities),
            Self::Mechanical(l) => l.energy(),
            Self::Species(l) => l.energy(),
            Self::Composite(members) => {
                composite::energy(members, state, temps, forcing, c, densities)
            }
        }
    }

    pub fn check_state(&self, state: &[f64], width: usize) -> Result<(), CaeError> {
        match self {
            Self::Defect(l) => l.check_state(state, width),
            Self::Ageing(l) => l.check_state(state, width),
            Self::Mechanical(l) => l.check_state(state, width),
            Self::Species(l) => l.check_state(state, width),
            Self::Composite(members) => composite::check_state(members, state, width),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterialHistoryBinding {
    pub name: String,
    pub component: HistoryComponent,
    pub settings: Value,
    pub law: HistoryLaw,
    pub dependencies: Vec<String>,
    pub effects: [&'static str; 3],
    pub metadata: Vec<Value>,
    pub size: usize,
    pub scales: Vec<f64>,
    pub initial: Vec<f64>,
    pub forcing_values: Vec<f64>,
    pub forcing_shape: [usize; 3],
    pub numerical_extension: Option<Value>,
    pub endpoints: Option<Vec<usize>>,
    pub material_intervals: [(f64, f64); 2],
    pub densities: [f64; 2],
}

pub fn selected(name: &str) -> Result<HistoryComponent, CaeError> {
    crate::components::selected_history(name)
}

impl MaterialHistoryBinding {
    pub fn new(
        name: &str,
        component: HistoryComponent,
        settings: Value,
        context: &Value,
    ) -> Result<Self, CaeError> {
        let dependencies = component.state_dependencies_for(&settings)?;
        let mut unique = dependencies.clone();
        unique.sort();
        unique.dedup();
        if unique.len() != dependencies.len()
            || dependencies
                .iter()
                .any(|d| d != "temperature" && d != "stress")
        {
            return contract("material history declares unsupported driving state");
        }
        let effects = component.property_effects();
        let law = component.bind_law(&settings)?;
        let rows = law.state_metadata();
        if rows.is_empty() {
            return contract("empty material history layout");
        }
        let mut names: Vec<String> = Vec::new();
        for r in &rows {
            let ok = has_exact_keys(r, &["initial", "name", "scale", "units"])
                && ["name", "units"].iter().all(|k| crate::util::text(&r[*k]));
            if !ok {
                return contract("material history state needs name, units, scale and initial");
            }
            let n = r["name"].as_str().unwrap_or_default().to_string();
            if names.contains(&n) {
                return contract("duplicate material history state name");
            }
            names.push(n);
            let scale = crate::util::num(&r["scale"]);
            let init = crate::util::num(&r["initial"]);
            if scale.is_none() || init.is_none() || scale.is_some_and(|s| s <= 0.0) {
                return contract("invalid material state scale/initial value");
            }
        }
        let scales: Vec<f64> = rows
            .iter()
            .map(|r| r["scale"].as_f64().unwrap_or(1.0))
            .collect();
        let initial: Vec<f64> = rows
            .iter()
            .map(|r| r["initial"].as_f64().unwrap_or(0.0))
            .collect();
        law.check_state(&initial, initial.len())?;
        let nt = crate::util::time_count(context);
        let nc = crate::util::grid_cells(context);
        let (shape, forcing) = law.forcing(&settings);
        if shape.len() != 3
            || shape[0] != nt
            || shape[1] != nc
            || shape[2] < 1
            || forcing.iter().any(|v| !v.is_finite())
        {
            return contract("constitutive forcing must have finite shape (times,cells,channels)");
        }
        let policy = context
            .get("material_history_numerical_extension")
            .filter(|v| !v.is_null());
        let inactive = context
            .get("inactive_phase_numerical_material")
            .is_some_and(|v| !v.is_null());
        if inactive && policy.is_none() {
            return contract(
                "material history with inactive-phase material requires explicit material_history_numerical_extension",
            );
        }
        let numerical_extension = match policy {
            Some(p) => Some(extension::validate_policy(
                p, context, &component, &settings,
            )?),
            None => None,
        };
        let endpoints = law.state_endpoints();
        if let Some(e) = &endpoints
            && (e.len() != rows.len() || e.iter().any(|i| *i > 1))
        {
            return contract("invalid endpoint ownership for material-history state");
        }
        let materials = &context["materials"];
        let interval = |i: usize| {
            (
                crate::util::f(&materials[i], "T_min"),
                crate::util::f(&materials[i], "T_max"),
            )
        };
        let density = |i: usize| crate::util::f(&materials[i], "density");
        Ok(Self {
            name: name.to_string(),
            component,
            settings,
            law,
            dependencies,
            effects,
            size: rows.len(),
            metadata: rows,
            scales,
            initial,
            forcing_values: forcing,
            forcing_shape: [shape[0], shape[1], shape[2]],
            numerical_extension,
            endpoints,
            material_intervals: [interval(0), interval(1)],
            densities: [density(0), density(1)],
        })
    }

    #[must_use]
    pub fn forcing_row(&self, step: usize, cell: usize) -> &[f64] {
        let ch = self.forcing_shape[2];
        let start = (step * self.forcing_shape[1] + cell) * ch;
        &self.forcing_values[start..start + ch]
    }

    fn temps<S: Scalar>(&self, temperature: S) -> [S; 2] {
        if self.numerical_extension.is_none() {
            [temperature, temperature]
        } else {
            let [(a0, a1), (b0, b1)] = self.material_intervals;
            [
                extension::endpoint_temperature(temperature, a0, a1),
                extension::endpoint_temperature(temperature, b0, b1),
            ]
        }
    }

    pub fn residual<S: Scalar>(
        &self,
        state: &[S],
        previous: &[S],
        temperature: S,
        stress: &crate::mandel::Mandel<S>,
        dt: S,
        forcing: &[S],
        out: &mut [S],
    ) {
        self.law.residual(
            state,
            previous,
            self.temps(temperature),
            stress,
            dt,
            forcing,
            out,
        );
    }

    pub fn energy<S: Scalar>(
        &self,
        state: &[S],
        temperature: S,
        forcing: &[S],
        composition: S,
    ) -> HistoryEnergy<S> {
        self.law.energy(
            state,
            self.temps(temperature),
            forcing,
            composition,
            self.densities,
        )
    }

    pub fn properties<S: Scalar>(&self, endpoint: usize, state: &[S], base: &Props<S>) -> Props<S> {
        let values = [
            base.get(idx::K),
            base.get(idx::YIELD),
            base.get(idx::CREEP_RATE),
        ];
        let [k, y, c] =
            self.law
                .properties_at_temperature(endpoint, state, values, base.temperature);
        let mut out = base.clone();
        out.values[idx::K] = k;
        out.values[idx::YIELD] = y;
        out.values[idx::CREEP_RATE] = c;
        let factor = self.law.properties_at_temperature(
            endpoint,
            state,
            [base.get(idx::K), base.get(idx::YIELD), S::one()],
            base.temperature,
        )[2];
        out.creep_multiplier *= factor;
        out
    }

    pub fn physical_support(&self, density: f64, composition: f64) -> Result<Vec<f64>, CaeError> {
        let Some(endpoints) = &self.endpoints else {
            return contract("material history has no declared endpoint ownership");
        };
        Ok(endpoints
            .iter()
            .map(|i| {
                density
                    * if *i == 0 {
                        1.0 - composition
                    } else {
                        composition
                    }
            })
            .collect())
    }

    #[must_use]
    pub fn numerical_extension_report(&self) -> Value {
        json!({"enabled": self.numerical_extension.is_some(),
            "policy": self.numerical_extension,
            "physical_material_extrapolation_authorized": false,
            "kinetic_argument_scope": "identity_on_physical_interval; C1_numerical_tails_only",
            "state_endpoints": self.endpoints,
            "state_semantics": "potential_inventories_require_physical_support",
            "energy_assembly": "endpoint_fraction_in_law; physical_solid_fraction_in_host_once"})
    }

    pub fn check_state(&self, state: &[f64]) -> Result<(), CaeError> {
        self.law.check_state(state, self.size)
    }
}

pub fn declaration(row: &Value, context: &Value) -> Result<Option<Value>, CaeError> {
    if row.is_null() {
        return Ok(None);
    }
    if !has_exact_keys(row, &["component", "settings"]) || !row["component"].is_string() {
        return contract("material_history requires explicit component and settings");
    }
    let name = row["component"].as_str().unwrap_or_default();
    let component = selected(name)?;
    let config = component.validate(&row["settings"], context)?;
    let binding = MaterialHistoryBinding::new(name, component, config, context)?;
    Ok(Some(
        json!({"component": name, "settings": binding.settings}),
    ))
}

pub fn bind(row: &Value, context: &Value) -> Result<Option<MaterialHistoryBinding>, CaeError> {
    let Some(row) = declaration(row, context)? else {
        return Ok(None);
    };
    let name = row["component"].as_str().unwrap_or_default();
    let component = selected(name)?;
    Ok(Some(MaterialHistoryBinding::new(
        name,
        component,
        row["settings"].clone(),
        context,
    )?))
}

#[must_use]
pub fn editor_schema(context: &Value) -> Value {
    if !context.is_object() {
        return json!({});
    }
    let mut properties = Map::new();
    for (field, kind, label) in [
        (
            "material_history",
            "material_state_evolution",
            "Material-state component",
        ),
        (
            "viscoelasticity",
            "viscoelastic_solid",
            "Viscoelastic component",
        ),
        (
            "fatigue_observer",
            "fatigue_history_observer",
            "Fatigue observer",
        ),
    ] {
        let Some(row) = context.get(field).filter(|r| r.is_object()) else {
            continue;
        };
        let Some(name) = row.get("component").and_then(Value::as_str) else {
            continue;
        };
        let Some((title, schema)) =
            crate::components::editor_schema_hook(name, kind, &row["settings"], context)
        else {
            continue;
        };
        if schema.as_object().is_some_and(|m| !m.is_empty()) {
            properties.insert(
                field.into(),
                json!({"title": title, "properties": {"component": {"title": label, "enum": [name]}, "settings": schema}}),
            );
        }
    }
    if context
        .get("material_history")
        .is_some_and(|v| !v.is_null())
    {
        properties.insert("material_history_numerical_extension".into(), json!({
            "title": "Absent-material kinetic continuation (explicit opt-in)",
            "description": "Requires phase-aware material validity. No physical temperature extrapolation; virtual inventories require exact support fields.",
            "type": "object", "properties": {
                "schema": {"const": extension::SCHEMA}, "method": {"enum": [extension::METHOD]},
                "state_semantics": {"enum": [extension::STATE_SEMANTICS]}, "energy_semantics": {"enum": [extension::ENERGY_SEMANTICS]},
                "provenance": {"title": "Numerical extension justification", "type": "string", "minLength": 1}},
            "required": ["schema", "method", "state_semantics", "energy_semantics", "provenance"]}));
    }
    if properties.is_empty() {
        json!({})
    } else {
        json!({"properties": properties})
    }
}

pub mod aged_condition;

pub mod stress_relaxing_age;
