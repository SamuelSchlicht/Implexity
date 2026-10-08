// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_physics_base::model_errors::{PhysicsError, PhysicsResult};

use crate::peng_robinson::{
    GAS_CONSTANT_J_MOL_K as RU, SQRT_TWO as SQRT2, cubic_coefficients, largest_real_root,
    largest_real_root_value, root_metrics,
};

pub const SWITCH_CERTIFICATE: f64 = 1e-8;

pub const PROPERTY_KEYS: [&str; 7] = ["density", "enthalpy", "entropy", "cp", "cv", "gamma", "sound_speed"];

#[derive(Debug, Clone, PartialEq)]
pub struct SpeciesSpec {
    pub name: String,
    pub molar_mass_kg_per_mol: f64,
    pub heat_capacity_j_per_kg_k: f64,
    pub critical_temperature_k: f64,
    pub critical_pressure_pa: f64,
    pub acentric_factor: f64,
}

impl SpeciesSpec {
    #[must_use]
    pub fn new(name: &str, molar_mass_kg_per_mol: f64, heat_capacity_j_per_kg_k: f64) -> Self {
        Self {
            name: name.into(),
            molar_mass_kg_per_mol,
            heat_capacity_j_per_kg_k,
            critical_temperature_k: 1.0,
            critical_pressure_pa: 1.0,
            acentric_factor: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EosFamily {
    IdealGas,
    PengRobinson,
}

impl EosFamily {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IdealGas => "ideal_gas",
            Self::PengRobinson => "peng_robinson",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EquationOfState {
    pub species: Vec<SpeciesSpec>,
    pub family: EosFamily,
    pub switch_certificate: f64,
    pub reference_temperature_k: f64,
    pub reference_pressure_pa: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EosProperties<S> {
    pub density: S,
    pub enthalpy: S,
    pub entropy: S,
    pub cp: S,
    pub cv: S,
    pub gamma: S,
    pub sound_speed: S,
}

impl<S: Scalar> EosProperties<S> {
    #[must_use]
    pub fn to_array(&self) -> [S; 7] {
        [self.density, self.enthalpy, self.entropy, self.cp, self.cv, self.gamma, self.sound_speed]
    }

    #[must_use]
    pub fn values_map(&self) -> Map<String, Value> {
        PROPERTY_KEYS.iter().zip(self.to_array()).map(|(k, v)| ((*k).to_string(), json!(v.value()))).collect()
    }
}

fn xlogy_self<S: Scalar>(x: S) -> S {
    if x.value() == 0.0 { S::zero() } else { x * x.ln() }
}

fn sum<S: Scalar>(xs: impl IntoIterator<Item = S>) -> S {
    let mut acc = S::zero();
    let mut first = true;
    for x in xs {
        if first {
            acc = x;
            first = false;
        } else {
            acc += x;
        }
    }
    acc
}

impl EquationOfState {


    pub fn new(
        species: Vec<SpeciesSpec>,
        families: &[&str],
        switch_certificate: f64,
        reference_temperature_k: f64,
        reference_pressure_pa: f64,
    ) -> PhysicsResult<Self> {
        let mut names: Vec<&str> = species.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        if species.is_empty() || names.len() != species.len() {
            return Err(PhysicsError::contract("EOS requires nonempty, uniquely named species"));
        }
        let family = match families {
            ["ideal_gas"] => EosFamily::IdealGas,
            ["peng_robinson"] => EosFamily::PengRobinson,
            _ => {
                return Err(PhysicsError::contract(
                    "Select exactly one EOS family explicitly; property-family blending is unsupported",
                ));
            }
        };
        for v in [switch_certificate, reference_temperature_k, reference_pressure_pa] {
            if !v.is_finite() || v <= 0.0 {
                return Err(PhysicsError::contract(
                    "EOS tolerances and reference states must be finite and positive",
                ));
            }
        }
        for s in &species {
            let values = [
                s.molar_mass_kg_per_mol,
                s.heat_capacity_j_per_kg_k,
                s.critical_temperature_k,
                s.critical_pressure_pa,
            ];
            if s.name.is_empty()
                || values.iter().any(|x| !x.is_finite() || *x <= 0.0)
                || !s.acentric_factor.is_finite()
                || s.heat_capacity_j_per_kg_k <= RU / s.molar_mass_kg_per_mol
            {
                return Err(PhysicsError::contract(
                    "EOS species require finite properties, positive critical data and positive ideal cv",
                ));
            }
        }
        Ok(Self { species, family, switch_certificate, reference_temperature_k, reference_pressure_pa })
    }


    pub fn with_family(species: Vec<SpeciesSpec>, family: &str) -> PhysicsResult<Self> {
        Self::new(species, &[family], SWITCH_CERTIFICATE, 298.15, 101_325.0)
    }

    fn inputs<S: Scalar>(
        &self,
        y: &[S],
        p: S,
        t: S,
        family_weights: Option<&[f64]>,
        family_logits: Option<&[f64]>,
    ) -> PhysicsResult<()> {
        if family_weights.is_some() && family_logits.is_some() {
            return Err(PhysicsError::contract("Specify family_logits only, not both selector spellings"));
        }
        let logits = family_logits.or(family_weights);
        if y.len() != self.species.len() {
            return Err(PhysicsError::contract(
                "EOS expects a species vector and scalar pressure/temperature; use vmap for batches",
            ));
        }
        let total = y.iter().fold(0.0, |acc, v| acc + v.value());
        let mut valid = y.iter().all(|v| v.value().is_finite() && v.value() >= 0.0)
            && (total - 1.0).abs() <= 1e-10
            && p.value().is_finite()
            && p.value() > 0.0
            && t.value().is_finite()
            && t.value() > 0.0;
        if let Some(l) = logits {
            if l.len() != 1 {
                return Err(PhysicsError::contract(
                    "A selected EOS has one family_logit; model-family optimization is unsupported",
                ));
            }
            valid = valid && l.iter().copied().all(f64::is_finite);
        }
        if !valid {
            return Err(PhysicsError::contract("Invalid EOS pressure, temperature, composition or selector"));
        }
        Ok(())
    }

    fn ideal_reference<S: Scalar>(&self, y: &[S], p: S, t: S) -> (S, Vec<S>, S, S, S) {
        let moles: Vec<S> =
            y.iter().zip(&self.species).map(|(yi, s)| *yi / s.molar_mass_kg_per_mol).collect();
        let mixture_mass = S::one() / sum(moles.iter().copied());
        let mole_fractions: Vec<S> = moles.iter().map(|m| *m * mixture_mass).collect();
        let gas_constant = S::from_f64(RU) / mixture_mass;
        let cp = sum(y.iter().zip(&self.species).map(|(yi, s)| *yi * s.heat_capacity_j_per_kg_k));
        let enthalpy = cp * (t - self.reference_temperature_k);
        let entropy = cp * (t / self.reference_temperature_k).ln()
            - gas_constant * (p / self.reference_pressure_pa).ln()
            - gas_constant * sum(mole_fractions.iter().map(|x| xlogy_self(*x)));
        (mixture_mass, mole_fractions, cp, enthalpy, entropy)
    }

    fn pr_parameters<S: Scalar>(&self, y: &[S], t: S) -> (S, S, S, S, S, bool) {
        let mixture_mass =
            S::one() / sum(y.iter().zip(&self.species).map(|(yi, s)| *yi / s.molar_mass_kg_per_mol));
        let mole_fractions: Vec<S> =
            y.iter().zip(&self.species).map(|(yi, s)| *yi / s.molar_mass_kg_per_mol * mixture_mass).collect();
        let mut alpha_ok = true;
        let mut root_a_terms = Vec::with_capacity(y.len());
        let mut root_a_t_terms = Vec::with_capacity(y.len());
        let mut b_terms = Vec::with_capacity(y.len());
        for (x, s) in mole_fractions.iter().zip(&self.species) {
            let om = s.acentric_factor;
            let tc = s.critical_temperature_k;
            let pc = s.critical_pressure_pa;
            let kappa = 0.37464 + 1.54226 * om - 0.26992 * om * om;
            let alpha_root = (S::one() - (t / tc).sqrt()) * kappa + 1.0;
            alpha_ok &= alpha_root.value() > 0.0;
            let root_a0 = (0.45724 * RU * RU * tc * tc / pc).sqrt();
            root_a_terms.push(*x * root_a0 * alpha_root);
            root_a_t_terms.push(*x * root_a0 * (S::from_f64(-kappa) / ((t * tc).sqrt() * 2.0)));
            b_terms.push(*x * (0.07780 * RU * tc / pc));
        }
        let root_a = sum(root_a_terms);
        let root_a_t = sum(root_a_t_terms);
        let attraction = root_a * root_a;
        let attraction_t = root_a * root_a_t * 2.0;
        let attraction_tt = root_a_t * root_a_t * 2.0 - root_a * root_a_t / t;
        let covolume = sum(b_terms);
        (mixture_mass, attraction, attraction_t, attraction_tt, covolume, alpha_ok)
    }

    fn properties<S: Scalar>(&self, y: &[S], p: S, t: S) -> (EosProperties<S>, bool) {
        let (mixture_mass, _, cp_ideal, h_ideal, s_ideal) = self.ideal_reference(y, p, t);
        let gas_constant = S::from_f64(RU) / mixture_mass;
        if self.family == EosFamily::IdealGas {
            let cv = cp_ideal - gas_constant;
            let gamma = cp_ideal / cv;
            let props = EosProperties {
                density: p / (gas_constant * t),
                enthalpy: h_ideal,
                entropy: s_ideal,
                cp: cp_ideal,
                cv,
                gamma,
                sound_speed: (gamma * gas_constant * t).sqrt(),
            };
            return (props, true);
        }
        let (_, a, a_t, a_tt, b, alpha_valid) = self.pr_parameters(y, t);
        let attraction = a * p / (t * (RU * RU) * t);
        let covolume = b * p / (t * RU);
        let z = largest_real_root(attraction, covolume);
        let volume = z * RU * t / p;
        let denominator = volume * volume + b * volume * 2.0 - b * b;
        let vb = volume - b;
        let pressure_v = -(t * RU) / (vb * vb) + a * (volume * 2.0 + b * 2.0) / (denominator * denominator);
        let pressure_t = S::from_f64(RU) / vb - a_t / denominator;
        let logarithm = (b * (2.0 * SQRT2) / (volume + b * (1.0 - SQRT2))).ln_1p();
        let factor = logarithm / (b * (2.0 * SQRT2));
        let enthalpy = h_ideal + ((t * RU) * (z - 1.0) + (t * a_t - a) * factor) / mixture_mass;
        let entropy = s_ideal + ((z - covolume).ln() * RU + a_t * factor) / mixture_mass;
        let cv = cp_ideal - gas_constant + t * a_tt * factor / mixture_mass;
        let cp = cv - t * (pressure_t * pressure_t) / (pressure_v * mixture_mass);
        let gamma = cp / cv;
        let sound_squared = -gamma * (volume * volume) * pressure_v / mixture_mass;
        let (residual, conditioning, _) = root_metrics(z.value(), attraction.value(), covolume.value());
        let valid = alpha_valid
            && volume.value() > b.value()
            && pressure_v.value() < 0.0
            && residual <= 1e-10
            && conditioning > self.switch_certificate
            && cp.value() > 0.0
            && cv.value() > 0.0
            && sound_squared.value() > 0.0;
        let props = EosProperties {
            density: mixture_mass / volume,
            enthalpy,
            entropy,
            cp,
            cv,
            gamma,
            sound_speed: sound_squared.sqrt(),
        };
        (props, valid)
    }


    pub fn evaluate<S: Scalar>(&self, y: &[S], p: S, t: S) -> PhysicsResult<EosProperties<S>> {
        self.evaluate_selected(y, p, t, None, None)
    }


    pub fn evaluate_selected<S: Scalar>(
        &self,
        y: &[S],
        p: S,
        t: S,
        family_weights: Option<&[f64]>,
        family_logits: Option<&[f64]>,
    ) -> PhysicsResult<EosProperties<S>> {
        self.inputs(y, p, t, family_weights, family_logits)?;
        let (props, root_valid) = self.properties(y, p, t);
        if !root_valid || !props.to_array().iter().all(|v| v.value().is_finite()) {
            return Err(PhysicsError::contract(
                "EOS evaluation has invalid properties or an unresolved/singular root",
            ));
        }
        Ok(props)
    }


    pub fn certify_sensitivity(&self, y: &[f64], p: f64, t: f64) -> PhysicsResult<Map<String, Value>> {
        self.inputs(y, p, t, None, None)?;
        if y.iter().any(|v| *v <= 0.0) {
            return Err(PhysicsError::contract("Composition-boundary entropy derivatives are not admitted"));
        }
        self.evaluate(y, p, t)?;
        let mut result = Map::new();
        result.insert("family".into(), json!(self.family.as_str()));
        result.insert("sensitivity_admissible".into(), json!(true));
        result.insert("scope".into(), json!("local_fixed_composition_single_root"));
        result.insert("phase_equilibrium_validated".into(), json!(false));
        result.insert("switch_certificate".into(), json!(self.switch_certificate));
        if self.family == EosFamily::IdealGas {
            return Ok(result);
        }
        if self.species.len() == 1 {
            let s = &self.species[0];
            let margin =
                (t / s.critical_temperature_k - 1.0).abs().max((p / s.critical_pressure_pa - 1.0).abs());
            if margin <= self.switch_certificate {
                return Err(PhysicsError::contract(
                    "Sensitivity at the declared pure critical reference is not admitted; rounded cubic coefficients do not establish regular physical behavior",
                ));
            }
            result.insert("pure_critical_reference_margin".into(), json!(margin));
        }
        let (_, a, _, _, b, _) = self.pr_parameters(y, t);
        let attraction = a * p / (RU * RU * t * t);
        let covolume = b * p / (RU * t);
        let z = largest_real_root_value(attraction, covolume);
        let admissible: Vec<f64> = cubic_roots(attraction, covolume)
            .into_iter()
            .filter(|(re, im)| im.abs() <= 1e-7 && *re > covolume)
            .map(|(re, _)| re)
            .collect();
        let (residual, conditioning, discriminant) = root_metrics(z, attraction, covolume);
        if admissible.len() != 1 {
            return Err(PhysicsError::contract(
                "Multiple/coincident PR roots require explicit phase selection and stability analysis",
            ));
        }
        result.insert("root_residual".into(), json!(residual));
        result.insert("normalized_root_jacobian".into(), json!(conditioning));
        result.insert("cubic_discriminant".into(), json!(discriminant));
        result.insert("compressibility_factor".into(), json!(z));
        Ok(result)
    }
}

#[must_use]
pub fn cubic_roots(attraction: f64, covolume: f64) -> Vec<(f64, f64)> {
    let (c2, c1, _) = cubic_coefficients(attraction, covolume);
    let z1 = largest_real_root_value(attraction, covolume);
    let pq = c2 + z1;
    let qq = c1 + pq * z1;
    let disc = pq * pq - 4.0 * qq;
    let mut roots = vec![(z1, 0.0)];
    if disc >= 0.0 {
        let s = disc.sqrt();

        let t = -0.5 * (pq + pq.signum() * s);
        if t == 0.0 {
            roots.push((0.0, 0.0));
            roots.push((0.0, 0.0));
        } else {
            roots.push((t, 0.0));
            roots.push((qq / t, 0.0));
        }
    } else {
        let im = 0.5 * (-disc).sqrt();
        roots.push((-0.5 * pq, im));
        roots.push((-0.5 * pq, -im));
    }
    roots
}
