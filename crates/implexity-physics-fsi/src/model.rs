// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::{CaeError, CaeResult};
use implexity_physics_lbm::moving::carrier::LagrangianCarrier;
use implexity_physics_lbm::moving::field::{AnyMovingLbm, MovingLbmConfig};
use implexity_physics_solid::soft::design::FieldMap;
use implexity_physics_solid::soft::model::{NodalPotential, SoftModel};
use implexity_physics_solid::soft::stepper::Loading;
use implexity_physics_solid::soft_fsi::field::{DesignLayout, SoftSolidField, SoftStepConfig, SoftStepCore};
use implexity_physics_solid::soft_fsi::observables::{Observables, SolidObservable};
use implexity_physics_solid::soft_fsi::potentials::{ElasticFoundation, RigidPlaneBarrier};
use implexity_physics_solid::soft_fsi::pushforward::PushforwardPoints;
use implexity_physics_solid::soft_fsi::voxel::{KuhnMesh, VoxelDesignMap, VoxelGrid, kuhn_tetrahedra};
use implexity_solve::multirate_coupling::{MultirateOptions, MultirateStepper};

use crate::carrier::SolidCarrier;
use crate::contact::RegionPlaneBarrier;
use crate::interface::FluidField;
use crate::json::in_box;
use crate::modifier::RemovalZoneModifier;
use crate::problem::FsiProblem;
use crate::problem::design::{mirror_voxel, voxel_centroids};
use crate::problem::solid::{NodeSelection, SolidSpec};

pub type FsiStepper<'m> = MultirateStepper<FluidField, SoftSolidField<'m>>;

#[derive(Clone, Debug)]
pub struct DesignChain {
    field: FieldMap,
    mirror: Option<Vec<usize>>,
    voxels: usize,
    bound: Option<Vec<f64>>,
}

impl DesignChain {
    fn new(problem: &FsiProblem) -> CaeResult<Self> {
        let grid = &problem.solid.grid;
        let d = &problem.design;
        let n = grid.voxel_count();
        let volume = grid.element_size_m.powi(3);
        let field = FieldMap::new(
            &voxel_centroids(grid),
            &vec![volume; n],
            d.filter_radius_m,
            d.region.clone(),
            d.protected_density.clone(),
            d.projection,
        )?;
        let mirror = d.mirror_axis.map(|a| (0..n).map(|v| mirror_voxel(grid, a, v)).collect());
        let bound = d
            .removal
            .as_ref()
            .map(|r| (0..n).map(|v| if r.removable[v] { r.reference[v] } else { 1.0 }).collect::<Vec<f64>>());
        Ok(Self { field, mirror, voxels: n, bound })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.voxels
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.voxels == 0
    }

    fn symmetrise(&self, x: &[f64]) -> Vec<f64> {
        match &self.mirror {
            None => x.to_vec(),
            Some(m) => (0..x.len()).map(|i| 0.5 * (x[i] + x[m[i]])).collect(),
        }
    }

    fn check(&self, x: &[f64]) -> CaeResult<()> {
        if x.len() != self.voxels || x.iter().any(|v| !(v.is_finite() && (0.0..=1.0).contains(v))) {
            return Err(CaeError::contract(format!(
                "model:control needs {} values in [0, 1] (one per reference voxel)",
                self.voxels
            )));
        }
        Ok(())
    }


    pub fn forward(&self, x: &[f64]) -> CaeResult<Vec<f64>> {
        self.check(x)?;
        let rho = self.field.forward(&self.symmetrise(x));
        Ok(match &self.bound {
            None => rho,
            Some(b) => rho.iter().zip(b).map(|(r, c)| r * c).collect(),
        })
    }


    pub fn pullback(&self, x: &[f64], g: &[f64]) -> CaeResult<Vec<f64>> {
        self.check(x)?;
        let bounded: Vec<f64>;
        let g = match &self.bound {
            None => g,
            Some(b) => {
                bounded = g.iter().zip(b).map(|(gi, c)| gi * c).collect();
                &bounded
            }
        };
        let gs = self.field.pullback(&self.symmetrise(x), g);

        Ok(self.symmetrise(&gs))
    }

    #[must_use]
    pub fn region(&self) -> &[bool] {
        &self.field.region
    }
}

pub struct FsiModel {
    pub problem: FsiProblem,
    pub kuhn: KuhnMesh,
    pub soft: SoftModel,
    pub points: Arc<PushforwardPoints>,
    pub carrier: Arc<SolidCarrier>,
    pub chain: DesignChain,
    pub modifier: Option<Arc<RemovalZoneModifier>>,
}

impl std::fmt::Debug for FsiModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsiModel")
            .field("identity", &self.problem.identity())
            .field("voxels", &self.chain.len())
            .field("points", &self.points.len())
            .finish_non_exhaustive()
    }
}

fn node_selection(points: &[[f64; 3]], b: &[[f64; 3]; 2], scale: f64) -> Vec<usize> {
    (0..points.len()).filter(|&n| in_box(b, points[n], scale)).collect()
}

fn element_materials(kuhn: &KuhnMesh, sd: &SolidSpec) -> (Vec<usize>, Vec<Vec<[f64; 3]>>) {
    let map = &sd.material_map;
    let element_material: Vec<usize> = kuhn.owner.iter().map(|&v| map.voxel_material[v]).collect();
    let fibres = element_material
        .iter()
        .map(|&m| if map.materials[m].fibres.is_some() { sd.fibre_directions.clone() } else { Vec::new() })
        .collect();
    (element_material, fibres)
}

fn with_removal_modifier(
    soft: SoftModel,
    kuhn: &KuhnMesh,
    problem: &FsiProblem,
) -> CaeResult<(SoftModel, Option<Arc<RemovalZoneModifier>>)> {
    let sd = &problem.solid;
    match (&sd.removal_modifier, &problem.design.removal) {
        (Some(spec), Some(removal)) => {
            let m = Arc::new(RemovalZoneModifier::new(
                kuhn,
                &sd.grid,
                removal,
                spec,
                problem.design.interpolation,
            )?);
            Ok((soft.with_potential(Arc::clone(&m) as Arc<dyn NodalPotential>), Some(m)))
        }
        _ => Ok((soft, None)),
    }
}

fn support_nodes(grid: &VoxelGrid, points: &[[f64; 3]], selection: &NodeSelection) -> CaeResult<Vec<usize>> {
    match selection {
        NodeSelection::Box(b) => Ok(node_selection(points, b, grid.element_size_m)),
        NodeSelection::Voxels(mask) => grid.voxel_nodes(mask),
    }
}

impl FsiModel {

    pub fn new(problem: FsiProblem) -> CaeResult<Self> {
        let sd = &problem.solid;
        let kuhn = kuhn_tetrahedra(&sd.grid)?;
        let mesh = kuhn.mesh.clone();
        let n = mesh.node_count();
        let ne = mesh.elements.len();
        let h = sd.grid.element_size_m;
        let mut fixed = vec![false; 3 * n];
        for (i, support) in sd.supports.iter().enumerate() {
            let nodes = support_nodes(&sd.grid, &mesh.points, &support.selection)?;
            if nodes.is_empty() {
                return Err(CaeError::contract(format!(
                    "solid.supports[{i}] selects no node of the reference grid"
                )));
            }
            for node in nodes {
                for c in 0..3 {
                    fixed[3 * node + c] |= support.components[c];
                }
            }
        }
        if sd.plane_strain {
            for node in 0..n {
                fixed[3 * node + 2] = true;
            }
        }
        let (element_material, fibres) = element_materials(&kuhn, sd);
        let mut soft = SoftModel::new(
            mesh.clone(),
            sd.material_map.materials.clone(),
            element_material,
            sd.formulation,
            fibres,
            vec![[0.0, 0.0, 1.0]; ne],
            problem.design.interpolation,
            fixed,
            Vec::new(),
            sd.lumped_mass,
        )?;
        let points = match problem.coupling.points_per_cell_axis {
            Some(p) => PushforwardPoints::adaptive(&mesh, problem.fluid.spacing_m, p)?,
            None => PushforwardPoints::new(&mesh, problem.coupling.points_per_axis)?,
        };
        let points = Arc::new(points.with_void_threshold(problem.coupling.void_threshold)?);
        for plane in &sd.contact_planes {
            if let Some(region) = &plane.region {
                let elements_in: Vec<bool> = kuhn.owner.iter().map(|&v| region[v]).collect();
                let barrier = RegionPlaneBarrier::new(
                    &mesh,
                    &points,
                    &elements_in,
                    plane.normal,
                    plane.offset_m,
                    plane.activation_m,
                    plane.stiffness_pa,
                    problem.design.blocking,
                )?;
                soft = soft.with_potential(Arc::new(barrier) as Arc<dyn NodalPotential>);
                continue;
            }
            let barrier = RigidPlaneBarrier::new(
                Arc::clone(&points),
                mesh.elements.clone(),
                plane.normal,
                plane.offset_m,
                plane.activation_m,
                plane.stiffness_pa,
                problem.design.blocking,
            )?;
            soft = soft.with_potential(Arc::new(barrier) as Arc<dyn NodalPotential>);
        }
        for (i, (b, k)) in sd.foundations.iter().enumerate() {
            let nodes = node_selection(&mesh.points, b, h);
            if nodes.is_empty() {
                return Err(CaeError::contract(format!("solid.foundations[{i}] selects no node")));
            }
            let f = ElasticFoundation::new(n, nodes.into_iter().map(|node| (node, *k)).collect())?;
            soft = soft.with_potential(Arc::new(f) as Arc<dyn NodalPotential>);
        }
        let (soft, modifier) = with_removal_modifier(soft, &kuhn, &problem)?;
        let map = VoxelDesignMap::new(&kuhn, sd.grid.voxel_count());
        let carrier = SolidCarrier::new(
            Arc::clone(&points),
            map,
            problem.design.blocking,
            problem.coupling.blocking_scale,
        )?
        .with_geometry(&mesh)?;
        let carrier = Arc::new(if problem.design.constant_blocking {
            carrier.with_constant_blocking()
        } else {
            carrier
        });
        let chain = DesignChain::new(&problem)?;
        Ok(Self { problem, kuhn, soft, points, carrier, chain, modifier })
    }

    pub fn solid_sample_name_metadata(&self) -> CaeResult<Vec<String>> {
        Ok(self.solid_observables()?.names().to_vec())
    }

    fn solid_observables(&self) -> CaeResult<Observables> {
        let list: Vec<(String, SolidObservable)> = self.problem.observables.solid.clone();
        let needs_points = list.iter().any(|(_, o)| matches!(o, SolidObservable::PlaneGap { .. }));
        Observables::new(
            &self.soft,
            list,
            needs_points.then(|| Arc::clone(&self.points)),
            self.problem.design.blocking,
        )
    }

    fn motion_patterns(&self) -> CaeResult<Vec<(Vec<f64>, Vec<f64>)>> {
        let t = &self.problem.time;
        let sd = &self.problem.solid;
        let (steps, dt, n) = (t.steps_per_period, t.macro_step_s(), self.soft.n());
        let mut groups: Vec<((f64, f64), Vec<f64>)> = Vec::new();
        for support in &sd.supports {
            let Some(motion) = &support.motion else { continue };
            let signal = motion.signal();
            let slot = if let Some(i) = groups.iter().position(|(s, _)| *s == signal) {
                i
            } else {
                groups.push((signal, vec![0.0; 3 * n]));
                groups.len() - 1
            };
            for node in support_nodes(&sd.grid, &self.soft.mesh.points, &support.selection)? {
                let p = motion.pattern(self.soft.mesh.points[node]);
                for (c, (value, held)) in p.iter().zip(support.components).enumerate() {
                    if held {
                        groups[slot].1[3 * node + c] += value;
                    }
                }
            }
        }
        Ok(groups
            .into_iter()
            .map(|((f, phase), pattern)| {
                let a = (1..=steps)
                    .map(|k| (2.0 * std::f64::consts::PI * f * dt * k as f64 + phase).sin())
                    .collect();
                (pattern, a)
            })
            .collect())
    }

    fn loading(&self) -> CaeResult<Loading> {
        let t = &self.problem.time;
        let steps = t.steps_per_period;
        let dt = t.macro_step_s();
        let n = self.soft.n();
        let (prescribed, displacement_amplitude) = self
            .motion_patterns()?
            .into_iter()
            .next()
            .unwrap_or_else(|| (vec![0.0; 3 * n], vec![0.0; steps]));
        Ok(Loading {
            times: (0..=steps).map(|k| dt * k as f64).collect(),
            prescribed,
            displacement_amplitude,
            force: vec![0.0; 3 * n],
            force_amplitude: vec![0.0; steps],
            pressure: 0.0,
            pressure_amplitude: vec![0.0; steps],
            initial_velocity: vec![0.0; 3 * n],
        })
    }

    #[must_use]
    pub fn lbm_config(&self) -> MovingLbmConfig {
        let f = &self.problem.fluid;
        let t = &self.problem.time;
        let c = &self.problem.coupling;
        let mut cfg = MovingLbmConfig::new(
            f.shape,
            f.spacing_m,
            f.periodic,
            f.density_kg_m3,
            f.kinematic_viscosity_m2_s,
            t.macro_step_s(),
            c.substeps,
        );
        cfg.origin_m = f.origin_m;
        cfg.collision = f.collision.clone();
        cfg.turbulence = f.turbulence;
        cfg.coupling_law = f.coupling_law;
        cfg.saturation_width = c.saturation_width;
        cfg.kernel_width_cells = c.kernel_width_cells;
        cfg.solid_mask.clone_from(&f.solid_mask);
        cfg.ports.clone_from(&f.ports);
        cfg.sponges.clone_from(&f.sponges);
        cfg.body_acceleration_m_s2 = f.body_acceleration_m_s2;
        cfg.mach_limit = f.mach_limit;
        cfg.lattice_velocity_limit = f.lattice_velocity_limit;
        cfg.tau_min = f.tau_min;
        cfg.observables.clone_from(&self.problem.observables.fluid);
        cfg.sampling = f.sampling;
        cfg.initial_velocity_m_s = f.initial_velocity_m_s;
        cfg.initial_pressure_pa = f.initial_pressure_pa;
        cfg.inner_checkpoint_bytes = f.inner_checkpoint_bytes;
        cfg.symmetry_faces.clone_from(&f.symmetry_faces);
        cfg.entrained_inertia = f.entrained_inertia;
        cfg
    }


    pub fn fluid_field(&self) -> CaeResult<FluidField> {
        let inner = AnyMovingLbm::new(
            self.problem.fluid.lattice,
            self.lbm_config(),
            Arc::clone(&self.carrier) as Arc<dyn LagrangianCarrier>,
        )?;
        Ok(FluidField::new(
            inner,
            self.problem.observables.interface.clone(),
            self.problem.time.macro_step_s(),
        )
        .with_void_ledger(Arc::clone(&self.carrier)))
    }


    pub fn solid_field(&self) -> CaeResult<SoftSolidField<'_>> {
        let sd = &self.problem.solid;
        let config = SoftStepConfig {
            scheme: sd.scheme,
            loading: self.loading()?,
            periodic_loading: true,
            rayleigh: sd.rayleigh,
            newton: sd.newton,
        };
        let layout = DesignLayout::VoxelDensity(VoxelDesignMap::new(&self.kuhn, sd.grid.voxel_count()));
        let mut core = SoftStepCore::new(&self.soft, config, layout, self.solid_observables()?)?
            .with_factorization_reuse(sd.factorization_reuse)?;
        let extra: Vec<(Vec<f64>, Vec<f64>)> = self.motion_patterns()?.into_iter().skip(1).collect();
        if !extra.is_empty() {
            core = core.with_prescribed_patterns(extra)?;
        }
        Ok(SoftSolidField::new(core))
    }


    pub fn stepper(&self) -> CaeResult<FsiStepper<'_>> {
        let c = &self.problem.coupling;
        let options = MultirateOptions {
            mode: c.mode.clone(),
            schur_ratio_limit: c.schur_ratio_limit,
            work_defect_limit: c.work_defect_limit,
        };
        Ok(MultirateStepper::new(self.fluid_field()?, self.solid_field()?, options)?
            .with_identity(format!("lattice_boltzmann_fsi_dynamic:{}", self.problem.identity()))
            .with_field_newton(c.field_newton)?
            .with_linear_solves(c.linear_solves)?
            .with_step_cache_bytes(c.step_cache_bytes))
    }
}
