// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::HyperDual;
use implexity_core::CaeError;
use implexity_linalg::sparse::CscMatrix;

use super::{SoftHistory, StepCotangent, StepSolution};
use crate::util::contract;

pub const MAX_GAUSS_POINTS: usize = 8;


pub fn gauss_legendre(n: usize) -> Result<(Vec<f64>, Vec<f64>), CaeError> {
    if !(1..=MAX_GAUSS_POINTS).contains(&n) {
        return contract("avf_midpoint requires 1..8 Gauss-Legendre points");
    }
    #[allow(clippy::cast_precision_loss)]
    let nf = n as f64;
    let legendre = |x: f64| -> (f64, f64) {

        let (mut p0, mut p1) = (1.0, x);
        for k in 2..=n {
            #[allow(clippy::cast_precision_loss)]
            let kf = k as f64;
            let p2 = ((2.0 * kf - 1.0) * x * p1 - (kf - 1.0) * p0) / kf;
            p0 = p1;
            p1 = p2;
        }
        (p1, nf * (x * p1 - p0) / (x * x - 1.0))
    };
    let mut nodes = vec![0.0; n];
    let mut weights = vec![0.0; n];
    for i in 0..n.div_ceil(2) {
        #[allow(clippy::cast_precision_loss)]
        let mut x = (std::f64::consts::PI * (i as f64 + 0.75) / (nf + 0.5)).cos();
        if 2 * i + 1 == n {
            x = 0.0;
        } else {
            for _ in 0..100 {
                let (p, dp) = legendre(x);
                let dx = p / dp;
                x -= dx;
                if dx.abs() <= 1e-16 {
                    break;
                }
            }
        }
        let (_, dp) = legendre(x);
        let w = 1.0 / ((1.0 - x * x) * dp * dp);
        nodes[i] = 0.5 * (1.0 - x);
        nodes[n - 1 - i] = 1.0 - nodes[i];
        weights[i] = w;
        weights[n - 1 - i] = w;
    }
    if n % 2 == 1 {
        nodes[n / 2] = 0.5;
    }
    Ok((nodes, weights))
}


#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EnergyLedger {
    pub kinetic_start: f64,
    pub kinetic_end: f64,
    pub potential_start: f64,
    pub potential_end: f64,
    pub external_work: f64,
    pub support_work: f64,
    pub damping_dissipation: f64,
    pub viscous_work: f64,
    pub quadrature_defect: f64,
    pub algorithmic_dissipation: f64,
    pub balance_residual: f64,
}

pub(super) struct Pass<'a> {
    pub(super) jacobian: bool,
    pub(super) pull: Option<&'a [f64]>,
}

pub(super) struct PathForces {
    pub(super) r: Vec<f64>,
    pub(super) rp: Vec<f64>,
    jac: Option<CscMatrix>,
    terms: [f64; 2],
    pull_u: Vec<f64>,
    pull_p: Vec<f64>,
}

impl SoftHistory<'_> {
    fn avf_rule(gauss_points: usize) -> Result<(Vec<f64>, Vec<f64>), CaeError> {
        gauss_legendre(gauss_points)
    }

    fn avf_load(&self, t: usize, extra: Option<&[f64]>) -> Vec<f64> {
        let a0 = self.amplitude_before(&self.loading.force_amplitude, t);
        let amp = 0.5 * (a0 + self.loading.force_amplitude[t]);
        let mut f: Vec<f64> = self.loading.force.iter().map(|v| v * amp).collect();
        if let Some(x) = extra {
            for (a, b) in f.iter_mut().zip(x) {
                *a += b;
            }
        }
        f
    }

    pub(super) fn avf_pressure(&self, t: usize) -> f64 {
        let a0 = self.amplitude_before(&self.loading.pressure_amplitude, t);
        self.loading.pressure * 0.5 * (a0 + self.loading.pressure_amplitude[t])
    }

    fn avf_start<'x>(&self, x: &'x [f64]) -> (&'x [f64], &'x [f64]) {
        let l = self.layout;
        (&x[l.u()..l.u() + l.n3], &x[l.p()..l.p() + l.np])
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn avf_assemble(
        &self,
        t: usize,
        x: &[f64],
        u1: &[f64],
        p1: &[f64],
        extra: Option<&[f64]>,
        gauss_points: usize,
        pass: &Pass<'_>,
    ) -> Result<PathForces, CaeError> {
        let m = self.model;
        let l = self.layout;
        let dt = self.dt(t);
        let (s, w) = Self::avf_rule(gauss_points)?;
        let (u0, p0) = self.avf_start(x);
        let nl = m.nl();
        let mut out = PathForces {
            r: vec![0.0; l.n3],
            rp: vec![0.0; l.np],
            jac: if pass.jacobian { Some(m.zero_matrix()?) } else { None },
            terms: [0.0; 2],
            pull_u: vec![0.0; if pass.pull.is_some() { l.n3 } else { 0 }],
            pull_p: vec![0.0; if pass.pull.is_some() { l.np } else { 0 }],
        };
        let need_hessian = pass.jacobian || pass.pull.is_some();
        let (cm, cc) = (2.0 / (dt * dt), 1.0 / dt);
        let (am, bk) = self.rayleigh;
        m.for_elements(
            |e| {
                let d0 = m.gather(e, u0, p0);
                let d1 = m.gather(e, u1, p1);
                let h = self.history_h(x, e, dt);
                let (_, gf) = self.branch_factors(e, dt);
                let mut grad = [0.0; 16];
                let mut hess = if need_hessian { Some(Box::new([0.0; 256])) } else { None };
                let mut big = [0.0_f64; 2];
                for (sg, wg) in s.iter().zip(&w) {
                    let d: [f64; 16] = core::array::from_fn(|k| d0[k] + sg * (d1[k] - d0[k]));
                    let local =
                        m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, gf, need_hessian)?;
                    for k in 0..nl {
                        grad[k] += wg * local.gradient[k];
                        big[usize::from(k >= 12)] = big[usize::from(k >= 12)].max(local.gradient[k].abs());
                    }
                    if let Some(hs) = hess.as_mut() {
                        for (a, b) in hs.iter_mut().zip(local.hessian.iter()) {
                            *a += wg * sg * b;
                        }
                    }
                }
                Ok((grad, hess, big))
            },
            |e, (grad, hess, big)| {
                let dofs = m.element_dofs(e);
                for k in 0..12 {
                    out.r[dofs[k]] += grad[k];
                }
                for k in 12..nl {
                    out.rp[dofs[k] - l.n3] += grad[k];
                }
                out.terms[0] = out.terms[0].max(big[0]);
                out.terms[1] = out.terms[1].max(big[1]);
                if let (Some(hs), Some(wr)) = (hess.as_ref(), pass.pull) {
                    for a in 0..nl {
                        let mut acc = 0.0;
                        for b in 0..12 {
                            acc += hs[b * 16 + a] * wr[dofs[b]];
                        }
                        if a < 12 {
                            out.pull_u[dofs[a]] += acc;
                        } else {
                            out.pull_p[dofs[a] - l.n3] += acc;
                        }
                    }
                }
                if let (Some(hs), Some(a)) = (hess.as_ref(), out.jac.as_mut()) {
                    let mut block = [0.0; 256];
                    block[..].copy_from_slice(&hs[..]);
                    let me = m.element_mass(e, self.ctx.mass[e]);
                    let ke = m.element_reference_stiffness(e);
                    for ai in 0..4 {
                        for bi in 0..4 {
                            for i in 0..3 {
                                block[(3 * ai + i) * 16 + 3 * bi + i] += (cm + cc * am) * me[ai][bi];
                                for j in 0..3 {
                                    block[(3 * ai + i) * 16 + 3 * bi + j] +=
                                        cc * bk * self.ctx.stiffness[e] * ke[(3 * ai + i) * 12 + 3 * bi + j];
                                }
                            }
                        }
                    }
                    m.scatter(a, &dofs[..nl], &block, 16);
                }
                Ok(())
            },
        )?;
        let path = |sg: f64| -> Vec<f64> { u0.iter().zip(u1).map(|(a, b)| a + sg * (b - a)).collect() };
        for pot in &m.potentials {
            for (sg, wg) in s.iter().zip(&w) {
                let ug = path(*sg);
                let f = pot.force(&ug, &self.params)?;
                if f.len() != l.n3 {
                    return contract("nodal potential force must be node-by-XYZ");
                }
                for (ri, fi) in out.r.iter_mut().zip(&f) {
                    *ri += wg * fi;
                    out.terms[0] = out.terms[0].max(fi.abs());
                }
                if pass.jacobian || pass.pull.is_some() {
                    let tangent = pot.tangent(&ug, &self.params)?;
                    if let Some(a) = out.jac.as_mut() {
                        m.scatter_triplets(a, &tangent, wg * sg)?;
                    }
                    if let Some(wr) = pass.pull {
                        for (r, c, v) in &tangent {
                            out.pull_u[*c] += wg * sg * v * wr[*r];
                        }
                    }
                }
            }
        }
        let pressure = self.avf_pressure(t);
        if pressure != 0.0 {
            for (f, face) in m.faces.iter().enumerate() {
                let dofs: [usize; 9] = core::array::from_fn(|k| 3 * face[k / 3] + k % 3);
                for (sg, wg) in s.iter().zip(&w) {
                    let ug = path(*sg);
                    let (res, jac) = m.face_local(f, &ug, pressure);
                    for node in face {
                        for i in 0..3 {
                            out.r[3 * node + i] += wg * res[i];
                            out.terms[0] = out.terms[0].max(res[i].abs());
                        }
                    }
                    if let Some(a) = out.jac.as_mut() {
                        let mut block = [0.0; 81];
                        for nk in 0..3 {
                            for i in 0..3 {
                                for k in 0..9 {
                                    block[(3 * nk + i) * 9 + k] = wg * sg * jac[i][k];
                                }
                            }
                        }
                        m.scatter(a, &dofs, &block, 9);
                    }
                    if let Some(wr) = pass.pull {
                        for k in 0..9 {
                            let mut acc = 0.0;
                            for node in face {
                                for i in 0..3 {
                                    acc += jac[i][k] * wr[3 * node + i];
                                }
                            }
                            out.pull_u[dofs[k]] += wg * sg * acc;
                        }
                    }
                }
            }
        }
        let fd = self.avf_load(t, extra);
        for (ri, fi) in out.r.iter_mut().zip(&fd) {
            *ri -= fi;
            out.terms[0] = out.terms[0].max(fi.abs());
        }
        Ok(out)
    }

    pub(super) fn avf_kinematics(&self, t: usize, x: &[f64], u1: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let l = self.layout;
        let dt = self.dt(t);
        let mut v1: Vec<f64> = (0..l.n3).map(|i| 2.0 * (u1[i] - x[l.u() + i]) / dt - x[l.v() + i]).collect();
        let (g, _) = self.prescribed_rates(t);
        for i in (0..l.n3).filter(|i| self.model.fixed[*i]) {
            v1[i] = g[i];
        }
        let a1: Vec<f64> = (0..l.n3).map(|i| (v1[i] - x[l.v() + i]) / dt).collect();
        (v1, a1)
    }

    pub(super) fn avf_inertia(&self, t: usize, x: &[f64], u1: &[f64]) -> Vec<f64> {
        let l = self.layout;
        let dt = self.dt(t);
        let (u0, v0) = (&x[l.u()..l.u() + l.n3], &x[l.v()..l.v() + l.n3]);
        let mut da: Vec<f64> = (0..l.n3).map(|i| 2.0 / (dt * dt) * (u1[i] - u0[i] - dt * v0[i])).collect();
        let (g, _) = self.prescribed_rates(t);
        for i in (0..l.n3).filter(|i| self.model.fixed[*i]) {
            da[i] = (g[i] - v0[i]) / dt;
        }
        da
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn avf_system(
        &self,
        t: usize,
        x: &[f64],
        y: &[f64],
        extra: Option<&[f64]>,
        jacobian: bool,
        gauss_points: usize,
    ) -> Result<(Vec<f64>, Vec<f64>, Option<CscMatrix>, [f64; 2]), CaeError> {
        let m = self.model;
        let l = self.layout;
        let dt = self.dt(t);
        let (u1, p1) = self.expand(t, y);
        let pf = self.avf_assemble(t, x, &u1, &p1, extra, gauss_points, &Pass { jacobian, pull: None })?;
        let mut terms = pf.terms;
        let (u0, v0) = (&x[l.u()..l.u() + l.n3], &x[l.v()..l.v() + l.n3]);
        let du: Vec<f64> = (0..l.n3).map(|i| u1[i] - u0[i]).collect();
        let da = self.avf_inertia(t, x, &u1);
        let ma = self.ctx.m.matvec(&da).map_err(|e| CaeError::contract(e.to_string()))?;
        let cv = self.damping_matvec(&du.iter().map(|d| d / dt).collect::<Vec<_>>())?;
        let mut full = pf.r.clone();
        for i in 0..l.n3 {
            full[i] += ma[i] + cv[i];
            terms[0] = terms[0].max(ma[i].abs()).max(cv[i].abs());
        }

        let amax = |v: &[f64]| v.iter().fold(0.0_f64, |a, b| a.max(b.abs()));
        let mass_row = self.ctx.m.norm_inf();
        let damp_row = self.rayleigh.0 * mass_row + self.rayleigh.1 * self.ctx.kref.norm_inf();
        terms[0] = terms[0]
            .max(2.0 / (dt * dt) * (mass_row + 0.5 * dt * damp_row) * (amax(&u1) + amax(u0) + dt * amax(v0)));
        let mut res = vec![0.0; m.n_unknowns];
        for i in 0..l.n3 {
            if m.unknown[i] != usize::MAX {
                res[m.unknown[i]] = full[i];
            }
        }
        for a in 0..l.np {
            res[m.unknown[l.n3 + a]] = pf.rp[a];
        }
        Ok((res, pf.r, pf.jac, terms))
    }

    fn avf_total_energy(
        &self,
        t: usize,
        x: &[f64],
        u: &[f64],
        p: &[f64],
        incremental: bool,
    ) -> Result<(f64, f64), CaeError> {
        let m = self.model;
        let dt = self.dt(t);
        let mut total = 0.0;
        let mut size = 0.0;
        m.for_elements(
            |e| {
                let d = m.gather(e, u, p);
                let (h, g) = if incremental {
                    (self.history_h(x, e, dt), self.branch_factors(e, dt).1)
                } else {
                    ([0.0; 6], 1.0)
                };
                m.element_energy::<f64>(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, g)
            },
            |e, v| {
                total += v;
                size += v.abs() + 4.0 * m.materials[m.element_material[e]].mu0 * m.mesh.volumes[e];
                Ok(())
            },
        )?;
        for pot in &m.potentials {
            let v = pot.energy(u, &self.params)?;
            total += v;
            size += v.abs();
        }
        Ok((total, size))
    }

    pub(super) fn avf_potential(
        &self,
        t: usize,
        x: &[f64],
        y: &[f64],
        extra: Option<&[f64]>,
        gauss_points: usize,
    ) -> Result<(f64, f64), CaeError> {
        let l = self.layout;
        let dt = self.dt(t);
        let (s, w) = Self::avf_rule(gauss_points)?;
        let (u1, p1) = self.expand(t, y);
        let (u0, p0) = self.avf_start(x);
        let (e0, size0) = self.avf_total_energy(t, x, u0, p0, true)?;
        let mut phi = 0.0;
        let mut size = 0.0;
        for (sg, wg) in s.iter().zip(&w) {
            let ug: Vec<f64> = u0.iter().zip(&u1).map(|(a, b)| a + sg * (b - a)).collect();
            let pg: Vec<f64> = p0.iter().zip(&p1).map(|(a, b)| a + sg * (b - a)).collect();
            let (eg, sz) = self.avf_total_energy(t, x, &ug, &pg, true)?;
            phi += wg / sg * (eg - e0);
            size += wg / sg * (sz + size0);
        }
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f64>();
        let absdot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| (p * q).abs()).sum::<f64>();
        let fd = self.avf_load(t, extra);
        phi -= dot(&fd, &u1);
        size += absdot(&fd, &u1);
        let v0 = &x[l.v()..l.v() + l.n3];
        let du: Vec<f64> = (0..l.n3).map(|i| u1[i] - u0[i]).collect();

        let mut z: Vec<f64> = (0..l.n3).map(|i| du[i] - dt * v0[i]).collect();
        let (g, _) = self.prescribed_rates(t);
        for i in (0..l.n3).filter(|i| self.model.fixed[*i]) {
            z[i] = 0.5 * dt * (g[i] - v0[i]);
        }
        let mz = self.ctx.m.matvec(&z).map_err(|e| CaeError::contract(e.to_string()))?;
        let cdu = self.damping_matvec(&du)?;
        phi += dot(&z, &mz) / (dt * dt) + dot(&du, &cdu) / (2.0 * dt);
        size += absdot(&z, &mz) / (dt * dt) + absdot(&du, &cdu) / (2.0 * dt);
        Ok((phi, size))
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn avf_step_vjp(
        &self,
        t: usize,
        x: &[f64],
        next: &StepSolution,
        extra: Option<&[f64]>,
        w: &[f64],
        gauss_points: usize,
    ) -> Result<StepCotangent, CaeError> {
        let m = self.model;
        let l = self.layout;
        let ne = m.ne();
        let nl = m.nl();
        let dt = self.dt(t);
        let (s, wg_rule) = Self::avf_rule(gauss_points)?;
        let x1 = &next.state;
        let (u1, p1) = (&x1[..l.n3], &x1[l.p()..l.p() + l.np]);
        let (u0, p0) = self.avf_start(x);
        let mut wx = vec![0.0; l.len()];
        let mut wparams = vec![0.0; 2 * ne];
        let mut wu1 = w[..l.n3].to_vec();
        let mut wp1 = w[l.p()..l.p() + l.np].to_vec();
        let wr = &w[l.r()..l.r() + l.n3];

        self.viscous_output_vjp(dt, u1, p1, w, &mut wx, &mut wu1, &mut wparams)?;
        for i in 0..l.n3 {
            let (wv, wa) = (w[l.v() + i], w[l.a() + i]);
            if m.fixed[i] {
                wx[l.v() + i] -= wa / dt;
                continue;
            }
            let g = 2.0 / dt * wv + 2.0 / (dt * dt) * wa;
            wu1[i] += g;
            wx[l.u() + i] -= g;
            wx[l.v() + i] -= wv + 2.0 / dt * wa;
        }

        let pf =
            self.avf_assemble(t, x, u1, p1, extra, gauss_points, &Pass { jacobian: true, pull: Some(wr) })?;
        for i in 0..l.n3 {
            wu1[i] += pf.pull_u[i];
        }
        for a in 0..l.np {
            wp1[a] += pf.pull_p[a];
        }

        let mut rhs = vec![0.0; m.n_unknowns];
        for i in 0..l.n3 {
            if m.unknown[i] != usize::MAX {
                rhs[m.unknown[i]] = wu1[i];
            }
        }
        for a in 0..l.np {
            rhs[m.unknown[l.n3 + a]] = wp1[a];
        }
        let jac = pf.jac.ok_or_else(|| CaeError::contract("internal: missing Jacobian"))?;
        let lu = m.factor(&jac)?;
        let mu = lu
            .solve_transpose(&rhs)
            .map_err(|e| CaeError::convergence(format!("adjoint step solve: {e}")))?;
        let mut mu_u = vec![0.0; l.n3];
        for i in 0..l.n3 {
            if m.unknown[i] != usize::MAX {
                mu_u[i] = mu[m.unknown[i]];
            }
        }
        let mu_p: Vec<f64> = (0..l.np).map(|a| mu[m.unknown[l.n3 + a]]).collect();

        let omega_u: Vec<f64> = (0..l.n3).map(|i| wr[i] - mu_u[i]).collect();

        let (am, bk) = self.rayleigh;
        let mmu = self.ctx.m.matvec(&mu_u).map_err(|e| CaeError::contract(e.to_string()))?;
        let cmu = self.damping_matvec(&mu_u)?;
        for i in 0..l.n3 {
            if m.fixed[i] {
                wx[l.u() + i] += cmu[i] / dt;
                wx[l.v() + i] += mmu[i] / dt;
            } else {
                wx[l.u() + i] += 2.0 / (dt * dt) * mmu[i] + cmu[i] / dt;
                wx[l.v() + i] += 2.0 / dt * mmu[i];
            }
        }
        let a1 = &x1[l.a()..l.a() + l.n3];
        let vmid: Vec<f64> = (0..l.n3).map(|i| (u1[i] - u0[i]) / dt).collect();
        for e in 0..ne {
            let t_ = m.mesh.elements[e];
            let me0 = m.element_mass(e, 1.0);
            let ke = m.element_reference_stiffness(e);
            let dm = HyperDual::new(self.ctx.rho[e], 1.0, 0.0, 0.0);
            let dmass = m.interpolation.mass(dm).e1;
            let dstiff = m.interpolation.stiffness(dm).e1;
            let (mut sm_a, mut sm_v, mut sk_v) = (0.0, 0.0, 0.0);
            for a in 0..4 {
                for b in 0..4 {
                    for i in 0..3 {
                        let mu_ai = mu_u[3 * t_[a] + i];
                        sm_a += mu_ai * me0[a][b] * a1[3 * t_[b] + i];
                        sm_v += mu_ai * me0[a][b] * vmid[3 * t_[b] + i];
                        for j in 0..3 {
                            sk_v += mu_ai * ke[(3 * a + i) * 12 + 3 * b + j] * vmid[3 * t_[b] + j];
                        }
                    }
                }
            }
            wparams[e] -= dmass * (sm_a + am * sm_v) + dstiff * bk * sk_v;
        }

        let with_h = l.ne_visc > 0;
        let seeds = if with_h { 8 } else { 2 };
        let parts: Vec<(usize, [f64; 16], [f64; 8])> = {
            let mut out = Vec::new();
            m.for_elements(
                |e| {
                    let dofs = m.element_dofs(e);
                    let omega: [f64; 16] = core::array::from_fn(|k| {
                        if k >= nl {
                            0.0
                        } else if k < 12 {
                            omega_u[dofs[k]]
                        } else {
                            -mu_p[dofs[k] - l.n3]
                        }
                    });
                    if omega.iter().all(|v| *v == 0.0) {
                        return Ok(None);
                    }
                    let d0 = m.gather(e, u0, p0);
                    let d1 = m.gather(e, u1, p1);
                    let h = self.history_h(x, e, dt);
                    let (_, gf) = self.branch_factors(e, dt);
                    let mut back = [0.0; 16];
                    let mut par = [0.0; 8];
                    for (sg, wg) in s.iter().zip(&wg_rule) {
                        let d: [f64; 16] = core::array::from_fn(|k| d0[k] + sg * (d1[k] - d0[k]));
                        let local =
                            m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, gf, true)?;
                        for a in 0..nl {
                            let mut acc = 0.0;
                            for b in 0..nl {
                                acc += local.hessian[b * 16 + a] * omega[b];
                            }
                            back[a] += wg * (1.0 - sg) * acc;
                        }
                        for (sd, slot) in par.iter_mut().enumerate().take(seeds) {
                            let dd: [HyperDual; 16] =
                                core::array::from_fn(|k| HyperDual::new(d[k], wg * omega[k], 0.0, 0.0));
                            let rho =
                                HyperDual::new(self.ctx.rho[e], 0.0, if sd == 0 { 1.0 } else { 0.0 }, 0.0);
                            let th =
                                HyperDual::new(self.ctx.theta[e], 0.0, if sd == 1 { 1.0 } else { 0.0 }, 0.0);
                            let hh: [HyperDual; 6] = core::array::from_fn(|k| {
                                HyperDual::new(h[k], 0.0, if sd == 2 + k { 1.0 } else { 0.0 }, 0.0)
                            });
                            *slot += m.element_energy(e, &dd, rho, th, &hh, gf)?.e12;
                        }
                    }
                    Ok(Some((back, par)))
                },
                |e, v| {
                    if let Some((back, par)) = v {
                        out.push((e, back, par));
                    }
                    Ok(())
                },
            )?;
            out
        };
        for (e, back, par) in parts {
            let dofs = m.element_dofs(e);
            for k in 0..12 {
                wx[l.u() + dofs[k]] += back[k];
            }
            for k in 12..nl {
                wx[l.p() + dofs[k] - l.n3] += back[k];
            }
            wparams[e] += par[0];
            wparams[ne + e] += par[1];
            if with_h {
                let (factors, _) = self.branch_factors(e, dt);
                for (i, (ei, bi)) in factors.iter().enumerate() {
                    let off = 6 * (e * l.nb + i);
                    for k in 0..6 {
                        wx[l.q() + off + k] += ei * par[2 + k];
                        wx[l.s() + 6 * e + k] -= bi * par[2 + k];
                    }
                }
            }
        }
        let path = |sg: f64| -> Vec<f64> { u0.iter().zip(u1).map(|(a, b)| a + sg * (b - a)).collect() };
        for pot in &m.potentials {
            for (sg, wg) in s.iter().zip(&wg_rule) {
                let ug = path(*sg);
                for (r, c, v) in pot.tangent(&ug, &self.params)? {
                    wx[l.u() + c] += wg * (1.0 - sg) * v * omega_u[r];
                }
                let scaled: Vec<f64> = omega_u.iter().map(|o| wg * o).collect();
                let g = pot.force_params_vjp(&ug, &self.params, &scaled)?;
                if g.len() != 2 * ne {
                    return contract("nodal potential parameter cotangent must hold 2 values per element");
                }
                for (a, b) in wparams.iter_mut().zip(&g) {
                    *a += b;
                }
            }
        }
        let pressure = self.avf_pressure(t);
        if pressure != 0.0 {
            for (f, face) in m.faces.iter().enumerate() {
                for (sg, wg) in s.iter().zip(&wg_rule) {
                    let ug = path(*sg);
                    let (_, jac) = m.face_local(f, &ug, pressure);
                    for k in 0..9 {
                        let mut acc = 0.0;
                        for node in face {
                            for i in 0..3 {
                                acc += jac[i][k] * omega_u[3 * node + i];
                            }
                        }
                        wx[l.u() + 3 * face[k / 3] + k % 3] += wg * (1.0 - sg) * acc;
                    }
                }
            }
        }

        let extra_load: Vec<f64> = (0..l.n3).map(|i| -wr[i] + mu_u[i]).collect();
        Ok(StepCotangent { state: wx, params: wparams, extra_load })
    }


    #[allow(clippy::too_many_lines)]
    pub fn energy_ledger(
        &self,
        t: usize,
        x: &[f64],
        x1: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<EnergyLedger, CaeError> {
        let m = self.model;
        let l = self.layout;
        if t >= self.loading.steps() || x.len() != l.len() || x1.len() != l.len() {
            return contract("step index or state length out of range");
        }
        if let Some(e) = extra
            && e.len() != l.n3
        {
            return contract("the extra nodal load must be node-by-XYZ");
        }
        let dt = self.dt(t);
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f64>();
        let (u0, p0) = (&x[..l.n3], &x[l.p()..l.p() + l.np]);
        let (u1, p1) = (&x1[..l.n3], &x1[l.p()..l.p() + l.np]);
        let (v0, v1) = (&x[l.v()..l.v() + l.n3], &x1[l.v()..l.v() + l.n3]);
        let du: Vec<f64> = (0..l.n3).map(|i| u1[i] - u0[i]).collect();
        let kinetic = |v: &[f64]| -> Result<f64, CaeError> {
            let mv = self.ctx.m.matvec(v).map_err(|e| CaeError::contract(e.to_string()))?;
            Ok(0.5 * dot(v, &mv))
        };
        let mut out = EnergyLedger {
            kinetic_start: kinetic(v0)?,
            kinetic_end: kinetic(v1)?,
            potential_start: self.avf_total_energy(t, x, u0, p0, false)?.0,
            potential_end: self.avf_total_energy(t, x, u1, p1, false)?.0,
            ..EnergyLedger::default()
        };
        let inc0 = self.avf_total_energy(t, x, u0, p0, true)?.0;
        let inc1 = self.avf_total_energy(t, x, u1, p1, true)?.0;
        out.viscous_work = (inc1 - inc0) - (out.potential_end - out.potential_start);
        let cdu = self.damping_matvec(&du)?;
        out.damping_dissipation = dot(&du, &cdu) / dt;

        let fd = self.avf_load(t, extra);
        out.external_work = dot(&fd, &du);
        let avf = matches!(self.scheme, super::Scheme::AvfMidpoint { .. });
        let (s, w) = match self.scheme {
            super::Scheme::AvfMidpoint { gauss_points } => Self::avf_rule(gauss_points)?,
            _ => (vec![0.0, 1.0], vec![0.5, 0.5]),
        };
        let pressure = if avf {
            self.avf_pressure(t)
        } else {
            0.5 * (self.pressure(t)
                + self.loading.pressure * self.amplitude_before(&self.loading.pressure_amplitude, t))
        };
        if pressure != 0.0 {
            for (f, face) in m.faces.iter().enumerate() {
                for (sg, wg) in s.iter().zip(&w) {
                    let ug: Vec<f64> = u0.iter().zip(u1).map(|(a, b)| a + sg * (b - a)).collect();
                    let (res, _) = m.face_local(f, &ug, pressure);
                    for node in face {
                        for i in 0..3 {
                            out.external_work -= wg * res[i] * du[3 * node + i];
                        }
                    }
                }
            }
        }
        if avf {

            let r_bar = &x1[l.r()..l.r() + l.n3];
            let dv: Vec<f64> = (0..l.n3).map(|i| (v1[i] - v0[i]) / dt).collect();
            let mdv = self.ctx.m.matvec(&dv).map_err(|e| CaeError::contract(e.to_string()))?;
            for i in 0..l.n3 {
                if m.fixed[i] {
                    let travel = 0.5 * dt * (v0[i] + v1[i]);
                    out.support_work += mdv[i] * travel + (cdu[i] / dt + r_bar[i]) * du[i];
                }
            }
            let mut averaged = 0.0;
            for (sg, wg) in s.iter().zip(&w) {
                let ug: Vec<f64> = u0.iter().zip(u1).map(|(a, b)| a + sg * (b - a)).collect();
                let pg: Vec<f64> = p0.iter().zip(p1).map(|(a, b)| a + sg * (b - a)).collect();
                let mut g_u = vec![0.0; l.n3];
                let mut g_p = vec![0.0; l.np];
                m.for_elements(
                    |e| {
                        let d = m.gather(e, &ug, &pg);
                        let h = self.history_h(x, e, dt);
                        let (_, gf) = self.branch_factors(e, dt);
                        m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, gf, false)
                    },
                    |e, local| {
                        let dofs = m.element_dofs(e);
                        for k in 0..12 {
                            g_u[dofs[k]] += local.gradient[k];
                        }
                        for k in 12..m.nl() {
                            g_p[dofs[k] - l.n3] += local.gradient[k];
                        }
                        Ok(())
                    },
                )?;
                for pot in &m.potentials {
                    for (a, b) in g_u.iter_mut().zip(pot.force(&ug, &self.params)?) {
                        *a += b;
                    }
                }
                let dp: Vec<f64> = (0..l.np).map(|a| p1[a] - p0[a]).collect();
                averaged += wg * (dot(&g_u, &du) + dot(&g_p, &dp));
            }
            out.quadrature_defect = (inc1 - inc0) - averaged;
        } else {

            let end_force = |xs: &[f64]| -> Result<Vec<f64>, CaeError> {
                let ma = self
                    .ctx
                    .m
                    .matvec(&xs[l.a()..l.a() + l.n3])
                    .map_err(|e| CaeError::contract(e.to_string()))?;
                let cv = self.damping_matvec(&xs[l.v()..l.v() + l.n3])?;
                Ok((0..l.n3).map(|i| ma[i] + cv[i] + xs[l.r() + i]).collect())
            };
            let (f0, f1) = (end_force(x)?, end_force(x1)?);
            for i in 0..l.n3 {
                if m.fixed[i] {
                    out.support_work += 0.5 * (f0[i] + f1[i]) * du[i];
                }
            }
        }
        let change = (out.kinetic_end - out.kinetic_start)
            + (out.potential_end - out.potential_start)
            + out.damping_dissipation
            + out.viscous_work;
        out.algorithmic_dissipation = out.external_work + out.support_work - change;
        out.balance_residual = out.algorithmic_dissipation + out.quadrature_defect;
        Ok(out)
    }
}

