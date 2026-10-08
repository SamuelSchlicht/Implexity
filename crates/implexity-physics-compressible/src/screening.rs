// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use implexity_ad::Scalar;
use implexity_physics_thermofluid::real_fluid_screening::{
    RealFluidSpec, density as real_fluid_density,
};
use crate::feed_system_screening::{FeedSpec, metrics as feed_metrics};
use crate::injection_screening::{InjectionSpec, metrics as injection_metrics};
use crate::radiation_screening::{RadiationSpec, coolant_section, radiative_flux};
use crate::thermoacoustic_screening::{ThermoacousticSpec, metrics as thermoacoustic_metrics};
pub use crate::array::Field;

pub fn reduce_min<S: Scalar>(xs: &[S]) -> S {
    let neg: Vec<S> = xs.iter().map(|x| -*x).collect();
    -reduce_max(&neg)
}

pub fn sum<S: Scalar>(xs: &[S]) -> S {
    xs.iter().copied().sum()
}

pub fn mean<S: Scalar>(xs: &[S]) -> S {
    #[allow(clippy::cast_precision_loss)]
    let n = xs.len() as f64;
    sum(xs) / n
}

pub fn smooth_min<S: Scalar>(a: S, b: S, beta: f64) -> S {
    let m = a.minimum(b);
    m - (((a - m) * -beta).exp() + ((b - m) * -beta).exp()).ln() / beta
}

pub fn softplus<S: Scalar>(x: S, beta: f64) -> S {
    (x * beta).softplus() / beta
}

pub fn softmax_value<S: Scalar>(values: &[S], beta: f64) -> S {
    let m = reduce_max(values);
    let s: S = values.iter().map(|v| ((*v - m) * beta).exp()).sum();
    m + s.ln() / beta
}

pub fn softmin_value<S: Scalar>(values: &[S], beta: f64, reference_count: Option<f64>) -> S {
    let neg: Vec<S> = values.iter().map(|v| -*v).collect();
    match reference_count {
        None => -softmax_value(&neg, beta),
        Some(reference) => {
            #[allow(clippy::cast_precision_loss)]
            let n = (values.len() as f64).max(1.0);
            let r = reference.max(1.0);
            let m = reduce_max(&neg);
            let s: S = neg.iter().map(|v| ((*v - m) * beta).exp()).sum();
            let lme = m + (s / n).ln() / beta;
            -(lme + r.ln() / beta)
        }
    }
}

pub fn supersonic_mach_from_area_ratio<S: Scalar>(area_ratio: S, gamma: f64) -> S {
    let ar = area_ratio.max_f64(1.0 + 1e-8);
    let mut m = ((ar - 1.0).max_f64(0.0) + 1e-8).sqrt() * 0.8 + 1.0;
    let expo = (gamma + 1.0) / (2.0 * (gamma - 1.0));
    for _ in 0..14 {
        let q = S::one() + m * m * (0.5 * (gamma - 1.0));
        let base = q * (2.0 / (gamma + 1.0));
        let f = m.recip() * base.powf(expo) - ar;
        let dlog = -m.recip() + m * ((gamma - 1.0) * expo) / q;
        let df = (f + ar) * dlog;
        m = (m - f / (df + 1e-12)).max_f64(1.000_001);
    }
    m
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub nx: usize,
    pub ny: usize,
    pub nz: usize,
}

impl Grid {
    #[must_use]
    pub fn len(&self) -> usize {
        self.nx * self.ny * self.nz
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn at(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.ny + j) * self.nz + k
    }

    pub fn section_mean<S: Scalar>(&self, f: &[S]) -> Vec<S> {
        let per = self.ny * self.nz;
        (0..self.nx).map(|i| mean(&f[i * per..(i + 1) * per])).collect()
    }
}

pub fn cross_section_hydraulic_radius<S: Scalar>(
    g: Grid,
    field: &[S],
    dy: f64,
    dz: f64,
    area_floor: f64,
) -> Vec<S> {
    #[allow(clippy::cast_precision_loss)]
    let full = (g.ny as f64 * dy) * (g.nz as f64 * dz);
    let smooth = 1.0e-8;
    let means = g.section_mean(field);
    (0..g.nx)
        .map(|i| {
            let mut py: Vec<S> = Vec::new();
            for j in 0..g.ny.saturating_sub(1) {
                for k in 0..g.nz {
                    let d = field[g.at(i, j + 1, k)] - field[g.at(i, j, k)];
                    py.push((d * d + smooth * smooth).sqrt() - smooth);
                }
            }
            let mut pz: Vec<S> = Vec::new();
            for j in 0..g.ny {
                for k in 0..g.nz.saturating_sub(1) {
                    let d = field[g.at(i, j, k + 1)] - field[g.at(i, j, k)];
                    pz.push((d * d + smooth * smooth).sqrt() - smooth);
                }
            }
            let perimeter = sum(&py) * dz + sum(&pz) * dy + 1.0e-9;
            (means[i] * full).max_f64(area_floor) / perimeter
        })
        .collect()
}

pub fn smooth_total_variation_area<S: Scalar>(g: Grid, field: &[S], dx: f64, dy: f64, dz: f64) -> S {
    let smoothing = 1.0e-8;
    let tv = |d: S| (d * d + smoothing * smoothing).sqrt() - smoothing;
    let mut ax = Vec::new();
    let mut ay = Vec::new();
    let mut az = Vec::new();
    for i in 0..g.nx {
        for j in 0..g.ny {
            for k in 0..g.nz {
                let c = field[g.at(i, j, k)];
                if i + 1 < g.nx {
                    ax.push((i, tv(field[g.at(i + 1, j, k)] - c)));
                }
                if j + 1 < g.ny {
                    ay.push((i, tv(field[g.at(i, j + 1, k)] - c)));
                }
                if k + 1 < g.nz {
                    az.push((i, tv(field[g.at(i, j, k + 1)] - c)));
                }
            }
        }
    }
    let s = |v: Vec<(usize, S)>| -> S { v.into_iter().map(|(_, x)| x).sum() };
    s(ax) * (dy * dz) + s(ay) * (dx * dz) + s(az) * (dx * dy)
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamModel {
    pub name: String,
    pub role: String,
    pub mdot: f64,
    pub mass_fraction: f64,
    pub density: f64,
    pub upstream_pressure: f64,
    pub feed: Option<FeedSpec>,
    pub injection: Option<InjectionSpec>,
}

#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub struct Constants {
    pub penal: f64,
    pub lx: f64,
    pub ly: f64,
    pub lz: f64,
    pub domain_volume: f64,
    pub min_cell: f64,
    pub area_floor: f64,
    pub area_beta: f64,
    pub axial_reference: f64,
    pub pressure_beta: f64,
    pub flow_beta: f64,
    pub pressure_loss_coeff: f64,
    pub mdot_supply: f64,
    pub fuel_fraction: f64,
    pub monopropellant: f64,
    pub inlet_temperature: f64,
    pub reaction_temperature: f64,
    pub cp_mix: f64,
    pub supply_pressure: f64,
    pub supply_beta: f64,
    pub stream_models: Vec<StreamModel>,
    pub radiation: Option<RadiationSpec>,
    pub thermoacoustics: Option<ThermoacousticSpec>,
    pub ambient_pressure: f64,
    pub declared_mixing_efficiency: f64,
    pub rho_reference: f64,
    pub reaction_rate_scale: f64,
    pub activation_energy: f64,
    pub gas_constant: f64,
    pub heat_release: f64,
    pub gamma: f64,
    pub specific_gas_constant: f64,
    pub discharge_coefficient: f64,
    pub exit_pressure_ratio: f64,
    pub expansion_model: String,
    pub g0: f64,
    pub min_wall: f64,
    pub h_gas: f64,
    pub wall_k: f64,
    pub h_coolant: f64,
    pub coolant_heat_transfer_model: String,
    pub coolant_hydraulic_model: String,
    pub coolant_minor_loss_coefficient: f64,
    pub coolant_density: f64,
    pub coolant_cp: f64,
    pub coolant_viscosity: f64,
    pub coolant_conductivity: f64,
    pub coolant_property_min_temperature: f64,
    pub coolant_property_max_temperature: f64,
    pub coolant_real_fluid: Option<RealFluidSpec>,
    pub coolant_pressure: f64,
    pub coolant_temperature: f64,
    pub hotspot_factor: f64,
    pub coolant_dp_coeff: f64,
    pub coolant_mdot: f64,
    pub coolant_open_fraction: f64,
    pub alpha_thermal: f64,
    pub youngs_modulus: f64,
    pub material_density: f64,
    pub allowable_temperature: f64,
    pub fatigue_ductility: f64,
    pub fatigue_exponent: f64,
    pub creep_reference_stress: f64,
    pub creep_stress_exponent: f64,
    pub creep_activation_energy: f64,
    pub oxidation_rate: f64,
    pub oxidation_scale: f64,
    pub cycles: f64,
    pub hot_time: f64,
}

fn prepare_phase<S: Scalar>(n: usize, phase_fields: &BTreeMap<String, Vec<S>>) -> Phases<S> {
    let mut authored = BTreeMap::new();
    for (role, coordinate) in PHASE_COORDINATES {
        authored.insert(role, phase_fields.contains_key(coordinate));
    }
    let zeros = vec![S::zero(); n];
    if !authored.values().any(|a| *a) {
        return Phases {
            propellant: zeros.clone(),
            fuel: zeros.clone(),
            oxidizer: zeros.clone(),
            coolant: zeros,
            authored,
        };
    }
    let active: Vec<(&str, &Vec<S>)> =
        PHASE_COORDINATES.iter().filter_map(|(role, c)| phase_fields.get(*c).map(|q| (*role, q))).collect();
    let mut m = zeros.clone();
    for (_, q) in &active {
        for (mi, qi) in m.iter_mut().zip(q.iter()) {
            *mi = mi.maximum(*qi);
        }
    }
    let mut denom: Vec<S> = m.iter().map(|mi| (-*mi).exp()).collect();
    for (_, q) in &active {
        for ((d, qi), mi) in denom.iter_mut().zip(q.iter()).zip(&m) {
            *d += (*qi - *mi).exp();
        }
    }
    let frac = |role: &str| -> Vec<S> {
        match active.iter().find(|(r, _)| *r == role) {
            None => zeros.clone(),
            Some((_, q)) => {
                q.iter().zip(&m).zip(&denom).map(|((qi, mi), d)| (*qi - *mi).exp() / *d).collect()
            }
        }
    };
    Phases {
        propellant: frac("propellant"),
        fuel: frac("fuel"),
        oxidizer: frac("oxidizer"),
        coolant: frac("coolant"),
        authored,
    }
}

struct Coolant<S> {
    h: S,
    dp: S,
    dh: S,
    re: S,
}

fn coolant_internal_flow<S: Scalar>(
    g: Grid,
    coolant_void: &[S],
    total_void_sum: S,
    c: &Constants,
    cross_area: f64,
) -> Coolant<S> {
    let eps = 1.0e-12;
    let sections: Vec<S> = g.section_mean(coolant_void).into_iter().map(|m| m * cross_area).collect();
    let mean_area = mean(&sections).max_f64(c.area_floor);
    let hd = ((mean_area * 4.0) / std::f64::consts::PI).sqrt().max_f64(0.1 * c.min_cell);
    let mdot = c.coolant_mdot.max(0.0);
    let (rho, mu, k, cp) = (c.coolant_density, c.coolant_viscosity, c.coolant_conductivity, c.coolant_cp);
    let velocity = S::from_f64(mdot) / (mean_area * rho + eps);
    let reynolds = velocity * rho * hd / (mu + eps);
    let prandtl = cp * mu / (k + eps);
    let nu_turb = reynolds.max_f64(1.0).powf(0.8) * (0.023 * prandtl.max(0.1).powf(0.4));
    let transition = ((reynolds - 3000.0) / 500.0).sigmoid();
    let nusselt = (S::one() - transition) * 3.66 + transition * nu_turb;
    let h_corr = nusselt * k / (hd + eps) * (S::one() - (reynolds / -100.0).exp());
    let h = if c.coolant_heat_transfer_model == "correlated_internal_flow" {
        h_corr.max_f64(1.0e-6)
    } else {
        S::from_f64(c.h_coolant)
    };
    let dp = if c.coolant_hydraulic_model == "darcy_weisbach" {
        let re1 = reynolds.max_f64(1.0);
        let f_lam = re1.recip() * 64.0;
        let f_turb = re1.powf(0.25).recip() * 0.3164;
        let friction = (S::one() - transition) * f_lam + transition * f_turb;
        let dyn_p = velocity * velocity * (0.5 * rho);
        friction * c.lx / (hd + eps) * dyn_p + dyn_p * c.coolant_minor_loss_coefficient
    } else {
        let open = sum(coolant_void) / (total_void_sum + 1.0e-6);
        (open + 1.0e-3).powf(2.0).recip() * (c.coolant_dp_coeff * mdot * mdot)
    };
    Coolant { h, dp, dh: hd, re: reynolds }
}

struct AxialCoolant<S> {
    dh: Vec<S>,
    re: Vec<S>,
    h: Vec<S>,
    dp: Vec<S>,
}

fn coolant_axial<S: Scalar>(
    g: Grid,
    coolant_void: &[S],
    c: &Constants,
    cross_area: f64,
    dx: f64,
    temps: Option<&[S]>,
) -> AxialCoolant<S> {
    let eps = 1.0e-12;
    let areas: Vec<S> =
        g.section_mean(coolant_void).into_iter().map(|m| (m * cross_area).max_f64(c.area_floor)).collect();
    let dh: Vec<S> =
        areas.iter().map(|a| ((*a * 4.0) / std::f64::consts::PI).sqrt().max_f64(0.1 * c.min_cell)).collect();
    let mdot = c.coolant_mdot.max(0.0);
    let (mu, k, cp) = (c.coolant_viscosity, c.coolant_conductivity, c.coolant_cp);
    let rho: Vec<S> = match &c.coolant_real_fluid {
        Some(spec) => (0..areas.len())
            .map(|i| {
                let t = temps.map_or(S::from_f64(c.coolant_temperature), |t| t[i]);
                real_fluid_density(S::from_f64(c.coolant_pressure), t, spec)
            })
            .collect(),
        None => vec![S::from_f64(c.coolant_density); areas.len()],
    };
    let pr = cp * mu / (k + eps);
    #[allow(clippy::cast_precision_loss)]
    let minor = c.coolant_minor_loss_coefficient / (g.nx.max(1) as f64);
    let mut out = AxialCoolant { dh: Vec::new(), re: Vec::new(), h: Vec::new(), dp: Vec::new() };
    for i in 0..areas.len() {
        let velocity = S::from_f64(mdot) / (rho[i] * areas[i] + eps);
        let re = rho[i] * velocity * dh[i] / (mu + eps);
        let nu_turb = re.max_f64(1.0).powf(0.8) * (0.023 * pr.max(0.1).powf(0.4));
        let blend = ((re - 3000.0) / 500.0).sigmoid();
        let nu = (S::one() - blend) * 3.66 + blend * nu_turb;
        let h = nu * k / (dh[i] + eps) * (S::one() - (re / -100.0).exp());
        let re1 = re.max_f64(1.0);
        let friction = (S::one() - blend) * (re1.recip() * 64.0) + blend * (re1.powf(0.25).recip() * 0.3164);
        let dyn_p = rho[i] * velocity * velocity * 0.5;
        let dp = friction * dx / (dh[i] + eps) * dyn_p + dyn_p * minor;
        out.dh.push(dh[i]);
        out.re.push(re);
        out.h.push(h);
        out.dp.push(dp);
    }
    out
}

#[allow(clippy::too_many_lines)]
pub fn response_vector<S: Scalar>(
    g: Grid,
    topology: &[S],
    phase_fields: &BTreeMap<String, Vec<S>>,
    c: &Constants,
) -> Vec<S> {
    let n = g.len();
    let s: Vec<S> = topology.iter().map(|x| x.clip(0.0, 1.0)).collect();
    let eps = 1.0e-6;
    let void: Vec<S> = s.iter().map(|x| (S::one() - *x).powf(c.penal) * (1.0 - eps) + eps).collect();
    let ph = prepare_phase(n, phase_fields);
    let gas_void: Vec<S> = void.iter().zip(&ph.coolant).map(|(v, q)| *v * (S::one() - *q)).collect();
    let coolant_void: Vec<S> = void.iter().zip(&ph.coolant).map(|(v, q)| *v * *q).collect();
    #[allow(clippy::cast_precision_loss)]
    let (nxf, nyf, nzf) = (g.nx as f64, g.ny as f64, g.nz as f64);
    let (dx, dy, dz) = (c.lx / nxf, c.ly / nyf, c.lz / nzf);
    let cross_area = c.ly * c.lz;
    let cell_volume = c.lx * c.ly * c.lz / (nxf * nyf * nzf);
    let gas_sections = g.section_mean(&gas_void);
    let areas: Vec<S> = gas_sections.iter().map(|m| *m * cross_area).collect();
    let throat_area = softmin_value(&areas, c.area_beta, Some(c.axial_reference));
    let tail = (g.nx / 8).max(1);
    let exit_area = mean(&areas[g.nx - tail..]);
    let volume = sum(&gas_void) * cell_volume;
    let solid_fraction = mean(&s);
    let diffuse: Vec<S> = gas_void.iter().map(|v| *v * 4.0 * (S::one() - *v)).collect();
    let diffuse_surface = sum(&diffuse) * cell_volume / dx.max(c.min_cell);
    let hydraulic_radius = volume * 2.0 / (diffuse_surface + eps);
    let hr_sections = cross_section_hydraulic_radius(g, &gas_void, dy, dz, c.area_floor);
    let inv: Vec<S> = areas.iter().map(|a| (*a + c.area_floor).powf(2.0).recip()).collect();
    let resistance = sum(&inv) * dx;
    let pressure_loss = resistance * (c.pressure_loss_coeff * (c.mdot_supply * c.mdot_supply));
    let total_inlet_area = gas_sections[0] * cross_area;

    let mut pump_powers: Vec<S> = Vec::new();
    let mut reactant_pressures: Vec<S> = Vec::new();
    let mut reactant_margins: Vec<S> = Vec::new();
    let mut injection_rows: Vec<(f64, S, S, InjectionSpec)> = Vec::new();
    for stream in &c.stream_models {
        let role = stream.role.as_str();
        let owner: Option<&Vec<S>> = match role {
            "fuel" if ph.authored.get("fuel") == Some(&true) => Some(&ph.fuel),
            "oxidizer" if ph.authored.get("oxidizer") == Some(&true) => Some(&ph.oxidizer),
            "coolant" if ph.authored.get("coolant") == Some(&true) => Some(&ph.coolant),
            _ => None,
        };
        let inlet_area = match owner {
            Some(q) => {
                let per = g.ny * g.nz;
                let owned: Vec<S> = if role == "coolant" {
                    coolant_void[..per].to_vec()
                } else {
                    gas_void[..per].iter().zip(&q[..per]).map(|(v, qi)| *v * *qi).collect()
                };
                mean(&owned) * cross_area
            }
            None => total_inlet_area * stream.mass_fraction,
        }
        .max_f64(c.area_floor);
        let rho = stream.density;
        let velocity;
        if let Some(feed) = &stream.feed {
            let fm = feed_metrics(
                S::from_f64(stream.mdot),
                S::from_f64(rho),
                inlet_area,
                S::from_f64(stream.upstream_pressure),
                feed,
            );
            pump_powers.push(fm[3]);
            velocity = fm[5];
            if role != "coolant" {
                reactant_pressures.push(fm[2]);
                reactant_margins.push(fm[4]);
            }
        } else {
            pump_powers.push(S::zero());
            velocity = S::from_f64(stream.mdot) / (inlet_area * rho + eps);
            if role != "coolant" {
                reactant_pressures.push(S::from_f64(stream.upstream_pressure));
                reactant_margins.push(S::zero());
            }
        }
        if let Some(inj) = &stream.injection
            && ["monopropellant", "fuel", "oxidizer"].contains(&role)
        {
            let equiv_d = (inlet_area * 4.0 / std::f64::consts::PI).sqrt();
            injection_rows.push((rho, velocity, equiv_d, *inj));
        }
    }
    let available_supply = if reactant_pressures.is_empty() {
        S::from_f64(c.supply_pressure)
    } else {
        let v: Vec<S> = reactant_pressures.iter().zip(&reactant_margins).map(|(p, r)| *p - *r).collect();
        softmin_value(&v, c.supply_beta, None)
    };
    let feed_pump_power = if pump_powers.is_empty() { S::zero() } else { sum(&pump_powers) };
    let floor_p = 1.02 * c.ambient_pressure;
    let chamber_pressure = softplus(available_supply - pressure_loss - floor_p, c.pressure_beta) + floor_p;
    let feed_pressure_margin = if reactant_margins.is_empty() {
        available_supply - chamber_pressure
    } else {
        let v: Vec<S> = reactant_pressures
            .iter()
            .zip(&reactant_margins)
            .map(|(p, r)| *p - *r - chamber_pressure)
            .collect();
        reduce_min(&v)
    };
    let gas_sum = sum(&gas_void);
    let mixture_uniformity = if c.monopropellant > 0.5 {
        if ph.authored.get("propellant") == Some(&true) {
            let w: Vec<S> = gas_void.iter().zip(&ph.propellant).map(|(v, q)| *v * *q).collect();
            sum(&w) / (gas_sum + eps)
        } else {
            S::from_f64(c.declared_mixing_efficiency)
        }
    } else if ph.authored.get("fuel") == Some(&true) && ph.authored.get("oxidizer") == Some(&true) {
        let w: Vec<S> = gas_void
            .iter()
            .zip(ph.fuel.iter().zip(&ph.oxidizer))
            .map(|(v, (f, o))| *v * (*f * 4.0 * *o))
            .collect();
        sum(&w) / (gas_sum + eps)
    } else {
        S::from_f64(c.declared_mixing_efficiency)
    };
    let gas_density_screen = chamber_pressure / (c.specific_gas_constant * c.inlet_temperature + eps);
    let residence = gas_density_screen * volume / (c.mdot_supply + eps);
    let mut vaporized = Vec::new();
    let mut webers = Vec::new();
    let mut ohnesorges = Vec::new();
    for (rho, velocity, equiv_d, inj) in &injection_rows {
        let im =
            injection_metrics(S::from_f64(*rho), gas_density_screen, *velocity, *equiv_d, residence, inj);
        webers.push(im[0]);
        ohnesorges.push(im[1]);
        vaporized.push(im[4]);
    }
    let injection_efficiency = if vaporized.is_empty() { S::one() } else { reduce_min(&vaporized) };
    let minimum_weber = if webers.is_empty() { S::zero() } else { reduce_min(&webers) };
    let maximum_ohnesorge = if ohnesorges.is_empty() { S::zero() } else { reduce_max(&ohnesorges) };
    let rate = c.reaction_rate_scale
        * (-c.activation_energy / (c.gas_constant * c.reaction_temperature + eps)).exp();
    let damkohler = (residence * rate).max_f64(0.0);
    let chemistry_efficiency = S::one() - (-damkohler).exp();
    let combustion_efficiency =
        (mixture_uniformity * injection_efficiency * chemistry_efficiency).clip(0.0, 1.0);
    let gas_temperature =
        combustion_efficiency * c.fuel_fraction * c.heat_release / c.cp_mix + c.inlet_temperature;
    let gamma = c.gamma;
    let rspec = c.specific_gas_constant;
    let choking = (S::from_f64(gamma) / (gas_temperature * rspec + eps)).sqrt()
        * (2.0 / (gamma + 1.0)).powf((gamma + 1.0) / (2.0 * (gamma - 1.0)));
    let mdot_choked = throat_area * c.discharge_coefficient * chamber_pressure * choking;
    let mass_flow = smooth_min(S::from_f64(c.mdot_supply), mdot_choked, c.flow_beta);
    let exit_pressure = if c.expansion_model == "quasi_1d_isentropic" {
        let area_ratio = (exit_area / (throat_area + eps)).max_f64(1.0 + 1e-8);
        let exit_mach = supersonic_mach_from_area_ratio(area_ratio, gamma);
        let pressure_ratio =
            (exit_mach * exit_mach * (0.5 * (gamma - 1.0)) + 1.0).powf(-gamma / (gamma - 1.0));
        chamber_pressure * pressure_ratio
    } else {
        (chamber_pressure * c.exit_pressure_ratio).max_f64(c.ambient_pressure)
    };
    let expansion_term = (S::one()
        - (exit_pressure.max_f64(1.0) / (chamber_pressure + eps)).powf((gamma - 1.0) / gamma))
    .max_f64(0.0);
    let exhaust_velocity =
        (gas_temperature * (2.0 * gamma / (gamma - 1.0) * rspec) * expansion_term + eps).sqrt();
    let thrust = mass_flow * exhaust_velocity + (exit_pressure - c.ambient_pressure) * exit_area;
    let isp = thrust / (mass_flow * c.g0 + eps);
    let (first_mode_hz, thermoacoustic_margin) = match &c.thermoacoustics {
        Some(spec) => {
            let sound_speed = (gas_temperature * (gamma * rspec)).max_f64(eps).sqrt();
            let acoustic =
                thermoacoustic_metrics(&gas_sections, dx, sound_speed, combustion_efficiency, spec);
            (acoustic[0], acoustic[1])
        }
        None => (S::zero(), S::zero()),
    };
    let wall_floor = 0.05 * c.min_cell;
    let wall_thickness = (solid_fraction * 2.0 * hydraulic_radius).max_f64(wall_floor);
    let sf_sections = g.section_mean(&s);
    let wt_sections: Vec<S> =
        sf_sections.iter().zip(&hr_sections).map(|(f, r)| (*f * 2.0 * *r).max_f64(wall_floor)).collect();
    let minimum_wall_thickness_proxy = softmin_value(&wt_sections, 100_000.0, Some(c.axial_reference));
    let axial = c.coolant_heat_transfer_model == "axial_correlated_internal_flow";
    let (mut h_coolant, mut coolant_dp, mut coolant_dh, mut min_coolant_dh, mut coolant_re);
    let mut axial_metrics = None;
    if axial {
        let m = coolant_axial(g, &coolant_void, c, cross_area, dx, None);
        h_coolant = mean(&m.h);
        coolant_dp = sum(&m.dp);
        coolant_dh = mean(&m.dh);
        min_coolant_dh = softmin_value(&m.dh, 100_000.0, Some(c.axial_reference));
        coolant_re = mean(&m.re);
        axial_metrics = Some(m);
    } else {
        let m = coolant_internal_flow(g, &coolant_void, sum(&void), c, cross_area);
        h_coolant = m.h;
        coolant_dp = m.dp;
        coolant_dh = m.dh;
        min_coolant_dh = m.dh;
        coolant_re = m.re;
    }
    let heat_transfer_area = smooth_total_variation_area(g, &gas_void, dx, dy, dz).max_f64(cross_area);
    let coolant_inlet_temperature = S::from_f64(c.coolant_temperature);
    let capacity = S::from_f64((c.coolant_mdot * c.coolant_cp).max(1.0e-9));
    let (h_gas, wall_k) = (S::from_f64(c.h_gas), S::from_f64(c.wall_k));
    let rad = c.radiation.as_ref();
    let coolant_outlet_temperature;
    let (max_wall_temperature, pressure_stress, thermal_reference_coolant, radiative_heat_flux);
    if let Some(mut m) = axial_metrics {
        let interface_w: Vec<S> = (0..gas_sections.len())
            .map(|i| {
                let prev = if i == 0 { gas_sections[0] } else { gas_sections[i - 1] };
                let d = gas_sections[i] - prev;
                (d * d + 1e-10).sqrt() + gas_sections[i] * (S::one() - gas_sections[i]) + 1e-6
            })
            .collect();
        let w_total = sum(&interface_w);
        let area_sections: Vec<S> = interface_w.iter().map(|w| heat_transfer_area * *w / w_total).collect();
        let scan = |h: &[S]| -> (S, Vec<S>, Vec<S>, Vec<S>) {
            let mut tin = coolant_inlet_temperature;
            let (mut tw, mut q, mut bulk) = (Vec::new(), Vec::new(), Vec::new());
            for i in 0..h.len() {
                let (tout, twi, qi) = coolant_section(
                    gas_temperature,
                    tin,
                    capacity,
                    area_sections[i],
                    h_gas,
                    wt_sections[i],
                    wall_k,
                    h[i],
                    rad,
                );
                tw.push(twi);
                q.push(qi);
                bulk.push((tin + tout) * 0.5);
                tin = tout;
            }
            (tin, tw, q, bulk)
        };
        let (mut outlet, mut wall_sections, mut q_sections, mut bulk_sections) = scan(&m.h);
        if c.coolant_real_fluid.is_some() {
            m = coolant_axial(g, &coolant_void, c, cross_area, dx, Some(&bulk_sections));
            h_coolant = mean(&m.h);
            coolant_dp = sum(&m.dp);
            coolant_dh = mean(&m.dh);
            min_coolant_dh = softmin_value(&m.dh, 100_000.0, Some(c.axial_reference));
            coolant_re = mean(&m.re);
            (outlet, wall_sections, q_sections, bulk_sections) = scan(&m.h);
        }
        coolant_outlet_temperature = outlet;
        let a_total = sum(&area_sections);
        radiative_heat_flux = match rad {
            Some(spec) => {
                let v: Vec<S> = area_sections
                    .iter()
                    .zip(&wall_sections)
                    .map(|(a, w)| *a * radiative_flux(gas_temperature, *w, spec))
                    .collect();
                sum(&v) / a_total
            }
            None => S::zero(),
        };
        let hotspot: Vec<S> = (0..wall_sections.len())
            .map(|i| wall_sections[i] + q_sections[i] * c.hotspot_factor * wt_sections[i] / c.wall_k)
            .collect();
        max_wall_temperature = softmax_value(&hotspot, 0.02);
        let local: Vec<S> =
            hr_sections.iter().zip(&wt_sections).map(|(r, w)| chamber_pressure * *r / (*w + eps)).collect();
        pressure_stress = softmax_value(&local, 1.0e-7);
        thermal_reference_coolant = mean(&bulk_sections);
    } else {
        let (outlet, wall_temperature, heat_flux) = coolant_section(
            gas_temperature,
            coolant_inlet_temperature,
            capacity,
            heat_transfer_area,
            h_gas,
            wall_thickness,
            wall_k,
            h_coolant,
            rad,
        );
        coolant_outlet_temperature = outlet;
        let coolant_bulk = (coolant_inlet_temperature + outlet) * 0.5;
        radiative_heat_flux =
            rad.map_or_else(S::zero, |spec| radiative_flux(gas_temperature, wall_temperature, spec));
        max_wall_temperature = wall_temperature + heat_flux * c.hotspot_factor * wall_thickness / c.wall_k;
        pressure_stress = chamber_pressure * hydraulic_radius / (wall_thickness + eps);
        thermal_reference_coolant = coolant_bulk;
    }
    let thermal_strain = (max_wall_temperature - thermal_reference_coolant).max_f64(0.0) * c.alpha_thermal;
    let strain_amplitude = thermal_strain + pressure_stress / c.youngs_modulus;
    let ratio = (strain_amplitude / c.fatigue_ductility).max_f64(1.0e-12);
    let fatigue_life = ratio.powf(1.0 / c.fatigue_exponent) * 0.5;
    let fatigue_damage = S::from_f64(c.cycles) / (fatigue_life + eps);
    let creep_rate = (pressure_stress / c.creep_reference_stress).max_f64(0.0).powf(c.creep_stress_exponent)
        * (S::from_f64(-c.creep_activation_energy) / (max_wall_temperature * c.gas_constant + eps)).exp();
    let creep_damage = creep_rate * c.hot_time;
    let oxidation_damage =
        ((max_wall_temperature - c.allowable_temperature).max_f64(0.0) / c.oxidation_scale).exp()
            * (c.oxidation_rate * c.hot_time);
    let cycle_damage = fatigue_damage + creep_damage + oxidation_damage;
    let solid_mass = solid_fraction * c.domain_volume * c.material_density;
    let grayness = if ph.authored.get("coolant") == Some(&true) {
        let v: Vec<S> = ph.coolant.iter().map(|q| *q * 4.0 * (S::one() - *q)).collect();
        mean(&v)
    } else {
        S::zero()
    };
    let validity = (coolant_outlet_temperature - c.coolant_property_min_temperature)
        .minimum(S::from_f64(c.coolant_property_max_temperature) - coolant_outlet_temperature);
    vec![
        thrust,
        isp,
        combustion_efficiency,
        mixture_uniformity,
        chamber_pressure,
        pressure_loss,
        mass_flow,
        max_wall_temperature,
        coolant_dp,
        cycle_damage,
        solid_mass,
        solid_fraction,
        throat_area,
        exit_area,
        feed_pressure_margin,
        feed_pump_power,
        injection_efficiency,
        minimum_weber,
        maximum_ohnesorge,
        first_mode_hz,
        thermoacoustic_margin,
        radiative_heat_flux,
        coolant_outlet_temperature,
        h_coolant,
        coolant_dh,
        coolant_re,
        grayness,
        validity,
        minimum_wall_thickness_proxy,
        min_coolant_dh,
    ]
}

pub fn reduce_max<S: Scalar>(xs: &[S]) -> S {
    let m = xs.iter().map(Scalar::value).fold(f64::NEG_INFINITY, f64::max);
    if xs.iter().any(|x| x.value().is_nan()) {
        return S::from_f64(f64::NAN);
    }
    #[allow(clippy::float_cmp)]
    let count = xs.iter().filter(|x| x.value() == m).count();
    #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
    let grad: Vec<f64> = xs.iter().map(|x| if x.value() == m { 1.0 / count as f64 } else { 0.0 }).collect();
    if count == 1 {
        S::lift3(m, xs, &grad, &[], |_, _| vec![0.0; xs.len()])
    } else {
        S::lift(m, xs, &grad, &[])
    }
}

struct Phases<S> {
    propellant: Vec<S>,
    fuel: Vec<S>,
    oxidizer: Vec<S>,
    coolant: Vec<S>,
    authored: BTreeMap<&'static str, bool>,
}

pub const PHASE_COORDINATES: [(&str, &str); 6] = [
    ("propellant", "model:phase:propellant"),
    ("fuel", "model:phase:fuel"),
    ("oxidizer", "model:phase:oxidizer"),
    ("coolant", "model:phase:coolant"),
    ("hot_gas", "model:phase:hot_gas"),
    ("ambient", "model:phase:ambient"),
];

pub mod dimensionless {

pub fn residence_time(length: f64, velocity: f64) -> f64 {
    length / velocity.max(1e-30)
}

pub fn groups(
    density: f64,
    velocity: f64,
    length: f64,
    viscosity: f64,
    sound_speed: f64,
    residence: f64,
    chemical_time: f64,
    flame_time: f64,
    kolmogorov_time: f64,
) -> [f64; 4] {
    [
        density * velocity * length / viscosity.max(1e-30),
        velocity / sound_speed.max(1e-30),
        residence / chemical_time.max(1e-30),
        (flame_time / kolmogorov_time.max(1e-30)).max(0.0).sqrt(),
    ]
}
}
