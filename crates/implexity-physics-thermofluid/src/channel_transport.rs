// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_physics_base::model_errors::{PhysicsError, PhysicsModelErrorKind, PhysicsResult};
use implexity_physics_cfd::pyfmt::{fmt_e, fmt_g};

use crate::equation_of_state::EquationOfState;

const COLEBROOK_NEWTON_STEPS: usize = 3;
const DEFAULT_SEGMENT_REFINE_STEPS: usize = 4;
const FRICTION_FLOOR: f64 = 1.0e-6;
const INV_SQRT_FLOOR: f64 = 1.0e-4;

pub const G0_M_S2: f64 = 9.80665;
pub const DEFAULT_VISCOSITY_PA_S: f64 = 1.0e-4;

pub const SUPPORTED_FRICTION_REGIMES: [&str; 5] = ["laminar", "blasius", "smooth_wall", "colebrook", "none"];

pub const RESULT_NAMES: [&str; 24] = [
    "pressure_Pa",
    "temperature_K",
    "density_kg_m3",
    "velocity_m_s",
    "mach",
    "sound_speed_m_s",
    "total_pressure_Pa",
    "total_temperature_K",
    "total_enthalpy_J_kg",
    "reynolds_number",
    "friction_factor",
    "wall_shear_stress_Pa",
    "outlet_pressure_Pa",
    "outlet_temperature_K",
    "outlet_total_pressure_Pa",
    "outlet_total_temperature_K",
    "outlet_mass_flow_kg_s",
    "outlet_composition",
    "pressure_drop_Pa",
    "total_heat_pickup_W",
    "energy_balance_residual",
    "segment_energy_residual_W",
    "segment_momentum_residual_N",
    "maximum_scaled_balance_residual",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrictionRegime {
    Laminar,
    Blasius,
    SmoothWall,
    Colebrook,
    None,
}

impl FrictionRegime {

    pub fn parse(s: &str) -> PhysicsResult<Self> {
        Ok(match s {
            "laminar" => Self::Laminar,
            "blasius" => Self::Blasius,
            "smooth_wall" => Self::SmoothWall,
            "colebrook" => Self::Colebrook,
            "none" => Self::None,
            other => {
                return Err(PhysicsError::contract(format!(
                    "friction_regime={} not in ('laminar', 'blasius', 'smooth_wall', 'colebrook', 'none')",
                    implexity_core::py_repr::repr_str(other)
                )));
            }
        })
    }
}

fn validation(message: String, path: &str, details: Map<String, Value>) -> PhysicsError {
    PhysicsError::Model { kind: PhysicsModelErrorKind::Validation, message, path: path.into(), details }
}

fn colebrook_iterate<S: Scalar>(f_seed: S, re_safe: S, roughness_over_dh: S) -> S {
    let mut f = f_seed;
    let a = roughness_over_dh / 3.7;
    let b = S::from_f64(2.51) / re_safe;
    let ln10 = 10.0_f64.ln();
    for _ in 0..COLEBROOK_NEWTON_STEPS {
        let f_safe = f.max_f64(FRICTION_FLOOR);
        let x = S::one() / f_safe.sqrt();
        let arg = a + b * x;
        let arg_safe = arg.max_f64(1.0e-30);
        let r = x + log10(arg_safe) * 2.0;
        let drdx = b * (2.0 / ln10) / arg_safe + 1.0;
        let x_new = x - r / drdx;
        let x_new_safe = x_new.max_f64(INV_SQRT_FLOOR);
        f = S::one() / (x_new_safe * x_new_safe);
    }
    f
}

fn log10<S: Scalar>(x: S) -> S {
    x.ln() / 10.0_f64.ln()
}

#[must_use]
pub fn friction_factor<S: Scalar>(regime: FrictionRegime, re: S, dh: S, roughness: S) -> S {
    let re_safe = re.max_f64(1.0);
    let f_lam = S::from_f64(64.0) / re_safe;
    let blend = |f_turb: S| {
        let w = ((re_safe - 3000.0) / 500.0).sigmoid();
        (S::one() - w) * f_lam + w * f_turb
    };
    match regime {
        FrictionRegime::None => re_safe * 0.0,
        FrictionRegime::Laminar => f_lam,
        FrictionRegime::Blasius => blend(re_safe.powf(-0.25) * 0.3164),
        FrictionRegime::SmoothWall => {
            let l = log10(S::from_f64(5.74) / re_safe.powf(0.9));
            let seed = (S::from_f64(0.25) / (l * l)).max_f64(1.0e-6);
            blend(colebrook_iterate(seed, re_safe, re_safe * 0.0))
        }
        FrictionRegime::Colebrook => {
            let rel = roughness / dh;
            let seed_arg = rel / 3.7 + S::from_f64(5.74) / re_safe.powf(0.9);
            let l = log10(seed_arg.max_f64(1.0e-30));
            let seed = (S::from_f64(0.25) / (l * l)).max_f64(1.0e-6);
            blend(colebrook_iterate(seed, re_safe, rel))
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Local<S> {
    rho: S,
    h: S,
    cp: S,
    gamma: S,
    a: S,
    u: S,
    mach: S,
    h_t: S,
}

fn local_state<S: Scalar>(
    eos: &EquationOfState,
    y: &[S],
    p: S,
    t: S,
    area: S,
    mdot: S,
) -> PhysicsResult<Local<S>> {
    let props = eos.evaluate(y, p, t)?;
    let u = mdot / (props.density * area);
    Ok(Local {
        rho: props.density,
        h: props.enthalpy,
        cp: props.cp,
        gamma: props.gamma,
        a: props.sound_speed,
        u,
        mach: u / props.sound_speed,
        h_t: props.enthalpy + u * 0.5 * u,
    })
}

fn traced_state<S: Scalar>(eos: &EquationOfState, y: &[S], p: S, t: S, area: S, mdot: S) -> Local<S> {
    local_state(eos, y, p, t, area, mdot).unwrap_or_else(|_| {
        let nan = S::from_f64(f64::NAN);
        Local { rho: nan, h: nan, cp: nan, gamma: nan, a: nan, u: nan, mach: nan, h_t: nan }
    })
}

fn total_state<S: Scalar>(p: S, t: S, gamma: S, mach: S) -> (S, S) {
    let fac = (gamma - 1.0) * 0.5 * mach * mach + 1.0;
    (p * fac.pow(gamma / (gamma - 1.0)), t * fac)
}

fn reduce_max<S: Scalar>(xs: &[S]) -> S {
    let m = xs.iter().fold(f64::NEG_INFINITY, |m, x| {
        if x.value().is_nan() || m.is_nan() { f64::NAN } else { m.max(x.value()) }
    });
    if m.is_nan() {
        return S::from_f64(f64::NAN);
    }
    let ties: Vec<S> =
        xs.iter().copied().filter(|x| x.value().total_cmp(&m).is_eq() || (x.value() - m) == 0.0).collect();
    let k = ties.len() as f64;
    let grad = vec![1.0 / k; ties.len()];
    S::lift(m, &ties, &grad, &[])
}

fn sum<S: Scalar>(xs: &[S]) -> S {
    let mut acc = S::zero();
    for (i, x) in xs.iter().enumerate() {
        acc = if i == 0 { *x } else { acc + *x };
    }
    acc
}

type Station<S> = (S, S, S, S, S, S, S, S, S, S, S, S);

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelResult<S> {
    pub pressure_pa: Vec<S>,
    pub temperature_k: Vec<S>,
    pub density_kg_m3: Vec<S>,
    pub velocity_m_s: Vec<S>,
    pub mach: Vec<S>,
    pub sound_speed_m_s: Vec<S>,
    pub total_pressure_pa: Vec<S>,
    pub total_temperature_k: Vec<S>,
    pub total_enthalpy_j_kg: Vec<S>,
    pub reynolds_number: Vec<S>,
    pub friction_factor: Vec<S>,
    pub wall_shear_stress_pa: Vec<S>,
    pub outlet_pressure_pa: S,
    pub outlet_temperature_k: S,
    pub outlet_total_pressure_pa: S,
    pub outlet_total_temperature_k: S,
    pub outlet_mass_flow_kg_s: S,
    pub outlet_composition: Vec<S>,
    pub pressure_drop_pa: S,
    pub total_heat_pickup_w: S,
    pub energy_balance_residual: S,
    pub segment_energy_residual_w: Vec<S>,
    pub segment_momentum_residual_n: Vec<S>,
    pub maximum_scaled_balance_residual: S,
}

impl<S: Scalar> ChannelResult<S> {
    fn all_values(&self) -> impl Iterator<Item = f64> + '_ {
        let arrays = [
            &self.pressure_pa,
            &self.temperature_k,
            &self.density_kg_m3,
            &self.velocity_m_s,
            &self.mach,
            &self.sound_speed_m_s,
            &self.total_pressure_pa,
            &self.total_temperature_k,
            &self.total_enthalpy_j_kg,
            &self.reynolds_number,
            &self.friction_factor,
            &self.wall_shear_stress_pa,
            &self.outlet_composition,
            &self.segment_energy_residual_w,
            &self.segment_momentum_residual_n,
        ];
        let scalars = [
            self.outlet_pressure_pa,
            self.outlet_temperature_k,
            self.outlet_total_pressure_pa,
            self.outlet_total_temperature_k,
            self.outlet_mass_flow_kg_s,
            self.pressure_drop_pa,
            self.total_heat_pickup_w,
            self.energy_balance_residual,
            self.maximum_scaled_balance_residual,
        ];
        arrays
            .into_iter()
            .flat_map(|a| a.iter().map(Scalar::value))
            .chain(scalars.into_iter().map(|s| s.value()))
    }

    #[must_use]
    pub fn to_map(&self) -> Map<String, Value> {
        let arr = |v: &Vec<S>| json!(v.iter().map(Scalar::value).collect::<Vec<_>>());
        let mut m = Map::new();
        m.insert("pressure_Pa".into(), arr(&self.pressure_pa));
        m.insert("temperature_K".into(), arr(&self.temperature_k));
        m.insert("density_kg_m3".into(), arr(&self.density_kg_m3));
        m.insert("velocity_m_s".into(), arr(&self.velocity_m_s));
        m.insert("mach".into(), arr(&self.mach));
        m.insert("sound_speed_m_s".into(), arr(&self.sound_speed_m_s));
        m.insert("total_pressure_Pa".into(), arr(&self.total_pressure_pa));
        m.insert("total_temperature_K".into(), arr(&self.total_temperature_k));
        m.insert("total_enthalpy_J_kg".into(), arr(&self.total_enthalpy_j_kg));
        m.insert("reynolds_number".into(), arr(&self.reynolds_number));
        m.insert("friction_factor".into(), arr(&self.friction_factor));
        m.insert("wall_shear_stress_Pa".into(), arr(&self.wall_shear_stress_pa));
        m.insert("outlet_pressure_Pa".into(), json!(self.outlet_pressure_pa.value()));
        m.insert("outlet_temperature_K".into(), json!(self.outlet_temperature_k.value()));
        m.insert("outlet_total_pressure_Pa".into(), json!(self.outlet_total_pressure_pa.value()));
        m.insert("outlet_total_temperature_K".into(), json!(self.outlet_total_temperature_k.value()));
        m.insert("outlet_mass_flow_kg_s".into(), json!(self.outlet_mass_flow_kg_s.value()));
        m.insert("outlet_composition".into(), arr(&self.outlet_composition));
        m.insert("pressure_drop_Pa".into(), json!(self.pressure_drop_pa.value()));
        m.insert("total_heat_pickup_W".into(), json!(self.total_heat_pickup_w.value()));
        m.insert("energy_balance_residual".into(), json!(self.energy_balance_residual.value()));
        m.insert("segment_energy_residual_W".into(), arr(&self.segment_energy_residual_w));
        m.insert("segment_momentum_residual_N".into(), arr(&self.segment_momentum_residual_n));
        m.insert(
            "maximum_scaled_balance_residual".into(),
            json!(self.maximum_scaled_balance_residual.value()),
        );
        m
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChannelInputs<S> {
    pub s: Vec<S>,
    pub area: Vec<S>,
    pub d_hydraulic: Vec<S>,
    pub wall_heat_flux: Vec<S>,
    pub wetted_perimeter: Vec<S>,
    pub p_in: S,
    pub t_in: S,
    pub mdot: S,
    pub y_in: Vec<S>,
    pub wall_roughness: S,
    pub viscosity: S,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompressibleChannelTransport {
    pub eos: EquationOfState,
    pub n_stations: usize,
    pub friction_regime: FrictionRegime,
    pub switch_certificate: f64,
    pub balance_tolerance: f64,
    pub n_segment_refine: usize,
}

fn check_positive(name: &str, x: f64) -> PhysicsResult<()> {
    if !x.is_finite() || x <= 0.0 {
        let mut details = Map::new();
        details.insert("value".into(), json!(x));
        return Err(validation(
            format!("{name} must be finite and > {}", fmt_g(0.0, 6)),
            &format!("channel_transport.{name}"),
            details,
        ));
    }
    Ok(())
}

impl CompressibleChannelTransport {

    pub fn new(
        eos: EquationOfState,
        n_stations: usize,
        friction_regime: &str,
        switch_certificate: f64,
        balance_tolerance: f64,
        n_segment_refine: usize,
    ) -> PhysicsResult<Self> {
        if !balance_tolerance.is_finite() || balance_tolerance <= 0.0 {
            return Err(PhysicsError::contract("Channel balance tolerance must be positive and finite"));
        }
        if n_stations < 2 {
            return Err(PhysicsError::contract(format!(
                "n_stations must be an integer >= 2; got {n_stations}"
            )));
        }
        let regime = FrictionRegime::parse(friction_regime)?;
        if !switch_certificate.is_finite() || switch_certificate <= 0.0 {
            return Err(PhysicsError::contract("switch_certificate must be a positive finite float"));
        }
        if n_segment_refine < 1 {
            return Err(PhysicsError::contract("n_segment_refine must be a positive integer"));
        }
        Ok(Self {
            eos,
            n_stations,
            friction_regime: regime,
            switch_certificate,
            balance_tolerance,
            n_segment_refine,
        })
    }


    pub fn with_defaults(eos: EquationOfState) -> PhysicsResult<Self> {
        Self::new(eos, 32, "colebrook", 1.0e-8, 1.0e-6, DEFAULT_SEGMENT_REFINE_STEPS)
    }

    fn validate_inputs<S: Scalar>(&self, i: &ChannelInputs<S>) -> PhysicsResult<()> {
        check_positive("p_in", i.p_in.value())?;
        check_positive("T_in", i.t_in.value())?;
        check_positive("mdot", i.mdot.value())?;
        check_positive("viscosity", i.viscosity.value())?;
        let wr = i.wall_roughness.value();
        if !wr.is_finite() || wr < 0.0 {
            let mut details = Map::new();
            details.insert("value".into(), json!(wr));
            return Err(validation(
                "wall_roughness must be finite and >= 0".into(),
                "channel_transport.wall_roughness",
                details,
            ));
        }
        let n = i.s.len();
        let lens = [
            ("s", i.s.len()),
            ("area", i.area.len()),
            ("D_hydraulic", i.d_hydraulic.len()),
            ("wall_heat_flux", i.wall_heat_flux.len()),
            ("wetted_perimeter", i.wetted_perimeter.len()),
        ];
        if lens.iter().any(|(_, l)| *l != n) {
            let mut shapes = Map::new();
            for (k, l) in lens {
                shapes.insert(k.into(), json!([l]));
            }
            let mut details = Map::new();
            details.insert("shapes".into(), Value::Object(shapes));
            return Err(validation(
                "s / area / D_hydraulic / wall_heat_flux / wetted_perimeter must be 1-D with the same length >= 2".into(),
                "channel_transport.arrays",
                details,
            ));
        }
        if n < 2 {
            let mut details = Map::new();
            details.insert("n_stations".into(), json!(n));
            return Err(validation(
                "channel must have at least 2 stations (inlet + outlet)".into(),
                "channel_transport.n_stations",
                details,
            ));
        }
        if i.y_in.len() != self.eos.species.len() {
            return Err(PhysicsError::validation(
                format!(
                    "y_in must have shape ({},) matching eos.species; got ({},)",
                    self.eos.species.len(),
                    i.y_in.len()
                ),
                "channel_transport.y_in",
            ));
        }
        let v = |x: &S| x.value();
        let finite_pos = |xs: &[S]| xs.iter().all(|x| x.value().is_finite() && x.value() > 0.0);
        let total: f64 = i.y_in.iter().map(v).sum();
        let valid = i.s.iter().all(|x| x.value().is_finite())
            && i.s.windows(2).all(|w| w[1].value() - w[0].value() > 0.0)
            && i.wall_heat_flux.iter().all(|x| x.value().is_finite())
            && i.y_in.iter().all(|x| x.value().is_finite() && x.value() >= 0.0)
            && (total - 1.0).abs() <= 1e-10
            && wr.is_finite()
            && wr >= 0.0
            && finite_pos(&i.area)
            && finite_pos(&i.d_hydraulic)
            && finite_pos(&i.wetted_perimeter)
            && finite_pos(&[i.p_in, i.t_in, i.mdot, i.viscosity]);
        if !valid {
            return Err(PhysicsError::validation(
                "Invalid channel geometry, state or composition",
                "channel_transport",
            ));
        }
        Ok(())
    }


    #[allow(clippy::too_many_lines)]
    pub fn evaluate<S: Scalar>(&self, i: &ChannelInputs<S>) -> PhysicsResult<ChannelResult<S>> {
        self.validate_inputs(i)?;
        let eos = &self.eos;
        let y = &i.y_in;
        let n = i.area.len();
        let regime = self.friction_regime;
        let (mdot, visc, rough) = (i.mdot, i.viscosity, i.wall_roughness);
        let inlet = local_state(eos, y, i.p_in, i.t_in, i.area[0], mdot)?;
        let h_t_in = inlet.h_t;
        let ds: Vec<S> = i.s.windows(2).map(|w| w[1] - w[0]).collect();

        let (mut p_i, mut t_i, mut h_t_i) = (i.p_in, i.t_in, h_t_in);
        let mut st: Vec<Station<S>> = Vec::with_capacity(n - 1);
        for (k, &d) in ds.iter().enumerate() {
            let (a_i, a_ip1) = (i.area[k], i.area[k + 1]);
            let (dh_i, dh_ip1) = (i.d_hydraulic[k], i.d_hydraulic[k + 1]);
            let (q_i, q_ip1) = (i.wall_heat_flux[k], i.wall_heat_flux[k + 1]);
            let (pw_i, pw_ip1) = (i.wetted_perimeter[k], i.wetted_perimeter[k + 1]);
            let loc = traced_state(eos, y, p_i, t_i, a_i, mdot);
            let re_i = loc.rho * loc.u * dh_i / visc;
            let f_i = friction_factor(regime, re_i, dh_i, rough);
            let tau_i = f_i * 0.125 * loc.rho * loc.u * loc.u;
            let pw_avg = (pw_i + pw_ip1) * 0.5;
            let a_avg = (a_i + a_ip1) * 0.5;
            let dh_avg = (dh_i + dh_ip1) * 0.5;
            let segment_heat = (q_i * pw_i + q_ip1 * pw_ip1) * 0.5 * d;
            let h_t_ip1 = h_t_i + segment_heat / mdot;
            let (mut p_next, mut t_next) = (p_i, t_i);
            for _ in 0..self.n_segment_refine {
                let nl = traced_state(eos, y, p_next, t_next, a_ip1, mdot);
                let rho_avg = (loc.rho + nl.rho) * 0.5;
                let u_avg = (loc.u + nl.u) * 0.5;
                let re_avg = rho_avg * u_avg.abs() * dh_avg / visc;
                let f_avg = friction_factor(regime, re_avg, dh_avg, rough);
                let tau_avg = f_avg * 0.125 * rho_avg * u_avg * u_avg;
                let p_new = p_i + (-mdot * (nl.u - loc.u) - tau_avg * pw_avg * d) / a_avg;
                let t_new = t_next + (h_t_ip1 - nl.u * 0.5 * nl.u - nl.h) / nl.cp;
                p_next = p_new;
                t_next = t_new;
            }
            st.push((p_i, t_i, loc.rho, loc.u, loc.mach, loc.a, h_t_i, re_i, f_i, tau_i, loc.gamma, loc.cp));
            p_i = p_next;
            t_i = t_next;
            h_t_i = h_t_ip1;
        }
        let (p_out, t_out) = (p_i, t_i);
        let out = local_state(eos, y, p_out, t_out, i.area[n - 1], mdot)?;
        let re_out = out.rho * out.u * i.d_hydraulic[n - 1] / visc;
        let f_out = friction_factor(regime, re_out, i.d_hydraulic[n - 1], rough);
        let tau_out = f_out * 0.125 * out.rho * out.u * out.u;

        let col = |f: &dyn Fn(&Station<S>) -> S, last: S| {
            let mut v: Vec<S> = st.iter().map(f).collect();
            v.push(last);
            v
        };
        let pressure = col(&|r| r.0, p_out);
        let temperature = col(&|r| r.1, t_out);
        let density = col(&|r| r.2, out.rho);
        let velocity = col(&|r| r.3, out.u);
        let mach = col(&|r| r.4, out.mach);
        let sound_speed = col(&|r| r.5, out.a);
        let reynolds = col(&|r| r.7, re_out);
        let friction = col(&|r| r.8, f_out);
        let wall_shear = col(&|r| r.9, tau_out);
        let gamma = col(&|r| r.10, out.gamma);
        let enthalpy_states: Vec<S> = pressure
            .iter()
            .zip(&temperature)
            .map(|(p, t)| eos.evaluate(y, *p, *t).map_or_else(|_| S::from_f64(f64::NAN), |pr| pr.enthalpy))
            .collect();
        let total_enthalpy: Vec<S> =
            enthalpy_states.iter().zip(&velocity).map(|(h, u)| *h + *u * *u * 0.5).collect();
        let totals: Vec<(S, S)> =
            (0..n).map(|k| total_state(pressure[k], temperature[k], gamma[k], mach[k])).collect();
        let p_t: Vec<S> = totals.iter().map(|t| t.0).collect();
        let t_t: Vec<S> = totals.iter().map(|t| t.1).collect();

        let integrand: Vec<S> =
            i.wall_heat_flux.iter().zip(&i.wetted_perimeter).map(|(q, p)| *q * *p).collect();
        let segment_heat: Vec<S> =
            (0..n - 1).map(|k| (integrand[k] + integrand[k + 1]) * 0.5 * ds[k]).collect();
        let q_total = sum(&segment_heat);
        let segment_energy: Vec<S> = (0..n - 1)
            .map(|k| mdot * (total_enthalpy[k + 1] - total_enthalpy[k]) - segment_heat[k])
            .collect();
        let avg = |v: &[S], k: usize| (v[k] + v[k + 1]) * 0.5;
        let mut energy_terms = Vec::with_capacity(n - 1);
        let mut force_terms = Vec::with_capacity(n - 1);
        let mut segment_momentum = Vec::with_capacity(n - 1);
        for k in 0..n - 1 {
            let rho_average = avg(&density, k);
            let velocity_average = avg(&velocity, k);
            let diameter_average = avg(&i.d_hydraulic, k);
            let perimeter_average = avg(&i.wetted_perimeter, k);
            let area_average = avg(&i.area, k);
            let re_average = rho_average * velocity_average.abs() * diameter_average / visc;
            let friction_average = friction_factor(regime, re_average, diameter_average, rough);
            let wall_force = friction_average
                * 0.125
                * rho_average
                * (velocity_average * velocity_average)
                * perimeter_average
                * ds[k];
            let momentum = area_average * (pressure[k + 1] - pressure[k])
                + mdot * (velocity[k + 1] - velocity[k])
                + wall_force;
            let energy_scale = (mdot * total_enthalpy[k]).abs().maximum(segment_heat[k].abs()).max_f64(1.0);
            let force_scale =
                (area_average * 0.5 * (pressure[k].abs() + pressure[k + 1].abs())).max_f64(1.0e-6);
            energy_terms.push(segment_energy[k].abs() / energy_scale);
            force_terms.push(momentum.abs() / force_scale);
            segment_momentum.push(momentum);
        }
        let balance = reduce_max(&energy_terms).maximum(reduce_max(&force_terms));
        let energy_residual = mdot * (out.h_t - h_t_in) - q_total;
        let result = ChannelResult {
            outlet_total_pressure_pa: p_t[n - 1],
            outlet_total_temperature_k: t_t[n - 1],
            pressure_pa: pressure,
            temperature_k: temperature,
            density_kg_m3: density,
            velocity_m_s: velocity,
            mach,
            sound_speed_m_s: sound_speed,
            total_pressure_pa: p_t,
            total_temperature_k: t_t,
            total_enthalpy_j_kg: total_enthalpy,
            reynolds_number: reynolds,
            friction_factor: friction,
            wall_shear_stress_pa: wall_shear,
            outlet_pressure_pa: p_out,
            outlet_temperature_k: t_out,
            outlet_mass_flow_kg_s: mdot,
            outlet_composition: y.clone(),
            pressure_drop_pa: i.p_in - p_out,
            total_heat_pickup_w: q_total,
            energy_balance_residual: energy_residual,
            segment_energy_residual_w: segment_energy,
            segment_momentum_residual_n: segment_momentum,
            maximum_scaled_balance_residual: balance,
        };
        if !result.all_values().all(f64::is_finite) {
            return Err(PhysicsError::validation(
                "Channel evaluation produced nonfinite values",
                "channel_transport",
            ));
        }
        Ok(result)
    }


    pub fn certify_sensitivity(&self, i: &ChannelInputs<f64>) -> PhysicsResult<Map<String, Value>> {
        let out = self.evaluate(i)?;
        let residual = out.maximum_scaled_balance_residual;
        if !residual.is_finite() || residual > self.balance_tolerance {
            return Err(PhysicsError::contract(
                "Channel segment balance has not converged to the declared tolerance",
            ));
        }
        for (p, t) in out.pressure_pa.iter().zip(&out.temperature_k) {
            self.eos.certify_sensitivity(&i.y_in, *p, *t)?;
        }
        let min_distance = out.mach.iter().fold(f64::INFINITY, |m, x| m.min((1.0 - x).abs()));
        let m_max = out.mach.iter().fold(f64::NEG_INFINITY, |m, x| m.max(*x));
        if m_max >= 1.0 - self.switch_certificate || !out.mach.iter().copied().all(f64::is_finite) {
            return Err(PhysicsError::contract(format!(
                "compressible channel choked or non-finite: max(M) = {}, min |1 - M| = {} <= switch_certificate = {}; the forward-marching sensitivity is not admissible in a subsonic channel with heating. Review the model and operating point; this check does not alter prescribed service conditions.",
                fmt_e(m_max, 6),
                fmt_e(min_distance, 3),
                fmt_e(self.switch_certificate, 0)
            )));
        }
        let mut m = Map::new();
        m.insert("maximum_scaled_balance_residual".into(), json!(residual));
        m.insert("balance_tolerance".into(), json!(self.balance_tolerance));
        m.insert("max_mach".into(), json!(m_max));
        m.insert("min_switch_distance".into(), json!(min_distance));
        m.insert("switch_certificate".into(), json!(self.switch_certificate));
        m.insert("sensitivity_admissible".into(), json!(true));
        Ok(m)
    }
}
