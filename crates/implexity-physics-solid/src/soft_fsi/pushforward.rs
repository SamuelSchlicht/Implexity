// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use rayon::prelude::*;

use implexity_ad::Scalar;
use implexity_core::CaeError;

use crate::hyperelastic::kinematics::TetMesh;
use crate::util::contract;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockingMap {
    pub beta: f64,
    pub eta: f64,
    pub constant: bool,
}

impl BlockingMap {
    #[must_use]
    pub const fn identity() -> Self {
        Self { beta: 0.0, eta: 0.5, constant: false }
    }

    #[must_use]
    pub const fn constant() -> Self {
        Self { beta: 0.0, eta: 0.5, constant: true }
    }


    pub fn new(beta: f64, eta: f64) -> Result<Self, CaeError> {
        if !(beta.is_finite() && beta >= 0.0 && eta.is_finite() && eta > 0.0 && eta < 1.0) {
            return contract("fluid_blocking requires beta >= 0 and 0 < eta < 1");
        }
        Ok(Self { beta, eta, constant: false })
    }

    pub fn value<S: Scalar>(&self, rho: S) -> S {
        if self.constant {
            return S::one();
        }
        if self.beta == 0.0 {
            return rho;
        }
        let (b, e) = (self.beta, self.eta);
        let lo = (b * e).tanh();
        let den = lo + (b * (1.0 - e)).tanh();
        ((rho - e) * b).tanh() * (1.0 / den) + lo / den
    }

    #[must_use]
    pub fn with_derivative(&self, rho: f64) -> (f64, f64) {
        let d = self.value(implexity_ad::Dual::<1>::variable(rho, 0));
        (d.re, d.eps[0])
    }
}

fn subdivision_centroids(k: usize) -> Vec<[f64; 4]> {
    const PERMUTATIONS: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    #[allow(clippy::cast_precision_loss)]
    let kf = k as f64;
    let mut out = Vec::with_capacity(k * k * k);

    for c0 in 0..k {
        for c1 in 0..k {
            for c2 in 0..k {
                for p in PERMUTATIONS {
                    #[allow(clippy::cast_precision_loss)]
                    let mut y = [c0 as f64, c1 as f64, c2 as f64];

                    y[p[0]] += 0.75;
                    y[p[1]] += 0.5;
                    y[p[2]] += 0.25;
                    if y[0] < y[1] && y[1] < y[2] {
                        let z = y.map(|v| v / kf);
                        out.push([1.0 - z[2], z[2] - z[1], z[1] - z[0], z[0]]);
                    }
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct PushforwardPoints {
    pub reference: Vec<[f64; 3]>,
    pub element: Vec<usize>,
    pub barycentric: Vec<[f64; 4]>,
    pub reference_weight: Vec<f64>,
    pub points_per_axis: usize,
    offsets: Vec<usize>,
    void_threshold: f64,
    elements: Vec<[usize; 4]>,
    gradients: Vec<[[f64; 3]; 4]>,
    node_count: usize,
    mesh_points: Vec<[f64; 3]>,
}

fn jacobian_and_cofactor(u: &[f64], t: &[usize; 4], g: &[[f64; 3]; 4]) -> (f64, [[f64; 3]; 3]) {
    let mut f = [[0.0; 3]; 3];
    for (i, row) in f.iter_mut().enumerate() {
        row[i] = 1.0;
        for (a, node) in t.iter().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v += u[3 * node + i] * g[a][j];
            }
        }
    }
    let cof = [
        [
            f[1][1] * f[2][2] - f[1][2] * f[2][1],
            f[1][2] * f[2][0] - f[1][0] * f[2][2],
            f[1][0] * f[2][1] - f[1][1] * f[2][0],
        ],
        [
            f[0][2] * f[2][1] - f[0][1] * f[2][2],
            f[0][0] * f[2][2] - f[0][2] * f[2][0],
            f[0][1] * f[2][0] - f[0][0] * f[2][1],
        ],
        [
            f[0][1] * f[1][2] - f[0][2] * f[1][1],
            f[0][2] * f[1][0] - f[0][0] * f[1][2],
            f[0][0] * f[1][1] - f[0][1] * f[1][0],
        ],
    ];
    let j = f[0][0] * cof[0][0] + f[0][1] * cof[0][1] + f[0][2] * cof[0][2];
    (j, cof)
}

impl PushforwardPoints {

    pub fn new(mesh: &TetMesh, points_per_axis: usize) -> Result<Self, CaeError> {
        if !(1..=8).contains(&points_per_axis) {
            return contract("pushforward points_per_axis must lie in 1..8");
        }
        Self::with_counts(mesh, &vec![points_per_axis; mesh.elements.len()])
    }


    pub fn adaptive(
        mesh: &TetMesh,
        lattice_spacing_m: f64,
        points_per_cell_axis: f64,
    ) -> Result<Self, CaeError> {
        if !(lattice_spacing_m.is_finite() && lattice_spacing_m > 0.0) {
            return contract("adaptive pushforward points need a positive finite lattice spacing");
        }
        if !(points_per_cell_axis.is_finite() && (0.25..=8.0).contains(&points_per_cell_axis)) {
            return contract("adaptive pushforward points_per_cell_axis must lie in [0.25, 8]");
        }
        let mut counts = Vec::with_capacity(mesh.elements.len());
        for (e, v) in mesh.volumes.iter().enumerate() {
            let need = points_per_cell_axis * v.abs().cbrt() / lattice_spacing_m;
            if !(need.is_finite() && need <= 8.0) {
                return contract(format!(
                    "adaptive pushforward: element {e} needs {need:.2} points per axis (at most 8); refine the \
                     mesh or coarsen the lattice"
                ));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            counts.push((need.ceil() as usize).max(1));
        }
        Self::with_counts(mesh, &counts)
    }

    fn with_counts(mesh: &TetMesh, counts: &[usize]) -> Result<Self, CaeError> {
        let kmax = counts.iter().copied().max().unwrap_or(1);
        let mut tables: Vec<Vec<[f64; 4]>> = vec![Vec::new(); kmax + 1];
        for &k in counts {
            if tables[k].is_empty() {
                let bary = subdivision_centroids(k);
                if bary.len() != k.pow(3) {
                    return contract("internal: Freudenthal subdivision count");
                }
                tables[k] = bary;
            }
        }
        let total: usize = counts.iter().map(|k| k.pow(3)).sum();
        let mut out = Self {
            reference: Vec::with_capacity(total),
            element: Vec::with_capacity(total),
            barycentric: Vec::with_capacity(total),
            reference_weight: Vec::with_capacity(total),
            points_per_axis: kmax,
            offsets: Vec::with_capacity(counts.len() + 1),
            void_threshold: 0.0,
            elements: mesh.elements.clone(),
            gradients: mesh.gradients.clone(),
            node_count: mesh.node_count(),
            mesh_points: mesh.points.clone(),
        };
        out.offsets.push(0);
        for (e, t) in mesh.elements.iter().enumerate() {
            let bary = &tables[counts[e]];
            #[allow(clippy::cast_precision_loss)]
            let share = 1.0 / bary.len() as f64;
            for l in bary {
                let x: [f64; 3] =
                    core::array::from_fn(|i| (0..4).map(|a| l[a] * mesh.points[t[a]][i]).sum::<f64>());
                out.reference.push(x);
                out.element.push(e);
                out.barycentric.push(*l);
                out.reference_weight.push(mesh.volumes[e] * share);
            }
            out.offsets.push(out.reference.len());
        }
        Ok(out)
    }

    #[must_use]
    pub fn matches_geometry(&self, mesh: &TetMesh) -> bool {
        if mesh.points != self.mesh_points
            || mesh.elements != self.elements
            || mesh.gradients != self.gradients
            || mesh.volumes.len() != self.elements.len()
            || self.offsets.len() != self.elements.len() + 1
            || self.reference.len() != self.barycentric.len()
            || self.reference.len() != self.reference_weight.len()
            || self.reference.len() != self.element.len()
        {
            return false;
        }
        for q in 0..self.reference.len() {
            let e = self.element[q];
            if e >= self.elements.len() {
                return false;
            }
            let t = self.elements[e];
            if t.iter().any(|&a| a >= mesh.points.len()) {
                return false;
            }
            let count = self.offsets[e + 1].checked_sub(self.offsets[e]);
            let Some(count) = count.filter(|&n| n > 0) else {
                return false;
            };
            let l = self.barycentric[q];
            let position: [f64; 3] =
                core::array::from_fn(|i| (0..4).map(|a| l[a] * mesh.points[t[a]][i]).sum::<f64>());
            let share = 1.0 / count as f64;
            if position != self.reference[q] || mesh.volumes[e] * share != self.reference_weight[q] {
                return false;
            }
        }
        true
    }


    pub fn with_void_threshold(mut self, threshold: f64) -> Result<Self, CaeError> {
        if !(threshold.is_finite() && (0.0..1.0).contains(&threshold)) {
            return contract("pushforward void_threshold must lie in [0, 1)");
        }
        self.void_threshold = threshold;
        Ok(self)
    }

    #[must_use]
    pub fn void_threshold(&self) -> f64 {
        self.void_threshold
    }


    pub fn skipped_void_elements(
        &self,
        u: &[f64],
        rho: &[f64],
        blocking: &BlockingMap,
    ) -> Result<Vec<usize>, CaeError> {
        self.check_trace(u)?;
        self.check_design(rho)?;
        Ok((0..self.elements.len())
            .filter(|&e| {
                let (j, _) = jacobian_and_cofactor(u, &self.elements[e], &self.gradients[e]);
                self.skipped(j, blocking.value(rho[e]))
            })
            .collect())
    }

    #[inline]
    fn skipped(&self, j: f64, b: f64) -> bool {
        j.is_finite() && j <= 0.0 && b <= self.void_threshold
    }

    #[must_use]
    pub fn element_points(&self, e: usize) -> core::ops::Range<usize> {
        self.offsets[e]..self.offsets[e + 1]
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.reference.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reference.is_empty()
    }

    #[must_use]
    pub fn trace_size(&self) -> usize {
        3 * self.node_count
    }

    #[must_use]
    pub fn element_count(&self) -> usize {
        self.elements.len()
    }

    fn check_trace(&self, u: &[f64]) -> Result<(), CaeError> {
        if u.len() == self.trace_size() && u.iter().copied().all(f64::is_finite) {
            Ok(())
        } else {
            contract("pushforward traces must be finite node-by-XYZ displacement arrays")
        }
    }

    fn interpolate(&self, u: &[f64]) -> Vec<[f64; 3]> {
        self.barycentric
            .par_iter()
            .zip(&self.element)
            .map(|(l, e)| {
                let t = &self.elements[*e];
                core::array::from_fn(|i| (0..4).map(|a| l[a] * u[3 * t[a] + i]).sum::<f64>())
            })
            .collect()
    }

    fn transpose(&self, c: &[[f64; 3]]) -> Vec<f64> {
        let local: Vec<[f64; 12]> = (0..self.elements.len())
            .into_par_iter()
            .map(|e| {
                let mut acc = [0.0; 12];
                for q in self.element_points(e) {
                    let l = &self.barycentric[q];
                    for a in 0..4 {
                        for i in 0..3 {
                            acc[3 * a + i] += l[a] * c[q][i];
                        }
                    }
                }
                acc
            })
            .collect();
        let mut out = vec![0.0; self.trace_size()];
        for (e, acc) in local.iter().enumerate() {
            let t = &self.elements[e];
            for a in 0..4 {
                for i in 0..3 {
                    out[3 * t[a] + i] += acc[3 * a + i];
                }
            }
        }
        out
    }


    pub fn positions(&self, u: &[f64]) -> Result<Vec<[f64; 3]>, CaeError> {
        self.check_trace(u)?;
        let mut x = self.interpolate(u);
        for (xq, xr) in x.iter_mut().zip(&self.reference) {
            for i in 0..3 {
                xq[i] += xr[i];
            }
        }
        Ok(x)
    }


    pub fn positions_jvp(&self, du: &[f64]) -> Result<Vec<[f64; 3]>, CaeError> {
        self.check_trace(du)?;
        Ok(self.interpolate(du))
    }


    pub fn positions_vjp(&self, x_bar: &[[f64; 3]]) -> Result<Vec<f64>, CaeError> {
        if x_bar.len() != self.len() {
            return contract("pushforward cotangents need one vector per point");
        }
        Ok(self.transpose(x_bar))
    }


    pub fn velocities(&self, u_start: &[f64], u_end: &[f64], dt: f64) -> Result<Vec<[f64; 3]>, CaeError> {
        self.check_trace(u_start)?;
        self.check_trace(u_end)?;
        if !(dt.is_finite() && dt > 0.0) {
            return contract("pushforward velocities need a positive finite step");
        }
        let du: Vec<f64> = u_start.iter().zip(u_end).map(|(a, b)| (b - a) / dt).collect();
        Ok(self.interpolate(&du))
    }


    pub fn velocities_vjp(
        &self,
        v: &[[f64; 3]],
        v_bar: &[[f64; 3]],
        dt: f64,
    ) -> Result<(Vec<f64>, Vec<f64>, f64), CaeError> {
        if v.len() != self.len() || v_bar.len() != self.len() || !(dt.is_finite() && dt > 0.0) {
            return contract("pushforward velocity cotangents need one vector per point and a positive step");
        }
        let end: Vec<f64> = self.transpose(v_bar).iter().map(|x| x / dt).collect();
        let start: Vec<f64> = end.iter().map(|x| -x).collect();
        let dt_bar =
            -v.iter().zip(v_bar).map(|(a, b)| a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).sum::<f64>() / dt;
        Ok((start, end, dt_bar))
    }

    fn check_design(&self, rho: &[f64]) -> Result<(), CaeError> {
        if rho.len() == self.elements.len() && rho.iter().copied().all(f64::is_finite) {
            Ok(())
        } else {
            contract("pushforward weights need one finite physical density per element")
        }
    }


    pub fn weights(&self, u: &[f64], rho: &[f64], blocking: &BlockingMap) -> Result<Vec<f64>, CaeError> {
        self.check_trace(u)?;
        self.check_design(rho)?;
        let per: Vec<Result<f64, CaeError>> = (0..self.elements.len())
            .into_par_iter()
            .map(|e| {
                let (j, _) = jacobian_and_cofactor(u, &self.elements[e], &self.gradients[e]);
                let b = blocking.value(rho[e]);
                if self.skipped(j, b) {
                    return Ok(0.0);
                }
                if !(j.is_finite() && j > 0.0) {
                    return crate::util::convergence(format!(
                        "pushforward: element {e} is inverted (J = {j:.3e}, blocking weight {b:.3e})"
                    ));
                }
                Ok(b * j)
            })
            .collect();
        let mut out = Vec::with_capacity(self.len());
        for (e, f) in per.into_iter().enumerate() {
            let f = f?;
            for q in self.element_points(e) {
                out.push(f * self.reference_weight[q]);
            }
        }
        Ok(out)
    }


    pub fn weights_jvp(
        &self,
        u: &[f64],
        rho: &[f64],
        blocking: &BlockingMap,
        du: &[f64],
        drho: &[f64],
    ) -> Result<Vec<f64>, CaeError> {
        self.check_trace(u)?;
        self.check_trace(du)?;
        self.check_design(rho)?;
        self.check_design(drho)?;
        let per: Vec<f64> = (0..self.elements.len())
            .into_par_iter()
            .map(|e| {
                let t = &self.elements[e];
                let g = &self.gradients[e];
                let (j, cof) = jacobian_and_cofactor(u, t, g);
                let (b, db) = blocking.with_derivative(rho[e]);
                if self.skipped(j, b) {
                    return 0.0;
                }
                let mut dj = 0.0;
                for (a, node) in t.iter().enumerate() {
                    for i in 0..3 {
                        for jj in 0..3 {
                            dj += cof[i][jj] * g[a][jj] * du[3 * node + i];
                        }
                    }
                }
                b * dj + db * drho[e] * j
            })
            .collect();
        let mut out = Vec::with_capacity(self.len());
        for (e, f) in per.iter().enumerate() {
            for q in self.element_points(e) {
                out.push(f * self.reference_weight[q]);
            }
        }
        Ok(out)
    }


    pub fn weights_vjp(
        &self,
        u: &[f64],
        rho: &[f64],
        blocking: &BlockingMap,
        a_bar: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), CaeError> {
        self.check_trace(u)?;
        self.check_design(rho)?;
        if a_bar.len() != self.len() {
            return contract("pushforward weight cotangents need one value per point");
        }
        let per: Vec<([f64; 12], f64)> = (0..self.elements.len())
            .into_par_iter()
            .map(|e| {
                let t = &self.elements[e];
                let g = &self.gradients[e];
                let (j, cof) = jacobian_and_cofactor(u, t, g);
                let (b, db) = blocking.with_derivative(rho[e]);
                if self.skipped(j, b) {
                    return ([0.0; 12], 0.0);
                }
                let s: f64 = self.element_points(e).map(|q| a_bar[q] * self.reference_weight[q]).sum();
                let mut local = [0.0; 12];
                for a in 0..4 {
                    for i in 0..3 {
                        local[3 * a + i] = b * s * (0..3).map(|jj| cof[i][jj] * g[a][jj]).sum::<f64>();
                    }
                }
                (local, db * j * s)
            })
            .collect();
        let mut u_bar = vec![0.0; self.trace_size()];
        let mut rho_bar = vec![0.0; self.elements.len()];
        for (e, (local, r)) in per.iter().enumerate() {
            let t = &self.elements[e];
            for a in 0..4 {
                for i in 0..3 {
                    u_bar[3 * t[a] + i] += local[3 * a + i];
                }
            }
            rho_bar[e] = *r;
        }
        Ok((u_bar, rho_bar))
    }


    pub fn nodal_force(&self, impulses: &[[f64; 3]], dt: f64) -> Result<Vec<f64>, CaeError> {
        if impulses.len() != self.len() || !(dt.is_finite() && dt > 0.0) {
            return contract("pushforward impulses need one vector per point and a positive step");
        }
        Ok(self.transpose(impulses).iter().map(|v| v / dt).collect())
    }


    pub fn nodal_force_vjp(
        &self,
        force: &[f64],
        f_bar: &[f64],
        dt: f64,
    ) -> Result<(Vec<[f64; 3]>, f64), CaeError> {
        self.check_trace(f_bar)?;
        if force.len() != self.trace_size() || !(dt.is_finite() && dt > 0.0) {
            return contract("pushforward force cotangents need node-by-XYZ arrays and a positive step");
        }
        let i_bar: Vec<[f64; 3]> = self.interpolate(f_bar).iter().map(|v| v.map(|x| x / dt)).collect();
        let dt_bar = -force.iter().zip(f_bar).map(|(a, b)| a * b).sum::<f64>() / dt;
        Ok((i_bar, dt_bar))
    }
}
