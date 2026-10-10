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

use crate::gas_transient_kinetic::{
    TransientGasPacket, TransientGasPolicy, TransientGasScalar, TransientVelocityGrid,
    velocity_equilibrium_density,
};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{lattice::Lattice, port_exchange::PortExchange};

#[derive(Clone, Debug)]
pub struct NativeGasPortConnector {
    exchange: PortExchange,
    mass_scale_kg: f64,
    native_speed_m_s: f64,
    reference_pressure_pa: f64,
    reference_density_kg_m3: f64,
}

#[derive(Clone, Debug)]
pub struct NativeGasPortIncoming<S> {
    pub incoming_rows: Vec<(usize, Vec<S>)>,
    pub exchange_mass_momentum_defect: [S; 4],
    pub projection_heat_j: S,
}

impl NativeGasPortConnector {
    pub fn new(
        exchange: PortExchange,
        spacing_m: f64,
        density_kg_m3: f64,
        native_step_s: f64,
        reference_pressure_pa: f64,
    ) -> CaeResult<Self> {
        exchange.validate_native_scales(spacing_m, density_kg_m3, native_step_s)?;
        let speed = spacing_m / native_step_s;
        let pressure = density_kg_m3 * speed * speed / 3.;
        if !reference_pressure_pa.is_finite()
            || reference_pressure_pa <= 0.
            || (pressure - reference_pressure_pa).abs()
                > 32. * f64::EPSILON * pressure.max(reference_pressure_pa)
        {
            return Err(CaeError::contract(
                "native and molecular absolute pressure scales differ",
            ));
        }
        Ok(Self {
            exchange,
            mass_scale_kg: density_kg_m3 * spacing_m.powi(3),
            native_speed_m_s: speed,
            reference_pressure_pa,
            reference_density_kg_m3: density_kg_m3,
        })
    }

    pub fn port_subset(&self, indices: &[usize]) -> CaeResult<Self> {
        Ok(Self {
            exchange: self.exchange.port_subset(indices)?,
            mass_scale_kg: self.mass_scale_kg,
            native_speed_m_s: self.native_speed_m_s,
            reference_pressure_pa: self.reference_pressure_pa,
            reference_density_kg_m3: self.reference_density_kg_m3,
        })
    }

    pub fn reference_pressure_pa(&self) -> f64 {
        self.reference_pressure_pa
    }

    pub fn exchange(&self) -> &PortExchange {
        &self.exchange
    }

    pub fn port_distributions<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        bulk_populations: &[S],
        offsets: &[[S; 3]],
        grid: &TransientVelocityGrid,
        gas_rt: S,
        policy: &TransientGasPolicy,
    ) -> CaeResult<Vec<Vec<S>>> {
        if offsets.len() != self.exchange.port_count()
            || offsets.iter().flatten().any(|x| !x.finite_first_order())
        {
            return Err(CaeError::contract(
                "native port velocity offset shape or value",
            ));
        }
        self.exchange
            .macroscopic_states::<S, Q, L>(bulk_populations, self.reference_pressure_pa)?
            .into_iter()
            .zip(offsets)
            .map(|(state, offset)| {
                let rt = state.absolute_pressure_pa / state.density_kg_m3;
                let difference = gas_rt - rt;
                let tolerance = 128. * f64::EPSILON * gas_rt.value().abs();
                if !difference.finite_first_order()
                    || difference.value().abs() > tolerance
                    || difference.maximum_direction() > tolerance
                {
                    return Err(CaeError::contract(
                        "native and molecular temperature scales or directions differ",
                    ));
                }
                velocity_equilibrium_density(
                    grid,
                    state.density_kg_m3,
                    std::array::from_fn(|a| state.population_velocity_m_s[a] + offset[a]),
                    gas_rt,
                    policy,
                )
            })
            .collect()
    }

    pub fn replace_incoming<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        rows: &[(usize, Vec<S>)],
        packets: &[TransientGasPacket<S>],
        reflections: &[(usize, [S; 3])],
        offsets: &[[S; 3]],
    ) -> CaeResult<NativeGasPortIncoming<S>> {
        let fail = |s: &str| CaeError::contract(s);
        if packets.len() != self.exchange.port_count()
            || offsets.len() != packets.len()
            || rows.windows(2).any(|w| w[0].0 >= w[1].0)
            || reflections.windows(2).any(|w| w[0].0 >= w[1].0)
            || offsets.iter().flatten().any(|x| !x.finite_first_order())
            || reflections
                .iter()
                .flat_map(|x| x.1)
                .any(|x| !x.finite_first_order())
            || packets.iter().any(|p| {
                !p.mass_kg.finite_first_order()
                    || !p.kinetic_energy_j.finite_first_order()
                    || p.momentum_kg_m_s.iter().any(|x| !x.finite_first_order())
            })
        {
            return Err(fail("native incoming packet shape or value"));
        }
        let mut raw = Vec::with_capacity(packets.len());
        for ((port, packet), offset) in self.exchange.ports().iter().zip(packets).zip(offsets) {
            let mut delta = [packet.mass_kg, S::zero(), S::zero(), S::zero()];
            for a in 0..3 {
                delta[a + 1] = packet.momentum_kg_m_s[a] - packet.mass_kg * offset[a];
            }
            for &(cell, weight) in port {
                if let Ok(i) = reflections.binary_search_by_key(&cell, |x| x.0) {
                    if port.len() != 1 || weight != 1. {
                        return Err(fail(
                            "reflection cancellation requires a separate native port for each cell",
                        ));
                    }
                    for a in 0..3 {
                        delta[a + 1] -= reflections[i].1[a];
                    }
                }
            }
            raw.push(delta);
        }
        if reflections.iter().any(|(cell, _)| {
            !self
                .exchange
                .ports()
                .iter()
                .flatten()
                .any(|(i, _)| i == cell)
        }) {
            return Err(fail("native reflected link lies outside exchange ports"));
        }
        let result = self.exchange.apply_incoming::<S, Q, L>(rows, &raw)?;
        let mut actual = [S::zero(); 5];
        for (port, offset) in self.exchange.ports().iter().zip(offsets) {
            for &(cell, _) in port {
                let old = &rows[rows
                    .binary_search_by_key(&cell, |x| x.0)
                    .map_err(|_| fail("native incoming cell absent"))?]
                .1;
                let new = &result[result.binary_search_by_key(&cell, |x| x.0).unwrap()].1;
                for q in 0..Q {
                    let mass = (new[q] - old[q]) * self.mass_scale_kg;
                    actual[0] += mass;
                    let mut speed2 = S::zero();
                    for a in 0..3 {
                        let velocity = offset[a] + L::CF[q][a] * self.native_speed_m_s;
                        actual[a + 1] += mass * velocity;
                        speed2 += velocity * velocity;
                    }
                    actual[4] += mass * speed2 * 0.5;
                }
                if let Ok(i) = reflections.binary_search_by_key(&cell, |x| x.0) {
                    for a in 0..3 {
                        actual[a + 1] += reflections[i].1[a];
                        actual[4] += offset[a] * reflections[i].1[a];
                    }
                }
            }
        }
        let mut expected = [S::zero(); 5];
        for p in packets {
            expected[0] += p.mass_kg;
            for a in 0..3 {
                expected[a + 1] += p.momentum_kg_m_s[a];
            }
            expected[4] += p.kinetic_energy_j;
        }
        let defect = std::array::from_fn(|a| actual[a] - expected[a]);
        let heat = expected[4] - actual[4];
        if defect
            .iter()
            .chain([&heat])
            .any(|x| !x.finite_first_order())
        {
            return Err(fail("native incoming exchange balance overflow"));
        }
        Ok(NativeGasPortIncoming {
            incoming_rows: result,
            exchange_mass_momentum_defect: defect,
            projection_heat_j: heat,
        })
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasPortIncomingBars {
    pub incoming_rows: Vec<(usize, Vec<f64>)>,
    pub packet_mass_momentum_energy: Vec<[f64; 5]>,
    pub reflections: Vec<(usize, [f64; 3])>,
    pub velocity_offsets: Vec<[f64; 3]>,
}

impl NativeGasPortConnector {
    pub fn replace_incoming_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        rows: &[(usize, Vec<f64>)],
        packets: &[TransientGasPacket<f64>],
        reflections: &[(usize, [f64; 3])],
        offsets: &[[f64; 3]],
        row_bar: &[(usize, Vec<f64>)],
        defect_bar: [f64; 4],
        heat_bar: f64,
    ) -> CaeResult<NativeGasPortIncomingBars> {
        let out = self.replace_incoming::<f64, Q, L>(rows, packets, reflections, offsets)?;
        if row_bar.len() != rows.len()
            || row_bar.iter().zip(rows).any(|((i, b), (j, r))| {
                i != j || b.len() != r.len() || b.iter().any(|v| !v.is_finite())
            })
            || defect_bar.iter().chain([&heat_bar]).any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "native incoming transpose shape or value",
            ));
        }
        let mut packet_bar = vec![[0.; 5]; packets.len()];
        let mut reflection_bar: Vec<_> = reflections.iter().map(|(i, _)| (*i, [0.; 3])).collect();
        let mut offset_bar = vec![[0.; 3]; offsets.len()];
        for (k, (port, offset)) in self.exchange.ports().iter().zip(offsets).enumerate() {
            let mut raw_bar = [0.; 4];
            for &(cell, weight) in port {
                let i = rows.binary_search_by_key(&cell, |x| x.0).unwrap();
                for q in 0..Q {
                    let velocity = std::array::from_fn::<_, 3, _>(|a| {
                        offset[a] + L::CF[q][a] * self.native_speed_m_s
                    });
                    let speed2 = velocity.iter().map(|v| v * v).sum::<f64>();
                    let effective = row_bar[i].1[q]
                        + self.mass_scale_kg
                            * (defect_bar[0]
                                + (0..3).map(|a| defect_bar[a + 1] * velocity[a]).sum::<f64>()
                                - 0.5 * heat_bar * speed2);
                    raw_bar[0] += effective * weight * L::W[q] / self.mass_scale_kg;
                    let mass = (out.incoming_rows[i].1[q] - rows[i].1[q]) * self.mass_scale_kg;
                    for a in 0..3 {
                        raw_bar[a + 1] += effective * 3. * weight * L::W[q] * L::CF[q][a]
                            / (self.mass_scale_kg * self.native_speed_m_s);
                        offset_bar[k][a] += mass * (defect_bar[a + 1] - heat_bar * velocity[a]);
                    }
                }
                if let Ok(j) = reflections.binary_search_by_key(&cell, |x| x.0) {
                    for a in 0..3 {
                        reflection_bar[j].1[a] += defect_bar[a + 1] - heat_bar * offset[a];
                        offset_bar[k][a] -= heat_bar * reflections[j].1[a];
                    }
                }
            }
            packet_bar[k][0] = raw_bar[0]
                - defect_bar[0]
                - (0..3).map(|a| raw_bar[a + 1] * offset[a]).sum::<f64>();
            for a in 0..3 {
                packet_bar[k][a + 1] = raw_bar[a + 1] - defect_bar[a + 1];
                offset_bar[k][a] -= packets[k].mass_kg * raw_bar[a + 1];
            }
            packet_bar[k][4] = heat_bar;
            for &(cell, _) in port {
                if let Ok(j) = reflections.binary_search_by_key(&cell, |x| x.0) {
                    for a in 0..3 {
                        reflection_bar[j].1[a] -= raw_bar[a + 1];
                    }
                }
            }
        }
        if packet_bar
            .iter()
            .flatten()
            .chain(offset_bar.iter().flatten())
            .chain(reflection_bar.iter().flat_map(|x| x.1.iter()))
            .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("native incoming transpose overflow"));
        }
        Ok(NativeGasPortIncomingBars {
            incoming_rows: row_bar.to_vec(),
            packet_mass_momentum_energy: packet_bar,
            reflections: reflection_bar,
            velocity_offsets: offset_bar,
        })
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasPortDistributionBars {
    pub populations: Vec<(usize, Vec<f64>)>,
    pub velocity_offsets: Vec<[f64; 3]>,
}

impl NativeGasPortConnector {
    pub fn port_distributions_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        bulk_populations: &[f64],
        offsets: &[[f64; 3]],
        grid: &TransientVelocityGrid,
        gas_rt: f64,
        policy: &TransientGasPolicy,
        density_bar: &[Vec<f64>],
    ) -> CaeResult<NativeGasPortDistributionBars> {
        use implexity_ad::Dual;
        self.port_distributions::<f64, Q, L>(bulk_populations, offsets, grid, gas_rt, policy)?;
        if density_bar.len() != self.exchange.port_count()
            || density_bar
                .iter()
                .any(|b| b.len() != grid.velocity_m_s.len() || b.iter().any(|v| !v.is_finite()))
        {
            return Err(CaeError::contract(
                "native port equilibrium transpose shape or value",
            ));
        }
        let states = self
            .exchange
            .macroscopic_states::<f64, Q, L>(bulk_populations, self.reference_pressure_pa)?;
        let mut populations = Vec::new();
        let mut offset_bars = Vec::new();
        for (((state, offset), bar), port) in states
            .iter()
            .zip(offsets)
            .zip(density_bar)
            .zip(self.exchange.ports())
        {
            let density = Dual::new(state.density_kg_m3, [1., 0., 0., 0.]);
            let velocity = std::array::from_fn(|a| {
                let mut direction = [0.; 4];
                direction[a + 1] = 1.;
                Dual::new(state.population_velocity_m_s[a] + offset[a], direction)
            });
            let equilibrium = velocity_equilibrium_density(
                grid,
                density,
                velocity,
                Dual::new(gas_rt, [0.; 4]),
                policy,
            )?;
            let mut moments_bar = [0.; 4];
            for (value, &bar) in equilibrium.iter().zip(bar) {
                for a in 0..4 {
                    moments_bar[a] += value.eps[a] * bar;
                }
            }
            let rho = state.density_kg_m3 / self.reference_density_kg_m3;
            offset_bars.push(std::array::from_fn(|a| moments_bar[a + 1]));
            for &(cell, weight) in port {
                let row = (0..Q)
                    .map(|q| {
                        weight
                            * (self.reference_density_kg_m3 * moments_bar[0]
                                + self.native_speed_m_s / rho
                                    * (0..3)
                                        .map(|a| {
                                            moments_bar[a + 1]
                                                * (L::CF[q][a]
                                                    - state.population_velocity_m_s[a]
                                                        / self.native_speed_m_s)
                                        })
                                        .sum::<f64>())
                    })
                    .collect::<Vec<_>>();
                populations.push((cell, row));
            }
        }
        populations.sort_unstable_by_key(|x| x.0);
        if populations
            .iter()
            .flat_map(|x| x.1.iter())
            .chain(offset_bars.iter().flatten())
            .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "native port equilibrium transpose overflow",
            ));
        }
        Ok(NativeGasPortDistributionBars {
            populations,
            velocity_offsets: offset_bars,
        })
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasReferencePackets<S> {
    pub packets: Vec<TransientGasPacket<S>>,
    pub reference_momentum_defect_kg_m_s: [S; 3],
}

impl NativeGasPortConnector {
    pub fn reference_projection<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        packets: &[TransientGasPacket<S>],
        reference_face_impulses: &[[S; 3]],
        links: &[implexity_physics_lbm::moving::field::gas_tick::CoupledInterfaceLink],
        interface_reference_pressure_pa: S,
    ) -> CaeResult<NativeGasReferencePackets<S>> {
        use std::collections::{BTreeMap, BTreeSet};
        if packets.len() != self.exchange.port_count()
            || reference_face_impulses.len() != packets.len()
            || reference_face_impulses
                .iter()
                .flatten()
                .any(|v| !v.finite_first_order())
            || !interface_reference_pressure_pa.finite_first_order()
            || interface_reference_pressure_pa.value() <= 0.
            || self
                .exchange
                .ports()
                .iter()
                .any(|p| p.len() != 1 || p[0].1 != 1.)
        {
            return Err(CaeError::contract(
                "native interface reference projection shape or value",
            ));
        }
        let lookup: BTreeMap<_, _> = self
            .exchange
            .ports()
            .iter()
            .enumerate()
            .map(|(i, p)| (p[0].0, i))
            .collect();
        let mut result = packets.to_vec();
        let mut defect = [S::zero(); 3];
        let mut scale = [0.; 3];
        let mut direction_scale = [0.; 3];
        for (packet, reference) in result.iter_mut().zip(reference_face_impulses) {
            for a in 0..3 {
                packet.momentum_kg_m_s[a] -= reference[a];
                defect[a] -= reference[a];
                scale[a] += reference[a].value().abs();
                direction_scale[a] += reference[a].maximum_direction();
            }
        }
        let mut seen = BTreeSet::new();
        for link in links {
            let q = link.outgoing_population();
            let port = lookup.get(&link.cell()).ok_or_else(|| {
                CaeError::contract("native reference link lacks a bookkeeping port")
            })?;
            if q >= Q || !seen.insert((link.cell(), q)) {
                return Err(CaeError::contract(
                    "native reference link duplicate or population index",
                ));
            }
            for a in 0..3 {
                let baseline = interface_reference_pressure_pa
                    * (-2. * self.mass_scale_kg * self.native_speed_m_s * L::W[q] * L::CF[q][a]
                        / self.reference_pressure_pa);
                result[*port].momentum_kg_m_s[a] += baseline;
                defect[a] += baseline;
                scale[a] += baseline.value().abs();
                direction_scale[a] += baseline.maximum_direction();
            }
        }
        if (0..3).any(|a| {
            !defect[a].finite_first_order()
                || defect[a].value().abs() > 512. * f64::EPSILON * scale[a]
                || defect[a].maximum_direction() > 512. * f64::EPSILON * direction_scale[a]
        }) {
            return Err(CaeError::contract(
                "native and finite-volume reference traction resultants differ",
            ));
        }
        Ok(NativeGasReferencePackets {
            packets: result,
            reference_momentum_defect_kg_m_s: defect,
        })
    }

    pub fn reference_projection_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        links: &[implexity_physics_lbm::moving::field::gas_tick::CoupledInterfaceLink],
        packet_bar: &[[f64; 5]],
    ) -> CaeResult<(Vec<[f64; 3]>, f64)> {
        use std::collections::{BTreeMap, BTreeSet};
        if packet_bar.len() != self.exchange.port_count()
            || packet_bar.iter().flatten().any(|v| !v.is_finite())
            || self
                .exchange
                .ports()
                .iter()
                .any(|p| p.len() != 1 || p[0].1 != 1.)
        {
            return Err(CaeError::contract(
                "native reference transpose shape or value",
            ));
        }
        let lookup: BTreeMap<_, _> = self
            .exchange
            .ports()
            .iter()
            .enumerate()
            .map(|(i, p)| (p[0].0, i))
            .collect();
        let mut pressure_bar = 0.;
        let mut seen = BTreeSet::new();
        for link in links {
            let q = link.outgoing_population();
            let port = lookup
                .get(&link.cell())
                .ok_or_else(|| CaeError::contract("native reference transpose link lacks port"))?;
            if q >= Q || !seen.insert((link.cell(), q)) {
                return Err(CaeError::contract(
                    "native reference transpose invalid link",
                ));
            }
            for a in 0..3 {
                pressure_bar += packet_bar[*port][a + 1]
                    * (-2. * self.mass_scale_kg * self.native_speed_m_s * L::W[q] * L::CF[q][a]
                        / self.reference_pressure_pa);
            }
        }
        let references = packet_bar.iter().map(|b| [-b[1], -b[2], -b[3]]).collect();
        if !pressure_bar.is_finite() {
            return Err(CaeError::contract("native reference transpose overflow"));
        }
        Ok((references, pressure_bar))
    }
}

impl NativeGasPortConnector {
    pub fn interface_reference_pressure<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        bulk_populations: &[S],
    ) -> CaeResult<S> {
        let states = self
            .exchange
            .macroscopic_states::<S, Q, L>(bulk_populations, self.reference_pressure_pa)?;
        let value =
            states.iter().map(|s| s.absolute_pressure_pa).sum::<S>() / (states.len() as f64);
        if !value.finite_first_order() || value.value() <= 0. {
            return Err(CaeError::contract(
                "native interface reference pressure is not positive and finite",
            ));
        }
        Ok(value)
    }

    pub fn interface_reference_pressure_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        pressure_bar: f64,
    ) -> CaeResult<Vec<(usize, Vec<f64>)>> {
        if !pressure_bar.is_finite() {
            return Err(CaeError::contract(
                "native interface pressure transpose value",
            ));
        }
        let factor =
            pressure_bar * self.reference_pressure_pa / (self.exchange.port_count() as f64);
        let mut rows = self
            .exchange
            .ports()
            .iter()
            .flat_map(|p| p.iter().map(|&(cell, w)| (cell, vec![factor * w; Q])))
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|x| x.0);
        if rows.iter().flat_map(|x| x.1.iter()).any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "native interface pressure transpose overflow",
            ));
        }
        Ok(rows)
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasMomentIncoming<S> {
    pub incoming_rows: Vec<(usize, Vec<S>)>,
    pub exchange_moment_defect: [S; 10],
    pub projection_heat_j: S,
}

impl NativeGasPortConnector {
    pub fn replace_incoming_moments<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        rows: &[(usize, Vec<S>)],
        packets: &[[S; 10]],
        reflections: &[(usize, [S; 10])],
        offsets: &[[S; 3]],
    ) -> CaeResult<NativeGasMomentIncoming<S>> {
        if packets.len() != self.exchange.port_count()
            || offsets.len() != packets.len()
            || rows
                .iter()
                .flat_map(|r| &r.1)
                .any(|v| !v.finite_first_order())
            || reflections.windows(2).any(|w| w[0].0 >= w[1].0)
            || offsets
                .iter()
                .flatten()
                .chain(packets.iter().flatten())
                .chain(reflections.iter().flat_map(|r| r.1.iter()))
                .any(|v| !v.finite_first_order())
        {
            return Err(CaeError::contract("native moment packet shape or value"));
        }
        let pairs = [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)];
        let mut raw = Vec::with_capacity(packets.len());
        for ((port, packet), offset) in self.exchange.ports().iter().zip(packets).zip(offsets) {
            let mut delta = *packet;
            for a in 0..3 {
                delta[1 + a] -= packet[0] * offset[a];
            }
            for (i, (a, b)) in pairs.into_iter().enumerate() {
                delta[4 + i] += packet[0] * offset[a] * offset[b]
                    - offset[a] * packet[1 + b]
                    - offset[b] * packet[1 + a];
            }
            for &(cell, weight) in port {
                if let Ok(i) = reflections.binary_search_by_key(&cell, |r| r.0) {
                    if port.len() != 1 || weight != 1. {
                        return Err(CaeError::contract(
                            "reflection cancellation requires separate native cell ports",
                        ));
                    }
                    for a in 0..10 {
                        delta[a] -= reflections[i].1[a];
                    }
                }
            }
            if delta.iter().any(|v| !v.finite_first_order()) {
                return Err(CaeError::contract("native moment transform overflow"));
            }
            raw.push(delta);
        }
        if reflections.iter().any(|(cell, _)| {
            !self
                .exchange
                .ports()
                .iter()
                .flatten()
                .any(|(i, _)| i == cell)
        }) {
            return Err(CaeError::contract(
                "native reflected cell lies outside exchange ports",
            ));
        }
        let result = self
            .exchange
            .apply_incoming_moments::<S, Q, L>(rows, &raw)?;
        let mut actual = [S::zero(); 10];
        for (port, offset) in self.exchange.ports().iter().zip(offsets) {
            for &(cell, _) in port {
                let old = &rows[rows.binary_search_by_key(&cell, |r| r.0).unwrap()].1;
                let new = &result[result.binary_search_by_key(&cell, |r| r.0).unwrap()].1;
                for q in 0..Q {
                    let mass = (new[q] - old[q]) * self.mass_scale_kg;
                    let velocity: [S; 3] =
                        std::array::from_fn(|a| offset[a] + L::CF[q][a] * self.native_speed_m_s);
                    actual[0] += mass;
                    for a in 0..3 {
                        actual[1 + a] += mass * velocity[a];
                    }
                    for (i, (a, b)) in pairs.into_iter().enumerate() {
                        actual[4 + i] += mass * velocity[a] * velocity[b];
                    }
                }
                if let Ok(i) = reflections.binary_search_by_key(&cell, |r| r.0) {
                    let reflection = reflections[i].1;
                    actual[0] += reflection[0];
                    for a in 0..3 {
                        actual[1 + a] += reflection[1 + a] + reflection[0] * offset[a];
                    }
                    for (j, (a, b)) in pairs.into_iter().enumerate() {
                        actual[4 + j] += reflection[4 + j]
                            + offset[a] * reflection[1 + b]
                            + offset[b] * reflection[1 + a]
                            + reflection[0] * offset[a] * offset[b];
                    }
                }
            }
        }
        let mut expected = [S::zero(); 10];
        for packet in packets {
            for i in 0..10 {
                expected[i] += packet[i];
            }
        }
        let defect: [S; 10] = std::array::from_fn(|i| actual[i] - expected[i]);
        let heat = -(defect[4] + defect[5] + defect[6]) * 0.5;
        if defect
            .iter()
            .chain([&heat])
            .any(|v| !v.finite_first_order())
        {
            return Err(CaeError::contract(
                "native moment exchange balance overflow",
            ));
        }
        Ok(NativeGasMomentIncoming {
            incoming_rows: result,
            exchange_moment_defect: defect,
            projection_heat_j: heat,
        })
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasCollisionExchange<S> {
    pub mass_momentum_change: Vec<[S; 4]>,
    pub energy_to_thermostat_j: Vec<S>,
}

impl NativeGasPortConnector {
    pub fn collision_exchange<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        accepted_pre_collision: &[(usize, Vec<S>)],
        accepted_post_collision: &[(usize, Vec<S>)],
        offsets: &[[S; 3]],
    ) -> CaeResult<NativeGasCollisionExchange<S>> {
        if offsets.len() != self.exchange.port_count()
            || accepted_pre_collision.len() != accepted_post_collision.len()
            || accepted_pre_collision.windows(2).any(|w| w[0].0 >= w[1].0)
            || accepted_pre_collision
                .iter()
                .zip(accepted_post_collision)
                .any(|((cell, pre), (other, post))| {
                    *cell >= self.exchange.cell_count()
                        || cell != other
                        || pre.len() != Q
                        || post.len() != Q
                        || pre
                            .iter()
                            .chain(post)
                            .any(|v| !v.finite_first_order() || v.value() < 0.)
                })
            || offsets.iter().flatten().any(|v| !v.finite_first_order())
        {
            return Err(CaeError::contract("native collision ledger shape or value"));
        }
        let mut mass_momentum_change = Vec::with_capacity(offsets.len());
        let mut energy_to_thermostat_j = Vec::with_capacity(offsets.len());
        for (port, offset) in self.exchange.ports().iter().zip(offsets) {
            let mut change = [S::zero(); 4];
            let mut heat = S::zero();
            for &(cell, _) in port {
                let i = accepted_pre_collision
                    .binary_search_by_key(&cell, |r| r.0)
                    .map_err(|_| CaeError::contract("native collision cell absent"))?;
                for q in 0..Q {
                    let mass = (accepted_post_collision[i].1[q] - accepted_pre_collision[i].1[q])
                        * self.mass_scale_kg;
                    let velocity: [S; 3] =
                        std::array::from_fn(|a| offset[a] + L::CF[q][a] * self.native_speed_m_s);
                    change[0] += mass;
                    for a in 0..3 {
                        change[1 + a] += mass * velocity[a];
                    }
                    heat -= mass * velocity.into_iter().map(|v| v * v).sum::<S>() * 0.5;
                }
            }
            if change
                .iter()
                .chain([&heat])
                .any(|v| !v.finite_first_order())
            {
                return Err(CaeError::contract("native collision ledger overflow"));
            }
            mass_momentum_change.push(change);
            energy_to_thermostat_j.push(heat);
        }
        Ok(NativeGasCollisionExchange {
            mass_momentum_change,
            energy_to_thermostat_j,
        })
    }
}

#[derive(Clone, Debug)]
pub struct NativeGasReferenceMomentPackets<S> {
    pub packets: Vec<[S; 10]>,
    pub reference_moment_defect: [S; 10],
}
impl NativeGasPortConnector {
    pub fn reference_moment_projection<S: TransientGasScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        packets: &[[S; 10]],
        reference_face_impulses: &[[S; 3]],
        links: &[implexity_physics_lbm::moving::field::gas_tick::CoupledInterfaceLink],
        interface_reference_pressure_pa: S,
    ) -> CaeResult<NativeGasReferenceMomentPackets<S>> {
        if packets.iter().flatten().any(|v| !v.finite_first_order()) {
            return Err(CaeError::contract("native reference moment packet value"));
        }
        let legacy = packets
            .iter()
            .map(|v| TransientGasPacket {
                mass_kg: v[0],
                momentum_kg_m_s: [v[1], v[2], v[3]],
                kinetic_energy_j: S::zero(),
                wall_work_j: S::zero(),
                wall_heat_j: S::zero(),
            })
            .collect::<Vec<_>>();
        let projected = self.reference_projection::<S, Q, L>(
            &legacy,
            reference_face_impulses,
            links,
            interface_reference_pressure_pa,
        )?;
        let mut result = packets.to_vec();
        for (packet, legacy) in result.iter_mut().zip(projected.packets) {
            packet[1..4].copy_from_slice(&legacy.momentum_kg_m_s);
        }
        let mut defect = [S::zero(); 10];
        defect[1..4].copy_from_slice(&projected.reference_momentum_defect_kg_m_s);
        Ok(NativeGasReferenceMomentPackets {
            packets: result,
            reference_moment_defect: defect,
        })
    }
    pub fn reference_moment_projection_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        links: &[implexity_physics_lbm::moving::field::gas_tick::CoupledInterfaceLink],
        packet_bar: &[[f64; 10]],
    ) -> CaeResult<(Vec<[f64; 3]>, f64)> {
        if packet_bar.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "native reference moment transpose value",
            ));
        }
        let legacy = packet_bar
            .iter()
            .map(|v| [v[0], v[1], v[2], v[3], 0.])
            .collect::<Vec<_>>();
        self.reference_projection_vjp::<Q, L>(links, &legacy)
    }
}
