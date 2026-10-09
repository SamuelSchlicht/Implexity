// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::HistoryEnergy;
use crate::mandel::{Mandel, equivalent};
use crate::util::{
    contract, convergence, f, grid_cells, has_exact_keys, real_array, text, time_count,
};
use implexity_ad::Scalar;
use implexity_core::CaeError;
use serde_json::{Value, json};

pub const LIMITATIONS: [&str; 4] = [
    "Calibrated effective property factors; no elastic damage, fracture, crack propagation or geometry recession.",
    "Rupture uses a stress and temperature dependent time-fraction rule; multiaxial life requires separate calibration.",
    "Oxidation uses parabolic oxygen mass gain and a prescribed local surface-exposure fraction; no oxygen transport or scale spallation.",
    "Property modifiers require joint calibration; oxidation heat and mass exchange are authored separately.",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Rupture,
    Oxidation,
}
impl Kind {
    pub fn fields(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Rupture => &[
                ("reference_life_s", "s"),
                ("reference_stress_Pa", "Pa"),
                ("stress_exponent", "1"),
                ("activation_J_mol", "J/mol"),
                ("reference_temperature_K", "K"),
                ("minimum_temperature_K", "K"),
                ("maximum_temperature_K", "K"),
                ("maximum_stress_Pa", "Pa"),
                ("initial_fraction", "1"),
                ("conductivity_remaining", "1"),
                ("yield_remaining", "1"),
                ("creep_log_multiplier", "1"),
            ],
            Self::Oxidation => &[
                ("parabolic_mass_gain_kg2_m4_s", "kg^2/m^4/s"),
                ("reference_mass_gain_kg_m2", "kg/m^2"),
                ("activation_J_mol", "J/mol"),
                ("reference_temperature_K", "K"),
                ("minimum_temperature_K", "K"),
                ("maximum_temperature_K", "K"),
                ("initial_fraction", "1"),
                ("conductivity_remaining", "1"),
                ("yield_remaining", "1"),
                ("creep_log_multiplier", "1"),
            ],
        }
    }
    pub fn authoring(self) -> Value {
        json!({"schema":"implexity-component-authoring/1","selection":"solid.material_history",
            "required_settings":["name","provenance","endmembers","exposure","units"],
            "endmember_parameter_units":self.fields().iter().map(|(k,u)|((*k).to_string(),json!(u))).collect::<serde_json::Map<_,_>>(),
            "exposure_shape":["len(times_s)","product(grid)",2],
            "exposure_channels":["material_0_exposure","material_1_exposure"],
            "exposure_bounds":[0,1],"sampling":"backward Euler right endpoint",
            "evolution":match self { Self::Rupture=>"dD/dt = exposure / t_rupture(stress,T)", Self::Oxidation=>"dU/dt = exposure kp(T)/w_ref^2; U = oxygen_mass_gain^2/w_ref^2" },
            "property_extent":match self { Self::Rupture=>"D", Self::Oxidation=>"U/(1+U)" },
            "calibrated_material_data_supplied":false,"limitations":LIMITATIONS})
    }
    pub fn editor(self) -> Value {
        let fields = self
            .fields()
            .iter()
            .map(|(k, u)| {
                (
                    (*k).to_string(),
                    json!({"type":"number","title":k.replace('_'," "),"units":u}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        json!({"properties":{"name":{"type":"string","title":"Model name"},"provenance":{"type":"string","title":"Joint calibration evidence"},"endmembers":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"object","properties":fields}},"exposure":{"type":"array","title":"Local exposure history","items":{"type":"array","items":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"number","minimum":0,"maximum":1}}}}}})
    }
    pub fn validate(self, settings: &Value, context: &Value) -> Result<Value, CaeError> {
        if !has_exact_keys(
            settings,
            &["name", "provenance", "endmembers", "exposure", "units"],
        ) || !text(&settings["name"])
            || !text(&settings["provenance"])
        {
            return contract(
                "mechanical ageing requires name, provenance, endmembers, exposure and units",
            );
        }
        if context.get("viscoelasticity").is_some_and(|v| !v.is_null()) {
            return contract("mechanical ageing does not implement Maxwell storage interactions");
        }
        let units = Value::Object(
            self.fields()
                .iter()
                .map(|(k, u)| ((*k).to_string(), json!(u)))
                .collect(),
        );
        if settings["units"] != units {
            return contract("mechanical ageing coefficient units do not match its contract");
        }
        let endpoints = settings["endmembers"]
            .as_array()
            .filter(|e| e.len() == 2)
            .ok_or_else(|| {
                CaeError::contract("mechanical ageing requires two material endpoints")
            })?;
        let keys = self.fields().iter().map(|(k, _)| *k).collect::<Vec<_>>();
        for (i, e) in endpoints.iter().enumerate() {
            if !has_exact_keys(e, &keys)
                || keys
                    .iter()
                    .any(|k| !e[*k].as_f64().is_some_and(f64::is_finite))
            {
                return contract("mechanical ageing coefficients must be finite and complete");
            }
            let lo = f(e, "minimum_temperature_K");
            let hi = f(e, "maximum_temperature_K");
            let tr = f(e, "reference_temperature_K");
            if lo <= 0.
                || hi < lo
                || tr < lo
                || tr > hi
                || f(&context["materials"][i], "T_min") < lo
                || f(&context["materials"][i], "T_max") > hi
            {
                return contract("material temperature interval exceeds ageing calibration");
            }
            if f(e, "activation_J_mol") < 0.
                || f(e, "initial_fraction") < 0.
                || ["conductivity_remaining", "yield_remaining"]
                    .iter()
                    .any(|k| f(e, k) <= 0. || f(e, k) > 1.)
                || f(e, "creep_log_multiplier").abs() > 50.
            {
                return contract("invalid mechanical ageing property factors");
            }
            match self {
                Self::Rupture
                    if f(e, "reference_life_s") <= 0.
                        || f(e, "reference_stress_Pa") <= 0.
                        || f(e, "stress_exponent") < 1.
                        || f(e, "maximum_stress_Pa") < f(e, "reference_stress_Pa")
                        || f(e, "initial_fraction") >= 1. =>
                {
                    return contract("invalid rupture-life calibration");
                }
                Self::Oxidation
                    if f(e, "parabolic_mass_gain_kg2_m4_s") < 0.
                        || f(e, "reference_mass_gain_kg_m2") <= 0. =>
                {
                    return contract("invalid parabolic oxidation calibration");
                }
                _ => {}
            }
        }
        for e in endpoints {
            for t in [f(e, "minimum_temperature_K"), f(e, "maximum_temperature_K")] {
                let thermal = (-(1. / t - 1. / f(e, "reference_temperature_K"))
                    * (f(e, "activation_J_mol") / super::ageing::R_GAS))
                    .exp();
                let r = match self {
                    Self::Rupture => {
                        (f(e, "maximum_stress_Pa") / f(e, "reference_stress_Pa"))
                            .powf(f(e, "stress_exponent"))
                            * thermal
                            / f(e, "reference_life_s")
                    }
                    Self::Oxidation => {
                        thermal * f(e, "parabolic_mass_gain_kg2_m4_s")
                            / f(e, "reference_mass_gain_kg_m2").powi(2)
                    }
                };
                if !thermal.is_finite()
                    || !r.is_finite()
                    || (r == 0.
                        && (self == Self::Rupture || f(e, "parabolic_mass_gain_kg2_m4_s") > 0.))
                {
                    return contract(
                        "ageing calibration rate overflows or underflows its declared interval",
                    );
                }
            }
        }
        let (shape, values) = real_array(&settings["exposure"])
            .ok_or_else(|| CaeError::contract("invalid ageing exposure history"))?;
        if shape != [time_count(context), grid_cells(context), 2]
            || values.iter().any(|x| !x.is_finite() || *x < 0. || *x > 1.)
        {
            return contract("ageing exposure requires [time,cell,endpoint] values in [0,1]");
        }
        Ok(settings.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MechanicalAgeing {
    pub kind: Kind,
    pub endpoints: [Value; 2],
}
impl MechanicalAgeing {
    pub fn bind(kind: Kind, settings: &Value) -> Self {
        Self {
            kind,
            endpoints: std::array::from_fn(|i| settings["endmembers"][i].clone()),
        }
    }
    pub fn state_metadata(&self) -> Vec<Value> {
        (0..2).map(|i|json!({"name":format!("{}_endpoint_{i}",match self.kind {Kind::Rupture=>"rupture_life_fraction",Kind::Oxidation=>"oxidation_mass_gain_squared_fraction"}),"units":"1","scale":1.,"initial":f(&self.endpoints[i],"initial_fraction")})).collect()
    }
    pub fn properties<S: Scalar>(&self, endpoint: usize, state: &[S], base: [S; 3]) -> [S; 3] {
        let e = &self.endpoints[endpoint];
        let u = state[endpoint];
        let extent = match self.kind {
            Kind::Rupture => u,
            Kind::Oxidation => u / (u + 1.),
        };
        [
            base[0] * (-extent * (1. - f(e, "conductivity_remaining")) + 1.),
            base[1] * (-extent * (1. - f(e, "yield_remaining")) + 1.),
            base[2] * (extent * f(e, "creep_log_multiplier")).exp(),
        ]
    }
    pub fn residual<S: Scalar>(
        &self,
        state: &[S],
        previous: &[S],
        temps: [S; 2],
        stress: &Mandel<S>,
        dt: S,
        forcing: &[S],
        out: &mut [S],
    ) {
        let q = equivalent(stress);
        for i in 0..2 {
            let e = &self.endpoints[i];
            let t = temps[i];
            let thermal = (-(S::one() / t - 1. / f(e, "reference_temperature_K"))
                * (f(e, "activation_J_mol") / super::ageing::R_GAS))
                .exp();
            let valid = t.value() >= f(e, "minimum_temperature_K")
                && t.value() <= f(e, "maximum_temperature_K");
            let rate = match self.kind {
                Kind::Rupture if valid && q.value() <= f(e, "maximum_stress_Pa") => {
                    (q / f(e, "reference_stress_Pa")).powf(f(e, "stress_exponent")) * thermal
                        / f(e, "reference_life_s")
                }
                Kind::Oxidation if valid => {
                    thermal
                        * (f(e, "parabolic_mass_gain_kg2_m4_s")
                            / f(e, "reference_mass_gain_kg_m2").powi(2))
                }
                _ => S::from_f64(f64::NAN),
            };
            out[i] = state[i] - previous[i] - dt * forcing[i] * rate;
        }
    }
    pub fn energy<S: Scalar>(&self) -> HistoryEnergy<S> {
        HistoryEnergy::zero()
    }
    pub fn check_state(&self, state: &[f64], width: usize) -> Result<(), CaeError> {
        if width != 2
            || !state.len().is_multiple_of(width)
            || state
                .iter()
                .any(|v| !v.is_finite() || *v < -1e-9 || (self.kind == Kind::Rupture && *v >= 1.))
        {
            return convergence("mechanical ageing state invalid or rupture life exhausted");
        }
        Ok(())
    }
}
