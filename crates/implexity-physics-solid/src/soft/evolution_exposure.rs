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

use super::evolution::EvolutionState;
use implexity_ad::Scalar;
use implexity_core::CaeError;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub struct ExposureChannel {
    pub name: String,
    pub unit: String,
}

#[derive(Clone, Debug)]
pub struct ExposureAccumulator<S> {
    pub duration_s: S,
    channels: Vec<ExposureChannel>,
    integrals: BTreeMap<String, S>,
}

#[derive(Clone, Debug)]
pub struct ExposureSample<S> {
    pub unit: String,
    pub value: S,
}

#[derive(Clone, Debug)]
pub struct RateCalibration {
    pub channel: String,
    pub unit: String,
    pub reference: f64,
    pub baseline_per_s: f64,
    pub amplitude_per_s: f64,
    pub provenance: String,
}

#[derive(Clone, Copy, Debug)]
pub struct ExposureRates<S> {
    pub damage_accumulation_per_s: S,
    pub damage_repair_per_s: S,
    pub remodelling_per_s: S,
    pub hydration_per_s: S,
    pub remodelling_target: f64,
    pub hydration_target: f64,
}

fn invalid(message: &str) -> CaeError {
    CaeError::contract(message)
}
fn nonnegative<S: Scalar>(v: S) -> bool {
    v.value().is_finite() && v.value() >= 0.0
}

impl<S: Scalar> ExposureAccumulator<S> {
    pub fn new(channels: Vec<ExposureChannel>) -> Result<Self, CaeError> {
        let mut integrals = BTreeMap::new();
        if channels.is_empty() {
            return Err(invalid("exposure requires named channels"));
        }
        for c in &channels {
            if c.name.trim().is_empty()
                || c.unit.trim().is_empty()
                || integrals.insert(c.name.clone(), S::zero()).is_some()
            {
                return Err(invalid(
                    "exposure channels require unique names and declared units",
                ));
            }
        }
        Ok(Self {
            duration_s: S::zero(),
            channels,
            integrals,
        })
    }

    pub fn integrate_trapezoid(
        &mut self,
        previous: &BTreeMap<String, ExposureSample<S>>,
        current: &BTreeMap<String, ExposureSample<S>>,
        duration_s: S,
    ) -> Result<(), CaeError> {
        if !nonnegative(duration_s)
            || previous.len() != self.channels.len()
            || current.len() != self.channels.len()
        {
            return Err(invalid(
                "exposure samples and duration do not match declared channels",
            ));
        }
        let mut next = self.integrals.clone();
        for c in &self.channels {
            let p = previous
                .get(&c.name)
                .ok_or_else(|| invalid("exposure sample channel is missing"))?;
            let q = current
                .get(&c.name)
                .ok_or_else(|| invalid("exposure sample channel is missing"))?;
            if p.unit != c.unit
                || q.unit != c.unit
                || !nonnegative(p.value)
                || !nonnegative(q.value)
            {
                return Err(invalid(
                    "exposure samples require matching units and finite nonnegative values",
                ));
            }
            let value = next[&c.name] + (p.value * 0.5 + q.value * 0.5) * duration_s;
            if !nonnegative(value) {
                return Err(invalid(
                    "integrated exposure exceeds the finite arithmetic domain",
                ));
            }
            next.insert(c.name.clone(), value);
        }
        let duration = self.duration_s + duration_s;
        if !nonnegative(duration) {
            return Err(invalid(
                "exposure duration exceeds the finite arithmetic domain",
            ));
        }
        self.integrals = next;
        self.duration_s = duration;
        Ok(())
    }

    pub fn integrated(&self, name: &str, unit: &str) -> Result<S, CaeError> {
        if !self
            .channels
            .iter()
            .any(|c| c.name == name && c.unit == unit)
        {
            return Err(invalid("exposure channel name or unit is invalid"));
        }
        Ok(self.integrals[name])
    }

    pub fn mean(&self, name: &str, unit: &str) -> Result<S, CaeError> {
        if !self.duration_s.value().is_finite() || self.duration_s.value() <= 0.0 {
            return Err(invalid("averaged exposure requires positive duration"));
        }
        let mean = self.integrated(name, unit)? / self.duration_s;
        if !nonnegative(mean) {
            return Err(invalid(
                "averaged exposure exceeds the finite nonnegative arithmetic domain",
            ));
        }
        Ok(mean)
    }
}

impl RateCalibration {
    pub fn rate<S: Scalar>(&self, exposure: &ExposureAccumulator<S>) -> Result<S, CaeError> {
        if self.provenance.trim().is_empty()
            || !self.reference.is_finite()
            || self.reference <= 0.0
            || !self.baseline_per_s.is_finite()
            || self.baseline_per_s < 0.0
            || !self.amplitude_per_s.is_finite()
            || self.amplitude_per_s < 0.0
            || !(self.baseline_per_s + self.amplitude_per_s).is_finite()
        {
            return Err(invalid(
                "exposure rates require positive reference, nonnegative finite coefficients and calibration provenance",
            ));
        }
        let x = exposure.mean(&self.channel, &self.unit)?;
        let fraction = if x.value() <= self.reference {
            let ratio = x / self.reference;
            ratio / (S::one() + ratio)
        } else {
            S::one() / (S::one() + S::from_f64(self.reference) / x)
        };
        let rate = S::from_f64(self.baseline_per_s) + fraction * self.amplitude_per_s;
        if !nonnegative(rate) {
            return Err(invalid(
                "conditioned evolution rate must be finite and nonnegative",
            ));
        }
        Ok(rate)
    }
}

impl<S: Scalar> ExposureRates<S> {
    pub fn advance(
        &self,
        previous: EvolutionState<S>,
        duration_s: S,
    ) -> Result<EvolutionState<S>, CaeError> {
        previous.validate()?;
        let total = self.damage_accumulation_per_s + self.damage_repair_per_s;
        if !nonnegative(duration_s)
            || [
                self.damage_accumulation_per_s,
                self.damage_repair_per_s,
                self.remodelling_per_s,
                self.hydration_per_s,
                total,
            ]
            .iter()
            .any(|x| !nonnegative(*x))
            || [self.remodelling_target, self.hydration_target]
                .iter()
                .any(|x| !x.is_finite() || !(0.0..=1.0).contains(x))
            || [total, self.remodelling_per_s, self.hydration_per_s]
                .iter()
                .any(|r| !(r.value() * duration_s.value()).is_finite())
        {
            return Err(invalid(
                "exposure evolution requires finite nonnegative duration and rates, with targets in [0, 1]",
            ));
        }
        let phi = |rate: S| {
            let x = rate * duration_s;
            if x.value().abs() < 1e-4 {
                (S::one()
                    + x * (S::from_f64(-0.5)
                        + x * (S::from_f64(1.0 / 6.0)
                            + x * (S::from_f64(-1.0 / 24.0)
                                + x * (S::from_f64(1.0 / 120.0) + x * (-1.0 / 720.0))))))
            } else {
                -(-x).exp_m1() / x
            }
        };
        let relax = |initial: S, target: f64, rate: S| {
            initial + (initial - target) * (-rate * duration_s).exp_m1()
        };
        let state = EvolutionState {
            damage: previous.damage
                + (self.damage_accumulation_per_s * duration_s
                    - (total * duration_s) * previous.damage)
                    * phi(total),
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
