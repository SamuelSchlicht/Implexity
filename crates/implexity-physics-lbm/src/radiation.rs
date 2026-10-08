// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::{Dual, Scalar};

pub const SIGMA: f64 = 5.670_374_419e-8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surface {
    pub spacing_m: f64,
    pub ambient_k: f64,
    pub emissivity: f64,
    pub convection_w_m2k: f64,
}

#[inline]
pub fn residual<S: Scalar>(surface: S, temperature: S, conductivity: S, p: &Surface) -> S {
    let a = p.ambient_k;
    let resistance = S::from_f64(0.5 * p.spacing_m) / conductivity;
    let radiative = (surface - a) * (p.emissivity * SIGMA) * (surface + a) * (surface * surface + a * a);
    surface - temperature + resistance * ((surface - a) * p.convection_w_m2k + radiative)
}

#[must_use]
pub fn solve_surface(temperature: f64, conductivity: f64, p: &Surface) -> f64 {
    let mut lo = temperature.min(p.ambient_k);
    let mut hi = temperature.max(p.ambient_k);
    for _ in 0..60 {
        let mid = 0.5 * (lo + hi);
        if residual(mid, temperature, conductivity, p) > 0.0 {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    0.5 * (lo + hi)
}

fn lifted_surface<S: Scalar>(temperature: S, conductivity: S, p: &Surface) -> S {
    let (t, k) = (temperature.value(), conductivity.value());
    let s = solve_surface(t, k, p);
    let seeds =
        [Dual::<3>::new(s, [1.0, 0.0, 0.0]), Dual::new(t, [0.0, 1.0, 0.0]), Dual::new(k, [0.0, 0.0, 1.0])];
    let r = residual(seeds[0], seeds[1], seeds[2], p);
    let rs = r.eps[0];
    let grad = [-r.eps[1] / rs, -r.eps[2] / rs];
    S::lift(s, &[temperature, conductivity], &grad, &[0.0; 4])
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exchange<S> {
    pub surface_temperature_k: S,
    pub conductance_w_k: S,
    pub reservoir_power_w: S,
    pub radiation_power_w: S,
    pub convection_power_w: S,
    pub outgoing_correction_w_k: S,
    pub surface_residual_k: S,
}

pub fn exchange<S: Scalar>(temperature: S, conductivity: S, p: &Surface) -> Exchange<S> {
    let a = p.ambient_k;
    let h = p.convection_w_m2k;
    let resistance = S::from_f64(0.5 * p.spacing_m) / conductivity;
    let surface = lifted_surface(temperature, conductivity, p);
    let hrad = (surface + a) * (p.emissivity * SIGMA) * (surface * surface + a * a);
    let htot = hrad + h;
    let area = p.spacing_m * p.spacing_m;
    let g = htot * area / (resistance * htot + 1.0);
    let slope_h = surface * surface * surface * (4.0 * p.emissivity * SIGMA) + h;
    let slope = slope_h * area / (resistance * slope_h + 1.0);
    Exchange {
        surface_temperature_k: surface,
        conductance_w_k: g,
        reservoir_power_w: g * a,
        radiation_power_w: hrad * area * (S::from_f64(a) - surface),
        convection_power_w: (S::from_f64(a) - surface) * (area * h),
        outgoing_correction_w_k: (slope - g).max_f64(0.0),
        surface_residual_k: residual(surface, temperature, conductivity, p),
    }
}

