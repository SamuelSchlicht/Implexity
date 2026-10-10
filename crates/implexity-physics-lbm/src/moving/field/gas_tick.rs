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

use super::*;

#[derive(Clone, Copy)]
pub struct CoupledTickInput<'a> {
    pub macro_index: usize,
    pub tick: usize,
    pub previous: &'a [f64],
    pub start: &'a [f64],
    pub end: &'a [f64],
    pub design: &'a [f64],
    pub time_scale: f64,
}

#[derive(Clone, Copy)]
pub struct CoupledTickDirection<'a> {
    pub previous: &'a [f64],
    pub start: &'a [f64],
    pub end: &'a [f64],
    pub design: &'a [f64],
    pub time_scale: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoupledInterfaceLink {
    cell: usize,
    outgoing_population: usize,
}
impl CoupledInterfaceLink {
    pub fn cell(&self) -> usize {
        self.cell
    }
    pub fn outgoing_population(&self) -> usize {
        self.outgoing_population
    }
}

pub struct CoupledReflectionBars {
    pub previous: Vec<f64>,
    pub time_scale: f64,
}

pub struct CoupledIncomingBars {
    pub native: SubcycleBars,
    pub incoming: Vec<(usize, Vec<f64>)>,
}

pub struct CoupledRelaxationBars {
    pub previous: Vec<f64>,
    pub incoming: Vec<(usize, Vec<f64>)>,
    pub time_scale: f64,
    pub absolute_origin_s: f64,
}

pub struct CoupledMacroSession<'a, const Q: usize, L: Lattice<Q>> {
    field: &'a MovingLbm<Q, L>,
    mac: Macro<f64>,
    acc: Accum<f64>,
    macro_index: usize,
    time_scale: f64,
    tick: usize,
    sample_before: Vec<f64>,
    exchange_accepted: bool,
}

impl<'a, const Q: usize, L: Lattice<Q>> CoupledMacroSession<'a, Q, L> {
    pub fn check_collision_exchange_ports(&self, cells: &[usize]) -> CaeResult<()> {
        self.field
            .check_collision_exchange_occupancy(&self.mac.occ, cells)
    }

    pub fn tick(&mut self, previous: &[f64]) -> CaeResult<Vec<f64>> {
        self.tick_with_incoming(previous, &[])
    }

    pub fn tick_with_incoming(
        &mut self,
        previous: &[f64],
        incoming: &[(usize, Vec<f64>)],
    ) -> CaeResult<Vec<f64>> {
        if self.tick >= self.field.config.substeps || previous.len() != self.field.state_size() {
            return Err(CaeError::contract(
                "coupled native tick state or order mismatch",
            ));
        }
        self.field.admit_incoming(incoming)?;
        self.sample_before.clone_from(&self.acc.samples);
        let mut state = Vec::new();
        let mut scratch = Vec::new();
        self.field.advance_from_with_incoming(
            self.macro_index,
            Some(previous),
            &mut state,
            self.tick,
            self.tick + 1,
            self.time_scale,
            &self.mac,
            Some(&mut self.acc),
            &mut scratch,
            incoming,
        );
        self.tick += 1;
        self.exchange_accepted = false;
        self.field.check_state(&state, self.acc.max_speed)?;
        Ok(state)
    }

    pub fn accept_exchange(&mut self, state: &[f64]) -> CaeResult<()> {
        if self.tick == 0 || self.exchange_accepted {
            return Err(CaeError::contract("coupled native exchange order mismatch"));
        }
        let samples = self.field.post_exchange_samples(
            self.macro_index,
            self.tick,
            state,
            self.time_scale,
        )?;
        for (i, o) in self.field.observables.iter().enumerate() {
            if o.reads_cells() || o.reads_populations() {
                self.acc.samples[i] = self.sample_before[i] + samples[i];
            }
        }
        let (rho, u) = self.field.post_exchange_fields(state, self.time_scale)?;
        let speed = u
            .iter()
            .map(|u| u.iter().map(|x| x * x).sum::<f64>().sqrt())
            .fold(0., f64::max);
        self.acc.max_speed = self.acc.max_speed.max(speed);
        self.field.check_state(state, speed)?;
        if self.tick == self.field.config.substeps {
            let c = self
                .field
                .params(self.time_scale, self.macro_index, self.tick)
                .scales
                .velocity;
            self.acc.kinetic = kinetic(&rho, &u, &self.field.fluid)
                * self.field.config.density_kg_m3
                * c
                * c
                * self.field.config.spacing_m.powi(3);
        }
        self.exchange_accepted = true;
        Ok(())
    }

    pub fn finish(mut self, initial: &[f64], state: Vec<f64>) -> CaeResult<SubcycleOutput> {
        if self.tick != self.field.config.substeps
            || initial.len() != self.field.state_size()
            || state.len() != self.field.state_size()
        {
            return Err(CaeError::contract(
                "coupled native macro interval incomplete",
            ));
        }
        let min = self.field.check_state(&state, self.acc.max_speed)?;
        if self.field.compensated() {
            let a = self.field.entrained(initial, &self.mac.occ, 0);
            let b = self.field.entrained(&state, &self.mac.occ, 1);
            let c = self
                .field
                .params(self.time_scale, self.macro_index, 1)
                .scales
                .velocity;
            self.field
                .compensate(&mut self.acc, &self.mac.occ, &a, &b, c);
        }
        let (flux, _) = self.field.flux_of(&self.mac, &self.acc, self.time_scale)?;
        let p = self.field.params(self.time_scale, self.macro_index, 1);
        let mut ledger = BTreeMap::new();
        ledger.insert("fluid_kinetic_energy_J".into(), self.acc.kinetic);
        ledger.insert("interface_work_J".into(), self.acc.pairing);
        for (e, name) in ["x", "y", "z"].iter().enumerate() {
            ledger.insert(
                format!("solid_impulse_{name}_N_s"),
                -self.acc.impulse[e] * p.impulse_scale,
            );
        }
        ledger.insert("max_mach".into(), self.acc.max_speed * 3f64.sqrt());
        ledger.insert("min_population".into(), min);
        ledger.insert("active_cells".into(), self.mac.occ.cells.len() as f64);
        ledger.insert("max_displacement_cells".into(), self.mac.displacement);
        Ok(SubcycleOutput {
            state,
            flux,
            samples: self.acc.samples,
            pairing: self.acc.pairing,
            ledger,
        })
    }
}

impl<const Q: usize, L: Lattice<Q>> MovingLbm<Q, L> {
    pub fn coupled_macro_session(
        &self,
        n: usize,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
    ) -> CaeResult<CoupledMacroSession<'_, Q, L>> {
        if start.len() != self.trace_size()
            || end.len() != self.trace_size()
            || design.len() != self.design_size()
        {
            return Err(CaeError::contract(
                "coupled native macro input length mismatch",
            ));
        }
        if start
            .iter()
            .chain(end)
            .chain(design)
            .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "coupled native macro input is not finite",
            ));
        }
        self.admit_time_scale(time_scale)?;
        let mac = self.setup::<f64>(start, end, design, None, None, None, false)?;
        let acc = self.new_accum(&mac.occ);
        Ok(CoupledMacroSession {
            field: self,
            mac,
            acc,
            macro_index: n,
            time_scale,
            tick: 0,
            sample_before: vec![0.; self.observables.len()],
            exchange_accepted: false,
        })
    }

    fn coupled_input(&self, input: CoupledTickInput<'_>) -> CaeResult<()> {
        self.check_inputs(input.previous, input.start, input.end, input.design)?;
        if input.tick == 0
            || input.tick > self.config.substeps
            || input
                .previous
                .iter()
                .chain(input.start)
                .chain(input.end)
                .chain(input.design)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("coupled native tick input refused"));
        }
        self.admit_time_scale(input.time_scale)
    }

    pub fn coupled_tick(&self, input: CoupledTickInput<'_>) -> CaeResult<SubcycleOutput> {
        self.coupled_tick_with_incoming(input, &[])
    }

    pub fn coupled_tick_with_incoming(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
    ) -> CaeResult<SubcycleOutput> {
        self.coupled_input(input)?;
        self.admit_incoming(incoming)?;
        let mac = self.setup::<f64>(
            input.start,
            input.end,
            input.design,
            None,
            None,
            None,
            false,
        )?;
        let mut acc = self.new_accum(&mac.occ);
        let mut state = Vec::new();
        let mut scratch = Vec::new();
        self.advance_from_with_incoming(
            input.macro_index,
            Some(input.previous),
            &mut state,
            input.tick - 1,
            input.tick,
            input.time_scale,
            &mac,
            Some(&mut acc),
            &mut scratch,
            incoming,
        );
        let min = self.check_state(&state, acc.max_speed)?;
        let (flux, _) = self.flux_of(&mac, &acc, input.time_scale)?;
        let mut ledger = BTreeMap::new();
        ledger.insert("min_population".into(), min);
        ledger.insert("max_mach".into(), acc.max_speed * 3f64.sqrt());
        Ok(SubcycleOutput {
            state,
            flux,
            samples: acc.samples,
            pairing: acc.pairing,
            ledger,
        })
    }

    pub fn coupled_tick_tangent(
        &self,
        input: CoupledTickInput<'_>,
        direction: CoupledTickDirection<'_>,
    ) -> CaeResult<SubcycleTangentOutput> {
        self.coupled_tick_with_incoming_tangent(input, direction, &[], &[])
    }

    pub fn coupled_tick_with_incoming_tangent(
        &self,
        input: CoupledTickInput<'_>,
        direction: CoupledTickDirection<'_>,
        incoming: &[(usize, Vec<f64>)],
        incoming_direction: &[(usize, Vec<f64>)],
    ) -> CaeResult<SubcycleTangentOutput> {
        self.coupled_input(input)?;
        self.admit_incoming(incoming)?;
        if incoming_direction.len() != incoming.len()
            || incoming
                .iter()
                .zip(incoming_direction)
                .any(|((i, row), (di, drow))| {
                    i != di || row.len() != drow.len() || drow.iter().any(|v| !v.is_finite())
                })
        {
            return Err(CaeError::contract(
                "coupled incoming population direction mismatch",
            ));
        }
        let incoming_dual: Vec<_> = incoming
            .iter()
            .zip(incoming_direction)
            .map(|((cell, row), (_, direction))| {
                (
                    *cell,
                    row.iter()
                        .zip(direction)
                        .map(|(v, d)| Dual::new(*v, [*d]))
                        .collect(),
                )
            })
            .collect();
        if direction.previous.len() != input.previous.len()
            || direction.start.len() != input.start.len()
            || direction.end.len() != input.end.len()
            || direction.design.len() != input.design.len()
            || !direction.time_scale.is_finite()
            || direction
                .previous
                .iter()
                .chain(direction.start)
                .chain(direction.end)
                .chain(direction.design)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("coupled native tick direction refused"));
        }
        let mac = self.setup::<Dual<1>>(
            input.start,
            input.end,
            input.design,
            Some(direction.start),
            Some(direction.end),
            Some(direction.design),
            false,
        )?;
        let tau = Dual::new(input.time_scale, [direction.time_scale]);
        let mut acc = self.new_accum(&mac.occ);
        let mut state: Vec<_> = input
            .previous
            .iter()
            .zip(direction.previous)
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect();
        let mut scratch = Vec::new();
        self.advance_from_with_incoming(
            input.macro_index,
            None,
            &mut state,
            input.tick - 1,
            input.tick,
            tau,
            &mac,
            Some(&mut acc),
            &mut scratch,
            &incoming_dual,
        );
        let values: Vec<_> = state.iter().map(|v| v.re).collect();
        self.check_state(&values, acc.max_speed)?;
        let (flux, _) = self.flux_of(&mac, &acc, tau)?;
        Ok(SubcycleTangentOutput {
            state: state.iter().map(|v| v.eps[0]).collect(),
            flux: flux.iter().map(|v| v.eps[0]).collect(),
            samples: acc.samples.iter().map(|v| v.eps[0]).collect(),
        })
    }

    pub fn coupled_tick_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleBars> {
        Ok(self
            .coupled_tick_with_incoming_adjoint(input, &[], state_bar, flux_bar, sample_bar)?
            .native)
    }

    pub fn coupled_tick_with_incoming_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<CoupledIncomingBars> {
        self.coupled_input(input)?;
        self.admit_incoming(incoming)?;
        if state_bar.len() != self.state_size()
            || flux_bar.len() != self.trace_size()
            || sample_bar.len() != self.observables.len()
            || state_bar
                .iter()
                .chain(flux_bar)
                .chain(sample_bar)
                .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("coupled native tick cotangent refused"));
        }
        let mac =
            self.setup::<f64>(input.start, input.end, input.design, None, None, None, true)?;
        let mut acc = self.new_accum(&mac.occ);
        let mut state = Vec::new();
        let mut scratch = Vec::new();
        self.advance_from_with_incoming(
            input.macro_index,
            Some(input.previous),
            &mut state,
            input.tick - 1,
            input.tick,
            input.time_scale,
            &mac,
            Some(&mut acc),
            &mut scratch,
            incoming,
        );
        self.check_state(&state, acc.max_speed)?;
        let p1 = self.params(input.time_scale, input.macro_index, 1);
        let (flux, _) = self.flux_of(&mac, &acc, input.time_scale)?;
        let ib = self
            .carrier
            .flux_to_impulses(flux_bar, p1.scales.macro_step);
        let ib: Vec<_> = ib.iter().map(|v| v.map(|v| v * p1.impulse_scale)).collect();
        let (pb0, g0) = pushforward::endpoint_impulses_vjp(
            &self.frame,
            &mac.ends[0],
            &mac.occ.slot,
            &mac.occ.cells,
            &acc.g[0],
            &ib,
        );
        let (pb1, g1) = pushforward::endpoint_impulses_vjp(
            &self.frame,
            &mac.ends[1],
            &mac.occ.slot,
            &mac.occ.cells,
            &acc.g[1],
            &ib,
        );
        let mut tau_bar = -flux
            .iter()
            .zip(flux_bar)
            .map(|(v, b)| v * b * 2. / input.time_scale)
            .sum::<f64>();
        for (o, (v, b)) in self
            .observables
            .iter()
            .zip(acc.samples.iter().zip(sample_bar))
        {
            tau_bar -= v * b * f64::from(o.degree) / input.time_scale;
        }
        let rows = mac.occ.cells.len();
        let mut db = [vec![0.; rows], vec![0.; rows]];
        let mut mb = [vec![[0.; 3]; rows], vec![[0.; 3]; rows]];
        let mut fb = vec![0.; self.state_size()];
        let (mut p, dp) =
            self.params_with_derivative::<f64>(input.time_scale, input.macro_index, input.tick);
        p.incoming = incoming.to_vec();
        let bars = self.reverse_substep(
            input.previous,
            Cotangent::Direct(state_bar),
            &mut fb,
            &p,
            &mac.occ,
            [&g0, &g1],
            sample_bar,
            input.tick,
            &mut db,
            &mut mb,
        );
        tau_bar += bars.contract(&dp);
        let origin_bar = if let Some(clock) = self.absolute_clock {
            let dp = self
                .params_absolute(
                    Dual::<1>::new(input.time_scale, [0.]),
                    Dual::<1>::new(clock.origin_s, [1.]),
                    input.tick,
                )
                .map(|v| v.eps[0]);
            bars.contract(&dp)
        } else {
            0.
        };
        let cells = self.cells();
        let incoming_bar = incoming
            .iter()
            .map(|(cell, _)| {
                let row = (0..Q)
                    .map(|q| {
                        let index = q * cells + cell;
                        let value = fb[index];
                        fb[index] = 0.;
                        value
                    })
                    .collect();
                (*cell, row)
            })
            .collect();
        let previous = self.streaming_transpose(&fb, state_bar, vec![]);
        let mut vb = vec![[0.; 3]; mac.v.len()];
        let e0 = pushforward::endpoint_vjp(
            &self.frame,
            &mac.ends[0],
            &mac.occ.slot,
            &mac.v,
            &db[0],
            &mb[0],
            &mut vb,
        );
        let e1 = pushforward::endpoint_vjp(
            &self.frame,
            &mac.ends[1],
            &mac.occ.slot,
            &mac.v,
            &db[1],
            &mb[1],
            &mut vb,
        );
        let (s, ds) = self.points_pullback(input.start, input.design, &[&e0, &pb0], None, None)?;
        let (e, de) = self.points_pullback(input.end, input.design, &[&e1, &pb1], None, None)?;
        let velocity = self
            .carrier
            .impulses_to_flux(&vb, self.config.substeps as f64 * self.config.spacing_m);
        Ok(CoupledIncomingBars {
            incoming: incoming_bar,
            native: SubcycleBars {
                previous,
                trace_start: s.0.iter().zip(&velocity).map(|(v, d)| v - d).collect(),
                trace_end: e.0.iter().zip(&velocity).map(|(v, d)| v + d).collect(),
                design: ds.0.iter().zip(de.0).map(|(a, b)| a + b).collect(),
                time_scale: tau_bar,
                absolute_origin_s: origin_bar,
            },
        })
    }

    fn admit_incoming<S: Scalar>(&self, incoming: &[(usize, Vec<S>)]) -> CaeResult<()> {
        if incoming.windows(2).any(|w| w[0].0 >= w[1].0)
            || incoming.iter().any(|(cell, row)| {
                *cell >= self.cells()
                    || !self.fluid[*cell]
                    || self.topology.simple[*cell]
                    || self.topology.port_slot[*cell] != NONE
                    || row.len() != Q
                    || row.iter().any(|v| !v.value().is_finite() || v.value() < 0.)
                    || !row.iter().map(|v| v.value()).sum::<f64>().is_finite()
                    || row.iter().map(|v| v.value()).sum::<f64>() <= 0.
            })
        {
            return Err(CaeError::contract(
                "coupled incoming populations require sorted native boundary cells and positive density",
            ));
        }
        Ok(())
    }

    pub fn coupled_tick_incoming(
        &self,
        input: CoupledTickInput<'_>,
        cells: &[usize],
    ) -> CaeResult<Vec<(usize, Vec<f64>)>> {
        self.coupled_input(input)?;
        if cells.windows(2).any(|w| w[0] >= w[1])
            || cells.iter().any(|i| {
                *i >= self.cells()
                    || !self.fluid[*i]
                    || self.topology.simple[*i]
                    || self.topology.port_slot[*i] != NONE
            })
        {
            return Err(CaeError::contract(
                "invalid coupled incoming cell selection",
            ));
        }
        let p = self.params(input.time_scale, input.macro_index, input.tick);
        let incoming: Vec<_> = cells
            .iter()
            .map(|&cell| (cell, self.pre(input.previous, cell, &p).to_vec()))
            .collect();
        self.admit_incoming(&incoming)?;
        Ok(incoming)
    }

    pub fn coupled_tick_incoming_tangent(
        &self,
        input: CoupledTickInput<'_>,
        cells: &[usize],
        previous_direction: &[f64],
    ) -> CaeResult<Vec<(usize, Vec<f64>)>> {
        self.coupled_tick_incoming(input, cells)?;
        if previous_direction.len() != input.previous.len()
            || previous_direction.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "invalid native incoming state direction",
            ));
        }
        Ok(cells
            .iter()
            .map(|&cell| {
                (
                    cell,
                    self.topology.pull_cell(previous_direction, cell).to_vec(),
                )
            })
            .collect())
    }

    pub fn coupled_tick_incoming_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        bars: &[(usize, Vec<f64>)],
    ) -> CaeResult<Vec<f64>> {
        let cells: Vec<_> = bars.iter().map(|(cell, _)| *cell).collect();
        self.coupled_tick_incoming(input, &cells)?;
        if bars
            .iter()
            .any(|(_, row)| row.len() != Q || row.iter().any(|v| !v.is_finite()))
        {
            return Err(CaeError::contract(
                "invalid native incoming population cotangent",
            ));
        }
        let mut previous = vec![0.; self.state_size()];
        for (cell, row) in bars {
            let offset = self.topology.table_slot[*cell] as usize * Q;
            for q in 0..Q {
                previous[self.topology.pull[offset + q] as usize] += row[q];
            }
        }
        Ok(previous)
    }

    fn check_collision_exchange_occupancy(
        &self,
        occupancy: &Occupancy<f64>,
        cells: &[usize],
    ) -> CaeResult<()> {
        if !matches!(
            self.collision.kind(),
            CollisionKind::Bgk | CollisionKind::Regularized
        ) || self.config.body_acceleration_m_s2.iter().any(|&v| v != 0.)
            || cells.windows(2).any(|w| w[0] >= w[1])
            || cells.iter().any(|&cell| {
                cell >= self.cells()
                    || !self.fluid[cell]
                    || self.topology.simple[cell]
                    || self.topology.port_slot[cell] != NONE
            })
        {
            return Err(CaeError::contract(
                "collision exchange requires unforced native interface cells",
            ));
        }
        for &cell in cells {
            let slot = occupancy.slot[cell];
            if slot != NONE
                && (occupancy.d[0][slot as usize] != 0. || occupancy.d[1][slot as usize] != 0.)
            {
                return Err(CaeError::contract(
                    "collision exchange port has solid carrier occupancy",
                ));
            }
        }
        Ok(())
    }

    pub fn coupled_tick_check_collision_exchange_ports(
        &self,
        input: CoupledTickInput<'_>,
        cells: &[usize],
    ) -> CaeResult<()> {
        self.coupled_input(input)?;
        let mac = self.setup::<f64>(
            input.start,
            input.end,
            input.design,
            None,
            None,
            None,
            false,
        )?;
        self.check_collision_exchange_occupancy(&mac.occ, cells)
    }

    pub fn coupled_tick_pre_collision_rates(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        cells: &[usize],
    ) -> CaeResult<Vec<(usize, f64)>> {
        self.coupled_tick_incoming(input, cells)?;
        self.admit_incoming(incoming)?;
        if !matches!(
            self.collision.kind(),
            CollisionKind::Bgk | CollisionKind::Regularized
        ) || matches!(self.config.turbulence, Turbulence::Smagorinsky(_))
            || self.config.body_acceleration_m_s2.iter().any(|&v| v != 0.)
        {
            return Err(CaeError::contract(
                "physical port stress requires zero-force BGK or regularized collision with laminar or WALE closure",
            ));
        }
        let mut p = self.params(input.time_scale, input.macro_index, input.tick);
        p.incoming = incoming.to_vec();
        let (rates, _) = self.wale_rates(input.previous, &p);
        let out: Vec<_> = cells
            .iter()
            .map(|&cell| {
                (
                    cell,
                    if rates.is_empty() {
                        p.cell.omega
                    } else {
                        rates[cell]
                    },
                )
            })
            .collect();
        if out
            .iter()
            .any(|(_, rate)| !rate.is_finite() || *rate <= 0. || *rate >= 2.)
        {
            return Err(CaeError::contract(
                "physical port relaxation rate is outside the open interval zero to two",
            ));
        }
        Ok(out)
    }

    pub fn coupled_tick_pre_collision_rates_tangent(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        cells: &[usize],
        previous_direction: &[f64],
        incoming_direction: &[(usize, Vec<f64>)],
        time_direction: f64,
    ) -> CaeResult<Vec<(usize, f64)>> {
        self.coupled_tick_pre_collision_rates(input, incoming, cells)?;
        if previous_direction.len() != input.previous.len()
            || previous_direction.iter().any(|v| !v.is_finite())
            || !time_direction.is_finite()
            || incoming_direction.len() != incoming.len()
            || incoming_direction
                .iter()
                .zip(incoming)
                .any(|((i, d), (j, row))| {
                    i != j || d.len() != row.len() || d.iter().any(|v| !v.is_finite())
                })
        {
            return Err(CaeError::contract("invalid physical port rate direction"));
        }
        let mut p = self.params(
            Dual::<1>::new(input.time_scale, [time_direction]),
            input.macro_index,
            input.tick,
        );
        p.incoming = incoming
            .iter()
            .zip(incoming_direction)
            .map(|((i, row), (_, d))| {
                (
                    *i,
                    row.iter()
                        .zip(d)
                        .map(|(&v, &dv)| Dual::new(v, [dv]))
                        .collect(),
                )
            })
            .collect();
        let previous: Vec<_> = input
            .previous
            .iter()
            .zip(previous_direction)
            .map(|(&v, &d)| Dual::<1>::new(v, [d]))
            .collect();
        let (rates, _) = self.wale_rates(&previous, &p);
        let out: Vec<_> = cells
            .iter()
            .map(|&cell| {
                (
                    cell,
                    if rates.is_empty() {
                        p.cell.omega.eps[0]
                    } else {
                        rates[cell].eps[0]
                    },
                )
            })
            .collect();
        if out.iter().any(|(_, rate)| !rate.is_finite()) {
            return Err(CaeError::contract("physical port rate direction overflow"));
        }
        Ok(out)
    }

    pub fn coupled_tick_pre_collision_rates_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        rate_bar: &[(usize, f64)],
    ) -> CaeResult<CoupledRelaxationBars> {
        let cells: Vec<_> = rate_bar.iter().map(|v| v.0).collect();
        self.coupled_tick_pre_collision_rates(input, incoming, &cells)?;
        if rate_bar.iter().any(|(_, v)| !v.is_finite()) {
            return Err(CaeError::contract("invalid physical port rate cotangent"));
        }
        let (mut p, dp) =
            self.params_with_derivative::<f64>(input.time_scale, input.macro_index, input.tick);
        p.incoming = incoming.to_vec();
        let mut pb = ParamBar::<f64>::zero(p.ports.len(), p.targets.len());
        let mut u_bar = vec![[0.; 3]; self.cells()];
        let (_, u) = self.wale_rates(input.previous, &p);
        if let (Turbulence::Wale(model), Some(stencil)) = (&self.config.turbulence, &self.stencil) {
            let tau0 = 1. / p.cell.omega;
            for &(cell, bar) in rate_bar {
                let (gb, tb) = wale::omega_vjp(model, &stencil.gradient(&u, cell), tau0, bar);
                pb.omega -= tb / (p.cell.omega * p.cell.omega);
                for b in 0..3 {
                    let mut add = |node: usize, weight: f64| {
                        for a in 0..3 {
                            u_bar[node][a] += gb[a][b] * weight;
                        }
                    };
                    match stencil.diff[cell][b] {
                        wale::Difference::Zero => {}
                        wale::Difference::Central(plus, minus) => {
                            add(plus, 0.5);
                            add(minus, -0.5);
                        }
                        wale::Difference::Forward(plus) => {
                            add(plus, 1.);
                            add(cell, -1.);
                        }
                        wale::Difference::Backward(minus) => {
                            add(cell, 1.);
                            add(minus, -1.);
                        }
                    }
                }
            }
        } else {
            pb.omega = rate_bar.iter().map(|v| v.1).sum();
        }
        let mut previous = vec![0.; input.previous.len()];
        let mut incoming_bar: Vec<_> = incoming
            .iter()
            .map(|(cell, row)| (*cell, vec![0.; row.len()]))
            .collect();
        for (cell, &ub) in u_bar.iter().enumerate() {
            if ub == [0.; 3] {
                continue;
            }
            let pre = self.pre(input.previous, cell, &p);
            let (rho, _) = moments::<f64, Q, L>(&pre);
            let udot: f64 = (0..3).map(|a| ub[a] * u[cell][a]).sum();
            let row: [f64; Q] = std::array::from_fn(|q| {
                ((0..3).map(|a| ub[a] * L::CF[q][a]).sum::<f64>() - udot) / rho
            });
            if let Ok(k) = incoming.binary_search_by_key(&cell, |v| v.0) {
                for q in 0..Q {
                    incoming_bar[k].1[q] += row[q];
                }
                continue;
            }
            let slot = self.topology.port_slot[cell];
            let (source, population_bar) = if slot == NONE {
                (cell, row)
            } else {
                let pc = &self.topology.ports[slot as usize];
                let neighbor = self.topology.pull_cell(input.previous, pc.neighbour);
                let value = self.port_value(&p, slot as usize);
                let (bar, value_bar) = port_populations_vjp::<f64, Q, L>(&neighbor, value, &row);
                match value {
                    PortValue::Velocity(_) => {
                        for a in 0..3 {
                            pb.ports[pc.port][a] += value_bar[a] * pc.profile;
                        }
                    }
                    PortValue::Density(_) => pb.ports[pc.port][0] += value_bar[0],
                };
                (pc.neighbour, bar)
            };
            for q in 0..Q {
                let index = if self.topology.simple[source] {
                    q * self.cells() + (source as isize - self.topology.offsets[q]) as usize
                } else {
                    self.topology.pull[self.topology.table_slot[source] as usize * Q + q] as usize
                };
                previous[index] += population_bar[q];
            }
        }
        let origin_bar = if let Some(clock) = self.absolute_clock {
            let d = self
                .params_absolute(
                    Dual::<1>::new(input.time_scale, [0.]),
                    Dual::<1>::new(clock.origin_s, [1.]),
                    input.tick,
                )
                .map(|v| v.eps[0]);
            pb.contract(&d)
        } else {
            0.
        };
        let time_bar = pb.contract(&dp);
        if previous
            .iter()
            .chain(incoming_bar.iter().flat_map(|v| &v.1))
            .chain([&time_bar, &origin_bar])
            .any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("physical port rate transpose overflow"));
        }
        Ok(CoupledRelaxationBars {
            previous,
            incoming: incoming_bar,
            time_scale: time_bar,
            absolute_origin_s: origin_bar,
        })
    }

    pub fn coupled_interface_links(
        &self,
        excluded: &[usize],
    ) -> CaeResult<Vec<CoupledInterfaceLink>> {
        if excluded.is_empty()
            || excluded.windows(2).any(|w| w[0] >= w[1])
            || excluded
                .iter()
                .any(|&cell| cell >= self.cells() || self.fluid[cell])
        {
            return Err(CaeError::contract(
                "native interface requires sorted excluded cells",
            ));
        }
        let mut owned = vec![false; self.cells()];
        for &cell in excluded {
            owned[cell] = true;
        }
        let mut links = Vec::new();
        for cell in 0..self.cells() {
            if !self.fluid[cell] {
                continue;
            }
            for q in 0..Q {
                let offset = L::C[q].map(i64::from);
                if !self
                    .topology
                    .grid
                    .inside(cell, offset, self.topology.periodic)
                {
                    continue;
                }
                let neighbor = self.topology.grid.wrap(cell, offset);
                if !owned[neighbor] {
                    continue;
                }
                if self.topology.port_slot[cell] != NONE || self.topology.simple[cell] {
                    return Err(CaeError::contract(
                        "native interface overlaps another boundary",
                    ));
                }
                let row = self.topology.table_slot[cell] as usize * Q;
                if self.topology.pull[row + L::OPPOSITE[q]] as usize != q * self.cells() + cell {
                    return Err(CaeError::contract(
                        "native interface reflection link does not match streaming",
                    ));
                }
                links.push(CoupledInterfaceLink {
                    cell,
                    outgoing_population: q,
                });
            }
        }
        if links.is_empty() {
            return Err(CaeError::contract("native interface has no fluid links"));
        }
        Ok(links)
    }

    pub fn coupled_tick_reflection(
        &self,
        input: CoupledTickInput<'_>,
        links: &[CoupledInterfaceLink],
    ) -> CaeResult<Vec<(usize, [f64; 3])>> {
        self.coupled_input(input)?;
        let scale = self.config.density_kg_m3
            * self.config.spacing_m.powi(3)
            * self
                .params(input.time_scale, input.macro_index, input.tick)
                .scales
                .velocity;
        if !scale.is_finite() || scale <= 0. {
            return Err(CaeError::contract(
                "native interface momentum scale is not positive and finite",
            ));
        }
        let mut values = BTreeMap::<usize, [f64; 3]>::new();
        let mut seen = std::collections::BTreeSet::new();
        for link in links {
            if link.cell >= self.cells()
                || link.outgoing_population >= Q
                || !self.fluid[link.cell]
                || self.topology.simple[link.cell]
                || self.topology.port_slot[link.cell] != NONE
                || !seen.insert((link.cell, link.outgoing_population))
            {
                return Err(CaeError::contract(
                    "invalid native interface reflection link",
                ));
            }
            let row = self.topology.table_slot[link.cell] as usize * Q;
            let q = link.outgoing_population;
            if self.topology.pull[row + L::OPPOSITE[q]] as usize != q * self.cells() + link.cell {
                return Err(CaeError::contract("native interface streaming changed"));
            }
            let population = input.previous[q * self.cells() + link.cell];
            if population < 0. {
                return Err(CaeError::convergence(
                    "native outgoing interface population is negative",
                ));
            }
            let value = values.entry(link.cell).or_insert([0.; 3]);
            for d in 0..3 {
                value[d] -= 2. * scale * population * L::CF[q][d];
            }
        }
        Ok(values.into_iter().collect())
    }

    pub fn coupled_tick_reflection_tangent(
        &self,
        input: CoupledTickInput<'_>,
        links: &[CoupledInterfaceLink],
        previous_direction: &[f64],
        time_direction: f64,
    ) -> CaeResult<Vec<(usize, [f64; 3])>> {
        let base = self.coupled_tick_reflection(input, links)?;
        if previous_direction.len() != self.state_size()
            || previous_direction.iter().any(|v| !v.is_finite())
            || !time_direction.is_finite()
        {
            return Err(CaeError::contract("invalid native reflection direction"));
        }
        let scale = self.config.density_kg_m3
            * self.config.spacing_m.powi(3)
            * self
                .params(input.time_scale, input.macro_index, input.tick)
                .scales
                .velocity;
        let mut direction: BTreeMap<usize, [f64; 3]> = base
            .iter()
            .map(|(cell, j)| (*cell, j.map(|v| -v * time_direction / input.time_scale)))
            .collect();
        for link in links {
            let derivative =
                previous_direction[link.outgoing_population * self.cells() + link.cell];
            let value = direction.get_mut(&link.cell).unwrap();
            for d in 0..3 {
                value[d] -= 2. * scale * derivative * L::CF[link.outgoing_population][d];
            }
        }
        Ok(direction.into_iter().collect())
    }

    pub fn coupled_tick_reflection_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        links: &[CoupledInterfaceLink],
        reflection_bar: &[(usize, [f64; 3])],
    ) -> CaeResult<CoupledReflectionBars> {
        let base = self.coupled_tick_reflection(input, links)?;
        if base.len() != reflection_bar.len()
            || base
                .iter()
                .zip(reflection_bar)
                .any(|((i, _), (j, b))| i != j || b.iter().any(|v| !v.is_finite()))
        {
            return Err(CaeError::contract("invalid native reflection cotangent"));
        }
        let scale = self.config.density_kg_m3
            * self.config.spacing_m.powi(3)
            * self
                .params(input.time_scale, input.macro_index, input.tick)
                .scales
                .velocity;
        let cotangent: BTreeMap<_, _> = reflection_bar.iter().copied().collect();
        let mut previous = vec![0.; self.state_size()];
        for link in links {
            previous[link.outgoing_population * self.cells() + link.cell] -= 2.
                * scale
                * (0..3)
                    .map(|d| L::CF[link.outgoing_population][d] * cotangent[&link.cell][d])
                    .sum::<f64>();
        }
        let time_scale = -base
            .iter()
            .zip(reflection_bar)
            .map(|((_, j), (_, b))| (0..3).map(|d| j[d] * b[d]).sum::<f64>())
            .sum::<f64>()
            / input.time_scale;
        Ok(CoupledReflectionBars {
            previous,
            time_scale,
        })
    }

    fn post_exchange_fields<S: Lane>(
        &self,
        state: &[S],
        time_scale: S,
    ) -> CaeResult<(Vec<S>, Vec<[S; 3]>)> {
        if state.len() != self.state_size() {
            return Err(CaeError::contract(
                "post-exchange native state length mismatch",
            ));
        }
        let p = self.params(time_scale, 1, 1);
        let mut rho = vec![S::zero(); self.cells()];
        let mut u = vec![[S::zero(); 3]; self.cells()];
        for x in 0..self.cells() {
            if !self.fluid[x] {
                continue;
            }
            let f = std::array::from_fn(|q| state[q * self.cells() + x]);
            let (r, j) = moments::<S, Q, L>(&f);
            if !r.value().is_finite() || r.value() <= 0. {
                return Err(CaeError::convergence(
                    "post-exchange native density is not positive",
                ));
            }
            rho[x] = r;
            u[x] = std::array::from_fn(|d| j[d] / r + p.cell.accel[d] * 0.5);
        }
        Ok((rho, u))
    }

    fn post_exchange_samples_scalar<S: Lane>(
        &self,
        n: usize,
        k: usize,
        state: &[S],
        time_scale: S,
    ) -> CaeResult<Vec<S>> {
        if k == 0 || k > self.config.substeps {
            return Err(CaeError::contract("post-exchange sample tick refused"));
        }
        let mut samples = vec![S::zero(); self.observables.len()];
        if self.config.sampling == Sampling::End && k != self.config.substeps {
            return Ok(samples);
        }
        let (rho, u) = self.post_exchange_fields(state, time_scale)?;
        let p = self.params(time_scale, n, k);
        let weight = if self.config.sampling == Sampling::MacroMean {
            1. / self.config.substeps as f64
        } else {
            1.
        };
        for (o, s) in self.observables.iter().zip(&mut samples) {
            if o.reads_populations() {
                *s = o.population_value(&p.scales, state) * weight;
            } else if o.reads_cells() {
                *s = o.value(&p.scales, &rho, &u, &self.fluid, [S::zero(); 3]) * weight;
            }
        }
        Ok(samples)
    }

    pub fn post_exchange_samples(
        &self,
        n: usize,
        k: usize,
        state: &[f64],
        time_scale: f64,
    ) -> CaeResult<Vec<f64>> {
        self.admit_time_scale(time_scale)?;
        let (rho, u) = self.post_exchange_fields(state, time_scale)?;
        let _ = rho;
        let speed = u
            .iter()
            .map(|u| u.iter().map(|v| v * v).sum::<f64>().sqrt())
            .fold(0., f64::max);
        self.check_state(state, speed)?;
        self.post_exchange_samples_scalar(n, k, state, time_scale)
    }

    pub fn post_exchange_samples_tangent(
        &self,
        n: usize,
        k: usize,
        state: &[f64],
        time_scale: f64,
        dstate: &[f64],
        dtime: f64,
    ) -> CaeResult<Vec<f64>> {
        self.post_exchange_samples(n, k, state, time_scale)?;
        if dstate.len() != state.len()
            || !dtime.is_finite()
            || dstate.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("post-exchange sample direction refused"));
        }
        let dual: Vec<_> = state
            .iter()
            .zip(dstate)
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect();
        Ok(self
            .post_exchange_samples_scalar(n, k, &dual, Dual::new(time_scale, [dtime]))?
            .iter()
            .map(|v| v.eps[0])
            .collect())
    }

    pub fn post_exchange_samples_adjoint(
        &self,
        n: usize,
        k: usize,
        state: &[f64],
        time_scale: f64,
        sample_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, f64)> {
        let samples = self.post_exchange_samples(n, k, state, time_scale)?;
        if sample_bar.len() != samples.len() || sample_bar.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("post-exchange sample cotangent refused"));
        }
        let mut out = vec![0.; state.len()];
        if self.config.sampling == Sampling::End && k != self.config.substeps {
            return Ok((out, 0.));
        }
        let (rho, u) = self.post_exchange_fields(state, time_scale)?;
        let p = self.params(time_scale, n, k);
        let weight = if self.config.sampling == Sampling::MacroMean {
            1. / self.config.substeps as f64
        } else {
            1.
        };
        let mut rb = vec![0.; self.cells()];
        let mut ub = vec![[0.; 3]; self.cells()];
        for (o, b) in self.observables.iter().zip(sample_bar) {
            if o.reads_populations() {
                for &(i, _, sign) in &o.population_links {
                    out[i] += b
                        * weight
                        * sign
                        * self.config.density_kg_m3
                        * self.config.spacing_m.powi(3)
                        / p.scales.fluid_step;
                }
            } else if o.reads_cells() {
                o.vjp(
                    &p.scales,
                    &rho,
                    &u,
                    &self.fluid,
                    b * weight,
                    &mut rb,
                    &mut ub,
                );
            }
        }
        let mut tb = -self
            .observables
            .iter()
            .zip(samples.iter().zip(sample_bar))
            .map(|(o, (v, b))| v * b * f64::from(o.degree) / time_scale)
            .sum::<f64>();
        for x in 0..self.cells() {
            if !self.fluid[x] {
                continue;
            }
            let j = std::array::from_fn::<_, 3, _>(|d| (u[x][d] - p.cell.accel[d] * 0.5) * rho[x]);
            let rbar = rb[x]
                - ub[x]
                    .iter()
                    .zip(j)
                    .map(|(b, j)| b * j / (rho[x] * rho[x]))
                    .sum::<f64>();
            for q in 0..Q {
                out[q * self.cells() + x] +=
                    rbar + (0..3).map(|d| ub[x][d] * L::CF[q][d] / rho[x]).sum::<f64>();
            }
            tb += (0..3)
                .map(|d| ub[x][d] * p.cell.accel[d] / time_scale)
                .sum::<f64>();
        }
        Ok((out, tb))
    }
}

pub enum AnyCoupledMacroSession<'a> {
    D2Q9(CoupledMacroSession<'a, 9, super::super::lattice::D2Q9>),
    D3Q19(CoupledMacroSession<'a, 19, super::super::lattice::D3Q19>),
    D3Q27(CoupledMacroSession<'a, 27, super::super::lattice::D3Q27>),
}

impl AnyCoupledMacroSession<'_> {
    pub fn tick(&mut self, state: &[f64]) -> CaeResult<Vec<f64>> {
        match self {
            Self::D2Q9(s) => s.tick(state),
            Self::D3Q19(s) => s.tick(state),
            Self::D3Q27(s) => s.tick(state),
        }
    }
    pub fn tick_with_incoming(
        &mut self,
        state: &[f64],
        incoming: &[(usize, Vec<f64>)],
    ) -> CaeResult<Vec<f64>> {
        match self {
            Self::D2Q9(s) => s.tick_with_incoming(state, incoming),
            Self::D3Q19(s) => s.tick_with_incoming(state, incoming),
            Self::D3Q27(s) => s.tick_with_incoming(state, incoming),
        }
    }
    pub fn accept_exchange(&mut self, state: &[f64]) -> CaeResult<()> {
        match self {
            Self::D2Q9(s) => s.accept_exchange(state),
            Self::D3Q19(s) => s.accept_exchange(state),
            Self::D3Q27(s) => s.accept_exchange(state),
        }
    }
    pub fn finish(self, initial: &[f64], state: Vec<f64>) -> CaeResult<SubcycleOutput> {
        match self {
            Self::D2Q9(s) => s.finish(initial, state),
            Self::D3Q19(s) => s.finish(initial, state),
            Self::D3Q27(s) => s.finish(initial, state),
        }
    }
}

impl AnyMovingLbm {
    pub fn coupled_macro_session(
        &self,
        n: usize,
        start: &[f64],
        end: &[f64],
        design: &[f64],
        time_scale: f64,
    ) -> CaeResult<AnyCoupledMacroSession<'_>> {
        Ok(match self {
            Self::D2Q9(f) => AnyCoupledMacroSession::D2Q9(
                f.coupled_macro_session(n, start, end, design, time_scale)?,
            ),
            Self::D3Q19(f) => AnyCoupledMacroSession::D3Q19(
                f.coupled_macro_session(n, start, end, design, time_scale)?,
            ),
            Self::D3Q27(f) => AnyCoupledMacroSession::D3Q27(
                f.coupled_macro_session(n, start, end, design, time_scale)?,
            ),
        })
    }
    pub fn coupled_tick(&self, input: CoupledTickInput<'_>) -> CaeResult<SubcycleOutput> {
        dispatch!(self,f=>f.coupled_tick(input))
    }
    pub fn coupled_tick_tangent(
        &self,
        input: CoupledTickInput<'_>,
        direction: CoupledTickDirection<'_>,
    ) -> CaeResult<SubcycleTangentOutput> {
        dispatch!(self,f=>f.coupled_tick_tangent(input,direction))
    }
    pub fn coupled_tick_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleBars> {
        dispatch!(self,f=>f.coupled_tick_adjoint(input,state_bar,flux_bar,sample_bar))
    }
    pub fn coupled_tick_with_incoming(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
    ) -> CaeResult<SubcycleOutput> {
        dispatch!(self,f=>f.coupled_tick_with_incoming(input,incoming))
    }
    pub fn coupled_tick_with_incoming_tangent(
        &self,
        input: CoupledTickInput<'_>,
        direction: CoupledTickDirection<'_>,
        incoming: &[(usize, Vec<f64>)],
        incoming_direction: &[(usize, Vec<f64>)],
    ) -> CaeResult<SubcycleTangentOutput> {
        dispatch!(self,f=>f.coupled_tick_with_incoming_tangent(input,direction,incoming,incoming_direction))
    }
    pub fn coupled_tick_with_incoming_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<CoupledIncomingBars> {
        dispatch!(self,f=>f.coupled_tick_with_incoming_adjoint(input,incoming,state_bar,flux_bar,sample_bar))
    }
    pub fn coupled_tick_check_collision_exchange_ports(
        &self,
        input: CoupledTickInput<'_>,
        cells: &[usize],
    ) -> CaeResult<()> {
        dispatch!(self, f => f.coupled_tick_check_collision_exchange_ports(input, cells))
    }

    pub fn coupled_tick_pre_collision_rates(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        cells: &[usize],
    ) -> CaeResult<Vec<(usize, f64)>> {
        dispatch!(self,f=>f.coupled_tick_pre_collision_rates(input,incoming,cells))
    }
    pub fn coupled_tick_pre_collision_rates_tangent(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        cells: &[usize],
        previous_direction: &[f64],
        incoming_direction: &[(usize, Vec<f64>)],
        time_direction: f64,
    ) -> CaeResult<Vec<(usize, f64)>> {
        dispatch!(self,f=>f.coupled_tick_pre_collision_rates_tangent(input,incoming,cells,previous_direction,incoming_direction,time_direction))
    }
    pub fn coupled_tick_pre_collision_rates_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        incoming: &[(usize, Vec<f64>)],
        rate_bar: &[(usize, f64)],
    ) -> CaeResult<CoupledRelaxationBars> {
        dispatch!(self,f=>f.coupled_tick_pre_collision_rates_adjoint(input,incoming,rate_bar))
    }
    pub fn coupled_tick_incoming(
        &self,
        input: CoupledTickInput<'_>,
        cells: &[usize],
    ) -> CaeResult<Vec<(usize, Vec<f64>)>> {
        dispatch!(self,f=>f.coupled_tick_incoming(input,cells))
    }
    pub fn coupled_tick_incoming_tangent(
        &self,
        input: CoupledTickInput<'_>,
        cells: &[usize],
        previous_direction: &[f64],
    ) -> CaeResult<Vec<(usize, Vec<f64>)>> {
        dispatch!(self,f=>f.coupled_tick_incoming_tangent(input,cells,previous_direction))
    }
    pub fn coupled_tick_incoming_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        bars: &[(usize, Vec<f64>)],
    ) -> CaeResult<Vec<f64>> {
        dispatch!(self,f=>f.coupled_tick_incoming_adjoint(input,bars))
    }
    pub fn coupled_interface_links(
        &self,
        excluded: &[usize],
    ) -> CaeResult<Vec<CoupledInterfaceLink>> {
        dispatch!(self,f=>f.coupled_interface_links(excluded))
    }
    pub fn coupled_tick_reflection(
        &self,
        input: CoupledTickInput<'_>,
        links: &[CoupledInterfaceLink],
    ) -> CaeResult<Vec<(usize, [f64; 3])>> {
        dispatch!(self,f=>f.coupled_tick_reflection(input,links))
    }
    pub fn coupled_tick_reflection_tangent(
        &self,
        input: CoupledTickInput<'_>,
        links: &[CoupledInterfaceLink],
        previous_direction: &[f64],
        time_direction: f64,
    ) -> CaeResult<Vec<(usize, [f64; 3])>> {
        dispatch!(self,f=>f.coupled_tick_reflection_tangent(input,links,previous_direction,time_direction))
    }
    pub fn coupled_tick_reflection_adjoint(
        &self,
        input: CoupledTickInput<'_>,
        links: &[CoupledInterfaceLink],
        reflection_bar: &[(usize, [f64; 3])],
    ) -> CaeResult<CoupledReflectionBars> {
        dispatch!(self,f=>f.coupled_tick_reflection_adjoint(input,links,reflection_bar))
    }
    pub fn post_exchange_samples(
        &self,
        n: usize,
        k: usize,
        state: &[f64],
        time_scale: f64,
    ) -> CaeResult<Vec<f64>> {
        dispatch!(self,f=>f.post_exchange_samples(n,k,state,time_scale))
    }
    pub fn post_exchange_samples_tangent(
        &self,
        n: usize,
        k: usize,
        state: &[f64],
        time_scale: f64,
        dstate: &[f64],
        dtime: f64,
    ) -> CaeResult<Vec<f64>> {
        dispatch!(self,f=>f.post_exchange_samples_tangent(n,k,state,time_scale,dstate,dtime))
    }
    pub fn post_exchange_samples_adjoint(
        &self,
        n: usize,
        k: usize,
        state: &[f64],
        time_scale: f64,
        sample_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, f64)> {
        dispatch!(self,f=>f.post_exchange_samples_adjoint(n,k,state,time_scale,sample_bar))
    }
}
