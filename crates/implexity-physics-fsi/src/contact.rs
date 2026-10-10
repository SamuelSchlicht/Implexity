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


use std::sync::Arc;

use implexity_ad::{HyperDual, Jet3, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::hyperelastic::kinematics::TetMesh;
use implexity_physics_solid::soft::model::NodalPotential;
use implexity_physics_solid::soft_fsi::pushforward::{BlockingMap, PushforwardPoints};

fn barrier<S: Scalar>(g: S, activation: f64) -> S {
    let r = (g - activation) / activation;
    -(r * r) * (g / activation).ln()
}

fn derivatives(g: f64, activation: f64) -> CaeResult<[f64; 3]> {
    if !(g.is_finite() && g > 0.0) {
        return Err(CaeError::convergence(format!(
            "region contact plane penetrated (gap {g:.3e} m): the state is inadmissible"
        )));
    }
    if g >= activation {
        return Ok([0.0; 3]);
    }
    let v = barrier(HyperDual::new(g, 1.0, 1.0, 0.0), activation);
    Ok([v.re, v.e1, v.e12])
}

fn third(g: f64, activation: f64) -> f64 {
    if g >= activation {
        return 0.0;
    }
    barrier(Jet3::<1>::variable(g, 0, 1.0, 1.0), activation).third()[0]
}

#[derive(Clone, Debug)]
struct Point {
    element: usize,
    nodes: [usize; 4],
    barycentric: [f64; 4],
    reference: [f64; 3],
    weight: f64,
}

#[derive(Clone, Debug)]
pub struct RegionPlaneBarrier {
    normal: [f64; 3],
    offset_m: f64,
    activation_m: f64,
    stiffness_pa: f64,
    blocking: BlockingMap,
    points: Vec<Point>,
    elements: usize,
}

impl RegionPlaneBarrier {

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mesh: &TetMesh,
        pushforward: &Arc<PushforwardPoints>,
        elements_in: &[bool],
        normal: [f64; 3],
        offset_m: f64,
        activation_m: f64,
        stiffness_pa: f64,
        blocking: BlockingMap,
    ) -> CaeResult<Self> {
        let norm = normal.iter().map(|v| v * v).sum::<f64>().sqrt();
        if !(normal.iter().copied().all(f64::is_finite) && (norm - 1.0).abs() < 1e-12) {
            return Err(CaeError::contract("contact plane normals must be finite unit vectors"));
        }
        if !(offset_m.is_finite() && activation_m.is_finite() && activation_m > 0.0) {
            return Err(CaeError::contract(
                "contact planes need a finite offset and a positive activation distance",
            ));
        }
        if !(stiffness_pa.is_finite() && stiffness_pa > 0.0) {
            return Err(CaeError::contract("contact plane stiffness must be positive"));
        }
        if elements_in.len() != mesh.elements.len() || pushforward.element_count() != mesh.elements.len() {
            return Err(CaeError::contract("region contact plane: element flags must cover the mesh"));
        }
        let zero = vec![0.0; pushforward.trace_size()];
        let positions = pushforward.positions(&zero)?;
        let weights =
            pushforward.weights(&zero, &vec![1.0; mesh.elements.len()], &BlockingMap::identity())?;
        let mut points = Vec::new();
        for (e, inside) in elements_in.iter().enumerate() {
            if !inside {
                continue;
            }
            let nodes = mesh.elements[e];
            let g = &mesh.gradients[e];
            let x0 = mesh.points[nodes[0]];
            for q in pushforward.element_points(e) {
                let x = positions[q];
                let barycentric: [f64; 4] = std::array::from_fn(|a| {
                    let delta = if a == 0 { 1.0 } else { 0.0 };
                    delta + (0..3).map(|i| g[a][i] * (x[i] - x0[i])).sum::<f64>()
                });
                points.push(Point { element: e, nodes, barycentric, reference: x, weight: weights[q] });
            }
        }
        if points.is_empty() {
            return Err(CaeError::contract("region contact plane: the region holds no solid point"));
        }
        let barrier = Self {
            normal,
            offset_m,
            activation_m,
            stiffness_pa,
            blocking,
            points,
            elements: mesh.elements.len(),
        };
        if barrier.gaps(&zero).iter().any(|g| *g <= 0.0) {
            return Err(CaeError::contract(
                "the reference configuration of the contact region penetrates its plane",
            ));
        }
        Ok(barrier)
    }

    #[must_use]
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    fn gaps(&self, u: &[f64]) -> Vec<f64> {
        self.points.iter().map(|p| self.gap(p, u)).collect()
    }

    fn gap(&self, p: &Point, u: &[f64]) -> f64 {
        (0..3)
            .map(|i| {
                let x = p.reference[i]
                    + (0..4)
                        .map(|a| p.barycentric[a] * u.get(3 * p.nodes[a] + i).copied().unwrap_or(f64::NAN))
                        .sum::<f64>();
                self.normal[i] * x
            })
            .sum::<f64>()
            - self.offset_m
    }


    pub fn minimum_gap(&self, u: &[f64]) -> CaeResult<f64> {
        Ok(self.gaps(u).into_iter().fold(f64::INFINITY, f64::min))
    }

    fn check(&self, u: &[f64], params: &[f64]) -> CaeResult<()> {
        if params.len() != 2 * self.elements || !u.len().is_multiple_of(3) {
            return Err(CaeError::contract(
                "region contact plane: displacement or parameter length mismatch",
            ));
        }
        Ok(())
    }

    fn weight(&self, p: &Point, params: &[f64]) -> [f64; 3] {
        let b = self.blocking.value(HyperDual::new(params[p.element], 1.0, 1.0, 0.0));
        let s = p.weight * self.stiffness_pa;
        [b.re * s, b.e1 * s, b.e12 * s]
    }

    fn project(&self, p: &Point, v: &[f64]) -> f64 {
        (0..4)
            .map(|a| p.barycentric[a] * (0..3).map(|i| self.normal[i] * v[3 * p.nodes[a] + i]).sum::<f64>())
            .sum()
    }

    fn scatter(&self, p: &Point, c: f64, out: &mut [f64]) {
        for a in 0..4 {
            for i in 0..3 {
                out[3 * p.nodes[a] + i] += c * p.barycentric[a] * self.normal[i];
            }
        }
    }

    fn active<'a>(&'a self, u: &[f64]) -> CaeResult<Vec<(&'a Point, f64, [f64; 3])>> {
        let mut out = Vec::new();
        for p in &self.points {
            let g = self.gap(p, u);
            if g < self.activation_m {
                out.push((p, g, derivatives(g, self.activation_m)?));
            } else if !g.is_finite() {
                return Err(CaeError::contract("region contact plane: displacement length mismatch"));
            }
        }
        Ok(out)
    }
}

impl NodalPotential for RegionPlaneBarrier {
    fn energy(&self, u: &[f64], params: &[f64]) -> Result<f64, CaeError> {
        self.check(u, params)?;
        Ok(self.active(u)?.iter().map(|(p, _, b)| self.weight(p, params)[0] * b[0]).sum())
    }

    fn force(&self, u: &[f64], params: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(u, params)?;
        let mut f = vec![0.0; u.len()];
        for (p, _, b) in self.active(u)? {
            self.scatter(p, self.weight(p, params)[0] * b[1], &mut f);
        }
        Ok(f)
    }

    fn tangent(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u, params)?;
        let n = self.normal;
        let mut out = Vec::new();
        for (p, _, b) in self.active(u)? {
            let c = self.weight(p, params)[0] * b[2];
            for a in 0..4 {
                for bb in 0..4 {
                    for i in 0..3 {
                        for j in 0..3 {
                            let v = c * p.barycentric[a] * p.barycentric[bb] * n[i] * n[j];
                            if v != 0.0 {
                                out.push((3 * p.nodes[a] + i, 3 * p.nodes[bb] + j, v));
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    fn force_params_vjp(&self, u: &[f64], params: &[f64], w: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(u, params)?;
        if w.len() != u.len() {
            return Err(CaeError::contract("region contact plane: cotangent length mismatch"));
        }
        let mut g = vec![0.0; params.len()];
        for (p, _, b) in self.active(u)? {
            g[p.element] += self.weight(p, params)[1] * b[1] * self.project(p, w);
        }
        Ok(g)
    }

    fn force_params_jacobian(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u, params)?;
        let mut out = Vec::new();
        for (p, _, b) in self.active(u)? {
            let c = self.weight(p, params)[1] * b[1];
            if c == 0.0 {
                continue;
            }
            for a in 0..4 {
                for i in 0..3 {
                    out.push((3 * p.nodes[a] + i, p.element, c * p.barycentric[a] * self.normal[i]));
                }
            }
        }
        Ok(out)
    }

    fn force_second_directional(
        &self,
        u: &[f64],
        params: &[f64],
        w: &[f64],
        du: &[f64],
        dparams: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), CaeError> {
        self.check(u, params)?;
        if w.len() != u.len() || du.len() != u.len() || dparams.len() != params.len() {
            return Err(CaeError::contract("region contact plane: direction or cotangent length mismatch"));
        }
        let mut gu = vec![0.0; u.len()];
        let mut gp = vec![0.0; params.len()];
        for (p, g, b) in self.active(u)? {
            let (nw, dg) = (self.project(p, w), self.project(p, du));
            let [a, da, dda] = self.weight(p, params);
            let drho = dparams[p.element];
            let b3 = third(g, self.activation_m);
            self.scatter(p, (a * b3 * dg + da * drho * b[2]) * nw, &mut gu);
            gp[p.element] += (da * b[2] * dg + dda * drho * b[1]) * nw;
        }
        Ok((gu, gp))
    }
}
