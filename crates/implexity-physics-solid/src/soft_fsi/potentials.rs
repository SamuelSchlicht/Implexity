// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use rayon::prelude::*;

use implexity_ad::{HyperDual, Scalar};
use implexity_core::CaeError;

use super::pushforward::{BlockingMap, PushforwardPoints};
pub use crate::soft::model::NodalPotential;
use crate::util::contract;

fn barrier<S: Scalar>(g: S, activation: f64) -> S {
    let r = (g - activation) / activation;
    -(r * r) * (g / activation).ln()
}

fn barrier_derivatives(g: f64, activation: f64) -> Result<(f64, f64, f64), CaeError> {
    if !(g.is_finite() && g > 0.0) {
        return crate::util::convergence(format!(
            "rigid-plane barrier penetrated (gap {g:.3e} m): the state is inadmissible"
        ));
    }
    if g >= activation {
        return Ok((0.0, 0.0, 0.0));
    }
    let v = barrier(HyperDual::new(g, 1.0, 1.0, 0.0), activation);
    Ok((v.re, v.e1, v.e12))
}

fn barrier_third(g: f64, activation: f64) -> f64 {
    barrier(implexity_ad::Jet3::<1>::variable(g, 0, 1.0, 1.0), activation).third()[0]
}


#[derive(Debug, Clone)]
pub struct RigidPlaneBarrier {
    pub normal: [f64; 3],
    pub offset_m: f64,
    pub activation_m: f64,
    pub stiffness_pa: f64,
    pub blocking: BlockingMap,
    points: Arc<PushforwardPoints>,
    elements: Vec<[usize; 4]>,
}

impl RigidPlaneBarrier {

    pub fn new(
        points: Arc<PushforwardPoints>,
        elements: Vec<[usize; 4]>,
        normal: [f64; 3],
        offset_m: f64,
        activation_m: f64,
        stiffness_pa: f64,
        blocking: BlockingMap,
    ) -> Result<Self, CaeError> {
        let norm = normal.iter().map(|v| v * v).sum::<f64>().sqrt();
        if !(normal.iter().copied().all(f64::is_finite) && (norm - 1.0).abs() < 1e-12) {
            return contract("contact plane normals must be finite unit vectors");
        }
        if !(offset_m.is_finite() && activation_m.is_finite() && activation_m > 0.0) {
            return contract("contact planes need a finite offset and a positive activation distance");
        }
        if !(stiffness_pa.is_finite() && stiffness_pa > 0.0) {
            return contract("contact plane stiffness must be positive");
        }
        if elements.len() != points.element_count() {
            return contract("contact plane elements must be the push-forward mesh's elements");
        }
        let barrier = Self { normal, offset_m, activation_m, stiffness_pa, blocking, points, elements };
        if barrier.gaps(&vec![0.0; barrier.points.trace_size()])?.iter().any(|g| *g <= 0.0) {
            return contract("the reference configuration penetrates a contact plane");
        }
        Ok(barrier)
    }

    fn gaps(&self, u: &[f64]) -> Result<Vec<f64>, CaeError> {
        let x = self.points.positions(u)?;
        let n = self.normal;
        Ok(x.iter().map(|p| n[0] * p[0] + n[1] * p[1] + n[2] * p[2] - self.offset_m).collect())
    }


    pub fn minimum_gap(&self, u: &[f64]) -> Result<f64, CaeError> {
        Ok(self.gaps(u)?.into_iter().fold(f64::INFINITY, f64::min))
    }

    fn weight(&self, q: usize, params: &[f64]) -> (f64, f64) {
        let e = self.points.element[q];
        let (b, db) = self.blocking.with_derivative(params[e]);
        let w = self.points.reference_weight[q] * self.stiffness_pa;
        (b * w, db * w)
    }

    fn check(&self, u: &[f64], params: &[f64]) -> Result<(), CaeError> {
        if u.len() != self.points.trace_size() || params.len() != 2 * self.elements.len() {
            return contract("contact plane: displacement or parameter length mismatch");
        }
        Ok(())
    }

    fn active(&self, u: &[f64]) -> Result<Vec<(usize, [f64; 3])>, CaeError> {
        let gaps = self.gaps(u)?;
        let evaluated: Vec<Result<Option<(usize, [f64; 3])>, CaeError>> = gaps
            .par_iter()
            .enumerate()
            .map(|(q, g)| {
                let (b, db, ddb) = barrier_derivatives(*g, self.activation_m)?;
                Ok((*g < self.activation_m).then_some((q, [b, db, ddb])))
            })
            .collect();
        let mut out = Vec::new();
        for r in evaluated {
            if let Some(v) = r? {
                out.push(v);
            }
        }
        Ok(out)
    }
}

impl NodalPotential for RigidPlaneBarrier {
    fn energy(&self, u: &[f64], params: &[f64]) -> Result<f64, CaeError> {
        self.check(u, params)?;
        Ok(self.active(u)?.iter().map(|(q, b)| self.weight(*q, params).0 * b[0]).sum())
    }

    fn force(&self, u: &[f64], params: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(u, params)?;
        let mut f = vec![0.0; u.len()];
        for (q, b) in self.active(u)? {
            let c = self.weight(q, params).0 * b[1];
            let t = &self.elements[self.points.element[q]];
            let l = &self.points.barycentric[q];
            for a in 0..4 {
                for i in 0..3 {
                    f[3 * t[a] + i] += c * l[a] * self.normal[i];
                }
            }
        }
        Ok(f)
    }

    fn tangent(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u, params)?;
        let mut out = Vec::new();
        let n = self.normal;
        for (q, b) in self.active(u)? {
            let c = self.weight(q, params).0 * b[2];
            let t = &self.elements[self.points.element[q]];
            let l = &self.points.barycentric[q];
            for a in 0..4 {
                for bb in 0..4 {
                    for i in 0..3 {
                        for j in 0..3 {
                            let v = c * l[a] * l[bb] * n[i] * n[j];
                            if v != 0.0 {
                                out.push((3 * t[a] + i, 3 * t[bb] + j, v));
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
            return contract("contact plane: cotangent length mismatch");
        }
        let mut g = vec![0.0; params.len()];
        for (q, b) in self.active(u)? {
            let e = self.points.element[q];
            let t = &self.elements[e];
            let l = &self.points.barycentric[q];
            let mut nw = 0.0;
            for a in 0..4 {
                for i in 0..3 {
                    nw += l[a] * self.normal[i] * w[3 * t[a] + i];
                }
            }
            g[e] += self.weight(q, params).1 * b[1] * nw;
        }
        Ok(g)
    }

    fn force_params_jacobian(&self, u: &[f64], params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u, params)?;
        let mut out = Vec::new();
        for (q, b) in self.active(u)? {
            let e = self.points.element[q];
            let c = self.weight(q, params).1 * b[1];
            if c == 0.0 {
                continue;
            }
            let t = &self.elements[e];
            let l = &self.points.barycentric[q];
            for a in 0..4 {
                for i in 0..3 {
                    out.push((3 * t[a] + i, e, c * l[a] * self.normal[i]));
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
            return contract("contact plane: direction or cotangent length mismatch");
        }
        let mut gu = vec![0.0; u.len()];
        let mut gp = vec![0.0; params.len()];
        let n = self.normal;
        let gaps = self.gaps(u)?;
        for (q, b) in self.active(u)? {
            let e = self.points.element[q];
            let t = &self.elements[e];
            let l = &self.points.barycentric[q];
            let project = |v: &[f64]| -> f64 {
                let mut acc = 0.0;
                for a in 0..4 {
                    for i in 0..3 {
                        acc += l[a] * n[i] * v[3 * t[a] + i];
                    }
                }
                acc
            };
            let (nw, dg) = (project(w), project(du));
            let scale = self.points.reference_weight[q] * self.stiffness_pa;
            let bd = self.blocking.value(HyperDual::new(params[e], 1.0, 1.0, 0.0));
            let (a, da, dda) = (bd.re * scale, bd.e1 * scale, bd.e12 * scale);
            let b3 = barrier_third(gaps[q], self.activation_m);
            let drho = dparams[e];
            let cu = (a * b3 * dg + da * drho * b[2]) * nw;
            for aa in 0..4 {
                for i in 0..3 {
                    gu[3 * t[aa] + i] += cu * l[aa] * n[i];
                }
            }
            gp[e] += (da * b[2] * dg + dda * drho * b[1]) * nw;
        }
        Ok((gu, gp))
    }
}

#[derive(Debug, Clone)]
pub struct ElasticFoundation {
    pub springs: Vec<(usize, f64)>,
    node_count: usize,
}

impl ElasticFoundation {

    pub fn new(node_count: usize, springs: Vec<(usize, f64)>) -> Result<Self, CaeError> {
        let mut seen = vec![false; node_count];
        for (n, k) in &springs {
            if *n >= node_count || seen[*n] {
                return contract("elastic foundation nodes must be distinct model nodes");
            }
            seen[*n] = true;
            if !(k.is_finite() && *k >= 0.0) {
                return contract("elastic foundation stiffness must be finite and nonnegative");
            }
        }
        Ok(Self { springs, node_count })
    }


    pub fn mass_proportional(
        nodal_mass: &[f64],
        nodes: &[usize],
        omega_rad_s: f64,
    ) -> Result<Self, CaeError> {
        if !(omega_rad_s.is_finite() && omega_rad_s >= 0.0) {
            return contract("elastic foundation frequency must be finite and nonnegative");
        }
        let springs = nodes
            .iter()
            .map(|n| nodal_mass.get(*n).map(|m| (*n, omega_rad_s * omega_rad_s * m)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| CaeError::contract("elastic foundation nodes must be model nodes"))?;
        Self::new(nodal_mass.len(), springs)
    }

    fn check(&self, u: &[f64]) -> Result<(), CaeError> {
        if u.len() == 3 * self.node_count {
            Ok(())
        } else {
            contract("elastic foundation: displacement length mismatch")
        }
    }
}

impl NodalPotential for ElasticFoundation {
    fn energy(&self, u: &[f64], _params: &[f64]) -> Result<f64, CaeError> {
        self.check(u)?;
        Ok(self
            .springs
            .iter()
            .map(|(n, k)| 0.5 * k * (0..3).map(|i| u[3 * n + i].powi(2)).sum::<f64>())
            .sum())
    }

    fn force(&self, u: &[f64], _params: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(u)?;
        let mut f = vec![0.0; u.len()];
        for (n, k) in &self.springs {
            for i in 0..3 {
                f[3 * n + i] = k * u[3 * n + i];
            }
        }
        Ok(f)
    }

    fn tangent(&self, u: &[f64], _params: &[f64]) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u)?;
        Ok(self.springs.iter().flat_map(|(n, k)| (0..3).map(move |i| (3 * n + i, 3 * n + i, *k))).collect())
    }

    fn force_params_vjp(&self, u: &[f64], params: &[f64], _w: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(u)?;
        Ok(vec![0.0; params.len()])
    }

    fn force_params_jacobian(
        &self,
        u: &[f64],
        _params: &[f64],
    ) -> Result<Vec<(usize, usize, f64)>, CaeError> {
        self.check(u)?;
        Ok(Vec::new())
    }

    fn force_second_directional(
        &self,
        u: &[f64],
        params: &[f64],
        _w: &[f64],
        _du: &[f64],
        _dparams: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), CaeError> {

        self.check(u)?;
        Ok((vec![0.0; u.len()], vec![0.0; params.len()]))
    }
}

