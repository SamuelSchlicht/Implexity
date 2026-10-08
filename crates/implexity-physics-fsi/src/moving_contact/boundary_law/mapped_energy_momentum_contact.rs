// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::contact_compliance::UnilateralContactCompliance;
use crate::moving_contact::surface_map::{PointMap, SurfaceFeature};
use super::contact_step_force::ContactStepForce;
use super::compliant_energy_momentum_contact;
fn fail(e: impl std::fmt::Display) -> CaeError { CaeError::contract(e.to_string()) }

pub struct MappedEnergyMomentumContact<'a> {
    maps: [Vec<(f64, PointMap)>; 4],
    bodies: [usize; 4],
    previous: &'a [Vec<[f64; 3]>; 2],
    current: &'a [Vec<[f64; 3]>; 2],
    law: ContactStepForce,
}
pub struct MappedContactPullback {
    pub previous_position: [Vec<[f64; 3]>; 2],
    pub current_position: [Vec<[f64; 3]>; 2],
    pub density: [Vec<f64>; 2],
    pub reference_area: f64,
}
impl<'a> MappedEnergyMomentumContact<'a> {
    pub fn evaluate(
        traces: &[Vec<(f64, SurfaceFeature)>; 4], bodies: [usize; 4],
        previous: &'a [Vec<[f64; 3]>; 2], current: &'a [Vec<[f64; 3]>; 2],
        density: &[Vec<f64>; 2], reference_area_m2: f64, compliance: UnilateralContactCompliance,
        minimum_parameter: f64,
    ) -> CaeResult<Self> {
        if bodies.iter().any(|b| *b > 1) || bodies[0] == bodies[1] || bodies[1] != bodies[2] || bodies[2] != bodies[3]
            || (0..2).any(|b| previous[b].len() != current[b].len() || current[b].len() != density[b].len()) {
            return Err(fail("mapped contact body and coordinate ownership"));
        }
        if previous.iter().chain(current).flat_map(|v| v.iter().flatten()).chain(density.iter().flatten()).any(|v| !v.is_finite()) {
            return Err(fail("mapped contact finite coordinates and density"));
        }
        let mut maps = vec![];
        let mut old = [[0.; 3]; 4];
        let mut new = [[0.; 3]; 4];
        for i in 0..4 {
            let trace = &traces[i];
            if trace.is_empty() || trace.iter().any(|(w, _)| !w.is_finite() || *w < 0.)
                || (trace.iter().map(|(w, _)| *w).sum::<f64>() - 1.).abs() > 32. * f64::EPSILON {
                return Err(fail("contact point trace partition"));
            }
            let mut terms = vec![];
            for (weight, feature) in trace {
                let map = feature.map(&density[bodies[i]])?;
                let a = map.position(&previous[bodies[i]])?;
                let z = map.position(&current[bodies[i]])?;
                for axis in 0..3 { old[i][axis] += weight * a[axis]; new[i][axis] += weight * z[axis]; }
                terms.push((*weight, map));
            }
            maps.push(terms);
        }
        let maps = maps.try_into().map_err(|_| fail("contact point count"))?;
        let law = compliant_energy_momentum_contact::vertex_face(old, new, reference_area_m2, compliance, minimum_parameter).map_err(fail)?;
        Ok(Self { maps, bodies, previous, current, law })
    }
    pub fn law(&self) -> &ContactStepForce { &self.law }
    pub fn force(&self) -> CaeResult<[Vec<[f64; 3]>; 2]> {
        let mut force: [Vec<[f64; 3]>; 2] = std::array::from_fn(|b| vec![[0.; 3]; self.current[b].len()]);
        for i in 0..4 {
            let point = std::array::from_fn(|a| self.law.force_n[3 * i + a]);
            for (weight, map) in &self.maps[i] { map.scatter(point.map(|x| weight * x), &mut force[self.bodies[i]])?; }
        }
        Ok(force)
    }
    pub fn direction(
        &self, previous_direction: &[Vec<[f64; 3]>; 2], current_direction: &[Vec<[f64; 3]>; 2],
        density_direction: &[Vec<f64>; 2], reference_area_direction: f64,
    ) -> CaeResult<[Vec<[f64; 3]>; 2]> {
        if !reference_area_direction.is_finite() || (0..2).any(|b| previous_direction[b].len() != self.previous[b].len()
            || current_direction[b].len() != self.current[b].len() || density_direction[b].len() != self.current[b].len()) {
            return Err(fail("mapped contact direction shape and area"));
        }
        let mut old = [0.; 12]; let mut new = [0.; 12];
        for i in 0..4 {
            let b = self.bodies[i];
            for (weight, map) in &self.maps[i] {
                let a = map.direction(&self.previous[b], &previous_direction[b], &density_direction[b])?;
                let z = map.direction(&self.current[b], &current_direction[b], &density_direction[b])?;
                for axis in 0..3 { old[3 * i + axis] += weight * a[axis]; new[3 * i + axis] += weight * z[axis]; }
            }
        }
        let direction: [f64; 12] = std::array::from_fn(|i| self.law.area_derivative_n_per_m2[i] * reference_area_direction
            + (0..12).map(|j| self.law.previous_jacobian_n_per_m[i][j] * old[j] + self.law.current_jacobian_n_per_m[i][j] * new[j]).sum::<f64>());
        let mut result: [Vec<[f64; 3]>; 2] = std::array::from_fn(|b| vec![[0.; 3]; self.current[b].len()]);
        for i in 0..4 {
            let f: [f64; 3] = std::array::from_fn(|a| self.law.force_n[3 * i + a]);
            let d: [f64; 3] = std::array::from_fn(|a| direction[3 * i + a]);
            for (weight, map) in &self.maps[i] {
                map.scatter_direction(f.map(|x| weight * x), d.map(|x| weight * x), &density_direction[self.bodies[i]], &mut result[self.bodies[i]])?;
            }
        }
        Ok(result)
    }
    pub fn pullback(&self, force_bar: &[Vec<[f64; 3]>; 2]) -> CaeResult<MappedContactPullback> {
        if (0..2).any(|b| force_bar[b].len() != self.current[b].len()) || force_bar.iter().flat_map(|v| v.iter().flatten()).any(|v| !v.is_finite()) {
            return Err(fail("mapped contact force cotangent"));
        }
        let mut density: [Vec<f64>; 2] = std::array::from_fn(|b| vec![0.; self.current[b].len()]);
        let mut point_bar = [0.; 12];
        for i in 0..4 {
            let b = self.bodies[i];
            let f: [f64; 3] = std::array::from_fn(|a| self.law.force_n[3 * i + a]);
            for (weight, map) in &self.maps[i] {
                let bar = map.position(&force_bar[b])?;
                for axis in 0..3 { point_bar[3 * i + axis] += weight * bar[axis]; }
                map.density_pullback(&force_bar[b], f.map(|x| weight * x), &mut density[b])?;
            }
        }
        let old: [f64; 12] = std::array::from_fn(|j| (0..12).map(|i| self.law.previous_jacobian_n_per_m[i][j] * point_bar[i]).sum());
        let new: [f64; 12] = std::array::from_fn(|j| (0..12).map(|i| self.law.current_jacobian_n_per_m[i][j] * point_bar[i]).sum());
        let mut old_bar: [Vec<[f64; 3]>; 2] = std::array::from_fn(|b| vec![[0.; 3]; self.current[b].len()]);
        let mut new_bar = old_bar.clone();
        for i in 0..4 {
            let b = self.bodies[i];
            let a: [f64; 3] = std::array::from_fn(|axis| old[3 * i + axis]);
            let z: [f64; 3] = std::array::from_fn(|axis| new[3 * i + axis]);
            for (weight, map) in &self.maps[i] {
                map.scatter(a.map(|x| weight * x), &mut old_bar[b])?;
                map.scatter(z.map(|x| weight * x), &mut new_bar[b])?;
                map.density_pullback(&self.previous[b], a.map(|x| weight * x), &mut density[b])?;
                map.density_pullback(&self.current[b], z.map(|x| weight * x), &mut density[b])?;
            }
        }
        let area: f64 = point_bar.iter().zip(&self.law.area_derivative_n_per_m2).map(|(a, b)| a * b).sum();
        if !area.is_finite() { return Err(fail("mapped contact area cotangent overflow")); }
        Ok(MappedContactPullback { previous_position: old_bar, current_position: new_bar, density, reference_area: area })
    }
}
