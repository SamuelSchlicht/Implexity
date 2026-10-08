// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::plug_nozzle_expansion::{area_ratio, check_positive, pressure_ratio};
use crate::pyval::fmt_g6;
use crate::roots::{Residual, bracketed_newton_root};

pub const DEFAULT_QUADRATURE_ORDER: usize = 64;

pub const NEWTON_POLISH_STEPS: usize = 3;

pub const G0_M_S2: f64 = 9.80665;

pub const RESULT_NAMES: [&str; 18] = [
    "mach",
    "pressure_Pa",
    "temperature_K",
    "density_kg_m3",
    "velocity_m_s",
    "area_m2",
    "throat_index",
    "throat_area_m2",
    "exit_mach",
    "exit_pressure_Pa",
    "exit_temperature_K",
    "exit_area_m2",
    "exit_velocity_m_s",
    "mass_flow_kg_s",
    "thrust_N",
    "thrust_wall_integrated_N",
    "specific_impulse_s",
    "area_ratio_exit",
];

struct AreaMach;

impl Residual for AreaMach {
    fn eval<T: Scalar>(&self, m: T, p: &[T]) -> T {
        area_ratio(m, p[0]) - p[1]
    }
}

pub fn subsonic_mach<S: Scalar>(area_ratio_value: S, gamma: S) -> S {
    bracketed_newton_root(&AreaMach, 1.0e-4, 1.0 - 1.0e-6, NEWTON_POLISH_STEPS, &[gamma, area_ratio_value])
}

pub fn supersonic_mach<S: Scalar>(area_ratio_value: S, gamma: S) -> S {
    bracketed_newton_root(&AreaMach, 1.0 + 1.0e-6, 50.0, NEWTON_POLISH_STEPS, &[gamma, area_ratio_value])
}

pub fn choked_mdot<S: Scalar>(p_c: S, t_c: S, gamma: S, r: S, throat_area: S) -> S {
    let coef =
        (gamma / (r * t_c)).sqrt() * ((gamma + 1.0).recip() * 2.0).pow((gamma + 1.0) / ((gamma - 1.0) * 2.0));
    throat_area * p_c * coef
}

#[must_use]
pub fn local_area_minima(a: &[f64]) -> Vec<usize> {
    let n = a.len();
    let mut idx = Vec::new();
    for i in 0..n {
        let left = if i > 0 { a[i - 1] } else { f64::INFINITY };
        let right = if i + 1 < n { a[i + 1] } else { f64::INFINITY };
        #[allow(clippy::float_cmp)]
        if (a[i] < left && a[i] < right) || (a[i] <= left && a[i] < right && i > 0 && a[i] == a[i - 1]) {
            idx.push(i);
        }
    }
    idx
}

#[derive(Debug, Clone, PartialEq)]
pub struct IsentropicResult<S> {
    pub mach: Vec<S>,
    pub pressure_pa: Vec<S>,
    pub temperature_k: Vec<S>,
    pub density_kg_m3: Vec<S>,
    pub velocity_m_s: Vec<S>,
    pub area_m2: Vec<S>,
    pub throat_index: usize,
    pub scalars: [S; 11],
}

impl IsentropicResult<f64> {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("mach".into(), json!(self.mach));
        m.insert("pressure_Pa".into(), json!(self.pressure_pa));
        m.insert("temperature_K".into(), json!(self.temperature_k));
        m.insert("density_kg_m3".into(), json!(self.density_kg_m3));
        m.insert("velocity_m_s".into(), json!(self.velocity_m_s));
        m.insert("area_m2".into(), json!(self.area_m2));
        m.insert("throat_index".into(), json!(self.throat_index));
        for (k, v) in RESULT_NAMES[7..].iter().zip(&self.scalars) {
            m.insert((*k).to_string(), json!(v));
        }
        Value::Object(m)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IsentropicExpansionAlongContour {
    pub n_quadrature: usize,
    pub switch_certificate: f64,
}

impl Default for IsentropicExpansionAlongContour {
    fn default() -> Self {
        Self { n_quadrature: DEFAULT_QUADRATURE_ORDER, switch_certificate: 1e-8 }
    }
}

impl IsentropicExpansionAlongContour {

    pub fn new(n_quadrature: usize, switch_certificate: f64) -> PResult<Self> {
        if n_quadrature < 2 {
            return Err(ModelError::validation(
                "n_quadrature must be an integer >= 2",
                "isentropic_expansion.n_quadrature",
            ));
        }
        if !switch_certificate.is_finite() || switch_certificate <= 0.0 {
            return Err(ModelError::validation(
                "switch_certificate must be a positive finite float",
                "isentropic_expansion.switch_certificate",
            ));
        }
        Ok(Self { n_quadrature, switch_certificate })
    }


    #[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
    pub fn evaluate<S: Scalar>(
        &self,
        r: &[S],
        z: &[S],
        p_c: S,
        t_c: S,
        gamma: S,
        rs: S,
        p_amb: S,
    ) -> PResult<IsentropicResult<S>> {
        let pre = "isentropic_expansion";
        check_positive(pre, "p_c", p_c.value(), 0.0)?;
        check_positive(pre, "T_c", t_c.value(), 0.0)?;
        check_positive(pre, "gamma", gamma.value(), 1.0)?;
        check_positive(pre, "R_specific", rs.value(), 0.0)?;
        let pa = p_amb.value();
        if !pa.is_finite() || pa < 0.0 {
            return Err(ModelError::validation(
                "p_amb must be finite and >= 0",
                "isentropic_expansion.p_amb",
            )
            .detail("value", json!(pa)));
        }
        let shapes = || {
            let mut d = Map::new();
            d.insert("r_shape".into(), json!([r.len()]));
            d.insert("z_shape".into(), json!([z.len()]));
            d
        };
        if r.len() != z.len() {
            return Err(ModelError::validation(
                "contour_r and contour_z must share their length",
                "isentropic_expansion.contour",
            )
            .with_details(shapes()));
        }
        if r.len() < 3 {
            return Err(ModelError::validation(
                "contour must have at least 3 samples (chamber, throat, exit)",
                "isentropic_expansion.contour",
            )
            .detail("n_samples", json!(r.len())));
        }
        let zv: Vec<f64> = z.iter().map(Scalar::value).collect();
        if !zv.iter().all(|v| v.is_finite()) {
            return Err(ModelError::validation("contour_z must be finite", "isentropic_expansion.contour_z"));
        }
        if r.iter().any(|v| !v.value().is_finite() || v.value() <= 0.0 || !(std::f64::consts::PI * v.value() * v.value()).is_finite() || std::f64::consts::PI * v.value() * v.value() <= 0.0) {
            return Err(ModelError::validation("contour_r must be finite and positive", "isentropic_expansion.contour_r"));
        }
        {
            let diffs: Vec<f64> = zv.windows(2).map(|w| w[1] - w[0]).collect();
            if !diffs.iter().all(|d| *d > 0.0) {
                let min = diffs.iter().copied().fold(f64::INFINITY, f64::min);
                return Err(ModelError::validation(
                    "contour_z must be strictly monotonically increasing",
                    "isentropic_expansion.contour_z",
                )
                .detail("min_dz", json!(min)));
            }
        }
        let areas: Vec<f64> = r.iter().map(|v| v.value() * std::f64::consts::PI * v.value()).collect();
        let throat = areas.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        let ratios: Vec<f64> = areas.iter().map(|a| (a / areas[throat]).max(1.0 + 1.0e-12)).collect();
        for (k, ratio) in ratios.iter().enumerate() {
            let (lo, hi) = if k < throat { (1.0e-4, 1.0 - 1.0e-6) } else { (1.0 + 1.0e-6, 50.0) };
            let ends = [area_ratio(lo, gamma.value()), area_ratio(hi, gamma.value())];
            if !ratio.is_finite() || (k != throat && (ends.iter().any(|v| !v.is_finite()) || *ratio < ends[0].min(ends[1]) || *ratio > ends[0].max(ends[1]))) {
                return Err(ModelError::validation("area ratio exceeds the supported Mach branch", "isentropic_expansion.contour_r"));
            }
        }
        let result = evaluate_along_contour(r, p_c, t_c, gamma, rs, p_amb);
        if result.scalars.iter().any(|v| !v.value().is_finite()) {
            return Err(ModelError::validation("nonfinite flow result", "isentropic_expansion.flow"));
        }
        let mdot = result.scalars[6].value();
        let enthalpy = gamma.value() / (gamma.value() - 1.0) * rs.value() * t_c.value();
        if !mdot.is_finite() || mdot <= 0.0 || !enthalpy.is_finite() || enthalpy <= 0.0 {
            return Err(ModelError::validation("unsupported stagnation state", "isentropic_expansion.flow"));
        }
        for k in 0..r.len() {
            let m = result.mach[k].value();
            let p = result.pressure_pa[k].value();
            let t = result.temperature_k[k].value();
            let rho = result.density_kg_m3[k].value();
            let v = result.velocity_m_s[k].value();
            let root_error = (area_ratio(m, gamma.value()) / ratios[k] - 1.0).abs();
            let mass_error = (rho * v * areas[k] / mdot - 1.0).abs();
            let energy_error = ((gamma.value() / (gamma.value() - 1.0) * rs.value() * t + 0.5 * v * v) / enthalpy - 1.0).abs();
            if [m, p, t, rho, v].iter().any(|x| !x.is_finite() || *x <= 0.0)
                || (k < throat && m >= 1.0) || (k > throat && m <= 1.0)
                || [root_error, mass_error, energy_error].iter().any(|x| !x.is_finite() || *x > 1.0e-8) {
                return Err(ModelError::validation("flow branch or conservation check failed", "isentropic_expansion.flow"));
            }
        }
        Ok(result)
    }


    pub fn certify_sensitivity(&self, contour_r: &[f64]) -> PResult<()> {
        if contour_r.len() < 3 {
            return Err(ModelError::validation(
                "contour_r must be 1-D with at least 3 samples",
                "isentropic_expansion.contour_r",
            ));
        }
        if contour_r.iter().any(|r| !r.is_finite() || *r <= 0.0 || !(std::f64::consts::PI * *r * *r).is_finite() || std::f64::consts::PI * *r * *r <= 0.0) {
            return Err(ModelError::validation("contour_r must be finite and positive", "isentropic_expansion.contour_r"));
        }
        let area: Vec<f64> = contour_r.iter().map(|r| std::f64::consts::PI * r * r).collect();
        let minima: Vec<usize> = local_area_minima(&area).into_iter().filter(|i| *i > 0 && *i + 1 < area.len()).collect();
        if minima.is_empty() {
            return Err(ModelError::validation(
                "contour has no interior local minimum: no choked throat",
                "isentropic_expansion.contour_r",
            )
            .detail("area_samples", json!(area)));
        }
        let a_min = area.iter().copied().fold(f64::INFINITY, f64::min);
        if area.iter().any(|a| !(a / a_min).is_finite()) {
            return Err(ModelError::validation("nonfinite area ratio", "isentropic_expansion.contour_r"));
        }
        if area[0] <= a_min * (1.0 + self.switch_certificate) || area[area.len() - 1] <= a_min * (1.0 + self.switch_certificate) {
            return Err(ModelError::contract("choked throat must be an unambiguous interior minimum"));
        }
        let near = area.iter().filter(|a| **a <= a_min * (1.0 + self.switch_certificate)).count();
        if near > 1 {
            return Err(ModelError::contract(format!(
                "ambiguous choked throat: {near} samples of A(s) within {} of the global min; the sensitivity is not defined (switch-distance certificate)",
                fmt_g6(self.switch_certificate)
            )));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn certify_flow_sensitivity(&self, r: &[f64], z: &[f64], p_c: f64, t_c: f64, gamma: f64, rs: f64, p_amb: f64) -> PResult<()> {
        self.evaluate(r, z, p_c, t_c, gamma, rs, p_amb)?;
        self.certify_sensitivity(r)
    }

}

#[allow(clippy::many_single_char_names)]
pub fn evaluate_along_contour<S: Scalar>(
    r: &[S],
    p_c: S,
    t_c: S,
    gamma: S,
    rs: S,
    p_amb: S,
) -> IsentropicResult<S> {
    let n = r.len();
    let area: Vec<S> = r.iter().map(|x| *x * std::f64::consts::PI * *x).collect();
    let throat_area = crate::screening::reduce_min(&area);
    let mut throat_index = 0;
    for (k, a) in area.iter().enumerate() {
        if a.value() < area[throat_index].value() {
            throat_index = k;
        }
    }
    let ar: Vec<S> = area.iter().map(|a| (*a / throat_area).max_f64(1.0 + 1.0e-12)).collect();
    let m_sub: Vec<S> = ar.iter().map(|a| subsonic_mach(*a, gamma)).collect();
    let m_sup: Vec<S> = ar.iter().map(|a| supersonic_mach(*a, gamma)).collect();
    let mach: Vec<S> = (0..n)
        .map(|k| match k.cmp(&throat_index) {
            std::cmp::Ordering::Less => m_sub[k],
            std::cmp::Ordering::Greater => m_sup[k],
            std::cmp::Ordering::Equal => S::one(),
        })
        .collect();
    let inv: Vec<S> = mach.iter().map(|m| (gamma - 1.0) * 0.5 * *m * *m + 1.0).collect();
    let temperature: Vec<S> = inv.iter().map(|i| t_c / *i).collect();
    let pressure: Vec<S> = inv.iter().map(|i| p_c * i.pow(-gamma / (gamma - 1.0))).collect();
    let density: Vec<S> = pressure.iter().zip(&temperature).map(|(p, t)| *p / (rs * *t)).collect();
    let velocity: Vec<S> =
        mach.iter().zip(&temperature).map(|(m, t)| *m * (gamma * rs * *t).sqrt()).collect();
    let mdot = choked_mdot(p_c, t_c, gamma, rs, throat_area);
    let exit_mach = m_sup[n - 1];
    let exit_area = area[n - 1];
    let exit_pressure = p_c * pressure_ratio(exit_mach, gamma);
    let exit_temperature = t_c / ((gamma - 1.0) * 0.5 * exit_mach * exit_mach + 1.0);
    let exit_velocity = exit_mach * (gamma * rs * exit_temperature).sqrt();
    let thrust_momentum = mdot * exit_velocity + (exit_pressure - p_amb) * exit_area;
    let mut integral = S::zero();
    for k in 0..n - 1 {
        let p_mid = (pressure[k] + pressure[k + 1]) * 0.5;
        integral += (p_mid - p_amb) * (area[k + 1] - area[k]);
    }
    let thrust_wall = (pressure[0] - p_amb) * area[0] + integral + mdot * velocity[0];
    IsentropicResult {
        scalars: [
            throat_area,
            exit_mach,
            exit_pressure,
            exit_temperature,
            exit_area,
            exit_velocity,
            mdot,
            thrust_momentum,
            thrust_wall,
            thrust_momentum / (mdot * G0_M_S2),
            exit_area / throat_area,
        ],
        mach,
        pressure_pa: pressure,
        temperature_k: temperature,
        density_kg_m3: density,
        velocity_m_s: velocity,
        area_m2: area,
        throat_index,
    }
}
