// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::sync::Arc;
use implexity_ad::Scalar;
use implexity_physics_base::materials::MaterialCard;
use implexity_physics_solid::fatigue_life::{FatigueInputs, FatigueLifeCoffinManson, FatigueLifeResults};
use implexity_physics_thermofluid::channel_transport::{
    ChannelInputs, ChannelResult, CompressibleChannelTransport, DEFAULT_VISCOSITY_PA_S,
};
use implexity_physics_thermofluid::thermal_stress_wall::{
    StationValue, ThermalStressResult, ThermalStressWall,
};
use implexity_physics_thermofluid::wall_closure::{WallClosureField, WallClosureInputs};
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};

use crate::screening::reduce_max;

#[derive(Debug, Clone, PartialEq)]
pub enum Out<S> {
    Scalar(S),
    Vector(Vec<S>),
    Matrix(Vec<Vec<S>>),
    Index(i64),
    Map(Outputs<S>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outputs<S>(pub Vec<(String, Out<S>)>);

impl<S: Clone> Outputs<S> {
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn set(&mut self, key: &str, value: Out<S>) {
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            self.0.push((key.to_string(), value));
        }
    }

    pub fn scalar_set(&mut self, key: &str, value: S) {
        self.set(key, Out::Scalar(value));
    }

    pub fn vector_set(&mut self, key: &str, value: Vec<S>) {
        self.set(key, Out::Vector(value));
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Out<S>> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[must_use]
    pub fn scalar(&self, key: &str) -> Option<S> {
        match self.get(key)? {
            Out::Scalar(s) => Some(s.clone()),
            _ => None,
        }
    }

    #[must_use]
    pub fn vector(&self, key: &str) -> Option<&[S]> {
        match self.get(key)? {
            Out::Vector(v) => Some(v),
            _ => None,
        }
    }

    #[must_use]
    pub fn matrix(&self, key: &str) -> Option<&[Vec<S>]> {
        match self.get(key)? {
            Out::Matrix(v) => Some(v),
            _ => None,
        }
    }

    #[must_use]
    pub fn map(&self, key: &str) -> Option<&Outputs<S>> {
        match self.get(key)? {
            Out::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn update(&mut self, other: Outputs<S>) {
        for (k, v) in other.0 {
            self.set(&k, v);
        }
    }
}

impl<S: Scalar> Outputs<S> {
    #[must_use]
    pub fn to_value(&self) -> Value {
        fn one<S: Scalar>(o: &Out<S>) -> Value {
            match o {
                Out::Scalar(s) => json!(s.value()),
                Out::Vector(v) => json!(v.iter().map(Scalar::value).collect::<Vec<_>>()),
                Out::Matrix(m) => json!(
                    m.iter().map(|r| r.iter().map(Scalar::value).collect::<Vec<_>>()).collect::<Vec<_>>()
                ),
                Out::Index(i) => json!(i),
                Out::Map(m) => m.to_value(),
            }
        }
        Value::Object(self.0.iter().map(|(k, v)| (k.clone(), one(v))).collect::<Map<String, Value>>())
    }
}

pub fn stress_outputs<S: Scalar>(r: &ThermalStressResult<S>) -> Outputs<S> {
    let mut o = Outputs::new();
    o.vector_set("sigma_theta_theta_Pa", r.sigma_theta_theta_pa.clone());
    o.vector_set("sigma_zz_Pa", r.sigma_zz_pa.clone());
    o.vector_set("sigma_von_mises_Pa", r.sigma_von_mises_pa.clone());
    o.vector_set("strain_amplitude", r.strain_amplitude.clone());
    o.vector_set("delta_T_K", r.delta_t_k.clone());
    o
}

pub fn fatigue_outputs<S: Scalar>(r: &FatigueLifeResults<S>) -> Outputs<S> {
    let mut o = Outputs::new();
    o.vector_set("N_f", r.n_f.clone());
    o.vector_set("two_Nf_reversals", r.two_nf_reversals.clone());
    o.scalar_set("N_f_min", r.n_f_min);
    o.set("min_station_index", Out::Index(i64::from(r.min_station_index)));
    o.vector_set("elastic_strain_amp", r.elastic_strain_amp.clone());
    o.vector_set("plastic_strain_amp", r.plastic_strain_amp.clone());
    o.scalar_set("miner_damage_sum", r.miner_damage_sum);
    o.vector_set("miner_damage_per_station", r.miner_damage_per_station.clone());
    o.vector_set("mean_stress_Pa", r.mean_stress_pa.clone());
    o.vector_set("stress_concentration_factor", r.stress_concentration_factor.clone());
    o.vector_set("effective_strain_amplitude", r.effective_strain_amplitude.clone());
    o
}


#[derive(Debug, Clone, Default)]
pub struct CycleInputs<S> {
    pub nozzle_contour_r: Vec<S>,
    pub nozzle_contour_z: Vec<S>,
    pub channel_s: Vec<S>,
    pub channel_area: Vec<S>,
    pub channel_d_h: Vec<S>,
    pub channel_perimeter: Vec<S>,
    pub wall_mode_logits: Vec<Vec<S>>,
    pub wall_film_h: Option<Vec<S>>,
    pub pump_direction: S,
    pub pump_alpha: S,
    pub pump_tip_radius_m: S,
    pub pump_shaft_rpm: S,
    pub p_chamber: S,
    pub t_chamber: S,
    pub y_fuel: Vec<S>,
    pub y_ox: Vec<S>,
    pub mdot_fuel: S,
    pub mdot_ox: S,
    pub fuel_tank_p: S,
    pub fuel_tank_t: S,
    pub p_ambient: S,
    pub injector_face_logits: Vec<[S; 3]>,
    pub injector_cell_area: Vec<S>,
    pub injector_combustion_efficiency: S,
    pub injector_heat_of_combustion: S,
    pub p_ox_supply: S,
    pub t_ox_supply: S,
    pub p_chamber_bootstrap: Option<S>,
    pub t_chamber_bootstrap: Option<S>,
    pub n_outer_iterations: Option<i64>,
    pub material_zone_logits: Vec<Vec<S>>,
    pub softmax_material_tau: f64,
    pub wall_thickness_m_field: Option<Vec<S>>,
    pub turbine_alpha: Option<S>,
    pub turbine_tip_radius_m: Option<S>,
    pub proj_beta: f64,
}

pub fn v(prefix: &str, name: &str, message: impl Into<String>) -> ModelError {
    ModelError::validation(message, format!("{prefix}.{name}"))
}

pub fn recovery<S: Scalar>(t_static: &[S], mach: &[S], gamma: S, r: f64) -> Vec<S> {
    t_static.iter().zip(mach).map(|(t, m)| *t * ((gamma - 1.0) * r * 0.5 * *m * *m + 1.0)).collect()
}

pub fn max_abs_diff<S: Scalar>(a: &[S], b: &[S]) -> S {
    let d: Vec<S> = a.iter().zip(b).map(|(x, y)| (*x - *y).abs()).collect();
    reduce_max(&d)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn picard<S: Scalar>(
    wall: &WallClosureField,
    channel: &CompressibleChannelTransport,
    i: &CycleInputs<S>,
    film_h: &[S],
    t_hot: &[S],
    p_in: S,
    t_in: S,
    tau: f64,
    iterations: usize,
) -> PResult<(Vec<S>, Vec<S>, ChannelResult<S>, Vec<S>)> {
    let n = i.channel_s.len();
    let mut t_cold = vec![t_in; n];
    let mut residuals = Vec::with_capacity(iterations);
    let mut last: Option<(Vec<S>, ChannelResult<S>)> = None;
    for _ in 0..iterations {
        let wc = wall.evaluate(&WallClosureInputs {
            mode_logits: i.wall_mode_logits.clone(),
            q_authored: vec![S::zero(); n],
            t_authored: t_hot.to_vec(),
            film_h: film_h.to_vec(),
            t_hot_side: t_hot.to_vec(),
            t_cold_side: t_cold.clone(),
            tau: S::from_f64(tau),
        })?;
        let out = channel.evaluate(&ChannelInputs {
            s: i.channel_s.clone(),
            area: i.channel_area.clone(),
            d_hydraulic: i.channel_d_h.clone(),
            wall_heat_flux: wc.heat_flux_w_m2.clone(),
            wetted_perimeter: i.channel_perimeter.clone(),
            p_in,
            t_in,
            mdot: i.mdot_fuel,
            y_in: i.y_fuel.clone(),
            wall_roughness: S::zero(),
            viscosity: S::from_f64(DEFAULT_VISCOSITY_PA_S),
        })?;
        residuals.push(max_abs_diff(&out.temperature_k, &t_cold));
        t_cold.clone_from(&out.temperature_k);
        last = Some((wc.heat_flux_w_m2, out));
    }
    let (q, out) =
        last.ok_or_else(|| ModelError::invalid("the Picard loop requires at least one iteration"))?;
    Ok((t_cold, q, out, residuals))
}

#[must_use]
pub fn pad_lmp_curves(materials: &[Arc<MaterialCard>]) -> (Vec<f64>, Vec<Vec<f64>>) {
    let lo =
        materials.iter().flat_map(|m| m.lmp_stress_curve.iter().map(|p| p[0])).fold(f64::INFINITY, f64::min);
    let hi = materials
        .iter()
        .flat_map(|m| m.lmp_stress_curve.iter().map(|p| p[0]))
        .fold(f64::NEG_INFINITY, f64::max);
    let grid = implexity_mesh::numeric::linspace(lo, hi, 32);
    let curves = materials
        .iter()
        .map(|m| {
            let xp: Vec<f64> = m.lmp_stress_curve.iter().map(|p| p[0]).collect();
            let yp: Vec<f64> = m.lmp_stress_curve.iter().map(|p| p[1]).collect();
            grid.iter().map(|x| implexity_mesh::numeric::interp(*x, &xp, &yp)).collect()
        })
        .collect();
    (grid, curves)
}

pub fn lookup_lmp_from_stress<S: Scalar>(stress: S, xp: &[f64], fp: &[f64]) -> S {
    let x = stress.log10();
    let xv = x.value();
    let n = xp.len();
    if n < 2 || xv.is_nan() || xv < xp[0] || xv > xp[n - 1] {
        return S::from_f64(f64::NAN);
    }
    let i = xp.partition_point(|p| *p <= xv).clamp(1, n - 1);
    let df = fp[i] - fp[i - 1];
    let dx = xp[i] - xp[i - 1];
    if dx.abs() <= f64::EPSILON * f64::EPSILON {
        return S::from_f64(fp[i - 1]);
    }
    (x - xp[i - 1]) / dx * df + fp[i - 1]
}


pub fn material_weights<S: Scalar>(
    logits: &[Vec<S>],
    tau: f64,
    n: usize,
    n_mat: usize,
    prefix: &str,
) -> PResult<Vec<Vec<S>>> {
    if logits.len() != n || logits.iter().any(|r| r.len() != n_mat) {
        let shape = if logits.is_empty() {
            "(0,)".to_string()
        } else {
            format!("({}, {})", logits.len(), logits[0].len())
        };
        return Err(v(
            prefix,
            "material_zone_logits",
            format!("material_zone_logits must be shape ({n}, {n_mat}); got {shape}"),
        ));
    }
    let tau = tau.max(1e-6);
    Ok(logits
        .iter()
        .map(|row| {
            let scaled: Vec<S> = row.iter().map(|x| *x / tau).collect();
            let m = reduce_max(&scaled);
            let e: Vec<S> = scaled.iter().map(|x| (*x - m).exp()).collect();
            let mut total = S::zero();
            for x in &e {
                total += *x;
            }
            let total = total.max_f64(1e-30);
            e.iter().map(|x| *x / total).collect()
        })
        .collect())
}

pub fn blend<S: Scalar>(w: &[Vec<S>], values: &[f64]) -> Vec<S> {
    w.iter()
        .map(|row| {
            let mut s = S::zero();
            for (a, b) in row.iter().zip(values) {
                s += *a * *b;
            }
            s
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Blend<S> {
    pub w: Vec<Vec<S>>,
    pub e: Vec<S>,
    pub alpha: Vec<S>,
    pub nu: Vec<S>,
    pub k: Vec<S>,
    pub sfp: Vec<S>,
    pub b: Vec<S>,
    pub efp: Vec<S>,
    pub c: Vec<S>,
    pub lmc: Vec<S>,
}

pub fn blend_materials<S: Scalar>(w: Vec<Vec<S>>, materials: &[Arc<MaterialCard>]) -> Blend<S> {
    let col = |f: &dyn Fn(&MaterialCard) -> f64| materials.iter().map(|m| f(m)).collect::<Vec<f64>>();
    Blend {
        e: blend(&w, &col(&|m| m.e_pa)),
        alpha: blend(&w, &col(&|m| m.alpha_per_k)),
        nu: blend(&w, &col(&|m| m.nu)),
        k: blend(&w, &col(&|m| m.k_w_per_m_k)),
        sfp: blend(&w, &col(&|m| m.sigma_f_prime_pa)),
        b: blend(&w, &col(&|m| m.b)),
        efp: blend(&w, &col(&|m| m.eps_f_prime)),
        c: blend(&w, &col(&|m| m.c)),
        lmc: blend(&w, &col(&|m| m.larson_miller_c)),
        w,
    }
}

pub struct Mechanics<S> {
    pub stress: ThermalStressResult<S>,
    pub fatigue: FatigueLifeResults<S>,
    pub lmp_eff: Vec<S>,
    pub log10_tr: Vec<S>,
    pub t_r: Vec<S>,
    pub creep: Vec<S>,
}

#[allow(clippy::too_many_arguments)]
pub fn mechanics<S: Scalar>(
    t_wall: &[S],
    bl: &Blend<S>,
    materials: &[Arc<MaterialCard>],
    stress: ThermalStressWall,
    fatigue: FatigueLifeCoffinManson,
    t_ref: f64,
    dwell: f64,
    n_applied: f64,
) -> PResult<Mechanics<S>> {
    let n = t_wall.len();
    let stress_out = stress.evaluate(
        t_wall,
        &StationValue::Scalar(S::from_f64(t_ref)),
        &StationValue::Stations(bl.e.clone()),
        &StationValue::Stations(bl.alpha.clone()),
        &StationValue::Stations(bl.nu.clone()),
    )?;
    let n_app = vec![S::from_f64(n_applied); n];
    let fat = fatigue.evaluate(&FatigueInputs {
        strain_amplitude: &stress_out.strain_amplitude,
        e_material: &bl.e,
        sigma_f_prime: &bl.sfp,
        b: &bl.b,
        eps_f_prime: &bl.efp,
        c: &bl.c,
        n_applied: Some(&n_app),
        mean_stress: None,
        stress_concentration_factor: None,
    })?;
    let (grid, curves) = pad_lmp_curves(materials);
    let lmp_eff: Vec<S> = (0..n)
        .map(|st| {
            let mut s = S::zero();
            for (j, curve) in curves.iter().enumerate() {
                s += bl.w[st][j] * lookup_lmp_from_stress(stress_out.sigma_von_mises_pa[st], &grid, curve);
            }
            s
        })
        .collect();
    let log10_tr: Vec<S> = (0..n).map(|st| lmp_eff[st] / t_wall[st].max_f64(1.0) - bl.lmc[st]).collect();
    let t_r: Vec<S> = log10_tr.iter().map(|x| S::from_f64(10.0).pow(*x)).collect();
    let creep: Vec<S> = t_r.iter().map(|t| S::from_f64(dwell) / t.max_f64(1.0e-30)).collect();
    Ok(Mechanics { stress: stress_out, fatigue: fat, lmp_eff, log10_tr, t_r, creep })
}

pub fn sum<S: Scalar>(v: &[S]) -> S {
    let mut s = S::zero();
    for x in v {
        s += *x;
    }
    s
}

pub fn mechanics_outputs<S: Scalar>(m: &Mechanics<S>) -> Outputs<S> {
    let creep_sum = sum(&m.creep);
    let creep_max = reduce_max(&m.creep);
    let mut o = Outputs::new();
    o.scalar_set("wall_stress_max_vm_Pa", reduce_max(&m.stress.sigma_von_mises_pa));
    o.scalar_set("wall_strain_amplitude_max", reduce_max(&m.stress.strain_amplitude));
    o.scalar_set("fatigue_min_Nf", m.fatigue.n_f_min);
    o.scalar_set("fatigue_damage_sum", m.fatigue.miner_damage_sum);
    o.scalar_set("creep_max_damage", creep_max);
    o.scalar_set("creep_damage_sum", creep_sum);
    o
}

pub fn mechanics_arrays<S: Scalar>(m: &Mechanics<S>, o: &mut Outputs<S>) {
    let creep_sum = sum(&m.creep);
    let creep_max = reduce_max(&m.creep);
    o.vector_set("sigma_von_mises_Pa", m.stress.sigma_von_mises_pa.clone());
    o.vector_set("sigma_theta_theta_Pa", m.stress.sigma_theta_theta_pa.clone());
    o.vector_set("sigma_zz_Pa", m.stress.sigma_zz_pa.clone());
    o.vector_set("strain_amplitude", m.stress.strain_amplitude.clone());
    o.vector_set("fatigue_N_f", m.fatigue.n_f.clone());
    o.vector_set("creep_t_rupture_hours", m.t_r.clone());
    o.vector_set("creep_damage_per_station", m.creep.clone());
    o.set("stress_result", Out::Map(stress_outputs(&m.stress)));
    o.set("fatigue_result", Out::Map(fatigue_outputs(&m.fatigue)));
    let mut c = Outputs::new();
    c.vector_set("t_rupture_hours", m.t_r.clone());
    c.vector_set("log10_t_rupture", m.log10_tr.clone());
    c.vector_set("LMP_per_station", m.lmp_eff.clone());
    c.scalar_set("creep_damage_sum", creep_sum);
    c.vector_set("creep_damage_per_station", m.creep.clone());
    c.scalar_set("creep_damage_max", creep_max);
    o.set("creep_result", Out::Map(c));
}

pub fn check_materials(prefix: &str, materials: &[Arc<MaterialCard>]) -> PResult<()> {
    if materials.len() < 2 {
        return Err(v(prefix, "materials", "materials must contain at least 2 entries")
            .detail("n", json!(materials.len())));
    }
    Ok(())
}

pub fn guest_prevost_project<S: Scalar>(t_raw: &[S], t_min: f64, t_max: f64, beta: f64) -> Vec<S> {
    let n = t_raw.len();
    let span = t_max - t_min;
    let mut smooth = Vec::with_capacity(n);
    smooth.push((t_raw[0] * 2.0 + t_raw[1]) / 3.0);
    for k in 1..n - 1 {
        smooth.push((t_raw[k - 1] + t_raw[k] * 2.0 + t_raw[k + 1]) * 0.25);
    }
    smooth.push((t_raw[n - 1] * 2.0 + t_raw[n - 2]) / 3.0);
    if beta <= 1.0e-6 || span <= 0.0 {
        return smooth;
    }
    let mid = 0.5 * (t_min + t_max);
    let tanh_beta = beta.tanh().max(1.0e-9);
    smooth
        .into_iter()
        .map(|t| {
            let u = (t - mid) * 2.0 / span.max(1.0e-12);
            let vv = (u * beta).tanh() / tanh_beta;
            (vv * (0.5 * span) + mid).clip(t_min, t_max)
        })
        .collect()
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn dittus_boelter_film_h<S: Scalar>(
    mdot: S,
    area: &[S],
    d_h: &[S],
    cp: f64,
    mu: f64,
    k: f64,
    h_min: f64,
    h_max: f64,
) -> (Vec<S>, Vec<S>, Vec<S>, Vec<S>, Vec<S>) {
    let pr = mu * cp / k.max(1.0e-12);
    let mut out = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (a, dh) in area.iter().zip(d_h) {
        let g = mdot / a.max_f64(1.0e-12);
        let re = g * *dh / mu.max(1.0e-12);
        let nu = re.max_f64(1.0).powf(0.8) * 0.023 * pr.powf(0.4);
        let h = (nu * k / dh.max_f64(1.0e-12)).clip(h_min, h_max);
        out.0.push(h);
        out.1.push(re);
        out.2.push(S::from_f64(pr));
        out.3.push(nu);
        out.4.push(g);
    }
    out
}

#[derive(Debug, Clone)]
pub struct BartzInputs<'a, S> {
    pub d_throat: S,
    pub a_throat: S,
    pub a_local: &'a [S],
    pub r_c_throat: S,
    pub p_c: S,
    pub t_c: S,
    pub mdot_choked: S,
    pub gamma: S,
    pub mach: &'a [S],
    pub t_wall_guess: &'a [S],
}

#[allow(clippy::too_many_arguments)]
pub fn bartz_film_coefficient<S: Scalar>(
    b: &BartzInputs<'_, S>,
    cp_gas: f64,
    mu_gas: f64,
    pr_gas: f64,
    h_min: f64,
    h_max: f64,
) -> (Vec<S>, Vec<S>, S) {
    let c_star = b.p_c * b.a_throat / b.mdot_choked.max_f64(1.0e-6);
    let c_star_safe = c_star.max_f64(100.0);
    let d_safe = b.d_throat.max_f64(1.0e-6);
    let prefactor = S::from_f64(0.026) / d_safe.powf(0.2);
    let prop_group = mu_gas.powf(0.2) * cp_gas / pr_gas.powf(0.6);
    let pressure_group = (b.p_c / c_star_safe).powf(0.8);
    let curvature_group = (d_safe / b.r_c_throat.max_f64(1.0e-6)).powf(0.1);
    let mut h = Vec::with_capacity(b.mach.len());
    let mut sigma = Vec::with_capacity(b.mach.len());
    for k in 0..b.mach.len() {
        let factor_m = (b.gamma - 1.0) * 0.5 * (b.mach[k] * b.mach[k]) + 1.0;
        let tw = b.t_wall_guess[k] / b.t_c.max_f64(1.0);
        let term1 = (tw * 0.5 * factor_m + 0.5).max_f64(1.0e-3);
        let s = (term1.powf(0.68) * factor_m.powf(0.12)).recip();
        let a_factor = (b.a_throat / b.a_local[k].max_f64(1.0e-12)).powf(0.9);
        let raw = prefactor * prop_group * pressure_group * curvature_group * s * a_factor;
        h.push(raw.clip(h_min, h_max));
        sigma.push(s);
    }
    (h, sigma, c_star)
}

#[allow(clippy::too_many_arguments)]
pub fn thermal_series<S: Scalar>(t_hot_clipped:&[S], t_cold:&[S], h_gas:&[S], h_cold:&[S], t_wall:&[S], k_eff:&[S]) -> (Vec<S>,Vec<S>,Vec<S>) {
        let n = t_hot_clipped.len();
        let mut t_metal = Vec::with_capacity(n);
        let mut q_new = Vec::with_capacity(n);
        let mut dt_new = Vec::with_capacity(n);
        for k in 0..n {
            let r_gas = h_gas[k].max_f64(1.0).recip();
            let r_wall = t_wall[k] / k_eff[k].max_f64(1.0);
            let r_cool = h_cold[k].max_f64(1.0).recip();
            let r_tot = r_gas + r_wall + r_cool;
            let weight = r_gas / r_tot.max_f64(1.0e-30);
            t_metal.push((t_cold[k] + (t_hot_clipped[k] - t_cold[k]) * weight).minimum(t_hot_clipped[k]));
            let q = (t_hot_clipped[k] - t_cold[k]) / r_tot.max_f64(1.0e-30);
            dt_new.push(q * r_wall);
            q_new.push(q);
        }
        (t_metal,q_new,dt_new)
}

#[allow(clippy::too_many_arguments)]
pub fn inferred_film_series<S:Scalar>(t_hot:&[S], t_cold:&[S], q2:&[S], t_wall:&[S], k_eff:&[S], h_cool:f64, gas_film_fraction:f64)->(Vec<S>,Vec<S>,Vec<S>){
        let n=t_hot.len();
        let r_cool = 1.0 / h_cool;
        let mut t_metal = Vec::with_capacity(n);
        let mut dt_wall = Vec::with_capacity(n);
        let mut h_gas = Vec::with_capacity(n);
        for k in 0..n {
            let h_comp = q2[k] / (t_hot[k] - t_cold[k]).max_f64(1.0);
            let r_gas = S::from_f64(gas_film_fraction) / h_comp.max_f64(1.0);
            let r_wall = t_wall[k] / k_eff[k].max_f64(1.0);
            let r_tot = r_gas + r_wall + r_cool;
            let weight = r_gas / r_tot.max_f64(1.0e-30);
            t_metal.push((t_cold[k] + (t_hot[k] - t_cold[k]) * weight).minimum(t_hot[k]));
            let q3 = (t_hot[k] - t_cold[k]) / r_tot.max_f64(1.0e-30);
            dt_wall.push(q3 * r_wall);
            h_gas.push(r_gas.max_f64(1.0e-30).recip());
        }
        (t_metal,dt_wall,h_gas)
}

pub fn wall_drop<S:Scalar>(q:&[S], t_cold:&[S], t_hot:&[S], conductivity:&[S], thickness:f64)->(Vec<S>,Vec<S>){
        let n=t_hot.len();
        let dt_wall: Vec<S> = (0..n).map(|k| q[k] * thickness / conductivity[k].max_f64(1.0)).collect();
        let t_metal: Vec<S> = (0..n).map(|k| (t_cold[k] + dt_wall[k]).minimum(t_hot[k])).collect();
        (dt_wall,t_metal)
}

pub fn characteristic_velocity<S:Scalar>(p_c:S,a_throat:S,mdot_choked:S)->S { p_c * a_throat / mdot_choked.max_f64(1.0e-6) }
