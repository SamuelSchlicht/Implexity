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

use crate::moving_contact::{
    discrete_geometry,
    surface_map::{PointMap, SurfaceFeature},
};
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
        old: &'a [Vec<[f64; 3]>; 2],
        new: &'a [Vec<[f64; 3]>; 2],
        density: &[Vec<f64>; 2],
    ) -> CaeResult<Self> {
        if bodies.iter().any(|b| *b > 1)
            || bodies[0] == bodies[1]
            || bodies[1] != bodies[2]
            || bodies[2] != bodies[3]
        {
            return Err(fail("point and triangle need distinct bodies"));
        }
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
            a[k] = maps[k].position(&old[bodies[k]])?;
            z[k] = maps[k].position(&new[bodies[k]])?;
        }
        let geometry =
            discrete_geometry::evaluate(a, z).map_err(|e| CaeError::contract(e.to_string()))?;
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
