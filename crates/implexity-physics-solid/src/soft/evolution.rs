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

use implexity_ad::Scalar;
use implexity_core::CaeError;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvolutionState<S> {
    pub damage: S,
    pub remodelling: S,
    pub hydration: S,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvolutionRates {
    pub damage_accumulation_per_s: f64,
    pub damage_repair_per_s: f64,
    pub remodelling_per_s: f64,
    pub remodelling_target: f64,
    pub hydration_per_s: f64,
    pub hydration_target: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvolutionSensitivity {
    pub matrix_stiffness: [f64; 3],
    pub fibre_stiffness: [f64; 3],
    pub matrix_beta: [f64; 3],
    pub matrix_tau: [f64; 3],
    pub fibre_beta: [f64; 3],
    pub fibre_tau: [f64; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvolutionModifiers<S> {
    pub matrix_stiffness: S,
    pub fibre_stiffness: S,
    pub matrix_beta: S,
    pub matrix_tau: S,
    pub fibre_beta: S,
    pub fibre_tau: S,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrescribedEvolution {
    pub initial: EvolutionState<f64>,
    pub reference: EvolutionState<f64>,
    pub rates: EvolutionRates,
    pub sensitivity: EvolutionSensitivity,
    pub interval_s: f64,
    pub provenance: String,
}

fn invalid(message: &str) -> CaeError {
    CaeError::contract(message)
}

impl<S: Scalar> EvolutionState<S> {
    pub fn validate(&self) -> Result<(), CaeError> {
        if [self.damage, self.remodelling, self.hydration]
            .iter()
            .all(|x| x.value().is_finite() && (0.0..=1.0).contains(&x.value()))
        {
            Ok(())
        } else {
            Err(invalid(
                "evolution states must be finite fractions in [0, 1]",
            ))
        }
    }
}

impl EvolutionRates {
    pub fn validate(&self) -> Result<(), CaeError> {
        if [
            self.damage_accumulation_per_s,
            self.damage_repair_per_s,
            self.remodelling_per_s,
            self.hydration_per_s,
        ]
        .iter()
        .any(|x| !x.is_finite() || *x < 0.0)
            || [self.remodelling_target, self.hydration_target]
                .iter()
                .any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
        {
            return Err(invalid(
                "evolution requires nonnegative finite rates and targets in [0, 1]",
            ));
        }
        if !(self.damage_accumulation_per_s + self.damage_repair_per_s).is_finite() {
            return Err(invalid("combined damage and repair rate must be finite"));
        }
        Ok(())
    }

    pub fn advance<S: Scalar>(
        &self,
        previous: EvolutionState<S>,
        interval_s: S,
    ) -> Result<EvolutionState<S>, CaeError> {
        self.validate()?;
        previous.validate()?;
        if !interval_s.value().is_finite() || interval_s.value() < 0.0 {
            return Err(invalid("evolution interval must be finite and nonnegative"));
        }
        if [
            self.damage_accumulation_per_s + self.damage_repair_per_s,
            self.remodelling_per_s,
            self.hydration_per_s,
        ]
        .iter()
        .any(|rate| !(interval_s.value() * rate).is_finite())
        {
            return Err(invalid(
                "dimensionless evolution exposure exceeds the finite arithmetic domain",
            ));
        }
        let relax = |initial: S, target: f64, rate: f64| {
            initial + (initial - target) * (-interval_s * rate).exp_m1()
        };
        let rate = self.damage_accumulation_per_s + self.damage_repair_per_s;
        let damage = if rate == 0.0 {
            previous.damage
        } else {
            relax(previous.damage, self.damage_accumulation_per_s / rate, rate)
        };
        let state = EvolutionState {
            damage,
            remodelling: relax(
                previous.remodelling,
                self.remodelling_target,
                self.remodelling_per_s,
            ),
            hydration: relax(
                previous.hydration,
                self.hydration_target,
                self.hydration_per_s,
            ),
        };
        state.validate()?;
        Ok(state)
    }
}

impl EvolutionSensitivity {
    pub fn modifiers<S: Scalar>(
        &self,
        state: EvolutionState<S>,
        reference: EvolutionState<S>,
    ) -> Result<EvolutionModifiers<S>, CaeError> {
        state.validate()?;
        reference.validate()?;
        let delta = [
            state.damage - reference.damage,
            state.remodelling - reference.remodelling,
            state.hydration - reference.hydration,
        ];
        let factor = |coefficients: [f64; 3]| -> Result<S, CaeError> {
            if coefficients.iter().any(|v| !v.is_finite()) {
                return Err(invalid("evolution sensitivities must be finite"));
            }
            let value = (delta[0] * coefficients[0]
                + delta[1] * coefficients[1]
                + delta[2] * coefficients[2])
                .exp();
            if !value.value().is_finite() || value.value() <= 0.0 {
                return Err(invalid(
                    "evolution property factor is outside the positive finite range",
                ));
            }
            Ok(value)
        };
        Ok(EvolutionModifiers {
            matrix_stiffness: factor(self.matrix_stiffness)?,
            fibre_stiffness: factor(self.fibre_stiffness)?,
            matrix_beta: factor(self.matrix_beta)?,
            matrix_tau: factor(self.matrix_tau)?,
            fibre_beta: factor(self.fibre_beta)?,
            fibre_tau: factor(self.fibre_tau)?,
        })
    }
}

impl PrescribedEvolution {
    pub fn endpoint(&self) -> Result<(EvolutionState<f64>, EvolutionModifiers<f64>), CaeError> {
        if self.provenance.trim().is_empty() {
            return Err(invalid("evolution calibration provenance is required"));
        }
        let state = self.rates.advance(self.initial, self.interval_s)?;
        Ok((state, self.sensitivity.modifiers(state, self.reference)?))
    }

    pub fn from_json(value: &Value) -> Result<Self, CaeError> {
        fn keys(value: &Value, expected: &[&str]) -> Result<(), CaeError> {
            let Some(object) = value.as_object() else {
                return Err(invalid("evolution settings must be an object"));
            };
            if object.len() != expected.len()
                || expected.iter().any(|key| !object.contains_key(*key))
            {
                return Err(invalid("evolution settings have missing or unknown fields"));
            }
            Ok(())
        }
        fn number(value: &Value) -> Result<f64, CaeError> {
            value
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or_else(|| invalid("finite evolution coefficient required"))
        }
        fn state(value: &Value) -> Result<EvolutionState<f64>, CaeError> {
            keys(value, &["damage", "remodelling", "hydration"])?;
            Ok(EvolutionState {
                damage: number(&value["damage"])?,
                remodelling: number(&value["remodelling"])?,
                hydration: number(&value["hydration"])?,
            })
        }
        fn coefficients(value: &Value) -> Result<[f64; 3], CaeError> {
            let Some(a) = value.as_array().filter(|a| a.len() == 3) else {
                return Err(invalid(
                    "evolution sensitivity requires damage, remodelling and hydration coefficients",
                ));
            };
            Ok([number(&a[0])?, number(&a[1])?, number(&a[2])?])
        }
        if value.get("exposure_rates").is_some() {
            use super::evolution_exposure::{
                ExposureAccumulator, ExposureChannel, ExposureSample, RateCalibration,
            };
            use std::collections::BTreeMap;
            keys(
                value,
                &[
                    "initial",
                    "reference",
                    "exposure_rates",
                    "sensitivity",
                    "interval_s",
                    "provenance",
                ],
            )?;
            let exposure = &value["exposure_rates"];
            keys(
                exposure,
                &[
                    "duration_s",
                    "channels",
                    "damage_accumulation",
                    "damage_repair",
                    "remodelling",
                    "hydration",
                    "remodelling_target",
                    "hydration_target",
                ],
            )?;
            let duration = number(&exposure["duration_s"])?;
            if duration <= 0.0 {
                return Err(invalid("measured exposure requires positive duration"));
            }
            let rows = exposure["channels"]
                .as_array()
                .ok_or_else(|| invalid("measured exposure channels must be an array"))?;
            let mut channels = Vec::new();
            let mut samples = BTreeMap::new();
            for row in rows {
                keys(row, &["name", "unit", "integrated"])?;
                let name = row["name"]
                    .as_str()
                    .ok_or_else(|| invalid("exposure channel name is required"))?
                    .to_owned();
                let unit = row["unit"]
                    .as_str()
                    .ok_or_else(|| invalid("exposure channel unit is required"))?
                    .to_owned();
                let integral = number(&row["integrated"])?;
                if integral < 0.0 {
                    return Err(invalid("integrated exposure must be nonnegative"));
                }
                channels.push(ExposureChannel {
                    name: name.clone(),
                    unit: unit.clone(),
                });
                samples.insert(
                    name,
                    ExposureSample {
                        unit,
                        value: integral / duration,
                    },
                );
            }
            let mut accumulator = ExposureAccumulator::new(channels)?;
            accumulator.integrate_trapezoid(&samples, &samples, duration)?;
            let calibrated = |name: &str| -> Result<f64, CaeError> {
                let rate = &exposure[name];
                keys(
                    rate,
                    &[
                        "channel",
                        "unit",
                        "reference",
                        "baseline_per_s",
                        "amplitude_per_s",
                        "provenance",
                    ],
                )?;
                let text = |key: &str| {
                    rate[key].as_str().map(str::to_owned).ok_or_else(|| {
                        invalid("exposure calibration names, units and provenance are required")
                    })
                };
                RateCalibration {
                    channel: text("channel")?,
                    unit: text("unit")?,
                    reference: number(&rate["reference"])?,
                    baseline_per_s: number(&rate["baseline_per_s"])?,
                    amplitude_per_s: number(&rate["amplitude_per_s"])?,
                    provenance: text("provenance")?,
                }
                .rate(&accumulator)
            };
            let rates = serde_json::json!({"damage_accumulation_per_s":calibrated("damage_accumulation")?,"damage_repair_per_s":calibrated("damage_repair")?,"remodelling_per_s":calibrated("remodelling")?,"hydration_per_s":calibrated("hydration")?,"remodelling_target":number(&exposure["remodelling_target"])?,"hydration_target":number(&exposure["hydration_target"])?});
            let mut fixed = value.clone();
            let object = fixed
                .as_object_mut()
                .ok_or_else(|| invalid("evolution settings must be an object"))?;
            object.remove("exposure_rates");
            object.insert("rates".to_owned(), rates);
            return Self::from_json(&fixed);
        }
        keys(
            value,
            &[
                "initial",
                "reference",
                "rates",
                "sensitivity",
                "interval_s",
                "provenance",
            ],
        )?;
        let r = &value["rates"];
        keys(
            r,
            &[
                "damage_accumulation_per_s",
                "damage_repair_per_s",
                "remodelling_per_s",
                "remodelling_target",
                "hydration_per_s",
                "hydration_target",
            ],
        )?;
        let s = &value["sensitivity"];
        keys(
            s,
            &[
                "matrix_stiffness",
                "fibre_stiffness",
                "matrix_beta",
                "matrix_tau",
                "fibre_beta",
                "fibre_tau",
            ],
        )?;
        let result = Self {
            initial: state(&value["initial"])?,
            reference: state(&value["reference"])?,
            rates: EvolutionRates {
                damage_accumulation_per_s: number(&r["damage_accumulation_per_s"])?,
                damage_repair_per_s: number(&r["damage_repair_per_s"])?,
                remodelling_per_s: number(&r["remodelling_per_s"])?,
                remodelling_target: number(&r["remodelling_target"])?,
                hydration_per_s: number(&r["hydration_per_s"])?,
                hydration_target: number(&r["hydration_target"])?,
            },
            sensitivity: EvolutionSensitivity {
                matrix_stiffness: coefficients(&s["matrix_stiffness"])?,
                fibre_stiffness: coefficients(&s["fibre_stiffness"])?,
                matrix_beta: coefficients(&s["matrix_beta"])?,
                matrix_tau: coefficients(&s["matrix_tau"])?,
                fibre_beta: coefficients(&s["fibre_beta"])?,
                fibre_tau: coefficients(&s["fibre_tau"])?,
            },
            interval_s: number(&value["interval_s"])?,
            provenance: value["provenance"]
                .as_str()
                .ok_or_else(|| invalid("evolution calibration provenance is required"))?
                .to_owned(),
        };
        result.endpoint()?;
        Ok(result)
    }
}

pub fn prescribed_property_work<S: Scalar>(
    matrix_energy: S,
    fibre_energy: S,
    before: EvolutionModifiers<S>,
    after: EvolutionModifiers<S>,
) -> S {
    matrix_energy * (after.matrix_stiffness - before.matrix_stiffness)
        + fibre_energy * (after.fibre_stiffness - before.fibre_stiffness)
}

pub fn editor_schema() -> Value {
    use serde_json::json;
    let fraction = json!({"type":"number","minimum":0,"maximum":1});
    let rate = json!({"type":"number","minimum":0,"x-unit":"s-1"});
    let text = json!({"type":"string","minLength":1,"pattern":"\\S"});
    let object = |properties: Value, required: &[&str]| json!({"type":"object","additionalProperties":false,"properties":properties,"required":required});
    let state = object(
        json!({"damage":fraction,"remodelling":fraction,"hydration":fraction}),
        &["damage", "remodelling", "hydration"],
    );
    let coefficients = json!({"type":"array","minItems":3,"maxItems":3,"items":{"type":"number"},"x-order":["damage","remodelling","hydration"]});
    let sensitivity = object(
        json!({"matrix_stiffness":coefficients,"fibre_stiffness":coefficients,"matrix_beta":coefficients,"matrix_tau":coefficients,"fibre_beta":coefficients,"fibre_tau":coefficients}),
        &[
            "matrix_stiffness",
            "fibre_stiffness",
            "matrix_beta",
            "matrix_tau",
            "fibre_beta",
            "fibre_tau",
        ],
    );
    let fixed = object(
        json!({"damage_accumulation_per_s":rate,"damage_repair_per_s":rate,"remodelling_per_s":rate,"hydration_per_s":rate,"remodelling_target":fraction,"hydration_target":fraction}),
        &[
            "damage_accumulation_per_s",
            "damage_repair_per_s",
            "remodelling_per_s",
            "hydration_per_s",
            "remodelling_target",
            "hydration_target",
        ],
    );
    let calibration = object(
        json!({"channel":text,"unit":text,"reference":{"type":"number","exclusiveMinimum":0,"x-unit":"declared observable unit"},"baseline_per_s":rate,"amplitude_per_s":rate,"provenance":text}),
        &[
            "channel",
            "unit",
            "reference",
            "baseline_per_s",
            "amplitude_per_s",
            "provenance",
        ],
    );
    let channel = object(
        json!({"name":text,"unit":text,"integrated":{"type":"number","minimum":0,"x-unit":"declared observable unit times seconds"}}),
        &["name", "unit", "integrated"],
    );
    let exposure = object(
        json!({"duration_s":{"type":"number","exclusiveMinimum":0,"x-unit":"s"},"channels":{"type":"array","minItems":1,"items":channel,"x-unique-by":"name"},"damage_accumulation":calibration,"damage_repair":calibration,"remodelling":calibration,"hydration":calibration,"remodelling_target":fraction,"hydration_target":fraction}),
        &[
            "duration_s",
            "channels",
            "damage_accumulation",
            "damage_repair",
            "remodelling",
            "hydration",
            "remodelling_target",
            "hydration_target",
        ],
    );
    let mut schema = object(
        json!({"initial":state,"reference":state,"sensitivity":sensitivity,"interval_s":{"type":"number","minimum":0,"x-unit":"s"},"provenance":text,"rates":fixed,"exposure_rates":exposure}),
        &[
            "initial",
            "reference",
            "sensitivity",
            "interval_s",
            "provenance",
        ],
    );
    schema["oneOf"] = json!([{"required":["rates"],"not":{"required":["exposure_rates"]}},{"required":["exposure_rates"],"not":{"required":["rates"]}}]);
    schema["x-conditioning"] =
        json!("Initialize the material at a prescribed endpoint before a new fast history.");
    schema
}
