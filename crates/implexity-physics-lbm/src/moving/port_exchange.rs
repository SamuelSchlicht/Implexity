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

use super::lattice::{Lattice, equilibrium, moments};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

#[derive(Clone, Debug)]
pub struct PortMacroscopicState<S> {
    pub density_kg_m3: S,
    pub population_velocity_m_s: [S; 3],
    pub absolute_pressure_pa: S,
}

#[derive(Clone, Debug)]
pub struct PortPhysicalMoments<S> {
    pub density_kg_m3: S,
    pub momentum_density_kg_m2_s: [S; 3],
    pub second_moment_pa: [[S; 3]; 3],
}

#[derive(Clone, Debug)]
pub struct PortExchange {
    cells: usize,
    excluded: std::sync::Arc<Vec<usize>>,
    ports: Vec<Vec<(usize, f64)>>,
    mass_scale_kg: f64,
    momentum_scale_kg_m_s: f64,
    pressure_scale_pa: f64,
    reference_density_kg_m3: f64,
    native_speed_m_s: f64,
}

fn fail(message: &str) -> CaeError {
    CaeError::contract(message)
}

impl PortExchange {
    pub fn new(
        cells: usize,
        mut excluded: Vec<usize>,
        ports: Vec<Vec<(usize, f64)>>,
        spacing_m: f64,
        density_kg_m3: f64,
        native_step_s: f64,
    ) -> CaeResult<Self> {
        if cells == 0
            || excluded.is_empty()
            || ports.is_empty()
            || [spacing_m, density_kg_m3, native_step_s]
                .iter()
                .any(|x| !x.is_finite() || *x <= 0.0)
        {
            return Err(fail(
                "port exchange requires positive scales and explicit gap ownership",
            ));
        }
        let mut owned = std::collections::BTreeSet::new();
        for &cell in &excluded {
            if cell >= cells || !owned.insert(cell) {
                return Err(fail("gap exclusion cells must be unique and valid"));
            }
        }
        for port in &ports {
            if port.is_empty()
                || port.iter().any(|(cell, w)| {
                    *cell >= cells || !w.is_finite() || *w <= 0.0 || !owned.insert(*cell)
                })
                || (port.iter().map(|(_, w)| w).sum::<f64>() - 1.0).abs() > 1e-12
            {
                return Err(fail(
                    "ports require disjoint active cells and positive normalized weights",
                ));
            }
        }
        let mass_scale_kg = density_kg_m3 * spacing_m.powi(3);
        let speed = spacing_m / native_step_s;
        let momentum_scale_kg_m_s = mass_scale_kg * speed;
        let pressure_scale_pa = density_kg_m3 * speed * speed / 3.0;
        if [mass_scale_kg, momentum_scale_kg_m_s, pressure_scale_pa]
            .iter()
            .any(|x| !x.is_finite() || *x <= 0.0)
        {
            return Err(fail(
                "port exchange scales are outside the positive finite range",
            ));
        }
        excluded.sort_unstable();
        Ok(Self {
            cells,
            excluded: std::sync::Arc::new(excluded),
            ports,
            mass_scale_kg,
            momentum_scale_kg_m_s,
            pressure_scale_pa,
            reference_density_kg_m3: density_kg_m3,
            native_speed_m_s: speed,
        })
    }

    pub fn ports(&self) -> &[Vec<(usize, f64)>] {
        &self.ports
    }

    pub fn port_subset(&self, indices: &[usize]) -> CaeResult<Self> {
        let mut selected = std::collections::BTreeSet::new();
        if indices.is_empty()
            || indices
                .iter()
                .any(|i| *i >= self.ports.len() || !selected.insert(*i))
        {
            return Err(fail("port subset requires unique valid owners"));
        }
        Ok(Self {
            cells: self.cells,
            excluded: self.excluded.clone(),
            ports: indices.iter().map(|i| self.ports[*i].clone()).collect(),
            mass_scale_kg: self.mass_scale_kg,
            momentum_scale_kg_m_s: self.momentum_scale_kg_m_s,
            pressure_scale_pa: self.pressure_scale_pa,
            reference_density_kg_m3: self.reference_density_kg_m3,
            native_speed_m_s: self.native_speed_m_s,
        })
    }

    pub fn port_count(&self) -> usize {
        self.ports.len()
    }
    pub fn cell_count(&self) -> usize {
        self.cells
    }
    pub fn excluded_cells(&self) -> &[usize] {
        &self.excluded
    }

    pub fn validate_native_scales(
        &self,
        spacing_m: f64,
        density_kg_m3: f64,
        native_step_s: f64,
    ) -> CaeResult<()> {
        if [spacing_m, density_kg_m3, native_step_s]
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0)
        {
            return Err(fail(
                "native port exchange scales must be positive and finite",
            ));
        }
        let mass = density_kg_m3 * spacing_m.powi(3);
        let speed = spacing_m / native_step_s;
        let native = [mass, mass * speed, density_kg_m3 * speed * speed / 3.0];
        let exchange = [
            self.mass_scale_kg,
            self.momentum_scale_kg_m_s,
            self.pressure_scale_pa,
        ];
        if native.iter().zip(exchange).any(|(a, b)| {
            !a.is_finite()
                || *a <= 0.0
                || (*a - b).abs() > 8.0 * f64::EPSILON * a.abs().max(b.abs())
        }) {
            return Err(fail(
                "port exchange scales do not match the native LBM scales",
            ));
        }
        Ok(())
    }

    pub fn validate_native_mask(&self, solid_mask: &[bool]) -> CaeResult<()> {
        if solid_mask.len() != self.cells
            || self.excluded.iter().any(|i| !solid_mask[*i])
            || self.ports.iter().flatten().any(|(i, _)| solid_mask[*i])
        {
            return Err(fail(
                "native fluid mask must exclude the gap and retain all exchange port cells",
            ));
        }
        Ok(())
    }

    fn check_lattice<const Q: usize, L: Lattice<Q>>() -> CaeResult<()> {
        if L::DIMENSIONS != 3 || (L::W.iter().sum::<f64>() - 1.0).abs() > 1e-12 {
            return Err(fail(
                "port exchange requires a normalized three-dimensional lattice",
            ));
        }
        for a in 0..3 {
            if (0..Q).map(|i| L::W[i] * L::CF[i][a]).sum::<f64>().abs() > 1e-12 {
                return Err(fail("port exchange requires zero lattice first moments"));
            }
            for b in 0..3 {
                let value = (0..Q)
                    .map(|i| L::W[i] * L::CF[i][a] * L::CF[i][b])
                    .sum::<f64>();
                if (value - if a == b { 1.0 / 3.0 } else { 0.0 }).abs() > 1e-12 {
                    return Err(fail(
                        "port exchange requires isotropic lattice second moments",
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_state<S: Scalar, const Q: usize, L: Lattice<Q>>(&self, state: &[S]) -> CaeResult<()> {
        Self::check_lattice::<Q, L>()?;
        if self.cells.checked_mul(Q) != Some(state.len())
            || state.iter().any(|v| !v.value().is_finite())
        {
            return Err(fail(
                "port exchange population shape or finite-value mismatch",
            ));
        }
        Ok(())
    }

    pub fn pressures<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[S],
    ) -> CaeResult<Vec<S>> {
        self.check_state::<S, Q, L>(state)?;
        self.ports
            .iter()
            .map(|port| {
                let mut rho = S::zero();
                for &(cell, w) in port {
                    for i in 0..Q {
                        rho += state[i * self.cells + cell] * w;
                    }
                }
                if rho.value() <= 0.0 {
                    return Err(fail("exchange port density must be positive"));
                }
                Ok((rho - 1.0) * self.pressure_scale_pa)
            })
            .collect()
    }

    pub fn macroscopic_states<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[S],
        reference_pressure_pa: f64,
    ) -> CaeResult<Vec<PortMacroscopicState<S>>> {
        self.check_state::<S, Q, L>(state)?;
        if !reference_pressure_pa.is_finite()
            || reference_pressure_pa <= 0.
            || (reference_pressure_pa - self.pressure_scale_pa).abs()
                > 32. * f64::EPSILON * reference_pressure_pa.max(self.pressure_scale_pa)
        {
            return Err(fail(
                "kinetic exchange requires matching absolute and native isothermal pressure scales",
            ));
        }
        self.ports
            .iter()
            .map(|port| {
                let mut density = S::zero();
                let mut momentum = [S::zero(); 3];
                for &(cell, weight) in port {
                    for q in 0..Q {
                        let population = state[q * self.cells + cell];
                        if population.value() < 0. {
                            return Err(fail(
                                "kinetic exchange requires nonnegative bulk populations",
                            ));
                        }
                        density += population * weight;
                        for axis in 0..3 {
                            momentum[axis] += population * (weight * L::CF[q][axis]);
                        }
                    }
                }
                if !density.value().is_finite() || density.value() <= 0. {
                    return Err(fail(
                        "kinetic exchange port density must be positive and finite",
                    ));
                }
                let population_velocity_m_s = momentum.map(|v| v / density * self.native_speed_m_s);
                let density_kg_m3 = density * self.reference_density_kg_m3;
                let absolute_pressure_pa = density * self.pressure_scale_pa;
                if population_velocity_m_s
                    .iter()
                    .chain([&density_kg_m3, &absolute_pressure_pa])
                    .any(|v| !v.value().is_finite())
                {
                    return Err(fail("kinetic exchange port moments overflow"));
                }
                Ok(PortMacroscopicState {
                    density_kg_m3,
                    population_velocity_m_s,
                    absolute_pressure_pa,
                })
            })
            .collect()
    }

    pub fn equilibrium_physical_moments<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[S],
        reference_pressure_pa: f64,
    ) -> CaeResult<Vec<PortPhysicalMoments<S>>> {
        let macroscopic = self.macroscopic_states::<S, Q, L>(state, reference_pressure_pa)?;
        self.ports
            .iter()
            .zip(macroscopic)
            .map(|(port, macro_state)| {
                let mut second = [[S::zero(); 3]; 3];
                for &(cell, weight) in port {
                    let f: [S; Q] = std::array::from_fn(|q| state[q * self.cells + cell]);
                    let tensor = equilibrium_cell_tensor::<S, Q, L>(&f)?;
                    for a in 0..3 {
                        for b in a..3 {
                            let value = tensor[a][b];
                            second[a][b] += value
                                * (weight
                                    * self.reference_density_kg_m3
                                    * self.native_speed_m_s.powi(2));
                        }
                    }
                }
                for a in 0..3 {
                    for b in 0..a {
                        second[a][b] = second[b][a];
                    }
                }
                if second.iter().flatten().any(|v| !v.value().is_finite()) {
                    return Err(fail("equilibrium physical port tensor overflow"));
                }
                Ok(PortPhysicalMoments {
                    density_kg_m3: macro_state.density_kg_m3,
                    momentum_density_kg_m2_s: macro_state
                        .population_velocity_m_s
                        .map(|v| v * macro_state.density_kg_m3),
                    second_moment_pa: second,
                })
            })
            .collect()
    }

    pub fn equilibrium_second_moments_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[f64],
        reference_pressure_pa: f64,
        bars: &[[f64; 6]],
    ) -> CaeResult<Vec<f64>> {
        self.equilibrium_physical_moments::<f64, Q, L>(state, reference_pressure_pa)?;
        if bars.len() != self.ports.len() || bars.iter().flatten().any(|v| !v.is_finite()) {
            return Err(fail("equilibrium tensor cotangent shape or value"));
        }
        let mut result = vec![0.; state.len()];
        for (port, bar) in self.ports.iter().zip(bars) {
            for &(cell, weight) in port {
                let row = std::array::from_fn(|q| {
                    let mut direction = [0.; Q];
                    direction[q] = 1.;
                    implexity_ad::Dual::new(state[q * self.cells + cell], direction)
                });
                let tensor = equilibrium_cell_tensor::<implexity_ad::Dual<Q>, Q, L>(&row)?;
                let scale = weight * self.reference_density_kg_m3 * self.native_speed_m_s.powi(2);
                for (k, (a, b)) in [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)].into_iter().enumerate() {
                    for q in 0..Q {
                        result[q * self.cells + cell] += tensor[a][b].eps[q] * scale * bar[k];
                    }
                }
            }
        }
        if result.iter().any(|v| !v.is_finite()) {
            return Err(fail("equilibrium tensor cotangent overflow"));
        }
        Ok(result)
    }

    fn check_stress_lattice<const Q: usize, L: Lattice<Q>>() -> CaeResult<()> {
        Self::check_lattice::<Q, L>()?;
        for a in 0..3 {
            for b in 0..3 {
                for c in 0..3 {
                    let third = (0..Q)
                        .map(|q| L::W[q] * L::CF[q][a] * L::CF[q][b] * L::CF[q][c])
                        .sum::<f64>();
                    if third.abs() > 1e-12 {
                        return Err(fail(
                            "physical port stress requires zero third lattice moments",
                        ));
                    }
                    for d in 0..3 {
                        let fourth = (0..Q)
                            .map(|q| {
                                L::W[q] * L::CF[q][a] * L::CF[q][b] * L::CF[q][c] * L::CF[q][d]
                            })
                            .sum::<f64>();
                        let expected = ((a == b && c == d) as u8
                            + (a == c && b == d) as u8
                            + (a == d && b == c) as u8)
                            as f64
                            / 9.;
                        if (fourth - expected).abs() > 1e-12 {
                            return Err(fail(
                                "physical port stress requires isotropic fourth lattice moments",
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn zero_force_srt_pre_collision_moments_from_rows<
        S: Scalar,
        const Q: usize,
        L: Lattice<Q>,
    >(
        &self,
        rows: &[(usize, Vec<S>)],
        relaxation_rates: &[(usize, S)],
        reference_pressure_pa: f64,
    ) -> CaeResult<Vec<PortPhysicalMoments<S>>> {
        Self::check_stress_lattice::<Q, L>()?;
        if !reference_pressure_pa.is_finite()
            || reference_pressure_pa <= 0.
            || (reference_pressure_pa - self.pressure_scale_pa).abs()
                > 32. * f64::EPSILON * reference_pressure_pa.max(self.pressure_scale_pa)
            || rows.len() != relaxation_rates.len()
            || rows.windows(2).any(|w| w[0].0 >= w[1].0)
            || rows
                .iter()
                .zip(relaxation_rates)
                .any(|((cell, row), (other, rate))| {
                    cell != other
                        || *cell >= self.cells
                        || row.len() != Q
                        || row.iter().any(|v| !v.value().is_finite() || v.value() < 0.)
                        || !rate.value().is_finite()
                        || rate.value() <= 0.
                        || rate.value() >= 2.
                })
        {
            return Err(fail(
                "pre-collision port rows, rates or pressure scale are invalid",
            ));
        }
        self.ports
            .iter()
            .map(|port| {
                let mut density = S::zero();
                let mut momentum = [S::zero(); 3];
                let mut second = [[S::zero(); 3]; 3];
                for &(cell, weight) in port {
                    let k = rows
                        .binary_search_by_key(&cell, |row| row.0)
                        .map_err(|_| fail("pre-collision port cell is absent"))?;
                    let f: [S; Q] = std::array::from_fn(|q| rows[k].1[q]);
                    let (rho, j) = moments::<S, Q, L>(&f);
                    if !rho.value().is_finite() || rho.value() <= 0. {
                        return Err(fail(
                            "pre-collision port density must be positive and finite",
                        ));
                    }
                    let eq = equilibrium::<S, Q, L>(rho, j.map(|v| v / rho));
                    let correction = S::one() - relaxation_rates[k].1 * 0.5;
                    density += rho * (weight * self.reference_density_kg_m3);
                    for a in 0..3 {
                        momentum[a] +=
                            j[a] * (weight * self.reference_density_kg_m3 * self.native_speed_m_s);
                        for b in a..3 {
                            let mut raw = S::zero();
                            for q in 0..Q {
                                raw += (eq[q] + correction * (f[q] - eq[q]))
                                    * (L::CF[q][a] * L::CF[q][b]);
                            }
                            second[a][b] += raw
                                * (weight
                                    * self.reference_density_kg_m3
                                    * self.native_speed_m_s.powi(2));
                        }
                    }
                }
                for a in 0..3 {
                    for b in 0..a {
                        second[a][b] = second[b][a];
                    }
                }
                if std::iter::once(&density)
                    .chain(momentum.iter())
                    .chain(second.iter().flatten())
                    .any(|v| !v.value().is_finite())
                {
                    return Err(fail("pre-collision physical port moments overflow"));
                }
                Ok(PortPhysicalMoments {
                    density_kg_m3: density,
                    momentum_density_kg_m2_s: momentum,
                    second_moment_pa: second,
                })
            })
            .collect()
    }

    pub fn pressure_mass_jacobian(&self) -> Vec<f64> {
        self.ports
            .iter()
            .map(|port| {
                self.pressure_scale_pa / self.mass_scale_kg
                    * port.iter().map(|(_, w)| w * w).sum::<f64>()
            })
            .collect()
    }

    pub fn apply<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[S],
        increments: &[[S; 4]],
    ) -> CaeResult<Vec<S>> {
        self.check_state::<S, Q, L>(state)?;
        if increments.len() != self.port_count()
            || increments.iter().flatten().any(|v| !v.value().is_finite())
        {
            return Err(fail(
                "port exchange requires finite mass and momentum increments per port",
            ));
        }
        let mut result = state.to_vec();
        for (port, delta) in self.ports.iter().zip(increments) {
            for &(cell, weight) in port {
                let mass = delta[0] * (weight / self.mass_scale_kg);
                for i in 0..Q {
                    let momentum =
                        (delta[1] * L::CF[i][0] + delta[2] * L::CF[i][1] + delta[3] * L::CF[i][2])
                            * (3.0 * weight / self.momentum_scale_kg_m_s);
                    let value = result[i * self.cells + cell] + (mass + momentum) * L::W[i];
                    if !value.value().is_finite() || value.value() <= 0.0 {
                        return Err(CaeError::convergence(
                            "port exchange requires positive finite populations",
                        ));
                    }
                    result[i * self.cells + cell] = value;
                }
                let rho = (0..Q)
                    .map(|i| result[i * self.cells + cell].value())
                    .sum::<f64>();
                if !rho.is_finite() || rho <= 0.0 {
                    return Err(CaeError::convergence(
                        "port exchange would exhaust a bulk fluid cell",
                    ));
                }
            }
        }
        Ok(result)
    }

    fn incoming_rows<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        rows: &[(usize, Vec<S>)],
    ) -> CaeResult<()> {
        Self::check_lattice::<Q, L>()?;
        if rows.windows(2).any(|w| w[0].0 >= w[1].0)
            || rows.iter().any(|(cell, row)| {
                *cell >= self.cells || row.len() != Q || row.iter().any(|v| !v.value().is_finite())
            })
            || self
                .ports
                .iter()
                .flatten()
                .any(|(cell, _)| rows.binary_search_by_key(cell, |(i, _)| *i).is_err())
        {
            return Err(fail("incoming port rows must cover all exchange cells"));
        }
        Ok(())
    }

    pub fn apply_incoming<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        rows: &[(usize, Vec<S>)],
        increments: &[[S; 4]],
    ) -> CaeResult<Vec<(usize, Vec<S>)>> {
        self.incoming_rows::<S, Q, L>(rows)?;
        if increments.len() != self.port_count()
            || increments.iter().flatten().any(|v| !v.value().is_finite())
        {
            return Err(fail("invalid incoming port increments"));
        }
        let mut result = rows.to_vec();
        for (port, delta) in self.ports.iter().zip(increments) {
            for &(cell, weight) in port {
                let index = result.binary_search_by_key(&cell, |(i, _)| *i).unwrap();
                let mass = delta[0] * (weight / self.mass_scale_kg);
                for q in 0..Q {
                    let momentum = (0..3).map(|a| delta[a + 1] * L::CF[q][a]).sum::<S>()
                        * (3. * weight / self.momentum_scale_kg_m_s);
                    result[index].1[q] += (mass + momentum) * L::W[q];
                    if !result[index].1[q].value().is_finite() || result[index].1[q].value() <= 0. {
                        return Err(CaeError::convergence(
                            "incoming port populations must remain positive",
                        ));
                    }
                }
            }
        }
        Ok(result)
    }

    fn incoming_moment_weights<const Q: usize, L: Lattice<Q>>(
        &self,
        q: usize,
        weight: f64,
    ) -> [f64; 10] {
        let c = L::CF[q];
        let factor = weight * L::W[q];
        let speed = self.native_speed_m_s;
        let mut map = [0.; 10];
        map[0] = factor * (2.5 - 1.5 * c.iter().map(|v| v * v).sum::<f64>()) / self.mass_scale_kg;
        for a in 0..3 {
            map[1 + a] = factor * 3. * c[a] / self.momentum_scale_kg_m_s;
        }
        for (i, (a, b)) in [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)]
            .into_iter()
            .enumerate()
        {
            map[4 + i] = factor
                * 4.5
                * (c[a] * c[b] - if a == b { 1. / 3. } else { 0. })
                * if a == b { 1. } else { 2. }
                / (self.mass_scale_kg * speed * speed);
        }
        map
    }

    pub fn apply_incoming_moments<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        rows: &[(usize, Vec<S>)],
        increments: &[[S; 10]],
    ) -> CaeResult<Vec<(usize, Vec<S>)>> {
        Self::check_stress_lattice::<Q, L>()?;
        self.incoming_rows::<S, Q, L>(rows)?;
        if increments.len() != self.port_count()
            || increments.iter().flatten().any(|v| !v.value().is_finite())
        {
            return Err(fail("invalid incoming moment increments"));
        }
        let mut result = rows.to_vec();
        for (port, delta) in self.ports.iter().zip(increments) {
            for &(cell, weight) in port {
                let index = result.binary_search_by_key(&cell, |(i, _)| *i).unwrap();
                for q in 0..Q {
                    let map = self.incoming_moment_weights::<Q, L>(q, weight);
                    result[index].1[q] += (0..10).map(|i| delta[i] * map[i]).sum::<S>();
                    if !result[index].1[q].value().is_finite() || result[index].1[q].value() <= 0. {
                        return Err(CaeError::convergence(
                            "incoming moment populations must remain positive",
                        ));
                    }
                }
            }
        }
        Ok(result)
    }

    pub fn apply_incoming_moments_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        population_bar: &[(usize, Vec<f64>)],
    ) -> CaeResult<(Vec<(usize, Vec<f64>)>, Vec<[f64; 10]>)> {
        Self::check_stress_lattice::<Q, L>()?;
        self.incoming_rows::<f64, Q, L>(population_bar)?;
        let mut increments = vec![[0.; 10]; self.port_count()];
        for (port, bar) in self.ports.iter().zip(&mut increments) {
            for &(cell, weight) in port {
                let row = &population_bar[population_bar
                    .binary_search_by_key(&cell, |(i, _)| *i)
                    .unwrap()]
                .1;
                for q in 0..Q {
                    let map = self.incoming_moment_weights::<Q, L>(q, weight);
                    for i in 0..10 {
                        bar[i] += row[q] * map[i];
                    }
                }
            }
        }
        if increments.iter().flatten().any(|v| !v.is_finite()) {
            return Err(fail("incoming moment transpose overflow"));
        }
        Ok((population_bar.to_vec(), increments))
    }

    pub fn apply_incoming_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        population_bar: &[(usize, Vec<f64>)],
    ) -> CaeResult<(Vec<(usize, Vec<f64>)>, Vec<[f64; 4]>)> {
        self.incoming_rows::<f64, Q, L>(population_bar)?;
        let mut increments = vec![[0.; 4]; self.port_count()];
        for (port, bar) in self.ports.iter().zip(&mut increments) {
            for &(cell, weight) in port {
                let row = &population_bar[population_bar
                    .binary_search_by_key(&cell, |(i, _)| *i)
                    .unwrap()]
                .1;
                for q in 0..Q {
                    let b = row[q] * weight * L::W[q];
                    bar[0] += b / self.mass_scale_kg;
                    for a in 0..3 {
                        bar[a + 1] += b * 3. * L::CF[q][a] / self.momentum_scale_kg_m_s;
                    }
                }
            }
        }
        if increments.iter().flatten().any(|v| !v.is_finite()) {
            return Err(fail("incoming port transpose overflow"));
        }
        Ok((population_bar.to_vec(), increments))
    }

    pub fn apply_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        population_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<[f64; 4]>)> {
        self.check_state::<f64, Q, L>(population_bar)?;
        let mut increments = vec![[0.0; 4]; self.port_count()];
        for (port, bar) in self.ports.iter().zip(&mut increments) {
            for &(cell, weight) in port {
                for i in 0..Q {
                    let b = population_bar[i * self.cells + cell] * weight * L::W[i];
                    bar[0] += b / self.mass_scale_kg;
                    for a in 0..3 {
                        bar[a + 1] += b * 3.0 * L::CF[i][a] / self.momentum_scale_kg_m_s;
                    }
                }
            }
        }
        if increments.iter().flatten().any(|x| !x.is_finite()) {
            return Err(fail("port exchange transpose overflow"));
        }
        Ok((population_bar.to_vec(), increments))
    }

    pub fn macroscopic_states_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[f64],
        reference_pressure_pa: f64,
        bars: &[PortMacroscopicState<f64>],
    ) -> CaeResult<Vec<f64>> {
        let values = self.macroscopic_states::<f64, Q, L>(state, reference_pressure_pa)?;
        if bars.len() != values.len()
            || bars.iter().any(|b| {
                !b.density_kg_m3.is_finite()
                    || !b.absolute_pressure_pa.is_finite()
                    || b.population_velocity_m_s.iter().any(|v| !v.is_finite())
            })
        {
            return Err(fail("port macroscopic cotangent shape or value"));
        }
        let mut out = vec![0.; state.len()];
        for ((port, value), bar) in self.ports.iter().zip(values).zip(bars) {
            let mut density = 0.;
            for &(cell, weight) in port {
                for q in 0..Q {
                    density += state[q * self.cells + cell] * weight;
                }
            }
            let density_bar = bar.density_kg_m3 * self.reference_density_kg_m3
                + bar.absolute_pressure_pa * self.pressure_scale_pa
                - (0..3)
                    .map(|a| {
                        bar.population_velocity_m_s[a] * value.population_velocity_m_s[a] / density
                    })
                    .sum::<f64>();
            let momentum_bar = bar
                .population_velocity_m_s
                .map(|b| b * self.native_speed_m_s / density);
            for &(cell, weight) in port {
                for q in 0..Q {
                    out[q * self.cells + cell] += weight
                        * (density_bar
                            + (0..3).map(|a| momentum_bar[a] * L::CF[q][a]).sum::<f64>());
                }
            }
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(fail("port macroscopic transpose overflow"));
        }
        Ok(out)
    }

    pub fn pressures_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        pressure_bar: &[f64],
    ) -> CaeResult<Vec<f64>> {
        Self::check_lattice::<Q, L>()?;
        if pressure_bar.len() != self.port_count() || pressure_bar.iter().any(|x| !x.is_finite()) {
            return Err(fail("port pressure cotangent shape mismatch"));
        }
        let size = self
            .cells
            .checked_mul(Q)
            .ok_or_else(|| fail("port pressure state size overflow"))?;
        let mut state_bar = vec![0.0; size];
        for (port, bar) in self.ports.iter().zip(pressure_bar) {
            for &(cell, weight) in port {
                for i in 0..Q {
                    state_bar[i * self.cells + cell] += bar * weight * self.pressure_scale_pa;
                }
            }
        }
        if state_bar.iter().any(|x| !x.is_finite()) {
            return Err(fail("port pressure transpose overflow"));
        }
        Ok(state_bar)
    }

    pub fn exchanged_moments<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        before: &[S],
        after: &[S],
        velocity_offsets_m_s: &[[S; 3]],
    ) -> CaeResult<[S; 5]> {
        self.check_state::<S, Q, L>(before)?;
        self.check_state::<S, Q, L>(after)?;
        if velocity_offsets_m_s.len() != self.port_count()
            || velocity_offsets_m_s
                .iter()
                .flatten()
                .any(|v| !v.value().is_finite())
        {
            return Err(fail(
                "kinetic exchange physical velocity offset shape or value",
            ));
        }
        let mut out = [S::zero(); 5];
        for (port, offset) in self.ports.iter().zip(velocity_offsets_m_s) {
            for &(cell, _) in port {
                for q in 0..Q {
                    let delta = after[q * self.cells + cell] - before[q * self.cells + cell];
                    out[0] += delta * self.mass_scale_kg;
                    let velocity: [S; 3] = std::array::from_fn(|axis| {
                        S::from_f64(L::CF[q][axis] * self.native_speed_m_s) + offset[axis]
                    });
                    for axis in 0..3 {
                        out[axis + 1] += delta * velocity[axis] * self.mass_scale_kg;
                    }
                    let speed_squared = velocity
                        .iter()
                        .map(|v| *v * *v)
                        .fold(S::zero(), |a, b| a + b);
                    out[4] += delta * speed_squared * (0.5 * self.mass_scale_kg);
                }
            }
        }
        if out.iter().any(|v| !v.value().is_finite()) {
            return Err(fail("kinetic exchange moment difference overflow"));
        }
        Ok(out)
    }

    pub fn moments<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[S],
    ) -> CaeResult<[S; 4]> {
        self.check_state::<S, Q, L>(state)?;
        let mut out = [S::zero(); 4];
        for cell in 0..self.cells {
            if self.excluded.binary_search(&cell).is_ok() {
                continue;
            }
            for i in 0..Q {
                let f = state[i * self.cells + cell];
                out[0] += f * self.mass_scale_kg;
                for a in 0..3 {
                    out[a + 1] += f * (L::CF[i][a] * self.momentum_scale_kg_m_s);
                }
            }
        }
        Ok(out)
    }
}

fn equilibrium_cell_tensor<S: Scalar, const Q: usize, L: Lattice<Q>>(f: &[S; Q]) -> CaeResult<[[S; 3]; 3]> {
    let (rho, j) = moments::<S, Q, L>(f);
    if !rho.value().is_finite() || rho.value() <= 0. {
        return Err(fail("equilibrium port cell density must be positive and finite"));
    }
    Ok(std::array::from_fn(|a| std::array::from_fn(|b| {
        j[a] * j[b] / rho + if a == b { rho / 3. } else { S::zero() }
    })))
}
