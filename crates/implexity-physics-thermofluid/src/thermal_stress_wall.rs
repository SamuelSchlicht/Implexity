// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_physics_base::model_errors::{PhysicsError, PhysicsModelErrorKind, PhysicsResult};

const ONE_MINUS_NU_FLOOR: f64 = 1.0e-6;

pub const RESULT_NAMES: [&str; 5] =
    ["sigma_theta_theta_Pa", "sigma_zz_Pa", "sigma_von_mises_Pa", "strain_amplitude", "delta_T_K"];

#[derive(Debug, Clone, PartialEq)]
pub enum StationValue<S> {
    Scalar(S),
    Stations(Vec<S>),
}

impl<S: Scalar> StationValue<S> {
    fn broadcast(&self, name: &str, n: usize) -> PhysicsResult<Vec<S>> {
        match self {
            Self::Scalar(v) => Ok(vec![*v; n]),
            Self::Stations(v) if v.len() == n => Ok(v.clone()),
            Self::Stations(v) => Err(PhysicsError::validation(
                format!("{name} must be scalar or shape ({n},); got ({},)", v.len()),
                format!("thermal_stress_wall.{name}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalStressResult<S> {
    pub sigma_theta_theta_pa: Vec<S>,
    pub sigma_zz_pa: Vec<S>,
    pub sigma_von_mises_pa: Vec<S>,
    pub strain_amplitude: Vec<S>,
    pub delta_t_k: Vec<S>,
}

impl<S: Scalar> ThermalStressResult<S> {
    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        let v = |x: &Vec<S>| json!(x.iter().map(Scalar::value).collect::<Vec<_>>());
        let mut m = Map::new();
        m.insert("sigma_theta_theta_Pa".into(), v(&self.sigma_theta_theta_pa));
        m.insert("sigma_zz_Pa".into(), v(&self.sigma_zz_pa));
        m.insert("sigma_von_mises_Pa".into(), v(&self.sigma_von_mises_pa));
        m.insert("strain_amplitude".into(), v(&self.strain_amplitude));
        m.insert("delta_T_K".into(), v(&self.delta_t_k));
        m
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalStressWall {
    pub switch_certificate: f64,
}

impl Default for ThermalStressWall {
    fn default() -> Self {
        Self { switch_certificate: 1.0e-8 }
    }
}

impl ThermalStressWall {

    pub fn new(switch_certificate: f64) -> PhysicsResult<Self> {
        if !switch_certificate.is_finite() || switch_certificate <= 0.0 {
            let mut details = Map::new();
            details.insert("value".into(), json!(switch_certificate));
            return Err(PhysicsError::Model {
                kind: PhysicsModelErrorKind::Validation,
                message: "switch_certificate must be a positive finite float".into(),
                path: "thermal_stress_wall.switch_certificate".into(),
                details,
            });
        }
        Ok(Self { switch_certificate })
    }


    pub fn evaluate<S: Scalar>(
        &self,
        t_wall_hot: &[S],
        t_wall_ref: &StationValue<S>,
        e_material: &StationValue<S>,
        alpha_material: &StationValue<S>,
        nu_material: &StationValue<S>,
    ) -> PhysicsResult<ThermalStressResult<S>> {
        let n = t_wall_hot.len();
        if n < 1 {
            let mut details = Map::new();
            details.insert("n_stations".into(), json!(n));
            return Err(PhysicsError::Model {
                kind: PhysicsModelErrorKind::Validation,
                message: "T_wall_hot must have at least one station".into(),
                path: "thermal_stress_wall.T_wall_hot".into(),
                details,
            });
        }
        let t_ref = t_wall_ref.broadcast("T_wall_ref", n)?;
        let e = e_material.broadcast("E_material", n)?;
        let alpha = alpha_material.broadcast("alpha_material", n)?;
        let nu = nu_material.broadcast("nu_material", n)?;
        if let StationValue::Scalar(v) = nu_material {
            let x = v.value();
            if x <= -1.0 || x >= 1.0 {
                let mut details = Map::new();
                details.insert("value".into(), json!(x));
                return Err(PhysicsError::Model {
                    kind: PhysicsModelErrorKind::Validation,
                    message: "nu_material must be in (-1, 1) for physical materials".into(),
                    path: "thermal_stress_wall.nu_material".into(),
                    details,
                });
            }
        }
        let mut r = ThermalStressResult {
            sigma_theta_theta_pa: Vec::with_capacity(n),
            sigma_zz_pa: Vec::with_capacity(n),
            sigma_von_mises_pa: Vec::with_capacity(n),
            strain_amplitude: Vec::with_capacity(n),
            delta_t_k: Vec::with_capacity(n),
        };
        for k in 0..n {
            let delta = t_wall_hot[k] - t_ref[k];
            let one_minus_nu = (S::one() - nu[k]).max_f64(ONE_MINUS_NU_FLOOR);
            let sigma = -e[k] * alpha[k] * delta / one_minus_nu;
            r.sigma_theta_theta_pa.push(sigma);
            r.sigma_zz_pa.push(sigma);
            r.sigma_von_mises_pa.push(sigma.abs());
            r.strain_amplitude.push(alpha[k] * delta.abs());
            r.delta_t_k.push(delta);
        }
        Ok(r)
    }


    pub fn certify_sensitivity(&self, nu_material: &[f64]) -> PhysicsResult<Map<String, Value>> {
        if !nu_material.iter().copied().all(f64::is_finite) {
            let mut details = Map::new();
            details.insert("value".into(), json!(nu_material));
            return Err(PhysicsError::Model {
                kind: PhysicsModelErrorKind::Validation,
                message: "nu_material must be finite".into(),
                path: "thermal_stress_wall.certify_sensitivity.nu_material".into(),
                details,
            });
        }
        let nu_max = nu_material.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let sc = self.switch_certificate;
        if nu_max >= 1.0 - sc {
            return Err(PhysicsError::contract(format!(
                "ThermalStressWall incompressible limit: max(nu) = {nu_max:.6} exceeds 1 - switch_certificate = {:.6}; the constrained-biaxial stress sensitivity is dominated by the 1 / (1 - nu) tail rather than by the mechanical physics.  Reduce nu before requesting a derivative.  (switch-distance certificate)",
                1.0 - sc
            )));
        }
        let mut m = Map::new();
        m.insert("nu_max".into(), json!(nu_max));
        m.insert("one_minus_nu_min".into(), json!(1.0 - nu_max));
        m.insert("switch_certificate".into(), json!(sc));
        m.insert("sensitivity_admissible".into(), json!(true));
        Ok(m)
    }
}
