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

use crate::gas_gap_mass::GasMassScalar;
use crate::gas_quadratic_gaussian::QuadraticGaussianBasis;
use crate::gas_quadratic_velocity::{QuadraticEntropyPolicy, QuadraticVelocityBasis};
use crate::gas_vjp::{ReverseScalar, scoped_pullback};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::port_exchange::PortMacroscopicState;
use implexity_physics_lbm::moving::{lattice::Lattice, port_exchange::PortExchange};
use std::sync::Arc;

pub struct NativeGasStressTrace {
    exchange: Arc<PortExchange>,
    reference_pressure_pa: f64,
}
impl NativeGasStressTrace {
    pub fn new(exchange: Arc<PortExchange>, reference_pressure_pa: f64) -> CaeResult<Self> {
        if !reference_pressure_pa.is_finite() || reference_pressure_pa <= 0. {
            return Err(CaeError::contract(
                "physical port stress requires positive reference pressure",
            ));
        }
        Ok(Self {
            exchange,
            reference_pressure_pa,
        })
    }
    pub fn dimension(&self) -> usize {
        6 * self.exchange.port_count()
    }
    pub fn equilibrium_initial_trace<S: GasMassScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        populations: &[S],
    ) -> CaeResult<Vec<S>> {
        if populations.iter().any(|v| !v.finite_first_order()) {
            return Err(CaeError::contract(
                "initial port populations must be finite",
            ));
        }
        let states = self
            .exchange
            .equilibrium_physical_moments::<S, Q, L>(populations, self.reference_pressure_pa)?;
        let mut trace = Vec::with_capacity(self.dimension());
        for state in states {
            for (a, b) in [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)] {
                trace.push(state.second_moment_pa[a][b]);
            }
        }
        Ok(trace)
    }
    pub fn equilibrium_initial_trace_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        populations: &[f64],
        trace_bars: &[f64],
    ) -> CaeResult<Vec<f64>> {
        if trace_bars.len() != self.dimension() {
            return Err(CaeError::contract("initial stress cotangent shape"));
        }
        let bars = trace_bars.chunks_exact(6).map(|row| std::array::from_fn(|i| row[i])).collect::<Vec<_>>();
        self.exchange.equilibrium_second_moments_vjp::<Q, L>(populations, self.reference_pressure_pa, &bars)
    }
    pub fn accepted_pre_collision_trace<S: GasMassScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        rows: &[(usize, Vec<S>)],
        rates: &[(usize, S)],
    ) -> CaeResult<Vec<S>> {
        if rows
            .iter()
            .flat_map(|r| &r.1)
            .chain(rates.iter().map(|r| &r.1))
            .any(|v| !v.finite_first_order())
        {
            return Err(CaeError::contract(
                "accepted pre-collision trace must be finite",
            ));
        }
        let moments = self
            .exchange
            .zero_force_srt_pre_collision_moments_from_rows::<S, Q, L>(
                rows,
                rates,
                self.reference_pressure_pa,
            )?;
        let mut trace = Vec::with_capacity(self.dimension());
        for state in moments {
            for (a, b) in [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)] {
                trace.push(state.second_moment_pa[a][b]);
            }
        }
        Ok(trace)
    }
    pub fn physical_moments<S: GasMassScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        populations: &[S],
        trace: &[S],
        offsets: &[[S; 3]],
    ) -> CaeResult<Vec<[S; 10]>> {
        if trace.len() != self.dimension()
            || offsets.len() != self.exchange.port_count()
            || trace
                .iter()
                .chain(offsets.iter().flatten())
                .any(|v| !v.finite_first_order())
        {
            return Err(CaeError::contract(
                "physical port stress trace shape or value",
            ));
        }
        self.exchange
            .macroscopic_states::<S, Q, L>(populations, self.reference_pressure_pa)?
            .into_iter()
            .enumerate()
            .map(|(k, state)| {
                physical_moment_transform(
                    state.density_kg_m3,
                    state.population_velocity_m_s,
                    std::array::from_fn(|i| trace[6 * k + i]),
                    offsets[k],
                )
            })
            .collect()
    }
    pub fn coefficient_densities<S: GasMassScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        populations: &[S],
        trace: &[S],
        offsets: &[[S; 3]],
        basis: &QuadraticVelocityBasis,
        policy: &QuadraticEntropyPolicy,
    ) -> CaeResult<Vec<Vec<S>>> {
        self.physical_moments::<S, Q, L>(populations, trace, offsets)?
            .into_iter()
            .map(|moments| Ok(basis.moment_entropy(moments, policy)?.coefficient_density))
            .collect()
    }
    pub fn gaussian_coefficient_densities<S: GasMassScalar, const Q: usize, L: Lattice<Q>>(
        &self,
        populations: &[S],
        trace: &[S],
        offsets: &[[S; 3]],
        basis: &QuadraticGaussianBasis,
        policy: &QuadraticEntropyPolicy,
    ) -> CaeResult<Vec<Vec<S>>> {
        let moments = self.physical_moments::<S, Q, L>(populations, trace, offsets)?;
        let mut owners = std::collections::HashMap::<[u64; 10], usize>::new();
        let mut output: Vec<Vec<S>> = Vec::with_capacity(moments.len());
        for moments in moments {
            let key = if moments.iter().all(|v| !v.has_direction()) {
                Some(moments.map(|v| v.value().to_bits()))
            } else {
                None
            };
            if let Some(index) = key.and_then(|key| owners.get(&key).copied()) {
                output.push(output[index].clone());
                continue;
            }
            let coefficients = gaussian_from_physical_moments(moments, basis, policy)?;
            if let Some(key) = key {
                owners.insert(key, output.len());
            }
            output.push(coefficients);
        }
        Ok(output)
    }
    pub fn gaussian_coefficient_densities_vjp<const Q: usize, L: Lattice<Q>>(
        &self,
        populations: &[f64],
        trace: &[f64],
        offsets: &[[f64; 3]],
        basis: &QuadraticGaussianBasis,
        policy: &QuadraticEntropyPolicy,
        coefficient_bars: &[Vec<f64>],
    ) -> CaeResult<NativeGasStressDonorBars> {
        let coefficients = self.gaussian_coefficient_densities::<f64, Q, L>(
            populations,
            trace,
            offsets,
            basis,
            policy,
        )?;
        if coefficient_bars.len() != coefficients.len()
            || coefficient_bars
                .iter()
                .zip(&coefficients)
                .any(|(b, a)| b.len() != a.len() || b.iter().any(|v| !v.is_finite()))
        {
            return Err(CaeError::contract(
                "native Gaussian donor cotangent shape or value",
            ));
        }
        let states = self
            .exchange
            .macroscopic_states::<f64, Q, L>(populations, self.reference_pressure_pa)?;
        let mut macro_bars = Vec::with_capacity(states.len());
        let mut trace_bars = vec![0.; trace.len()];
        let mut offset_bars = vec![[0.; 3]; offsets.len()];
        for (k, state) in states.into_iter().enumerate() {
            let mut values = Vec::with_capacity(13);
            values.push(state.density_kg_m3);
            values.extend(state.population_velocity_m_s);
            values.extend(&trace[6 * k..6 * k + 6]);
            values.extend(offsets[k]);
            let bars = scoped_pullback(&values, |v| {
                let moments = physical_moment_transform(
                    v[0],
                    std::array::from_fn(|a| v[1 + a]),
                    std::array::from_fn(|a| v[4 + a]),
                    std::array::from_fn(|a| v[10 + a]),
                )?;
                let coefficients = gaussian_from_physical_moments(moments, basis, policy)?;
                Ok(coefficients
                    .iter()
                    .zip(&coefficient_bars[k])
                    .fold(ReverseScalar::zero(), |value, (a, b)| value + *a * *b))
            })?;
            macro_bars.push(PortMacroscopicState {
                density_kg_m3: bars[0],
                population_velocity_m_s: std::array::from_fn(|a| bars[1 + a]),
                absolute_pressure_pa: 0.,
            });
            trace_bars[6 * k..6 * k + 6].copy_from_slice(&bars[4..10]);
            offset_bars[k].copy_from_slice(&bars[10..13]);
        }
        Ok(NativeGasStressDonorBars {
            populations: self.exchange.macroscopic_states_vjp::<Q, L>(
                populations,
                self.reference_pressure_pa,
                &macro_bars,
            )?,
            stress_trace_pa: trace_bars,
            velocity_offsets_m_s: offset_bars,
        })
    }
}

pub struct NativeGasStressDonorBars {
    pub populations: Vec<f64>,
    pub stress_trace_pa: Vec<f64>,
    pub velocity_offsets_m_s: Vec<[f64; 3]>,
}

fn physical_moment_transform<S: GasMassScalar>(
    rho: S,
    velocity: [S; 3],
    trace: [S; 6],
    shift: [S; 3],
) -> CaeResult<[S; 10]> {
    let j = velocity.map(|v| rho * v);
    let mut target = [S::zero(); 10];
    target[0] = rho;
    for a in 0..3 {
        target[a + 1] = j[a] + rho * shift[a];
    }
    for (i, (a, b)) in [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)]
        .into_iter()
        .enumerate()
    {
        target[4 + i] = trace[i] + shift[a] * j[b] + shift[b] * j[a] + rho * shift[a] * shift[b];
    }
    if target.iter().any(|v| !v.finite_first_order()) {
        return Err(CaeError::contract(
            "physical port moment transform overflow",
        ));
    }
    Ok(target)
}

fn gaussian_from_physical_moments<S: GasMassScalar>(
    moments: [S; 10],
    basis: &QuadraticGaussianBasis,
    policy: &QuadraticEntropyPolicy,
) -> CaeResult<Vec<S>> {
    let rho = moments[0];
    if rho.value() <= 0. {
        return Err(CaeError::contract(
            "native Gaussian port density must be positive",
        ));
    }
    let velocity = std::array::from_fn(|a| moments[1 + a] / rho);
    let mut covariance = [[S::zero(); 3]; 3];
    for (i, (a, b)) in [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)]
        .into_iter()
        .enumerate()
    {
        let v = moments[4 + i] / rho - velocity[a] * velocity[b];
        covariance[a][b] = v;
        covariance[b][a] = v;
    }
    Ok(basis
        .gaussian(rho, velocity, covariance, policy)?
        .coefficient_density)
}
