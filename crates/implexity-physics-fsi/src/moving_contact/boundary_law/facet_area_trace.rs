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

use implexity_core::{CaeError, CaeResult};
use crate::moving_contact::surface_map::{PointMap, SurfaceFeature};
fn fail(e: impl std::fmt::Display) -> CaeError { CaeError::contract(e.to_string()) }
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] { [a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0]] }

pub struct ReferenceFacetArea<'a> {
    maps: [Vec<(f64, PointMap)>; 3],
    reference: &'a [[f64; 3]],
    gradient: [[f64; 3]; 3],
    area_m2: f64,
}
impl<'a> ReferenceFacetArea<'a> {
    pub fn new(traces: &[Vec<(f64, SurfaceFeature)>; 3], reference: &'a [[f64; 3]], density: &[f64]) -> CaeResult<Self> {
        if reference.len() != density.len() || reference.iter().flatten().chain(density).any(|x| !x.is_finite()) {
            return Err(fail("reference facet coordinates and density"));
        }
        let mut maps = vec![]; let mut points = [[0.; 3]; 3];
        for i in 0..3 {
            if traces[i].is_empty() || traces[i].iter().any(|(w, _)| !w.is_finite() || *w < 0.)
                || (traces[i].iter().map(|(w, _)| *w).sum::<f64>() - 1.).abs() > 32. * f64::EPSILON {
                return Err(fail("reference facet point partition"));
            }
            let mut terms = vec![];
            for (weight, feature) in &traces[i] {
                let map = feature.map(density)?; let p = map.position(reference)?;
                for a in 0..3 { points[i][a] += weight * p[a]; }
                terms.push((*weight, map));
            }
            maps.push(terms);
        }
        let u = std::array::from_fn(|a| points[1][a] - points[0][a]);
        let v = std::array::from_fn(|a| points[2][a] - points[0][a]);
        let n = cross(u, v); let norm = n[0].hypot(n[1]).hypot(n[2]);
        if !norm.is_finite() || norm <= 0. { return Err(fail("reference facet area degeneracy")); }
        let normal = n.map(|x| x / norm);
        let b = cross(v, normal).map(|x| 0.5 * x);
        let c = cross(normal, u).map(|x| 0.5 * x);
        let a = std::array::from_fn(|i| -b[i] - c[i]);
        Ok(Self { maps: maps.try_into().map_err(|_| fail("reference facet count"))?, reference, gradient: [a, b, c], area_m2: 0.5 * norm })
    }
    pub fn area_m2(&self) -> f64 { self.area_m2 }
    pub fn direction(&self, reference_direction: &[[f64; 3]], density_direction: &[f64]) -> CaeResult<f64> {
        let mut result = 0.;
        for i in 0..3 {
            for (weight, map) in &self.maps[i] {
                let d = map.direction(self.reference, reference_direction, density_direction)?;
                result += weight * (0..3).map(|a| self.gradient[i][a] * d[a]).sum::<f64>();
            }
        }
        if !result.is_finite() { return Err(fail("reference facet area direction overflow")); }
        Ok(result)
    }
    pub fn pullback(&self, area_bar: f64) -> CaeResult<(Vec<[f64; 3]>, Vec<f64>)> {
        if !area_bar.is_finite() { return Err(fail("reference facet area cotangent")); }
        let mut points = vec![[0.; 3]; self.reference.len()]; let mut density = vec![0.; self.reference.len()];
        for i in 0..3 {
            for (weight, map) in &self.maps[i] {
                let bar = self.gradient[i].map(|x| weight * area_bar * x);
                map.scatter(bar, &mut points)?;
                map.density_pullback(self.reference, bar, &mut density)?;
            }
        }
        Ok((points, density))
    }
}
