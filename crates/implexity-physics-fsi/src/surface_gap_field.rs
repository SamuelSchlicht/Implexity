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

use crate::narrow_gap::ContactParameters;
use crate::surface_gap::SurfaceGapKinematics;
use crate::surface_gap_history::{GapFaceConnection, OrthogonalFacePolicy};
use implexity_physics_lbm::moving::{carrier::LagrangianCarrier, field::MovingLbmConfig};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeGapLattice {
    D3Q19,
    D3Q27,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceGapFieldPolicy {
    pub normal_axis: usize,
    pub envelope_cell_bounds: [[usize; 3]; 2],
    pub geometry_tolerance_m: f64,
    pub partition_absolute_tolerance_m3: f64,
    pub partition_relative_tolerance: f64,
    pub maximum_aspect_ratio: f64,
    pub maximum_gap_slope: f64,
    pub maximum_reduced_reynolds: f64,
    pub maximum_unsteady_inertia: f64,
    pub flow_timescale_s: f64,
    pub absolute_gas_pressure_pa: f64,
    pub maximum_gas_pressure_ratio: f64,
}

pub struct SurfaceGapFieldConfig {
    pub bulk: MovingLbmConfig,
    pub lattice: NativeGapLattice,
    pub original_carrier: Arc<dyn LagrangianCarrier>,
    pub geometric_owner: Option<Arc<crate::surface_gap_geometry_owner::NativeGapGeometryOwner>>,
    pub removed_carrier_points: Vec<usize>,
    pub kinematics: SurfaceGapKinematics,
    pub initial_trace: Vec<f64>,
    pub excluded_cells: Vec<usize>,
    pub ports: Vec<Vec<(usize, f64)>>,
    pub connections: Vec<GapFaceConnection>,
    pub orthogonal_face_policy: OrthogonalFacePolicy,
    pub contact: ContactParameters,
    pub policy: SurfaceGapFieldPolicy,
}

pub fn sample_names() -> Vec<String> {
    [
        "surface_gap_mean_pressure_Pa",
        "surface_gap_net_flow_m3_s",
        "surface_gap_contact_dissipation_W",
        "surface_gap_viscous_dissipation_W",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

use crate::surface_gap::{dissipative_loads, elastic_contact_loads, pressure_loads};
use crate::surface_gap_envelope::GapEnvelope;
use crate::surface_gap_history::{
    orthogonal_face, pressure_driven_skin_exchange, solve_gas_volume_feedback,
};
use crate::surface_gap_transport::boundary_transport_pressure_profile;
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::{
    carrier_filter::FilteredCarrier,
    field::{AnyMovingLbm, MovingLbm},
    lattice::{Lattice, D3Q19, D3Q27},
    port_exchange::PortExchange,
    psm::CouplingLaw,
};
use implexity_solve::multirate_coupling::{
    SubcycleCotangent, SubcycleRecord, SubcycleTangent, SubcycledField,
};
use implexity_solve::time_stepper::StepParameters;

pub struct SurfaceGapField {
    config: SurfaceGapFieldConfig,
    bulk: AnyMovingLbm,
    filtered: Arc<FilteredCarrier>,
    exchange: PortExchange,
    port_cells: Vec<usize>,
    names: Vec<String>,
    patch: SurfaceGapKinematics,
    trace_map: Vec<usize>,
    design_map: Vec<usize>,
}

struct LocalOutput<S> {
    volume: Vec<S>,
    flux: Vec<S>,
    increments: Vec<[S; 4]>,
    samples: Vec<S>,
    ledger: std::collections::BTreeMap<String, S>,
}

fn fail(message: &str) -> CaeError {
    CaeError::contract(message)
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn sub<S: Scalar>(a: [S; 3], b: [S; 3]) -> [S; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn norm<S: Scalar>(a: [S; 3]) -> S {
    dot(a, a).sqrt()
}

impl SurfaceGapField {
    pub fn new(config: SurfaceGapFieldConfig) -> CaeResult<Self> {
        let nt = config.original_carrier.trace_size();
        let nd = config.original_carrier.design_size();
        let policy = &config.policy;
        if config.bulk.substeps != 1
            || !config.bulk.observables.is_empty()
            || !matches!(
                config.bulk.coupling_law,
                CouplingLaw::Psm | CouplingLaw::PsmSuperposition
            )
            || config.kinematics.force_size() != nt
            || config.kinematics.state_size() != nt
            || config.kinematics.design_size() != nd
            || config.initial_trace.len() != nt
            || config.initial_trace.iter().any(|v| !v.is_finite())
            || policy.normal_axis >= 3
            || ![
                policy.geometry_tolerance_m,
                policy.maximum_aspect_ratio,
                policy.maximum_gap_slope,
                policy.maximum_reduced_reynolds,
                policy.maximum_unsteady_inertia,
                policy.flow_timescale_s,
                policy.absolute_gas_pressure_pa,
                policy.maximum_gas_pressure_ratio,
            ]
            .iter()
            .all(|v| v.is_finite() && *v > 0.)
            || ![
                policy.partition_absolute_tolerance_m3,
                policy.partition_relative_tolerance,
            ]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.)
            || config.connections.iter().any(|c| {
                c.left >= config.kinematics.cell_count()
                    || c.left_edge >= 3
                    || c.right
                        .is_some_and(|r| r[0] >= config.kinematics.cell_count() || r[1] >= 3)
                    || c.port.is_some_and(|p| p >= config.ports.len())
            })
        {
            return Err(fail("invalid native surface-gap field configuration"));
        }
        let filtered = Arc::new(FilteredCarrier::new(
            config.original_carrier.clone(),
            config.removed_carrier_points.clone(),
        )?);
        let bulk = match config.lattice {
            NativeGapLattice::D3Q19 => AnyMovingLbm::D3Q19(MovingLbm::<19, D3Q19>::new(
                config.bulk.clone(),
                filtered.clone(),
            )?),
            NativeGapLattice::D3Q27 => AnyMovingLbm::D3Q27(MovingLbm::<27, D3Q27>::new(
                config.bulk.clone(),
                filtered.clone(),
            )?),
        };
        let cells = config.bulk.solid_mask.len();
        let exchange = PortExchange::new(
            cells,
            config.excluded_cells.clone(),
            config.ports.clone(),
            config.bulk.spacing_m,
            config.bulk.density_kg_m3,
            config.bulk.macro_step_s,
        )?;
        exchange.validate_native_mask(&config.bulk.solid_mask)?;
        let mut port_cells = config
            .ports
            .iter()
            .flat_map(|p| p.iter().map(|(c, _)| *c))
            .collect::<Vec<_>>();
        port_cells.sort_unstable();
        port_cells.dedup();
        let (patch, trace_map, design_map) = config.kinematics.compact()?;
        Ok(Self {
            patch,
            trace_map,
            design_map,
            config,
            bulk,
            filtered,
            exchange,
            port_cells,
            names: sample_names(),
        })
    }
    fn frame(&self) -> &implexity_physics_lbm::moving::pushforward::Frame {
        match &self.bulk {
            AnyMovingLbm::D3Q19(b) => b.frame(),
            AnyMovingLbm::D3Q27(b) => b.frame(),
            _ => unreachable!(),
        }
    }
    fn population_size(&self) -> usize {
        self.bulk.state_size()
    }
    fn check(
        &self,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<std::collections::BTreeMap<String, f64>> {
        if previous.len() != self.state_size()
            || start.len() != self.trace_size()
            || end.len() != self.trace_size()
            || p.design.len() != self.design_size()
            || !p.time_scale.is_finite()
            || p.time_scale <= 0.
            || previous
                .iter()
                .chain(start)
                .chain(end)
                .chain(p.design)
                .any(|v| !v.is_finite())
            || previous[self.population_size()..].iter().any(|v| *v <= 0.)
        {
            return Err(fail("invalid surface-gap history input"));
        }
        self.admit(start, p.design)?;
        let midpoint: Vec<f64> = start
            .iter()
            .zip(end)
            .map(|(a, b)| 0.5 * a + 0.5 * b)
            .collect();
        self.admit(&midpoint, p.design)?;
        self.admit(end, p.design)
    }
    fn admit(
        &self,
        trace: &[f64],
        design: &[f64],
    ) -> CaeResult<std::collections::BTreeMap<String, f64>> {
        let current = self.config.kinematics.geometry_from_trace(trace, design)?;
        let initial = self
            .config
            .kinematics
            .geometry_from_trace(&self.config.initial_trace, design)?;
        let p = &self.config.policy;
        let volume = GapEnvelope {
            normal_axis: p.normal_axis,
            cell_bounds: p.envelope_cell_bounds,
            tolerance_m: p.geometry_tolerance_m,
        }
        .validate(
            &self.config.bulk,
            &self.config.excluded_cells,
            &initial,
            &current,
        )?;
        let mut ledger = std::collections::BTreeMap::new();
        if let Some(owner) = &self.config.geometric_owner {
            let bounds = std::array::from_fn(|side| {
                std::array::from_fn(|axis| {
                    self.config.bulk.origin_m[axis]
                        + p.envelope_cell_bounds[side][axis] as f64 * self.config.bulk.spacing_m
                })
            });
            let sharp = owner.validate(
                trace,
                design,
                bounds,
                p.normal_axis,
                &current,
                p.partition_absolute_tolerance_m3,
                p.partition_relative_tolerance,
            )?;
            let diffuse = self.filtered.volume_partition(
                trace,
                design,
                self.frame(),
                &self.config.excluded_cells,
                &self.config.bulk.solid_mask,
                self.config.bulk.saturation_width,
                volume,
            )?;
            ledger.insert(
                "surface_gap_sharp_partition_defect_m3".into(),
                sharp.volume_defect_m3,
            );
            ledger.insert(
                "surface_gap_diffuse_partition_difference_m3".into(),
                diffuse.unrepresented_volume_m3,
            );
            ledger.insert(
                "surface_gap_sharp_solid_volume_m3".into(),
                sharp.solid_volumes_m3.iter().sum(),
            );
        } else {
            let diffuse = self.filtered.validate_volume_partition(
                trace,
                design,
                self.frame(),
                &self.config.excluded_cells,
                &self.config.bulk.solid_mask,
                self.config.bulk.saturation_width,
                volume,
                p.partition_absolute_tolerance_m3,
                p.partition_relative_tolerance,
            )?;
            ledger.insert(
                "surface_gap_diffuse_partition_defect_m3".into(),
                diffuse.unrepresented_volume_m3,
            );
        }
        Ok(ledger)
    }
    fn moments<S: Scalar, const Q: usize, L: Lattice<Q>>(&self, state: &[S]) -> CaeResult<Vec<S>> {
        let n = self.config.bulk.solid_mask.len();
        if state.len() != n * Q {
            return Err(fail("surface-gap bulk population shape"));
        }
        Ok(self
            .port_cells
            .iter()
            .flat_map(|c| {
                let rho = (0..Q).map(|i| state[i * n + *c]).sum::<S>();
                let m: [S; 3] = std::array::from_fn(|a| {
                    (0..Q).map(|i| state[i * n + *c] * L::CF[i][a]).sum::<S>()
                });
                [rho, m[0], m[1], m[2]]
            })
            .collect())
    }
    fn bulk_moments<S: Scalar>(&self, state: &[S]) -> CaeResult<Vec<S>> {
        match self.config.lattice {
            NativeGapLattice::D3Q19 => self.moments::<S, 19, D3Q19>(state),
            NativeGapLattice::D3Q27 => self.moments::<S, 27, D3Q27>(state),
        }
    }
    fn moment_bars<const Q: usize, L: Lattice<Q>>(&self, bars: &[f64], population: &mut [f64]) {
        let n = self.config.bulk.solid_mask.len();
        for (j, c) in self.port_cells.iter().enumerate() {
            for i in 0..Q {
                population[i * n + *c] += bars[4 * j]
                    + (0..3)
                        .map(|a| bars[4 * j + a + 1] * L::CF[i][a])
                        .sum::<f64>();
            }
        }
    }
    fn local<S: Scalar>(
        &self,
        old_volume: &[S],
        start: &[S],
        end: &[S],
        design: &[S],
        scale: S,
        moments: &[S],
    ) -> CaeResult<LocalOutput<S>> {
        if !scale.value().is_finite()
            || scale.value() <= 0.
            || old_volume.len() != self.patch.cell_count()
            || moments.len() != 4 * self.port_cells.len()
        {
            return Err(fail("surface-gap local input shape or scale"));
        }
        let cfg = &self.config.bulk;
        let policy = &self.config.policy;
        let dt = scale * cfg.macro_step_s;
        let rho = S::from_f64(cfg.density_kg_m3);
        let mu = S::from_f64(cfg.density_kg_m3 * cfg.kinematic_viscosity_m2_s);
        let endpoint = self.patch.geometry_from_trace(end, design)?;
        let midpoint: Vec<S> = start
            .iter()
            .zip(end)
            .map(|(a, b)| (*a + *b) * 0.5)
            .collect();
        let geometry = self.patch.geometry_from_trace(&midpoint, design)?;
        let velocity = self.patch.vertex_velocities(end, start, design, dt)?;
        let capacity = endpoint.iter().map(|g| g.volume_m3).collect::<Vec<_>>();
        let c = S::from_f64(cfg.spacing_m / cfg.macro_step_s) / scale;
        let pressure_scale = rho * c * c / 3.;
        let mut base_pressure = vec![S::zero(); self.config.ports.len()];
        let mut port_velocity = vec![[S::zero(); 3]; self.config.ports.len()];
        for (p, port) in self.config.ports.iter().enumerate() {
            for (cell, w) in port {
                let j = self
                    .port_cells
                    .binary_search(cell)
                    .map_err(|_| fail("surface-gap port ownership"))?;
                let density = moments[4 * j];
                if density.value() <= 0. || !density.value().is_finite() {
                    return Err(fail("surface-gap bulk port density"));
                }
                base_pressure[p] += (density - S::one()) * pressure_scale * *w;
                for a in 0..3 {
                    port_velocity[p][a] += (moments[4 * j + a + 1] / density * c
                        + dt * (0.5 * cfg.body_acceleration_m_s2[a]))
                        * *w;
                }
            }
        }
        let gain = self
            .exchange
            .pressure_mass_jacobian()
            .iter()
            .map(|g| dt * rho * *g / (scale * scale))
            .collect::<Vec<_>>();
        let mut faces = Vec::with_capacity(self.config.connections.len());
        for con in &self.config.connections {
            let right = con.right.map(|r| (r[0], &geometry[r[0]], r[1]));
            faces.push(orthogonal_face(
                con.left,
                &geometry[con.left],
                con.left_edge,
                right,
                con.port,
                mu,
                [S::zero(); 3],
                S::one(),
                &self.config.orthogonal_face_policy,
            )?);
        }
        let solved =
            solve_gas_volume_feedback(&capacity, old_volume, &base_pressure, &gain, &faces, dt)?;
        if solved
            .cell_pressure_pa
            .iter()
            .chain(&solved.port_pressure_pa)
            .any(|v| {
                v.value().abs() / policy.absolute_gas_pressure_pa
                    > policy.maximum_gas_pressure_ratio
                    || policy.absolute_gas_pressure_pa + v.value() <= 0.0
            })
        {
            return Err(fail(
                "surface-gap cell or port pressure exceeds incompressible gas validity",
            ));
        }
        let shear = pressure_driven_skin_exchange(
            &geometry,
            &solved.cell_pressure_pa,
            &solved.port_pressure_pa,
            &self.config.connections,
            mu,
            &self.config.orthogonal_face_policy,
        )?;
        let transport = boundary_transport_pressure_profile(
            &geometry,
            &velocity,
            &self.config.connections,
            &solved.port_pressure_pa,
            &solved.face_flux_m3_s,
            &port_velocity,
            rho,
            dt,
            &shear.pressure_gradient_pa_m,
            mu,
        )?;
        let mut force = vec![[[S::zero(); 3]; 6]; geometry.len()];
        let mut contact_loss = S::zero();
        let mut slip_loss = S::zero();
        let mut contact_energy = S::zero();
        let mut transient_bound = S::zero();
        let mut wall_fluid_force = [S::zero(); 3];
        for (i, g) in geometry.iter().enumerate() {
            let pressure = solved.cell_pressure_pa[i];
            let cl = g.vertices_m;

            let height: [f64; 3] =
                std::array::from_fn(|j| dot(sub(cl[j + 3], cl[j]), g.normal).value());
            let h = height.iter().copied().fold(0., f64::max);
            let midvertex: [[S; 3]; 3] =
                std::array::from_fn(|j| std::array::from_fn(|a| (cl[j][a] + cl[j + 3][a]) * 0.5));
            let longest_edge = (0..3)
                .map(|e| norm(sub(midvertex[(e + 1) % 3], midvertex[e])).value())
                .fold(0.0, f64::max);
            let length = 2.0 * g.area_m2.value() / longest_edge;
            let edge0 = sub(midvertex[1], midvertex[0]);
            let edge1 = sub(midvertex[2], midvertex[0]);
            let a = dot(edge0, g.tangent[0]).value();
            let b = dot(edge0, g.tangent[1]).value();
            let c = dot(edge1, g.tangent[0]).value();
            let d = dot(edge1, g.tangent[1]).value();
            let det = a * d - b * c;
            let gx = ((height[1] - height[0]) * d - (height[2] - height[0]) * b) / det;
            let gy = (a * (height[2] - height[0]) - c * (height[1] - height[0])) / det;
            if !gx.is_finite()
                || !gy.is_finite()
                || (gx * gx + gy * gy).sqrt() > policy.maximum_gap_slope
            {
                return Err(fail(
                    "surface-gap height slope exceeds lubrication validity",
                ));
            }
            if h / length > policy.maximum_aspect_ratio
                || cfg.density_kg_m3 * h * h / (mu.value() * policy.flow_timescale_s)
                    > policy.maximum_unsteady_inertia
            {
                return Err(fail(
                    "surface-gap geometry or unsteady inertia exceeds reduced-flow validity",
                ));
            }
            let pforces = pressure_loads(g, pressure)?;
            let elastic = elastic_contact_loads(
                g,
                self.config.contact.penalty_pa_per_m,
                self.config.contact.asperity_height_m,
            )?;
            let diss = dissipative_loads(
                g,
                velocity[i],
                self.config.contact.damping_pa_s_per_m,
                mu.value(),
                g.volume_m3,
                self.config.contact.asperity_height_m,
            )?;
            contact_loss += diss.normal_dissipation_w;
            slip_loss += diss.tangential_dissipation_w;
            contact_energy += elastic.energy_j;
            for node in 0..6 {
                for a in 0..3 {
                    let fluid = pforces.cover_n[node][a] + shear.cover_force_n[i][node][a];
                    wall_fluid_force[a] += fluid;
                    force[i][node][a] = fluid + elastic.force_n[node][a] + diss.force_n[node][a];
                }
            }
            let mean: [S; 3] = std::array::from_fn(|a| {
                velocity[i].iter().map(|v| v[a]).sum::<S>() / 6.
                    - shear.pressure_gradient_pa_m[i][a] * g.hydraulic_gap_m * g.hydraulic_gap_m
                        / (mu * 12.)
            });
            let mean_norm = if dot(mean, mean).value() == 0. {
                S::zero()
            } else {
                norm(mean)
            };
            transient_bound += rho * g.volume_m3 * mean_norm / policy.flow_timescale_s;
        }
        for (con, q) in self.config.connections.iter().zip(&solved.face_flux_m3_s) {
            let g = &geometry[con.left];
            let e = con.left_edge;
            let j = (e + 1) % 3;
            let a = std::array::from_fn(|k| (g.vertices_m[e][k] + g.vertices_m[e + 3][k]) * 0.5);
            let b = std::array::from_fn(|k| (g.vertices_m[j][k] + g.vertices_m[j + 3][k]) * 0.5);
            let length = norm(sub(b, a)).value();
            let ha = dot(sub(g.vertices_m[e + 3], g.vertices_m[e]), g.normal).value();
            let hb = dot(sub(g.vertices_m[j + 3], g.vertices_m[j]), g.normal).value();
            let area = 0.5 * (ha + hb) * length;
            let speed = q.value().abs() / area;
            if cfg.density_kg_m3 * speed * ha.max(hb).powi(2) / (mu.value() * length)
                > policy.maximum_reduced_reynolds
            {
                return Err(fail(
                    "surface-gap solved flux exceeds creeping-flow validity",
                ));
            }
        }
        let flux = self.patch.scatter_cover(&force, design)?;
        let total = capacity.iter().copied().sum::<S>();
        let mean_p = solved
            .cell_pressure_pa
            .iter()
            .zip(&capacity)
            .map(|(p, v)| *p * *v)
            .sum::<S>()
            / total;
        let net_q = transport.increments.iter().map(|i| i[0]).sum::<S>() / (dt * rho);
        let mut ledger = std::collections::BTreeMap::new();
        ledger.insert("surface_gap_contact_energy_J".into(), contact_energy);
        ledger.insert("surface_gap_cap_power_W".into(), transport.cap_power_w);
        ledger.insert(
            "surface_gap_advective_inertia_bound_N".into(),
            transport.advective_momentum_bound_n,
        );
        ledger.insert(
            "surface_gap_transient_inertia_bound_N".into(),
            transient_bound,
        );
        ledger.insert(
            "surface_gap_mass_exchange_defect_kg".into(),
            (total - old_volume.iter().copied().sum::<S>()) * rho
                + transport.increments.iter().map(|i| i[0]).sum::<S>(),
        );
        for (a, name) in ["x", "y", "z"].iter().enumerate() {
            ledger.insert(
                format!("surface_gap_creeping_force_residual_{name}_N"),
                wall_fluid_force[a] + transport.cap_force_n[a],
            );
            ledger.insert(
                format!("surface_gap_advective_momentum_flux_{name}_N"),
                transport.advective_momentum_flux_n[a],
            );
        }
        let increments = transport
            .increments
            .into_iter()
            .map(|i| [i[0], i[1] * scale, i[2] * scale, i[3] * scale])
            .collect();
        Ok(LocalOutput {
            volume: capacity,
            flux,
            increments,
            samples: vec![
                mean_p,
                net_q,
                contact_loss,
                slip_loss + shear.hydraulic_dissipation_w,
            ],
            ledger,
        })
    }
}

impl SurfaceGapField {
    fn apply<S: Scalar>(&self, state: &[S], increments: &[[S; 4]]) -> CaeResult<Vec<S>> {
        let result = match self.config.lattice {
            NativeGapLattice::D3Q19 => self.exchange.apply::<S, 19, D3Q19>(state, increments)?,
            NativeGapLattice::D3Q27 => self.exchange.apply::<S, 27, D3Q27>(state, increments)?,
        };
        let cells = self.config.bulk.solid_mask.len();
        if result.iter().enumerate().any(|(i, value)| {
            !self.config.bulk.solid_mask[i % cells]
                && (!value.value().is_finite() || value.value() <= 0.0)
        }) {
            return Err(CaeError::convergence(
                "surface-gap source requires positive finite native populations",
            ));
        }
        Ok(result)
    }
    fn source_vjp(&self, bar: &[f64]) -> CaeResult<(Vec<f64>, Vec<[f64; 4]>)> {
        match self.config.lattice {
            NativeGapLattice::D3Q19 => self.exchange.apply_vjp::<19, D3Q19>(bar),
            NativeGapLattice::D3Q27 => self.exchange.apply_vjp::<27, D3Q27>(bar),
        }
    }
    fn admit_populations<S: Scalar, const Q: usize, L: Lattice<Q>>(
        &self,
        state: &[S],
        scale: f64,
    ) -> CaeResult<(f64, f64, f64)> {
        let cfg = &self.config.bulk;
        let n = cfg.solid_mask.len();
        let dt = cfg.macro_step_s * scale;
        let limit = cfg.lattice_velocity_limit.min(cfg.mach_limit / 3f64.sqrt());
        let mut kinetic = 0.;
        let mut minimum = f64::INFINITY;
        let mut max_speed: f64 = 0.;
        for cell in 0..n {
            if cfg.solid_mask[cell] {
                continue;
            }
            let rho = (0..Q).map(|i| state[i * n + cell].value()).sum::<f64>();
            for i in 0..Q {
                minimum = minimum.min(state[i * n + cell].value());
            }
            if !rho.is_finite() || rho <= 0. {
                return Err(CaeError::convergence(
                    "surface-gap source exhausts native bulk density",
                ));
            }
            let u: [f64; 3] = std::array::from_fn(|a| {
                (0..Q)
                    .map(|i| state[i * n + cell].value() * L::CF[i][a])
                    .sum::<f64>()
                    / rho
                    + 0.5 * cfg.body_acceleration_m_s2[a] * dt * dt / cfg.spacing_m
            });
            let speed = dot(u, u).sqrt();
            if !speed.is_finite() || speed >= limit {
                return Err(CaeError::convergence(
                    "surface-gap source exceeds native fluid velocity bounds",
                ));
            }
            max_speed = max_speed.max(speed);
            kinetic += 0.5
                * rho
                * dot(u, u)
                * cfg.density_kg_m3
                * cfg.spacing_m.powi(3)
                * (cfg.spacing_m / dt).powi(2);
        }
        Ok((kinetic, minimum, max_speed * 3f64.sqrt()))
    }
    fn metrics_after_source<S: Scalar>(
        &self,
        state: &[S],
        scale: f64,
    ) -> CaeResult<(f64, f64, f64)> {
        match self.config.lattice {
            NativeGapLattice::D3Q19 => self.admit_populations::<S, 19, D3Q19>(state, scale),
            NativeGapLattice::D3Q27 => self.admit_populations::<S, 27, D3Q27>(state, scale),
        }
    }
    fn patch_trace<S: Copy>(&self, values: &[S]) -> Vec<S> {
        self.trace_map.iter().map(|i| values[*i]).collect()
    }
    fn patch_design<S: Copy>(&self, values: &[S]) -> Vec<S> {
        self.design_map.iter().map(|i| values[*i]).collect()
    }
    fn local_direction(
        &self,
        old: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        mom: &[f64],
        dold: &[f64],
        ds: &[f64],
        de: &[f64],
        dd: &[f64],
        dt: f64,
        dm: &[f64],
    ) -> CaeResult<LocalOutput<Dual<1>>> {
        let lift = |v: &[f64], d: &[f64]| {
            v.iter()
                .zip(d)
                .map(|(v, d)| Dual::new(*v, [*d]))
                .collect::<Vec<_>>()
        };
        self.local(
            &lift(old, dold),
            &lift(&self.patch_trace(start), &self.patch_trace(ds)),
            &lift(&self.patch_trace(end), &self.patch_trace(de)),
            &lift(&self.patch_design(p.design), &self.patch_design(dd)),
            Dual::new(p.time_scale, [dt]),
            &lift(mom, dm),
        )
    }
    fn direction_domain(
        &self,
        old: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        mom: &[f64],
    ) -> CaeResult<()> {
        let start = self.patch_trace(start);
        let end = self.patch_trace(end);
        let design = self.patch_design(p.design);
        let geometry = self.patch.geometry_from_trace(
            &start
                .iter()
                .zip(&end)
                .map(|(a, b)| (a + b) * 0.5)
                .collect::<Vec<_>>(),
            &design,
        )?;
        let velocity = self.patch.vertex_velocities(
            &end,
            &start,
            &design,
            p.time_scale * self.config.bulk.macro_step_s,
        )?;
        let capacity = self
            .patch
            .geometry_from_trace(&end, &design)?
            .into_iter()
            .map(|g| g.volume_m3)
            .collect::<Vec<_>>();
        let cfg = &self.config.bulk;
        let dt = p.time_scale * cfg.macro_step_s;
        let c = cfg.spacing_m / dt;
        let mut pressure = vec![0.; self.config.ports.len()];
        let mut bulk_velocity = vec![[0.; 3]; self.config.ports.len()];
        for (i, port) in self.config.ports.iter().enumerate() {
            for (cell, w) in port {
                let j = self
                    .port_cells
                    .binary_search(cell)
                    .map_err(|_| fail("gap derivative port layout"))?;
                pressure[i] += (mom[4 * j] - 1.) * cfg.density_kg_m3 * c * c / 3. * *w;
                for a in 0..3 {
                    bulk_velocity[i][a] += (mom[4 * j + a + 1] / mom[4 * j] * c
                        + 0.5 * cfg.body_acceleration_m_s2[a] * dt)
                        * *w;
                }
            }
        }
        let gain = self
            .exchange
            .pressure_mass_jacobian()
            .iter()
            .map(|g| dt * cfg.density_kg_m3 * g / (p.time_scale * p.time_scale))
            .collect::<Vec<_>>();
        let mu = cfg.density_kg_m3 * cfg.kinematic_viscosity_m2_s;
        let faces = self
            .config
            .connections
            .iter()
            .map(|con| {
                orthogonal_face(
                    con.left,
                    &geometry[con.left],
                    con.left_edge,
                    con.right.map(|r| (r[0], &geometry[r[0]], r[1])),
                    con.port,
                    mu,
                    [0.; 3],
                    1.,
                    &self.config.orthogonal_face_policy,
                )
            })
            .collect::<CaeResult<Vec<_>>>()?;
        let solution = solve_gas_volume_feedback(&capacity, old, &pressure, &gain, &faces, dt)?;
        for (g, v) in geometry.iter().zip(&velocity) {
            let h = g.local_separation_m[2];
            let contact = &self.config.contact;
            let rates = dissipative_loads(
                g,
                *v,
                contact.damping_pa_s_per_m,
                mu,
                g.volume_m3,
                contact.asperity_height_m,
            )?
            .local_rate_m_s;
            if h == contact.asperity_height_m
                && (contact.penalty_pa_per_m > 0. || contact.damping_pa_s_per_m > 0.)
                || h < contact.asperity_height_m
                    && contact.damping_pa_s_per_m > 0.
                    && rates[2] == 0.
            {
                return Err(fail(
                    "surface-gap contact derivative is undefined at a switching boundary",
                ));
            }
        }
        let shear = pressure_driven_skin_exchange(
            &geometry,
            &solution.cell_pressure_pa,
            &solution.port_pressure_pa,
            &self.config.connections,
            mu,
            &self.config.orthogonal_face_policy,
        )?;
        crate::surface_gap_transport::pressure_profile_derivative_domain(
            &geometry,
            &velocity,
            &self.config.connections,
            &solution.face_flux_m3_s,
            &bulk_velocity,
            &shear.pressure_gradient_pa_m,
            mu,
        )
    }
}

impl SubcycledField for SurfaceGapField {
    fn state_size(&self) -> usize {
        self.population_size() + self.config.kinematics.cell_count()
    }
    fn trace_size(&self) -> usize {
        self.config.original_carrier.trace_size()
    }
    fn design_size(&self) -> usize {
        self.config.original_carrier.design_size()
    }
    fn sample_names(&self) -> &[String] {
        &self.names
    }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        self.admit(&self.config.initial_trace, design)?;
        let mut state = SubcycledField::initial_state(&self.bulk, design)?;
        state.extend(
            self.config
                .kinematics
                .geometry_from_trace(&self.config.initial_trace, design)?
                .into_iter()
                .map(|g| g.volume_m3),
        );
        Ok(state)
    }
    fn initial_state_vjp(&self, design: &[f64], bar: &[f64]) -> CaeResult<Vec<f64>> {
        if bar.len() != self.state_size() || bar.iter().any(|v| !v.is_finite()) {
            return Err(fail("surface-gap initial cotangent shape"));
        }
        self.admit(&self.config.initial_trace, design)?;
        let nf = self.population_size();
        let mut result = SubcycledField::initial_state_vjp(&self.bulk, design, &bar[..nf])?;
        let trace = self
            .patch_trace(&self.config.initial_trace)
            .iter()
            .map(|v| Dual::<1>::constant(*v))
            .collect::<Vec<_>>();
        let local_design = self.patch_design(design);
        for (j, i) in self.design_map.iter().enumerate() {
            let mut d = local_design
                .iter()
                .map(|v| Dual::<1>::constant(*v))
                .collect::<Vec<_>>();
            d[j].eps[0] = 1.;
            let geometry = self.patch.geometry_from_trace(&trace, &d)?;
            result[*i] += geometry
                .iter()
                .zip(&bar[nf..])
                .map(|(g, b)| g.volume_m3.eps[0] * b)
                .sum::<f64>();
        }
        Ok(result)
    }
    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord> {
        let partition = self.check(previous, start, end, p)?;
        let nf = self.population_size();
        let mut native = SubcycledField::subcycle(&self.bulk, n, &previous[..nf], start, end, p)?;
        let local = self.local(
            &previous[nf..],
            &self.patch_trace(start),
            &self.patch_trace(end),
            &self.patch_design(p.design),
            p.time_scale,
            &self.bulk_moments(&native.state)?,
        )?;
        let mut state = self.apply(&native.state, &local.increments)?;
        let (kinetic, minimum, mach) = self.metrics_after_source(&state, p.time_scale)?;
        state.extend(local.volume);
        for (j, i) in self.trace_map.iter().enumerate() {
            native.flux[*i] += local.flux[j];
        }
        let gap_work = local
            .flux
            .iter()
            .enumerate()
            .map(|(j, f)| f * (end[self.trace_map[j]] - start[self.trace_map[j]]))
            .sum::<f64>();
        native.pairing += gap_work;
        native
            .ledger
            .insert("interface_work_J".into(), native.pairing);
        native.ledger.extend(partition);
        native.ledger.extend(local.ledger);
        native
            .ledger
            .insert("surface_gap_interface_work_J".into(), gap_work);
        native
            .ledger
            .insert("fluid_kinetic_energy_J".into(), kinetic);
        native.ledger.insert("min_population".into(), minimum);
        let maximum_mach = native
            .ledger
            .get("max_mach")
            .copied()
            .unwrap_or(0.0)
            .max(mach);
        native.ledger.insert("max_mach".into(), maximum_mach);
        native.state = state;
        native.samples = local.samples;
        Ok(native)
    }
    fn subcycle_tangent(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        dprev: &[f64],
        ds: &[f64],
        de: &[f64],
        dd: Option<&[f64]>,
        dt: f64,
    ) -> CaeResult<SubcycleTangent> {
        self.check(previous, start, end, p)?;
        if dprev.len() != self.state_size()
            || ds.len() != self.trace_size()
            || de.len() != self.trace_size()
            || dd.is_some_and(|d| d.len() != self.design_size())
            || dprev
                .iter()
                .chain(ds)
                .chain(de)
                .chain(dd.unwrap_or(&[]))
                .any(|v| !v.is_finite())
            || !dt.is_finite()
        {
            return Err(fail("surface-gap tangent shape or value"));
        }
        let nf = self.population_size();
        let native = SubcycledField::subcycle(&self.bulk, n, &previous[..nf], start, end, p)?;
        let tangent = SubcycledField::subcycle_tangent(
            &self.bulk,
            n,
            &previous[..nf],
            start,
            end,
            p,
            &dprev[..nf],
            ds,
            de,
            dd,
            dt,
        )?;
        let moments = self.bulk_moments(&native.state)?;
        self.direction_domain(&previous[nf..], start, end, p, &moments)?;
        let dm = self.bulk_moments(&tangent.state)?;
        let zero = vec![0.; self.design_size()];
        let local = self.local_direction(
            &previous[nf..],
            start,
            end,
            p,
            &moments,
            &dprev[nf..],
            ds,
            de,
            dd.unwrap_or(&zero),
            dt,
            &dm,
        )?;
        let state = native
            .state
            .iter()
            .zip(&tangent.state)
            .map(|(v, d)| Dual::new(*v, [*d]))
            .collect::<Vec<_>>();
        let sourced = self.apply(&state, &local.increments)?;
        self.metrics_after_source(&sourced, p.time_scale)?;
        let mut result = sourced.into_iter().map(|v| v.eps[0]).collect::<Vec<_>>();
        result.extend(local.volume.iter().map(|v| v.eps[0]));
        let mut flux = tangent.flux;
        for (j, i) in self.trace_map.iter().enumerate() {
            flux[*i] += local.flux[j].eps[0];
        }
        Ok(SubcycleTangent {
            state: result,
            flux,
            samples: local.samples.iter().map(|v| v.eps[0]).collect(),
        })
    }
    fn subcycle_adjoint(
        &self,
        n: usize,
        previous: &[f64],
        start: &[f64],
        end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent> {
        self.check(previous, start, end, p)?;
        if state_bar.len() != self.state_size()
            || flux_bar.len() != self.trace_size()
            || sample_bar.len() != self.names.len()
            || state_bar
                .iter()
                .chain(flux_bar)
                .chain(sample_bar)
                .any(|v| !v.is_finite())
        {
            return Err(fail("surface-gap cotangent shape or value"));
        }
        let nf = self.population_size();
        let native = SubcycledField::subcycle(&self.bulk, n, &previous[..nf], start, end, p)?;
        let moments = self.bulk_moments(&native.state)?;
        self.direction_domain(&previous[nf..], start, end, p, &moments)?;
        let primal = self.local(
            &previous[nf..],
            &self.patch_trace(start),
            &self.patch_trace(end),
            &self.patch_design(p.design),
            p.time_scale,
            &moments,
        )?;
        let sourced = self.apply(&native.state, &primal.increments)?;
        self.metrics_after_source(&sourced, p.time_scale)?;
        drop(sourced);
        let (mut population_bar, source_bar) = self.source_vjp(&state_bar[..nf])?;
        #[derive(Clone, Copy)]
        enum Input {
            Old(usize),
            Start(usize),
            End(usize),
            Design(usize),
            Moment(usize),
            Scale,
        }
        let active_trace: Vec<_> = (0..self.trace_map.len()).collect();
        let local_start = self.patch_trace(start);
        let local_end = self.patch_trace(end);
        let local_design = self.patch_design(p.design);
        let local_flux_bar = self.patch_trace(flux_bar);
        let mut inputs = (0..previous.len() - nf).map(Input::Old).collect::<Vec<_>>();
        inputs.extend(active_trace.iter().copied().map(Input::Start));
        inputs.extend(active_trace.iter().copied().map(Input::End));
        inputs.extend((0..self.design_map.len()).map(Input::Design));
        inputs.extend((0..moments.len()).map(Input::Moment));
        inputs.push(Input::Scale);
        let mut old_bar = vec![0.; previous.len() - nf];
        let mut start_bar = vec![0.; self.trace_map.len()];
        let mut end_bar = vec![0.; self.trace_map.len()];
        let mut design_bar = vec![0.; self.design_map.len()];
        let mut moment_bar = vec![0.; moments.len()];
        let mut scale_bar = 0.;
        for chunk in inputs.chunks(16) {
            let constant = |x: &[f64]| {
                x.iter()
                    .map(|v| Dual::<16>::constant(*v))
                    .collect::<Vec<_>>()
            };
            let mut old = constant(&previous[nf..]);
            let mut s = constant(&local_start);
            let mut e = constant(&local_end);
            let mut d = constant(&local_design);
            let mut m = constant(&moments);
            let mut t = Dual::<16>::constant(p.time_scale);
            for (k, input) in chunk.iter().enumerate() {
                match *input {
                    Input::Old(i) => old[i].eps[k] = 1.,
                    Input::Start(i) => s[i].eps[k] = 1.,
                    Input::End(i) => e[i].eps[k] = 1.,
                    Input::Design(i) => d[i].eps[k] = 1.,
                    Input::Moment(i) => m[i].eps[k] = 1.,
                    Input::Scale => t.eps[k] = 1.,
                }
            }
            let output = self.local(&old, &s, &e, &d, t, &m)?;
            for (k, input) in chunk.iter().enumerate() {
                let bar = output
                    .volume
                    .iter()
                    .zip(&state_bar[nf..])
                    .map(|(v, b)| v.eps[k] * b)
                    .sum::<f64>()
                    + output
                        .flux
                        .iter()
                        .zip(&local_flux_bar)
                        .map(|(v, b)| v.eps[k] * b)
                        .sum::<f64>()
                    + output
                        .samples
                        .iter()
                        .zip(sample_bar)
                        .map(|(v, b)| v.eps[k] * b)
                        .sum::<f64>()
                    + output
                        .increments
                        .iter()
                        .zip(&source_bar)
                        .map(|(v, b)| (0..4).map(|j| v[j].eps[k] * b[j]).sum::<f64>())
                        .sum::<f64>();
                if !bar.is_finite() {
                    return Err(fail(
                        "surface-gap transpose exceeds finite derivative domain",
                    ));
                }
                match *input {
                    Input::Old(i) => old_bar[i] = bar,
                    Input::Start(i) => start_bar[i] = bar,
                    Input::End(i) => end_bar[i] = bar,
                    Input::Design(i) => design_bar[i] = bar,
                    Input::Moment(i) => moment_bar[i] = bar,
                    Input::Scale => scale_bar = bar,
                }
            }
        }
        match self.config.lattice {
            NativeGapLattice::D3Q19 => {
                self.moment_bars::<19, D3Q19>(&moment_bar, &mut population_bar)
            }
            NativeGapLattice::D3Q27 => {
                self.moment_bars::<27, D3Q27>(&moment_bar, &mut population_bar)
            }
        }
        let mut native_bar = SubcycledField::subcycle_adjoint(
            &self.bulk,
            n,
            &previous[..nf],
            start,
            end,
            p,
            &population_bar,
            flux_bar,
            &[],
        )?;
        native_bar.previous.extend(old_bar);
        for (j, i) in self.trace_map.iter().enumerate() {
            native_bar.trace_start[*i] += start_bar[j];
        }
        for (j, i) in self.trace_map.iter().enumerate() {
            native_bar.trace_end[*i] += end_bar[j];
        }
        for (j, i) in self.design_map.iter().enumerate() {
            native_bar.design[*i] += design_bar[j];
        }
        native_bar.time_scale += scale_bar;
        Ok(native_bar)
    }
}
