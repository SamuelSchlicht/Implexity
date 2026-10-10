// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
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

