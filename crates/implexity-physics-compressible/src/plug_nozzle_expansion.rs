// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::pyval::fmt_g6;
use crate::roots::{Residual, bracketed_newton_root};

pub const DEFAULT_BASE_PRESSURE_ALPHA: f64 = 3.0;

pub const DEFAULT_BASE_PRESSURE_BETA: f64 = 0.5;

pub const DEFAULT_QUADRATURE_ORDER: usize = 24;

pub const AMBIENT_FLOOR_PA: f64 = 1e-6;

pub const NEWTON_POLISH_STEPS: usize = 3;

pub const G0_M_S2: f64 = 9.80665;

pub const RESULT_NAMES: [&str; 20] = [
    "thrust_N",
    "thrust_per_unit_mass_flow_m_s",
    "specific_impulse_s",
    "mass_flow_kg_s",
    "plug_axial_thrust_N",
    "plug_surface_pressure_thrust_N",
    "sonic_line_thrust_N",
    "base_pressure_Pa",
    "base_thrust_N",
    "base_area_m2",
    "effective_exit_area_m2",
    "effective_expansion_ratio",
    "design_expansion_ratio",
    "design_exit_mach",
    "ambient_matched_mach",
    "truncation_mach",
    "plug_length_full_m",
    "plug_length_truncated_m",
    "thrust_momentum_form_N",
    "truncation_penalty",
];

#[must_use]
pub fn leggauss(n: usize) -> (Vec<f64>, Vec<f64>) {
    implexity_physics_fields::neutral::leggauss(n)
}


pub fn check_positive(prefix: &str, name: &str, value: f64, strict_lower: f64) -> PResult<()> {
    if !value.is_finite() || value <= strict_lower {
        return Err(ModelError::validation(
            format!("{name} must be finite and > {}", fmt_g6(strict_lower)),
            format!("{prefix}.{name}"),
        )
        .detail("value", json!(value)));
    }
    Ok(())
}


pub fn check_bounded(prefix: &str, name: &str, value: f64, lo: f64, hi: f64) -> PResult<()> {
    if !value.is_finite() || value < lo || value > hi {
        let mut d = Map::new();
        d.insert("value".into(), json!(value));
        d.insert("lo".into(), json!(lo));
        d.insert("hi".into(), json!(hi));
        return Err(ModelError::validation(
            format!("{name} must be finite and in [{}, {}]", fmt_g6(lo), fmt_g6(hi)),
            format!("{prefix}.{name}"),
        )
        .with_details(d));
    }
    Ok(())
}

pub fn prandtl_meyer_t<S: Scalar>(t: S, gamma: S) -> S {
    let lam = ((gamma + 1.0) / (gamma - 1.0)).sqrt();
    lam * (t / lam).atan() - t.atan()
}

pub fn area_ratio<S: Scalar>(mach: S, gamma: S) -> S {
    let expo = (gamma + 1.0) / ((gamma - 1.0) * 2.0);
    ((gamma + 1.0).recip() * 2.0 * ((gamma - 1.0) * 0.5 * mach * mach + 1.0)).pow(expo) / mach
}

pub fn pressure_ratio<S: Scalar>(mach: S, gamma: S) -> S {
    ((gamma - 1.0) * 0.5 * mach * mach + 1.0).pow(-gamma / (gamma - 1.0))
}

pub fn mach_from_pressure_ratio<S: Scalar>(p_over_pc: S, gamma: S) -> S {
    let m2 = (gamma - 1.0).recip() * 2.0 * (p_over_pc.pow(-(gamma - 1.0) / gamma) - 1.0);
    m2.max_f64(0.0).sqrt()
}

pub fn plug_surface_xy<S: Scalar>(t: S, gamma: S, nu_e: S, throat_area: S, exit_height: S) -> (S, S) {
    let mach = (t * t + 1.0).sqrt();
    let length = mach * area_ratio(mach, gamma) * throat_area;
    let theta = nu_e - prandtl_meyer_t(t, gamma);
    let mu = S::one().atan2(t);
    let phi = theta + mu;
    (length * phi.cos(), exit_height - length * phi.sin())
}

pub fn plug_surface_dy_dt<S: Scalar>(t: S, gamma: S, nu_e: S, throat_area: S) -> S {
    let mach = (t * t + 1.0).sqrt();
    let two_over = (gamma + 1.0).recip() * 2.0;
    let f = two_over * ((gamma - 1.0) * 0.5 * mach * mach + 1.0);
    let expo = (gamma + 1.0) / ((gamma - 1.0) * 2.0);
    let length = f.pow(expo) * throat_area;
    let dl_dm = expo * f.pow(expo - 1.0) * two_over * (gamma - 1.0) * mach * throat_area;
    let dl_dt = dl_dm * t / mach;
    let lam2 = (gamma + 1.0) / (gamma - 1.0);
    let dphi_dt = -(t * t / lam2 + 1.0).recip();
    let phi = nu_e - prandtl_meyer_t(t, gamma) + S::one().atan2(t);
    -(dl_dt * phi.sin() + length * phi.cos() * dphi_dt)
}

pub fn base_pressure_fit<S: Scalar>(
    chamber_pressure: S,
    ambient_pressure: S,
    truncation_fraction: S,
    alpha: S,
    beta: S,
) -> S {
    let ratio = chamber_pressure / ambient_pressure.max_f64(AMBIENT_FLOOR_PA);
    let exponent = -alpha * (-truncation_fraction + 1.0) * ratio.pow(beta);
    ambient_pressure * exponent.exp()
}

struct Truncation;

impl Residual for Truncation {
    fn eval<T: Scalar>(&self, t: T, p: &[T]) -> T {
        plug_surface_xy(t, p[0], p[1], p[2], p[3]).0 - p[4]
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PlugInputs<S> {
    pub chamber_pressure: S,
    pub chamber_temperature: S,
    pub gamma: S,
    pub specific_gas_constant: S,
    pub throat_area: S,
    pub ambient_pressure: S,
    pub design_pressure_ratio: S,
    pub truncation_fraction: S,
    pub alpha: S,
    pub beta: S,
    pub mass_flow: Option<S>,
}

#[allow(clippy::too_many_arguments)]
fn momentum_form<S: Scalar>(
    mach_1: S,
    gamma: S,
    rspec: S,
    t_c: S,
    p_c: S,
    p_amb: S,
    mdot: S,
    nu_e: S,
    throat_area: S,
    exit_height: S,
    base_pressure: S,
    base_area: S,
) -> S {
    let t_1 = (mach_1 * mach_1 - 1.0).max_f64(0.0).sqrt();
    let theta_1 = nu_e - prandtl_meyer_t(t_1, gamma);
    let temperature = t_c / ((gamma - 1.0) * 0.5 * mach_1 * mach_1 + 1.0);
    let velocity = mach_1 * (gamma * rspec * temperature).sqrt();
    let pressure = p_c * pressure_ratio(mach_1, gamma);
    let (_, y_1) = plug_surface_xy(t_1, gamma, nu_e, throat_area, exit_height);
    mdot * velocity * theta_1.cos()
        + (pressure - p_amb) * (exit_height - y_1)
        + (base_pressure - p_amb) * base_area
}

fn evaluate_kernel<S: Scalar>(
    i: &PlugInputs<S>,
    truncation_fraction: S,
    nodes: &[f64],
    weights: &[f64],
) -> [S; 19] {
    let (p_c, p_amb, gamma, throat_area) = (i.chamber_pressure, i.ambient_pressure, i.gamma, i.throat_area);
    let mach_e = mach_from_pressure_ratio(i.design_pressure_ratio.recip(), gamma);
    let eps_e = area_ratio(mach_e, gamma);
    let exit_height = eps_e * throat_area;
    let t_e = (mach_e * mach_e - 1.0).sqrt();
    let nu_e = prandtl_meyer_t(t_e, gamma);
    let t_star = i.chamber_temperature * 2.0 / (gamma + 1.0);
    let a_star = (gamma * i.specific_gas_constant * t_star).sqrt();
    let p_star = p_c * ((gamma + 1.0).recip() * 2.0).pow(gamma / (gamma - 1.0));
    let rho_star = p_star / (i.specific_gas_constant * t_star);
    let mdot = i.mass_flow.unwrap_or(rho_star * a_star * throat_area);
    let x_of = |t: S| plug_surface_xy(t, gamma, nu_e, throat_area, exit_height).0;
    let x_throat = x_of(S::zero());
    let x_tip = x_of(t_e);
    let length_full = x_tip - x_throat;
    let x_target = x_throat + truncation_fraction * length_full;
    let t_trunc: S = bracketed_newton_root(
        &Truncation,
        0.0,
        t_e.value(),
        NEWTON_POLISH_STEPS,
        &[gamma, nu_e, throat_area, exit_height, x_target],
    );
    let mach_trunc = (t_trunc * t_trunc + 1.0).sqrt();
    let base_area = plug_surface_xy(t_trunc, gamma, nu_e, throat_area, exit_height).1.max_f64(0.0);
    let mach_amb = mach_from_pressure_ratio(p_amb.max_f64(AMBIENT_FLOOR_PA) / p_c, gamma);
    let t_amb = (mach_amb * mach_amb - 1.0).max_f64(0.0).sqrt();
    let t_1 = t_trunc.minimum(t_amb);
    let mach_1 = (t_1 * t_1 + 1.0).sqrt();
    let sonic_line_thrust = (mdot * a_star + (p_star - p_amb) * throat_area) * nu_e.cos();
    let mut total = S::zero();
    for (x, w) in nodes.iter().zip(weights) {
        let t_node = t_1 * 0.5 * (x + 1.0);
        let mach_node = (t_node * t_node + 1.0).sqrt();
        let integrand = (p_c * pressure_ratio(mach_node, gamma) - p_amb)
            * (-plug_surface_dy_dt(t_node, gamma, nu_e, throat_area));
        total += integrand * *w;
    }
    let plug_surface_thrust = t_1 * 0.5 * total;
    let base_pressure = base_pressure_fit(p_c, p_amb, truncation_fraction, i.alpha, i.beta);
    let base_thrust = (base_pressure - p_amb) * base_area;
    let plug_axial_thrust = sonic_line_thrust + plug_surface_thrust;
    let thrust = plug_axial_thrust + base_thrust;
    let eps_eff = area_ratio(mach_1, gamma);
    let momentum = momentum_form(
        mach_1,
        gamma,
        i.specific_gas_constant,
        i.chamber_temperature,
        p_c,
        p_amb,
        mdot,
        nu_e,
        throat_area,
        exit_height,
        base_pressure,
        base_area,
    );
    [
        thrust,
        thrust / mdot,
        thrust / (mdot * G0_M_S2),
        mdot,
        plug_axial_thrust,
        plug_surface_thrust,
        sonic_line_thrust,
        base_pressure,
        base_thrust,
        base_area,
        eps_eff * throat_area,
        eps_eff,
        eps_e,
        mach_e,
        mach_amb,
        mach_trunc,
        length_full,
        x_target - x_throat,
        momentum,
    ]
}

fn validate_contour<S: Scalar>(inputs: &PlugInputs<S>, out: &[S; 19], fraction: f64) -> PResult<()> {
    let invalid = || ModelError::validation("computed contour is outside the supported branch", "plug_nozzle.result");
    if out.iter().any(|v| !v.value().is_finite()) {
        return Err(invalid());
    }
    let (g, area, me, mt) = (inputs.gamma.value(), inputs.throat_area.value(), out[13].value(), out[15].value());
    let length = out[16].value();
    if me <= 1.0 || mt < 1.0 || mt > me + 64.0 * f64::EPSILON * me || length <= 0.0 || out[3].value() <= 0.0 {
        return Err(invalid());
    }
    let te = (me * me - 1.0).sqrt();
    let tt = (mt * mt - 1.0).sqrt();
    let nu = prandtl_meyer_t(te, g);
    let height = out[12].value() * area;
    let x0 = plug_surface_xy(0.0, g, nu, area, height).0;
    let xt = plug_surface_xy(tt, g, nu, area, height).0;
    let target = x0 + fraction * length;
    let tolerance = 1e-8 * length + 64.0 * f64::EPSILON * (x0.abs() + xt.abs() + target.abs());
    if !te.is_finite() || !tt.is_finite() || !xt.is_finite() || !target.is_finite()
        || !tolerance.is_finite() || (xt - target).abs() > tolerance {
        return Err(invalid());
    }
    Ok(())
}


pub fn plug_nozzle_expansion<S: Scalar>(inputs: &PlugInputs<S>, quadrature_order: usize) -> PResult<[S; 20]> {
    if quadrature_order < 2 {
        return Err(ModelError::validation(
            "quadrature_order must be an integer >= 2",
            "plug_nozzle.quadrature_order",
        ));
    }
    let pre = "plug_nozzle";
    check_positive(pre, "chamber_pressure", inputs.chamber_pressure.value(), 0.0)?;
    check_positive(pre, "chamber_temperature", inputs.chamber_temperature.value(), 0.0)?;
    check_positive(pre, "gamma", inputs.gamma.value(), 1.0)?;
    check_positive(pre, "specific_gas_constant", inputs.specific_gas_constant.value(), 0.0)?;
    check_positive(pre, "throat_area", inputs.throat_area.value(), 0.0)?;
    check_positive(pre, "design_pressure_ratio", inputs.design_pressure_ratio.value(), 1.0)?;
    check_positive(pre, "truncation_fraction", inputs.truncation_fraction.value(), 0.0)?;
    check_positive(pre, "alpha", inputs.alpha.value(), 0.0)?;
    check_positive(pre, "beta", inputs.beta.value(), 0.0)?;
    if let Some(m) = inputs.mass_flow {
        check_positive(pre, "mass_flow", m.value(), 0.0)?;
    }
    let l_frac = inputs.truncation_fraction.value();
    if l_frac > 1.0 {
        return Err(ModelError::validation(
            "truncation_fraction must lie in (0, 1]",
            "plug_nozzle.truncation_fraction",
        )
        .detail("value", json!(l_frac)));
    }
    let (p_amb, p_c, g) =
        (inputs.ambient_pressure.value(), inputs.chamber_pressure.value(), inputs.gamma.value());
    if !p_amb.is_finite() || p_amb < 0.0 {
        return Err(ModelError::validation(
            "ambient_pressure must be finite and >= 0",
            "plug_nozzle.ambient_pressure",
        )
        .detail("value", json!(p_amb)));
    }
    let critical_ratio = ((g + 1.0) / 2.0).powf(g / (g - 1.0));
    if !critical_ratio.is_finite() || inputs.design_pressure_ratio.value() <= critical_ratio {
        return Err(ModelError::validation(
            "design_pressure_ratio must give a supersonic full contour",
            "plug_nozzle.design_pressure_ratio",
        ).detail("sonic_pressure_ratio", json!(critical_ratio)));
    }
    let choke = p_c * (2.0 / (g + 1.0)).powf(g / (g - 1.0));
    if p_amb >= choke {
        let mut d = Map::new();
        d.insert("ambient_pressure".into(), json!(p_amb));
        d.insert("throat_pressure".into(), json!(choke));
        return Err(ModelError::validation(
            "ambient_pressure must be below the sonic throat pressure (unchoked nozzle is out of scope)",
            "plug_nozzle.ambient_pressure",
        )
        .with_details(d));
    }
    let (nodes, weights) = leggauss(quadrature_order);
    let full = evaluate_kernel(inputs, S::one(), &nodes, &weights);
    let out = evaluate_kernel(inputs, inputs.truncation_fraction, &nodes, &weights);
    validate_contour(inputs, &full, 1.0)?;
    validate_contour(inputs, &out, l_frac)?;
    let penalty = -(out[0] / full[0]) + 1.0;
    if !penalty.value().is_finite() {
        return Err(ModelError::validation("truncation penalty must be finite", "plug_nozzle.result"));
    }
    Ok(std::array::from_fn(|k| if k < 19 { out[k] } else { penalty }))
}

#[must_use]
pub fn to_value(values: &[f64; 20]) -> Value {
    Value::Object(RESULT_NAMES.iter().zip(values).map(|(k, v)| ((*k).to_string(), json!(v))).collect())
}

#[allow(clippy::too_many_arguments)]
pub fn bell_nozzle_thrust<S: Scalar>(
    chamber_pressure: S,
    chamber_temperature: S,
    gamma: S,
    specific_gas_constant: S,
    throat_area: S,
    ambient_pressure: S,
    design_pressure_ratio: S,
    mass_flow: Option<S>,
) -> S {
    let (p_c, t_c, g, r, a_t) =
        (chamber_pressure, chamber_temperature, gamma, specific_gas_constant, throat_area);
    let p_e = p_c / design_pressure_ratio;
    let mach_e = mach_from_pressure_ratio(p_e / p_c, g);
    let a_e = area_ratio(mach_e, g) * a_t;
    let mdot = mass_flow.unwrap_or_else(|| {
        let choking = (g / (r * t_c)).sqrt() * ((g + 1.0).recip() * 2.0).pow((g + 1.0) / ((g - 1.0) * 2.0));
        a_t * p_c * choking
    });
    let v_e = (g * 2.0 / (g - 1.0) * r * t_c * (-(p_e / p_c).pow((g - 1.0) / g) + 1.0)).sqrt();
    mdot * v_e + (p_e - ambient_pressure) * a_e
}
