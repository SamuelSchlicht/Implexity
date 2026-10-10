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

use std::sync::Arc;

use implexity_core::{CaeError, CaeResult};

use super::carrier::RigidCarrier;
use super::field::{EntrainedInertia, MovingLbm, MovingLbmConfig};
use super::ib::{ImmersedBoundary, SurfaceMarkers};
use super::ibb::{Cylinder, InterpolatedBounceBack};
use super::lattice::D2Q9;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BodyMethod {
    Psm {
        fill: f64,
    },
    PsmCompensated {
        fill: f64,
    },
    InterpolatedBounceBack,
    ImmersedBoundary {
        iterations: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OscillatingCylinder {
    pub diameter_cells: f64,
    pub keulegan_carpenter: f64,
    pub reynolds: f64,
    pub lattice_velocity: f64,
    pub domain_diameters: [f64; 2],
    pub periods: usize,
    pub fit_periods: usize,
    pub substeps: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MorisonFit {
    pub drag: f64,
    pub added_mass: f64,
    pub added_mass_raw: f64,
    pub residual: f64,
    pub times: Vec<f64>,
    pub forces: Vec<f64>,
}

impl OscillatingCylinder {
    pub fn run(&self, method: BodyMethod) -> CaeResult<MorisonFit> {
        let d = self.diameter_cells;
        let u_max = self.lattice_velocity;
        if !(d >= 4.0
            && u_max > 0.0
            && u_max < 0.1
            && self.periods > self.fit_periods
            && self.fit_periods > 0)
        {
            return Err(CaeError::contract("oscillating cylinder: invalid parameters"));
        }
        let nu = u_max * d / self.reynolds;
        let amplitude = self.keulegan_carpenter * d / (2.0 * std::f64::consts::PI);
        let period = self.keulegan_carpenter * d / u_max;
        let omega = 2.0 * std::f64::consts::PI / period;
        let m = self.substeps;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps_per_period = (period / m as f64).round() as usize;
        let dt_s = period / steps_per_period as f64;
        if u_max * dt_s > 0.5 {
            return Err(CaeError::contract(
                "oscillating cylinder: displacement per macro step above half a cell",
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let shape = [
            (self.domain_diameters[0] * d).round() as usize,
            (self.domain_diameters[1] * d).round() as usize,
            1,
        ];
        let centre = [shape[0] as f64 / 2.0, shape[1] as f64 / 2.0, 0.5];
        let mut cfg = MovingLbmConfig::new(shape, 1.0, [false, false, true], 1.0, nu, dt_s, m);
        cfg.lattice_velocity_limit = 0.2;
        cfg.mach_limit = 0.3;
        let r = d / 2.0;
        let position = |t: f64| -amplitude * (omega * t).sin();
        let mut internal = 0.0;
        let field: Box<dyn Fn(usize, &[f64], &[f64], &[f64]) -> CaeResult<(Vec<f64>, Vec<f64>)>> =
            match method {
                BodyMethod::Psm { fill } | BodyMethod::PsmCompensated { fill } => {
                    let carrier = RigidCarrier::sampled(
                        [centre[0] - r, centre[1] - r, 0.0],
                        [centre[0] + r, centre[1] + r, 1.0],
                        [0.5, 0.5, 1.0],
                        &|x| (x[0] - centre[0]).powi(2) + (x[1] - centre[1]).powi(2) < r * r,
                    )?
                    .scaled_weights(fill)?;
                    let mut cfg = cfg;
                    if matches!(method, BodyMethod::PsmCompensated { .. }) {
                        cfg.entrained_inertia = EntrainedInertia::Compensated;
                    } else {
                        internal = super::carrier::LagrangianCarrier::point_count(&carrier) as f64 * 0.25;
                    }
                    let lbm = MovingLbm::<9, D2Q9>::new(cfg, Arc::new(carrier))?;
                    Box::new(move |n, s, a, b| {
                        let o = lbm.subcycle(n, s, a, b, &[], 1.0)?;
                        Ok((o.state, o.flux))
                    })
                }
                BodyMethod::ImmersedBoundary { iterations } => {
                    let markers = SurfaceMarkers::circle(centre, r, 2, 1.0, 1.0, 1.0)?;
                    internal = std::f64::consts::PI * r * r;
                    let ib = ImmersedBoundary::<9, D2Q9>::new(cfg, markers, iterations)?;
                    Box::new(move |_, s, a, b| {
                        let o = ib.subcycle(s, a, b)?;
                        Ok((o.state, o.flux))
                    })
                }
                BodyMethod::InterpolatedBounceBack => {
                    let ibb = InterpolatedBounceBack::<9, D2Q9>::new(
                        cfg,
                        Arc::new(Cylinder { centre, radius: r, axis: 2 }),
                    )?;
                    Box::new(move |_, s, a, b| {
                        let o = ibb.subcycle(s, a, b)?;
                        Ok((o.state, o.flux))
                    })
                }
            };
        let mut state = {
            let eq = super::lattice::equilibrium::<f64, 9, D2Q9>(1.0, [0.0; 3]);
            let cells = shape[0] * shape[1];
            let mut s = vec![0.0; 9 * cells];
            for i in 0..9 {
                s[i * cells..(i + 1) * cells].fill(eq[i]);
            }
            s
        };
        let total = steps_per_period * self.periods;
        let fit_from = total - steps_per_period * self.fit_periods;
        let mut times = Vec::new();
        let mut forces = Vec::new();
        let (mut saa, mut sab, mut sbb, mut sfa, mut sfb, mut sff) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for n in 1..=total {
            let (t0, t1) = ((n - 1) as f64 * dt_s, n as f64 * dt_s);
            let (next, flux) = field(n, &state, &[position(t0), 0.0, 0.0], &[position(t1), 0.0, 0.0])?;
            state = next;
            if n > fit_from {
                let tm = 0.5 * (t0 + t1);
                let u = -amplitude * omega * (omega * tm).cos();
                let du = amplitude * omega * omega * (omega * tm).sin();
                let (fa, fb) = (-u.abs() * u, -du);
                let f = flux[0];
                saa += fa * fa;
                sab += fa * fb;
                sbb += fb * fb;
                sfa += f * fa;
                sfb += f * fb;
                sff += f * f;
                times.push(tm);
                forces.push(f);
            }
        }
        let det = saa * sbb - sab * sab;
        let a = (sfa * sbb - sfb * sab) / det;
        let b = (saa * sfb - sab * sfa) / det;
        let resid2 = sff - 2.0 * (a * sfa + b * sfb) + a * a * saa + 2.0 * a * b * sab + b * b * sbb;
        let area = std::f64::consts::PI * d * d / 4.0;
        Ok(MorisonFit {
            drag: 2.0 * a / d,
            added_mass: (b - internal) / area,
            added_mass_raw: b / area,
            residual: (resid2.max(0.0) / sff).sqrt(),
            times,
            forces,
        })
    }
}
