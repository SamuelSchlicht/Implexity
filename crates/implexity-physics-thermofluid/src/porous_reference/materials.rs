// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use super::ad::interp;

#[derive(Clone, Debug, PartialEq)]
pub struct Phase {
    pub t: Vec<f64>,
    pub e: Vec<f64>,
    pub sy: Vec<f64>,
    pub k: Vec<f64>,
    pub nu: f64,
    pub alpha: f64,
    pub density: f64,
    pub cp: f64,
    pub t_melt: f64,
    pub t_limit: f64,
}

pub struct Damage {
    pub dpa: f64,
    pub k_a: f64,
    pub k_n: f64,
    pub e_a: f64,
    pub e_n: f64,
    pub dsy_coeff: f64,
    pub dsy_max: f64,
}

impl Damage {
    pub fn k_retention(&self) -> f64 {
        1.0 / (1.0 + self.k_a * self.dpa.powf(self.k_n))
    }
    pub fn e_retention(&self) -> f64 {
        1.0 / (1.0 + self.e_a * self.dpa.powf(self.e_n))
    }
    pub fn yield_increment(&self) -> f64 {
        (self.dsy_coeff * self.dpa.powf(0.5)).min(self.dsy_max)
    }
}

pub struct PhasePair<'a> {
    pub phase0: &'a Phase,
    pub phase1: &'a Phase,
    pub nu_shared: f64,
    pub k_void: f64,
    pub e_min_rel: f64,
}

impl PhasePair<'_> {
    pub fn surface_limit<S: Scalar>(&self, fraction: S) -> S {
        (-fraction + 1.0) * self.phase0.t_limit + fraction * self.phase1.t_limit
    }

    pub fn melt_limit<S: Scalar>(&self, fraction: S) -> S {
        (-fraction + 1.0) * self.phase0.t_melt + fraction * self.phase1.t_melt
    }

    pub fn solid_conductivity<S: Scalar>(&self, t: S, fraction: S, dose: &Damage) -> S {
        let kw = interp(t, &self.phase0.t, &self.phase0.k);
        let kc = interp(t, &self.phase1.t, &self.phase1.k);
        (kw * (-fraction + 1.0) + kc * fraction) * dose.k_retention()
    }

    pub fn solid_modulus<S: Scalar>(&self, t: S, fraction: S, dose: &Damage) -> S {
        let ew = interp(t, &self.phase0.t, &self.phase0.e);
        let ec = interp(t, &self.phase1.t, &self.phase1.e);
        (ew * (-fraction + 1.0) + ec * fraction) * dose.e_retention()
    }

    pub fn solid_yield<S: Scalar>(&self, t: S, fraction: S, dose: &Damage) -> S {
        let sw = interp(t, &self.phase0.t, &self.phase0.sy);
        let sc = interp(t, &self.phase1.t, &self.phase1.sy);
        sw * (-fraction + 1.0) + sc * fraction + dose.yield_increment()
    }

    pub fn solid_expansion<S: Scalar>(&self, fraction: S) -> S {
        (-fraction + 1.0) * self.phase0.alpha + fraction * self.phase1.alpha
    }

    pub fn solid_rho_cp<S: Scalar>(&self, fraction: S) -> S {
        (-fraction + 1.0) * (self.phase0.density * self.phase0.cp) + fraction * (self.phase1.density * self.phase1.cp)
    }

    pub fn effective_conductivity<S: Scalar>(&self, rho: S, t: S, fraction: S, dose: &Damage) -> S {
        let ks = self.solid_conductivity(t, fraction, dose).max_f64(self.k_void);
        let kv = self.k_void;
        let denom = S::from_f64(1.0) / (-ks + kv) + rho / (ks * 3.0);
        ks + (-rho + 1.0) / denom
    }

    pub fn effective_modulus<S: Scalar>(&self, rho_stiff: S, t: S, fraction: S, dose: &Damage) -> S {
        let es = self.solid_modulus(t, fraction, dose);
        es * (rho_stiff * (1.0 - self.e_min_rel) + self.e_min_rel)
    }

    pub fn effective_rho_cp<S: Scalar>(&self, rho: S, fraction: S) -> S {
        let floor = 1.0e-3;
        let rs = (rho + (rho * rho + floor * floor).sqrt()) * 0.5;
        self.solid_rho_cp(fraction) * rs
    }
}

impl PhasePair<'_> {
    pub fn thermal_strain<S: Scalar>(&self, temperature: S, fraction: S, reference: f64) -> S {
        self.solid_expansion(fraction) * (temperature - reference)
    }
    pub fn stress_modulus<S: Scalar>(&self, density: S, temperature: S, fraction: S, damage: &Damage, floor: f64, exponent: f64) -> S {
        self.solid_modulus(temperature, fraction, damage) * density.max_f64(floor).powf(exponent)
    }
}
