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

use crate::narrow_gap_provider::{FeedbackPortSolution, GapDocument, Phase};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::dense::{DenseLu, DenseMatrix};
use implexity_physics_lbm::moving::{
    field::AnyMovingLbm,
    lattice::{D3Q19, D3Q27},
    port_exchange::PortExchange,
};
use implexity_solve::{multirate_coupling::SubcycledField, time_stepper::StepParameters};

pub struct GapLbm {
    bulk: AnyMovingLbm,
    gap: GapDocument,
    exchange: PortExchange,
    sides: Vec<usize>,
    port_velocity_m_s: Vec<[f64; 3]>,
    tolerance_pa: f64,
    maximum_iterations: usize,
}

pub struct GapLbmState {
    pub populations: Vec<f64>,
    pub gap_m: Vec<f64>,
}
pub struct GapLbmStep {
    pub state: GapLbmState,
    pub interface: FeedbackPortSolution,
    pub bulk_mass_increment_kg: f64,
    pub gap_mass_increment_kg: f64,
    pub exchange_mass_defect_kg: f64,
    pub external_port_momentum_kg_m_s: [f64; 3],
}
pub struct GapLbmBars {
    pub previous_populations: Vec<f64>,
    pub previous_gap_m: Vec<f64>,
    pub gap_m: Vec<f64>,
}

fn fail(message: &str) -> CaeError {
    CaeError::contract(message)
}
impl GapLbm {
    pub fn new(
        bulk: AnyMovingLbm,
        gap: GapDocument,
        exchange: PortExchange,
        sides: Vec<usize>,
        port_velocity_m_s: Vec<[f64; 3]>,
        tolerance_pa: f64,
        maximum_iterations: usize,
    ) -> CaeResult<Self> {
        gap.validate()?;
        exchange.validate_native_mask(&bulk.config().solid_mask)?;
        let point_count = match &bulk {
            AnyMovingLbm::D3Q19(f) => f.carrier().point_count(),
            AnyMovingLbm::D3Q27(f) => f.carrier().point_count(),
            _ => return Err(fail("gap bulk exchange requires D3Q19 or D3Q27")),
        };
        let config = bulk.config();
        exchange.validate_native_scales(
            config.spacing_m,
            config.density_kg_m3,
            config.macro_step_s,
        )?;
        let mut owned = [false; 4];
        if !matches!(gap.phase, Phase::Gas)
            || config.substeps != 1
            || point_count != 0
            || bulk.design_size() != 0
            || exchange.cell_count() != bulk.cells()
            || sides.len() != exchange.port_count()
            || sides.len() != port_velocity_m_s.len()
            || sides
                .iter()
                .any(|side| *side >= 4 || core::mem::replace(&mut owned[*side], true))
            || (0..4).any(|side| gap.grid.boundary_pressure_pa[side].is_some() != owned[side])
            || port_velocity_m_s.iter().flatten().any(|v| !v.is_finite())
            || (config.macro_step_s - gap.grid.time_step_s).abs() > 1e-12 * gap.grid.time_step_s
            || (config.density_kg_m3 - gap.grid.density_kg_m3).abs()
                > 1e-12 * gap.grid.density_kg_m3
            || (config.kinematic_viscosity_m2_s * config.density_kg_m3 - gap.grid.viscosity_pa_s)
                .abs()
                > 1e-12 * gap.grid.viscosity_pa_s
            || !tolerance_pa.is_finite()
            || tolerance_pa <= 0.0
            || maximum_iterations == 0
        {
            return Err(fail(
                "fixed planar gas exchange requires matching scales, one native fluid step, exclusive static gap ownership and no moving carrier",
            ));
        }
        Ok(Self {
            bulk,
            gap,
            exchange,
            sides,
            port_velocity_m_s,
            tolerance_pa,
            maximum_iterations,
        })
    }
    pub fn bulk(&self) -> &AnyMovingLbm {
        &self.bulk
    }
    pub fn initial_state(&self) -> CaeResult<GapLbmState> {
        Ok(GapLbmState {
            populations: self.bulk.initial_state(&[])?,
            gap_m: self.gap.old_gap_m.clone(),
        })
    }
    fn pressures(&self, state: &[f64]) -> CaeResult<Vec<f64>> {
        match &self.bulk {
            AnyMovingLbm::D3Q19(_) => self.exchange.pressures::<f64, 19, D3Q19>(state),
            AnyMovingLbm::D3Q27(_) => self.exchange.pressures::<f64, 27, D3Q27>(state),
            _ => Err(fail("unsupported gap bulk lattice")),
        }
    }
    fn apply(&self, state: &[f64], increments: &[[f64; 4]]) -> CaeResult<Vec<f64>> {
        let result = match &self.bulk {
            AnyMovingLbm::D3Q19(_) => self.exchange.apply::<f64, 19, D3Q19>(state, increments)?,
            AnyMovingLbm::D3Q27(_) => self.exchange.apply::<f64, 27, D3Q27>(state, increments)?,
            _ => return Err(fail("unsupported gap bulk lattice")),
        };
        let (density, velocity) = self.bulk.macroscopic(&result);
        let config = self.bulk.config();
        if result.iter().enumerate().any(|(i, v)| {
            !config.solid_mask[i % config.solid_mask.len()] && (!v.is_finite() || *v <= 0.0)
        }) {
            return Err(CaeError::convergence(
                "gap exchange requires positive finite fluid populations",
            ));
        }
        let speed_limit = config
            .lattice_velocity_limit
            .min(config.mach_limit / 3.0_f64.sqrt());
        for (i, (rho, u)) in density.iter().zip(&velocity).enumerate() {
            let velocity: [f64; 3] = core::array::from_fn(|a| {
                u[a] + 0.5
                    * config.body_acceleration_m_s2[a]
                    * config.macro_step_s
                    * config.macro_step_s
                    / config.spacing_m
            });
            if !config.solid_mask[i]
                && (!rho.is_finite()
                    || *rho <= 0.0
                    || velocity.iter().any(|v| !v.is_finite())
                    || velocity.iter().map(|v| v * v).sum::<f64>().sqrt() > speed_limit)
            {
                return Err(CaeError::convergence(
                    "gap exchange exceeds the native fluid velocity bounds",
                ));
            }
        }
        Ok(result)
    }
    fn moments(&self, state: &[f64]) -> CaeResult<[f64; 4]> {
        match &self.bulk {
            AnyMovingLbm::D3Q19(_) => self.exchange.moments::<f64, 19, D3Q19>(state),
            AnyMovingLbm::D3Q27(_) => self.exchange.moments::<f64, 27, D3Q27>(state),
            _ => Err(fail("unsupported gap bulk lattice")),
        }
    }
    fn document(&self, previous: &GapLbmState) -> GapDocument {
        let mut d = self.gap.clone();
        d.old_gap_m = previous.gap_m.clone();
        d.old_inventory_m = previous.gap_m.clone();
        d
    }
    fn trace(&self) -> Vec<f64> {
        vec![0.0; self.bulk.trace_size()]
    }
    fn parameters() -> StepParameters<'static> {
        StepParameters {
            design: &[],
            time_scale: 1.0,
        }
    }
    fn streamed(&self, n: usize, previous: &GapLbmState) -> CaeResult<Vec<f64>> {
        let trace = self.trace();
        Ok(self
            .bulk
            .subcycle(n, &previous.populations, &trace, &trace, Self::parameters())?
            .state)
    }
    fn feedback(
        &self,
        document: &GapDocument,
        gap: &[f64],
        streamed: &[f64],
    ) -> CaeResult<FeedbackPortSolution> {
        let p = self.pressures(streamed)?;
        let derivative = self.exchange.pressure_mass_jacobian();
        let mut base = [None; 4];
        let mut gain = [0.0; 4];
        for (port, &side) in self.sides.iter().enumerate() {
            base[side] = Some(p[port]);
            gain[side] = derivative[port] * document.grid.density_kg_m3 * document.grid.time_step_s;
        }
        document.solve_feedback_ports(gap, base, gain, self.tolerance_pa, self.maximum_iterations)
    }
    fn increments(&self, interface: &FeedbackPortSolution) -> Vec<[f64; 4]> {
        let scale = self.gap.grid.density_kg_m3 * self.gap.grid.time_step_s;
        self.sides
            .iter()
            .zip(&self.port_velocity_m_s)
            .map(|(&side, v)| {
                let mass = scale * interface.linearization.flux_m3_s[side];
                [mass, mass * v[0], mass * v[1], mass * v[2]]
            })
            .collect()
    }
    pub fn advance(&self, n: usize, previous: &GapLbmState, gap: &[f64]) -> CaeResult<GapLbmStep> {
        if n == 0 {
            return Err(fail("gap bulk step indices start at one"));
        }
        let document = self.document(previous);
        let streamed = self.streamed(n, previous)?;
        let interface = self.feedback(&document, gap, &streamed)?;
        let increments = self.increments(&interface);
        let populations = self.apply(&streamed, &increments)?;
        let before = self.moments(&streamed)?;
        let after = self.moments(&populations)?;
        let area = document.grid.spacing_m[0] * document.grid.spacing_m[1];
        let rho = document.grid.density_kg_m3;
        let gap_mass_increment_kg = rho
            * area
            * gap
                .iter()
                .zip(&previous.gap_m)
                .map(|(h, p)| h - p)
                .sum::<f64>();
        let bulk_mass_increment_kg = after[0] - before[0];
        let external_port_momentum_kg_m_s =
            core::array::from_fn(|a| increments.iter().map(|v| v[a + 1]).sum());
        let exchange_mass_defect_kg = bulk_mass_increment_kg + gap_mass_increment_kg;
        Ok(GapLbmStep {
            state: GapLbmState {
                populations,
                gap_m: gap.to_vec(),
            },
            interface,
            bulk_mass_increment_kg,
            gap_mass_increment_kg,
            exchange_mass_defect_kg,
            external_port_momentum_kg_m_s,
        })
    }
    pub fn advance_vjp(
        &self,
        n: usize,
        previous: &GapLbmState,
        gap: &[f64],
        population_bar: &[f64],
        gap_state_bar: &[f64],
        flux_bar: [f64; 4],
    ) -> CaeResult<GapLbmBars> {
        if n == 0 {
            return Err(fail("gap bulk step indices start at one"));
        }
        if population_bar
            .iter()
            .chain(gap_state_bar)
            .chain(&flux_bar)
            .any(|v| !v.is_finite())
            || gap_state_bar.len() != gap.len()
        {
            return Err(fail("gap bulk cotangent shape or finite-value mismatch"));
        }
        let document = self.document(previous);
        let streamed = self.streamed(n, previous)?;
        let interface = self.feedback(&document, gap, &streamed)?;
        self.apply(&streamed, &self.increments(&interface))?;
        let (mut streamed_bar, increment_bar) = match &self.bulk {
            AnyMovingLbm::D3Q19(_) => self.exchange.apply_vjp::<19, D3Q19>(population_bar)?,
            AnyMovingLbm::D3Q27(_) => self.exchange.apply_vjp::<27, D3Q27>(population_bar)?,
            _ => return Err(fail("unsupported gap bulk lattice")),
        };
        let scale = document.grid.density_kg_m3 * document.grid.time_step_s;
        let mut qbar = flux_bar;
        for (port, &side) in self.sides.iter().enumerate() {
            qbar[side] += scale
                * (increment_bar[port][0]
                    + (0..3)
                        .map(|a| increment_bar[port][a + 1] * self.port_velocity_m_s[port][a])
                        .sum::<f64>());
        }
        let map = &interface.linearization;
        let derivative = self.exchange.pressure_mass_jacobian();
        let mut gain = [0.0; 4];
        for (port, &side) in self.sides.iter().enumerate() {
            gain[side] = derivative[port] * scale;
        }
        let mut jac = DenseMatrix::identity(4);
        for row in 0..4 {
            for col in 0..4 {
                jac.data[row * 4 + col] -= gain[row] * map.flux_pressure_m3_per_pa_s[row][col];
            }
        }
        let rhs = (0..4)
            .map(|col| {
                (0..4)
                    .map(|row| map.flux_pressure_m3_per_pa_s[row][col] * qbar[row])
                    .sum::<f64>()
            })
            .collect::<Vec<_>>();
        let lambda = DenseLu::new(&jac)
            .map_err(|e| fail(&e.to_string()))?
            .solve(&rhs, 1, true)
            .map_err(|e| fail(&e.to_string()))?;
        let effective = (0..4)
            .map(|i| qbar[i] + gain[i] * lambda[i])
            .collect::<Vec<_>>();
        let gap_m = (0..gap.len())
            .map(|j| {
                gap_state_bar[j]
                    + (0..4)
                        .map(|i| effective[i] * map.flux_gap_m2_s[i][j])
                        .sum::<f64>()
            })
            .collect();
        let previous_gap_m = (0..gap.len())
            .map(|j| {
                (0..4)
                    .map(|i| effective[i] * map.flux_previous_m2_s[i][j])
                    .sum::<f64>()
            })
            .collect();
        let pressure_bar = self
            .sides
            .iter()
            .map(|&side| lambda[side])
            .collect::<Vec<_>>();
        let pbar = match &self.bulk {
            AnyMovingLbm::D3Q19(_) => self.exchange.pressures_vjp::<19, D3Q19>(&pressure_bar)?,
            AnyMovingLbm::D3Q27(_) => self.exchange.pressures_vjp::<27, D3Q27>(&pressure_bar)?,
            _ => return Err(fail("unsupported gap bulk lattice")),
        };
        for (v, p) in streamed_bar.iter_mut().zip(pbar) {
            *v += p;
        }
        let trace = self.trace();
        let bars = self.bulk.subcycle_adjoint(
            n,
            &previous.populations,
            &trace,
            &trace,
            Self::parameters(),
            &streamed_bar,
            &vec![0.0; trace.len()],
            &vec![0.0; self.bulk.sample_names().len()],
        )?;
        Ok(GapLbmBars {
            previous_populations: bars.previous,
            previous_gap_m,
            gap_m,
        })
    }
}
