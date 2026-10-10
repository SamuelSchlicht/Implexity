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

use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapGrid {
    pub shape: [usize; 2],
    pub spacing_m: [f64; 2],
    pub viscosity_pa_s: f64,
    pub density_kg_m3: f64,
    pub time_step_s: f64,
    pub flow_timescale_s: f64,
    pub boundary_pressure_pa: [Option<f64>; 4],
    pub boundary_inventory_m: [f64; 4],
    pub mean_velocity_m_s: [f64; 2],
    pub relative_velocity_m_s: [f64; 2],
    pub maximum_aspect_ratio: f64,
    pub maximum_reduced_reynolds: f64,
    pub maximum_unsteady_inertia: f64,
    pub maximum_gap_slope: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilmState {
    pub pressure_above_cavitation_pa: Vec<f64>,
    pub inventory_m: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceExchange {
    pub lower_traction_pa: Vec<[f64; 3]>,
    pub upper_traction_pa: Vec<[f64; 3]>,
    pub fluid_reaction_pa: Vec<[f64; 3]>,
    pub boundary_volume_flux_m3_s: [f64; 4],
    pub volume_balance_m3_s: f64,
    pub contact_dissipation_w: f64,
    pub couette_dissipation_w: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContactParameters {
    pub asperity_height_m: f64,
    pub penalty_pa_per_m: f64,
    pub damping_pa_s_per_m: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GapSolution {
    pub state: FilmState,
    pub iterations: usize,
    pub relative_residual: f64,
}

#[derive(Clone, Debug)]
pub struct PairedSurfaceTraction<S> {
    pub lower_pa: [S; 3],
    pub upper_pa: [S; 3],
    pub fluid_reaction_pa: [S; 3],
    pub contact_dissipation_w_m2: S,
    pub couette_dissipation_w_m2: S,
}

pub fn paired_surface_traction<S: Scalar>(
    viscosity_pa_s: f64,
    gap: S,
    inventory: S,
    fluid_pressure: S,
    signed_gap: S,
    normal_velocity: S,
    relative_velocity: [S; 2],
    pressure_gradient: [S; 2],
    contact: &ContactParameters,
) -> CaeResult<PairedSurfaceTraction<S>> {
    if !viscosity_pa_s.is_finite()
        || viscosity_pa_s <= 0.
        || !gap.value().is_finite()
        || gap.value() <= 0.
        || !inventory.value().is_finite()
        || inventory.value() < 0.
        || inventory.value() > gap.value()
        || [fluid_pressure, signed_gap, normal_velocity]
            .iter()
            .chain(&relative_velocity)
            .chain(&pressure_gradient)
            .any(|v| !v.value().is_finite())
        || [
            contact.asperity_height_m,
            contact.penalty_pa_per_m,
            contact.damping_pa_s_per_m,
        ]
        .iter()
        .any(|v| !v.is_finite() || *v < 0.)
    {
        return Err(CaeError::contract(
            "invalid paired surface traction parameters",
        ));
    }
    let in_contact = signed_gap.value() < contact.asperity_height_m;
    let elastic = if in_contact {
        (S::from_f64(contact.asperity_height_m) - signed_gap) * contact.penalty_pa_per_m
    } else {
        S::zero()
    };
    let closing = if normal_velocity.value() < 0. {
        -normal_velocity
    } else {
        S::zero()
    };
    let viscous = if in_contact {
        closing * contact.damping_pa_s_per_m
    } else {
        S::zero()
    };
    let pressure = fluid_pressure + elastic + viscous;
    let mut lower = [S::zero(), S::zero(), -pressure];
    let mut upper = [S::zero(), S::zero(), pressure];
    let mut fluid = [S::zero(); 3];
    let fraction = inventory / gap;
    let mut dissipation = S::zero();
    for axis in 0..2 {
        let drag = relative_velocity[axis] * viscosity_pa_s * fraction / gap;
        let pressure_shear = -gap * pressure_gradient[axis] * fraction * 0.5;
        lower[axis] = drag + pressure_shear;
        upper[axis] = -drag + pressure_shear;
        fluid[axis] = -lower[axis] - upper[axis];
        dissipation += drag * relative_velocity[axis];
    }
    Ok(PairedSurfaceTraction {
        lower_pa: lower,
        upper_pa: upper,
        fluid_reaction_pa: fluid,
        contact_dissipation_w_m2: viscous * closing,
        couette_dissipation_w_m2: dissipation,
    })
}

impl GapGrid {
    pub fn array_shape(&self) -> [usize; 2] {
        [self.shape[1], self.shape[0]]
    }

    pub fn cells(&self) -> usize {
        self.shape[0].saturating_mul(self.shape[1])
    }

    pub fn validate(&self, gap: &[f64], liquid: bool) -> CaeResult<()> {
        if self.shape.iter().any(|n| *n == 0 || *n > 4096)
            || self.shape[0].checked_mul(self.shape[1]).is_none()
            || self.cells() > 65536
            || gap.len() != self.cells()
            || [
                self.viscosity_pa_s,
                self.density_kg_m3,
                self.time_step_s,
                self.flow_timescale_s,
                self.maximum_aspect_ratio,
                self.maximum_reduced_reynolds,
                self.maximum_unsteady_inertia,
                self.maximum_gap_slope,
            ]
            .iter()
            .any(|x| !x.is_finite() || *x <= 0.0)
            || self.spacing_m.iter().any(|x| !x.is_finite() || *x <= 0.0)
            || self
                .mean_velocity_m_s
                .iter()
                .chain(&self.relative_velocity_m_s)
                .any(|x| !x.is_finite())
            || self
                .boundary_pressure_pa
                .iter()
                .flatten()
                .any(|x| !x.is_finite() || (liquid && *x < 0.0))
            || self
                .boundary_inventory_m
                .iter()
                .any(|x| !x.is_finite() || *x < 0.0)
            || gap
                .iter()
                .any(|x| !x.is_finite() || *x <= 0.0 || !x.powi(3).is_finite() || x.powi(3) <= 0.0)
            || !(self.spacing_m[0] * self.spacing_m[1] / self.time_step_s).is_finite()
            || self.spacing_m[0] * self.spacing_m[1] / self.time_step_s <= 0.0
        {
            return Err(CaeError::contract("invalid narrow-gap parameters"));
        }
        for (i, &h) in gap.iter().enumerate() {
            let length = self.spacing_m[0].min(self.spacing_m[1]);
            let speed = self
                .mean_velocity_m_s
                .iter()
                .chain(&self.relative_velocity_m_s)
                .fold(0.0_f64, |a, b| a.max(b.abs()));
            let reduced_re = self.density_kg_m3 * speed * h * h / (self.viscosity_pa_s * length);
            let unsteady =
                self.density_kg_m3 * h * h / (self.viscosity_pa_s * self.flow_timescale_s);
            let slope = (0..4)
                .filter_map(|side| {
                    self.neighbour(i, side)
                        .map(|j| (gap[j] - h).abs() / self.spacing_m[side / 2])
                })
                .fold(0.0_f64, f64::max);
            if h / length > self.maximum_aspect_ratio
                || reduced_re > self.maximum_reduced_reynolds
                || unsteady > self.maximum_unsteady_inertia
                || slope > self.maximum_gap_slope
            {
                return Err(CaeError::contract(
                    "gap lies outside the prescribed Reynolds approximation bounds",
                ));
            }
        }
        Ok(())
    }

    fn neighbour(&self, i: usize, side: usize) -> Option<usize> {
        let x = i % self.shape[0];
        let y = i / self.shape[0];
        match side {
            0 if x > 0 => Some(i - 1),
            1 if x + 1 < self.shape[0] => Some(i + 1),
            2 if y > 0 => Some(i - self.shape[0]),
            3 if y + 1 < self.shape[1] => Some(i + self.shape[0]),
            _ => None,
        }
    }

    fn coefficients<S: Scalar>(&self, h: &[S], i: usize, side: usize) -> (S, f64) {
        let axis = side / 2;
        let width = self.spacing_m[1 - axis];
        let distance = self.spacing_m[axis];
        let hi3 = h[i] * h[i] * h[i];
        let conductivity = if let Some(j) = self.neighbour(i, side) {
            let hj3 = h[j] * h[j] * h[j];
            if hi3.value() <= hj3.value() {
                hi3 * 2.0 / (S::from_f64(1.0) + hi3 / hj3)
            } else {
                hj3 * 2.0 / (S::from_f64(1.0) + hj3 / hi3)
            }
        } else {
            hi3 * 2.0
        };
        let pressure = conductivity * (width / (12.0 * self.viscosity_pa_s * distance));
        let sign = if side % 2 == 0 { -1.0 } else { 1.0 };
        (pressure, self.mean_velocity_m_s[axis] * sign * width)
    }

    fn volume_flux<S: Scalar>(
        &self,
        gap: &[S],
        pressure: &[S],
        inventory: &[S],
        i: usize,
        side: usize,
    ) -> S {
        let ports = self.boundary_pressure_pa.map(|p| p.map(S::from_f64));
        self.volume_flux_ports(gap, pressure, inventory, i, side, &ports)
    }

    fn volume_flux_ports<S: Scalar>(
        &self,
        gap: &[S],
        pressure: &[S],
        inventory: &[S],
        i: usize,
        side: usize,
        boundary_pressure: &[Option<S>; 4],
    ) -> S {
        let (k, advection) = self.coefficients(gap, i, side);
        if let Some(j) = self.neighbour(i, side) {
            k * (pressure[i] - pressure[j])
                + if advection >= 0.0 {
                    inventory[i] * advection
                } else {
                    inventory[j] * advection
                }
        } else if let Some(p) = boundary_pressure[side] {
            k * (pressure[i] - p)
                + if advection >= 0.0 {
                    inventory[i] * advection
                } else {
                    S::from_f64(self.boundary_inventory_m[side] * advection)
                }
        } else {
            S::zero()
        }
    }

    pub fn conservation_residual<S: Scalar>(
        &self,
        gap: &[S],
        pressure: &[S],
        inventory: &[S],
        old_inventory: &[S],
    ) -> CaeResult<Vec<S>> {
        self.validate(&gap.iter().map(Scalar::value).collect::<Vec<_>>(), false)?;
        let n = self.cells();
        if [
            gap.len(),
            pressure.len(),
            inventory.len(),
            old_inventory.len(),
        ]
        .iter()
        .any(|v| *v != n)
        {
            return Err(CaeError::contract(
                "narrow-gap residual field length mismatch",
            ));
        }
        if pressure
            .iter()
            .chain(inventory)
            .chain(old_inventory)
            .any(|v| !v.value().is_finite())
        {
            return Err(CaeError::contract("nonfinite narrow-gap residual fields"));
        }
        let area = self.spacing_m[0] * self.spacing_m[1];
        Ok((0..n)
            .map(|i| {
                let flux = (0..4)
                    .map(|side| self.volume_flux(gap, pressure, inventory, i, side))
                    .sum::<S>();
                (inventory[i] - old_inventory[i]) * (area / self.time_step_s) + flux
            })
            .collect())
    }

    pub fn conservation_residual_ports<S: Scalar>(
        &self,
        gap: &[S],
        pressure: &[S],
        inventory: &[S],
        old_inventory: &[S],
        boundary_pressure: &[Option<S>; 4],
    ) -> CaeResult<Vec<S>> {
        let _ = self.conservation_residual(gap, pressure, inventory, old_inventory)?;
        if boundary_pressure
            .iter()
            .flatten()
            .any(|p| !p.value().is_finite())
        {
            return Err(CaeError::contract("nonfinite gap pressure ports"));
        }
        let area_dt = self.spacing_m[0] * self.spacing_m[1] / self.time_step_s;
        Ok((0..self.cells())
            .map(|i| {
                (inventory[i] - old_inventory[i]) * area_dt
                    + (0..4)
                        .map(|side| {
                            self.volume_flux_ports(
                                gap,
                                pressure,
                                inventory,
                                i,
                                side,
                                boundary_pressure,
                            )
                        })
                        .sum::<S>()
            })
            .collect())
    }

    pub fn boundary_flux_ports<S: Scalar>(
        &self,
        gap: &[S],
        pressure: &[S],
        inventory: &[S],
        boundary_pressure: &[Option<S>; 4],
    ) -> CaeResult<[S; 4]> {
        let _ = self.conservation_residual_ports(
            gap,
            pressure,
            inventory,
            inventory,
            boundary_pressure,
        )?;
        let mut flux = [S::zero(); 4];
        for i in 0..self.cells() {
            for side in 0..4 {
                if self.neighbour(i, side).is_none() {
                    flux[side] += self.volume_flux_ports(
                        gap,
                        pressure,
                        inventory,
                        i,
                        side,
                        boundary_pressure,
                    );
                }
            }
        }
        Ok(flux)
    }

    pub fn liquid_residual<S: Scalar>(
        &self,
        gap: &[S],
        pressure_above_cavitation: &[S],
        inventory: &[S],
        old_inventory: &[S],
        pressure_scale_pa: f64,
    ) -> CaeResult<Vec<S>> {
        if !pressure_scale_pa.is_finite() || pressure_scale_pa <= 0.0 {
            return Err(CaeError::contract("positive pressure scale required"));
        }
        let mut r =
            self.conservation_residual(gap, pressure_above_cavitation, inventory, old_inventory)?;
        for i in 0..self.cells() {
            let p = pressure_above_cavitation[i] / pressure_scale_pa;
            let deficit = S::from_f64(1.0) - inventory[i] / gap[i];
            if p.value() == 0.0 && deficit.value() == 0.0 {
                return Err(CaeError::contract(
                    "liquid-film derivative is undefined at the cavitation transition",
                ));
            }
            r.push((p * p + deficit * deficit).sqrt() - p - deficit);
        }
        Ok(r)
    }

    pub fn solve_liquid(
        &self,
        gap: &[f64],
        old_inventory: &[f64],
        tolerance: f64,
        max_iterations: usize,
    ) -> CaeResult<GapSolution> {
        self.validate(gap, true)?;
        if old_inventory.len() != self.cells()
            || old_inventory.iter().any(|v| !v.is_finite() || *v < 0.0)
            || !tolerance.is_finite()
            || tolerance <= 0.0
            || max_iterations == 0
        {
            return Err(CaeError::contract(
                "invalid liquid-film state or solver options",
            ));
        }
        let mut state = FilmState {
            pressure_above_cavitation_pa: vec![0.0; self.cells()],
            inventory_m: old_inventory
                .iter()
                .zip(gap)
                .map(|(m, h)| m.min(*h))
                .collect(),
        };
        let area_dt = self.spacing_m[0] * self.spacing_m[1] / self.time_step_s;
        let scale = old_inventory
            .iter()
            .chain(gap)
            .fold(0.0_f64, |a, b| a.max(*b))
            * area_dt;
        for iteration in 1..=max_iterations {
            for i in 0..self.cells() {
                let mut rhs = old_inventory[i] * area_dt;
                let mut pressure_diagonal = 0.0;
                let mut inventory_diagonal = area_dt;
                for side in 0..4 {
                    let (k, a) = self.coefficients(gap, i, side);
                    let neighbour = self.neighbour(i, side);
                    if neighbour.is_none() && self.boundary_pressure_pa[side].is_none() {
                        continue;
                    }
                    pressure_diagonal += k;
                    if let Some(j) = neighbour {
                        rhs += k * state.pressure_above_cavitation_pa[j];
                        if a >= 0.0 {
                            inventory_diagonal += a;
                        } else {
                            rhs -= a * state.inventory_m[j];
                        }
                    } else {
                        rhs += k * self.boundary_pressure_pa[side].unwrap_or(0.0);
                        if a >= 0.0 {
                            inventory_diagonal += a;
                        } else {
                            rhs -= a * self.boundary_inventory_m[side];
                        }
                    }
                }
                let capacity_rhs = inventory_diagonal * gap[i];
                if rhs > capacity_rhs {
                    if pressure_diagonal <= 0.0 {
                        return Err(CaeError::convergence(
                            "sealed incompressible film cannot accommodate the prescribed closure",
                        ));
                    }
                    state.pressure_above_cavitation_pa[i] =
                        (rhs - capacity_rhs) / pressure_diagonal;
                    state.inventory_m[i] = gap[i];
                } else {
                    state.pressure_above_cavitation_pa[i] = 0.0;
                    state.inventory_m[i] = rhs / inventory_diagonal;
                }
            }
            let r = self.conservation_residual(
                gap,
                &state.pressure_above_cavitation_pa,
                &state.inventory_m,
                old_inventory,
            )?;
            if r.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::convergence("nonfinite conservative gap residual"));
            }
            let relative_residual =
                r.iter().fold(0.0_f64, |a, b| a.max(b.abs())) / scale.max(f64::MIN_POSITIVE);
            if relative_residual <= tolerance {
                self.check_solution_inertia(gap, &state)?;
                return Ok(GapSolution {
                    state,
                    iterations: iteration,
                    relative_residual,
                });
            }
        }
        Err(CaeError::convergence(
            "conservative liquid-film solve did not converge",
        ))
    }

    pub fn solve_gas(
        &self,
        gap: &[f64],
        old_gap: &[f64],
        absolute_reference_pressure_pa: f64,
        maximum_pressure_ratio: f64,
        tolerance: f64,
        max_iterations: usize,
    ) -> CaeResult<GapSolution> {
        self.validate(gap, false)?;
        if self.boundary_pressure_pa.iter().all(Option::is_none) {
            return Err(CaeError::contract("gas gap requires a pressure boundary"));
        }
        if old_gap.len() != self.cells()
            || old_gap.iter().any(|v| !v.is_finite() || *v <= 0.0)
            || !absolute_reference_pressure_pa.is_finite()
            || absolute_reference_pressure_pa <= 0.0
            || !maximum_pressure_ratio.is_finite()
            || maximum_pressure_ratio <= 0.0
            || maximum_pressure_ratio >= 1.0
            || !tolerance.is_finite()
            || tolerance <= 0.0
            || max_iterations == 0
        {
            return Err(CaeError::contract(
                "invalid incompressible gas-gap state or solver options",
            ));
        }
        if self
            .boundary_pressure_pa
            .iter()
            .flatten()
            .any(|p| p.abs() / absolute_reference_pressure_pa > maximum_pressure_ratio)
        {
            return Err(CaeError::contract(
                "gas-gap boundary pressure requires a compressible model",
            ));
        }
        let mut pressure = vec![0.0; self.cells()];
        let area_dt = self.spacing_m[0] * self.spacing_m[1] / self.time_step_s;
        let scale = gap.iter().chain(old_gap).fold(0.0_f64, |a, b| a.max(*b)) * area_dt;
        for iteration in 1..=max_iterations {
            for i in 0..self.cells() {
                let mut rhs = (old_gap[i] - gap[i]) * area_dt;
                let mut diagonal = 0.0;
                for side in 0..4 {
                    let (k, a) = self.coefficients(gap, i, side);
                    if let Some(j) = self.neighbour(i, side) {
                        diagonal += k;
                        rhs += k * pressure[j] - a * if a >= 0.0 { gap[i] } else { gap[j] };
                    } else if let Some(p) = self.boundary_pressure_pa[side] {
                        diagonal += k;
                        rhs += k * p
                            - a * if a >= 0.0 {
                                gap[i]
                            } else {
                                self.boundary_inventory_m[side]
                            };
                    }
                }
                if diagonal <= 0.0 {
                    return Err(CaeError::convergence("gas-gap pressure is underdetermined"));
                }
                pressure[i] = rhs / diagonal;
            }
            let r = self.conservation_residual(gap, &pressure, gap, old_gap)?;
            if r.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::convergence("nonfinite conservative gap residual"));
            }
            let relative_residual =
                r.iter().fold(0.0_f64, |a, b| a.max(b.abs())) / scale.max(f64::MIN_POSITIVE);
            if relative_residual <= tolerance {
                if pressure
                    .iter()
                    .any(|p| p.abs() / absolute_reference_pressure_pa > maximum_pressure_ratio)
                {
                    return Err(CaeError::contract(
                        "gas-gap pressure requires a compressible model",
                    ));
                }
                let state = FilmState {
                    pressure_above_cavitation_pa: pressure,
                    inventory_m: gap.to_vec(),
                };
                self.check_solution_inertia(gap, &state)?;
                return Ok(GapSolution {
                    state,
                    iterations: iteration,
                    relative_residual,
                });
            }
        }
        Err(CaeError::convergence(
            "gas-gap Reynolds solve did not converge",
        ))
    }

    pub fn surface_exchange(
        &self,
        gap: &[f64],
        old_inventory: &[f64],
        state: &FilmState,
        signed_surface_gap: &[f64],
        normal_velocity_m_s: &[f64],
        contact: &ContactParameters,
        pressure_reference_pa: f64,
    ) -> CaeResult<SurfaceExchange> {
        self.validate(gap, false)?;
        let n = self.cells();
        if [
            gap.len(),
            old_inventory.len(),
            state.pressure_above_cavitation_pa.len(),
            state.inventory_m.len(),
            signed_surface_gap.len(),
            normal_velocity_m_s.len(),
        ]
        .iter()
        .any(|v| *v != n)
            || [
                contact.asperity_height_m,
                contact.penalty_pa_per_m,
                contact.damping_pa_s_per_m,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
            || !pressure_reference_pa.is_finite()
            || gap.iter().any(|h| !h.is_finite() || *h <= 0.0)
            || signed_surface_gap
                .iter()
                .chain(normal_velocity_m_s)
                .chain(&state.pressure_above_cavitation_pa)
                .chain(&state.inventory_m)
                .chain(old_inventory)
                .any(|v| !v.is_finite())
            || state
                .inventory_m
                .iter()
                .zip(gap)
                .any(|(m, h)| *m < 0.0 || *m > *h)
        {
            return Err(CaeError::contract("invalid surface exchange fields"));
        }
        let area = self.spacing_m[0] * self.spacing_m[1];
        let mut lower = Vec::with_capacity(n);
        let mut upper = Vec::with_capacity(n);
        let mut fluid = Vec::with_capacity(n);
        let mut contact_dissipation = 0.0;
        let mut couette_dissipation = 0.0;
        for i in 0..n {
            let gradient = [
                self.pressure_gradient(&state.pressure_above_cavitation_pa, i, 0),
                self.pressure_gradient(&state.pressure_above_cavitation_pa, i, 1),
            ];
            let traction = paired_surface_traction(
                self.viscosity_pa_s,
                gap[i],
                state.inventory_m[i],
                state.pressure_above_cavitation_pa[i] + pressure_reference_pa,
                signed_surface_gap[i],
                normal_velocity_m_s[i],
                self.relative_velocity_m_s,
                gradient,
                contact,
            )?;
            contact_dissipation += traction.contact_dissipation_w_m2 * area;
            couette_dissipation += traction.couette_dissipation_w_m2 * area;
            lower.push(traction.lower_pa);
            upper.push(traction.upper_pa);
            fluid.push(traction.fluid_reaction_pa);
        }
        let mut boundary_flux = [0.0; 4];
        for i in 0..n {
            for side in 0..4 {
                if self.neighbour(i, side).is_none() {
                    boundary_flux[side] += self.volume_flux(
                        gap,
                        &state.pressure_above_cavitation_pa,
                        &state.inventory_m,
                        i,
                        side,
                    );
                }
            }
        }
        let storage = state
            .inventory_m
            .iter()
            .zip(old_inventory)
            .map(|(m, o)| (m - o) * area / self.time_step_s)
            .sum::<f64>();
        Ok(SurfaceExchange {
            lower_traction_pa: lower,
            upper_traction_pa: upper,
            fluid_reaction_pa: fluid,
            boundary_volume_flux_m3_s: boundary_flux,
            volume_balance_m3_s: storage + boundary_flux.iter().sum::<f64>(),
            contact_dissipation_w: contact_dissipation,
            couette_dissipation_w: couette_dissipation,
        })
    }

    fn check_solution_inertia(&self, gap: &[f64], state: &FilmState) -> CaeResult<()> {
        for i in 0..self.cells() {
            for side in 0..4 {
                let q = self
                    .volume_flux(
                        gap,
                        &state.pressure_above_cavitation_pa,
                        &state.inventory_m,
                        i,
                        side,
                    )
                    .abs();
                let axis = side / 2;
                let face_gap = self
                    .neighbour(i, side)
                    .map_or(gap[i], |j| gap[i].min(gap[j]));
                let speed = q / (self.spacing_m[1 - axis] * face_gap);
                let inertia = self.density_kg_m3 * speed * face_gap * face_gap
                    / (self.viscosity_pa_s * self.spacing_m[axis]);
                if !inertia.is_finite() || inertia > self.maximum_reduced_reynolds {
                    return Err(CaeError::contract(
                        "pressure-driven gap flow requires an inertial model",
                    ));
                }
            }
        }
        Ok(())
    }

    fn pressure_gradient(&self, pressure: &[f64], i: usize, axis: usize) -> f64 {
        let side0 = axis * 2;
        let side1 = side0 + 1;
        let centre = pressure[i];
        let left = self.neighbour(i, side0).map_or_else(
            || self.boundary_pressure_pa[side0].map_or(centre, |v| 2.0 * v - centre),
            |j| pressure[j],
        );
        let right = self.neighbour(i, side1).map_or_else(
            || self.boundary_pressure_pa[side1].map_or(centre, |v| 2.0 * v - centre),
            |j| pressure[j],
        );
        (right - left) / (2.0 * self.spacing_m[axis])
    }
}
