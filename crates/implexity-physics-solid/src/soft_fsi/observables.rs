// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::sync::Arc;

use implexity_core::CaeError;

use super::pushforward::{BlockingMap, PushforwardPoints};
use crate::soft::model::SoftModel;
use crate::soft::stepper::{Measure, SoftHistory};
use crate::util::contract;

#[derive(Debug, Clone, PartialEq)]
pub enum SolidObservable {
    ProbeDisplacement {
        point_m: [f64; 3],
        component: usize,
    },
    PlaneGap {
        normal: [f64; 3],
        offset_m: f64,
        beta: f64,
    },
    ProbeSeparation {
        point_a_m: [f64; 3],
        point_b_m: [f64; 3],
        component: usize,
    },
    StrainEnergy,
    KineticEnergy,
    StressAggregate {
        p: f64,
    },
}

#[derive(Debug, Clone)]
pub struct Observables {
    names: Vec<String>,
    list: Vec<SolidObservable>,
    probes: Vec<Vec<(usize, [f64; 4])>>,
    points: Option<Arc<PushforwardPoints>>,
    blocking: BlockingMap,
}

fn barycentric(p: &[[f64; 3]; 4], x: &[f64; 3]) -> [f64; 4] {
    let a: [[f64; 3]; 3] = core::array::from_fn(|r| core::array::from_fn(|c| p[c + 1][r] - p[0][r]));
    let b: [f64; 3] = core::array::from_fn(|r| x[r] - p[0][r]);
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let col = |k: usize| -> f64 {
        let mut m = a;
        for r in 0..3 {
            m[r][k] = b[r];
        }
        (m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]))
            / det
    };
    let (l1, l2, l3) = (col(0), col(1), col(2));
    [1.0 - l1 - l2 - l3, l1, l2, l3]
}

impl Observables {

    pub fn new(
        m: &SoftModel,
        list: Vec<(String, SolidObservable)>,
        points: Option<Arc<PushforwardPoints>>,
        blocking: BlockingMap,
    ) -> Result<Self, CaeError> {
        let mut names: Vec<String> = Vec::with_capacity(list.len());
        let mut probes = Vec::with_capacity(list.len());
        let locate = |name: &str, point: &[f64; 3]| -> Result<(usize, [f64; 4]), CaeError> {
            if !point.iter().copied().all(f64::is_finite) {
                return contract("solid probes need finite points");
            }
            m.mesh
                .elements
                .iter()
                .enumerate()
                .find_map(|(e, t)| {
                    let p = t.map(|n| m.mesh.points[n]);
                    let l = barycentric(&p, point);
                    l.iter().all(|v| *v >= -1e-12).then_some((e, l))
                })
                .ok_or_else(|| {
                    CaeError::contract(format!("solid probe '{name}' lies outside the reference mesh"))
                })
        };
        for (name, obs) in &list {
            if name.is_empty() || names.contains(name) {
                return contract("solid observable names must be unique and nonempty");
            }
            names.push(name.clone());
            probes.push(match obs {
                SolidObservable::ProbeDisplacement { point_m, component } => {
                    if *component > 2 {
                        return contract("solid probes need a finite point and a component in 0..3");
                    }
                    vec![locate(name, point_m)?]
                }
                SolidObservable::ProbeSeparation { point_a_m, point_b_m, component } => {
                    if *component > 2 {
                        return contract("probe separations need a component in 0..3");
                    }
                    vec![locate(name, point_a_m)?, locate(name, point_b_m)?]
                }
                SolidObservable::PlaneGap { normal, offset_m, beta } => {
                    let norm = normal.iter().map(|v| v * v).sum::<f64>().sqrt();
                    if (norm - 1.0).abs() > 1e-12
                        || !offset_m.is_finite()
                        || !(beta.is_finite() && *beta > 0.0)
                    {
                        return contract("plane gaps need a unit normal, a finite offset and beta > 0");
                    }
                    if points.as_ref().is_none_or(|p| p.element_count() != m.ne()) {
                        return contract("plane gaps need the push-forward points of the model");
                    }
                    Vec::new()
                }
                SolidObservable::StressAggregate { p } => {
                    if !(p.is_finite() && *p >= 2.0) {
                        return contract("stress aggregates need an exponent p >= 2");
                    }
                    Vec::new()
                }
                SolidObservable::StrainEnergy | SolidObservable::KineticEnergy => Vec::new(),
            });
        }
        Ok(Self { names, list: list.into_iter().map(|(_, o)| o).collect(), probes, points, blocking })
    }

    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    fn history_sum(h: &SoftHistory<'_>, x: &[f64], e: usize) -> [f64; 6] {
        let l = h.layout;
        let mut out = [0.0; 6];
        if l.ne_visc > 0 {
            for i in 0..l.nb {
                let off = l.q() + 6 * (e * l.nb + i);
                for k in 0..6 {
                    out[k] += x[off + k];
                }
            }
        }
        out
    }

    #[allow(clippy::too_many_lines)]
    fn evaluate(
        &self,
        k: usize,
        h: &SoftHistory<'_>,
        x: &[f64],
        bar: Option<(f64, &mut [f64], &mut [f64])>,
    ) -> Result<f64, CaeError> {
        let m = h.model;
        let l = h.layout;
        let ne = m.ne();
        let (rho, theta) = h.design();
        let (u, p) = (&x[..l.n3], &x[l.p()..l.p() + l.np]);
        match &self.list[k] {
            SolidObservable::ProbeDisplacement { component, .. } => {
                let Some(&(e, lam)) = self.probes[k].first() else {
                    return contract("internal: unresolved probe");
                };
                let t = m.mesh.elements[e];
                let v = (0..4).map(|a| lam[a] * u[3 * t[a] + component]).sum();
                if let Some((w, xs, _)) = bar {
                    for a in 0..4 {
                        xs[3 * t[a] + component] += w * lam[a];
                    }
                }
                Ok(v)
            }
            SolidObservable::ProbeSeparation { point_a_m, point_b_m, component } => {
                let [(ea, la), (eb, lb)] = self.probes[k][..] else {
                    return contract("internal: unresolved probe separation");
                };
                let (ta, tb) = (m.mesh.elements[ea], m.mesh.elements[eb]);
                let ua: f64 = (0..4).map(|a| la[a] * u[3 * ta[a] + component]).sum();
                let ub: f64 = (0..4).map(|a| lb[a] * u[3 * tb[a] + component]).sum();
                if let Some((w, xs, _)) = bar {
                    for a in 0..4 {
                        xs[3 * tb[a] + component] += w * lb[a];
                        xs[3 * ta[a] + component] -= w * la[a];
                    }
                }
                Ok(point_b_m[*component] - point_a_m[*component] + ub - ua)
            }
            SolidObservable::PlaneGap { normal, offset_m, beta } => {
                let pts =
                    self.points.as_ref().ok_or_else(|| CaeError::contract("internal: missing points"))?;
                let pos = pts.positions(u)?;
                let gaps: Vec<f64> = pos
                    .iter()
                    .map(|q| normal[0] * q[0] + normal[1] * q[1] + normal[2] * q[2] - offset_m)
                    .collect();
                let omega: Vec<(f64, f64)> = (0..pts.len())
                    .map(|q| {
                        let (b, db) = self.blocking.with_derivative(rho[pts.element[q]]);
                        (b * pts.reference_weight[q], db * pts.reference_weight[q])
                    })
                    .collect();
                let s0: f64 = omega.iter().map(|o| o.0).sum();
                if s0 <= 0.0 {
                    return crate::util::convergence("plane gap: the solid has no blocking weight");
                }
                let gmin = gaps.iter().copied().fold(f64::INFINITY, f64::min);
                let ex: Vec<f64> = gaps.iter().map(|g| (-beta * (g - gmin)).exp()).collect();
                let s1: f64 = omega.iter().zip(&ex).map(|(o, e)| o.0 * e).sum();
                if s1 <= 0.0 {
                    return crate::util::convergence(
                        "plane gap: the nearest points carry no blocking weight",
                    );
                }
                let v = gmin - (s1 / s0).ln() / beta;
                if let Some((w, xs, ps)) = bar {
                    let xbar: Vec<[f64; 3]> = (0..pts.len())
                        .map(|q| {
                            let c = w * omega[q].0 * ex[q] / s1;
                            normal.map(|n| c * n)
                        })
                        .collect();
                    for (a, b) in xs[..l.n3].iter_mut().zip(pts.positions_vjp(&xbar)?) {
                        *a += b;
                    }
                    for q in 0..pts.len() {
                        ps[pts.element[q]] -= w / beta * omega[q].1 * (ex[q] / s1 - 1.0 / s0);
                    }
                }
                Ok(v)
            }
            SolidObservable::StrainEnergy | SolidObservable::KineticEnergy => {
                let measure = if self.list[k] == SolidObservable::StrainEnergy {
                    Measure::StrainEnergy
                } else {
                    Measure::KineticEnergy
                };
                match bar {
                    None => measure.value(h, 0, x),
                    Some((w, xs, ps)) => {
                        let g = measure.eval(h, 0, x)?;
                        for (a, b) in xs.iter_mut().zip(&g.d_state) {
                            *a += w * b;
                        }
                        for (a, b) in ps.iter_mut().zip(&g.d_params) {
                            *a += w * b;
                        }
                        Ok(g.value)
                    }
                }
            }
            SolidObservable::StressAggregate { p: pe } => {
                let vols = &m.mesh.volumes;
                let vtot: f64 = vols.iter().sum();
                let mut per: Vec<(f64, [f64; 16], [f64; 2], [f64; 6])> = Vec::with_capacity(ne);
                m.for_elements(
                    |e| {
                        let d = m.gather(e, u, p);
                        let hh = Self::history_sum(h, x, e);
                        Ok(m.element_von_mises_squared(e, &d, rho[e], theta[e], &hh))
                    },
                    |_, v| {
                        per.push(v);
                        Ok(())
                    },
                )?;
                let half = 0.5 * pe;
                let sum: f64 = per.iter().zip(vols).map(|(q, v)| v * q.0.powf(half)).sum::<f64>() / vtot;
                if sum <= 0.0 {
                    return crate::util::convergence(
                        "stress aggregate at a stress-free state: the p-norm is not differentiable there",
                    );
                }
                let value = sum.powf(1.0 / pe);
                if let Some((w, xs, ps)) = bar {
                    let base = value.powf(1.0 - pe) / (2.0 * vtot);
                    for (e, (q, dd, dp, dh)) in per.iter().enumerate() {
                        let c = w * base * vols[e] * q.powf(half - 1.0);
                        let dofs = m.element_dofs(e);
                        for kk in 0..m.nl() {
                            let idx = if kk < 12 { dofs[kk] } else { l.p() + dofs[kk] - l.n3 };
                            xs[idx] += c * dd[kk];
                        }
                        ps[e] += c * dp[0];
                        ps[ne + e] += c * dp[1];
                        if l.ne_visc > 0 {
                            for i in 0..l.nb {
                                let off = l.q() + 6 * (e * l.nb + i);
                                for kk in 0..6 {
                                    xs[off + kk] += c * dh[kk];
                                }
                            }
                        }
                    }
                }
                Ok(value)
            }
        }
    }


    pub fn values(&self, history: &SoftHistory<'_>, x: &[f64]) -> Result<Vec<f64>, CaeError> {
        (0..self.len()).map(|k| self.evaluate(k, history, x, None)).collect()
    }


    pub fn vjp(
        &self,
        history: &SoftHistory<'_>,
        x: &[f64],
        bar: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), CaeError> {
        if bar.len() != self.len() {
            return contract("solid observable cotangents need one value per observable");
        }
        let mut xs = vec![0.0; x.len()];
        let mut ps = vec![0.0; 2 * history.model.ne()];
        for (k, w) in bar.iter().enumerate() {
            if *w != 0.0 {
                self.evaluate(k, history, x, Some((*w, &mut xs, &mut ps)))?;
            }
        }
        Ok((xs, ps))
    }
}
