// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::CaeError;
use implexity_physics_base::PhysicsError;
use serde_json::{Value, json};

use crate::util::{argmin, format_e, pow10, tie_min};

const BISECTION_STEPS: usize = 60;
const NEWTON_POLISH_STEPS: usize = 3;
const LOG10_2NF_LO: f64 = 0.0;
const LOG10_2NF_HI: f64 = 12.0;

pub const RESULT_NAMES: [&str; 11] = [
    "N_f",
    "two_Nf_reversals",
    "N_f_min",
    "min_station_index",
    "elastic_strain_amp",
    "plastic_strain_amp",
    "miner_damage_sum",
    "miner_damage_per_station",
    "mean_stress_Pa",
    "stress_concentration_factor",
    "effective_strain_amplitude",
];

#[derive(Debug, Clone, Copy)]
struct Station<S> {
    eps_a: S,
    e: S,
    sigma_f: S,
    b: S,
    eps_f: S,
    c: S,
    sigma_m: S,
}

impl<S: Scalar> Station<S> {
    fn residual(&self, x: S) -> S {
        (self.sigma_f - self.sigma_m) / self.e * pow10(self.b * x) + self.eps_f * pow10(self.c * x)
            - self.eps_a
    }

    fn derivative(&self, x: S) -> S {
        let ln10 = 10f64.ln();
        (self.sigma_f - self.sigma_m) / self.e * pow10(self.b * x) * (self.b * ln10)
            + self.eps_f * pow10(self.c * x) * (self.c * ln10)
    }

    fn values(&self) -> Station<f64> {
        Station {
            eps_a: self.eps_a.value(),
            e: self.e.value(),
            sigma_f: self.sigma_f.value(),
            b: self.b.value(),
            eps_f: self.eps_f.value(),
            c: self.c.value(),
            sigma_m: self.sigma_m.value(),
        }
    }

    fn solve(&self) -> S {
        let primal = self.values();
        let sign = |v: f64| {
            if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                v
            }
        };
        let r_lo_sign = sign(primal.residual(LOG10_2NF_LO));
        let (mut lo, mut hi) = (LOG10_2NF_LO, LOG10_2NF_HI);
        for _ in 0..BISECTION_STEPS {
            let mid = 0.5 * (lo + hi);
            if sign(primal.residual(mid)) * r_lo_sign > 0.0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let mut root = S::from_f64(0.5 * (lo + hi));
        for _ in 0..NEWTON_POLISH_STEPS {
            let r = self.residual(root);
            let d = self.derivative(root);
            let safe = if d.value().abs() < 1e-30 { S::from_f64(sign(d.value()) * 1e-30 + 1e-30) } else { d };
            root -= r / safe;
        }
        root
    }
}

#[derive(Debug, Clone)]
pub struct FatigueLifeResults<S> {
    pub n_f: Vec<S>,
    pub two_nf_reversals: Vec<S>,
    pub n_f_min: S,
    pub min_station_index: i32,
    pub elastic_strain_amp: Vec<S>,
    pub plastic_strain_amp: Vec<S>,
    pub miner_damage_sum: S,
    pub miner_damage_per_station: Vec<S>,
    pub mean_stress_pa: Vec<S>,
    pub stress_concentration_factor: Vec<S>,
    pub effective_strain_amplitude: Vec<S>,
}

impl<S: Scalar> FatigueLifeResults<S> {
    #[must_use]
    pub fn to_json(&self) -> Value {
        let v = |x: &[S]| x.iter().map(Scalar::value).collect::<Vec<f64>>();
        json!({"N_f": v(&self.n_f), "two_Nf_reversals": v(&self.two_nf_reversals),
            "N_f_min": self.n_f_min.value(), "min_station_index": self.min_station_index,
            "elastic_strain_amp": v(&self.elastic_strain_amp), "plastic_strain_amp": v(&self.plastic_strain_amp),
            "miner_damage_sum": self.miner_damage_sum.value(),
            "miner_damage_per_station": v(&self.miner_damage_per_station),
            "mean_stress_Pa": v(&self.mean_stress_pa),
            "stress_concentration_factor": v(&self.stress_concentration_factor),
            "effective_strain_amplitude": v(&self.effective_strain_amplitude)})
    }
}

#[derive(Debug, Clone)]
pub struct FatigueInputs<'a, S> {
    pub strain_amplitude: &'a [S],
    pub e_material: &'a [S],
    pub sigma_f_prime: &'a [S],
    pub b: &'a [S],
    pub eps_f_prime: &'a [S],
    pub c: &'a [S],
    pub n_applied: Option<&'a [S]>,
    pub mean_stress: Option<&'a [S]>,
    pub stress_concentration_factor: Option<&'a [S]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FatigueLifeCoffinManson {
    pub switch_certificate: f64,
}

impl Default for FatigueLifeCoffinManson {
    fn default() -> Self {
        Self { switch_certificate: 1.0e-8 }
    }
}

impl FatigueLifeCoffinManson {

    pub fn new(switch_certificate: f64) -> Result<Self, PhysicsError> {
        if !switch_certificate.is_finite() || switch_certificate <= 0.0 {
            return Err(PhysicsError::validation(
                "switch_certificate must be a positive finite float",
                "fatigue_life.switch_certificate",
            ));
        }
        Ok(Self { switch_certificate })
    }


    pub fn evaluate<S: Scalar>(
        &self,
        inputs: &FatigueInputs<'_, S>,
    ) -> Result<FatigueLifeResults<S>, PhysicsError> {
        let eps_a = inputs.strain_amplitude;
        let n = eps_a.len();
        if n < 1 {
            return Err(PhysicsError::validation(
                "strain_amplitude must have at least one station",
                "fatigue_life.strain_amplitude",
            ));
        }
        let broadcast = |name: &str, value: &[S]| -> Result<Vec<S>, PhysicsError> {
            match value.len() {
                1 => Ok(vec![value[0]; n]),
                k if k == n => Ok(value.to_vec()),
                k => Err(PhysicsError::validation(
                    format!("{name} must be scalar or shape ({n},); got ({k},)"),
                    format!("fatigue_life.{name}"),
                )),
            }
        };
        let e = broadcast("E_material", inputs.e_material)?;
        let sigma_f = broadcast("sigma_f_prime", inputs.sigma_f_prime)?;
        let b = broadcast("b", inputs.b)?;
        let eps_f = broadcast("eps_f_prime", inputs.eps_f_prime)?;
        let c = broadcast("c", inputs.c)?;
        let n_applied = match inputs.n_applied {
            None => vec![S::zero(); n],
            Some(v) => broadcast("N_applied", v)?,
        };
        let sigma_m = match inputs.mean_stress {
            None => vec![S::zero(); n],
            Some(v) => broadcast("mean_stress", v)?,
        };
        Self::new(self.switch_certificate)?;
        for i in 0..n {
            if !eps_a[i].value().is_finite() || eps_a[i].value() <= 0.0
                || [e[i], sigma_f[i], eps_f[i]].iter().any(|v| !v.value().is_finite() || v.value() <= 0.0)
                || [b[i], c[i]].iter().any(|v| !v.value().is_finite() || v.value() >= 0.0)
                || !sigma_m[i].value().is_finite() || !n_applied[i].value().is_finite() || n_applied[i].value() < 0.0 {
                return Err(PhysicsError::validation("finite positive strain and moduli, negative exponents and nonnegative cycle counts required", "fatigue_life.station"));
            }
        }

        let sigma_m: Vec<S> =
            sigma_m.iter().zip(&sigma_f).map(|(m, s)| m.maximum(*s * -0.5).minimum(*s * 0.9)).collect();
        let k_f = match inputs.stress_concentration_factor {
            None => vec![S::one(); n],
            Some(v) => broadcast("stress_concentration_factor", v)?,
        };
        if k_f.iter().any(|v| !v.value().is_finite() || v.value() <= 0.0) {
            return Err(PhysicsError::validation("stress concentration factor must be positive and finite", "fatigue_life.stress_concentration_factor"));
        }
        let k_f: Vec<S> = k_f.iter().map(|k| k.max_f64(1.0e-6)).collect();
        let eps_kf: Vec<S> = k_f.iter().zip(eps_a).map(|(k, e)| *k * *e).collect();
        let mut out = FatigueLifeResults {
            n_f: Vec::with_capacity(n),
            two_nf_reversals: Vec::with_capacity(n),
            n_f_min: S::zero(),
            min_station_index: 0,
            elastic_strain_amp: Vec::with_capacity(n),
            plastic_strain_amp: Vec::with_capacity(n),
            miner_damage_sum: S::zero(),
            miner_damage_per_station: Vec::with_capacity(n),
            mean_stress_pa: sigma_m.clone(),
            stress_concentration_factor: k_f,
            effective_strain_amplitude: eps_kf.clone(),
        };
        for i in 0..n {
            let station = Station {
                eps_a: eps_kf[i].max_f64(1.0e-30),
                e: e[i],
                sigma_f: sigma_f[i],
                b: b[i],
                eps_f: eps_f[i],
                c: c[i],
                sigma_m: sigma_m[i],
            };
            let primal = station.values();
            let lo = primal.residual(LOG10_2NF_LO);
            let hi = primal.residual(LOG10_2NF_HI);
            if !lo.is_finite() || !hi.is_finite() || lo < 0.0 || hi > 0.0 {
                return Err(CaeError::contract("fatigue life root is outside the supported log10(2N) bracket [0,12]").into());
            }
            let x = station.solve();
            let residual = primal.residual(x.value()).abs() / primal.eps_a;
            let slope = primal.derivative(x.value());
            if !x.value().is_finite() || x.value() < LOG10_2NF_LO || x.value() > LOG10_2NF_HI
                || !residual.is_finite() || residual > self.switch_certificate || !slope.is_finite() || slope.abs() < 1.0e-30 {
                return Err(CaeError::contract("fatigue life solve failed its bracket or relative strain residual check").into());
            }
            let two = pow10(x);
            let nf = two * 0.5;
            if !nf.value().is_finite() || nf.value() <= 0.0 {
                return Err(CaeError::contract("fatigue life must be positive and finite").into());
            }
            out.elastic_strain_amp.push((sigma_f[i] - sigma_m[i]) / e[i] * pow10(b[i] * x));
            out.plastic_strain_amp.push(eps_f[i] * pow10(c[i] * x));
            let damage = n_applied[i] / nf.max_f64(1.0e-30);
            out.miner_damage_sum += damage;
            out.miner_damage_per_station.push(damage);
            out.two_nf_reversals.push(two);
            out.n_f.push(nf);
        }
        if !out.miner_damage_sum.value().is_finite()
            || out.elastic_strain_amp.iter().chain(&out.plastic_strain_amp).chain(&out.miner_damage_per_station).any(|v| !v.value().is_finite()) {
            return Err(CaeError::contract("fatigue output exceeds the finite numerical range").into());
        }
        let values: Vec<f64> = out.n_f.iter().map(Scalar::value).collect();
        out.min_station_index = i32::try_from(argmin(&values)).unwrap_or(0);
        out.n_f_min = tie_min(&out.n_f);
        Ok(out)
    }

    pub fn certify_sensitivity(&self, strain_amplitude: Option<&[f64]>) -> Result<Value, PhysicsError> {
        let path = "fatigue_life.certify_sensitivity.strain_amplitude";
        let Some(eps) = strain_amplitude else {
            return Err(PhysicsError::validation("certify_sensitivity requires strain_amplitude", path));
        };
        Self::new(self.switch_certificate)?;
        if eps.is_empty() || !eps.iter().all(Scalar::is_finite) {
            return Err(PhysicsError::validation("strain_amplitude must be finite", path));
        }
        let eps_min = eps.iter().copied().fold(f64::INFINITY, f64::min);
        let sc = self.switch_certificate;
        if eps_min <= sc {
            return Err(CaeError::contract(format!("fatigue strain {} is at or below singularity tolerance {}", format_e(eps_min,3), format_e(sc,3))).into());
        }
        Ok(json!({"strain_amplitude_min": eps_min, "switch_certificate": sc, "sensitivity_admissible": true, "qualification_scope": "strain_singularity_only"}))
    }
    pub fn certify_model_sensitivity(&self, inputs: &FatigueInputs<'_, f64>) -> Result<Value, PhysicsError> {
        let result = self.evaluate(inputs)?;
        self.certify_sensitivity(Some(&result.effective_strain_amplitude))?;
        let minimum = result.n_f_min;
        if result.n_f.iter().filter(|n| **n / minimum - 1.0 <= self.switch_certificate).count() != 1 {
            return Err(CaeError::contract("fatigue minimum life has no unique local station branch").into());
        }
        for i in 0..inputs.strain_amplitude.len() {
            let get = |v: &[f64]| if v.len() == 1 { v[0] } else { v[i] };
            let sf = get(inputs.sigma_f_prime);
            let mean = inputs.mean_stress.map_or(0.0, get);
            let factor = inputs.stress_concentration_factor.map_or(1.0, get);
            if ((mean + 0.5 * sf) / sf).abs() <= self.switch_certificate
                || ((mean - 0.9 * sf) / sf).abs() <= self.switch_certificate
                || (factor / 1.0e-6 - 1.0).abs() <= self.switch_certificate {
                return Err(CaeError::contract("fatigue sensitivity is at a clipping boundary").into());
            }
        }
        Ok(json!({"sensitivity_admissible": true, "qualification_scope": "admitted_model_local_branch", "derivative_orders": [1,2], "relative_strain_residual_tolerance": self.switch_certificate}))
    }

}

