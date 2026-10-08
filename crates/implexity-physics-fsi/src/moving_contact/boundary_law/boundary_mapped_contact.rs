// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use super::boundary_discrete_geometry as discrete_geometry;
use crate::moving_contact::surface_map::{PointMap,SurfaceFeature};
use implexity_core::{CaeError, CaeResult};
fn fail(message: &str) -> CaeError {
    CaeError::contract(message)
}
pub struct MappedContact<'a> {
    maps: [PointMap; 4],
    bodies: [usize; 4],
    geometry: discrete_geometry::Geometry,
    old: &'a [Vec<[f64; 3]>; 2],
    new: &'a [Vec<[f64; 3]>; 2],
}
pub struct ForceDirection {
    pub force: [Vec<[f64; 3]>; 2],
    pub direction: [Vec<[f64; 3]>; 2],
    pub gap_direction: f64,
}
impl<'a> MappedContact<'a> {
    pub fn geometry(&self) -> &discrete_geometry::Geometry {
        &self.geometry
    }
    pub fn evaluate(
        features: &[SurfaceFeature; 4],
        bodies: [usize; 4],
        references:[[f64;3];4],
        old: &'a [Vec<[f64; 3]>; 2],
        new: &'a [Vec<[f64; 3]>; 2],
        density: &[Vec<f64>; 2],
    ) -> CaeResult<Self> {
        Self::evaluate_selected(discrete_geometry::SelectedKind::VertexFace,features,bodies,references,old,new,density)
    }
    pub fn evaluate_selected(kind:discrete_geometry::SelectedKind,features:&[SurfaceFeature;4],bodies:[usize;4],references:[[f64;3];4],old:&'a[Vec<[f64;3]>;2],new:&'a[Vec<[f64;3]>;2],density:&[Vec<f64>;2])->CaeResult<Self>{
        let owned=match kind{discrete_geometry::SelectedKind::VertexFace=>bodies[0]!=bodies[1]&&bodies[1]==bodies[2]&&bodies[2]==bodies[3],discrete_geometry::SelectedKind::EdgeEdge{orientation}=>bodies[0]==bodies[1]&&bodies[2]==bodies[3]&&bodies[0]!=bodies[2]&&(orientation==1.||orientation== -1.)};
        if bodies.iter().any(|b|*b>1)||!owned{return Err(fail("selected native boundary feature body/normal ownership"));}
        for b in 0..2 {
            if old[b].len() != new[b].len() || old[b].len() != density[b].len() {
                return Err(fail("surface body shapes differ"));
            }
        }
        let mut maps = vec![];
        for k in 0..4 {
            maps.push(features[k].map(&density[bodies[k]])?);
        }
        let maps: [PointMap; 4] = maps.try_into().map_err(|_| fail("surface feature count"))?;
        let mut a = [[0.; 3]; 4];
        let mut z = a;
        for k in 0..4 {
            a[k] = super::reference_embedding::position(&maps[k],references[k],&old[bodies[k]])?;
            z[k] = super::reference_embedding::position(&maps[k],references[k],&new[bodies[k]])?;
        }
        let geometry =
            discrete_geometry::evaluate(kind,a, z).map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(Self {
            maps,
            bodies,
            geometry,
            old,
            new,
        })
    }
    pub fn force_direction(
        &self,
        lambda: f64,
        d_lambda: f64,
        d_old: &[Vec<[f64; 3]>; 2],
        d_new: &[Vec<[f64; 3]>; 2],
        d_density: &[Vec<f64>; 2],
    ) -> CaeResult<ForceDirection> {
        let old = self.old;
        let new = self.new;
        if ![lambda, d_lambda].iter().all(|v| v.is_finite()) {
            return Err(fail("invalid contact multiplier direction"));
        }
        let mut q0 = [0.; 12];
        let mut q1 = [0.; 12];
        for k in 0..4 {
            let b = self.bodies[k];
            let a = self.maps[k].direction(&old[b], &d_old[b], &d_density[b])?;
            let z = self.maps[k].direction(&new[b], &d_new[b], &d_density[b])?;
            for i in 0..3 {
                q0[3 * k + i] = a[i];
                q1[3 * k + i] = z[i];
            }
        }
        let mut force: [Vec<[f64; 3]>; 2] = std::array::from_fn(|b| vec![[0.; 3]; new[b].len()]);
        let mut direction = force.clone();
        for k in 0..4 {
            let f = std::array::from_fn(|i| lambda * self.geometry.force[3 * k + i]);
            let df = std::array::from_fn(|i| {
                d_lambda * self.geometry.force[3 * k + i]
                    + lambda
                        * (0..12)
                            .map(|j| {
                                self.geometry.current[3 * k + i][j] * q1[j]
                                    + self.geometry.previous[3 * k + i][j] * q0[j]
                            })
                            .sum::<f64>()
            });
            let b = self.bodies[k];
            self.maps[k].scatter(f, &mut force[b])?;
            self.maps[k].scatter_direction(f, df, &d_density[b], &mut direction[b])?;
        }
        let gap_direction = self
            .geometry
            .gap_gradient
            .iter()
            .zip(q1)
            .map(|(g, d)| g * d)
            .sum::<f64>();
        if !gap_direction.is_finite() {
            return Err(fail("mapped gap direction overflow"));
        }
        Ok(ForceDirection {
            force,
            direction,
            gap_direction,
        })
    }
}